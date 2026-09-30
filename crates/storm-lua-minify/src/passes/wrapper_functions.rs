//! Numeric constant wrapper merging (`passes/wrapper-functions.ts`).

use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use crate::scope_rename::scope_rename_fast;
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId};
use storm_lua_syntax::numeric::{num_val, short_num};
use storm_lua_syntax::size::measure_size;

#[derive(Clone)]
struct Wrapper {
    bid: BindingId,
    declaration: NodeId,
    function: NodeId,
    callee: NodeId,
    values: Vec<f64>,
    arguments: Vec<NodeId>,
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

fn collect_wrappers(ast: &Ast, res: &Resolution) -> Vec<Wrapper> {
    let mut wrappers = Vec::new();
    for (bid, binding) in res.bindings.iter().enumerate().skip(1) {
        let (Some(function), Some(declaration)) = (binding.function_node, binding.decl_node) else {
            continue;
        };
        let Node::Function(parameters, variadic, body) = ast.node(function) else {
            continue;
        };
        if *variadic || !parameters.is_empty() {
            continue;
        }
        let Node::Block(statements) = ast.node(*body) else {
            continue;
        };
        if statements.len() != 1 {
            continue;
        }
        let Node::Callstat(call) = ast.node(statements[0]) else {
            continue;
        };
        let Node::Call(callee, arguments, _) = ast.node(*call) else {
            continue;
        };
        if arguments.is_empty()
            || !arguments
                .iter()
                .all(|argument| matches!(ast.node(*argument), Node::Num(_)))
        {
            continue;
        }
        let values = arguments
            .iter()
            .map(|argument| match ast.node(*argument) {
                Node::Num(value) => num_val(value),
                _ => unreachable!(),
            })
            .collect();
        wrappers.push(Wrapper {
            bid: bid as BindingId,
            declaration,
            function,
            callee: *callee,
            arguments: arguments.clone(),
            values,
        });
    }
    wrappers
}

fn wrapper_references(
    ast: &Ast,
    root: NodeId,
    res: &Resolution,
    wrapper_bids: &HashSet<BindingId>,
) -> (HashMap<BindingId, usize>, HashSet<BindingId>) {
    let mut direct_calls = HashMap::<BindingId, usize>::new();
    let mut allowed_names = HashSet::<NodeId>::new();
    let mut nodes = Vec::new();
    storm_lua_analysis::effects::walk(ast, root, &mut nodes);
    for node in &nodes {
        match ast.node(*node) {
            Node::Call(function, arguments, method)
                if method.is_none()
                    && arguments.is_empty()
                    && matches!(ast.node(*function), Node::Name(_)) =>
            {
                if let Some(bid) = res.node_bid.get(*function as usize).copied().flatten() {
                    if wrapper_bids.contains(&bid) {
                        *direct_calls.entry(bid).or_default() += 1;
                        allowed_names.insert(*function);
                    }
                }
            }
            Node::Funcstat(target, _) if matches!(ast.node(*target), Node::Name(_)) => {
                if let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() {
                    if wrapper_bids.contains(&bid)
                        && res.bindings[bid as usize].decl_node == Some(*node)
                    {
                        allowed_names.insert(*target);
                    }
                }
            }
            _ => {}
        }
    }
    let mut invalid = HashSet::new();
    for node in nodes {
        if allowed_names.contains(&node) || !matches!(ast.node(node), Node::Name(_)) {
            continue;
        }
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            if wrapper_bids.contains(&bid) {
                invalid.insert(bid);
            }
        }
    }
    (direct_calls, invalid)
}

fn all_name_strings(ast: &Ast, root: NodeId) -> HashSet<String> {
    let mut nodes = Vec::new();
    storm_lua_analysis::effects::walk(ast, root, &mut nodes);
    nodes
        .into_iter()
        .filter_map(|node| match ast.node(node) {
            Node::Name(symbol) => Some(ast.strings.get(*symbol).to_string()),
            _ => None,
        })
        .collect()
}

fn remove_declaration(ast: &mut Ast, declaration: NodeId) {
    ast.nodes
        .retain_block_statements(|id| id != declaration, "wrapper-declaration-removal");
}

