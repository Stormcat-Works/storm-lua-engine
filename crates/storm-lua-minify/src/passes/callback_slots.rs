//! Callback-exclusive function slotting (`passes/callback-function-slots.ts`).
//!
//! Helpers reachable only from `onTick` and helpers reachable only from
//! `onDraw` cannot be live at the same time. This pass pairs the highest-value
//! helpers and moves each definition into its callback while reusing one global
//! function name for both phases. Every prefix length is measured after scope
//! renaming and only the smallest candidate is committed.

use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use crate::scope_rename::measure_renamed_size;
use storm_lua_analysis::resolver::{resolve, BindingId};
use storm_lua_syntax::ast::{Ast, Node, NodeId};

#[derive(Clone)]
struct Definition {
    bid: BindingId,
    statement: NodeId,
    frequency: u32,
    order: usize,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

#[allow(clippy::too_many_arguments)]
fn scan_calls(
    ast: &Ast,
    res: &storm_lua_analysis::resolver::Resolution,
    definitions: &HashSet<BindingId>,
    node: NodeId,
    owner: BindingId,
    is_call_target: bool,
    calls: &mut HashMap<BindingId, HashSet<BindingId>>,
    non_call_references: &mut HashSet<BindingId>,
) {
    if matches!(ast.node(node), Node::Name(_)) {
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            if definitions.contains(&bid) {
                if is_call_target {
                    calls.entry(owner).or_default().insert(bid);
                } else {
                    non_call_references.insert(bid);
                }
            }
        }
        return;
    }
    match ast.node(node) {
        Node::Function(_, _, body) => scan_calls(
            ast,
            res,
            definitions,
            *body,
            owner,
            false,
            calls,
            non_call_references,
        ),
        Node::Call(function, args, _) => {
            scan_calls(
                ast,
                res,
                definitions,
                *function,
                owner,
                true,
                calls,
                non_call_references,
            );
            for argument in args {
                scan_calls(
                    ast,
                    res,
                    definitions,
                    *argument,
                    owner,
                    false,
                    calls,
                    non_call_references,
                );
            }
        }
        _ => storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
            scan_calls(
                ast,
                res,
                definitions,
                child,
                owner,
                false,
                calls,
                non_call_references,
            );
        }),
    }
}

fn reachable(
    start: BindingId,
    calls: &HashMap<BindingId, HashSet<BindingId>>,
) -> HashSet<BindingId> {
    let mut result = HashSet::new();
    let mut stack = vec![start];
    while let Some(owner) = stack.pop() {
        if let Some(callees) = calls.get(&owner) {
            let mut ordered = callees.iter().copied().collect::<Vec<_>>();
            ordered.sort_unstable();
            for callee in ordered {
                if result.insert(callee) {
                    stack.push(callee);
                }
            }
        }
    }
    result
}

fn rewrite_names(
    target: &mut Ast,
    source: &Ast,
    res: &storm_lua_analysis::resolver::Resolution,
    names: &HashMap<BindingId, String>,
    node: NodeId,
) -> NodeId {
    if matches!(source.node(node), Node::Name(_)) {
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            if let Some(name) = names.get(&bid) {
                let copy = target.name(name);
                target
                    .nodes
                    .derive_from(copy, &source.nodes, node, "callback-function-slot-use");
                target.nodes.copy_name_from(
                    copy,
                    storm_lua_syntax::NameSite::Reference,
                    &source.nodes,
                    node,
                    storm_lua_syntax::NameSite::Reference,
                );
                return copy;
            }
        }
    }
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        rewrite_names(target, source, res, names, child)
    });
    let copy = target.push(mapped);
    target
        .nodes
        .derive_from(copy, &source.nodes, node, "callback-function-relocation");
    copy
}

fn measured(ast: &Ast, root: NodeId) -> usize {
    measure_renamed_size(ast, root)
}

