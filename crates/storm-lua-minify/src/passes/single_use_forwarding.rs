//! Single-use global forwarding (`passes/global-forwarding.ts`).
//!
//! A stable global assignment is removed when all reads before its next write
//! occur in exactly one later statement and inlining the expression makes the
//! two statements shorter. Dynamic global access, dependency mutation, calls
//! between definition and use, and loop-carried writes are conservative
//! barriers.

use crate::pass::PassResult;
use std::collections::HashSet;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::print::Printer;
use storm_lua_syntax::size::measure_size;

#[derive(Clone, Copy)]
struct Plan {
    assignment_index: usize,
    part: usize,
    use_index: usize,
    target_bid: BindingId,
    expression: NodeId,
}

fn node_bid(resolution: &Resolution, node: NodeId) -> Option<BindingId> {
    resolution.node_bid.get(node as usize).copied().flatten()
}

fn is_write(resolution: &Resolution, node: NodeId) -> bool {
    resolution
        .node_write
        .get(node as usize)
        .copied()
        .unwrap_or(false)
}

fn scoped_read_count(ast: &Ast, resolution: &Resolution, node: NodeId, bid: BindingId) -> usize {
    if matches!(ast.node(node), Node::Function(..)) {
        return 0;
    }
    let mut count = usize::from(
        matches!(ast.node(node), Node::Name(_))
            && node_bid(resolution, node) == Some(bid)
            && !is_write(resolution, node),
    );
    let mut children = Vec::new();
    storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
        children.push(child)
    });
    for child in children {
        count += scoped_read_count(ast, resolution, child, bid);
    }
    count
}

/// Count reads across callback/function boundaries.  The local block scan is
/// intentionally opaque to nested functions, but globals are shared state and
/// a read in `onDraw` must keep an assignment in `onTick` observable.
fn all_read_count(ast: &Ast, resolution: &Resolution, node: NodeId, bid: BindingId) -> usize {
    let mut count = usize::from(
        matches!(ast.node(node), Node::Name(_))
            && node_bid(resolution, node) == Some(bid)
            && !is_write(resolution, node),
    );
    let mut children = Vec::new();
    storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
        children.push(child)
    });
    for child in children {
        count += all_read_count(ast, resolution, child, bid);
    }
    count
}

fn read_follows_effectful_call_argument(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    statement: NodeId,
    bid: BindingId,
) -> bool {
    let Node::Callstat(call) = ast.node(statement) else {
        return false;
    };
    let Node::Call(function, arguments, _) = ast.node(*call) else {
        return false;
    };
    let mut prior_effectful =
        analyzer.effects_for_expr(*function).calls || analyzer.effects_for_expr(*function).ordered;
    for argument in arguments {
        if scoped_read_count(ast, resolution, *argument, bid) > 0 && prior_effectful {
            return true;
        }
        let effect = analyzer.effects_for_expr(*argument);
        prior_effectful |= effect.calls || effect.ordered || !effect.writes.is_empty();
    }
    false
}

fn is_loop_statement(node: &Node) -> bool {
    matches!(
        node,
        Node::Fornum(..) | Node::Forin(..) | Node::While(..) | Node::Repeat(..)
    )
}

fn dynamic_globals(ast: &Ast, root: NodeId) -> bool {
    let mut dynamic = false;
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |node| {
        if dynamic {
            return;
        }
        match ast.node(node) {
            Node::Name(symbol) if ast.strings.get(*symbol) == "_ENV" => dynamic = true,
            Node::Call(function, _, _) => {
                if let Node::Name(symbol) = ast.node(*function) {
                    if matches!(
                        ast.strings.get(*symbol),
                        "rawget" | "rawset" | "load" | "loadstring"
                    ) {
                        dynamic = true;
                    }
                }
            }
            _ => {}
        }
    });
    dynamic
}

