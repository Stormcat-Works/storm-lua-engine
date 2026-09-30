//! Coefficient carrier synthesis (`passes/coefficient-carriers.ts`).

use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use crate::scope_rename::scope_rename_fast;
use storm_lua_analysis::resolver::{resolve, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::size::{measure_expr, measure_size};

#[derive(Clone, Copy)]
struct Factor {
    sign: i8,
    node: NodeId,
}

#[derive(Clone)]
struct Occurrence {
    node: NodeId,
    factors: Vec<Factor>,
    binding_factor: usize,
    constant_factor: usize,
    estimate: isize,
}

#[derive(Clone)]
struct Group {
    key: String,
    binding_name: storm_lua_syntax::ast::SymbolId,
    binding_sign: i8,
    constant: NodeId,
    constant_sign: i8,
    declaration: usize,
    occurrences: Vec<Occurrence>,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn renamed_size(ast: &Ast, root: NodeId) -> usize {
    let renamed = scope_rename_fast(ast, root);
    measure_size(&renamed.ast, renamed.root)
}

fn product_factors(ast: &Ast, node: NodeId, sign: i8, out: &mut Vec<Factor>) {
    match ast.node(node) {
        Node::Bin(operator, left, right) if operator == "*" => {
            product_factors(ast, *left, sign, out);
            product_factors(ast, *right, sign, out);
        }
        Node::Bin(operator, left, right) if operator == "/" => {
            product_factors(ast, *left, sign, out);
            product_factors(ast, *right, -sign, out);
        }
        _ => out.push(Factor { sign, node }),
    }
}

fn build_product(ast: &mut Ast, factors: &[Factor]) -> Option<NodeId> {
    let positives = factors
        .iter()
        .copied()
        .filter(|factor| factor.sign == 1)
        .collect::<Vec<_>>();
    let negatives = factors
        .iter()
        .copied()
        .filter(|factor| factor.sign == -1)
        .collect::<Vec<_>>();
    let mut result = positives.first()?.node;
    for factor in positives.iter().skip(1) {
        result = ast.push(Node::Bin("*".to_string(), result, factor.node));
    }
    for factor in negatives {
        result = ast.push(Node::Bin("/".to_string(), result, factor.node));
    }
    Some(result)
}

fn collect_groups(
    ast: &Ast,
    root: NodeId,
    res: &Resolution,
    immutable: &HashMap<BindingId, (storm_lua_syntax::ast::SymbolId, usize)>,
) -> Vec<Group> {
    let mut groups = Vec::<Group>::new();

    #[allow(clippy::too_many_arguments)]
    fn inspect(
        ast: &Ast,
        node: NodeId,
        res: &Resolution,
        immutable: &HashMap<BindingId, (storm_lua_syntax::ast::SymbolId, usize)>,
        inside_function: bool,
        parent_product: bool,
        exact: bool,
        groups: &mut Vec<Group>,
    ) {
        let child_exact = exact
            || matches!(ast.node(node), Node::Bin(op, _, _) if matches!(op.as_str(), "&" | "|" | "~" | "<<" | ">>" | "//" | "%"))
            || matches!(ast.node(node), Node::Index(..) | Node::Fornum(..));
        if inside_function
            && !exact
            && matches!(ast.node(node), Node::Bin(op, _, _) if op == "*" || op == "/")
            && !parent_product
        {
            let mut factors = Vec::new();
            product_factors(ast, node, 1, &mut factors);
            for binding_factor in 0..factors.len() {
                let binding_node = factors[binding_factor].node;
                if !matches!(ast.node(binding_node), Node::Name(_)) {
                    continue;
                }
                let Some(binding_bid) = res.node_bid.get(binding_node as usize).copied().flatten()
                else {
                    continue;
                };
                let Some((binding_name, declaration)) = immutable.get(&binding_bid).copied() else {
                    continue;
                };
                for constant_factor in 0..factors.len() {
                    if constant_factor == binding_factor
                        || !matches!(ast.node(factors[constant_factor].node), Node::Num(_))
                    {
                        continue;
                    }
                    let remaining = factors
                        .iter()
                        .copied()
                        .enumerate()
                        .filter_map(|(index, factor)| {
                            (index != binding_factor && index != constant_factor).then_some(factor)
                        })
                        .collect::<Vec<_>>();
                    if !remaining.iter().any(|factor| factor.sign == 1) {
                        continue;
                    }
                    let mut scratch = clone_ast(ast);
                    let q_symbol = scratch.strings.intern("q");
                    let q = scratch.push(Node::Name(q_symbol));
                    let mut estimated_factors = remaining.clone();
                    estimated_factors.push(Factor { sign: 1, node: q });
                    let Some(estimated) = build_product(&mut scratch, &estimated_factors) else {
                        continue;
                    };
                    let estimate = measure_expr(ast, node) as isize
                        - measure_expr(&scratch, estimated) as isize;
                    let constant = factors[constant_factor].node;
                    let constant_key = match ast.node(constant) {
                        Node::Num(value) => storm_lua_syntax::numeric::normalize_num_literal(value),
                        _ => unreachable!(),
                    };
                    let key = format!(
                        "{}:{}:{}:{}",
                        binding_bid,
                        factors[binding_factor].sign,
                        factors[constant_factor].sign,
                        constant_key
                    );
                    let occurrence = Occurrence {
                        node,
                        factors: factors.clone(),
                        binding_factor,
                        constant_factor,
                        estimate,
                    };
                    if let Some(group) = groups.iter_mut().find(|group| group.key == key) {
                        group.occurrences.push(occurrence);
                    } else {
                        groups.push(Group {
                            key,
                            binding_name,
                            binding_sign: factors[binding_factor].sign,
                            constant,
                            constant_sign: factors[constant_factor].sign,
                            declaration,
                            occurrences: vec![occurrence],
                        });
                    }
                }
            }
        }
        if let Node::Function(_, _, body) = ast.node(node) {
            inspect(ast, *body, res, immutable, true, false, exact, groups);
            return;
        }
        let current_product =
            matches!(ast.node(node), Node::Bin(op, _, _) if op == "*" || op == "/");
        let mut children = Vec::new();
        storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| children.push(child));
        for child in children {
            inspect(
                ast,
                child,
                res,
                immutable,
                inside_function,
                current_product,
                child_exact,
                groups,
            );
        }
    }

    inspect(ast, root, res, immutable, false, false, false, &mut groups);
    groups
}

pub fn synthesize_coefficient_carriers(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    max_rounds: usize,
) -> PassResult {
    if !aggressive || !matches!(ast.node(root), Node::Block(_)) {
        return PassResult {
            root,
            saved: None,
            details: Some(vec!["synthesized=0;considered=0".to_string()]),
        };
    }
    let mut synthesized = 0usize;
    let mut considered = 0usize;
    for _ in 0..max_rounds {
        let source = clone_ast(ast);
        let res = resolve(&source, root);
        let Node::Block(statements) = source.node(root) else {
            break;
        };
        let mut declarations = HashMap::<BindingId, usize>::new();
        for (index, statement) in statements.iter().copied().enumerate() {
            if let Node::Assign(targets, _) = source.node(statement) {
                for target in targets {
                    if matches!(source.node(*target), Node::Name(_)) {
                        if let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() {
                            declarations.insert(bid, index);
                        }
                    }
                }
            }
        }
        let mut immutable = HashMap::new();
        for (bid, binding) in res.bindings.iter().enumerate().skip(1) {
            let bid = bid as BindingId;
            if binding.kind == BindingKind::Global
                && !binding.fixed
                && res
                    .binding_write_counts
                    .get(bid as usize)
                    .copied()
                    .unwrap_or(0)
                    == 1
            {
                if let Some(declaration) = declarations.get(&bid).copied() {
                    immutable.insert(bid, (binding.name, declaration));
                }
            }
        }
        let groups = collect_groups(&source, root, &res, &immutable);
        let baseline = renamed_size(&source, root);
        let used_names = {
            let mut nodes = Vec::new();
            storm_lua_analysis::effects::walk(&source, root, &mut nodes);
            nodes
                .into_iter()
                .filter_map(|node| match source.node(node) {
                    Node::Name(symbol) => Some(source.strings.get(*symbol).to_string()),
                    _ => None,
                })
                .collect::<HashSet<_>>()
        };
        let mut best: Option<(Ast, usize)> = None;
        for group in groups
            .into_iter()
            .filter(|group| group.occurrences.len() >= 2)
            .take(32)
        {
            let mut ranked = group.occurrences.clone();
            ranked.sort_by_key(|item| std::cmp::Reverse(item.estimate));
            for count in 2..=ranked.len() {
                considered += 1;
                let mut serial = 0usize;
                let mut carrier = format!("__stormmin_coefficient_{serial}");
                while used_names.contains(&carrier) {
                    serial += 1;
                    carrier = format!("__stormmin_coefficient_{serial}");
                }
                let selected = ranked
                    .iter()
                    .take(count)
                    .map(|occurrence| occurrence.node)
                    .collect::<HashSet<_>>();
                let mut candidate = clone_ast(&source);
                let carrier_symbol = candidate.strings.intern(&carrier);
                for occurrence in &ranked {
                    if !selected.contains(&occurrence.node) {
                        continue;
                    }
                    let mut factors = occurrence
                        .factors
                        .iter()
                        .copied()
                        .enumerate()
                        .filter_map(|(index, factor)| {
                            (index != occurrence.binding_factor
                                && index != occurrence.constant_factor)
                                .then_some(factor)
                        })
                        .collect::<Vec<_>>();
                    let carrier_node = candidate.push(Node::Name(carrier_symbol));
                    super::origins::derive(
                        &mut candidate,
                        carrier_node,
                        &source,
                        &[
                            occurrence.factors[occurrence.binding_factor].node,
                            occurrence.factors[occurrence.constant_factor].node,
                        ],
                        "coefficient-carrier-read",
                    );
                    let first_formula = candidate.nodes.len();
                    factors.push(Factor {
                        sign: 1,
                        node: carrier_node,
                    });
                    if let Some(replacement) = build_product(&mut candidate, &factors) {
                        if candidate.nodes.tracks_origins() {
                            for id in first_formula..candidate.nodes.len() {
                                candidate.nodes.derive_from(
                                    id as NodeId,
                                    &source.nodes,
                                    occurrence.node,
                                    "coefficient-product-reassociation",
                                );
                            }
                        }
                        let value = candidate.node(replacement).clone();
                        candidate.nodes.rewrite(
                            occurrence.node,
                            value,
                            "coefficient-product-reassociation",
                        );
                    }
                }
                let binding_node = candidate.push(Node::Name(group.binding_name));
                let first_occurrence = &group.occurrences[0];
                let source_binding = first_occurrence.factors[first_occurrence.binding_factor].node;
                candidate.nodes.derive_from(
                    binding_node,
                    &source.nodes,
                    source_binding,
                    "coefficient-binding",
                );
                let mut coefficient_factors = vec![Factor {
                    sign: group.binding_sign,
                    node: binding_node,
                }];
                let constant = candidate.push(source.node(group.constant).clone());
                candidate.nodes.derive_from(
                    constant,
                    &source.nodes,
                    group.constant,
                    "coefficient-constant",
                );
                coefficient_factors.push(Factor {
                    sign: group.constant_sign,
                    node: constant,
                });
                let first_formula = candidate.nodes.len();
                let Some(coefficient) = build_product(&mut candidate, &coefficient_factors) else {
                    continue;
                };
                if candidate.nodes.tracks_origins() {
                    for id in first_formula..candidate.nodes.len() {
                        super::origins::derive(
                            &mut candidate,
                            id as NodeId,
                            &source,
                            &[source_binding, group.constant],
                            "coefficient-definition",
                        );
                    }
                }
                let carrier_target = candidate.push(Node::Name(carrier_symbol));
                candidate
                    .nodes
                    .mark_synthetic(carrier_target, "coefficient-storage");
                let assignment =
                    candidate.push(Node::Assign(vec![carrier_target], vec![coefficient]));
                candidate
                    .nodes
                    .mark_synthetic(assignment, "coefficient-storage");
                let Node::Block(mut top) = candidate.node(root).clone() else {
                    unreachable!()
                };
                top.insert(group.declaration + 1, assignment);
                candidate
                    .nodes
                    .rewrite(root, Node::Block(top), "coefficient-storage-insertion");
                let size = renamed_size(&candidate, root);
                if size < baseline && best.as_ref().is_none_or(|(_, best_size)| size < *best_size) {
                    best = Some((candidate, size));
                }
            }
        }
        let Some((candidate, _)) = best else { break };
        *ast = candidate;
        synthesized += 1;
    }
    PassResult {
        root,
        saved: None,
        details: Some(vec![format!(
            "synthesized={synthesized};considered={considered}"
        )]),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::{parse_source, Printer};

    #[test]
    fn synthesizes_repeated_coefficient() {
        let source = "k=input.getNumber(1)function onTick()a=input.getNumber(2)*k*12345 b=input.getNumber(3)*k*12345 output.setNumber(1,a+b)end";
        let (mut ast, root) = parse_source(source).unwrap();
        let result = synthesize_coefficient_carriers(&mut ast, root, true, 8);
        assert!(result.details.as_ref().unwrap()[0].contains("synthesized=1"));
        let _ = Printer::new(&ast, false).output(root);
    }
}
