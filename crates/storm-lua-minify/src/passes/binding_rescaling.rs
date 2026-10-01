//! Binding unit rescaling (`passes/binding-rescaling.ts`).
//!
//! A one-write finite numeric binding can be stored divided by an integer
//! factor. Reads are compensated by multiplication, while matching quotient
//! sites lose the division entirely. Exact bit/index/for/pack routes disable
//! the transformation.

use std::collections::HashMap;

use crate::pass::PassResult;
use crate::scope_rename::measure_renamed_size;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::numeric::{num_val, short_num};
use storm_lua_syntax::size::measure_size;

const EXACT_BINARY: &[&str] = &["&", "|", "~", "<<", ">>"];

#[derive(Clone)]
struct Initializer {
    statement: NodeId,
    expression_index: usize,
    bid: BindingId,
    name: String,
    value: f64,
}

#[derive(Clone)]
struct Candidate {
    initializer: Initializer,
    divisor: f64,
    divisor_expression: NodeId,
    estimate: usize,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn measured(ast: &Ast, root: NodeId, measure_renamed: bool) -> usize {
    if measure_renamed {
        measure_renamed_size(ast, root)
    } else {
        measure_size(ast, root)
    }
}

fn count_bid(ast: &Ast, res: &Resolution, node: NodeId, bid: BindingId) -> usize {
    let mut count = usize::from(
        matches!(ast.node(node), Node::Name(_))
            && res.node_bid.get(node as usize).copied().flatten() == Some(bid),
    );
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        count += count_bid(ast, res, child, bid);
    });
    count
}

fn direct_product_factor(ast: &Ast, res: &Resolution, node: NodeId, bid: BindingId) -> bool {
    let Node::Bin(op, left, right) = ast.node(node) else {
        return false;
    };
    op == "*"
        && ((matches!(ast.node(*left), Node::Name(_))
            && res.node_bid.get(*left as usize).copied().flatten() == Some(bid))
            || (matches!(ast.node(*right), Node::Name(_))
                && res.node_bid.get(*right as usize).copied().flatten() == Some(bid)))
}

fn exact_use(
    ast: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    bid: BindingId,
    exact: bool,
) -> bool {
    if matches!(ast.node(node), Node::Name(_)) {
        return res.node_bid.get(node as usize).copied().flatten() == Some(bid) && exact;
    }
    let exact_call = if let Node::Call(function, _, _) = ast.node(node) {
        matches!(
            analyzer.resolve_builtin_reference(*function).as_deref(),
            Some("string.pack" | "string.unpack")
        )
    } else {
        false
    };
    let mut found = false;
    storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |key, child| {
        if found || (matches!(ast.node(node), Node::Assign(..)) && key == "vs") {
            return;
        }
        let child_exact = exact
            || exact_call
            || matches!(ast.node(node), Node::Bin(op, _, _) if EXACT_BINARY.contains(&op.as_str()))
            || (matches!(ast.node(node), Node::Fornum(..)) && matches!(key, "a" | "b" | "c"))
            || (matches!(ast.node(node), Node::Index(..)) && key == "key");
        found = exact_use(ast, res, analyzer, child, bid, child_exact);
    });
    found
}

fn scaled_name(ast: &mut Ast, candidate: &Candidate) -> NodeId {
    ast.name(&candidate.initializer.name)
}

fn divisor_node(ast: &mut Ast, candidate: &Candidate) -> NodeId {
    ast.num(short_num(candidate.divisor))
}

fn compensated_name(ast: &mut Ast, candidate: &Candidate) -> NodeId {
    let name = scaled_name(ast, candidate);
    let divisor = divisor_node(ast, candidate);
    ast.bin("*", name, divisor)
}

fn rewrite_numerator(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    candidate: &Candidate,
) -> NodeId {
    if matches!(source.node(node), Node::Name(_))
        && res.node_bid.get(node as usize).copied().flatten() == Some(candidate.initializer.bid)
    {
        let name = scaled_name(target, candidate);
        target
            .nodes
            .derive_from(name, &source.nodes, node, "rescaled-name");
        return name;
    }
    if count_bid(source, res, node, candidate.initializer.bid) == 0 {
        return rewrite_node(target, source, res, node, candidate, false);
    }
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        rewrite_numerator(target, source, res, child, candidate)
    });
    let copy = target.push(mapped);
    target
        .nodes
        .derive_from(copy, &source.nodes, node, "binding-unit-rescaling");
    copy
}