fn collect_plans(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    block: NodeId,
    root: NodeId,
) -> Vec<Plan> {
    let Node::Block(statements) = ast.node(block) else {
        return Vec::new();
    };
    let mut plans = Vec::new();
    for (from, assignment_id) in statements.iter().copied().enumerate() {
        let Node::Assign(targets, expressions) = ast.node(assignment_id) else {
            continue;
        };
        if targets.len() != expressions.len()
            || !targets
                .iter()
                .all(|target| matches!(ast.node(*target), Node::Name(_)))
        {
            continue;
        }
        let assignment_effect = analyzer.effects_for_statement(assignment_id);
        for part in 0..targets.len() {
            let target = targets[part];
            let expression = expressions[part];
            let Some(target_bid) = node_bid(resolution, target) else {
                continue;
            };
            let binding = &resolution.bindings[target_bid as usize];
            let expression_effect = analyzer.effects_for_expr(expression);
            if binding.kind != BindingKind::Global
                || binding.fixed
                || binding.function_node.is_some()
                || expression_effect.calls
                || expression_effect.ordered
                || !expression_effect.writes.is_empty()
                || expression_effect
                    .reads
                    .iter()
                    .any(|bid| *bid != target_bid && assignment_effect.writes.contains(bid))
                || (expression_effect.may_throw
                    && !crate::passes::global_stores::total_numeric_expression(
                        ast, analyzer, expression,
                    ))
                || matches!(ast.node(expression), Node::Table(_) | Node::Function(..))
                || !expression_effect.stable
            {
                continue;
            }

            let mut use_statement = None;
            let mut segment_reads = 0usize;
            let mut multiple_use_statements = false;
            for (to, statement) in statements.iter().copied().enumerate().skip(from + 1) {
                let use_count = scoped_read_count(ast, resolution, statement, target_bid);
                if use_count > 0 {
                    segment_reads += use_count;
                    if let Some(previous) = use_statement {
                        if previous != to {
                            multiple_use_statements = true;
                        }
                    } else {
                        use_statement = Some(to);
                    }
                }
                let effect = analyzer.effects_for_statement(statement);
                if use_count == 0 && effect.reads.contains(&target_bid) {
                    multiple_use_statements = true;
                }
                if effect.writes.contains(&target_bid) {
                    break;
                }
            }
            let Some(use_index) = use_statement else {
                continue;
            };
            if !(1..=8).contains(&segment_reads) || multiple_use_statements {
                continue;
            }
            // `scoped_read_count` deliberately stops at nested functions.  A
            // global, however, can be observed by another callback, so only
            // forward when this is the global's sole read in the whole chunk.
            if all_read_count(ast, resolution, root, target_bid) != segment_reads {
                continue;
            }
            let use_effect = analyzer.effects_for_statement(statements[use_index]);
            // The read is a snapshot established by the assignment.  Moving
            // its expression into a call can place evaluation after an
            // earlier argument or inside an effectful API, changing the value
            // observed by the call (for example `output.setNumber(g(), x)`).
            if read_follows_effectful_call_argument(
                ast,
                resolution,
                analyzer,
                statements[use_index],
                target_bid,
            ) {
                continue;
            }
            if is_loop_statement(ast.node(statements[use_index]))
                && use_effect.writes.contains(&target_bid)
            {
                continue;
            }
            let blocked = statements[from + 1..use_index].iter().any(|statement| {
                let effect = analyzer.effects_for_statement(*statement);
                effect.calls
                    || expression_effect
                        .reads
                        .iter()
                        .any(|bid| effect.writes.contains(bid))
            });
            if blocked {
                continue;
            }
            plans.push(Plan {
                assignment_index: from,
                part,
                use_index,
                target_bid,
                expression,
            });
        }
    }
    plans
}

fn clone_subtree(ast: &mut Ast, node: NodeId) -> NodeId {
    let origin = ast.nodes.capture_origin(node);
    let original = ast.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_subtree(ast, child)
    });
    let copied = ast.push(mapped);
    ast.nodes
        .finish_rewrite(copied, origin, "global-forwarding-copy");
    copied
}

fn clone_replacing_reads(
    ast: &mut Ast,
    resolution: &Resolution,
    node: NodeId,
    target_bid: BindingId,
    expression: NodeId,
) -> NodeId {
    let original = ast.node(node).clone();
    if matches!(original, Node::Name(_))
        && node_bid(resolution, node) == Some(target_bid)
        && !is_write(resolution, node)
    {
        let replacement = clone_subtree(ast, expression);
        ast.nodes
            .relate_within(replacement, node, "single-use-global-forwarding");
        return replacement;
    }
    let origin = ast.nodes.capture_origin(node);
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_replacing_reads(ast, resolution, child, target_bid, expression)
    });
    let copied = ast.push(mapped);
    ast.nodes
        .finish_rewrite(copied, origin, "single-use-global-forwarding");
    copied
}

fn statement_size(ast: &Ast, statement: NodeId) -> usize {
    Printer::new(ast, false).stat_public(statement).len()
}

