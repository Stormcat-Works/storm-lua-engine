//! Numeric literal approximation (`passes/numeric-literals.ts`).
//!
//! Non-integral literals are rounded only when the configured tolerance allows
//! it and the literal does not feed an exact route (bit/radix operations,
//! numeric-for bounds, table indexes, string.pack/unpack, or a binding/function
//! transitively used by one of those routes).

use std::collections::{HashMap, HashSet};

use crate::config::NumericTolerance;
use crate::pass::PassResult;
#[cfg(test)]
use crate::provenance_audit_support::Printer;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::numeric::{num_val, short_num, shortest_exact_hex_float_literal};
use storm_lua_syntax::print::render_num;
use storm_lua_syntax::size::measure_expr;

const EXACT_BINARY: &[&str] = &["&", "|", "~", "<<", ">>", "//", "%"];
const PER_LITERAL_ABS_CAP: f64 = 1e-12;
const PER_LITERAL_REL_CAP: f64 = 1e-9;

/// Canonicalize source/generated floating-point tokens to a shorter exact
/// hexadecimal spelling once before the search fan-out. This is deliberately
/// separate from `render_num`: the printer is invoked repeatedly while scoring
/// candidates, so doing IEEE-754 candidate synthesis there is disproportionately
/// expensive. Integer-source tokens are rejected by the shared helper.
pub fn canonicalize_exact_float_literals(ast: &mut Ast, root: NodeId) -> PassResult {
    let mut ids = Vec::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |id| {
        if matches!(ast.node(id), Node::Num(_)) {
            ids.push(id);
        }
    });

    let mut saved = 0u64;
    let mut changed = 0usize;
    for id in ids {
        let Node::Num(raw) = ast.node(id).clone() else {
            continue;
        };
        // Tiny numeric tokens cannot beat the shortest legal hexadecimal float
        // (`0x.1`), and skipping them keeps the common integer/draw path cheap.
        if raw.len() < 7 {
            continue;
        }
        let current_len = render_num(&raw).len();
        let Some(hex) = shortest_exact_hex_float_literal(&raw) else {
            continue;
        };
        if hex.len() >= current_len {
            continue;
        }
        saved += (current_len - hex.len()) as u64;
        changed += 1;
        ast.nodes
            .rewrite(id, Node::Num(hex.into()), "exact-float-spelling");
    }

    PassResult {
        root,
        saved: Some(saved),
        details: (changed > 0).then(|| vec![format!("canonicalized={changed}")]),
    }
}

fn within_tolerance(original: f64, candidate: f64, tolerance: NumericTolerance) -> bool {
    (candidate - original).abs() <= tolerance.abs + tolerance.rel * original.abs()
}

/// `Number(value.toPrecision(digits))`. Only the rounded numeric value matters;
/// Rust's scientific formatter supplies exactly `digits` significant decimal
/// digits and parses back through IEEE-754 round-to-nearest.
fn to_precision_number(value: f64, digits: usize) -> Option<f64> {
    let text = format!("{:.*e}", digits.saturating_sub(1), value);
    text.parse::<f64>().ok()
}

fn shortest_approximation(
    ast: &mut Ast,
    value: f64,
    tolerance: NumericTolerance,
) -> Option<String> {
    let budget = NumericTolerance {
        abs: tolerance.abs.min(PER_LITERAL_ABS_CAP),
        rel: tolerance.rel.min(PER_LITERAL_REL_CAP),
    };
    let mut best: Option<String> = None;
    for digits in 1..=16 {
        let Some(rounded) = to_precision_number(value, digits) else {
            continue;
        };
        if !rounded.is_finite() || !within_tolerance(value, rounded, budget) {
            continue;
        }
        let printed = short_num(rounded);
        if best
            .as_ref()
            .is_none_or(|current| printed.len() < current.len())
        {
            best = Some(printed);
        }
    }
    let exact = ast.num(short_num(value));
    let exact_len = measure_expr(ast, exact);
    best.filter(|candidate| candidate.len() < exact_len)
}

fn collect_definitions(
    ast: &Ast,
    res: &Resolution,
    root: NodeId,
) -> HashMap<BindingId, Vec<NodeId>> {
    let mut definitions: HashMap<BindingId, Vec<NodeId>> = HashMap::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |id| match ast.node(id) {
        Node::Assign(vs, es) => {
            for (index, target) in vs.iter().enumerate() {
                let Some(expression) = es.get(index) else {
                    continue;
                };
                if !matches!(ast.node(*target), Node::Name(_)) {
                    continue;
                }
                if let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() {
                    definitions.entry(bid).or_default().push(*expression);
                }
            }
        }
        Node::Local(_, es) => {
            let bids = res
                .node_bids
                .get(id as usize)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            for (index, bid) in bids.iter().enumerate() {
                if let Some(expression) = es.get(index) {
                    definitions.entry(*bid).or_default().push(*expression);
                }
            }
        }
        _ => {}
    });
    definitions
}

