//! Color unpack helper specialization (`passes/misc.ts`).

use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, TableField};
use storm_lua_syntax::size::measure_size;

#[derive(Clone)]
struct FunctionInfo {
    bid: BindingId,
    function: NodeId,
    declaration: NodeId,
    param_bids: Vec<BindingId>,
}

fn bid_of(res: &Resolution, id: NodeId) -> Option<BindingId> {
    res.node_bid.get(id as usize).copied().flatten()
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn pure_table_literal(ast: &Ast, node: NodeId) -> bool {
    match ast.node(node) {
        Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil => true,
        Node::Un(_, expression) => pure_table_literal(ast, *expression),
        Node::Bin(_, left, right) => {
            pure_table_literal(ast, *left) && pure_table_literal(ast, *right)
        }
        Node::Table(fields) => fields.iter().all(|field| match field {
            TableField::Arr(value) | TableField::Name(_, value) => pure_table_literal(ast, *value),
            TableField::KVar(key, value) => {
                pure_table_literal(ast, *key) && pure_table_literal(ast, *value)
            }
        }),
        _ => false,
    }
}

fn functions(ast: &Ast, res: &Resolution) -> Vec<FunctionInfo> {
    res.bindings
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(bid, binding)| {
            Some(FunctionInfo {
                bid: bid as BindingId,
                function: binding.function_node?,
                declaration: binding.decl_node?,
                param_bids: res
                    .node_bids
                    .get(binding.function_node? as usize)
                    .cloned()
                    .unwrap_or_default(),
            })
        })
        .filter(|info| matches!(ast.node(info.function), Node::Function(..)))
        .collect()
}

fn remove_definition(ast: &mut Ast, declaration: NodeId) {
    ast.nodes
        .retain_block_statements(|id| id != declaration, "color-unpack-declaration-removal");
}

