//! Propagate only globals certified absent by the selected game contract.
//! Unknown external bindings are not inferred to be nil from missing source writes.
//! The whole-program pipeline is bypassed for explicit _ENV access or extended hosts.

use crate::pass::PassResult;
use storm_lua_analysis::resolver::{resolve, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};

/// `propagateUnwrittenGlobalsAsNil` の移植。resolution は内部で実行する。
pub fn propagate_unwritten_globals_as_nil(ast: &mut Ast, root: NodeId) -> PassResult {
    let res = resolve(ast, root);
    let (new_ast, new_root, replaced) = propagate_unwritten_globals_as_nil_impl(ast, &res, root);
    ast.nodes = new_ast.nodes;
    PassResult {
        root: new_root,
        saved: Some(replaced as u64),
        details: None,
    }
}

fn propagate_unwritten_globals_as_nil_impl(
    ast: &Ast,
    res: &Resolution,
    root: NodeId,
) -> (Ast, NodeId, usize) {
    // `__fsN`/`__sN` are also valid user globals.  Their spelling is reserved
    // by older slotting passes, so when one is present we cannot prove that a
    // resolver write is compiler-generated rather than user-visible state.
    // Keep all globals untouched; a false nil replacement is much worse than
    // foregoing this small cleanup.
    if ast.strings.iter().any(|name| {
        name.strip_prefix("__fs")
            .or_else(|| name.strip_prefix("__s"))
            .is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
            })
    }) {
        let mut new_ast = storm_lua_syntax::ast_utils::inherit_ast(ast);
        new_ast.nodes = ast.nodes.clone();
        return (new_ast, root, 0);
    }
    // 動的 global access がある場合は何もしない
    let mut dynamic = false;
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |id| match ast.node(id) {
        Node::Name(s) => {
            if ast.strings.get(*s) == "_ENV" {
                dynamic = true;
            }
        }
        Node::Call(fn_, _, _) => {
            if let Node::Name(s) = ast.node(*fn_) {
                let name = ast.strings.get(*s);
                if ["rawget", "rawset", "load", "loadstring"].contains(&name) {
                    dynamic = true;
                }
            }
        }
        _ => {}
    });
    if dynamic {
        let mut new_ast = storm_lua_syntax::ast_utils::inherit_ast(ast);
        new_ast.nodes = ast.nodes.clone();
        return (new_ast, root, 0);
    }

    let mut written = vec![false; res.bindings.len()];
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |id| {
        if let Node::Name(_) = ast.node(id) {
            if res.node_write[id as usize] {
                if let Some(bid) = res.node_bid[id as usize] {
                    written[bid as usize] = true;
                }
            }
        }
    });

    let mut nil_globals = vec![false; res.bindings.len()];
    for (i, b) in res.bindings.iter().enumerate() {
        if i == 0 {
            continue;
        }
        if b.kind == BindingKind::Global
            && !b.fixed
            && b.function_node.is_none()
            && !written[i]
            && storm_lua_spec::environment::EnvironmentProfile::Game
                .is_unavailable(ast.strings.get(b.name))
        {
            nil_globals[i] = true;
        }
    }

    let mut replaced = 0usize;
    let mut new_ast = storm_lua_syntax::ast_utils::inherit_ast(ast);
    new_ast.nodes = ast.nodes.clone();
    let new_root = rewrite(&mut new_ast, res, &nil_globals, root, &mut replaced);
    (new_ast, new_root, replaced)
}

fn rewrite(
    new_ast: &mut Ast,
    res: &Resolution,
    nil_globals: &[bool],
    id: NodeId,
    replaced: &mut usize,
) -> NodeId {
    let node = new_ast.nodes[id as usize].clone();
    match &node {
        Node::Name(_) => {
            let is_nil = !res.node_write[id as usize]
                && res
                    .node_bid
                    .get(id as usize)
                    .copied()
                    .flatten()
                    .map(|b| nil_globals.get(b as usize).copied().unwrap_or(false))
                    .unwrap_or(false);
            if is_nil {
                *replaced += 1;
                let origin = new_ast.nodes.capture_origin(id);
                let replacement = new_ast.push(Node::Nil);
                new_ast.nodes.finish_rewrite(
                    replacement,
                    origin,
                    "unwritten-global-nil-propagation",
                );
                replacement
            } else {
                id
            }
        }
        _ => {
            let (new_node, _) = storm_lua_syntax::ast_utils::map_children(&node, &mut |c| {
                rewrite(new_ast, res, nil_globals, c, replaced)
            });
            new_ast
                .nodes
                .rewrite(id, new_node, "unwritten-global-nil-propagation");
            id
        }
    }
}