fn is_exact_call(ast: &Ast, analyzer: &EffectAnalyzer<'_>, id: NodeId) -> bool {
    let Node::Call(function, _, _) = ast.node(id) else {
        return false;
    };
    matches!(
        analyzer.resolve_builtin_reference(*function).as_deref(),
        Some("string.pack" | "string.unpack")
    )
}

fn visit_function_returns(
    ast: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    function_node: NodeId,
    current: NodeId,
    exact_bids: &mut HashSet<BindingId>,
    exact_functions: &mut HashSet<BindingId>,
) {
    if current != function_node && matches!(ast.node(current), Node::Function(..)) {
        return;
    }
    if let Node::Return(expressions) = ast.node(current) {
        for expression in expressions {
            visit_exact_uses(
                ast,
                res,
                analyzer,
                *expression,
                true,
                exact_bids,
                exact_functions,
            );
        }
    }
    storm_lua_analysis::resolver::for_each_child(ast, current, &mut |child| {
        visit_function_returns(
            ast,
            res,
            analyzer,
            function_node,
            child,
            exact_bids,
            exact_functions,
        );
    });
}

fn child_is_exact(ast: &Ast, parent: NodeId, key: &str, inherited: bool, exact_call: bool) -> bool {
    if inherited || exact_call {
        return true;
    }
    match ast.node(parent) {
        Node::Bin(op, _, _) if EXACT_BINARY.contains(&op.as_str()) => true,
        Node::Fornum(..) if matches!(key, "a" | "b" | "c") => true,
        Node::Index(_, _, _) if key == "key" => true,
        _ => false,
    }
}

