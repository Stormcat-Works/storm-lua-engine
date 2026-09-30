//! Function-specialization and inlining passes (`passes/inline-functions.ts`).

use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId};
use storm_lua_syntax::numeric::{num_val, short_num};
use storm_lua_syntax::size::{measure_expr, measure_size, measure_stmt};

#[derive(Clone)]
struct FunctionInfo {
    bid: BindingId,
    function: NodeId,
    declaration: NodeId,
    parameters: Vec<SymbolId>,
    parameter_bids: Vec<BindingId>,
    body: NodeId,
    variadic: bool,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn function_infos(ast: &Ast, res: &Resolution) -> Vec<FunctionInfo> {
    let mut output = Vec::new();
    for (bid, binding) in res.bindings.iter().enumerate().skip(1) {
        let (Some(function), Some(declaration)) = (binding.function_node, binding.decl_node) else {
            continue;
        };
        let Node::Function(parameters, variadic, body) = ast.node(function) else {
            continue;
        };
        output.push(FunctionInfo {
            bid: bid as BindingId,
            function,
            declaration,
            parameters: parameters.clone(),
            parameter_bids: res
                .node_bids
                .get(function as usize)
                .cloned()
                .unwrap_or_default(),
            body: *body,
            variadic: *variadic,
        });
    }
    output
}

fn info_map(infos: &[FunctionInfo]) -> HashMap<BindingId, FunctionInfo> {
    infos.iter().cloned().map(|info| (info.bid, info)).collect()
}

fn direct_function_bid(ast: &Ast, res: &Resolution, node: NodeId) -> Option<BindingId> {
    let Node::Call(function, _, _) = ast.node(node) else {
        return None;
    };
    if !matches!(ast.node(*function), Node::Name(_)) {
        return None;
    }
    res.node_bid.get(*function as usize).copied().flatten()
}

fn walk_nodes(ast: &Ast, root: NodeId) -> Vec<NodeId> {
    let mut output = Vec::new();
    storm_lua_analysis::effects::walk(ast, root, &mut output);
    output
}

fn function_reference_sets(
    ast: &Ast,
    root: NodeId,
    res: &Resolution,
    functions: &HashMap<BindingId, FunctionInfo>,
) -> (HashMap<BindingId, usize>, HashSet<BindingId>) {
    let mut call_counts = HashMap::<BindingId, usize>::new();
    let mut allowed_name_nodes = HashSet::<NodeId>::new();

    for node in walk_nodes(ast, root) {
        match ast.node(node) {
            Node::Call(function, _, _) if matches!(ast.node(*function), Node::Name(_)) => {
                if let Some(bid) = res.node_bid.get(*function as usize).copied().flatten() {
                    if functions.contains_key(&bid) {
                        *call_counts.entry(bid).or_default() += 1;
                        allowed_name_nodes.insert(*function);
                    }
                }
            }
            Node::Funcstat(target, _) if matches!(ast.node(*target), Node::Name(_)) => {
                if let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() {
                    if functions.contains_key(&bid) {
                        allowed_name_nodes.insert(*target);
                    }
                }
            }
            _ => {}
        }
    }

    let mut non_call_refs = HashSet::<BindingId>::new();
    for node in walk_nodes(ast, root) {
        if !matches!(ast.node(node), Node::Name(_)) || allowed_name_nodes.contains(&node) {
            continue;
        }
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            if functions.contains_key(&bid) {
                non_call_refs.insert(bid);
            }
        }
    }
    (call_counts, non_call_refs)
}

fn is_literal(ast: &Ast, node: NodeId) -> bool {
    matches!(
        ast.node(node),
        Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
    )
}

fn literal_nodes_same(ast: &Ast, a: Option<NodeId>, b: Option<NodeId>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => storm_lua_analysis::effects::ast_same(ast, a, b),
        (None, Some(b)) | (Some(b), None) => matches!(ast.node(b), Node::Nil),
    }
}

// Track source binding identities through expression copies and substitutions.
// A single re-resolution at the pass boundary rejects relocation across a
// private module scope or into a caller's same-spelling local. Declaration
// NodeIds, unlike numeric BindingIds, remain stable for unmoved free storage.
type ReferenceOrigins = HashMap<NodeId, BindingId>;
fn reference_origins(res: &Resolution) -> ReferenceOrigins {
    res.node_bid
        .iter()
        .enumerate()
        .filter_map(|(n, b)| b.map(|b| (n as NodeId, b)))
        .collect()
}
fn install_expression(
    ast: &mut Ast,
    node: NodeId,
    replacement: NodeId,
    origins: &mut ReferenceOrigins,
) {
    ast.nodes
        .relate_within(replacement, node, "function-inline-site");
    let source_origin = ast.nodes.capture_origin(replacement);
    ast.nodes[node as usize] = ast.node(replacement).clone();
    ast.nodes
        .finish_rewrite(node, source_origin, "function-inline-expression");
    if let Some(bid) = origins.get(&replacement).copied() {
        origins.insert(node, bid);
    } else {
        origins.remove(&node);
    }
}
fn expression_bindings_preserved(
    ast: &Ast,
    root: NodeId,
    old: &Resolution,
    origins: &ReferenceOrigins,
) -> bool {
    let new = resolve(ast, root);
    let mut valid = true;
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |node| {
        if !matches!(ast.node(node), Node::Name(_)) {
            return;
        }
        let (Some(&before), Some(after)) = (origins.get(&node), new.node_bid[node as usize]) else {
            valid = false;
            return;
        };
        let (before, after) = (old.binding(before), new.binding(after));
        valid &= before.name == after.name
            && before.kind == after.kind
            && before.decl_node == after.decl_node;
    });
    valid
}