#[allow(clippy::too_many_arguments)]
fn rewrite_wrapper_calls(
    ast: &mut Ast,
    source: &Ast,
    res: &Resolution,
    root: NodeId,
    a: BindingId,
    b: BindingId,
    keeper_symbol: SymbolId,
    mut argument: impl FnMut(&mut Ast, BindingId) -> NodeId,
) {
    let mut nodes = Vec::new();
    storm_lua_analysis::effects::walk(source, root, &mut nodes);
    for node in nodes.into_iter().rev() {
        let Node::Call(function, arguments, method) = source.node(node) else {
            continue;
        };
        if method.is_some()
            || !arguments.is_empty()
            || !matches!(source.node(*function), Node::Name(_))
        {
            continue;
        }
        let Some(bid) = res.node_bid.get(*function as usize).copied().flatten() else {
            continue;
        };
        if bid != a && bid != b {
            continue;
        }
        let fn_node = ast.push(Node::Name(keeper_symbol));
        ast.nodes.derive_from(
            fn_node,
            &source.nodes,
            *function,
            "merged-wrapper-reference",
        );
        ast.nodes.copy_name_from(
            fn_node,
            storm_lua_syntax::NameSite::Reference,
            &source.nodes,
            *function,
            storm_lua_syntax::NameSite::Reference,
        );
        let arg = argument(ast, bid);
        ast.nodes.rewrite(
            node,
            Node::Call(fn_node, vec![arg], None),
            "merged-wrapper-call",
        );
    }
}

fn same_callee(ast: &Ast, res: &Resolution, a: NodeId, b: NodeId) -> bool {
    super::immutable_values::expression_key(ast, res, a)
        == super::immutable_values::expression_key(ast, res, b)
}

fn construct_translated_candidate(
    source: &Ast,
    root: NodeId,
    res: &Resolution,
    a: &Wrapper,
    b: &Wrapper,
    base: usize,
    parameter_name: &str,
) -> Ast {
    let mut candidate = clone_ast(source);
    let parameter_symbol = candidate.strings.intern(parameter_name);
    let offsets = a
        .values
        .iter()
        .map(|value| *value - a.values[base])
        .collect::<Vec<_>>();
    let mut args = Vec::with_capacity(offsets.len());
    for (argument_index, offset) in offsets.into_iter().enumerate() {
        let first = candidate.nodes.len();
        let parameter = candidate.push(Node::Name(parameter_symbol));
        let expression = if offset == 0.0 {
            parameter
        } else {
            let magnitude = candidate.push(Node::Num(short_num(offset.abs()).into()));
            candidate.push(Node::Bin(
                if offset > 0.0 { "+" } else { "-" }.to_string(),
                parameter,
                magnitude,
            ))
        };
        annotate_wrapper_argument(
            &mut candidate,
            source,
            first,
            &[a.arguments[argument_index], a.arguments[base]],
            "translated-wrapper-argument",
        );
        args.push(expression);
    }
    if let Node::Function(_, variadic, body) = candidate.node(a.function).clone() {
        candidate.nodes.rewrite(
            a.function,
            Node::Function(vec![parameter_symbol], variadic, body),
            "merged-wrapper-function",
        );
        candidate.nodes.relate_from(
            a.function,
            &source.nodes,
            b.function,
            "merged-wrapper-function",
        );
        let Node::Block(statements) = candidate.node(body).clone() else {
            unreachable!()
        };
        let Node::Callstat(call) = candidate.node(statements[0]).clone() else {
            unreachable!()
        };
        let Node::Call(callee, _, method) = candidate.node(call).clone() else {
            unreachable!()
        };
        candidate.nodes.rewrite(
            call,
            Node::Call(callee, args, method),
            "merged-wrapper-body",
        );
        candidate
            .nodes
            .relate_from(call, &source.nodes, b.function, "merged-wrapper-body");
    }
    let keeper_symbol = res.bindings[a.bid as usize].name;
    rewrite_wrapper_calls(
        &mut candidate,
        source,
        res,
        root,
        a.bid,
        b.bid,
        keeper_symbol,
        |ast, bid| {
            let wrapper = if bid == a.bid { a } else { b };
            let value = ast.push(Node::Num(short_num(wrapper.values[base]).into()));
            ast.nodes.derive_from(
                value,
                &source.nodes,
                wrapper.arguments[base],
                "translated-wrapper-actual",
            );
            value
        },
    );
    remove_declaration(&mut candidate, b.declaration);
    candidate
}

