//! Captured single-use hoisting (`passes/captured-use-hoisting.ts`).
//!
//! A movable value stored only to survive intervening statements can be
//! substituted into its sole later assignment and moved before those
//! statements. Control flow, calls, ordered/throwing effects, target accesses,
//! and dependency writes are barriers.

use std::collections::HashSet;

use super::immutable_values::contains_fresh_reference;
use crate::pass::PassResult;
use storm_lua_analysis::effects::EffectAnalyzer;
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

fn clone_subtree(target: &mut Ast, source: &Ast, node: NodeId) -> NodeId {
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_subtree(target, source, child)
    });
    let cloned = target.push(mapped);
    target
        .nodes
        .derive_from(cloned, &source.nodes, node, "captured-single-use-hoisting");
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
    target
        .nodes
        .derive_from(cloned, &source.nodes, node, "captured-single-use-hoisting");
    cloned
}

fn control_barrier(node: &Node) -> bool {
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
) -> NodeId {
    if matches!(source.node(node), Node::Block(_)) {
        return transform_block(target, source, res, analyzer, node, hoisted);
    }
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        transform_node(target, source, res, analyzer, child, hoisted)
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
) -> NodeId {
    let Node::Block(statements) = source.node(block).clone() else {
        unreachable!("transform_block requires block")
    };
    let mut nested = statements
        .into_iter()
        .map(|statement| transform_node(target, source, res, analyzer, statement, hoisted))
        .collect::<Vec<_>>();

    for from in 0..nested.len().saturating_sub(2) {
        let capture = nested[from];
        let Node::Assign(targets, expressions) = target.node(capture).clone() else {
            continue;
        };
        if targets.len() != 1
            || expressions.len() != 1
            || !matches!(target.node(targets[0]), Node::Name(_))
        {
            continue;
        }
        let Some(bid) = res.node_bid.get(targets[0] as usize).copied().flatten() else {
            continue;
        };
        let captured = expressions[0];
        if !analyzer.is_movable(captured) || contains_fresh_reference(source, captured) {
            continue;
        }

        let mut use_at = None;
        for (at, statement) in nested.iter().enumerate().skip(from + 1) {
            if scoped_read_count(source, res, *statement, bid) > 0 {
                use_at = Some(at);
                break;
            }
            let effect = analyzer.effects_for_statement(*statement);
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
            continue;
        }
        if use_at < from + 2 {
            continue;
        }
        let use_statement = nested[use_at];
        let Node::Assign(use_targets, use_expressions) = target.node(use_statement).clone() else {
            continue;
        };
        if use_targets.len() != 1
            || use_expressions.len() != 1
            || !matches!(target.node(use_targets[0]), Node::Name(_))
            || scoped_read_count(source, res, use_statement, bid) != 1
            || !analyzer.is_movable(use_expressions[0])
        {
            continue;
        }
        let Some(target_bid) = res.node_bid.get(use_targets[0] as usize).copied().flatten() else {
            continue;
        };
        let use_effect = analyzer.effects_for_expr(use_expressions[0]);
        let late_reads = use_effect
            .reads
            .iter()
            .copied()
            .filter(|read| *read != bid)
            .collect::<HashSet<_>>();
        let mut blocked = false;
        for statement in &nested[from + 1..use_at] {
            let effect = analyzer.effects_for_statement(*statement);
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
            continue;
        }
        nested.splice(from..=use_at, replacement);
        *hoisted += 1;
        break;
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
    let out = transform_block(ast, &source, &res, &analyzer, root, &mut hoisted);
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
}
