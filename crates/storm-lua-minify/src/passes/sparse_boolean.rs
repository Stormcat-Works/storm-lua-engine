//! Sparse boolean decode scalarization (`passes/misc.ts`).
//!
//! This is intentionally proof-driven: only a fixed numeric-for table decoder
//! followed by the exact snapshot/decode/pulse pipeline is scalarized.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::pass::PassResult;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId};
use storm_lua_syntax::numeric::{num_val, short_num};
use storm_lua_syntax::size::{measure_expr, measure_size};

#[derive(Clone)]
struct Helper {
    function: NodeId,
    declaration: NodeId,
    parameter_bids: Vec<BindingId>,
    parameters: Vec<SymbolId>,
    loop_node: NodeId,
    loop_bid: BindingId,
    value: NodeId,
    table_decl: NodeId,
    table_pos: usize,
    loop_pos: usize,
    lower: i64,
    upper: i64,
}

#[derive(Clone)]
struct Pipeline {
    block: NodeId,
    snapshot_index: usize,
    pulse_index: usize,
    prev: NodeId,
    current: NodeId,
    pulse_loop: NodeId,
    pulse_expr: NodeId,
    current_bid: BindingId,
    old_bid: BindingId,
    pulse_bid: BindingId,
    helper_bid: BindingId,
    call: NodeId,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn walk(ast: &Ast, root: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    storm_lua_analysis::effects::walk(ast, root, &mut out);
    out
}

fn bid(res: &Resolution, node: NodeId) -> Option<BindingId> {
    res.node_bid.get(node as usize).copied().flatten()
}

fn unwrap_paren(ast: &Ast, mut node: NodeId) -> NodeId {
    while let Node::Paren(inner) = ast.node(node) {
        node = *inner;
    }
    node
}

/// Verify the exact bit layout that `shorten_bit` later reconstructs.  The
/// scalarizer indexes bits as k-1, so accepting `1<<i` (or another mask shape)
/// would silently move every decoded bit by one position.
fn is_canonical_decode_value(
    ast: &Ast,
    res: &Resolution,
    value: NodeId,
    parameter_bids: &[BindingId],
    loop_bid: BindingId,
) -> bool {
    let Node::Bin(compare, left, right) = ast.node(unwrap_paren(ast, value)) else {
        return false;
    };
    if compare != "~=" {
        return false;
    }
    let is_zero = |node: NodeId| matches!(ast.node(unwrap_paren(ast, node)), Node::Num(raw) if num_val(raw) == 0.0);
    let bit = if is_zero(*right) {
        *left
    } else if is_zero(*left) {
        *right
    } else {
        return false;
    };
    let Node::Bin(and, data, mask) = ast.node(unwrap_paren(ast, bit)) else {
        return false;
    };
    let Some(data_bid) = bid(res, unwrap_paren(ast, *data)) else {
        return false;
    };
    if and != "&"
        || !matches!(ast.node(unwrap_paren(ast, *data)), Node::Name(_))
        || !parameter_bids.contains(&data_bid)
    {
        return false;
    }
    let Node::Bin(shift, one, offset) = ast.node(unwrap_paren(ast, *mask)) else {
        return false;
    };
    if shift != "<<"
        || !matches!(ast.node(unwrap_paren(ast, *one)), Node::Num(raw) if num_val(raw) == 1.0)
    {
        return false;
    }
    let Node::Bin(subtract, induction, one) = ast.node(unwrap_paren(ast, *offset)) else {
        return false;
    };
    subtract == "-"
        && matches!(ast.node(unwrap_paren(ast, *induction)), Node::Name(_))
        && bid(res, unwrap_paren(ast, *induction)) == Some(loop_bid)
        && matches!(ast.node(unwrap_paren(ast, *one)), Node::Num(raw) if num_val(raw) == 1.0)
}

fn discover_helpers(
    ast: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
) -> HashMap<BindingId, Helper> {
    let mut helpers = HashMap::new();
    for (binding_id, binding) in res.bindings.iter().enumerate().skip(1) {
        let (Some(function), Some(declaration)) = (binding.function_node, binding.decl_node) else {
            continue;
        };
        let Node::Function(parameters, variadic, body) = ast.node(function) else {
            continue;
        };
        if *variadic {
            continue;
        }
        let Node::Block(statements) = ast.node(*body) else {
            continue;
        };
        let loops = statements
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, statement)| matches!(ast.node(*statement), Node::Fornum(_, _, _, None, _)))
            .collect::<Vec<_>>();
        if loops.len() != 1 {
            continue;
        }
        let (loop_pos, loop_node) = loops[0];
        let Node::Fornum(_, start, end, None, loop_body) = ast.node(loop_node) else {
            continue;
        };
        let (Node::Num(start_text), Node::Num(end_text)) = (ast.node(*start), ast.node(*end))
        else {
            continue;
        };
        let lower = num_val(start_text);
        let upper = num_val(end_text);
        if !lower.is_finite() || !upper.is_finite() || lower.fract() != 0.0 || upper.fract() != 0.0
        {
            continue;
        }
        let loop_bid = bid(res, loop_node).unwrap_or(0);
        if loop_bid == 0 {
            continue;
        }
        let Node::Block(loop_statements) = ast.node(*loop_body) else {
            continue;
        };
        if loop_statements.len() != 1 {
            continue;
        }
        let Node::Assign(targets, values) = ast.node(loop_statements[0]) else {
            continue;
        };
        if targets.len() != 1 || values.len() != 1 {
            continue;
        }
        let Node::Index(object, key, _) = ast.node(targets[0]) else {
            continue;
        };
        if !matches!(ast.node(*object), Node::Name(_))
            || !matches!(ast.node(*key), Node::Name(_))
            || bid(res, *key) != Some(loop_bid)
        {
            continue;
        }
        let Some(table_bid) = bid(res, *object) else {
            continue;
        };
        let Some(parameter_bids) = res.node_bids.get(function as usize) else {
            continue;
        };
        if !is_canonical_decode_value(ast, res, values[0], parameter_bids, loop_bid) {
            continue;
        }
        let Some(last) = statements.last().copied() else {
            continue;
        };
        let Node::Return(expressions) = ast.node(last) else {
            continue;
        };
        if expressions.len() != 1
            || !matches!(ast.node(expressions[0]), Node::Name(_))
            || bid(res, expressions[0]) != Some(table_bid)
        {
            continue;
        }
        let mut table_decl = None;
        for statement in statements {
            let Node::Local(names, expressions) = ast.node(*statement) else {
                continue;
            };
            let bids = res
                .node_bids
                .get(*statement as usize)
                .cloned()
                .unwrap_or_default();
            if let Some(position) = bids.iter().position(|candidate| *candidate == table_bid) {
                if position == names.len() - 1
                    && names.len() == expressions.len()
                    && expressions.get(position).is_some_and(
                        |expr| matches!(ast.node(*expr),Node::Table(fields) if fields.is_empty()),
                    )
                {
                    table_decl = Some((*statement, position));
                    break;
                }
            }
        }
        let Some((table_decl, table_pos)) = table_decl else {
            continue;
        };
        let effect = analyzer.effects_for_expr(values[0]);
        if effect.calls || effect.ordered || !effect.writes.is_empty() {
            continue;
        }
        helpers.insert(
            binding_id as BindingId,
            Helper {
                function,
                declaration,
                parameter_bids: res
                    .node_bids
                    .get(function as usize)
                    .cloned()
                    .unwrap_or_default(),
                parameters: parameters.clone(),
                loop_node,
                loop_bid,
                value: values[0],
                table_decl,
                table_pos,
                loop_pos,
                lower: lower as i64,
                upper: upper as i64,
            },
        );
    }
    helpers
}