pub fn specialize_color_unpack_helpers(ast: &mut Ast, root: NodeId) -> PassResult {
    let original_size = measure_size(ast, root);
    let source = clone_ast(ast);
    let res = resolve(&source, root);
    let analyzer = EffectAnalyzer::new(&source, &res, root, true);
    let infos = functions(&source, &res);

    let mut table_bindings = HashMap::<BindingId, NodeId>::new();
    let mut nodes = Vec::new();
    storm_lua_analysis::effects::walk(&source, root, &mut nodes);
    for node in &nodes {
        if let Node::Local(names, expressions) = source.node(*node) {
            if names.len() != expressions.len() {
                continue;
            }
            let bids = res
                .node_bids
                .get(*node as usize)
                .cloned()
                .unwrap_or_default();
            for (index, bid) in bids.into_iter().enumerate() {
                let Some(expression) = expressions.get(index).copied() else {
                    continue;
                };
                let Node::Table(fields) = source.node(expression) else {
                    continue;
                };
                if fields.iter().all(|field| match field {
                    TableField::Arr(value) => pure_table_literal(&source, *value),
                    _ => false,
                }) {
                    table_bindings.insert(bid, expression);
                }
            }
        }
    }

    let mut helpers = HashMap::<BindingId, FunctionInfo>::new();
    for info in &infos {
        let Node::Function(parameters, variadic, body) = source.node(info.function) else {
            continue;
        };
        if parameters.len() != 1 || *variadic || info.param_bids.len() != 1 {
            continue;
        }
        let Node::Block(statements) = source.node(*body) else {
            continue;
        };
        if statements.len() != 1 {
            continue;
        }
        let Node::Callstat(call) = source.node(statements[0]) else {
            continue;
        };
        let Node::Call(function, arguments, _) = source.node(*call) else {
            continue;
        };
        if analyzer.resolve_builtin_reference(*function).as_deref() != Some("screen.setColor")
            || arguments.len() != 1
        {
            continue;
        }
        let Node::Call(unpack_fn, unpack_args, _) = source.node(arguments[0]) else {
            continue;
        };
        if analyzer.resolve_builtin_reference(*unpack_fn).as_deref() != Some("table.unpack")
            || unpack_args.len() != 1
            || !matches!(source.node(unpack_args[0]), Node::Name(_))
            || res.node_bid.get(unpack_args[0] as usize).copied().flatten()
                != Some(info.param_bids[0])
        {
            continue;
        }
        helpers.insert(info.bid, info.clone());
    }

    // `node_write` only marks writes to the table binding itself.  Indexed
    // writes (including a simple alias) must also invalidate specialization:
    // table.unpack observes the current element values at call time.
    let mut aliases = HashMap::<BindingId, BindingId>::new();
    for node in &nodes {
        let Node::Assign(targets, expressions) = source.node(*node) else {
            continue;
        };
        for (target, expression) in targets.iter().zip(expressions) {
            if !matches!(source.node(*target), Node::Name(_))
                || !matches!(source.node(*expression), Node::Name(_))
            {
                continue;
            }
            let (Some(target_bid), Some(source_bid)) =
                (bid_of(&res, *target), bid_of(&res, *expression))
            else {
                continue;
            };
            if table_bindings.contains_key(&source_bid) {
                aliases.insert(target_bid, source_bid);
            }
        }
    }
    let root_table = |mut bid: BindingId| {
        let mut seen = HashSet::new();
        while let Some(next) = aliases.get(&bid).copied() {
            if !seen.insert(bid) {
                break;
            }
            bid = next;
        }
        bid
    };
    let mut mutable_tables = HashSet::<BindingId>::new();
    for node in &nodes {
        if matches!(source.node(*node), Node::Name(_))
            && res.node_write.get(*node as usize).copied().unwrap_or(false)
        {
            if let Some(bid) = bid_of(&res, *node).map(root_table) {
                if table_bindings.contains_key(&bid) {
                    mutable_tables.insert(bid);
                }
            }
        }
        let Node::Assign(targets, _) = source.node(*node) else {
            continue;
        };
        for target in targets {
            let mut target_nodes = Vec::new();
            storm_lua_syntax::ast_utils::walk(&source, *target, &mut |id| target_nodes.push(id));
            for target_node in target_nodes {
                let Node::Index(object, _, _) = source.node(target_node) else {
                    continue;
                };
                let Some(bid) = bid_of(&res, *object).map(root_table) else {
                    continue;
                };
                if table_bindings.contains_key(&bid) {
                    mutable_tables.insert(bid);
                }
            }
        }
    }
    table_bindings.retain(|bid, _| !mutable_tables.contains(bid));
    if helpers.is_empty() {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }

    let mut calls = HashMap::<BindingId, usize>::new();
    let mut direct_names = HashSet::<NodeId>::new();
    let mut definition_names = HashSet::<NodeId>::new();
    for node in &nodes {
        match source.node(*node) {
            Node::Call(function, _, _) if matches!(source.node(*function), Node::Name(_)) => {
                if let Some(bid) = res.node_bid.get(*function as usize).copied().flatten() {
                    if helpers.contains_key(&bid) {
                        *calls.entry(bid).or_default() += 1;
                        direct_names.insert(*function);
                    }
                }
            }
            Node::Funcstat(target, _) if matches!(source.node(*target), Node::Name(_)) => {
                if let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() {
                    if helpers.contains_key(&bid) {
                        definition_names.insert(*target);
                    }
                }
            }
            _ => {}
        }
    }
    let mut non_call_refs = HashSet::<BindingId>::new();
    for node in &nodes {
        if !matches!(source.node(*node), Node::Name(_))
            || direct_names.contains(node)
            || definition_names.contains(node)
        {
            continue;
        }
        if let Some(bid) = res.node_bid.get(*node as usize).copied().flatten() {
            if helpers.contains_key(&bid) {
                non_call_refs.insert(bid);
            }
        }
    }

    let mut candidate = clone_ast(&source);
    let mut replaced = HashMap::<BindingId, usize>::new();
    for node in &nodes {
        let Node::Callstat(call) = source.node(*node) else {
            continue;
        };
        let Node::Call(function, arguments, method) = source.node(*call) else {
            continue;
        };
        if arguments.len() != 1 || !matches!(source.node(*function), Node::Name(_)) {
            continue;
        }
        let Some(bid) = res.node_bid.get(*function as usize).copied().flatten() else {
            continue;
        };
        if !helpers.contains_key(&bid) {
            continue;
        }
        let argument = arguments[0];
        let table = match source.node(argument) {
            Node::Table(fields)
                if fields
                    .iter()
                    .all(|field| matches!(field, TableField::Arr(_))) =>
            {
                Some(argument)
            }
            Node::Name(_) => res
                .node_bid
                .get(argument as usize)
                .copied()
                .flatten()
                .and_then(|table_bid| table_bindings.get(&table_bid).copied()),
            _ => None,
        };
        let Some(table) = table else {
            continue;
        };
        let Node::Table(fields) = source.node(table) else {
            continue;
        };
        let args = fields
            .iter()
            .filter_map(|field| match field {
                TableField::Arr(value) => Some(*value),
                _ => None,
            })
            .collect::<Vec<_>>();
        let screen = candidate.strings.intern("screen");
        let screen_name = candidate.push(Node::Name(screen));
        let key = candidate.push(Node::Str(
            storm_lua_syntax::numeric::quote_lua("setColor").into(),
        ));
        let set_color = candidate.push(Node::Index(screen_name, key, true));
        let new_call = candidate.push(Node::Call(set_color, args, method.clone()));
        let info = &helpers[&bid];
        let Node::Function(_, _, body) = source.node(info.function) else {
            unreachable!()
        };
        let Node::Block(stmts) = source.node(*body) else {
            unreachable!()
        };
        let Node::Callstat(original_call) = source.node(stmts[0]) else {
            unreachable!()
        };
        let Node::Call(original_fn, _, _) = source.node(*original_call) else {
            unreachable!()
        };
        candidate.nodes.derive_from(
            set_color,
            &source.nodes,
            *original_fn,
            "color-unpack-callee",
        );
        if let Node::Index(object, member, _) = source.node(*original_fn) {
            candidate
                .nodes
                .derive_from(screen_name, &source.nodes, *object, "color-unpack-callee");
            candidate
                .nodes
                .derive_from(key, &source.nodes, *member, "color-unpack-callee");
        } else {
            candidate.nodes.derive_from(
                screen_name,
                &source.nodes,
                *original_fn,
                "color-unpack-callee-expansion",
            );
            candidate.nodes.derive_from(
                key,
                &source.nodes,
                *original_fn,
                "color-unpack-callee-expansion",
            );
        }
        candidate
            .nodes
            .derive_from(new_call, &source.nodes, *call, "color-unpack-expansion");
        candidate.nodes.relate_from(
            new_call,
            &source.nodes,
            *original_call,
            "color-unpack-helper",
        );
        candidate
            .nodes
            .rewrite(*node, Node::Callstat(new_call), "color-unpack-expansion");
        *replaced.entry(bid).or_default() += 1;
    }
    for (bid, info) in &helpers {
        if !non_call_refs.contains(bid)
            && replaced.get(bid).copied().unwrap_or(0) == calls.get(bid).copied().unwrap_or(0)
        {
            remove_definition(&mut candidate, info.declaration);
        }
    }
    super::locals::eliminate_dead_locals(&mut candidate, root);
    let new_size = measure_size(&candidate, root);
    if new_size < original_size {
        *ast = candidate;
        PassResult {
            root,
            saved: Some((original_size - new_size) as u64),
            details: None,
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
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    #[test]
    fn expands_array_color_helper() {
        let (mut ast, root) = parse_source(
            "local function color(c)screen.setColor(table.unpack(c))end function onDraw()color({1,2,3})end",
        )
        .unwrap();
        specialize_color_unpack_helpers(&mut ast, root);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("screen.setColor(1,2,3)"), "{out}");
    }

    #[test]
    fn keeps_helper_definition_for_function_value_reference() {
        let (mut ast, root) = parse_source(
            "local function color(c)screen.setColor(table.unpack(c))end g=color function onDraw()color({1,2,3})g({4,5,6})end",
        )
        .unwrap();
        specialize_color_unpack_helpers(&mut ast, root);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("function color"), "{out}");
        assert!(out.contains("g=color"), "{out}");
    }
}
