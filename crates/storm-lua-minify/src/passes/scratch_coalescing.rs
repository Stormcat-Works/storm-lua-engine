//! Equal scratch value coalescing (`passes/global-forwarding.ts`).
//!
//! When one parallel assignment gives identical expressions to two compiler
//! scratch globals (`__sN` / `__fsN`), later straight-line reads of the second
//! slot can reuse the first. Control-flow statements are barriers. Each block
//! accepts at most one profitable pair per round, and the pass repeats for at
//! most 16 rounds.

use crate::pass::PassResult;
use std::collections::HashSet;
use storm_lua_analysis::effects::{ast_same, EffectAnalyzer};
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId};
use storm_lua_syntax::print::Printer;

#[derive(Clone)]
struct Plan {
    assignment_index: usize,
    remove_index: usize,
    keeper_symbol: SymbolId,
    duplicate_bid: BindingId,
    scanned: Vec<(NodeId, usize)>,
}

fn is_scratch_name(name: &str) -> bool {
    let suffix = name
        .strip_prefix("__fs")
        .or_else(|| name.strip_prefix("__s"));
    suffix.is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit()))
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

fn control_flow_barrier(node: &Node) -> bool {
    matches!(
        node,
        Node::If(..)
            | Node::While(..)
            | Node::Repeat(..)
            | Node::Fornum(..)
            | Node::Forin(..)
            | Node::Do(..)
    )
}

fn collect_plans(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    block: NodeId,
) -> Vec<Plan> {
    let Node::Block(statements) = ast.node(block) else {
        return Vec::new();
    };
    let mut plans = Vec::new();
    for (at, assignment_id) in statements.iter().copied().enumerate() {
        let Node::Assign(targets, expressions) = ast.node(assignment_id) else {
            continue;
        };
        if targets.len() != expressions.len()
            || targets.len() < 2
            || !targets
                .iter()
                .all(|target| matches!(ast.node(*target), Node::Name(_)))
        {
            continue;
        }
        for keep in 0..targets.len() {
            for remove in keep + 1..targets.len() {
                let keeper = targets[keep];
                let duplicate = targets[remove];
                let (Some(keeper_bid), Some(duplicate_bid)) = (
                    node_bid(resolution, keeper),
                    node_bid(resolution, duplicate),
                ) else {
                    continue;
                };
                if keeper_bid == duplicate_bid {
                    continue;
                }
                let keeper_binding = &resolution.bindings[keeper_bid as usize];
                let duplicate_binding = &resolution.bindings[duplicate_bid as usize];
                let keeper_name = ast.strings.get(keeper_binding.name);
                let duplicate_name = ast.strings.get(duplicate_binding.name);
                if !is_scratch_name(keeper_name)
                    || !is_scratch_name(duplicate_name)
                    || !ast_same(ast, expressions[keep], expressions[remove])
                {
                    continue;
                }
                // Equal syntax does not imply equal value when evaluating the
                // expression has an observable effect.  Coalescing removes
                // the duplicate RHS evaluation, so only genuinely inert
                // expressions are eligible.
                let expression_effect = analyzer.effects_for_expr(expressions[keep]);
                if expression_effect.calls
                    || expression_effect.ordered
                    || !expression_effect.writes.is_empty()
                {
                    continue;
                }

                let mut reads = 0usize;
                let mut blocked = false;
                let mut scanned = Vec::new();
                for statement in statements.iter().copied().skip(at + 1) {
                    if control_flow_barrier(ast.node(statement)) {
                        blocked = true;
                        break;
                    }
                    let use_count = scoped_read_count(ast, resolution, statement, duplicate_bid);
                    scanned.push((statement, use_count));
                    reads += use_count;
                    let effects = analyzer.effects_for_statement(statement);
                    if effects.writes.contains(&keeper_bid)
                        || effects.writes.contains(&duplicate_bid)
                    {
                        break;
                    }
                }
                if blocked || reads == 0 {
                    continue;
                }
                let Node::Name(keeper_symbol) = ast.node(keeper) else {
                    unreachable!();
                };
                plans.push(Plan {
                    assignment_index: at,
                    remove_index: remove,
                    keeper_symbol: *keeper_symbol,
                    duplicate_bid,
                    scanned,
                });
            }
        }
    }
    plans
}