fn affine_argument(ast: &mut Ast, parameter_symbol: SymbolId, base: f64, delta: f64) -> NodeId {
    if delta == 0.0 {
        return ast.push(Node::Num(short_num(base).into()));
    }
    let parameter = ast.push(Node::Name(parameter_symbol));
    let magnitude = delta.abs();
    let term = if magnitude == 1.0 {
        parameter
    } else {
        let number = ast.push(Node::Num(short_num(magnitude).into()));
        ast.push(Node::Bin("*".to_string(), parameter, number))
    };
    if base == 0.0 {
        if delta > 0.0 {
            term
        } else {
            ast.push(Node::Un("-".to_string(), term))
        }
    } else {
        let base_node = ast.push(Node::Num(short_num(base).into()));
        ast.push(Node::Bin(
            if delta > 0.0 { "+" } else { "-" }.to_string(),
            base_node,
            term,
        ))
    }
}

fn construct_affine_candidate(
    source: &Ast,
    root: NodeId,
    res: &Resolution,
    a: &Wrapper,
    b: &Wrapper,
    parameter_name: &str,
) -> Ast {
    let mut candidate = clone_ast(source);
    let parameter_symbol = candidate.strings.intern(parameter_name);
    let mut args = Vec::with_capacity(a.values.len());
    for (index, base) in a.values.iter().copied().enumerate() {
        let first = candidate.nodes.len();
        args.push(affine_argument(
            &mut candidate,
            parameter_symbol,
            base,
            b.values[index] - base,
        ));
        annotate_wrapper_argument(
            &mut candidate,
            source,
            first,
            &[a.arguments[index], b.arguments[index]],
            "affine-wrapper-argument",
        );
    }
    if let Node::Function(_, variadic, body) = candidate.node(a.function).clone() {
        candidate.nodes.rewrite(
            a.function,
            Node::Function(vec![parameter_symbol], variadic, body),
            "merged-wrapper-function",
        );
        candidate.nodes.relate_from(
            a.function,
            &source.nodes,
            b.function,
            "merged-wrapper-function",
        );
        let Node::Block(statements) = candidate.node(body).clone() else {
            unreachable!()
        };
        let Node::Callstat(call) = candidate.node(statements[0]).clone() else {
            unreachable!()
        };
        let Node::Call(callee, _, method) = candidate.node(call).clone() else {
            unreachable!()
        };
        candidate.nodes.rewrite(
            call,
            Node::Call(callee, args, method),
            "merged-wrapper-body",
        );
        candidate
            .nodes
            .relate_from(call, &source.nodes, b.function, "merged-wrapper-body");
    }
    let keeper_symbol = res.bindings[a.bid as usize].name;
    rewrite_wrapper_calls(
        &mut candidate,
        source,
        res,
        root,
        a.bid,
        b.bid,
        keeper_symbol,
        |ast, bid| {
            let selector = ast.push(Node::Num(if bid == a.bid { "0" } else { "1" }.into()));
            ast.nodes
                .mark_synthetic(selector, "affine-wrapper-selector");
            selector
        },
    );
    remove_declaration(&mut candidate, b.declaration);
    candidate
}

pub fn merge_translated_constant_wrappers(ast: &mut Ast, root: NodeId) -> PassResult {
    let source = clone_ast(ast);
    let res = resolve(&source, root);
    let wrappers = collect_wrappers(&source, &res);
    if wrappers.len() < 2 {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let wrapper_bids = wrappers
        .iter()
        .map(|wrapper| wrapper.bid)
        .collect::<HashSet<_>>();
    let (calls, invalid) = wrapper_references(&source, root, &res, &wrapper_bids);
    let original = renamed_size(&source, root);
    let names = all_name_strings(&source, root);
    let mut serial = 0usize;
    let parameter = loop {
        let value = format!("__stormmin_wrapper_{serial}");
        if !names.contains(&value) {
            break value;
        }
        serial += 1;
    };
    let mut best: Option<(Ast, usize)> = None;
    for left in 0..wrappers.len() {
        for right in left + 1..wrappers.len() {
            let a = &wrappers[left];
            let b = &wrappers[right];
            if invalid.contains(&a.bid)
                || invalid.contains(&b.bid)
                || calls.get(&a.bid).copied().unwrap_or(0) == 0
                || calls.get(&b.bid).copied().unwrap_or(0) == 0
                || !same_callee(&source, &res, a.callee, b.callee)
                || a.values.len() != b.values.len()
            {
                continue;
            }
            for base in 0..a.values.len() {
                if a.values[base] == b.values[base] {
                    continue;
                }
                let offsets = a
                    .values
                    .iter()
                    .map(|value| *value - a.values[base])
                    .collect::<Vec<_>>();
                if !b
                    .values
                    .iter()
                    .enumerate()
                    .all(|(index, value)| *value - b.values[base] == offsets[index])
                {
                    continue;
                }
                let candidate =
                    construct_translated_candidate(&source, root, &res, a, b, base, &parameter);
                let size = renamed_size(&candidate, root);
                if size < original && best.as_ref().is_none_or(|(_, best_size)| size < *best_size) {
                    best = Some((candidate, size));
                }
            }
        }
    }
    if let Some((candidate, size)) = best {
        *ast = candidate;
        PassResult {
            root,
            saved: Some((original - size) as u64),
            details: Some(vec!["merged=1".to_string()]),
        }
    } else {
        PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["merged=0".to_string()]),
        }
    }
}