fn rewrite_node(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    candidate: &Candidate,
    assignment_target: bool,
) -> NodeId {
    if !assignment_target
        && matches!(source.node(node), Node::Name(_))
        && res.node_bid.get(node as usize).copied().flatten() == Some(candidate.initializer.bid)
    {
        let first = target.nodes.len();
        let compensated = compensated_name(target, candidate);
        if source.nodes.tracks_origins() {
            for id in first..target.nodes.len() {
                let id = id as NodeId;
                let original = if matches!(target.node(id), Node::Num(_)) {
                    candidate.divisor_expression
                } else {
                    node
                };
                target
                    .nodes
                    .derive_from(id, &source.nodes, original, "rescaled-read-compensation");
            }
        }
        return compensated;
    }
    if let Node::Bin(op, left, right) = source.node(node) {
        if op == "/" {
            if let Node::Num(raw) = source.node(*right) {
                if num_val(raw) == candidate.divisor
                    && count_bid(source, res, *left, candidate.initializer.bid) == 1
                    && ((matches!(source.node(*left), Node::Name(_))
                        && res.node_bid.get(*left as usize).copied().flatten()
                            == Some(candidate.initializer.bid))
                        || direct_product_factor(source, res, *left, candidate.initializer.bid))
                {
                    let result = rewrite_numerator(target, source, res, *left, candidate);
                    target.nodes.relate_from(
                        result,
                        &source.nodes,
                        node,
                        "rescaled-quotient-elision",
                    );
                    return result;
                }
            }
        }
    }
    if let Node::Assign(targets, expressions) = source.node(node).clone() {
        let targets = targets
            .into_iter()
            .map(|child| rewrite_node(target, source, res, child, candidate, true))
            .collect::<Vec<_>>();
        let mut expressions = expressions
            .into_iter()
            .map(|child| rewrite_node(target, source, res, child, candidate, false))
            .collect::<Vec<_>>();
        if node == candidate.initializer.statement {
            let value = target.num(short_num(candidate.initializer.value / candidate.divisor));
            let Node::Assign(_, originals) = source.node(node) else {
                unreachable!()
            };
            let original = originals[candidate.initializer.expression_index];
            if source.nodes.origin(original).is_some()
                && source.nodes.origin(candidate.divisor_expression).is_some()
            {
                target
                    .nodes
                    .derive_from(value, &source.nodes, original, "rescaled-initializer");
                target.nodes.relate_from(
                    value,
                    &source.nodes,
                    candidate.divisor_expression,
                    "rescaled-initializer",
                );
            }
            expressions[candidate.initializer.expression_index] = value;
        }
        let copy = target.push(Node::Assign(targets, expressions));
        target
            .nodes
            .derive_from(copy, &source.nodes, node, "binding-unit-rescaling");
        return copy;
    }
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        rewrite_node(target, source, res, child, candidate, false)
    });
    let copy = target.push(mapped);
    target
        .nodes
        .derive_from(copy, &source.nodes, node, "binding-unit-rescaling");
    copy
}

fn apply_candidate(
    source: &Ast,
    root: NodeId,
    res: &Resolution,
    candidate: &Candidate,
) -> (Ast, NodeId) {
    let mut target = storm_lua_syntax::ast_utils::inherit_ast(source);
    let root = rewrite_node(&mut target, source, res, root, candidate, false);
    (target, root)
}