fn clone_local_subtree(ast: &mut Ast, node: NodeId, origins: &mut ReferenceOrigins) -> NodeId {
    let source_origin = ast.nodes.capture_origin(node);
    let original = ast.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_local_subtree(ast, child, origins)
    });
    let copied = ast.push(mapped);
    ast.nodes
        .finish_rewrite(copied, source_origin, "function-inline-argument-copy");
    if let Some(bid) = origins.get(&node).copied() {
        origins.insert(copied, bid);
    }
    copied
}

fn clone_subtree_with_replacements(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    replacements: &HashMap<BindingId, NodeId>,
    origins: &mut ReferenceOrigins,
) -> NodeId {
    if matches!(source.node(node), Node::Name(_)) {
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            if let Some(replacement) = replacements.get(&bid) {
                let copied = clone_local_subtree(target, *replacement, origins);
                target.nodes.relate_from(
                    copied,
                    &source.nodes,
                    node,
                    "function-parameter-substitution",
                );
                return copied;
            }
        }
    }
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_subtree_with_replacements(target, source, res, child, replacements, origins)
    });
    let copied = target.push(mapped);
    target
        .nodes
        .derive_from(copied, &source.nodes, node, "function-body-copy");
    if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
        origins.insert(copied, bid);
    }
    copied
}

fn count_param_uses(ast: &Ast, res: &Resolution, expression: NodeId, bid: BindingId) -> usize {
    walk_nodes(ast, expression)
        .into_iter()
        .filter(|node| {
            matches!(ast.node(*node), Node::Name(_))
                && res.node_bid.get(*node as usize).copied().flatten() == Some(bid)
        })
        .count()
}

fn remove_statement_from_blocks(ast: &mut Ast, statement: NodeId) {
    ast.nodes
        .retain_block_statements(|id| id != statement, "inline-declaration-removal");
}