pub fn merge_affine_constant_wrappers(ast: &mut Ast, root: NodeId) -> PassResult {
    let source = clone_ast(ast);
    let res = resolve(&source, root);
    let wrappers = collect_wrappers(&source, &res);
    let wrapper_bids = wrappers
        .iter()
        .map(|wrapper| wrapper.bid)
        .collect::<HashSet<_>>();
    let (calls, invalid) = wrapper_references(&source, root, &res, &wrapper_bids);
    let original = renamed_size(&source, root);
    let names = all_name_strings(&source, root);
    let mut serial = 0usize;
    let parameter = loop {
        let value = format!("__stormmin_affine_wrapper_{serial}");
        if !names.contains(&value) {
            break value;
        }
        serial += 1;
    };
    let mut best: Option<(Ast, usize)> = None;
    for left in 0..wrappers.len() {
        for right in left + 1..wrappers.len() {
            for &(a_index, b_index) in &[(left, right), (right, left)] {
                let a = &wrappers[a_index];
                let b = &wrappers[b_index];
                if invalid.contains(&a.bid)
                    || invalid.contains(&b.bid)
                    || calls.get(&a.bid).copied().unwrap_or(0) == 0
                    || calls.get(&b.bid).copied().unwrap_or(0) == 0
                    || !same_callee(&source, &res, a.callee, b.callee)
                    || a.values.len() != b.values.len()
                {
                    continue;
                }
                let candidate = construct_affine_candidate(&source, root, &res, a, b, &parameter);
                let size = renamed_size(&candidate, root);
                if size < original && best.as_ref().is_none_or(|(_, best_size)| size < *best_size) {
                    best = Some((candidate, size));
                }
            }
        }
    }
    if let Some((candidate, size)) = best {
        *ast = candidate;
        PassResult {
            root,
            saved: Some((original - size) as u64),
            details: Some(vec!["merged=1".to_string()]),
        }
    } else {
        PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["merged=0".to_string()]),
        }
    }
}

// This range consists only of the newly built single argument expression.
// The synthetic parameter is storage; constants and arithmetic derive from
// the selected original wrapper arguments, not from the whole function body.
fn annotate_wrapper_argument(
    target: &mut Ast,
    source: &Ast,
    first: usize,
    inputs: &[NodeId],
    reason: &str,
) {
    if !target.nodes.tracks_origins() {
        return;
    }
    let complete = inputs.iter().all(|&n| source.nodes.origin(n).is_some());
    for id in first..target.nodes.len() {
        let id = id as NodeId;
        if matches!(target.node(id), Node::Name(_)) {
            target.nodes.mark_synthetic(id, "merged-wrapper-parameter");
        } else if complete {
            target
                .nodes
                .derive_from(id, &source.nodes, inputs[0], reason);
            for &input in &inputs[1..] {
                target.nodes.relate_from(id, &source.nodes, input, reason);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    #[test]
    fn translated_wrappers_merge() {
        let source = "function dark()screen.setColor(45,45,55)end function bg()screen.setColor(60,60,70)end function onDraw()dark()bg()dark()bg()dark()bg()end";
        let (mut ast, root) = parse_source(source).unwrap();
        let result = merge_translated_constant_wrappers(&mut ast, root);
        assert!(result.saved.unwrap_or(0) > 0);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("function dark("), "{out}");
    }

    #[test]
    fn affine_wrappers_merge() {
        let source = "function warm()screen.setColor(180,180,200)end function dark()screen.setColor(35,30,30)end function onDraw()warm()dark()warm()dark()warm()dark()end";
        let (mut ast, root) = parse_source(source).unwrap();
        let result = merge_affine_constant_wrappers(&mut ast, root);
        assert!(result.saved.unwrap_or(0) > 0);
    }
}