fn try_block(ast: &mut Ast, root: NodeId, block: NodeId) -> bool {
    let resolution = resolve(ast, root);
    let plans = {
        let analyzer = EffectAnalyzer::new(ast, &resolution, root, true);
        collect_plans(ast, &resolution, &analyzer, block, root)
    };
    let Node::Block(base_statements) = ast.node(block).clone() else {
        return false;
    };

    for plan in plans {
        let assignment_id = base_statements[plan.assignment_index];
        let Node::Assign(targets, expressions) = ast.node(assignment_id).clone() else {
            continue;
        };
        let checkpoint = ast.nodes.len();
        let replaced = clone_replacing_reads(
            ast,
            &resolution,
            base_statements[plan.use_index],
            plan.target_bid,
            plan.expression,
        );
        let kept = (0..targets.len())
            .filter(|index| *index != plan.part)
            .collect::<Vec<_>>();
        let replacement_assignment = if kept.is_empty() {
            None
        } else {
            Some(ast.assign(
                kept.iter().map(|index| targets[*index]).collect(),
                kept.iter().map(|index| expressions[*index]).collect(),
            ))
        };
        if let Some(replacement) = replacement_assignment {
            let origin = ast.nodes.capture_origin(assignment_id);
            ast.nodes
                .finish_rewrite(replacement, origin, "single-use-global-forwarding");
        }
        let old_cost = statement_size(ast, assignment_id)
            + statement_size(ast, base_statements[plan.use_index]);
        let new_cost = replacement_assignment.map_or(0, |statement| statement_size(ast, statement))
            + statement_size(ast, replaced);
        if new_cost < old_cost {
            let mut statements = base_statements.clone();
            statements[plan.use_index] = replaced;
            if let Some(statement) = replacement_assignment {
                statements[plan.assignment_index] = statement;
            } else {
                statements.remove(plan.assignment_index);
            }
            ast.nodes.rewrite(
                block,
                Node::Block(statements),
                "single-use-global-forwarding",
            );
            return true;
        }
        ast.nodes.truncate(checkpoint);
    }
    false
}

fn collect_blocks_postorder(
    ast: &Ast,
    node: NodeId,
    seen: &mut HashSet<NodeId>,
    output: &mut Vec<NodeId>,
) {
    if !seen.insert(node) {
        return;
    }
    let mut children = Vec::new();
    storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
        children.push(child)
    });
    for child in children {
        collect_blocks_postorder(ast, child, seen, output);
    }
    if matches!(ast.node(node), Node::Block(_)) {
        output.push(node);
    }
}

pub fn forward_single_use_globals(ast: &mut Ast, root: NodeId) -> PassResult {
    let original_size = measure_size(ast, root);
    let mut forwarded = 0usize;
    for _ in 0..16 {
        if dynamic_globals(ast, root) {
            break;
        }
        let mut blocks = Vec::new();
        collect_blocks_postorder(ast, root, &mut HashSet::new(), &mut blocks);
        let mut applied = 0usize;
        for block in blocks {
            if try_block(ast, root, block) {
                applied += 1;
            }
        }
        if applied == 0 {
            break;
        }
        forwarded += applied;
    }
    PassResult {
        root,
        saved: Some(original_size.saturating_sub(measure_size(ast, root)) as u64),
        details: Some(vec![format!("forwarded={forwarded}")]),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::{parse_source, Printer};

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = forward_single_use_globals(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn forwards_global_into_its_only_use_statement() {
        assert_eq!(
            output("sensor=0 function onTick()sensor=input.getNumber(1)output.setNumber(1,sensor+2)end"),
            "sensor=0 function onTick()output.setNumber(1,input.getNumber(1)+2)end"
        );
    }

    #[test]
    fn rejects_multiple_use_statements() {
        let source = "x=1 output.setNumber(1,x)output.setNumber(2,x)";
        assert_eq!(output(source), source);
    }

    #[test]
    fn rejects_dependency_mutation_between_definition_and_use() {
        let source =
            "function onTick()local x=input.getNumber(1)sensor=x x=2 output.setNumber(1,sensor)end";
        assert_eq!(output(source), source);
    }

    #[test]
    fn dynamic_global_access_disables_forwarding() {
        let source = "sensor=1 output.setNumber(1,sensor)rawget(_ENV,\"sensor\")";
        assert_eq!(output(source), source);
    }

    #[test]
    fn preserves_loop_carried_accumulation() {
        let source = "sum=0 for i=1,4 do sum=sum+input.getNumber(i)end output.setNumber(1,sum)";
        assert_eq!(output(source), source.replace(" then ", "then"));
    }
}