fn find_pipeline(
    ast: &Ast,
    root: NodeId,
    res: &Resolution,
    helpers: &HashMap<BindingId, Helper>,
) -> Option<Pipeline> {
    for block in walk(ast, root)
        .into_iter()
        .filter(|node| matches!(ast.node(*node), Node::Block(_)))
    {
        let Node::Block(statements) = ast.node(block) else {
            continue;
        };
        for i in 1..statements.len().saturating_sub(1) {
            let cur = statements[i];
            let Node::Assign(vs, es) = ast.node(cur) else {
                continue;
            };
            if vs.len() != 1 || es.len() != 1 || !matches!(ast.node(vs[0]), Node::Name(_)) {
                continue;
            }
            let Node::Call(function, _, method) = ast.node(es[0]) else {
                continue;
            };
            if method.is_some() || !matches!(ast.node(*function), Node::Name(_)) {
                continue;
            }
            let Some(helper_bid) = bid(res, *function) else {
                continue;
            };
            let Some(helper) = helpers.get(&helper_bid) else {
                continue;
            };
            let current_bid = bid(res, vs[0])?;
            let prev = statements[i - 1];
            let Node::Assign(pvs, pes) = ast.node(prev) else {
                continue;
            };
            let mut old_bid = None;
            for (index, target) in pvs.iter().copied().enumerate() {
                if matches!(ast.node(target), Node::Name(_))
                    && pes.get(index).is_some_and(|expression| {
                        matches!(ast.node(*expression), Node::Name(_))
                            && bid(res, *expression) == Some(current_bid)
                    })
                {
                    old_bid = bid(res, target);
                }
            }
            let Some(old_bid) = old_bid else { continue };
            let pulse_loop = statements[i + 1];
            let Node::Fornum(_, start, end, step, pulse_body) = ast.node(pulse_loop) else {
                continue;
            };
            let Node::Fornum(_, hstart, hend, hstep, _) = ast.node(helper.loop_node) else {
                continue;
            };
            if step.is_some() != hstep.is_some()
                || super::immutable_values::expression_key(ast, res, *start)
                    != super::immutable_values::expression_key(ast, res, *hstart)
                || super::immutable_values::expression_key(ast, res, *end)
                    != super::immutable_values::expression_key(ast, res, *hend)
            {
                continue;
            }
            let pulse_loop_bid = bid(res, pulse_loop)?;
            let Node::Block(pulse_ss) = ast.node(*pulse_body) else {
                continue;
            };
            if pulse_ss.len() != 1 {
                continue;
            }
            let Node::Assign(pulse_targets, pulse_values) = ast.node(pulse_ss[0]) else {
                continue;
            };
            if pulse_targets.len() != 1 || pulse_values.len() != 1 {
                continue;
            }
            let Node::Index(pulse_obj, pulse_key, _) = ast.node(pulse_targets[0]) else {
                continue;
            };
            if !matches!(ast.node(*pulse_obj), Node::Name(_))
                || !matches!(ast.node(*pulse_key), Node::Name(_))
                || bid(res, *pulse_key) != Some(pulse_loop_bid)
            {
                continue;
            }
            let pulse_bid = bid(res, *pulse_obj)?;
            return Some(Pipeline {
                block,
                snapshot_index: i - 1,
                pulse_index: i + 1,
                prev,
                current: cur,
                pulse_loop,
                pulse_expr: pulse_values[0],
                current_bid,
                old_bid,
                pulse_bid,
                helper_bid,
                call: es[0],
            });
        }
    }
    None
}