pub(crate) fn total_numeric_expression(
    ast: &Ast,
    analyzer: &storm_lua_analysis::effects::EffectAnalyzer<'_>,
    expression: NodeId,
) -> bool {
    match ast.node(expression) {
        Node::Num(_) => true,
        Node::Paren(inner) => total_numeric_expression(ast, analyzer, *inner),
        Node::Un(op, inner) => {
            matches!(op.as_str(), "-" | "~") && total_numeric_expression(ast, analyzer, *inner)
        }
        Node::Bin(op, left, right) => {
            matches!(
                op.as_str(),
                "+" | "-" | "*" | "/" | "^" | "&" | "|" | "~" | "<<" | ">>"
            ) && total_numeric_expression(ast, analyzer, *left)
                && total_numeric_expression(ast, analyzer, *right)
        }
        Node::Call(function, arguments, _) => {
            let builtin = analyzer.resolve_builtin_reference(*function);
            if matches!(
                builtin.as_deref(),
                Some("input.getNumber" | "property.getNumber")
            ) {
                return true;
            }
            // 引数ゼロは all() で素通りしてしまうが、PUC の math.* は
            // 引数が無いとエラーになる（Q04: `math.max()` 等）。下の
            // math.* 分岐では空でないことを全域性の条件に含める。
            matches!(
                builtin.as_deref(),
                Some(
                    "math.abs"
                        | "math.max"
                        | "math.min"
                        | "math.floor"
                        | "math.sin"
                        | "math.cos"
                        | "math.tan"
                        | "math.atan"
                        | "math.sqrt"
                )
            ) && !arguments.is_empty()
                && arguments
                    .iter()
                    .all(|argument| total_numeric_expression(ast, analyzer, *argument))
        }
        _ => false,
    }
}

fn is_scratch_name(name: &str) -> bool {
    name.strip_prefix("__fs").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn has_root_scratch_assignment(ast: &Ast, root: NodeId) -> bool {
    let Node::Block(statements) = ast.node(root) else {
        return false;
    };
    statements.iter().any(|statement| {
        let Node::Assign(targets, _) = ast.node(*statement) else {
            return false;
        };
        targets.iter().any(|target| {
            matches!(ast.node(*target), Node::Name(symbol) if is_scratch_name(ast.strings.get(*symbol)))
        })
    })
}

fn cleanup_read_counts(ast: &Ast, res: &Resolution, root: NodeId) -> Vec<u32> {
    let mut reads = vec![0u32; res.bindings.len()];
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |id| {
        if matches!(ast.node(id), Node::Name(_)) && !res.node_write[id as usize] {
            if let Some(bid) = res.node_bid[id as usize] {
                reads[bid as usize] += 1;
            }
        }
    });
    reads
}

fn cleanup_transfer_effects(
    live: &mut std::collections::HashSet<u32>,
    scratch: &std::collections::HashSet<u32>,
    effect: &storm_lua_analysis::effects::Effects,
) {
    for bid in &effect.writes {
        if scratch.contains(bid) {
            live.remove(bid);
        }
    }
    for bid in &effect.reads {
        if scratch.contains(bid) {
            live.insert(*bid);
        }
    }
}

fn cleanup_transfer_assignment(
    source: &Ast,
    res: &Resolution,
    analyzer: &storm_lua_analysis::effects::EffectAnalyzer<'_>,
    live: &mut std::collections::HashSet<u32>,
    scratch: &std::collections::HashSet<u32>,
    targets: &[NodeId],
    expressions: &[NodeId],
) {
    let mut writes = Vec::new();
    let mut reads = Vec::new();
    let mut effect = storm_lua_analysis::effects::empty_effects();
    for (target, expression) in targets.iter().zip(expressions) {
        if let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() {
            writes.push(bid);
        }
        analyzer.effects_for_expr_into(*expression, &mut effect);
        writes.extend_from_slice(&effect.writes);
        reads.extend_from_slice(&effect.reads);
    }
    for bid in writes {
        if scratch.contains(&bid) {
            live.remove(&bid);
        }
    }
    for bid in reads {
        if scratch.contains(&bid) {
            live.insert(bid);
        }
    }
    let _ = source;
}

