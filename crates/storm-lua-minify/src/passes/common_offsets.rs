//! Common additive offset absorption (`passes/common-offsets.ts`).
//!
//! Repeated `x+d` / `-x-d` terms inside one straight-line statement may absorb
//! the offset into `x` immediately before that statement when `x` is definitely
//! overwritten before its next observation. Candidates are ranked and measured
//! after scope renaming, exactly like the TypeScript oracle.

use std::cmp::Ordering;
use std::collections::HashSet;

use crate::pass::PassResult;
use crate::scope_rename::measure_renamed_size;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::print::Printer;

#[derive(Clone, Copy)]
struct SignedTerm {
    sign: i8,
    node: NodeId,
}

#[derive(Clone)]
struct Candidate {
    block: NodeId,
    statement: usize,
    bid: BindingId,
    name: String,
    offset: NodeId,
    offset_key: String,
    relative_sign: i8,
    estimate: usize,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn measured(ast: &Ast, root: NodeId) -> usize {
    measure_renamed_size(ast, root)
}

fn node_bid(resolution: &Resolution, node: NodeId) -> Option<BindingId> {
    resolution.node_bid.get(node as usize).copied().flatten()
}

fn is_control(node: &Node) -> bool {
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

fn signed_terms(ast: &Ast, node: NodeId, sign: i8, output: &mut Vec<SignedTerm>) {
    match ast.node(node) {
        Node::Bin(op, left, right) if op == "+" => {
            signed_terms(ast, *left, sign, output);
            signed_terms(ast, *right, sign, output);
        }
        Node::Bin(op, left, right) if op == "-" => {
            signed_terms(ast, *left, sign, output);
            signed_terms(ast, *right, -sign, output);
        }
        Node::Un(op, inner) if op == "-" => signed_terms(ast, *inner, -sign, output),
        _ => output.push(SignedTerm { sign, node }),
    }
}

fn terms(ast: &Ast, node: NodeId) -> Vec<SignedTerm> {
    let mut output = Vec::new();
    signed_terms(ast, node, 1, &mut output);
    output
}

fn count_bid(ast: &Ast, resolution: &Resolution, node: NodeId, bid: BindingId) -> usize {
    let mut count = usize::from(
        matches!(ast.node(node), Node::Name(_)) && node_bid(resolution, node) == Some(bid),
    );
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        count += count_bid(ast, resolution, child, bid);
    });
    count
}

fn expression_roots(ast: &Ast, node: NodeId, output: &mut Vec<NodeId>) {
    if matches!(ast.node(node), Node::Bin(op, _, _) if op == "+" || op == "-")
        || matches!(ast.node(node), Node::Un(op, _) if op == "-")
    {
        output.push(node);
        return;
    }
    if matches!(ast.node(node), Node::Block(_) | Node::Function(..)) {
        return;
    }
    storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |key, child| {
        if !(matches!(ast.node(node), Node::Assign(..)) && key == "vs") {
            expression_roots(ast, child, output);
        }
    });
}

fn expression_key(ast: &Ast, node: NodeId) -> String {
    Printer::new(ast, false).expr_public(node)
}

fn inspect_nested_blocks(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    candidates: &mut Vec<Candidate>,
) {
    match ast.node(node) {
        Node::Block(_) => {
            inspect_block(ast, resolution, analyzer, node, candidates);
            return;
        }
        Node::Function(_, _, body) => {
            inspect_block(ast, resolution, analyzer, *body, candidates);
            return;
        }
        _ => {}
    }
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        inspect_nested_blocks(ast, resolution, analyzer, child, candidates);
    });
}