pub fn specialize_constant_arguments(
    ast: &mut Ast,
    root: NodeId,
    fold_numeric: bool,
) -> PassResult {
    let before = measure_size(ast, root);
    let source = clone_ast(ast);
    let res = resolve(&source, root);
    let infos = function_infos(&source, &res);
    let functions = info_map(&infos);
    // This pass mirrors the TS scanner exactly: only a direct call callee is
    // exempt from non-call references. A global Funcstat declaration target
    // is therefore ineligible, while a Localfunc declaration can specialize.
    let mut direct_call_names = HashSet::<NodeId>::new();
    for node in walk_nodes(&source, root) {
        if let Node::Call(function, _, _) = source.node(node) {
            if matches!(source.node(*function), Node::Name(_)) {
                if let Some(bid) = res.node_bid.get(*function as usize).copied().flatten() {
                    if functions.contains_key(&bid) {
                        direct_call_names.insert(*function);
                    }
                }
            }
        }
    }
    let mut non_call_refs = HashSet::<BindingId>::new();
    for node in walk_nodes(&source, root) {
        if !matches!(source.node(node), Node::Name(_)) || direct_call_names.contains(&node) {
            continue;
        }
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            if functions.contains_key(&bid) {
                non_call_refs.insert(bid);
            }
        }
    }

    let mut calls = HashMap::<BindingId, Vec<NodeId>>::new();
    for node in walk_nodes(&source, root) {
        if let Some(bid) = direct_function_bid(&source, &res, node) {
            if functions.contains_key(&bid) {
                calls.entry(bid).or_default().push(node);
            }
        }
    }

    #[derive(Clone)]
    struct Spec {
        remove: Vec<usize>,
        values: HashMap<BindingId, Option<NodeId>>,
    }
    let mut specs = HashMap::<BindingId, Spec>::new();
    for info in &infos {
        let Some(function_calls) = calls.get(&info.bid) else {
            continue;
        };
        if function_calls.is_empty() || non_call_refs.contains(&info.bid) {
            continue;
        }
        let mut remove = Vec::new();
        let mut values = HashMap::new();
        for index in 0..info.parameters.len() {
            // A parameter that is assigned anywhere in the body cannot be
            // specialized safely: later reads observe the assigned value,
            // not the call-site constant.  Keep both the parameter and its
            // argument for this conservative case.
            if info
                .parameter_bids
                .get(index)
                .and_then(|bid| res.binding_write_counts.get(*bid as usize))
                .is_some_and(|count| *count > 0)
            {
                continue;
            }
            let first = match source.node(function_calls[0]) {
                Node::Call(_, arguments, _) => arguments.get(index).copied(),
                _ => None,
            };
            if first.is_some_and(|node| !is_literal(&source, node)) {
                continue;
            }
            if first.is_none() {
                // Missing argument is Lua nil and therefore a literal candidate.
            }
            let same = function_calls.iter().all(|call| match source.node(*call) {
                Node::Call(_, arguments, _) => {
                    literal_nodes_same(&source, first, arguments.get(index).copied())
                }
                _ => false,
            });
            if same {
                remove.push(index);
                if let Some(param_bid) = info.parameter_bids.get(index) {
                    values.insert(*param_bid, first);
                }
            }
        }
        if !remove.is_empty() {
            specs.insert(info.bid, Spec { remove, values });
        }
    }

    if specs.is_empty() {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }

    let mut target = clone_ast(&source);
    for node in walk_nodes(&source, root) {
        let Some(bid) = direct_function_bid(&source, &res, node) else {
            continue;
        };
        let Some(spec) = specs.get(&bid) else {
            continue;
        };
        let Node::Call(function, arguments, method) = target.node(node).clone() else {
            continue;
        };
        let filtered = arguments
            .into_iter()
            .enumerate()
            .filter_map(|(index, argument)| (!spec.remove.contains(&index)).then_some(argument))
            .collect();
        target.nodes.rewrite(
            node,
            Node::Call(function, filtered, method),
            "constant-argument-specialization",
        );
    }

    for info in &infos {
        let Some(spec) = specs.get(&info.bid) else {
            continue;
        };
        // Replace parameter reads with their common literal value.
        for node in walk_nodes(&source, info.body) {
            if !matches!(source.node(node), Node::Name(_)) {
                continue;
            }
            let Some(bid) = res.node_bid.get(node as usize).copied().flatten() else {
                continue;
            };
            let Some(value) = spec.values.get(&bid) else {
                continue;
            };
            // Parameter bindings are mutable locals.  Replacing an lvalue
            // (`x = ...`) with a literal produces invalid Lua and also erases
            // the function's state transition.  Only substitute read sites.
            if res.node_write.get(node as usize).copied().unwrap_or(false) {
                continue;
            }
            target.nodes[node as usize] = match value {
                Some(value) => source.node(*value).clone(),
                None => Node::Nil,
            };
            if let Some(_value) = value {
                let inputs = info
                    .parameter_bids
                    .iter()
                    .position(|b| *b == bid)
                    .map(|index| {
                        calls[&info.bid]
                            .iter()
                            .filter_map(|&call| match source.node(call) {
                                Node::Call(_, args, _) => args.get(index).copied(),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                super::origins::derive(
                    &mut target,
                    node,
                    &source,
                    &inputs,
                    "specialized-argument-sites",
                );
            } else {
                target
                    .nodes
                    .derive_from(node, &source.nodes, node, "specialized-implicit-nil");
                for &call in &calls[&info.bid] {
                    target.nodes.relate_from(
                        node,
                        &source.nodes,
                        call,
                        "specialized-omitted-arguments",
                    );
                }
            }
            target
                .nodes
                .relate_from(node, &source.nodes, node, "specialized-parameter-read");
        }
        let filtered_parameters = info
            .parameters
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(index, parameter)| (!spec.remove.contains(&index)).then_some(parameter))
            .collect();
        target.nodes.rewrite(
            info.function,
            Node::Function(filtered_parameters, info.variadic, info.body),
            "constant-parameter-removal",
        );
        for (new_index, old_index) in (0..info.parameters.len())
            .filter(|i| !spec.remove.contains(i))
            .enumerate()
        {
            target.nodes.copy_name_from(
                info.function,
                storm_lua_syntax::NameSite::Parameter(new_index as u32),
                &source.nodes,
                info.function,
                storm_lua_syntax::NameSite::Parameter(old_index as u32),
            );
        }
    }

    // Numeric folding is separately gated by the caller. Exact mode passes
    // `false` here so specialization cannot bypass the exact-numeric contract.
    if fold_numeric {
        super::literal_folding::constant_fold(&mut target, root, true);
    }
    let after = measure_size(&target, root);
    if after < before {
        *ast = target;
        PassResult {
            root,
            saved: Some((before - after) as u64),
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

fn numeric_literal_value(ast: &Ast, node: NodeId) -> Option<f64> {
    match ast.node(node) {
        Node::Num(value) => Some(num_val(value)),
        Node::Un(operator, expression) if operator == "-" => match ast.node(*expression) {
            Node::Num(value) => Some(-num_val(value)),
            _ => None,
        },
        _ => None,
    }
}

fn fold_inlined_expression(ast: &mut Ast, expression: NodeId, aggressive: bool) -> NodeId {
    let return_node = ast.push(Node::Return(vec![expression]));
    let block = ast.push(Node::Block(vec![return_node]));
    let (folded, _) = super::literal_folding::fold_expressions(
        ast,
        block,
        aggressive,
        super::literal_folding::NumericTolerance::default(),
    );
    *ast = folded;
    match ast.node(return_node) {
        Node::Return(expressions) if expressions.len() == 1 => expressions[0],
        _ => expression,
    }
}

#[allow(clippy::too_many_arguments)]
fn rewrite_literal_calls(
    source: &Ast,
    target: &mut Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    functions: &HashMap<BindingId, FunctionInfo>,
    candidates: &HashMap<BindingId, NodeId>,
    node: NodeId,
    aggressive: bool,
    origins: &mut ReferenceOrigins,
    folded: &mut usize,
) {
    let mut children = Vec::new();
    storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| children.push(child));
    for child in children {
        rewrite_literal_calls(
            source, target, res, analyzer, functions, candidates, child, aggressive, origins,
            folded,
        );
    }

    let Node::Call(function, arguments, method) = target.node(node).clone() else {
        return;
    };
    if !arguments.is_empty()
        && arguments
            .iter()
            .all(|arg| numeric_literal_value(target, *arg).is_some())
    {
        if let Some(builtin) = analyzer.resolve_builtin_reference(match source.node(node) {
            Node::Call(function, _, _) => *function,
            _ => function,
        }) {
            let values = arguments
                .iter()
                .filter_map(|argument| numeric_literal_value(target, *argument))
                .collect::<Vec<_>>();
            let value = match builtin.as_str() {
                "math.abs" if values.len() == 1 => Some(values[0].abs()),
                "math.floor" if values.len() == 1 => Some(values[0].floor()),
                "math.min" if !values.is_empty() => {
                    Some(values.iter().copied().fold(f64::INFINITY, f64::min))
                }
                "math.max" if !values.is_empty() => {
                    Some(values.iter().copied().fold(f64::NEG_INFINITY, f64::max))
                }
                _ => None,
            };
            if let Some(value) = value.filter(|value| value.is_finite()) {
                let candidate = target.push(Node::Num(short_num(value).into()));
                if measure_expr(target, candidate) < measure_expr(target, node) {
                    target.nodes.rewrite(
                        node,
                        target.node(candidate).clone(),
                        "literal-call-folding",
                    );
                    super::origins::derive(target, node, source, &[node], "literal-call-folding");
                    *folded += 1;
                    return;
                }
            }
        }
    }

    let Some(bid) = (match source.node(node) {
        Node::Call(source_function, _, _)
            if matches!(source.node(*source_function), Node::Name(_)) =>
        {
            res.node_bid
                .get(*source_function as usize)
                .copied()
                .flatten()
        }
        _ => None,
    }) else {
        return;
    };
    let (Some(info), Some(expression)) = (functions.get(&bid), candidates.get(&bid)) else {
        return;
    };
    if arguments.len() != info.parameters.len()
        || !arguments
            .iter()
            .all(|argument| is_literal(target, *argument))
    {
        return;
    }
    let replacements = info
        .parameter_bids
        .iter()
        .copied()
        .zip(arguments.iter().copied())
        .collect::<HashMap<_, _>>();
    let inlined =
        clone_subtree_with_replacements(target, source, res, *expression, &replacements, origins);
    let simplified = fold_inlined_expression(target, inlined, aggressive);
    if measure_expr(target, simplified) < measure_expr(target, node) {
        install_expression(target, node, simplified, origins);
        *folded += 1;
    } else {
        target.nodes.rewrite(
            node,
            Node::Call(function, arguments, method),
            "literal-call-rejected-candidate",
        );
    }
}

pub fn fold_literal_function_calls(ast: &mut Ast, root: NodeId, aggressive: bool) -> PassResult {
    let before = measure_size(ast, root);
    if ast.strings.contains("_ENV") {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let source = clone_ast(ast);
    let res = resolve(&source, root);
    let infos = function_infos(&source, &res);
    let functions = info_map(&infos);
    let analyzer = EffectAnalyzer::new(&source, &res, root, true);
    let mut candidates = HashMap::<BindingId, NodeId>::new();
    for info in &infos {
        if info.variadic {
            continue;
        }
        let Node::Block(statements) = source.node(info.body) else {
            continue;
        };
        if statements.len() != 1 {
            continue;
        }
        let Node::Return(expressions) = source.node(statements[0]) else {
            continue;
        };
        if expressions.len() == 1 {
            candidates.insert(info.bid, expressions[0]);
        }
    }

    let mut target = clone_ast(&source);
    let mut origins = reference_origins(&res);
    let mut folded = 0usize;
    rewrite_literal_calls(
        &source,
        &mut target,
        &res,
        &analyzer,
        &functions,
        &candidates,
        root,
        aggressive,
        &mut origins,
        &mut folded,
    );
    let mut after = measure_size(&target, root);
    if after <= before && expression_bindings_preserved(&target, root, &res, &origins) {
        *ast = target;
    } else {
        after = before;
        folded = 0;
    }
    PassResult {
        root,
        saved: Some(before.saturating_sub(after) as u64),
        details: Some(vec![format!("folded={folded}")]),
    }
}

#[allow(clippy::too_many_arguments)]
fn rewrite_expression_helpers(
    source: &Ast,
    target: &mut Ast,
    res: &Resolution,
    effects: &EffectAnalyzer<'_>,
    functions: &HashMap<BindingId, FunctionInfo>,
    candidates: &HashMap<BindingId, NodeId>,
    call_counts: &HashMap<BindingId, usize>,
    non_call_refs: &HashSet<BindingId>,
    node: NodeId,
    aggressive: bool,
    whole_program: bool,
    origins: &mut ReferenceOrigins,
    inlined_counts: &mut HashMap<BindingId, usize>,
) {
    let mut children = Vec::new();
    storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| children.push(child));
    for child in children {
        rewrite_expression_helpers(
            source,
            target,
            res,
            effects,
            functions,
            candidates,
            call_counts,
            non_call_refs,
            child,
            aggressive,
            whole_program,
            origins,
            inlined_counts,
        );
    }

    let Some(bid) = direct_function_bid(source, res, node) else {
        return;
    };
    let (Some(info), Some(expression)) = (functions.get(&bid), candidates.get(&bid)) else {
        return;
    };
    let Node::Call(_, arguments, _) = target.node(node).clone() else {
        return;
    };
    if arguments.len() != info.parameters.len() {
        return;
    }
    if arguments.len() > 1
        && arguments
            .iter()
            .any(|argument| !is_literal(target, *argument))
    {
        // Multiple dynamic arguments can be observed in a different order
        // after substitution (the helper body may use b before a).
        return;
    }
    // Inlining substitutes arguments at their parameter's use sites.  Lua
    // evaluates call arguments left-to-right, so `return b-a` with two
    // effectful arguments would otherwise become `arg2-arg1`.  Pure/movable
    // arguments are safe; retain the call when an effectful argument would be
    // observed in a different order.
    let parameter_order = walk_nodes(source, *expression)
        .into_iter()
        .filter_map(|id| {
            let bid = res.node_bid.get(id as usize).copied().flatten()?;
            info.parameter_bids
                .iter()
                .position(|candidate| *candidate == bid)
        })
        .collect::<Vec<_>>();
    let preserves_order = parameter_order.windows(2).all(|pair| pair[0] <= pair[1]);
    if !preserves_order
        && arguments
            .iter()
            .any(|argument| !is_literal(target, *argument))
    {
        return;
    }
    let mut replacements = HashMap::new();
    for (index, parameter_bid) in info.parameter_bids.iter().copied().enumerate() {
        let uses = count_param_uses(source, res, *expression, parameter_bid);
        let source_argument = match source.node(node) {
            Node::Call(_, source_arguments, _) => source_arguments[index],
            _ => return,
        };
        if uses > 1 && !effects.is_movable(source_argument) {
            return;
        }
        if !aggressive && uses == 0 && !effects.is_movable(source_argument) {
            return;
        }
        replacements.insert(parameter_bid, arguments[index]);
    }
    let inlined =
        clone_subtree_with_replacements(target, source, res, *expression, &replacements, origins);
    let old_len = measure_expr(target, node);
    let new_len = measure_expr(target, inlined);
    let count = call_counts.get(&bid).copied().unwrap_or(1);
    let decl_cost = measure_stmt(source, info.declaration);
    let profitable = new_len < old_len
        || (whole_program
            && !non_call_refs.contains(&bid)
            && new_len * count < old_len * count + decl_cost);
    if profitable {
        install_expression(target, node, inlined, origins);
        *inlined_counts.entry(bid).or_default() += 1;
    }
}

pub fn inline_expression_functions(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    whole_program: bool,
) -> PassResult {
    let before = measure_size(ast, root);
    if ast.strings.contains("_ENV") {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let source = clone_ast(ast);
    let res = resolve(&source, root);
    let infos = function_infos(&source, &res);
    let functions = info_map(&infos);
    let effects = EffectAnalyzer::new(&source, &res, root, true);
    let (call_counts, non_call_refs) = function_reference_sets(&source, root, &res, &functions);
    let mut candidates = HashMap::<BindingId, NodeId>::new();
    for info in &infos {
        if info.variadic {
            continue;
        }
        let Node::Block(statements) = source.node(info.body) else {
            continue;
        };
        if statements.len() == 1 {
            if let Node::Return(expressions) = source.node(statements[0]) {
                if expressions.len() == 1 {
                    candidates.insert(info.bid, expressions[0]);
                }
            }
        }
    }

    let mut target = clone_ast(&source);
    let mut origins = reference_origins(&res);
    let mut inlined_counts = HashMap::<BindingId, usize>::new();
    rewrite_expression_helpers(
        &source,
        &mut target,
        &res,
        &effects,
        &functions,
        &candidates,
        &call_counts,
        &non_call_refs,
        root,
        aggressive,
        whole_program,
        &mut origins,
        &mut inlined_counts,
    );

    if whole_program {
        for info in &infos {
            if non_call_refs.contains(&info.bid) {
                continue;
            }
            let calls = call_counts.get(&info.bid).copied().unwrap_or(0);
            let inlined = inlined_counts.get(&info.bid).copied().unwrap_or(0);
            if calls > 0 && calls == inlined {
                remove_statement_from_blocks(&mut target, info.declaration);
            }
        }
    }

    let after = measure_size(&target, root);
    if after < before && expression_bindings_preserved(&target, root, &res, &origins) {
        *ast = target;
        PassResult {
            root,
            saved: Some((before - after) as u64),
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

    fn output(source: &str, which: u8) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        match which {
            0 => specialize_constant_arguments(&mut ast, root, true),
            1 => fold_literal_function_calls(&mut ast, root, true),
            _ => inline_expression_functions(&mut ast, root, true, true),
        };
        Printer::new(&ast, false).output(root)
    }

    #[test]
    fn specializes_common_literal_argument() {
        let out = output("local function f(x,y)return x+y end a=f(1,2)b=f(3,2)", 0);
        assert!(out.contains("function f(x)"), "{out}");
        assert!(out.contains("f(1)"), "{out}");
    }

    #[test]
    fn specialization_does_not_bypass_numeric_folding_gate() {
        let source = "local function f(x,d)return x/d end a=f(.10000000149011612,6)b=f(.20000000298023224,6)";
        let (mut ast, root) = parse_source(source).expect("parse");
        specialize_constant_arguments(&mut ast, root, false);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains(".10000000149011612"), "{out}");
    }

    #[test]
    fn folds_literal_user_call() {
        let out = output("function f(x)return x+1 end a=f(2)", 1);
        assert!(out.contains("a=3"), "{out}");
    }

    #[test]
    fn fuses_one_use_tail_function() {
        let (mut ast, root) = parse_source(
            "local function f(x)local y=x+1 return y*2 end function onTick()local z=f(input.getNumber(1))output.setNumber(1,z)end",
        )
        .expect("parse");
        inline_one_use_statement_and_tail_functions(&mut ast, root, 32);
        let out = Printer::new(&ast, false).output(root);
        assert!(!out.contains("function f"), "{out}");
    }

    #[test]
    fn preserves_function_definition_for_non_call_reference() {
        let out = output(
            "local function f(x)return x+1 end g=f function onTick()output.setNumber(1,f(1))output.setNumber(2,g(2))end",
            2,
        );
        assert!(out.contains("function f"), "{out}");
        assert!(out.contains("g=f"), "{out}");
    }
}

fn scope_descendant(res: &Resolution, mut scope: u32, ancestor: u32) -> bool {
    loop {
        if scope == ancestor {
            return true;
        }
        let Some(parent) = res
            .scopes
            .get(scope as usize)
            .and_then(|scope| scope.parent)
        else {
            return false;
        };
        scope = parent;
    }
}

fn alpha_clone_node(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    names: &HashMap<BindingId, SymbolId>,
    free_references: &mut Vec<(NodeId, BindingId)>,
) -> NodeId {
    if let Node::Name(symbol) = source.node(node) {
        let symbol = res
            .node_bid
            .get(node as usize)
            .copied()
            .flatten()
            .and_then(|bid| names.get(&bid).copied())
            .unwrap_or(*symbol);
        let copied = target.push(Node::Name(symbol));
        target
            .nodes
            .finish_rename(copied, source.nodes.capture_origin(node));
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            if !names.contains_key(&bid) {
                free_references.push((copied, bid));
            }
        }
        return copied;
    }
    let original = source.node(node).clone();
    let (mut mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        alpha_clone_node(target, source, res, child, names, free_references)
    });
    match &mut mapped {
        Node::Local(local_names, _) | Node::Forin(local_names, _, _) => {
            if let Some(bids) = res.node_bids.get(node as usize) {
                for (index, bid) in bids.iter().enumerate() {
                    if let (Some(slot), Some(symbol)) =
                        (local_names.get_mut(index), names.get(bid).copied())
                    {
                        *slot = symbol;
                    }
                }
            }
        }
        Node::Function(parameters, _, _) => {
            if let Some(bids) = res.node_bids.get(node as usize) {
                for (index, bid) in bids.iter().enumerate() {
                    if let (Some(slot), Some(symbol)) =
                        (parameters.get_mut(index), names.get(bid).copied())
                    {
                        *slot = symbol;
                    }
                }
            }
        }
        Node::Localfunc(name, _) | Node::Fornum(name, ..) => {
            if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
                if let Some(symbol) = names.get(&bid).copied() {
                    *name = symbol;
                }
            }
        }
        _ => {}
    }
    let copied = target.push(mapped);
    // Alpha conversion changes binding spellings but never reorders the slots.
    target
        .nodes
        .finish_rename(copied, source.nodes.capture_origin(node));
    copied
}

#[derive(Clone, Copy)]
enum FusionKind {
    Statement,
    TailLocal,
    TailAssign,
}

#[derive(Clone, Copy)]
struct FusionSite {
    block: NodeId,
    index: usize,
    statement: NodeId,
    call: NodeId,
    kind: FusionKind,
}

fn direct_call_matches(ast: &Ast, res: &Resolution, call: NodeId, bid: BindingId) -> bool {
    let Node::Call(function, _, method) = ast.node(call) else {
        return false;
    };
    method.is_none()
        && matches!(ast.node(*function), Node::Name(_))
        && res.node_bid.get(*function as usize).copied().flatten() == Some(bid)
}

fn find_fusion_site_node(
    ast: &Ast,
    res: &Resolution,
    node: NodeId,
    bid: BindingId,
    statement_mode: bool,
    tail_mode: bool,
) -> Option<FusionSite> {
    if matches!(ast.node(node), Node::Block(_)) {
        return find_fusion_site_block(ast, res, node, bid, statement_mode, tail_mode);
    }
    let mut result = None;
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        if result.is_none() {
            result = find_fusion_site_node(ast, res, child, bid, statement_mode, tail_mode);
        }
    });
    result
}