fn clone_replacing_reads(
    ast: &mut Ast,
    resolution: &Resolution,
    node: NodeId,
    duplicate_bid: BindingId,
    keeper_symbol: SymbolId,
) -> NodeId {
    let original = ast.node(node).clone();
    if matches!(original, Node::Name(_))
        && node_bid(resolution, node) == Some(duplicate_bid)
        && !is_write(resolution, node)
    {
        let copied = ast.push(Node::Name(keeper_symbol));
        let origin = ast.nodes.capture_origin(node);
        ast.nodes.finish_rename(copied, origin);
        return copied;
    }
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_replacing_reads(ast, resolution, child, duplicate_bid, keeper_symbol)
    });
    let copied = ast.push(mapped);
    super::origins::within(ast, copied, &[node], "scratch-value-coalescing");
    copied
}

fn statement_size(ast: &Ast, statement: NodeId) -> usize {
    Printer::new(ast, false).stat_public(statement).len()
}

fn try_block(ast: &mut Ast, root: NodeId, block: NodeId) -> bool {
    let resolution = resolve(ast, root);
    let plans = {
        let analyzer = EffectAnalyzer::new(ast, &resolution, root, true);
        collect_plans(ast, &resolution, &analyzer, block)
    };
    let Node::Block(base_statements) = ast.node(block).clone() else {
        return false;
    };

    for plan in plans {
        let Node::Assign(targets, expressions) =
            ast.node(base_statements[plan.assignment_index]).clone()
        else {
            continue;
        };
        let checkpoint = ast.nodes.len();
        let kept_indices = (0..targets.len())
            .filter(|index| *index != plan.remove_index)
            .collect::<Vec<_>>();
        let reduced = ast.assign(
            kept_indices.iter().map(|index| targets[*index]).collect(),
            kept_indices
                .iter()
                .map(|index| expressions[*index])
                .collect(),
        );
        super::origins::within(
            ast,
            reduced,
            &[base_statements[plan.assignment_index]],
            "scratch-definition-reduction",
        );
        let rewritten = plan
            .scanned
            .iter()
            .map(|(statement, use_count)| {
                if *use_count == 0 {
                    *statement
                } else {
                    clone_replacing_reads(
                        ast,
                        &resolution,
                        *statement,
                        plan.duplicate_bid,
                        plan.keeper_symbol,
                    )
                }
            })
            .collect::<Vec<_>>();

        let before = statement_size(ast, base_statements[plan.assignment_index])
            + plan
                .scanned
                .iter()
                .map(|(statement, _)| statement_size(ast, *statement))
                .sum::<usize>();
        let after = statement_size(ast, reduced)
            + rewritten
                .iter()
                .map(|statement| statement_size(ast, *statement))
                .sum::<usize>();
        if after < before {
            let mut statements = base_statements.clone();
            statements[plan.assignment_index] = reduced;
            let start = plan.assignment_index + 1;
            statements.splice(start..start + rewritten.len(), rewritten);
            ast.nodes
                .rewrite(block, Node::Block(statements), "scratch-value-coalescing");
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

pub fn coalesce_equal_scratch_values(ast: &mut Ast, root: NodeId) -> PassResult {
    let mut coalesced = 0usize;
    for _ in 0..16 {
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
        coalesced += applied;
    }
    PassResult {
        root,
        saved: None,
        details: Some(vec![format!("coalesced={coalesced}")]),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::{parse_source, Printer};

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = coalesce_equal_scratch_values(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn coalesces_equal_parallel_scratch_definitions() {
        let source = "function onTick()local x=input.getNumber(1)__fs0,__fs1=x*2,x*2 output.setNumber(1,__fs0)output.setNumber(2,__fs1)end";
        assert_eq!(
            output(source),
            "function onTick()local x=input.getNumber(1)__fs0=x*2 output.setNumber(1,__fs0)output.setNumber(2,__fs0)end"
        );
    }

    #[test]
    fn supports_short_scratch_prefix() {
        assert_eq!(
            output("__s0,__s1=1,1 output.setNumber(1,__s1)"),
            "__s0=1 output.setNumber(1,__s0)"
        );
    }

    #[test]
    fn rejects_non_scratch_bindings() {
        assert_eq!(
            output("a,b=1,1 output.setNumber(1,b)"),
            "a,b=1,1 output.setNumber(1,b)"
        );
    }

    #[test]
    fn control_flow_is_a_barrier() {
        let source = "__fs0,__fs1=1,1 if input.getBool(1)then output.setNumber(1,__fs1)end";
        assert_eq!(
            output(source),
            "__fs0,__fs1=1,1 if input.getBool(1)then output.setNumber(1,__fs1)end"
        );
    }
}