fn inspect_block(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    block: NodeId,
    candidates: &mut Vec<Candidate>,
) {
    let Node::Block(statements) = ast.node(block) else {
        return;
    };
    for (statement_index, statement) in statements.iter().copied().enumerate() {
        let effect = analyzer.effects_for_statement(statement);
        if effect.calls || effect.ordered || is_control(ast.node(statement)) {
            continue;
        }
        let mut roots = Vec::new();
        expression_roots(ast, statement, &mut roots);
        for expression in &roots {
            let signed = terms(ast, *expression);
            for (variable_index, variable) in signed.iter().enumerate() {
                if !matches!(ast.node(variable.node), Node::Name(_)) {
                    continue;
                }
                let Some(bid) = node_bid(resolution, variable.node) else {
                    continue;
                };
                let binding = &resolution.bindings[bid as usize];
                if binding.fixed || effect.writes.contains(&bid) {
                    continue;
                }
                for (offset_index, offset) in signed.iter().enumerate() {
                    if offset_index == variable_index
                        || count_bid(ast, resolution, offset.node, bid) != 0
                    {
                        continue;
                    }
                    let offset_effect = analyzer.effects_for_expr(offset.node);
                    if offset_effect.calls
                        || offset_effect.ordered
                        || !offset_effect.writes.is_empty()
                        || !offset_effect.stable
                    {
                        continue;
                    }
                    let occurrences = roots
                        .iter()
                        .filter(|root| count_bid(ast, resolution, **root, bid) != 0)
                        .count();
                    if occurrences < 2 {
                        continue;
                    }
                    let offset_key = expression_key(ast, offset.node);
                    candidates.push(Candidate {
                        block,
                        statement: statement_index,
                        bid,
                        name: ast.strings.get(binding.name).to_string(),
                        offset: offset.node,
                        offset_key: offset_key.clone(),
                        relative_sign: if offset.sign == variable.sign { 1 } else { -1 },
                        estimate: occurrences * offset_key.len() - offset_key.len(),
                    });
                }
            }
        }
    }
    for statement in statements {
        inspect_nested_blocks(ast, resolution, analyzer, *statement, candidates);
    }
}

fn clone_subtree(target: &mut Ast, source: &Ast, node: NodeId) -> NodeId {
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_subtree(target, source, child)
    });
    let copied = target.push(mapped);
    target
        .nodes
        .derive_from(copied, &source.nodes, node, "common-offset-copy");
    copied
}

fn build_sum(target: &mut Ast, source: &Ast, signed: &[SignedTerm], original: NodeId) -> NodeId {
    let first = signed[0];
    let first_node = clone_subtree(target, source, first.node);
    let mut result = if first.sign == 1 {
        first_node
    } else {
        {
            let result = target.un("-", first_node);
            target
                .nodes
                .derive_from(result, &source.nodes, original, "common-offset-sum");
            result
        }
    };
    for term in &signed[1..] {
        let right = clone_subtree(target, source, term.node);
        result = target.bin(if term.sign == 1 { "+" } else { "-" }, result, right);
        target
            .nodes
            .derive_from(result, &source.nodes, original, "common-offset-sum");
    }
    result
}

struct RewriteState {
    matched: usize,
    failed: bool,
}

fn rewrite_statement_node(
    target: &mut Ast,
    source: &Ast,
    resolution: &Resolution,
    node: NodeId,
    candidate: &Candidate,
    state: &mut RewriteState,
) -> NodeId {
    let additive = matches!(source.node(node), Node::Bin(op, _, _) if op == "+" || op == "-")
        || matches!(source.node(node), Node::Un(op, _) if op == "-");
    if additive {
        if count_bid(source, resolution, node, candidate.bid) == 0 {
            return node;
        }
        let signed = terms(source, node);
        let variables = signed
            .iter()
            .enumerate()
            .filter(|(_, term)| {
                matches!(source.node(term.node), Node::Name(_))
                    && node_bid(resolution, term.node) == Some(candidate.bid)
            })
            .collect::<Vec<_>>();
        if variables.len() != 1 {
            state.failed = true;
            return node;
        }
        let (variable_index, variable) = variables[0];
        let expected_sign = if variable.sign == candidate.relative_sign {
            1
        } else {
            -1
        };
        let Some(offset_index) = signed.iter().enumerate().position(|(index, term)| {
            index != variable_index
                && term.sign == expected_sign
                && expression_key(source, term.node) == candidate.offset_key
        }) else {
            state.failed = true;
            return node;
        };
        state.matched += 1;
        let remaining = signed
            .into_iter()
            .enumerate()
            .filter_map(|(index, term)| (index != offset_index).then_some(term))
            .collect::<Vec<_>>();
        return build_sum(target, source, &remaining, node);
    }
    if matches!(source.node(node), Node::Block(_) | Node::Function(..)) {
        return node;
    }
    if let Node::Assign(targets, expressions) = source.node(node) {
        let targets = targets.clone();
        let expressions = expressions
            .iter()
            .map(|expression| {
                rewrite_statement_node(target, source, resolution, *expression, candidate, state)
            })
            .collect::<Vec<_>>();
        let result = target.push(Node::Assign(targets, expressions));
        target
            .nodes
            .derive_from(result, &source.nodes, node, "common-offset-rewrite");
        return result;
    }
    let original = source.node(node).clone();
    let (mapped, changed) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        rewrite_statement_node(target, source, resolution, child, candidate, state)
    });
    if changed {
        let result = target.push(mapped);
        target
            .nodes
            .derive_from(result, &source.nodes, node, "common-offset-rewrite");
        result
    } else {
        node
    }
}

