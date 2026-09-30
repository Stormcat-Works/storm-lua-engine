//! Equivalent trailing argument omission (`passes/omit-arguments.ts`).

use std::collections::HashMap;

use crate::pass::PassResult;
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};

#[derive(Clone)]
struct FunctionInfo {
    parameter_bids: Vec<BindingId>,
    body: NodeId,
    variadic: bool,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn unconditional_direct_call_args(
    ast: &Ast,
    res: &Resolution,
    body: NodeId,
    parameter_bids: &[BindingId],
) -> u64 {
    if parameter_bids.len() > u64::BITS as usize {
        return 0;
    }
    let Node::Block(statements) = ast.node(body) else {
        return 0;
    };
    // Keep this proof intentionally narrow and cheap. A single top-level call
    // statement cannot be folded away by substituting other parameters, so a
    // direct parameter argument there is guaranteed to remain observable in
    // the normalized body.
    let [statement] = statements.as_slice() else {
        return 0;
    };
    let Node::Callstat(call) = ast.node(*statement) else {
        return 0;
    };
    let Node::Call(_, arguments, _) = ast.node(*call) else {
        return 0;
    };
    let mut used = 0u64;
    for argument in arguments {
        if !matches!(ast.node(*argument), Node::Name(_)) {
            continue;
        }
        let Some(argument_bid) = res.node_bid.get(*argument as usize).copied().flatten() else {
            continue;
        };
        if let Some(index) = parameter_bids.iter().position(|&bid| bid == argument_bid) {
            used |= 1u64 << index;
        }
    }
    used
}

fn functions(ast: &Ast, res: &Resolution) -> HashMap<BindingId, FunctionInfo> {
    res.bindings
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(bid, binding)| {
            let function = binding.function_node?;
            binding.decl_node?;
            let Node::Function(_, variadic, body) = ast.node(function) else {
                return None;
            };
            let parameter_bids = res
                .node_bids
                .get(function as usize)
                .cloned()
                .unwrap_or_default();
            Some((
                bid as BindingId,
                FunctionInfo {
                    parameter_bids,
                    body: *body,
                    variadic: *variadic,
                },
            ))
        })
        .collect()
}

fn literal(ast: &Ast, node: NodeId) -> bool {
    matches!(
        ast.node(node),
        Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
    )
}

// Normalization only observes this function body. Importing its reachable
// subtree avoids repeatedly cloning and resolving thousands of unrelated draw
// calls. Substitution still uses ORIGINAL BindingIds, before rebasing NodeIds.
fn copy_with_bindings(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    replacements: &HashMap<BindingId, Option<NodeId>>,
) -> NodeId {
    if matches!(source.node(node), Node::Name(_)) {
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            if let Some(value) = replacements.get(&bid) {
                let literal = match value {
                    Some(value) => source.node(*value).clone(),
                    None => Node::Nil,
                };
                return target.push(literal);
            }
        }
    }
    let original = source.node(node);
    let (copied, _) = storm_lua_syntax::ast_utils::map_children(original, &mut |child| {
        copy_with_bindings(target, source, res, child, replacements)
    });
    target.push(copied)
}

fn normalized_body(
    source: &Ast,
    res: &Resolution,
    body: NodeId,
    replacements: &HashMap<BindingId, Option<NodeId>>,
) -> String {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    let body = copy_with_bindings(&mut ast, source, res, body, replacements);
    for _ in 0..3 {
        super::literal_folding::constant_fold(&mut ast, body, false);
    }
    storm_lua_syntax::print::Printer::new(&ast, false).output(body)
}

fn can_omit(
    source: &Ast,
    res: &Resolution,
    info: &FunctionInfo,
    arguments: &[NodeId],
    keep: usize,
    missing_cache: &mut HashMap<(NodeId, usize, usize), String>,
) -> bool {
    if arguments.len() > info.parameter_bids.len() {
        return false;
    }
    let mut original = HashMap::<BindingId, Option<NodeId>>::new();
    let mut shortened = HashMap::<BindingId, Option<NodeId>>::new();
    for (index, argument) in arguments.iter().copied().enumerate().skip(keep) {
        if !literal(source, argument) {
            return false;
        }
        original.insert(info.parameter_bids[index], Some(argument));
        shortened.insert(info.parameter_bids[index], None);
    }
    let missing = missing_cache
        .entry((info.body, arguments.len(), keep))
        .or_insert_with(|| normalized_body(source, res, info.body, &shortened));
    normalized_body(source, res, info.body, &original) == *missing
}

