//! Interval origin shifting (`passes/interval-shifting.ts`).

use crate::pass::PassResult;
use crate::scope_rename::scope_rename_fast;
use storm_lua_analysis::effects::{ast_same, EffectAnalyzer};
use storm_lua_analysis::resolver::{resolve, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::size::measure_size;

#[derive(Clone)]
struct Candidate {
    point: NodeId,
    name: String,
    block: NodeId,
    statement: usize,
    conditions: Vec<NodeId>,
    width: NodeId,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn clone_subtree(target: &mut Ast, source: &Ast, node: NodeId) -> NodeId {
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_subtree(target, source, child)
    });
    let copied = target.push(mapped);
    target
        .nodes
        .derive_from(copied, &source.nodes, node, "interval-origin-copy");
    copied
}

fn measured(ast: &Ast, root: NodeId, measure_renamed: bool) -> usize {
    if measure_renamed {
        let renamed = scope_rename_fast(ast, root);
        measure_size(&renamed.ast, renamed.root)
    } else {
        measure_size(ast, root)
    }
}

fn without_parens(ast: &Ast, mut node: NodeId) -> NodeId {
    while let Node::Paren(inner) = ast.node(node) {
        node = *inner;
    }
    node
}

fn and_terms(ast: &Ast, node: NodeId, output: &mut Vec<NodeId>) {
    let node = without_parens(ast, node);
    if let Node::Bin(op, left, right) = ast.node(node) {
        if op == "and" {
            and_terms(ast, *left, output);
            and_terms(ast, *right, output);
            return;
        }
    }
    output.push(node);
}

fn lower_bound(ast: &Ast, res: &Resolution, node: NodeId) -> Option<(NodeId, BindingId)> {
    let node = without_parens(ast, node);
    let Node::Bin(op, left, right) = ast.node(node) else {
        return None;
    };
    let (point, lower) = match op.as_str() {
        ">=" => (*left, without_parens(ast, *right)),
        "<=" => (*right, without_parens(ast, *left)),
        _ => return None,
    };
    if !matches!(ast.node(lower), Node::Name(_)) {
        return None;
    }
    Some((point, res.node_bid.get(lower as usize).copied().flatten()?))
}

fn upper_width(
    ast: &Ast,
    res: &Resolution,
    node: NodeId,
    point: NodeId,
    lower_bid: BindingId,
) -> Option<NodeId> {
    let node = without_parens(ast, node);
    let Node::Bin(op, left, right) = ast.node(node) else {
        return None;
    };
    let (candidate_point, upper) = match op.as_str() {
        "<=" => (*left, without_parens(ast, *right)),
        ">=" => (*right, without_parens(ast, *left)),
        _ => return None,
    };
    if !ast_same(ast, candidate_point, point) {
        return None;
    }
    let Node::Bin(op, left, right) = ast.node(upper) else {
        return None;
    };
    if op != "+" {
        return None;
    }
    let left = without_parens(ast, *left);
    let right = without_parens(ast, *right);
    if matches!(ast.node(left), Node::Name(_))
        && res.node_bid.get(left as usize).copied().flatten() == Some(lower_bid)
    {
        return Some(right);
    }
    if matches!(ast.node(right), Node::Name(_))
        && res.node_bid.get(right as usize).copied().flatten() == Some(lower_bid)
    {
        return Some(left);
    }
    None
}

fn build_and(ast: &mut Ast, terms: Vec<NodeId>, source: &Ast, original: NodeId) -> NodeId {
    let mut iter = terms.into_iter();
    #[expect(
        clippy::expect_used,
        reason = "Interval candidates contain at least two proven comparison terms before rebuilding the conjunction"
    )]
    let mut result = iter.next().expect("at least two interval terms");
    for term in iter {
        result = ast.bin("and", result, term);
        ast.nodes
            .derive_from(result, &source.nodes, original, "interval-conjunction");
    }
    result
}

fn collect_candidates(
    ast: &Ast,
    root: NodeId,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
) -> Vec<Candidate> {
    let mut blocks = Vec::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |node| {
        if matches!(ast.node(node), Node::Block(_)) {
            blocks.push(node);
        }
    });
    let mut candidates = Vec::new();
    for block in blocks {
        let Node::Block(statements) = ast.node(block) else {
            continue;
        };
        for (statement, node) in statements.iter().enumerate() {
            let Node::Return(expressions) = ast.node(*node) else {
                continue;
            };
            if expressions.len() != 1 {
                continue;
            }
            let mut conditions = Vec::new();
            and_terms(ast, expressions[0], &mut conditions);
            if conditions.len() < 2 {
                continue;
            }
            let Some((point, bid)) = lower_bound(ast, res, conditions[0]) else {
                continue;
            };
            let binding = res.binding(bid);
            if !matches!(binding.kind, BindingKind::Param) || binding.fixed {
                continue;
            }
            let Some(width) = upper_width(ast, res, conditions[1], point, bid) else {
                continue;
            };
            let point_effect = analyzer.effects_for_expr(point);
            let width_effect = analyzer.effects_for_expr(width);
            if point_effect.calls
                || point_effect.ordered
                || !point_effect.writes.is_empty()
                || !point_effect.stable
                || width_effect.calls
                || width_effect.ordered
                || !width_effect.writes.is_empty()
                || width_effect.reads.contains(&bid)
            {
                continue;
            }
            let mut uses = 0usize;
            let mut has_write = false;
            storm_lua_syntax::ast_utils::walk(ast, root, &mut |item| {
                if matches!(ast.node(item), Node::Name(_))
                    && res.node_bid.get(item as usize).copied().flatten() == Some(bid)
                {
                    uses += 1;
                    has_write |= res.node_write.get(item as usize).copied().unwrap_or(false);
                }
            });
            if uses != 2 || has_write {
                continue;
            }
            candidates.push(Candidate {
                point,
                name: ast.strings.get(binding.name).to_string(),
                block,
                statement,
                conditions,
                width,
            });
        }
    }
    candidates
}

