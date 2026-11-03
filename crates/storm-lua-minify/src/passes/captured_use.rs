//! Captured single-use hoisting (`passes/captured-use-hoisting.ts`).
//!
//! A movable value stored only to survive intervening statements can be
//! substituted into its sole later assignment and moved before those
//! statements. Control flow, calls, ordered/throwing effects, target accesses,
//! and dependency writes are barriers.

use std::collections::HashSet;

use super::immutable_values::contains_fresh_reference;
use crate::pass::PassResult;
use storm_lua_analysis::effects::{empty_effects, EffectAnalyzer};
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::print::Printer;
use storm_lua_syntax::size::measure_size;

fn scoped_read_count(ast: &Ast, res: &Resolution, node: NodeId, bid: BindingId) -> usize {
    if matches!(ast.node(node), Node::Function(..)) {
        return 0;
    }
    let mut count = usize::from(
        matches!(ast.node(node), Node::Name(_))
            && !res.node_write.get(node as usize).copied().unwrap_or(false)
            && res.node_bid.get(node as usize).copied().flatten() == Some(bid),
    );
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        count += scoped_read_count(ast, res, child, bid);
    });
    count
}

/// Collect the bids read inside any nested function body in the whole chunk.
/// The same-block scan (`scoped_read_count`) is deliberately opaque to
/// function bodies, but a global (or an upvalue local) read from a nested
/// function still observes the capture: the function may be called after
/// the capture position (e.g. a top-level global read from `onTick`).
/// Hoisting a capture while such a read exists drops the definition the
/// nested function still references, so callers must reject those bids.
fn collect_nested_read_bids(
    ast: &Ast,
    res: &Resolution,
    node: NodeId,
    in_function: bool,
    out: &mut HashSet<BindingId>,
) {
    let in_function = in_function || matches!(ast.node(node), Node::Function(..));
    if in_function
        && matches!(ast.node(node), Node::Name(_))
        && !res.node_write.get(node as usize).copied().unwrap_or(false)
    {
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            out.insert(bid);
        }
    }
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        collect_nested_read_bids(ast, res, child, in_function, out);
    });
}

fn clone_subtree(target: &mut Ast, source: &Ast, node: NodeId) -> NodeId {
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_subtree(target, source, child)
    });
    let cloned = target.push(mapped);
    // A clone, not a replacement: the source and target arenas share their
    // source snapshots, so sharing the origin slot is sound here for the
    // same reason as `derive_from`'s fast path. Later mutation paths copy
    // the slot before writing, so a shared slot is never corrupted.
    target
        .nodes
        .derive_from_clone(cloned, &source.nodes, node, "captured-single-use-hoisting");
    cloned
}

fn replace_read(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    bid: BindingId,
    expression: NodeId,
) -> NodeId {
    if matches!(source.node(node), Node::Name(_))
        && !res.node_write.get(node as usize).copied().unwrap_or(false)
        && res.node_bid.get(node as usize).copied().flatten() == Some(bid)
    {
        let replacement = clone_subtree(target, source, expression);
        target.nodes.relate_from(
            replacement,
            &source.nodes,
            node,
            "captured-single-use-hoisting",
        );
        return replacement;
    }
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        replace_read(target, source, res, child, bid, expression)
    });
    let cloned = target.push(mapped);
    // A clone, not a replacement: replaced subtrees shift later child IDs,
    // so the value-equality fast path in `derive_from` would miss almost
    // every node after the first replaced read. The replaced read itself
    // keeps `relate_from` above, so it still records its use-site relation.
    target
        .nodes
        .derive_from_clone(cloned, &source.nodes, node, "captured-single-use-hoisting");
    cloned
}