#[allow(clippy::too_many_arguments)]
fn candidate_for_count(
    source: &Ast,
    root: NodeId,
    res: &storm_lua_analysis::resolver::Resolution,
    tick_bid: BindingId,
    draw_bid: BindingId,
    tick_only: &[Definition],
    draw_only: &[Definition],
    count: usize,
) -> (Ast, NodeId) {
    let selected_tick = &tick_only[..count];
    let selected_draw = &draw_only[..count];
    let selected = selected_tick
        .iter()
        .chain(selected_draw)
        .map(|entry| entry.bid)
        .collect::<HashSet<_>>();
    let mut names = HashMap::new();
    for index in 0..count {
        let name = format!("__stormmin_phase_function_{index}");
        names.insert(selected_tick[index].bid, name.clone());
        names.insert(selected_draw[index].bid, name);
    }

    let mut target = storm_lua_syntax::ast_utils::inherit_ast(source);
    target.nodes = source.nodes.clone();
    let mut tick_entries = selected_tick.to_vec();
    let mut draw_entries = selected_draw.to_vec();
    tick_entries.sort_by_key(|entry| entry.order);
    draw_entries.sort_by_key(|entry| entry.order);
    let tick_definitions = tick_entries
        .iter()
        .map(|entry| rewrite_names(&mut target, source, res, &names, entry.statement))
        .collect::<Vec<_>>();
    let draw_definitions = draw_entries
        .iter()
        .map(|entry| rewrite_names(&mut target, source, res, &names, entry.statement))
        .collect::<Vec<_>>();

    let Node::Block(statements) = source.node(root) else {
        unreachable!("root was checked as block")
    };
    let mut output = Vec::with_capacity(statements.len());
    for statement in statements {
        let is_selected_definition = match source.node(*statement) {
            Node::Funcstat(target_name, _)
                if matches!(source.node(*target_name), Node::Name(_)) =>
            {
                res.node_bid
                    .get(*target_name as usize)
                    .copied()
                    .flatten()
                    .is_some_and(|bid| selected.contains(&bid))
            }
            _ => false,
        };
        if is_selected_definition {
            continue;
        }
        let transformed = rewrite_names(&mut target, source, res, &names, *statement);
        let callback_bid = match source.node(*statement) {
            Node::Funcstat(target_name, _)
                if matches!(source.node(*target_name), Node::Name(_)) =>
            {
                res.node_bid.get(*target_name as usize).copied().flatten()
            }
            _ => None,
        };
        if callback_bid == Some(tick_bid) || callback_bid == Some(draw_bid) {
            let definitions = if callback_bid == Some(tick_bid) {
                &tick_definitions
            } else {
                &draw_definitions
            };
            if let Node::Funcstat(_, function) = target.node(transformed).clone() {
                if let Node::Function(params, vararg, body) = target.node(function).clone() {
                    if let Node::Block(body_statements) = target.node(body).clone() {
                        let mut merged = definitions.clone();
                        merged.extend(body_statements);
                        target.nodes.rewrite(
                            body,
                            Node::Block(merged),
                            "callback-function-insertion",
                        );
                        target.nodes.rewrite(
                            function,
                            Node::Function(params, vararg, body),
                            "callback-function-insertion",
                        );
                    }
                }
            }
        }
        output.push(transformed);
    }
    let candidate_root = target.block(output);
    target.nodes.derive_from(
        candidate_root,
        &source.nodes,
        root,
        "callback-function-relocation",
    );
    (target, candidate_root)
}

