//! Multiplicative carrier reassociation
//! (`passes/immutable-values.ts::reuseMultiplicativeCarriers`).
//!
//! A stable root product such as `tau=math.pi*2` may replace the same factor
//! subset inside a larger function-local product (`x*math.pi*2` -> `x*tau`).
//! Factor equality includes resolved BindingIds, so shadowed names cannot
//! collide. Each round measures the 48 highest-estimate opportunities after
//! scope renaming and accepts only the smallest strict improvement.

use crate::pass::PassResult;
use crate::scope_rename::scope_rename_fast;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId};
use storm_lua_syntax::size::{measure_expr, measure_size};

#[derive(Clone)]
struct Carrier {
    symbol: SymbolId,
    factor_keys: Vec<String>,
    expression: NodeId,
}

#[derive(Clone)]
struct Opportunity {
    node: NodeId,
    remaining: Vec<NodeId>,
    carrier_symbol: SymbolId,
    definition: NodeId,
    replaced: Vec<NodeId>,
    estimate: usize,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn measured(ast: &Ast, root: NodeId) -> usize {
    let renamed = scope_rename_fast(ast, root);
    measure_size(&renamed.ast, renamed.root)
}

fn without_parens(ast: &Ast, mut node: NodeId) -> NodeId {
    while let Node::Paren(inner) = ast.node(node) {
        node = *inner;
    }
    node
}

fn product_factors(ast: &Ast, node: NodeId, output: &mut Vec<NodeId>) {
    let node = without_parens(ast, node);
    if let Node::Bin(operator, left, right) = ast.node(node) {
        if operator == "*" {
            product_factors(ast, *left, output);
            product_factors(ast, *right, output);
            return;
        }
    }
    output.push(node);
}

fn factors(ast: &Ast, node: NodeId) -> Vec<NodeId> {
    let mut output = Vec::new();
    product_factors(ast, node, &mut output);
    output
}

fn node_bid(resolution: &Resolution, node: NodeId) -> Option<BindingId> {
    resolution.node_bid.get(node as usize).copied().flatten()
}

fn collect_carriers(
    ast: &Ast,
    root: NodeId,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
) -> Vec<Carrier> {
    let Node::Block(statements) = ast.node(root) else {
        return Vec::new();
    };
    let mut carriers = Vec::new();
    for statement in statements {
        let entries = match ast.node(*statement) {
            Node::Local(_, expressions) => {
                let bids = resolution
                    .node_bids
                    .get(*statement as usize)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                if bids.len() != expressions.len() {
                    Vec::new()
                } else {
                    bids.iter()
                        .copied()
                        .zip(expressions.iter().copied())
                        .map(|(bid, expression)| (bid, expression, 0u32))
                        .collect::<Vec<_>>()
                }
            }
            Node::Assign(targets, expressions) if targets.len() == expressions.len() => targets
                .iter()
                .copied()
                .zip(expressions.iter().copied())
                .filter_map(|(target, expression)| {
                    matches!(ast.node(target), Node::Name(_))
                        .then(|| node_bid(resolution, target))
                        .flatten()
                        .map(|bid| (bid, expression, 1u32))
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };

        for (bid, expression, expected_writes) in entries {
            let product = factors(ast, expression);
            if product.len() < 2
                || resolution
                    .binding_write_counts
                    .get(bid as usize)
                    .copied()
                    .unwrap_or(0)
                    != expected_writes
            {
                continue;
            }
            let binding = &resolution.bindings[bid as usize];
            let effect = analyzer.effects_for_expr(expression);
            if binding.scope != 0
                || binding.fixed
                || effect.calls
                || effect.ordered
                || !effect.writes.is_empty()
                || !effect.stable
            {
                continue;
            }
            carriers.push(Carrier {
                symbol: binding.name,
                expression,
                factor_keys: product
                    .iter()
                    .map(|factor| {
                        crate::passes::immutable_values::expression_key(ast, resolution, *factor)
                    })
                    .collect(),
            });
        }
    }
    carriers
}

fn replacement_length(source: &Ast, remaining: &[NodeId], carrier_symbol: SymbolId) -> usize {
    let mut trial = clone_ast(source);
    let carrier = trial.push(Node::Name(carrier_symbol));
    let mut product = remaining[0];
    for factor in remaining.iter().copied().skip(1).chain([carrier]) {
        product = trial.bin("*", product, factor);
    }
    measure_expr(&trial, product)
}

fn inspect(
    ast: &Ast,
    resolution: &Resolution,
    node: NodeId,
    inside_function: bool,
    parent_product: bool,
    carriers: &[Carrier],
    output: &mut Vec<Opportunity>,
) {
    if inside_function
        && !parent_product
        && matches!(ast.node(node), Node::Bin(operator, _, _) if operator == "*")
    {
        let product = factors(ast, node);
        let product_keys = product
            .iter()
            .map(|factor| crate::passes::immutable_values::expression_key(ast, resolution, *factor))
            .collect::<Vec<_>>();
        let original_length = measure_expr(ast, node);

        for carrier in carriers {
            let mut remaining_indices = (0..product.len()).collect::<Vec<_>>();
            let mut valid = true;
            for wanted in &carrier.factor_keys {
                let Some(position) = remaining_indices
                    .iter()
                    .position(|index| product_keys[*index] == *wanted)
                else {
                    valid = false;
                    break;
                };
                remaining_indices.remove(position);
            }
            if !valid || remaining_indices.is_empty() {
                continue;
            }
            let remaining = remaining_indices
                .iter()
                .map(|index| product[*index])
                .collect::<Vec<_>>();
            let replacement_len = replacement_length(ast, &remaining, carrier.symbol);
            if original_length > replacement_len {
                output.push(Opportunity {
                    node,
                    remaining,
                    carrier_symbol: carrier.symbol,
                    definition: carrier.expression,
                    replaced: product
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| !remaining_indices.contains(i))
                        .map(|(_, n)| *n)
                        .collect(),
                    estimate: original_length - replacement_len,
                });
            }
        }
    }

    if let Node::Function(_, _, body) = ast.node(node) {
        inspect(ast, resolution, *body, true, false, carriers, output);
        return;
    }

    let is_product = matches!(ast.node(node), Node::Bin(operator, _, _) if operator == "*");
    storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
        inspect(
            ast,
            resolution,
            child,
            inside_function,
            is_product,
            carriers,
            output,
        );
    });
}