fn cleanup_droppable(
    ast: &Ast,
    analyzer: &storm_lua_analysis::effects::EffectAnalyzer<'_>,
    expression: NodeId,
    aggressive: bool,
) -> bool {
    let effect = analyzer.effects_for_expr(expression);
    !effect.calls
        && !effect.ordered
        && effect.writes.is_empty()
        && (analyzer.is_droppable(expression)
            || (aggressive && total_numeric_expression(ast, analyzer, expression)))
}

#[allow(clippy::too_many_arguments)]
fn cleanup_block(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    analyzer: &storm_lua_analysis::effects::EffectAnalyzer<'_>,
    reads: &[u32],
    scratch: &std::collections::HashSet<u32>,
    block: NodeId,
    live_out: &std::collections::HashSet<u32>,
    aggressive: bool,
) -> (NodeId, std::collections::HashSet<u32>) {
    let Node::Block(statements) = source.node(block).clone() else {
        unreachable!("cleanup_block requires block")
    };
    let mut rhs_pool: Vec<storm_lua_analysis::effects::Effects> = Vec::new();
    let mut effect = storm_lua_analysis::effects::empty_effects();
    let mut live = live_out.clone();
    let mut output = Vec::with_capacity(statements.len());
    for statement in statements.into_iter().rev() {
        if let Node::Assign(targets, expressions) = source.node(statement) {
            if targets.len() == expressions.len()
                && targets
                    .iter()
                    .all(|target| matches!(source.node(*target), Node::Name(_)))
            {
                while rhs_pool.len() < expressions.len() {
                    rhs_pool.push(storm_lua_analysis::effects::empty_effects());
                }
                for (index, expression) in expressions.iter().enumerate() {
                    analyzer.effects_for_expr_into(*expression, &mut rhs_pool[index]);
                }
                let rhs_effects = &rhs_pool[..expressions.len()];
                let target_bids = targets
                    .iter()
                    .map(|target| res.node_bid.get(*target as usize).copied().flatten())
                    .collect::<Vec<_>>();
                let mut keep = Vec::new();
                for index in 0..targets.len() {
                    let target = targets[index];
                    let expression = expressions[index];
                    let bid = res.node_bid.get(target as usize).copied().flatten();
                    // The self-assignment shortcut is valid only when
                    // the bid has no other part in this statement: the
                    // stores of one assignment run right-to-left, so
                    // with a repeated target the self-assigned part is
                    // the store that executes LAST and decides the
                    // binding's final value (candidate R, third
                    // consumer, w9 cycle 66).
                    let self_assignment = target_bids.iter().filter(|other| **other == bid).count()
                        == 1
                        && matches!(source.node(expression), Node::Name(_))
                        && bid.is_some()
                        && res.node_bid.get(expression as usize).copied().flatten() == bid
                        && !rhs_effects.iter().enumerate().any(|(rhs_index, effect)| {
                            rhs_index != index
                                && bid.is_some_and(|target_bid| effect.writes.contains(&target_bid))
                        });
                    if self_assignment {
                        continue;
                    }
                    let unread_global = bid.is_some_and(|binding_id| {
                        let binding = res.binding(binding_id);
                        matches!(binding.kind, BindingKind::Global)
                            && !binding.fixed
                            && reads.get(binding_id as usize).copied().unwrap_or(0) == 0
                    });
                    let dead_scratch = bid.is_some_and(|binding_id| {
                        scratch.contains(&binding_id)
                            && !live.contains(&binding_id)
                            && cleanup_droppable(source, analyzer, expression, aggressive)
                    });
                    if !(dead_scratch
                        || unread_global
                            && cleanup_droppable(source, analyzer, expression, aggressive))
                    {
                        keep.push(index);
                    }
                }
                if !keep.is_empty() {
                    let kept_targets = keep.iter().map(|index| targets[*index]).collect::<Vec<_>>();
                    let kept_expressions = keep
                        .iter()
                        .map(|index| expressions[*index])
                        .collect::<Vec<_>>();
                    target.nodes.rewrite(
                        statement,
                        Node::Assign(kept_targets.clone(), kept_expressions.clone()),
                        "global-store-cleanup",
                    );
                    cleanup_transfer_assignment(
                        source,
                        res,
                        analyzer,
                        &mut live,
                        scratch,
                        &kept_targets,
                        &kept_expressions,
                    );
                    output.push(statement);
                }
                continue;
            }
        }
        match source.node(statement).clone() {
            Node::If(arms, else_block) => {
                let mut branch_inputs = Vec::new();
                let mut rewritten_arms = Vec::with_capacity(arms.len());
                for arm in &arms {
                    let (body, live_in) = cleanup_block(
                        target, source, res, analyzer, reads, scratch, arm.body, &live, aggressive,
                    );
                    branch_inputs.push(live_in);
                    rewritten_arms.push(storm_lua_syntax::ast::IfArm {
                        cond: arm.cond,
                        body,
                    });
                }
                let rewritten_else = if let Some(else_id) = else_block {
                    let (body, live_in) = cleanup_block(
                        target, source, res, analyzer, reads, scratch, else_id, &live, aggressive,
                    );
                    branch_inputs.push(live_in);
                    Some(body)
                } else {
                    branch_inputs.push(live.clone());
                    None
                };
                let mut merged = std::collections::HashSet::new();
                for input in branch_inputs {
                    merged.extend(input);
                }
                for arm in &arms {
                    analyzer.effects_for_expr_into(arm.cond, &mut effect);
                    for bid in &effect.reads {
                        if scratch.contains(bid) {
                            merged.insert(*bid);
                        }
                    }
                }
                live = merged;
                target.nodes.rewrite(
                    statement,
                    Node::If(rewritten_arms, rewritten_else),
                    "global-store-cleanup",
                );
                output.push(statement);
            }
            Node::Do(body) => {
                let (body, live_in) = cleanup_block(
                    target, source, res, analyzer, reads, scratch, body, &live, aggressive,
                );
                live = live_in;
                target
                    .nodes
                    .rewrite(statement, Node::Do(body), "global-store-cleanup");
                output.push(statement);
            }
            Node::While(..) | Node::Repeat(..) | Node::Fornum(..) | Node::Forin(..) => {
                // Loop bodies may execute zero times; retain values written in
                // the loop as live at entry rather than treating them as
                // unconditional overwrites.
                analyzer.effects_for_statement_into(statement, &mut effect);
                cleanup_transfer_effects(&mut live, scratch, &effect);
                live.extend(
                    effect
                        .writes
                        .iter()
                        .copied()
                        .filter(|bid| scratch.contains(bid)),
                );
                output.push(statement);
            }
            _ => {
                analyzer.effects_for_statement_into(statement, &mut effect);
                cleanup_transfer_effects(&mut live, scratch, &effect);
                output.push(statement);
            }
        }
    }
    output.reverse();
    target
        .nodes
        .rewrite(block, Node::Block(output), "global-store-cleanup");
    (block, live)
}