pub fn omit_equivalent_trailing_arguments(ast: &mut Ast, root: NodeId) -> PassResult {
    let source = clone_ast(ast);
    let res = resolve(&source, root);
    let functions = functions(&source, &res);
    let mut omitted = 0usize;
    // Nil-substitution depends on the function and omitted suffix, not on the
    // particular literal values at hundreds of other call sites.
    let mut missing_cache = HashMap::new();
    let mut direct_call_args_cache = HashMap::<BindingId, u64>::new();
    let mut nodes = Vec::new();
    storm_lua_analysis::effects::walk(&source, root, &mut nodes);

    for node in nodes.into_iter().rev() {
        let Node::Call(function, arguments, method) = source.node(node).clone() else {
            continue;
        };
        if method.is_some()
            || arguments.is_empty()
            || !matches!(source.node(function), Node::Name(_))
        {
            continue;
        }
        let Some(bid) = res.node_bid.get(function as usize).copied().flatten() else {
            continue;
        };
        let Some(info) = functions.get(&bid) else {
            continue;
        };
        if info.variadic || arguments.len() > info.parameter_bids.len() {
            continue;
        }
        // can_omit requires the entire omitted suffix to consist of literals.
        // Start after the last non-literal instead of repeatedly discovering the
        // same impossible prefixes inside normalized-body comparisons.
        let literal_suffix_start = arguments
            .iter()
            .rposition(|argument| !literal(&source, *argument))
            .map_or(0, |index| index + 1);
        if literal_suffix_start == arguments.len() {
            continue;
        }

        // If a literal argument is passed directly to an unconditional
        // top-level call in the callee, replacing a non-nil value with the
        // omitted-argument value `nil` must change that retained call. Compute
        // this proof lazily only for functions that actually have an omittable
        // literal suffix, then cache it across their call sites.
        let direct_call_args = *direct_call_args_cache.entry(bid).or_insert_with(|| {
            unconditional_direct_call_args(&source, &res, info.body, &info.parameter_bids)
        });
        let first_possible_keep = arguments
            .iter()
            .enumerate()
            .skip(literal_suffix_start)
            .filter(|(index, argument)| {
                *index < u64::BITS as usize
                    && direct_call_args & (1u64 << *index) != 0
                    && !matches!(source.node(**argument), Node::Nil)
            })
            .map(|(index, _)| index + 1)
            .max()
            .unwrap_or(literal_suffix_start);
        for keep in first_possible_keep..arguments.len() {
            if can_omit(&source, &res, info, &arguments, keep, &mut missing_cache) {
                omitted += arguments.len() - keep;
                ast.nodes.rewrite(
                    node,
                    Node::Call(function, arguments[..keep].to_vec(), method.clone()),
                    "trailing-argument-omission",
                );
                break;
            }
        }
    }

    PassResult {
        root,
        saved: None,
        details: Some(vec![format!("omitted={omitted}")]),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use storm_lua_syntax::parser::parse_source;
    use storm_lua_syntax::print::Printer;

    #[test]
    fn compact_normalization_matches_full_arena_with_shadowing_and_captures() {
        let source = format!(
            "local outer=5 local function f(a,b)do local b=a if b then output.setNumber(1,b)end end local function g()return b,outer end if a then return g()end end function onDraw(){}end",
            "screen.drawRectF(1,2,3,4)".repeat(2000)
        );
        let (ast, root) = parse_source(&source).unwrap();
        let res = resolve(&ast, root);
        let functions = functions(&ast, &res);
        let info = functions
            .iter()
            .find(|(bid, _)| ast.strings.get(res.binding(**bid).name) == "f")
            .unwrap()
            .1;
        for replaced in [
            vec![info.parameter_bids[0]],
            vec![info.parameter_bids[1]],
            info.parameter_bids.clone(),
        ] {
            let replacements = replaced
                .into_iter()
                .map(|bid| (bid, None))
                .collect::<HashMap<_, _>>();
            let mut compact = storm_lua_syntax::ast_utils::inherit_ast(&ast);
            copy_with_bindings(&mut compact, &ast, &res, info.body, &replacements);
            assert!(compact.nodes.len() < 60);
            assert!(ast.nodes.len() > 10000);
            // Reference the old whole-arena normalization independently.
            let mut full = ast.clone();
            storm_lua_syntax::ast_utils::walk(&ast, info.body, &mut |node| {
                if matches!(ast.node(node), Node::Name(_))
                    && res.node_bid[node as usize]
                        .is_some_and(|bid| replacements.contains_key(&bid))
                {
                    full.nodes[node as usize] = Node::Nil;
                }
            });
            for _ in 0..3 {
                super::super::literal_folding::constant_fold(&mut full, info.body, false);
            }
            assert_eq!(
                normalized_body(&ast, &res, info.body, &replacements),
                Printer::new(&full, false).output(info.body)
            );
        }
    }

    #[test]
    fn omits_equivalent_default_literal() {
        let (mut ast, root) = parse_source(
            "x=0 local function f(a,b)if a then x=b end end function onTick()f(true,1)f(false,0)output.setNumber(1,x)end",
        )
        .unwrap();
        omit_equivalent_trailing_arguments(&mut ast, root);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("f(1>0,1)"), "{out}");
        assert!(out.contains("f()"), "{out}");
    }

    #[test]
    fn direct_unconditional_call_argument_blocks_non_nil_omission() {
        let (mut ast, root) = parse_source(
            "local function f(a,b)output.setNumber(a,b)end function onTick()f(1,2)f(1,nil)end",
        )
        .unwrap();
        omit_equivalent_trailing_arguments(&mut ast, root);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("f(1,2)"), "{out}");
        assert!(out.contains("f(1)"), "{out}");
    }

    #[test]
    fn extra_actual_arguments_do_not_shift_or_change_output() {
        let args = std::iter::repeat_n("1", 70).collect::<Vec<_>>().join(",");
        let source =
            format!("local function f(a)output.setNumber(1,a)end function onTick()f({args})end");
        let (mut ast, root) = parse_source(&source).unwrap();
        let before = Printer::new(&ast, false).output(root);
        omit_equivalent_trailing_arguments(&mut ast, root);
        assert_eq!(Printer::new(&ast, false).output(root), before);
    }

    #[test]
    fn more_than_sixty_four_parameters_falls_back_without_shift_overflow() {
        let params = (0..65).map(|i| format!("p{i}")).collect::<Vec<_>>();
        let args = std::iter::repeat_n("1", 65).collect::<Vec<_>>().join(",");
        let source = format!(
            "local function f({})output.setNumber(1,p64)end function onTick()f({args})end",
            params.join(",")
        );
        let (mut ast, root) = parse_source(&source).unwrap();
        let before = Printer::new(&ast, false).output(root);
        omit_equivalent_trailing_arguments(&mut ast, root);
        assert_eq!(Printer::new(&ast, false).output(root), before);
    }
}