fn find_fusion_site_block(
    ast: &Ast,
    res: &Resolution,
    block: NodeId,
    bid: BindingId,
    statement_mode: bool,
    tail_mode: bool,
) -> Option<FusionSite> {
    let Node::Block(statements) = ast.node(block) else {
        return None;
    };
    for (index, statement) in statements.iter().copied().enumerate() {
        // mapBlocks() transforms nested blocks before testing the containing statement.
        let mut nested = None;
        storm_lua_analysis::resolver::for_each_child(ast, statement, &mut |child| {
            if nested.is_none() {
                nested = find_fusion_site_node(ast, res, child, bid, statement_mode, tail_mode);
            }
        });
        if nested.is_some() {
            return nested;
        }
        match ast.node(statement) {
            Node::Callstat(call) if statement_mode && direct_call_matches(ast, res, *call, bid) => {
                return Some(FusionSite {
                    block,
                    index,
                    statement,
                    call: *call,
                    kind: FusionKind::Statement,
                });
            }
            Node::Local(_, expressions)
                if tail_mode
                    && expressions.len() == 1
                    && direct_call_matches(ast, res, expressions[0], bid) =>
            {
                return Some(FusionSite {
                    block,
                    index,
                    statement,
                    call: expressions[0],
                    kind: FusionKind::TailLocal,
                });
            }
            Node::Assign(_, expressions)
                if tail_mode
                    && expressions.len() == 1
                    && direct_call_matches(ast, res, expressions[0], bid) =>
            {
                return Some(FusionSite {
                    block,
                    index,
                    statement,
                    call: expressions[0],
                    kind: FusionKind::TailAssign,
                });
            }
            _ => {}
        }
    }
    None
}