#[allow(clippy::too_many_arguments)]
fn cleanup_rewrite_node(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    analyzer: &storm_lua_analysis::effects::EffectAnalyzer<'_>,
    reads: &[u32],
    scratch: &std::collections::HashSet<u32>,
    id: NodeId,
    aggressive: bool,
) -> Option<NodeId> {
    if let Node::Function(params, vararg, body) = source.node(id).clone() {
        let (body, _) = cleanup_block(
            target,
            source,
            res,
            analyzer,
            reads,
            scratch,
            body,
            &std::collections::HashSet::new(),
            aggressive,
        );
        target.nodes.rewrite(
            id,
            Node::Function(params, vararg, body),
            "global-store-cleanup",
        );
        return Some(id);
    }
    if let Node::Funcstat(target_name, _) = source.node(id) {
        if matches!(source.node(*target_name), Node::Name(_)) {
            if let Some(bid) = res.node_bid.get(*target_name as usize).copied().flatten() {
                let binding = res.binding(bid);
                if matches!(binding.kind, BindingKind::Global)
                    && !binding.fixed
                    && reads.get(bid as usize).copied().unwrap_or(0) == 0
                {
                    return None;
                }
            }
        }
    }
    if let Node::Block(statements) = source.node(id).clone() {
        let mut output = Vec::new();
        for statement in statements {
            if let Some(next) = cleanup_rewrite_node(
                target, source, res, analyzer, reads, scratch, statement, aggressive,
            ) {
                if let Node::Block(nested) = target.node(next).clone() {
                    output.extend(nested);
                } else {
                    output.push(next);
                }
            }
        }
        target
            .nodes
            .rewrite(id, Node::Block(output), "global-store-cleanup");
        return Some(id);
    }
    let node = source.node(id).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&node, &mut |child| {
        cleanup_rewrite_node(
            target, source, res, analyzer, reads, scratch, child, aggressive,
        )
        .unwrap_or_else(|| target.block(Vec::new()))
    });
    target.nodes.rewrite(id, mapped, "global-store-cleanup");
    Some(id)
}