fn direct_assignment_writes(
    ast: &Ast,
    resolution: &Resolution,
    statement: NodeId,
    bid: BindingId,
) -> bool {
    let Node::Assign(targets, _) = ast.node(statement) else {
        return false;
    };
    targets.iter().any(|target| {
        matches!(ast.node(*target), Node::Name(_)) && node_bid(resolution, *target) == Some(bid)
    })
}

fn has_definite_overwrite(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    candidate: &Candidate,
) -> bool {
    let Node::Block(statements) = ast.node(candidate.block) else {
        return false;
    };
    for statement in statements.iter().copied().skip(candidate.statement + 1) {
        let effect = analyzer.effects_for_statement(statement);
        if effect.reads.contains(&candidate.bid) {
            break;
        }
        if effect.writes.contains(&candidate.bid) {
            return direct_assignment_writes(ast, resolution, statement, candidate.bid)
                && !effect.calls
                && !effect.ordered;
        }
        if effect.calls || effect.ordered || is_control(ast.node(statement)) {
            break;
        }
    }
    false
}

fn apply_candidate(
    source: &Ast,
    root: NodeId,
    resolution: &Resolution,
    candidate: &Candidate,
) -> Option<(Ast, NodeId)> {
    let Node::Block(source_statements) = source.node(candidate.block) else {
        return None;
    };
    let original_statement = source_statements[candidate.statement];
    let total = count_bid(source, resolution, original_statement, candidate.bid);
    let mut target = clone_ast(source);
    let mut state = RewriteState {
        matched: 0,
        failed: false,
    };
    let rewritten = rewrite_statement_node(
        &mut target,
        source,
        resolution,
        original_statement,
        candidate,
        &mut state,
    );
    if state.failed || state.matched < 2 || state.matched != total {
        return None;
    }

    let write_target = target.name(&candidate.name);
    let current_value = target.name(&candidate.name);
    let offset = clone_subtree(&mut target, source, candidate.offset);
    let updated = target.bin(
        if candidate.relative_sign == 1 {
            "+"
        } else {
            "-"
        },
        current_value,
        offset,
    );
    let update = target.assign(vec![write_target], vec![updated]);
    if target.nodes.tracks_origins() {
        let mut reads = Vec::new();
        storm_lua_syntax::ast_utils::walk(source, original_statement, &mut |id| {
            if matches!(source.node(id), Node::Name(_))
                && node_bid(resolution, id) == Some(candidate.bid)
            {
                reads.push(id);
            }
        });
        super::origins::derive(
            &mut target,
            write_target,
            source,
            &reads,
            "common-offset-storage",
        );
        super::origins::derive(
            &mut target,
            current_value,
            source,
            &reads,
            "common-offset-read",
        );
        reads.push(candidate.offset);
        super::origins::derive(
            &mut target,
            updated,
            source,
            &reads,
            "common-offset-adjustment",
        );
        target.nodes.mark_synthetic(update, "common-offset-storage");
    }
    let mut statements = source_statements.clone();
    statements[candidate.statement] = rewritten;
    statements.insert(candidate.statement, update);
    target.nodes.rewrite(
        candidate.block,
        Node::Block(statements),
        "common-offset-absorption",
    );
    Some((target, root))
}