fn one_use_reference_sets(
    ast: &Ast,
    root: NodeId,
    res: &Resolution,
    functions: &HashMap<BindingId, FunctionInfo>,
) -> (HashMap<BindingId, usize>, HashSet<BindingId>) {
    let mut direct_names = HashSet::new();
    let mut definition_names = HashSet::new();
    let mut call_counts = HashMap::<BindingId, usize>::new();
    for node in walk_nodes(ast, root) {
        match ast.node(node) {
            Node::Call(function, _, method)
                if method.is_none() && matches!(ast.node(*function), Node::Name(_)) =>
            {
                if let Some(bid) = res.node_bid.get(*function as usize).copied().flatten() {
                    if functions.contains_key(&bid) {
                        direct_names.insert(*function);
                        *call_counts.entry(bid).or_default() += 1;
                    }
                }
            }
            Node::Funcstat(target, _) if matches!(ast.node(*target), Node::Name(_)) => {
                if let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() {
                    if functions.contains_key(&bid) {
                        definition_names.insert(*target);
                    }
                }
            }
            _ => {}
        }
    }
    let mut bad_refs = HashSet::new();
    for node in walk_nodes(ast, root) {
        if !matches!(ast.node(node), Node::Name(_))
            || direct_names.contains(&node)
            || definition_names.contains(&node)
        {
            continue;
        }
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            if functions.contains_key(&bid) {
                bad_refs.insert(bid);
            }
        }
    }
    (call_counts, bad_refs)
}