fn is_descendant(ast: &Ast, root: NodeId, target: NodeId) -> bool {
    walk(ast, root).contains(&target)
}

fn scan_static_uses(
    ast: &Ast,
    root: NodeId,
    res: &Resolution,
    p: &Pipeline,
    h: &Helper,
) -> Option<(BTreeSet<i64>, BTreeSet<i64>)> {
    let mut current = BTreeSet::new();
    let mut pulse = BTreeSet::new();
    for node in walk(ast, root) {
        if node == h.function || is_descendant(ast, h.function, node) {
            continue;
        }
        if !matches!(ast.node(node), Node::Name(_)) {
            continue;
        }
        let Some(binding) = bid(res, node) else {
            continue;
        };
        if binding != p.current_bid && binding != p.old_bid && binding != p.pulse_bid {
            continue;
        }
        if is_descendant(ast, p.prev, node)
            || is_descendant(ast, p.current, node)
            || is_descendant(ast, p.pulse_loop, node)
        {
            continue;
        }
        let mut accepted = false;
        // Find an index node whose object is this exact name.
        for candidate in walk(ast, root) {
            if let Node::Index(object, key, _) = ast.node(candidate) {
                if *object == node {
                    if let Node::Num(text) = ast.node(*key) {
                        let value = num_val(text);
                        if value.fract() == 0.0 {
                            if binding == p.pulse_bid {
                                pulse.insert(value as i64);
                            } else if binding == p.current_bid {
                                current.insert(value as i64);
                            } else {
                                return None;
                            }
                            accepted = true;
                            break;
                        }
                    }
                }
            }
        }
        if !accepted {
            return None;
        }
    }
    let mut needed = current.clone();
    needed.extend(pulse.iter().copied());
    if needed.is_empty() || needed.iter().any(|k| *k < h.lower || *k > h.upper) {
        return None;
    }
    Some((needed, pulse))
}

