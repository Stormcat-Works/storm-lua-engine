//! Destructive result globalization (`passes/destructive-results.ts`).
//!
//! A one-parameter helper that mutates and returns its parameter can instead
//! mutate a common non-fixed global when every call appears as an assignment to
//! that same global. Candidates are measured after scope renaming and the
//! smallest one is committed in each round.

use crate::pass::PassResult;
use crate::scope_rename::measure_renamed_size;
use storm_lua_analysis::resolver::{resolve, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};

#[derive(Clone)]
struct Candidate {
    function_bid: BindingId,
    function_node: NodeId,
    parameter_bid: BindingId,
    target_bid: BindingId,
    target_name: String,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn measured(ast: &Ast, root: NodeId) -> usize {
    measure_renamed_size(ast, root)
}

fn has_nested_function(ast: &Ast, body: NodeId) -> bool {
    let Node::Block(statements) = ast.node(body) else {
        return true;
    };
    statements.iter().any(|statement| {
        let mut found = false;
        storm_lua_syntax::ast_utils::walk(ast, *statement, &mut |node| {
            if matches!(ast.node(node), Node::Function(..)) {
                found = true;
            }
        });
        found
    })
}

fn has_early_return(ast: &Ast, body: NodeId) -> bool {
    let Node::Block(statements) = ast.node(body) else {
        return true;
    };
    let Some((_, prefix)) = statements.split_last() else {
        return true;
    };
    prefix.iter().any(|statement| {
        let mut found = false;
        storm_lua_syntax::ast_utils::walk(ast, *statement, &mut |node| {
            if matches!(ast.node(node), Node::Return(_)) {
                found = true;
            }
        });
        found
    })
}

struct CallInspection {
    target_bid: Option<BindingId>,
    calls: usize,
    invalid: bool,
}

fn inspect_calls(
    ast: &Ast,
    res: &Resolution,
    node: NodeId,
    parent: Option<NodeId>,
    function_bid: BindingId,
    inspection: &mut CallInspection,
) {
    if inspection.invalid {
        return;
    }
    if let Node::Call(function, args, _) = ast.node(node) {
        if matches!(ast.node(*function), Node::Name(_))
            && res.node_bid.get(*function as usize).copied().flatten() == Some(function_bid)
        {
            let Some(parent) = parent else {
                inspection.invalid = true;
                return;
            };
            let Node::Assign(targets, expressions) = ast.node(parent) else {
                inspection.invalid = true;
                return;
            };
            if expressions.len() != 1
                || expressions[0] != node
                || targets.len() != 1
                || !matches!(ast.node(targets[0]), Node::Name(_))
                || args.len() != 1
            {
                inspection.invalid = true;
                return;
            }
            let Some(target_bid) = res.node_bid.get(targets[0] as usize).copied().flatten() else {
                inspection.invalid = true;
                return;
            };
            if inspection
                .target_bid
                .is_some_and(|previous| previous != target_bid)
            {
                inspection.invalid = true;
                return;
            }
            inspection.target_bid = Some(target_bid);
            inspection.calls += 1;
        }
    }
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        inspect_calls(ast, res, child, Some(node), function_bid, inspection)
    });
}

fn contains_binding(ast: &Ast, res: &Resolution, node: NodeId, bid: BindingId) -> bool {
    let mut found = false;
    storm_lua_syntax::ast_utils::walk(ast, node, &mut |id| {
        if !found
            && matches!(ast.node(id), Node::Name(_))
            && res.node_bid.get(id as usize).copied().flatten() == Some(bid)
        {
            found = true;
        }
    });
    found
}