pub fn inline_one_use_statement_and_tail_functions(
    ast: &mut Ast,
    root: NodeId,
    max_rounds: usize,
) -> PassResult {
    let original_size = measure_size(ast, root);
    // Implicit global accesses use the lexical _ENV upvalue in Lua 5.3.
    // The resolver's global BindingId alone cannot prove that environment is
    // unchanged after crossing a function boundary.
    if ast.strings.contains("_ENV") {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let mut serial = 0usize;
    let mut details = Vec::new();

    for _ in 0..max_rounds {
        let source = clone_ast(ast);
        let res = resolve(&source, root);
        let infos = function_infos(&source, &res);
        let functions = info_map(&infos);
        let (call_counts, bad_refs) = one_use_reference_sets(&source, root, &res, &functions);
        let mut applied = false;

        for info in &infos {
            if call_counts.get(&info.bid).copied().unwrap_or(0) != 1 || bad_refs.contains(&info.bid)
            {
                continue;
            }
            let binding_name = source.strings.get(res.binding(info.bid).name);
            if storm_lua_spec::environment::VEHICLE_CALLBACKS.contains(&binding_name)
                || info.variadic
            {
                continue;
            }
            let body_nodes = walk_nodes(&source, info.body);
            if body_nodes.iter().any(|node| {
                matches!(
                    source.node(*node),
                    Node::Goto(_) | Node::Label(_) | Node::Vararg
                )
            }) {
                continue;
            }
            let returns = body_nodes
                .iter()
                .copied()
                .filter(|node| matches!(source.node(*node), Node::Return(_)))
                .collect::<Vec<_>>();
            let Node::Block(body_statements) = source.node(info.body) else {
                continue;
            };
            let statement_mode = returns.is_empty();
            let tail_mode =
                returns.len() == 1 && body_statements.last().copied() == Some(returns[0]);
            if !statement_mode && !tail_mode {
                continue;
            }

            serial += 1;
            let function_scope = res
                .node_scope_id
                .get(info.function as usize)
                .copied()
                .flatten()
                .unwrap_or(0);
            let mut trial = clone_ast(&source);
            let mut taken = source
                .strings
                .all_strings()
                .into_iter()
                .collect::<HashSet<_>>();
            let mut renamed = HashMap::<BindingId, SymbolId>::new();
            for (bid, binding) in res.bindings.iter().enumerate().skip(1) {
                if binding.kind != storm_lua_analysis::resolver::BindingKind::Global
                    && scope_descendant(&res, binding.scope, function_scope)
                {
                    let base = format!("__i{serial}_{bid}");
                    let mut name = base.clone();
                    let mut suffix = 0;
                    while !taken.insert(name.clone()) {
                        suffix += 1;
                        name = format!("{base}_{suffix}");
                    }
                    let symbol = trial.strings.intern(&name);
                    renamed.insert(bid as BindingId, symbol);
                }
            }
            let mut free_references = Vec::new();
            let first_alpha_node = trial.nodes.len();
            let alpha_function = alpha_clone_node(
                &mut trial,
                &source,
                &res,
                info.function,
                &renamed,
                &mut free_references,
            );
            let Node::Function(alpha_params, _, alpha_body) = trial.node(alpha_function).clone()
            else {
                continue;
            };
            let Node::Block(alpha_statements) = trial.node(alpha_body).clone() else {
                continue;
            };
            let alpha_tail = alpha_statements.last().copied();

            let Some(site) =
                find_fusion_site_block(&source, &res, root, info.bid, statement_mode, tail_mode)
            else {
                continue;
            };
            let Node::Call(_, call_args, _) = source.node(site.call).clone() else {
                continue;
            };
            // Safety fix mirrored from TS: a zero-parameter function cannot drop
            // evaluation of surplus arguments in tail mode (or statement mode).
            if alpha_params.is_empty() && !call_args.is_empty() {
                continue;
            }

            if trial.nodes.tracks_origins() {
                for copied in first_alpha_node..trial.nodes.len() {
                    trial.nodes.relate_from(
                        copied as NodeId,
                        &source.nodes,
                        site.call,
                        "one-use-function-call-site",
                    );
                }
            }
            let mut replacement = Vec::<NodeId>::new();
            if !alpha_params.is_empty() {
                let args = call_args;
                let local = trial.push(Node::Local(alpha_params.clone(), args));
                trial.nodes.derive_from(
                    local,
                    &source.nodes,
                    site.statement,
                    "inlined-parameter-binding",
                );
                for index in 0..alpha_params.len() {
                    trial.nodes.copy_name_from(
                        local,
                        storm_lua_syntax::NameSite::Binding(index as u32),
                        &source.nodes,
                        info.function,
                        storm_lua_syntax::NameSite::Parameter(index as u32),
                    );
                }
                replacement.push(local);
            }
            match site.kind {
                FusionKind::Statement => {
                    replacement.extend(alpha_statements.iter().copied());
                }
                FusionKind::TailLocal => {
                    replacement.extend(
                        alpha_statements[..alpha_statements.len().saturating_sub(1)]
                            .iter()
                            .copied(),
                    );
                    let Some(Node::Return(tail_values)) =
                        alpha_tail.map(|tail| trial.node(tail).clone())
                    else {
                        continue;
                    };
                    let Node::Local(names, _) = source.node(site.statement).clone() else {
                        continue;
                    };
                    let statement = trial.push(Node::Local(names, tail_values));
                    trial.nodes.derive_from(
                        statement,
                        &source.nodes,
                        site.statement,
                        "inlined-tail-local",
                    );
                    if let Some(tail) = alpha_tail {
                        trial
                            .nodes
                            .relate_within(statement, tail, "inlined-tail-return");
                    }
                    replacement.push(statement);
                }
                FusionKind::TailAssign => {
                    replacement.extend(
                        alpha_statements[..alpha_statements.len().saturating_sub(1)]
                            .iter()
                            .copied(),
                    );
                    let Some(Node::Return(tail_values)) =
                        alpha_tail.map(|tail| trial.node(tail).clone())
                    else {
                        continue;
                    };
                    let Node::Assign(targets, _) = source.node(site.statement).clone() else {
                        continue;
                    };
                    let statement = trial.push(Node::Assign(targets, tail_values));
                    trial.nodes.derive_from(
                        statement,
                        &source.nodes,
                        site.statement,
                        "inlined-tail-assignment",
                    );
                    if let Some(tail) = alpha_tail {
                        trial
                            .nodes
                            .relate_within(statement, tail, "inlined-tail-return");
                    }
                    replacement.push(statement);
                }
            }
            let Node::Block(mut block_statements) = trial.node(site.block).clone() else {
                continue;
            };
            block_statements.splice(site.index..=site.index, replacement);
            trial.nodes.rewrite(
                site.block,
                Node::Block(block_statements),
                "one-use-function-fusion",
            );
            remove_statement_from_blocks(&mut trial, info.declaration);

            // Alpha-renaming protects the callee's own locals, not its free
            // variables. Resolve the actual inserted references again: the
            // original declaration node + name + kind must still identify the
            // same captured storage (BindingIds are rebuilt after insertion).
            // This also catches caller-local shadowing of an original global.
            if !free_references.is_empty() {
                let relocated = resolve(&trial, root);
                if free_references.iter().any(|&(node, old_bid)| {
                    let Some(new_bid) = relocated.node_bid.get(node as usize).copied().flatten()
                    else {
                        return true;
                    };
                    let (old, new) = (res.binding(old_bid), relocated.binding(new_bid));
                    old.name != new.name || old.kind != new.kind || old.decl_node != new.decl_node
                }) {
                    continue;
                }
            }

            let old_len = measure_size(&source, root);
            let new_len = measure_size(&trial, root);
            let allowance = 4000.0f64.max(old_len as f64 * 0.4);
            if (new_len as f64) < old_len as f64 + allowance {
                *ast = trial;
                details.push(format!(
                    "function={binding_name};mode={};delta={}",
                    if statement_mode { "statement" } else { "tail" },
                    old_len as isize - new_len as isize
                ));
                applied = true;
                break;
            }
        }
        if !applied {
            break;
        }
    }

    PassResult {
        root,
        saved: Some(original_size.saturating_sub(measure_size(ast, root)) as u64),
        details: Some(details),
    }
}

/// Late reversal of a one-use expression helper (`postprocess.ts`).
pub fn inline_one_use_expression_helpers(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
) -> PassResult {
    let before = measure_size(ast, root);
    if ast.strings.contains("_ENV") {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let source = clone_ast(ast);
    let res = resolve(&source, root);
    let infos = function_infos(&source, &res);
    let functions = info_map(&infos);
    let effects = EffectAnalyzer::new(&source, &res, root, true);
    let (call_counts, non_call_refs) = function_reference_sets(&source, root, &res, &functions);
    let mut returns = HashMap::<BindingId, NodeId>::new();
    for info in &infos {
        if info.variadic {
            continue;
        }
        let Node::Block(statements) = source.node(info.body) else {
            continue;
        };
        if statements.len() != 1 {
            continue;
        }
        if let Node::Return(expressions) = source.node(statements[0]) {
            if expressions.len() == 1 {
                returns.insert(info.bid, expressions[0]);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rewrite(
        source: &Ast,
        target: &mut Ast,
        res: &Resolution,
        effects: &EffectAnalyzer<'_>,
        functions: &HashMap<BindingId, FunctionInfo>,
        returns: &HashMap<BindingId, NodeId>,
        call_counts: &HashMap<BindingId, usize>,
        non_call_refs: &HashSet<BindingId>,
        node: NodeId,
        aggressive: bool,
        origins: &mut ReferenceOrigins,
        inlined: &mut HashSet<BindingId>,
    ) {
        let mut children = Vec::new();
        storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| {
            children.push(child)
        });
        for child in children {
            rewrite(
                source,
                target,
                res,
                effects,
                functions,
                returns,
                call_counts,
                non_call_refs,
                child,
                aggressive,
                origins,
                inlined,
            );
        }
        let Some(bid) = direct_function_bid(source, res, node) else {
            return;
        };
        if call_counts.get(&bid).copied().unwrap_or(0) != 1 || non_call_refs.contains(&bid) {
            return;
        }
        let (Some(info), Some(return_expr)) = (functions.get(&bid), returns.get(&bid)) else {
            return;
        };
        let Node::Call(_, arguments, _) = target.node(node).clone() else {
            return;
        };
        if arguments.len() != info.parameters.len() {
            return;
        }
        if arguments.len() > 1
            && arguments
                .iter()
                .any(|argument| !is_literal(target, *argument))
        {
            // Keep left-to-right Lua argument evaluation when the helper body
            // uses parameters in a reordered expression.
            return;
        }
        let mut replacements = HashMap::new();
        for (index, parameter_bid) in info.parameter_bids.iter().copied().enumerate() {
            let uses = count_param_uses(source, res, *return_expr, parameter_bid);
            let source_argument = match source.node(node) {
                Node::Call(_, source_arguments, _) => source_arguments[index],
                _ => return,
            };
            let duplicable = if effects.is_movable(source_argument) {
                true
            } else if aggressive {
                let effect = effects.effects_for_expr(source_argument);
                !effect.calls && !effect.ordered && effect.writes.is_empty() && effect.stable
            } else {
                false
            };
            if uses > 1 && !duplicable {
                return;
            }
            if uses == 0 && !effects.is_movable(source_argument) {
                return;
            }
            replacements.insert(parameter_bid, arguments[index]);
        }
        let replacement = clone_subtree_with_replacements(
            target,
            source,
            res,
            *return_expr,
            &replacements,
            origins,
        );
        install_expression(target, node, replacement, origins);
        inlined.insert(bid);
    }

    let mut target = clone_ast(&source);
    let mut origins = reference_origins(&res);
    let mut inlined = HashSet::new();
    rewrite(
        &source,
        &mut target,
        &res,
        &effects,
        &functions,
        &returns,
        &call_counts,
        &non_call_refs,
        root,
        aggressive,
        &mut origins,
        &mut inlined,
    );
    for bid in &inlined {
        if let Some(info) = functions.get(bid) {
            remove_statement_from_blocks(&mut target, info.declaration);
        }
    }
    let after = measure_size(&target, root);
    if after < before && expression_bindings_preserved(&target, root, &res, &origins) {
        *ast = target;
        PassResult {
            root,
            saved: Some((before - after) as u64),
            details: Some(vec![format!("inlined={}", inlined.len())]),
        }
    } else {
        PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["inlined=0".to_string()]),
        }
    }
}