pub fn cleanup_global_stores_with_options(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
) -> PassResult {
    let before = storm_lua_syntax::size::measure_size(ast, root);
    // A root-level scratch spelling may be user-owned state rather than a
    // compiler temporary.  The backwards scratch liveness pass cannot tell
    // those cases apart across callback boundaries; retain the whole cleanup
    // transaction instead of deleting a live initialization.
    if has_root_scratch_assignment(ast, root) {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let mut source = storm_lua_syntax::ast_utils::inherit_ast(ast);
    source.nodes = ast.nodes.clone();
    let res = resolve(&source, root);
    let analyzer = storm_lua_analysis::effects::EffectAnalyzer::new(&source, &res, root, false);
    let reads = cleanup_read_counts(&source, &res, root);
    let scratch = res
        .bindings
        .iter()
        .enumerate()
        .filter_map(|(bid, binding)| {
            if bid == 0 {
                return None;
            }
            let name = source.strings.get(binding.name);
            (matches!(binding.kind, BindingKind::Global) && is_scratch_name(name))
                .then_some(bid as u32)
        })
        .collect::<std::collections::HashSet<_>>();
    let mut root_cleaned = storm_lua_syntax::ast_utils::inherit_ast(&source);
    root_cleaned.nodes = source.nodes.clone();
    let (root_cleaned_id, _) = cleanup_block(
        &mut root_cleaned,
        &source,
        &res,
        &analyzer,
        &reads,
        &scratch,
        root,
        &std::collections::HashSet::new(),
        aggressive,
    );
    let rewritten_source = root_cleaned;
    let mut target = storm_lua_syntax::ast_utils::inherit_ast(&rewritten_source);
    target.nodes = rewritten_source.nodes.clone();
    let rewritten_root = cleanup_rewrite_node(
        &mut target,
        &rewritten_source,
        &res,
        &analyzer,
        &reads,
        &scratch,
        root_cleaned_id,
        aggressive,
    )
    .unwrap_or_else(|| target.block(Vec::new()));
    *ast = target;
    PassResult {
        root: rewritten_root,
        saved: Some(
            before.saturating_sub(storm_lua_syntax::size::measure_size(ast, rewritten_root)) as u64,
        ),
        details: None,
    }
}

pub fn cleanup_global_stores(ast: &mut Ast, root: NodeId) -> PassResult {
    cleanup_global_stores_with_options(ast, root, true)
}

fn unread_droppable(
    ast: &Ast,
    analyzer: &storm_lua_analysis::effects::EffectAnalyzer<'_>,
    expression: NodeId,
) -> bool {
    matches!(
        ast.node(expression),
        Node::Name(_) | Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
    ) || total_numeric_expression(ast, analyzer, expression)
}

fn remove_unread_rewrite(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    analyzer: &storm_lua_analysis::effects::EffectAnalyzer<'_>,
    reads: &[u32],
    id: NodeId,
    removed: &mut usize,
) -> NodeId {
    if let Node::Block(statements) = source.node(id).clone() {
        let mut output = Vec::new();
        for statement in statements {
            let nested =
                remove_unread_rewrite(target, source, res, analyzer, reads, statement, removed);
            if let Node::Assign(targets, expressions) = target.node(nested).clone() {
                if targets.len() == expressions.len()
                    && targets
                        .iter()
                        .all(|target_id| matches!(target.node(*target_id), Node::Name(_)))
                {
                    let keep = (0..targets.len())
                        .filter(|index| {
                            let target_id = targets[*index];
                            let Some(bid) = res.node_bid.get(target_id as usize).copied().flatten()
                            else {
                                return true;
                            };
                            let binding = res.binding(bid);
                            let drop = matches!(binding.kind, BindingKind::Global)
                                && !binding.fixed
                                && binding.function_node.is_none()
                                && reads.get(bid as usize).copied().unwrap_or(0) == 0
                                && unread_droppable(source, analyzer, expressions[*index]);
                            if drop {
                                *removed += 1;
                            }
                            !drop
                        })
                        .collect::<Vec<_>>();
                    if !keep.is_empty() {
                        target.nodes.rewrite(
                            nested,
                            Node::Assign(
                                keep.iter().map(|index| targets[*index]).collect(),
                                keep.iter().map(|index| expressions[*index]).collect(),
                            ),
                            "unread-global-store-elimination",
                        );
                        output.push(nested);
                    }
                    continue;
                }
            }
            output.push(nested);
        }
        target
            .nodes
            .rewrite(id, Node::Block(output), "global-store-cleanup");
        return id;
    }
    let node = source.node(id).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&node, &mut |child| {
        remove_unread_rewrite(target, source, res, analyzer, reads, child, removed)
    });
    target.nodes.rewrite(id, mapped, "global-store-cleanup");
    id
}