pub fn absorb_common_offsets_with_options(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    max_rounds: usize,
) -> PassResult {
    if !aggressive {
        return PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["absorbed=0".to_string(), "considered=0".to_string()]),
        };
    }
    let original_size = measured(ast, root);
    let mut absorbed = 0usize;
    let mut considered = 0usize;

    for _ in 0..max_rounds {
        let source = clone_ast(ast);
        let resolution = resolve(&source, root);
        let analyzer = EffectAnalyzer::new(&source, &resolution, root, true);
        let baseline = measured(&source, root);
        let mut candidates = Vec::new();
        inspect_block(&source, &resolution, &analyzer, root, &mut candidates);
        candidates.sort_by(|left, right| right.estimate.cmp(&left.estimate).then(Ordering::Equal));

        let mut best: Option<(Ast, NodeId, usize)> = None;
        let mut seen = HashSet::new();
        for candidate in candidates.into_iter().take(48) {
            let scope = resolution
                .node_scope_id
                .get(candidate.block as usize)
                .copied()
                .flatten()
                .unwrap_or(0);
            let identity = format!(
                "{scope}:{}:{}:{}:{}",
                candidate.statement, candidate.bid, candidate.relative_sign, candidate.offset_key
            );
            if !seen.insert(identity) {
                continue;
            }
            considered += 1;
            if !has_definite_overwrite(&source, &resolution, &analyzer, &candidate) {
                continue;
            }
            let Some((trial, trial_root)) = apply_candidate(&source, root, &resolution, &candidate)
            else {
                continue;
            };
            let size = measured(&trial, trial_root);
            if size < baseline
                && best
                    .as_ref()
                    .is_none_or(|(_, _, best_size)| size < *best_size)
            {
                best = Some((trial, trial_root, size));
            }
        }
        let Some((best_ast, best_root, _)) = best else {
            break;
        };
        ast.nodes = best_ast.nodes;
        ast.strings = best_ast.strings;
        debug_assert_eq!(best_root, root);
        absorbed += 1;
    }

    PassResult {
        root,
        saved: Some(original_size.saturating_sub(measured(ast, root)) as u64),
        details: Some(vec![
            format!("absorbed={absorbed}"),
            format!("considered={considered}"),
        ]),
    }
}

pub fn absorb_common_offsets(ast: &mut Ast, root: NodeId, aggressive: bool) -> PassResult {
    absorb_common_offsets_with_options(ast, root, aggressive, 8)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::{parse_source, Printer};

    fn output(source: &str, aggressive: bool) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = absorb_common_offsets(&mut ast, root, aggressive);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn absorbs_repeated_offset_before_definite_overwrite() {
        let source = "function onTick()a=input.getNumber(1)b=input.getNumber(2)c=input.getNumber(3)t={-a+b+.022+c,a+b+.022+c,-a-b-.022+c,a-b-.022+c}b=0 output.setNumber(1,t[1])end";
        let result = output(source, true);
        assert!(result.contains("b=b+.022"), "{result}");
        assert!(!result.contains("b+.022+c"), "{result}");
    }

    #[test]
    fn requires_overwrite_before_next_observation() {
        let source =
            "function onTick()b=input.getNumber(1)t={b+.022,b+.022}output.setNumber(1,b)end";
        assert_eq!(output(source, true), source);
    }

    #[test]
    fn calls_are_barriers_to_the_overwrite_proof() {
        let source =
            "function onTick()b=input.getNumber(1)t={b+.022,b+.022}output.setNumber(1,b)b=0 end";
        assert_eq!(output(source, true), source);
    }

    #[test]
    fn aggressive_mode_is_required() {
        let source = "function onTick()b=1 t={b+.022,b+.022}b=0 end";
        assert_eq!(output(source, false), source);
    }
}