fn visit_exact_uses(
    ast: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    id: NodeId,
    exact: bool,
    exact_bids: &mut HashSet<BindingId>,
    exact_functions: &mut HashSet<BindingId>,
) {
    if exact && matches!(ast.node(id), Node::Name(_)) {
        if let Some(bid) = res.node_bid.get(id as usize).copied().flatten() {
            exact_bids.insert(bid);
        }
    }
    if exact {
        if let Node::Call(function, _, _) = ast.node(id) {
            if matches!(ast.node(*function), Node::Name(_)) {
                if let Some(bid) = res.node_bid.get(*function as usize).copied().flatten() {
                    if exact_functions.insert(bid) {
                        if let Some(function_node) = res.binding(bid).function_node {
                            if let Node::Function(_, _, body) = ast.node(function_node) {
                                visit_function_returns(
                                    ast,
                                    res,
                                    analyzer,
                                    function_node,
                                    *body,
                                    exact_bids,
                                    exact_functions,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    let exact_call = is_exact_call(ast, analyzer, id);
    storm_lua_syntax::ast_utils::for_each_child_key(ast, id, &mut |key, child| {
        visit_exact_uses(
            ast,
            res,
            analyzer,
            child,
            child_is_exact(ast, id, key, exact, exact_call),
            exact_bids,
            exact_functions,
        );
    });
}

fn propagate_exact_definitions(
    ast: &Ast,
    res: &Resolution,
    definitions: &HashMap<BindingId, Vec<NodeId>>,
    exact_bids: &mut HashSet<BindingId>,
) {
    loop {
        let mut changed = false;
        let current = exact_bids.iter().copied().collect::<Vec<_>>();
        for bid in current {
            for expression in definitions.get(&bid).into_iter().flatten() {
                storm_lua_syntax::ast_utils::walk(ast, *expression, &mut |id| {
                    if matches!(ast.node(id), Node::Name(_)) {
                        if let Some(input_bid) = res.node_bid.get(id as usize).copied().flatten() {
                            if exact_bids.insert(input_bid) {
                                changed = true;
                            }
                        }
                    }
                });
            }
        }
        if !changed {
            break;
        }
    }
}

struct RewriteContext<'a> {
    source: &'a Ast,
    res: &'a Resolution,
    analyzer: &'a EffectAnalyzer<'a>,
    exact_bids: &'a HashSet<BindingId>,
    tolerance: NumericTolerance,
    shortened: usize,
}

fn rewrite(target: &mut Ast, context: &mut RewriteContext<'_>, id: NodeId, exact: bool) -> NodeId {
    if let Node::Num(raw) = context.source.node(id) {
        if !exact {
            let value = num_val(raw);
            if value.is_finite() && value.fract() != 0.0 {
                if let Some(candidate) = shortest_approximation(target, value, context.tolerance) {
                    let original_len = measure_expr(context.source, id);
                    if candidate.len() < original_len {
                        context.shortened += 1;
                        let replacement = target.num(candidate);
                        target.nodes.derive_from(
                            replacement,
                            &context.source.nodes,
                            id,
                            "numeric-literal-approximation",
                        );
                        return replacement;
                    }
                }
            }
        }
    }

    let node = context.source.node(id).clone();
    let exact_call = is_exact_call(context.source, context.analyzer, id);
    let rewritten = match node {
        Node::Assign(vs, es) => {
            let new_vs = vs
                .iter()
                .map(|child| {
                    rewrite(
                        target,
                        context,
                        *child,
                        child_is_exact(context.source, id, "vs", exact, exact_call),
                    )
                })
                .collect::<Vec<_>>();
            let new_es = es
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    let target_exact = vs
                        .get(index)
                        .and_then(|target_id| {
                            if matches!(context.source.node(*target_id), Node::Name(_)) {
                                context
                                    .res
                                    .node_bid
                                    .get(*target_id as usize)
                                    .copied()
                                    .flatten()
                            } else {
                                None
                            }
                        })
                        .is_some_and(|bid| context.exact_bids.contains(&bid));
                    rewrite(target, context, *child, exact || exact_call || target_exact)
                })
                .collect();
            Node::Assign(new_vs, new_es)
        }
        Node::Local(names, es) => {
            let bids = context
                .res
                .node_bids
                .get(id as usize)
                .cloned()
                .unwrap_or_default();
            let new_es = es
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    let target_exact = bids
                        .get(index)
                        .is_some_and(|bid| context.exact_bids.contains(bid));
                    rewrite(target, context, *child, exact || exact_call || target_exact)
                })
                .collect();
            Node::Local(names, new_es)
        }
        other => {
            let mut child_index = 0usize;
            let mut keyed_children = Vec::new();
            storm_lua_syntax::ast_utils::for_each_child_key(
                context.source,
                id,
                &mut |key, child| {
                    keyed_children.push((key, child));
                },
            );
            let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&other, &mut |child| {
                let key = keyed_children
                    .get(child_index)
                    .map(|entry| entry.0)
                    .unwrap_or("");
                child_index += 1;
                rewrite(
                    target,
                    context,
                    child,
                    child_is_exact(context.source, id, key, exact, exact_call),
                )
            });
            mapped
        }
    };
    target
        .nodes
        .rewrite(id, rewritten, "numeric-literal-approximation");
    id
}

pub fn shorten_numeric_literals_with_options(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    tolerance: NumericTolerance,
) -> PassResult {
    if !aggressive || (tolerance.abs <= 0.0 && tolerance.rel <= 0.0) {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let res = resolve(ast, root);
    let analyzer = EffectAnalyzer::new(ast, &res, root, true);
    let definitions = collect_definitions(ast, &res, root);
    let mut exact_bids = HashSet::new();
    let mut exact_functions = HashSet::new();
    visit_exact_uses(
        ast,
        &res,
        &analyzer,
        root,
        false,
        &mut exact_bids,
        &mut exact_functions,
    );
    propagate_exact_definitions(ast, &res, &definitions, &mut exact_bids);

    let source_nodes = ast.nodes.clone();
    let mut source = storm_lua_syntax::ast_utils::inherit_ast(ast);
    source.nodes = source_nodes;
    let source_res = resolve(&source, root);
    let source_analyzer = EffectAnalyzer::new(&source, &source_res, root, true);
    let mut context = RewriteContext {
        source: &source,
        res: &source_res,
        analyzer: &source_analyzer,
        exact_bids: &exact_bids,
        tolerance,
        shortened: 0,
    };
    let out = rewrite(ast, &mut context, root, false);
    PassResult {
        root: out,
        saved: Some(context.shortened as u64),
        details: None,
    }
}

pub fn shorten_numeric_literals(ast: &mut Ast, root: NodeId) -> PassResult {
    shorten_numeric_literals_with_options(
        ast,
        root,
        true,
        NumericTolerance {
            abs: 1e-6,
            rel: 1e-6,
        },
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = shorten_numeric_literals(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn shortens_tolerant_non_integral_literals() {
        assert_eq!(output("x=1.23456789012345"), "x=1.23456789");
    }

    #[test]
    fn preserves_bit_and_index_routes() {
        let out = output("x=1.23456789012345&3 y=t[1.23456789012345]");
        assert_eq!(out.matches("1.23456789012345").count(), 2, "{out}");
    }

    #[test]
    fn exact_binding_dataflow_is_transitive() {
        let out = output("local a=1.23456789012345 local b=a x=b&3");
        assert!(out.contains("1.23456789012345"), "{out}");
    }

    #[test]
    fn exact_hex_canonicalization_is_shorter_and_subtype_safe() {
        let source = "a=.10000000149011612 b=-.6000000238418579 c=65536.0 d=65536 e=5e-324";
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = canonicalize_exact_float_literals(&mut ast, root);
        let out = Printer::new(&ast, false).output(result.root);
        assert_eq!(out, "a=0x.199999a b=-0x.99999a c=0x1p16 d=65536 e=5e-324");
        assert_eq!(result.saved, Some(17));
    }

    #[test]
    fn disabled_for_zero_tolerance() {
        let (mut ast, root) = parse_source("x=1.23456789012345").expect("parse");
        let result = shorten_numeric_literals_with_options(
            &mut ast,
            root,
            true,
            NumericTolerance { abs: 0.0, rel: 0.0 },
        );
        assert_eq!(
            Printer::new(&ast, false).output(result.root),
            "x=1.23456789012345"
        );
    }
}