fn collect_alpha_symbols(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    function: NodeId,
) -> HashMap<BindingId, SymbolId> {
    let mut bids = BTreeSet::new();
    for bid in res
        .node_bids
        .get(function as usize)
        .cloned()
        .unwrap_or_default()
    {
        bids.insert(bid);
    }
    for node in walk(source, function) {
        match source.node(node) {
            Node::Local(..) | Node::Forin(..) => {
                for bid in res
                    .node_bids
                    .get(node as usize)
                    .cloned()
                    .unwrap_or_default()
                {
                    bids.insert(bid);
                }
            }
            Node::Localfunc(..) | Node::Fornum(..) => {
                if let Some(bid) = bid(res, node) {
                    bids.insert(bid);
                }
            }
            _ => {}
        }
    }
    bids.into_iter()
        .map(|bid| (bid, target.strings.intern(&format!("__i999_{bid}"))))
        .collect()
}

fn alpha_clone_node(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    alpha: &HashMap<BindingId, SymbolId>,
    literal_binding: Option<(BindingId, i64)>,
) -> NodeId {
    let result = alpha_clone_node_inner(target, source, res, node, alpha, literal_binding);
    target
        .nodes
        .derive_from(result, &source.nodes, node, "sparse-decode-alpha-copy");
    use storm_lua_syntax::NameSite;
    match source.node(node) {
        Node::Name(_) if matches!(target.node(result), Node::Name(_)) => {
            target.nodes.copy_name_from(
                result,
                NameSite::Reference,
                &source.nodes,
                node,
                NameSite::Reference,
            )
        }
        Node::Local(names, _) | Node::Forin(names, _, _) => {
            for i in 0..names.len() {
                target.nodes.copy_name_from(
                    result,
                    NameSite::Binding(i as u32),
                    &source.nodes,
                    node,
                    NameSite::Binding(i as u32),
                );
            }
        }
        Node::Localfunc(..) | Node::Fornum(..) => target.nodes.copy_name_from(
            result,
            NameSite::Binding(0),
            &source.nodes,
            node,
            NameSite::Binding(0),
        ),
        Node::Function(names, _, _) => {
            for i in 0..names.len() {
                target.nodes.copy_name_from(
                    result,
                    NameSite::Parameter(i as u32),
                    &source.nodes,
                    node,
                    NameSite::Parameter(i as u32),
                );
            }
        }
        _ => {}
    }
    result
}

fn alpha_clone_node_inner(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    alpha: &HashMap<BindingId, SymbolId>,
    literal_binding: Option<(BindingId, i64)>,
) -> NodeId {
    if matches!(source.node(node), Node::Name(_)) {
        if let Some(node_bid) = bid(res, node) {
            if let Some((binding, value)) = literal_binding {
                if binding == node_bid {
                    return target.push(Node::Num(short_num(value as f64).into()));
                }
            }
            if let Some(symbol) = alpha.get(&node_bid) {
                let result = target.push(Node::Name(*symbol));
                target
                    .nodes
                    .derive_from(result, &source.nodes, node, "sparse-array-read");
                return result;
            }
        }
    }
    match source.node(node).clone() {
        Node::Local(names, expressions) => {
            let bids = res
                .node_bids
                .get(node as usize)
                .cloned()
                .unwrap_or_default();
            let names = names
                .iter()
                .enumerate()
                .map(|(index, symbol)| {
                    bids.get(index)
                        .and_then(|bid| alpha.get(bid))
                        .copied()
                        .unwrap_or(*symbol)
                })
                .collect();
            let expressions = expressions
                .iter()
                .map(|value| alpha_clone_node(target, source, res, *value, alpha, literal_binding))
                .collect();
            target.push(Node::Local(names, expressions))
        }
        Node::Localfunc(name, function) => {
            let name = bid(res, node)
                .and_then(|bid| alpha.get(&bid).copied())
                .unwrap_or(name);
            let function = alpha_clone_node(target, source, res, function, alpha, literal_binding);
            target.push(Node::Localfunc(name, function))
        }
        Node::Fornum(name, start, end, step, body) => {
            let name = bid(res, node)
                .and_then(|bid| alpha.get(&bid).copied())
                .unwrap_or(name);
            let start = alpha_clone_node(target, source, res, start, alpha, literal_binding);
            let end = alpha_clone_node(target, source, res, end, alpha, literal_binding);
            let step = step
                .map(|value| alpha_clone_node(target, source, res, value, alpha, literal_binding));
            let body = alpha_clone_node(target, source, res, body, alpha, literal_binding);
            target.push(Node::Fornum(name, start, end, step, body))
        }
        Node::Forin(names, expressions, body) => {
            let bids = res
                .node_bids
                .get(node as usize)
                .cloned()
                .unwrap_or_default();
            let names = names
                .iter()
                .enumerate()
                .map(|(index, symbol)| {
                    bids.get(index)
                        .and_then(|bid| alpha.get(bid))
                        .copied()
                        .unwrap_or(*symbol)
                })
                .collect();
            let expressions = expressions
                .iter()
                .map(|value| alpha_clone_node(target, source, res, *value, alpha, literal_binding))
                .collect();
            let body = alpha_clone_node(target, source, res, body, alpha, literal_binding);
            target.push(Node::Forin(names, expressions, body))
        }
        Node::Function(parameters, variadic, body) => {
            let bids = res
                .node_bids
                .get(node as usize)
                .cloned()
                .unwrap_or_default();
            let parameters = parameters
                .iter()
                .enumerate()
                .map(|(index, symbol)| {
                    bids.get(index)
                        .and_then(|bid| alpha.get(bid))
                        .copied()
                        .unwrap_or(*symbol)
                })
                .collect();
            let body = alpha_clone_node(target, source, res, body, alpha, literal_binding);
            target.push(Node::Function(parameters, variadic, body))
        }
        original => {
            let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
                alpha_clone_node(target, source, res, child, alpha, literal_binding)
            });
            target.push(mapped)
        }
    }
}