fn apply_candidate(source: &Ast, candidate: &Candidate) -> Ast {
    let mut ast = clone_ast(source);
    let point = clone_subtree(&mut ast, source, candidate.point);
    let lower_for_subtraction = ast.name(&candidate.name);
    let shifted = ast.bin("-", point, lower_for_subtraction);
    let assign_target = ast.name(&candidate.name);
    let assignment = ast.assign(vec![assign_target], vec![shifted]);

    let lower_for_zero = ast.name(&candidate.name);
    let zero = ast.num("0".to_string());
    let first = ast.bin(">=", lower_for_zero, zero);
    let lower_for_width = ast.name(&candidate.name);
    let width = clone_subtree(&mut ast, source, candidate.width);
    let second = ast.bin("<=", lower_for_width, width);
    let mut conditions = vec![first, second];
    conditions.extend(
        candidate
            .conditions
            .iter()
            .skip(2)
            .map(|condition| clone_subtree(&mut ast, source, *condition)),
    );
    let Node::Block(original_statements) = source.node(candidate.block) else {
        unreachable!()
    };
    let original_return = original_statements[candidate.statement];
    let Node::Return(original_expressions) = source.node(original_return) else {
        unreachable!()
    };
    let expression = build_and(&mut ast, conditions, source, original_expressions[0]);
    let replacement = ast.push(Node::Return(vec![expression]));
    let Node::Block(statements) = ast.node(candidate.block).clone() else {
        unreachable!()
    };
    let mut statements = statements;
    statements[candidate.statement] = replacement;
    statements.insert(candidate.statement, assignment);
    let Node::Bin(op, l, r) = source.node(without_parens(source, candidate.conditions[0])) else {
        unreachable!()
    };
    let lower = if op == ">=" { *r } else { *l };
    for name in [
        lower_for_subtraction,
        assign_target,
        lower_for_zero,
        lower_for_width,
    ] {
        ast.nodes
            .derive_from(name, &source.nodes, lower, "shifted-interval-binding");
    }
    super::origins::derive(
        &mut ast,
        shifted,
        source,
        &[candidate.point, lower],
        "interval-origin-subtraction",
    );
    ast.nodes
        .mark_synthetic(assignment, "interval-origin-storage");
    super::origins::derive(
        &mut ast,
        zero,
        source,
        &[candidate.point, lower],
        "interval-zero-bound",
    );
    ast.nodes.derive_from(
        first,
        &source.nodes,
        candidate.conditions[0],
        "interval-lower-bound",
    );
    ast.nodes.derive_from(
        second,
        &source.nodes,
        candidate.conditions[1],
        "interval-upper-bound",
    );
    ast.nodes.derive_from(
        replacement,
        &source.nodes,
        original_return,
        "shifted-interval-return",
    );
    ast.nodes.rewrite(
        candidate.block,
        Node::Block(statements),
        "interval-origin-shifting",
    );
    ast
}

pub fn shift_interval_origins_with_options(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    measure_renamed: bool,
    max_rounds: usize,
) -> PassResult {
    if !aggressive {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let original = measured(ast, root, measure_renamed);
    let mut shifted = 0usize;
    let mut considered = 0usize;
    for _ in 0..max_rounds {
        let source = clone_ast(ast);
        let res = resolve(&source, root);
        let analyzer = EffectAnalyzer::new(&source, &res, root, true);
        let candidates = collect_candidates(&source, root, &res, &analyzer);
        let baseline = measured(&source, root, measure_renamed);
        let mut best: Option<(Ast, usize)> = None;
        for candidate in candidates.into_iter().take(32) {
            considered += 1;
            let transformed = apply_candidate(&source, &candidate);
            let size = measured(&transformed, root, measure_renamed);
            if size < baseline && best.as_ref().is_none_or(|entry| size < entry.1) {
                best = Some((transformed, size));
            }
        }
        let Some((transformed, _)) = best else {
            break;
        };
        *ast = transformed;
        shifted += 1;
    }
    PassResult {
        root,
        saved: Some(original.saturating_sub(measured(ast, root, measure_renamed)) as u64),
        details: if shifted == 0 && considered == 0 {
            None
        } else {
            Some(vec![
                format!("shifted={shifted}"),
                format!("considered={considered}"),
            ])
        },
    }
}

pub fn shift_interval_origins(ast: &mut Ast, root: NodeId) -> PassResult {
    shift_interval_origins_with_options(ast, root, true, true, 4)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = shift_interval_origins(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn shifts_two_sided_interval_to_zero_origin() {
        let source = "function inside(lower,width)return input.getNumber(1)>=lower and input.getNumber(1)<=lower+width end";
        let out = output(source);
        assert!(out.contains("lower=input.getNumber(1)-lower"), "{out}");
        assert!(out.contains("return lower>=0 and lower<=width"), "{out}");
    }

    #[test]
    fn requires_exactly_two_parameter_uses() {
        let source = "function inside(lower,point,width)return point>=lower and point<=lower+width and lower>1 end";
        assert!(!output(source).contains("lower=point-lower"));
    }

    #[test]
    fn rejects_width_that_depends_on_lower() {
        let source =
            "function inside(lower,point)return point>=lower and point<=lower+(lower+1)end";
        assert!(!output(source).contains("lower=point-lower"));
    }
}