fn control_barrier(node: &Node) -> bool {
    // Unconditional control transfers are barriers too (candidate Y,
    // cycle 67): a `goto` between the capture and the use can jump over
    // the use (or a label can be jumped into past it), and `break` /
    // `return` end the block's fall-through -- moving the use before
    // any of them executes an assignment the original never reached.
    matches!(
        node,
        Node::If(..)
            | Node::While(..)
            | Node::Repeat(..)
            | Node::Fornum(..)
            | Node::Forin(..)
            | Node::Do(..)
            | Node::Goto(..)
            | Node::Label(..)
            | Node::Break
            | Node::Return(..)
    )
}

fn statement_size(ast: &Ast, statement: NodeId) -> usize {
    Printer::new(ast, false).stat_public(statement).len()
}

fn transform_node(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    hoisted: &mut usize,
    nested_read_bids: &HashSet<BindingId>,
) -> NodeId {
    if matches!(source.node(node), Node::Block(_)) {
        return transform_block(
            target,
            source,
            res,
            analyzer,
            node,
            hoisted,
            nested_read_bids,
        );
    }
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        transform_node(
            target,
            source,
            res,
            analyzer,
            child,
            hoisted,
            nested_read_bids,
        )
    });
    target
        .nodes
        .rewrite(node, mapped, "captured-single-use-hoisting");
    node
}

fn transform_block(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    block: NodeId,
    hoisted: &mut usize,
    nested_read_bids: &HashSet<BindingId>,
) -> NodeId {
    let Node::Block(statements) = source.node(block).clone() else {
        unreachable!("transform_block requires block")
    };
    let mut nested = statements
        .into_iter()
        .map(|statement| {
            transform_node(
                target,
                source,
                res,
                analyzer,
                statement,
                hoisted,
                nested_read_bids,
            )
        })
        .collect::<Vec<_>>();

    // 同一ブロック内で複数件 hoist する。ループ上限は毎回評価する
    // （splice で nested が短くなるため、開始時固定の for 上限では
    //  3 件以上の兄弟 hoist で範囲外になり得る）。hoist 成功後は
    // from を1進め、移動文自身は capture として再訪しない
    // （移動文を起点とする連鎖は次ラウンドに残す、保守的設計）。
    let mut effect = empty_effects();
    let mut from = 0;
    while from + 2 < nested.len() {
        let capture = nested[from];
        let Node::Assign(targets, expressions) = target.node(capture).clone() else {
            from += 1;
            continue;
        };
        if targets.len() != 1
            || expressions.len() != 1
            || !matches!(target.node(targets[0]), Node::Name(_))
        {
            from += 1;
            continue;
        }
        let Some(bid) = res.node_bid.get(targets[0] as usize).copied().flatten() else {
            from += 1;
            continue;
        };
        let captured = expressions[0];
        if !analyzer.is_movable(captured) || contains_fresh_reference(source, captured) {
            from += 1;
            continue;
        }

        let mut use_at = None;
        for (at, statement) in nested.iter().enumerate().skip(from + 1) {
            if scoped_read_count(source, res, *statement, bid) > 0 {
                use_at = Some(at);
                break;
            }
            analyzer.effects_for_statement_into(*statement, &mut effect);
            if control_barrier(source.node(*statement))
                || effect.calls
                || effect.ordered
                || effect.may_throw
                || effect.writes.contains(&bid)
            {
                break;
            }
        }
        let Some(use_at) = use_at else {
            from += 1;
            continue;
        };
        // Only the first read is a valid sink target when all later reads are
        // absent.  Otherwise removing the capture changes the value observed
        // by a later statement (the original snapshot may outlive writes).
        let suffix_reads = nested
            .iter()
            .skip(use_at)
            .map(|statement| scoped_read_count(source, res, *statement, bid))
            .sum::<usize>();
        if suffix_reads != 1 {
            from += 1;
            continue;
        }
        if use_at < from + 2 {
            from += 1;
            continue;
        }
        let use_statement = nested[use_at];
        let Node::Assign(use_targets, use_expressions) = target.node(use_statement).clone() else {
            from += 1;
            continue;
        };
        if use_targets.len() != 1
            || use_expressions.len() != 1
            || !matches!(target.node(use_targets[0]), Node::Name(_))
            || scoped_read_count(source, res, use_statement, bid) != 1
            || !analyzer.is_movable(use_expressions[0])
        {
            from += 1;
            continue;
        }
        let Some(target_bid) = res.node_bid.get(use_targets[0] as usize).copied().flatten() else {
            from += 1;
            continue;
        };
        analyzer.effects_for_expr_into(use_expressions[0], &mut effect);
        let use_effect = &effect;
        let late_reads = use_effect
            .reads
            .iter()
            .copied()
            .filter(|read| *read != bid)
            .collect::<HashSet<_>>();
        let mut blocked = false;
        for statement in &nested[from + 1..use_at] {
            analyzer.effects_for_statement_into(*statement, &mut effect);
            if control_barrier(source.node(*statement))
                || effect.calls
                || effect.ordered
                || effect.may_throw
                || effect.reads.contains(&target_bid)
                || effect.writes.contains(&target_bid)
                || effect
                    .writes
                    .iter()
                    .any(|written| late_reads.contains(written))
            {
                blocked = true;
                break;
            }
        }
        if blocked {
            from += 1;
            continue;
        }
        // A nested function anywhere in the chunk may still read the
        // capture (globals are visible everywhere; locals become upvalues),
        // and the same-block scan above cannot see those reads. Hoisting
        // would drop the definition the nested function references.
        if nested_read_bids.contains(&bid) {
            from += 1;
            continue;
        }

        let moved = replace_read(target, source, res, use_statement, bid, captured);
        let old_cost = nested[from..=use_at]
            .iter()
            .map(|statement| statement_size(target, *statement))
            .sum::<usize>();
        let mut replacement = vec![moved];
        replacement.extend_from_slice(&nested[from + 1..use_at]);
        let new_cost = replacement
            .iter()
            .map(|statement| statement_size(target, *statement))
            .sum::<usize>();
        if new_cost >= old_cost {
            from += 1;
            continue;
        }
        nested.splice(from..=use_at, replacement);
        *hoisted += 1;
        from += 1;
    }

    target
        .nodes
        .rewrite(block, Node::Block(nested), "captured-single-use-hoisting");
    block
}