fn shorten_bit(target: &mut Ast, node: NodeId, k: i64) -> NodeId {
    let Node::Bin(op, left, right) = target.node(node).clone() else {
        return node;
    };
    if op != "~=" || !matches!(target.node(right),Node::Num(ref v) if num_val(v)==0.0) {
        return node;
    }
    let bit = match target.node(left) {
        Node::Paren(inner) => *inner,
        _ => left,
    };
    let Node::Bin(bitop, value, mask_expression) = target.node(bit).clone() else {
        return node;
    };
    if bitop != "&" || k < 1 {
        return node;
    }
    let shift = k - 1;
    if shift >= 53 {
        return node;
    }
    let mask = 2f64.powi(shift as i32);
    if !mask.is_finite() || mask > 9_007_199_254_740_991.0 {
        return node;
    }

    let first_generated = target.nodes.len();
    let mask_node = target.push(Node::Num(short_num(mask).into()));
    let bit_expr = target.push(Node::Bin("&".into(), value, mask_node));
    let zero = target.push(Node::Num("0".into()));
    let mask_candidate = target.push(Node::Bin(">".into(), bit_expr, zero));

    let shifted = if shift == 0 {
        value
    } else {
        let shift_node = target.push(Node::Num(short_num(shift as f64).into()));
        target.push(Node::Bin(">>".into(), value, shift_node))
    };
    let two = target.push(Node::Num("2".into()));
    let modulo = target.push(Node::Bin("%".into(), shifted, two));
    let zero2 = target.push(Node::Num("0".into()));
    let modulo_candidate = target.push(Node::Bin(">".into(), modulo, zero2));

    if target.nodes.tracks_origins() {
        for id in first_generated..target.nodes.len() {
            super::origins::within(
                target,
                id as NodeId,
                &[mask_expression, right],
                "sparse-bit-specialization",
            );
        }
        super::origins::within(target, mask_candidate, &[node], "sparse-bit-comparison");
        super::origins::within(target, modulo_candidate, &[node], "sparse-bit-comparison");
    }
    let mut candidates = [node, mask_candidate, modulo_candidate];
    candidates.sort_by_key(|candidate| measure_expr(target, *candidate));
    candidates[0]
}