fn find_candidates(ast: &Ast, root: NodeId, res: &Resolution) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for (function_bid, binding) in res.bindings.iter().enumerate().skip(1) {
        let function_bid = function_bid as BindingId;
        let Some(function_node) = binding.function_node else {
            continue;
        };
        let Node::Function(parameters, variadic, body) = ast.node(function_node) else {
            continue;
        };
        let Node::Block(statements) = ast.node(*body) else {
            continue;
        };
        let parameter_bids = res
            .node_bids
            .get(function_node as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if *variadic
            || parameters.len() != 1
            || parameter_bids.len() != 1
            || statements.len() < 2
            || has_nested_function(ast, *body)
            || has_early_return(ast, *body)
        {
            continue;
        }
        let parameter_bid = parameter_bids[0];
        #[expect(
            clippy::expect_used,
            reason = "The preceding eligibility condition rejects empty statement bodies"
        )]
        let Node::Return(expressions) = ast.node(*statements.last().expect("non-empty")) else {
            continue;
        };
        if expressions.len() != 1
            || !matches!(ast.node(expressions[0]), Node::Name(_))
            || res.node_bid.get(expressions[0] as usize).copied().flatten() != Some(parameter_bid)
        {
            continue;
        }
        let mut inspection = CallInspection {
            target_bid: None,
            calls: 0,
            invalid: false,
        };
        inspect_calls(ast, res, root, None, function_bid, &mut inspection);
        let Some(target_bid) = inspection.target_bid else {
            continue;
        };
        if inspection.invalid || inspection.calls < 2 {
            continue;
        }
        let target = res.binding(target_bid);
        if !matches!(target.kind, BindingKind::Global)
            || target.fixed
            || contains_binding(ast, res, function_node, target_bid)
        {
            continue;
        }
        candidates.push(Candidate {
            function_bid,
            function_node,
            parameter_bid,
            target_bid,
            target_name: ast.strings.get(target.name).to_string(),
        });
    }
    candidates
}

fn rewrite_expression(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    parameter_bid: BindingId,
    target_name: &str,
) -> NodeId {
    if matches!(source.node(node), Node::Name(_))
        && res.node_bid.get(node as usize).copied().flatten() == Some(parameter_bid)
    {
        let result = target.name(target_name);
        let snapshot = source.nodes.capture_origin(node);
        target.nodes.finish_rename(result, snapshot);
        return result;
    }
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        rewrite_expression(target, source, res, child, parameter_bid, target_name)
    });
    let result = target.push(mapped);
    target
        .nodes
        .derive_from(result, &source.nodes, node, "destructive-result-copy");
    result
}

fn matching_call_assignment(
    ast: &Ast,
    res: &Resolution,
    statement: NodeId,
    candidate: &Candidate,
) -> Option<NodeId> {
    let Node::Assign(targets, expressions) = ast.node(statement) else {
        return None;
    };
    if targets.len() != 1
        || expressions.len() != 1
        || !matches!(ast.node(targets[0]), Node::Name(_))
        || res.node_bid.get(targets[0] as usize).copied().flatten() != Some(candidate.target_bid)
    {
        return None;
    }
    let Node::Call(function, args, _) = ast.node(expressions[0]) else {
        return None;
    };
    if args.len() != 1
        || !matches!(ast.node(*function), Node::Name(_))
        || res.node_bid.get(*function as usize).copied().flatten() != Some(candidate.function_bid)
    {
        return None;
    }
    Some(args[0])
}

fn transform_node(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    candidate: &Candidate,
) -> NodeId {
    if node == candidate.function_node {
        let Node::Function(_, _, body) = source.node(node) else {
            unreachable!()
        };
        let Node::Block(statements) = source.node(*body) else {
            unreachable!()
        };
        let rewritten = statements[..statements.len() - 1]
            .iter()
            .map(|statement| {
                rewrite_expression(
                    target,
                    source,
                    res,
                    *statement,
                    candidate.parameter_bid,
                    &candidate.target_name,
                )
            })
            .collect();
        target.nodes.rewrite(
            *body,
            Node::Block(rewritten),
            "destructive-result-return-removal",
        );
        target.nodes.rewrite(
            node,
            Node::Function(Vec::new(), false, *body),
            "destructive-result-parameter-removal",
        );
        return node;
    }
    if matches!(source.node(node), Node::Block(_)) {
        return transform_block(target, source, res, node, candidate);
    }
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        transform_node(target, source, res, child, candidate)
    });
    target
        .nodes
        .rewrite(node, mapped, "destructive-result-globalization");
    node
}