pub fn hoist_captured_single_uses_with_options(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
) -> PassResult {
    if !aggressive {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let before = measure_size(ast, root);
    let mut source = storm_lua_syntax::ast_utils::inherit_ast(ast);
    source.nodes = ast.nodes.clone();
    let res = resolve(&source, root);
    let analyzer = EffectAnalyzer::new(&source, &res, root, true);
    let mut hoisted = 0usize;
    let mut nested_read_bids = HashSet::new();
    collect_nested_read_bids(&source, &res, root, false, &mut nested_read_bids);
    let out = transform_block(
        ast,
        &source,
        &res,
        &analyzer,
        root,
        &mut hoisted,
        &nested_read_bids,
    );
    PassResult {
        root: out,
        saved: Some(before.saturating_sub(measure_size(ast, out)) as u64),
        details: if hoisted == 0 {
            None
        } else {
            Some(vec![format!("hoisted={hoisted}")])
        },
    }
}

pub fn hoist_captured_single_uses(ast: &mut Ast, root: NodeId) -> PassResult {
    hoist_captured_single_uses_with_options(ast, root, true)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::{parse_source, Printer};

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = hoist_captured_single_uses(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn hoists_a_single_use_before_unrelated_statements() {
        assert_eq!(output("a=123 x=1 y=a"), "y=123 x=1");
    }

    #[test]
    fn hoists_multiple_sibling_captures_in_one_apply() {
        assert_eq!(output("a=123 b=456 p=1 x=a y=b"), "x=123 y=456 p=1");
    }

    #[test]
    fn hoists_three_sibling_captures_in_one_apply() {
        assert_eq!(
            output("a=123 b=456 c=789 p=1 x=a y=b z=c"),
            "x=123 y=456 z=789 p=1"
        );
    }

    #[test]
    fn does_not_revisit_the_moved_statement_as_a_capture() {
        // a の hoist で生まれた y=123 は同 apply 内では capture として
        // 再訪されない（次ラウンドに残る、保守的設計）。
        assert_eq!(output("a=123 x=1 y=a w=1 v=y"), "y=123 x=1 w=1 v=y");
    }

    #[test]
    fn rejects_throwing_intermediate_arithmetic() {
        let source = "function onTick()a=input.getNumber(1)b=a%1251 a=a//1251 c=b/8 output.setNumber(1,a)output.setNumber(2,c)end";
        let out = output(source);
        assert!(out.contains("b=a%1251 a=a//1251 c=b/8"), "{out}");
    }

    #[test]
    fn rejects_unknown_calls_between_capture_and_use() {
        let out = output("a=123 x=1 z=f() y=a+1");
        assert!(out.starts_with("a=123"), "{out}");
    }

    #[test]
    fn ignores_reads_inside_nested_functions() {
        let out = output("a=123 function f()return a end x=1 y=a+1");
        assert!(out.starts_with("a=123"), "{out}");
    }

    #[test]
    fn global_capture_with_nested_function_read_is_not_hoisted() {
        let out = output("a=123 x=1 y=a function f() return a end");
        assert!(out.starts_with("a=123"), "{out}");
        assert!(out.contains("y=a"), "{out}");
    }

    #[test]
    fn global_copy_chain_with_nested_read_is_not_hoisted() {
        let out =
            output("delayfix=123 roy=1 timer=delayfix function onTick() px=(timer-delayfix) end");
        assert!(out.contains("delayfix=123"), "{out}");
        assert!(out.contains("timer=delayfix"), "{out}");
    }

    #[test]
    fn declaration_of_the_use_target_between_capture_and_use_blocks() {
        // Candidate Z (cycle 67): the use assigns T1, whose declaration
        // sits between the capture (ACC = 0) and the use. Hoisting the
        // use before the declaration let the declaration overwrite the
        // assigned value, so the arithmetic below saw the table.
        let out =
            output("ACC=0 local T1={x=1} T1=ACC output.setBool(8,(T1+input.getNumber(5))>(T1%2))");
        assert!(out.starts_with("ACC=0"), "{out}");
        assert!(out.contains("T1=ACC"), "{out}");
    }

    #[test]
    fn goto_or_label_between_capture_and_use_is_a_barrier() {
        // Candidate Y (cycle 67): the use sits after an unconditional
        // goto that jumps over it, so it must stay unexecuted; hoisting
        // it before the goto assigned it.
        let out = output("a=123 goto L b=a ::L:: c=1");
        assert!(out.starts_with("a=123"), "{out}");
        assert!(out.contains("b=a"), "{out}");
        // A bare label between capture and use can be jumped into from
        // later code, landing past the capture; it is a barrier too.
        let out = output("a=123 ::L:: b=a c=1");
        assert!(out.starts_with("a=123"), "{out}");
        assert!(out.contains("b=a"), "{out}");
    }

    #[test]
    fn break_or_return_between_capture_and_use_is_a_barrier() {
        // The use after `break` / `return` is unreachable on the
        // fall-through path; hoisting would execute it.
        let out = output("while true do a=123 break b=a end");
        assert!(out.contains("a=123"), "{out}");
        assert!(out.contains("b=a"), "{out}");
        let out = output("function f() a=123 return 1 b=a end");
        assert!(out.contains("a=123"), "{out}");
        assert!(out.contains("b=a"), "{out}");
    }

    #[test]
    fn global_copy_chain_without_nested_read_still_hoists() {
        let out = output("delayfix=123 roy=1 timer=delayfix");
        assert_eq!(out, "timer=123 roy=1");
    }
}