fn rewrite_static_arrays(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    p: &Pipeline,
    current_names: &HashMap<i64, SymbolId>,
    pulse_names: &HashMap<i64, SymbolId>,
) -> NodeId {
    if let Node::Index(object, key, _) = source.node(node) {
        if matches!(source.node(*object), Node::Name(_)) {
            if let Node::Num(text) = source.node(*key) {
                let k = num_val(text) as i64;
                if bid(res, *object) == Some(p.current_bid) {
                    if let Some(symbol) = current_names.get(&k) {
                        let result = target.push(Node::Name(*symbol));
                        target
                            .nodes
                            .derive_from(result, &source.nodes, node, "sparse-array-read");
                        return result;
                    }
                }
                if bid(res, *object) == Some(p.pulse_bid) {
                    if let Some(symbol) = pulse_names.get(&k) {
                        let result = target.push(Node::Name(*symbol));
                        target
                            .nodes
                            .derive_from(result, &source.nodes, node, "sparse-array-read");
                        return result;
                    }
                }
            }
        }
    }
    if matches!(source.node(node), Node::Block(_)) {
        return node;
    }
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        rewrite_static_arrays(target, source, res, child, p, current_names, pulse_names)
    });
    let result = target.push(mapped);
    target
        .nodes
        .derive_from(result, &source.nodes, node, "sparse-array-copy");
    result
}

fn remove_definition_and_initializers(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    root: NodeId,
    h: &Helper,
    p: &Pipeline,
) {
    let remove_bids = HashSet::from([p.current_bid, p.old_bid, p.pulse_bid]);
    let blocks = walk(target, root)
        .into_iter()
        .filter(|node| matches!(target.node(*node), Node::Block(_)))
        .collect::<Vec<_>>();
    for node in blocks {
        let Node::Block(statements) = target.node(node).clone() else {
            continue;
        };
        let mut out = Vec::new();
        for statement in statements {
            if statement == h.declaration {
                continue;
            }
            if let Node::Local(names, expressions) = target.node(statement).clone() {
                let bids = res
                    .node_bids
                    .get(statement as usize)
                    .cloned()
                    .unwrap_or_default();
                // Newly synthesized locals are not part of the original Resolution.
                if !bids.is_empty() {
                    let keep = bids
                        .iter()
                        .enumerate()
                        .filter_map(|(i, b)| (!remove_bids.contains(b)).then_some(i))
                        .collect::<Vec<_>>();
                    if keep.is_empty() {
                        continue;
                    }
                    if keep.len() != bids.len() {
                        target.nodes.rewrite(
                            statement,
                            Node::Local(
                                keep.iter().map(|i| names[*i]).collect(),
                                keep.iter()
                                    .filter_map(|i| expressions.get(*i).copied())
                                    .collect(),
                            ),
                            "sparse-array-storage-removal",
                        );
                        for (new_index, &old_index) in keep.iter().enumerate() {
                            target.nodes.copy_name_from(
                                statement,
                                storm_lua_syntax::NameSite::Binding(new_index as u32),
                                &source.nodes,
                                statement,
                                storm_lua_syntax::NameSite::Binding(old_index as u32),
                            );
                        }
                    }
                }
            }
            out.push(statement);
        }
        target
            .nodes
            .rewrite(node, Node::Block(out), "sparse-array-storage-removal");
    }
}