fn transform_block(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    block: NodeId,
    candidate: &Candidate,
) -> NodeId {
    let Node::Block(statements) = source.node(block).clone() else {
        unreachable!()
    };
    let function_name = source
        .strings
        .get(res.binding(candidate.function_bid).name)
        .to_string();
    let mut output = Vec::with_capacity(statements.len());
    for statement in statements {
        if let Some(argument) = matching_call_assignment(source, res, statement, candidate) {
            let target_name = target.name(&candidate.target_name);
            let argument = rewrite_expression(
                target,
                source,
                res,
                argument,
                candidate.parameter_bid,
                &candidate.target_name,
            );
            let Node::Assign(original_targets, expressions) = source.node(statement) else {
                unreachable!()
            };
            let original_call = expressions[0];
            let Node::Call(original_fn, _, _) = source.node(original_call) else {
                unreachable!()
            };
            target.nodes.derive_from(
                target_name,
                &source.nodes,
                original_targets[0],
                "destructive-result-initialization",
            );
            let assign = target.assign(vec![target_name], vec![argument]);
            target.nodes.derive_from(
                assign,
                &source.nodes,
                statement,
                "destructive-result-initialization",
            );
            output.push(assign);
            let function = target.name(&function_name);
            target.nodes.derive_from(
                function,
                &source.nodes,
                *original_fn,
                "destructive-result-callee",
            );
            let call = target.call(function, Vec::new(), None);
            target.nodes.derive_from(
                call,
                &source.nodes,
                original_call,
                "destructive-result-call",
            );
            let stmt = target.callstat(call);
            target
                .nodes
                .derive_from(stmt, &source.nodes, statement, "destructive-result-call");
            output.push(stmt);
        } else {
            output.push(transform_node(target, source, res, statement, candidate));
        }
    }
    target.nodes.rewrite(
        block,
        Node::Block(output),
        "destructive-result-globalization",
    );
    block
}

fn apply_candidate(source: &Ast, root: NodeId, res: &Resolution, candidate: &Candidate) -> Ast {
    let mut transformed = clone_ast(source);
    transform_block(&mut transformed, source, res, root, candidate);
    transformed
}

pub fn globalize_destructive_results_with_options(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    max_rounds: usize,
) -> PassResult {
    if !aggressive {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let original = measured(ast, root);
    let mut globalized = 0usize;
    let mut considered = 0usize;
    for _ in 0..max_rounds {
        let source = clone_ast(ast);
        let res = resolve(&source, root);
        let candidates = find_candidates(&source, root, &res);
        let baseline = measured(&source, root);
        let mut best: Option<(Ast, usize)> = None;
        for candidate in candidates.into_iter().take(24) {
            considered += 1;
            let transformed = apply_candidate(&source, root, &res, &candidate);
            let size = measured(&transformed, root);
            if size < baseline && best.as_ref().is_none_or(|entry| size < entry.1) {
                best = Some((transformed, size));
            }
        }
        let Some((transformed, _)) = best else {
            break;
        };
        *ast = transformed;
        globalized += 1;
    }
    PassResult {
        root,
        saved: Some(original.saturating_sub(measured(ast, root)) as u64),
        details: if globalized == 0 && considered == 0 {
            None
        } else {
            Some(vec![
                format!("globalized={globalized}"),
                format!("considered={considered}"),
            ])
        },
    }
}

pub fn globalize_destructive_results(ast: &mut Ast, root: NodeId) -> PassResult {
    globalize_destructive_results_with_options(ast, root, true, 8)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = globalize_destructive_results(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn globalizes_common_destructive_call_target() {
        let source = "value=0 function normalize(x)x=x*2 x=x+1 return x end function onTick()value=normalize(input.getNumber(1))output.setNumber(1,value)value=normalize(input.getNumber(2))output.setNumber(2,value)end";
        let out = output(source);
        assert!(
            out.contains("function normalize()value=value*2 value=value+1 end"),
            "{out}"
        );
        assert!(out.contains("value=input.getNumber(1)normalize()"), "{out}");
    }

    #[test]
    fn rejects_different_call_targets() {
        let source = "a=0 b=0 function f(x)x=x+1 return x end a=f(1)b=f(2)";
        assert!(output(source).contains("function f(x)"));
    }

    #[test]
    fn rejects_non_assignment_calls() {
        let source = "a=0 function f(x)x=x+1 return x end a=f(1)output.setNumber(1,f(2))";
        assert!(output(source).contains("function f(x)"));
    }
}