pub fn remove_unread_global_stores(ast: &mut Ast, root: NodeId) -> PassResult {
    let before = storm_lua_syntax::size::measure_size(ast, root);
    if has_root_scratch_assignment(ast, root) {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let mut source = storm_lua_syntax::ast_utils::inherit_ast(ast);
    source.nodes = ast.nodes.clone();
    let res = resolve(&source, root);
    let analyzer = storm_lua_analysis::effects::EffectAnalyzer::new(&source, &res, root, true);
    let reads = cleanup_read_counts(&source, &res, root);
    let mut target = storm_lua_syntax::ast_utils::inherit_ast(&source);
    target.nodes = source.nodes.clone();
    let mut removed = 0usize;
    let out = remove_unread_rewrite(
        &mut target,
        &source,
        &res,
        &analyzer,
        &reads,
        root,
        &mut removed,
    );
    *ast = target;
    PassResult {
        root: out,
        saved: Some(before.saturating_sub(storm_lua_syntax::size::measure_size(ast, out)) as u64),
        details: if removed == 0 {
            None
        } else {
            Some(vec![format!("removed={removed}")])
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    #[test]
    fn keeps_unread_global_store_whose_rhs_may_throw_on_dynamic_argument() {
        // Candidate O (w9 cycle 66), global-store path: the store is
        // never read back, but evaluating `math.min(g, 2)` throws when
        // the dynamically typed `g` is nil, so the store is not
        // droppable. (The local form is covered by the dead-local
        // tests; this is the same predicate at the cleanup consumer.)
        let (mut ast, root) = parse_source("v = math.min(g, 2) v = 1 return 0").expect("parse");
        let result = cleanup_global_stores(&mut ast, root);
        let out = Printer::new(&ast, false).output(result.root);
        assert!(out.contains("math.min"), "{out}");
    }

    #[test]
    fn keeps_unread_global_store_whose_rhs_is_zero_argument_math_call() {
        // Q04: `math.max()` with no arguments is an error in PUC, so
        // the store is not droppable even though the all-arguments
        // totality check used to pass vacuously.
        let (mut ast, root) = parse_source("v = math.max() v = 1 return 0").expect("parse");
        let result = cleanup_global_stores(&mut ast, root);
        let out = Printer::new(&ast, false).output(result.root);
        assert!(out.contains("math.max"), "{out}");
        let (mut ast, root) = parse_source("v = math.min() v = 1 return 0").expect("parse");
        let result = cleanup_global_stores(&mut ast, root);
        let out = Printer::new(&ast, false).output(result.root);
        assert!(out.contains("math.min"), "{out}");
    }

    #[test]
    fn keeps_self_assignment_part_of_a_repeated_target() {
        // Candidate R, third consumer (w9 cycle 66): in
        // `a, a, e = a, -2, s` the first part looks like a no-op
        // self-assignment, but the stores run right-to-left, so it is
        // the last store executed and `a` keeps its old value.
        // Dropping the part would leave `a = -2`.
        let (mut ast, root) =
            parse_source("a, a, e = a, -2, tostring(\"true\") output.setNumber(1, a)")
                .expect("parse");
        let result = cleanup_global_stores(&mut ast, root);
        let out = Printer::new(&ast, false).output(result.root);
        // (The `e` part is dropped on its own merits: `e` is never
        // read and `tostring("true")` is provably total.)
        assert!(out.contains("a,a=a,-2"), "{out}");
    }

    #[test]
    fn handles_more_bindings_than_ast_nodes() {
        let names = (0..80)
            .map(|index| format!("a{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("local {names}\nwritten=1\nreturn pcall");
        let (mut ast, root) = parse_source(&source).expect("parse ok");
        let res = propagate_unwritten_globals_as_nil(&mut ast, root);
        assert_eq!(res.saved, Some(1));
        let output = Printer::new(&ast, false).output(res.root);
        assert!(output.contains("written=1 return nil"), "{output}");
    }
}