pub fn sparse_boolean_decode_scalarization(
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
    let original = measure_size(ast, root);
    let source = clone_ast(ast);
    let res = resolve(&source, root);
    let effects = EffectAnalyzer::new(&source, &res, root, true);
    let helpers = discover_helpers(&source, &res, &effects);
    let Some(p) = find_pipeline(&source, root, &res, &helpers) else {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    };
    let h = &helpers[&p.helper_bid];
    let Some((needed, pulse_static)) = scan_static_uses(&source, root, &res, &p, h) else {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    };
    let Node::Function(_, _, body) = source.node(h.function) else {
        unreachable!()
    };
    let Node::Block(helper_ss) = source.node(*body) else {
        unreachable!()
    };
    let Node::Call(_, call_args, _) = source.node(p.call) else {
        unreachable!()
    };
    if call_args.len() != h.parameters.len() {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let mut target = clone_ast(&source);
    let alpha = collect_alpha_symbols(&mut target, &source, &res, h.function);
    let param_symbols = h
        .parameter_bids
        .iter()
        .map(|bid| alpha[bid])
        .collect::<Vec<_>>();
    let mut pre = Vec::<NodeId>::new();
    if !param_symbols.is_empty() || !call_args.is_empty() {
        let mut names = param_symbols.clone();
        while names.len() < call_args.len() {
            let index = names.len();
            names.push(target.strings.intern(&format!("__drop{index}")));
        }
        let local = target.push(Node::Local(names, call_args.clone()));
        target
            .nodes
            .mark_synthetic(local, "sparse-helper-argument-storage");
        for i in 0..h.parameters.len() {
            target.nodes.copy_name_from(
                local,
                storm_lua_syntax::NameSite::Binding(i as u32),
                &source.nodes,
                h.function,
                storm_lua_syntax::NameSite::Parameter(i as u32),
            );
        }
        pre.push(local)
    }
    // Clone helper setup before the decode loop, alpha-renaming helper locals.
    // The empty result table binding itself is removed from its declaration.
    for statement in helper_ss.iter().copied().take(h.loop_pos) {
        if statement == h.table_decl {
            let Node::Local(names, expressions) = source.node(statement) else {
                continue;
            };
            let bids = res
                .node_bids
                .get(statement as usize)
                .cloned()
                .unwrap_or_default();
            let keep = (0..names.len())
                .filter(|index| *index != h.table_pos)
                .collect::<Vec<_>>();
            if keep.is_empty() {
                continue;
            }
            let new_names = keep
                .iter()
                .map(|index| {
                    bids.get(*index)
                        .and_then(|bid| alpha.get(bid))
                        .copied()
                        .unwrap_or(names[*index])
                })
                .collect();
            let new_values = keep
                .iter()
                .filter_map(|index| expressions.get(*index).copied())
                .map(|value| alpha_clone_node(&mut target, &source, &res, value, &alpha, None))
                .collect();
            let local = target.push(Node::Local(new_names, new_values));
            target.nodes.derive_from(
                local,
                &source.nodes,
                statement,
                "sparse-helper-table-removal",
            );
            for (new_index, &old_index) in keep.iter().enumerate() {
                target.nodes.copy_name_from(
                    local,
                    storm_lua_syntax::NameSite::Binding(new_index as u32),
                    &source.nodes,
                    statement,
                    storm_lua_syntax::NameSite::Binding(old_index as u32),
                );
            }
            pre.push(local);
        } else {
            pre.push(alpha_clone_node(
                &mut target,
                &source,
                &res,
                statement,
                &alpha,
                None,
            ));
        }
    }
    let mut new_names = HashMap::new();
    let mut current_names = HashMap::new();
    let mut pulse_names = HashMap::new();
    for k in &needed {
        new_names.insert(*k, target.strings.intern(&format!("__new{k}")));
        current_names.insert(*k, target.strings.intern(&format!("__cur{k}")));
    }
    for k in &pulse_static {
        pulse_names.insert(*k, target.strings.intern(&format!("__pulse{k}")));
    }
    let mut values = Vec::new();
    for k in &needed {
        let v = alpha_clone_node(
            &mut target,
            &source,
            &res,
            h.value,
            &alpha,
            Some((h.loop_bid, *k)),
        );
        values.push(shorten_bit(&mut target, v, *k));
    }
    let new_local = target.push(Node::Local(
        needed.iter().map(|k| new_names[k]).collect(),
        values,
    ));
    target
        .nodes
        .mark_synthetic(new_local, "sparse-decoded-bit-storage");
    pre.push(new_local);
    // Pulse RHS: rewrite old[i] -> current scalar, current[i] -> new scalar,
    // and the pulse loop induction variable -> literal k.
    #[allow(clippy::too_many_arguments)]
    fn pulse_rewrite(
        target: &mut Ast,
        source: &Ast,
        res: &Resolution,
        node: NodeId,
        p: &Pipeline,
        loop_bid: BindingId,
        k: i64,
        current_symbol: SymbolId,
        new_symbol: SymbolId,
    ) -> NodeId {
        if let Node::Index(object, key, _) = source.node(node) {
            if matches!(source.node(*object), Node::Name(_))
                && matches!(source.node(*key), Node::Name(_))
                && bid(res, *key) == Some(loop_bid)
            {
                if bid(res, *object) == Some(p.old_bid) {
                    let result = target.push(Node::Name(current_symbol));
                    target
                        .nodes
                        .derive_from(result, &source.nodes, node, "sparse-old-bit-read");
                    return result;
                }
                if bid(res, *object) == Some(p.current_bid) {
                    let result = target.push(Node::Name(new_symbol));
                    target
                        .nodes
                        .derive_from(result, &source.nodes, node, "sparse-new-bit-read");
                    return result;
                }
            }
        }
        if matches!(source.node(node), Node::Name(_)) && bid(res, node) == Some(loop_bid) {
            let result = target.push(Node::Num(short_num(k as f64).into()));
            target
                .nodes
                .derive_from(result, &source.nodes, node, "sparse-pulse-index");
            return result;
        }
        let original = source.node(node).clone();
        let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |c| {
            pulse_rewrite(
                target,
                source,
                res,
                c,
                p,
                loop_bid,
                k,
                current_symbol,
                new_symbol,
            )
        });
        let result = target.push(mapped);
        target
            .nodes
            .derive_from(result, &source.nodes, node, "sparse-pulse-copy");
        result
    }
    let pulse_loop_bid = bid(&res, p.pulse_loop).unwrap_or(0);
    let mut rhs_pulse = Vec::new();
    for k in &pulse_static {
        rhs_pulse.push(pulse_rewrite(
            &mut target,
            &source,
            &res,
            p.pulse_expr,
            &p,
            pulse_loop_bid,
            *k,
            current_names[k],
            new_names[k],
        ));
    }
    let mut assign_targets = Vec::new();
    for k in &pulse_static {
        let name = target.push(Node::Name(pulse_names[k]));
        target.nodes.mark_synthetic(name, "sparse-pulse-storage");
        assign_targets.push(name);
    }
    for k in &needed {
        let name = target.push(Node::Name(current_names[k]));
        target.nodes.mark_synthetic(name, "sparse-current-storage");
        assign_targets.push(name);
    }
    let mut assign_values = rhs_pulse;
    for k in &needed {
        let name = target.push(Node::Name(new_names[k]));
        target
            .nodes
            .derive_from(name, &source.nodes, h.value, "sparse-decoded-value");
        assign_values.push(name);
    }
    let assignment = target.push(Node::Assign(assign_targets, assign_values));
    target
        .nodes
        .mark_synthetic(assignment, "sparse-state-update");
    pre.push(assignment);
    // Rewrite the matched block, replacing exactly snapshot/decode/pulse by pre.
    for block in walk(&source, root)
        .into_iter()
        .filter(|n| matches!(source.node(*n), Node::Block(_)))
    {
        let Node::Block(ss) = source.node(block).clone() else {
            continue;
        };
        let mut out = Vec::new();
        let mut i = 0;
        while i < ss.len() {
            if block == p.block && i == p.snapshot_index {
                out.extend(pre.iter().copied());
                i = p.pulse_index + 1;
                continue;
            }
            out.push(ss[i]);
            i += 1;
        }
        target
            .nodes
            .rewrite(block, Node::Block(out), "sparse-pipeline-rewrite");
    }
    // Static reads outside the pattern become scalars. Process parents without
    // cloning already-rewritten block identities.
    for node in (0..source.nodes.len() as NodeId).rev() {
        if matches!(source.node(node), Node::Block(_)) {
            continue;
        }
        let replacement = rewrite_static_arrays(
            &mut target,
            &source,
            &res,
            node,
            &p,
            &current_names,
            &pulse_names,
        );
        let value = target.node(replacement).clone();
        let origin = target.nodes.capture_origin(replacement);
        target.nodes[node as usize] = value;
        target
            .nodes
            .finish_rewrite(node, origin, "sparse-array-rewrite");
    }
    remove_definition_and_initializers(&mut target, &source, &res, root, h, &p);
    super::locals::eliminate_dead_locals(&mut target, root);
    let new_len = measure_size(&target, root);
    if new_len < original {
        *ast = target;
        PassResult {
            root,
            saved: Some((original - new_len) as u64),
            details: Some(vec![format!(
                "indices={:?};pulse={:?}",
                needed, pulse_static
            )]),
        }
    } else {
        PassResult {
            root,
            saved: Some(0),
            details: None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::{parse_source, Printer};

    #[test]
    fn rejects_dynamic_sparse_table_uses() {
        let source = "local current,old,pulse={},{},{} local function decode(data)local out={}for i=1,8 do out[i]=(data&(1<<(i-1)))~=0 end return out end function onTick()old=current current=decode(input.getNumber(1))for i=1,8 do pulse[i]=not old[i]and current[i]end local k=input.getNumber(2)output.setBool(1,current[k])end";
        let (mut ast, root) = parse_source(source).unwrap();
        let result = sparse_boolean_decode_scalarization(&mut ast, root, true);
        assert_eq!(result.saved.unwrap_or(0), 0);
        let _ = Printer::new(&ast, false).output(result.root);
    }
}
