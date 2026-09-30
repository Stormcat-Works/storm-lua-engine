//! Ordered repeated screen-setColor call factoring (`scope-and-api.ts`).
//!
//! Literal `screen.setColor(...)` call statements are grouped in first-seen
//! order. Profitable groups are replaced by zero-argument wrappers, and the
//! wrapper subset is selected by exhaustive subset search (<=12 groups) or
//! deterministic greedy search (>12), measured after scope renaming.

use std::collections::HashMap;

use crate::pass::PassResult;
use crate::scope_rename::scope_rename_fast;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::resolve;
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId};
use storm_lua_syntax::print::Printer;
use storm_lua_syntax::size::{measure_expr, measure_size};

#[derive(Clone)]
struct Group {
    key: String,
    call: NodeId,
    sites: Vec<NodeId>,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn literal(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
    )
}

fn collect_groups(ast: &Ast, root: NodeId) -> Vec<Group> {
    let resolution = resolve(ast, root);
    let analyzer = EffectAnalyzer::new(ast, &resolution, root, true);
    let mut groups = Vec::<Group>::new();
    let mut by_key = HashMap::<String, usize>::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |id| {
        let Node::Callstat(expression) = ast.node(id) else {
            return;
        };
        let Node::Call(function, arguments, _) = ast.node(*expression) else {
            return;
        };
        if analyzer.resolve_builtin_reference(*function).as_deref() != Some("screen.setColor")
            || !arguments.iter().all(|argument| literal(ast, *argument))
        {
            return;
        }
        let key = Printer::new(ast, false).expr_public(*expression);
        if let Some(index) = by_key.get(&key).copied() {
            groups[index].call = *expression; // TS nodes.set(): keep latest equivalent call.
            groups[index].sites.push(id);
        } else {
            let index = groups.len();
            by_key.insert(key.clone(), index);
            groups.push(Group {
                key,
                call: *expression,
                sites: vec![id],
            });
        }
    });
    groups
}

fn eligibility_cost(ast: &Ast, group: &Group) -> bool {
    let count = group.sites.len();
    if count < 3 {
        return false;
    }
    let Node::Call(_, arguments, method) = ast.node(group.call).clone() else {
        return false;
    };
    let mut scratch = clone_ast(ast);
    let a_symbol = scratch.strings.intern("a");
    let b_symbol = scratch.strings.intern("b");
    let a = scratch.push(Node::Name(a_symbol));
    let direct = scratch.push(Node::Call(a, arguments, method));
    let direct_length = measure_expr(&scratch, direct);

    let b_call_name = scratch.push(Node::Name(b_symbol));
    let wrapper_call = scratch.push(Node::Call(b_call_name, Vec::new(), None));
    let wrapper_call_length = measure_expr(&scratch, wrapper_call);

    let callstat = scratch.push(Node::Callstat(direct));
    let body = scratch.push(Node::Block(vec![callstat]));
    let function = scratch.push(Node::Function(Vec::new(), false, body));
    let target = scratch.push(Node::Name(b_symbol));
    let definition = scratch.push(Node::Funcstat(target, function));
    let definition_block = scratch.push(Node::Block(vec![definition]));
    let definition_length = measure_size(&scratch, definition_block);

    count * direct_length.saturating_sub(wrapper_call_length) > definition_length
}