pub fn rescale_bindings_with_options(
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
    let mut current_root = root;
    let mut rescaled = 0usize;
    let mut considered = 0usize;
    for _ in 0..max_rounds {
        let source = clone_ast(ast);
        let res = resolve(&source, current_root);
        let analyzer = EffectAnalyzer::new(&source, &res, current_root, true);

        // ⚡ Bolt: Cache a flat vector of AST nodes directly with a single traversal (`walk`)
        // to avoid repeatedly running a full recursive tree traversal during write counting,
        // initializer discovery, and per-initializer divisor scans, significantly reducing
        // recursive calls and AST walk overhead.
        let mut ast_nodes = Vec::new();
        storm_lua_syntax::ast_utils::walk(&source, current_root, &mut |node| {
            ast_nodes.push(node);
        });

        let mut writes = vec![0u32; res.bindings.len()];
        for &node in &ast_nodes {
            if matches!(source.node(node), Node::Name(_))
                && res.node_write.get(node as usize).copied().unwrap_or(false)
            {
                if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
                    writes[bid as usize] += 1;
                }
            }
        }
        let mut initializers = Vec::new();
        for &node in &ast_nodes {
            let Node::Assign(targets, expressions) = source.node(node) else {
                continue;
            };
            for index in 0..targets.len() {
                let Some(target) = targets.get(index) else {
                    continue;
                };
                let Some(expression) = expressions.get(index) else {
                    continue;
                };
                if !matches!(source.node(*target), Node::Name(_)) {
                    continue;
                }
                let Node::Num(raw) = source.node(*expression) else {
                    continue;
                };
                let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() else {
                    continue;
                };
                let value = num_val(raw);
                let binding = res.binding(bid);
                if binding.fixed
                    || !value.is_finite()
                    || value == 0.0
                    || writes.get(bid as usize).copied().unwrap_or(0) != 1
                {
                    continue;
                }
                initializers.push(Initializer {
                    statement: node,
                    expression_index: index,
                    bid,
                    name: source.strings.get(binding.name).to_string(),
                    value,
                });
            }
        }

        let mut candidates = Vec::new();
        for initializer in initializers {
            if exact_use(
                &source,
                &res,
                &analyzer,
                current_root,
                initializer.bid,
                false,
            ) {
                continue;
            }
            let mut divisors = HashMap::<u64, (f64, usize, NodeId)>::new();
            for &node in &ast_nodes {
                let Node::Bin(op, left, right) = source.node(node) else {
                    continue;
                };
                if op != "/"
                    || count_bid(&source, &res, *left, initializer.bid) != 1
                    || !((matches!(source.node(*left), Node::Name(_))
                        && res.node_bid.get(*left as usize).copied().flatten()
                            == Some(initializer.bid))
                        || direct_product_factor(&source, &res, *left, initializer.bid))
                {
                    continue;
                }
                let Node::Num(raw) = source.node(*right) else {
                    continue;
                };
                let divisor = num_val(raw);
                if !divisor.is_finite() || divisor.fract() != 0.0 || divisor.abs() < 2.0 {
                    continue;
                }
                let entry = divisors
                    .entry(divisor.to_bits())
                    .or_insert((divisor, 0usize, *right));
                entry.1 += 1;
            }
            for (_, (divisor, removable, divisor_expression)) in divisors {
                candidates.push(Candidate {
                    initializer: initializer.clone(),
                    divisor,
                    divisor_expression,
                    estimate: removable * (short_num(divisor).len() + 1),
                });
            }
        }
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.estimate));
        let baseline = measured(&source, current_root, measure_renamed);
        let mut best: Option<(Ast, NodeId, usize)> = None;
        for candidate in candidates.into_iter().take(32) {
            considered += 1;
            let (transformed, transformed_root) =
                apply_candidate(&source, current_root, &res, &candidate);
            let size = measured(&transformed, transformed_root, measure_renamed);
            if size < baseline && best.as_ref().is_none_or(|entry| size < entry.2) {
                best = Some((transformed, transformed_root, size));
            }
        }
        let Some((transformed, transformed_root, _)) = best else {
            break;
        };
        *ast = transformed;
        current_root = transformed_root;
        rescaled += 1;
    }
    PassResult {
        root: current_root,
        saved: Some(original.saturating_sub(measured(ast, current_root, measure_renamed)) as u64),
        details: if rescaled == 0 && considered == 0 {
            None
        } else {
            Some(vec![
                format!("rescaled={rescaled}"),
                format!("considered={considered}"),
            ])
        },
    }
}

pub fn rescale_bindings(ast: &mut Ast, root: NodeId) -> PassResult {
    rescale_bindings_with_options(ast, root, true, true, 4)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = rescale_bindings(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn stores_binding_in_divided_units() {
        let source = "size=192 function onTick()x=input.getNumber(1)output.setNumber(1,(size/2+x*size/2)//1)output.setNumber(2,size)end";
        let out = output(source);
        assert!(out.contains("size=96"), "{out}");
        assert!(!out.contains("size/2"), "{out}");
        assert!(out.contains("size*2"), "{out}");
    }

    #[test]
    fn rejects_bit_exact_route() {
        let source = "size=192 function onTick()output.setNumber(1,(size/2)&255)end";
        assert!(!output(source).contains("size=96"));
    }

    #[test]
    fn requires_single_write_initializer() {
        let source = "size=192 size=96 output.setNumber(1,size/2)";
        assert!(!output(source).contains("size=48"));
    }
}