pub fn slot_callback_exclusive_functions(ast: &mut Ast, root: NodeId) -> PassResult {
    let Node::Block(statements) = ast.node(root) else {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    };
    let source = clone_ast(ast);
    let res = resolve(&source, root);
    let mut definitions = HashMap::new();
    let mut tick_bid = None;
    let mut draw_bid = None;
    for (order, statement) in statements.iter().enumerate() {
        let Node::Funcstat(target, _) = source.node(*statement) else {
            continue;
        };
        if !matches!(source.node(*target), Node::Name(_)) {
            continue;
        }
        let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() else {
            continue;
        };
        let binding = res.binding(bid);
        let name = source.strings.get(binding.name);
        if name == "onTick" {
            tick_bid = Some(bid);
        } else if name == "onDraw" {
            draw_bid = Some(bid);
        } else if !binding.fixed {
            definitions.insert(
                bid,
                Definition {
                    bid,
                    statement: *statement,
                    frequency: binding.freq,
                    order,
                },
            );
        }
    }
    let (Some(tick_bid), Some(draw_bid)) = (tick_bid, draw_bid) else {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    };
    if definitions.is_empty() {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }

    let definition_ids = definitions.keys().copied().collect::<HashSet<_>>();
    let mut calls = HashMap::new();
    let mut non_call_references = HashSet::new();
    for statement in statements {
        let owner = match source.node(*statement) {
            Node::Funcstat(target, function) if matches!(source.node(*target), Node::Name(_)) => {
                let owner = res
                    .node_bid
                    .get(*target as usize)
                    .copied()
                    .flatten()
                    .unwrap_or(0);
                if let Node::Function(_, _, body) = source.node(*function) {
                    scan_calls(
                        &source,
                        &res,
                        &definition_ids,
                        *body,
                        owner,
                        false,
                        &mut calls,
                        &mut non_call_references,
                    );
                    continue;
                }
                owner
            }
            _ => 0,
        };
        scan_calls(
            &source,
            &res,
            &definition_ids,
            *statement,
            owner,
            false,
            &mut calls,
            &mut non_call_references,
        );
    }
    let tick_reachable = reachable(tick_bid, &calls);
    let draw_reachable = reachable(draw_bid, &calls);
    let mut tick_only = Vec::new();
    let mut draw_only = Vec::new();
    for entry in definitions.values() {
        if non_call_references.contains(&entry.bid) {
            continue;
        }
        let tick = tick_reachable.contains(&entry.bid);
        let draw = draw_reachable.contains(&entry.bid);
        if tick && !draw {
            tick_only.push(entry.clone());
        } else if draw && !tick {
            draw_only.push(entry.clone());
        }
    }
    let rank = |items: &mut Vec<Definition>| {
        items.sort_by(|left, right| {
            right
                .frequency
                .cmp(&left.frequency)
                .then_with(|| left.order.cmp(&right.order))
        });
    };
    rank(&mut tick_only);
    rank(&mut draw_only);
    let pair_count = tick_only.len().min(draw_only.len());
    if pair_count == 0 {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }

    let baseline = measured(&source, root);
    let mut best: Option<(Ast, NodeId, usize, usize)> = None;
    for count in 1..=pair_count {
        let (candidate, candidate_root) = candidate_for_count(
            &source, root, &res, tick_bid, draw_bid, &tick_only, &draw_only, count,
        );
        let size = measured(&candidate, candidate_root);
        if size < baseline && best.as_ref().is_none_or(|entry| size < entry.2) {
            best = Some((candidate, candidate_root, size, count));
        }
    }
    if let Some((candidate, candidate_root, size, count)) = best {
        *ast = candidate;
        PassResult {
            root: candidate_root,
            saved: Some(baseline.saturating_sub(size) as u64),
            details: Some(vec![
                format!("slotted={count}"),
                format!("considered={pair_count}"),
            ]),
        }
    } else {
        PassResult {
            root,
            saved: Some(0),
            details: Some(vec![format!("considered={pair_count}")]),
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
    fn slots_tick_and_draw_exclusive_helpers() {
        let globals = (0..55)
            .map(|index| format!("s{index}=0"))
            .collect::<Vec<_>>()
            .join(" ");
        let source = format!(
            "{globals} function tickHelper(x)return x+1 end function drawHelper(x)return x+2 end function onTick()output.setNumber(1,tickHelper(input.getNumber(1)))end function onDraw()screen.drawText(0,0,drawHelper(input.getNumber(2)))end"
        );
        let (mut ast, root) = parse_source(&source).expect("parse");
        let result = slot_callback_exclusive_functions(&mut ast, root);
        let output = Printer::new(&ast, false).output(result.root);
        assert!(output.contains("__stormmin_phase_function_0"), "{output}");
        assert!(!output.contains("function tickHelper"), "{output}");
    }

    #[test]
    fn rejects_helpers_with_non_call_references() {
        let source = r#"
function tickHelper() return 1 end
function drawHelper() return 2 end
x=tickHelper
function onTick() output.setNumber(1,tickHelper()) end
function onDraw() screen.drawText(1,1,drawHelper()) end
"#;
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = slot_callback_exclusive_functions(&mut ast, root);
        let output = Printer::new(&ast, false).output(result.root);
        assert!(output.starts_with("function tickHelper"), "{output}");
    }
}