fn apply_opportunity(source: &Ast, opportunity: &Opportunity) -> Ast {
    let mut candidate = clone_ast(source);
    let carrier = candidate.push(Node::Name(opportunity.carrier_symbol));
    super::origins::derive(
        &mut candidate,
        carrier,
        source,
        &opportunity.replaced,
        "multiplicative-carrier-substitution",
    );
    candidate.nodes.relate_from(
        carrier,
        &source.nodes,
        opportunity.definition,
        "multiplicative-carrier-definition",
    );
    let mut product = opportunity.remaining[0];
    for factor in opportunity
        .remaining
        .iter()
        .copied()
        .skip(1)
        .chain([carrier])
    {
        product = candidate.bin("*", product, factor);
        candidate.nodes.derive_from(
            product,
            &source.nodes,
            opportunity.node,
            "multiplicative-carrier-reassociation",
        );
    }
    let value = candidate.node(product).clone();
    candidate.nodes.rewrite(
        opportunity.node,
        value,
        "multiplicative-carrier-reassociation",
    );
    candidate
}

pub fn reuse_multiplicative_carriers_with_options(
    ast: &mut Ast,
    root: NodeId,
    max_rounds: usize,
) -> PassResult {
    let original_size = measured(ast, root);
    let mut reused = 0usize;
    let mut considered = 0usize;

    for _ in 0..max_rounds {
        let source = clone_ast(ast);
        let resolution = resolve(&source, root);
        let analyzer = EffectAnalyzer::new(&source, &resolution, root, true);
        let carriers = collect_carriers(&source, root, &resolution, &analyzer);
        let mut opportunities = Vec::new();
        inspect(
            &source,
            &resolution,
            root,
            false,
            false,
            &carriers,
            &mut opportunities,
        );
        opportunities.sort_by_key(|opportunity| std::cmp::Reverse(opportunity.estimate));
        opportunities.truncate(48);

        let baseline = measured(&source, root);
        let mut best: Option<(Ast, usize)> = None;
        for opportunity in opportunities {
            considered += 1;
            let candidate = apply_opportunity(&source, &opportunity);
            let size = measured(&candidate, root);
            if size < baseline && best.as_ref().is_none_or(|(_, best_size)| size < *best_size) {
                best = Some((candidate, size));
            }
        }
        let Some((candidate, _)) = best else {
            break;
        };
        *ast = candidate;
        reused += 1;
    }

    PassResult {
        root,
        saved: Some(original_size.saturating_sub(measured(ast, root)) as u64),
        details: Some(vec![
            format!("reused={reused}"),
            format!("considered={considered}"),
        ]),
    }
}

pub fn reuse_multiplicative_carriers(ast: &mut Ast, root: NodeId) -> PassResult {
    reuse_multiplicative_carriers_with_options(ast, root, 16)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = reuse_multiplicative_carriers(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn reuses_product_factor_subset() {
        let source = "tau=math.pi*2 function onTick()local x=input.getNumber(1)output.setNumber(1,x*math.pi*2)output.setNumber(2,x*math.pi*2)end";
        let code = output(source);
        assert!(code.contains("x*tau"), "{code}");
        assert_eq!(code.matches("math.pi*2").count(), 1, "{code}");
    }

    #[test]
    fn respects_shadowed_factor_bindings() {
        let source = "tau=math.pi*2 function onTick()local math={pi=3}local x=input.getNumber(1)output.setNumber(1,x*math.pi*2)end";
        assert_eq!(output(source), source);
    }

    #[test]
    fn requires_a_remaining_factor() {
        let source = "tau=math.pi*2 function onTick()output.setNumber(1,math.pi*2)end";
        assert_eq!(output(source), source);
    }

    #[test]
    fn ignores_reassigned_carrier() {
        let source = "tau=math.pi*2 tau=1 function onTick()local x=input.getNumber(1)output.setNumber(1,x*math.pi*2)end";
        assert_eq!(output(source), source);
    }
}