fn make_candidate(source: &Ast, root: NodeId, groups: &[Group], chosen: &[usize]) -> Ast {
    if chosen.is_empty() {
        return clone_ast(source);
    }
    let mut candidate = clone_ast(source);
    let mut aliases = Vec::<SymbolId>::with_capacity(chosen.len());
    for index in 0..chosen.len() {
        aliases.push(candidate.strings.intern(&format!("__sc{index}")));
    }

    for (alias_index, group_index) in chosen.iter().copied().enumerate() {
        let symbol = aliases[alias_index];
        for site in &groups[group_index].sites {
            let function = candidate.push(Node::Name(symbol));
            let call = candidate.push(Node::Call(function, Vec::new(), None));
            // This invocation still belongs to its own original call site.
            let Node::Callstat(original) = source.node(*site) else {
                unreachable!()
            };
            candidate.nodes.derive_from(
                function,
                &source.nodes,
                *original,
                "screen-call-helper-reference",
            );
            candidate.nodes.derive_from(
                call,
                &source.nodes,
                *original,
                "screen-call-helper-invocation",
            );
            candidate
                .nodes
                .rewrite(*site, Node::Callstat(call), "screen-call-factoring");
        }
    }

    let mut definitions = Vec::<NodeId>::with_capacity(chosen.len());
    for (alias_index, group_index) in chosen.iter().copied().enumerate() {
        let symbol = aliases[alias_index];
        // The source call node is immutable and structurally equivalent for every
        // occurrence in the group, so sharing it into the wrapper body is safe.
        let callstat = candidate.push(Node::Callstat(groups[group_index].call));
        let group = &groups[group_index];
        let original = group.sites[group.sites.len() - 1];
        candidate
            .nodes
            .derive_from(callstat, &source.nodes, original, "screen-call-shared-body");
        for &site in &group.sites {
            candidate
                .nodes
                .relate_from(callstat, &source.nodes, site, "screen-call-shared-body");
        }
        let body = candidate.push(Node::Block(vec![callstat]));
        let function = candidate.push(Node::Function(Vec::new(), false, body));
        let target = candidate.push(Node::Name(symbol));
        let definition = candidate.push(Node::Funcstat(target, function));
        for node in [body, function, target, definition] {
            candidate
                .nodes
                .mark_synthetic(node, "screen-call-helper-definition");
        }
        definitions.push(definition);
    }

    if let Node::Block(statements) = candidate.node(root).clone() {
        definitions.extend(statements);
        candidate.nodes.rewrite(
            root,
            Node::Block(definitions),
            "screen-call-helper-insertion",
        );
    }
    candidate
}

fn measured(ast: &Ast, root: NodeId) -> usize {
    let renamed = scope_rename_fast(ast, root);
    measure_size(&renamed.ast, renamed.root)
}

pub fn factor_repeated_screen_calls(ast: &mut Ast, root: NodeId) -> PassResult {
    let source = clone_ast(ast);
    let groups = collect_groups(&source, root);
    let eligible = groups
        .iter()
        .enumerate()
        .filter_map(|(index, group)| eligibility_cost(&source, group).then_some(index))
        .collect::<Vec<_>>();
    if eligible.is_empty() {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }

    let baseline = measured(&source, root);
    let mut chosen = Vec::<usize>::new();
    let mut best_ast = clone_ast(&source);
    let mut best_length = baseline;

    if eligible.len() <= 12 {
        for mask in 1usize..(1usize << eligible.len()) {
            let subset = eligible
                .iter()
                .enumerate()
                .filter_map(|(bit, group)| ((mask & (1usize << bit)) != 0).then_some(*group))
                .collect::<Vec<_>>();
            let trial = make_candidate(&source, root, &groups, &subset);
            let length = measured(&trial, root);
            if length < best_length {
                chosen = subset;
                best_ast = trial;
                best_length = length;
            }
        }
    } else {
        loop {
            let mut next: Option<(Vec<usize>, Ast, usize)> = None;
            for group in &eligible {
                if chosen.contains(group) {
                    continue;
                }
                let mut subset = chosen.clone();
                subset.push(*group);
                let trial = make_candidate(&source, root, &groups, &subset);
                let length = measured(&trial, root);
                if length < best_length
                    && next
                        .as_ref()
                        .is_none_or(|(_, _, next_length)| length < *next_length)
                {
                    next = Some((subset, trial, length));
                }
            }
            let Some((subset, trial, length)) = next else {
                break;
            };
            chosen = subset;
            best_ast = trial;
            best_length = length;
        }
    }

    *ast = best_ast;
    PassResult {
        root,
        saved: Some(baseline.saturating_sub(best_length) as u64),
        details: if chosen.is_empty() {
            None
        } else {
            Some(
                chosen
                    .iter()
                    .map(|group| {
                        format!(
                            "call={};uses={}",
                            groups[*group].key,
                            groups[*group].sites.len()
                        )
                    })
                    .collect(),
            )
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::{parse_source, Printer};

    #[test]
    fn factors_profitable_repeated_color_call() {
        let source = "function onDraw()screen.setColor(123,234,56,78)screen.drawText(0,0,'a')screen.setColor(123,234,56,78)screen.drawText(0,6,'b')screen.setColor(123,234,56,78)screen.drawText(0,12,'c')screen.setColor(123,234,56,78)screen.drawText(0,18,'d')screen.setColor(123,234,56,78)end";
        let (mut ast, root) = parse_source(source).unwrap();
        let result = factor_repeated_screen_calls(&mut ast, root);
        let output = Printer::new(&ast, false).output(result.root);
        assert!(output.contains("__sc0"), "{output}");
    }
}
