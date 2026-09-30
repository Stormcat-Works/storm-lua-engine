//! Temporary global packing (`passes/temporary-globals.ts`).

use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use storm_lua_analysis::resolver::{
    api_roots, resolve, BindingId, BindingKind, Resolution, ScopeId,
};
use storm_lua_syntax::ast::{Ast, Node, NodeId};

#[derive(Clone, Copy)]
struct Reference {
    owner: ScopeId,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn scan_owner(
    ast: &Ast,
    res: &Resolution,
    node: NodeId,
    owner: ScopeId,
    references: &mut HashMap<BindingId, Vec<Reference>>,
    calls: &mut HashMap<ScopeId, HashSet<ScopeId>>,
    unknown_call_owners: &mut HashSet<ScopeId>,
) {
    if matches!(ast.node(node), Node::Function(..)) {
        let Some(scope) = res.node_scope_id.get(node as usize).copied().flatten() else {
            return;
        };
        let Node::Function(_, _, body) = ast.node(node) else {
            unreachable!()
        };
        scan_owner(
            ast,
            res,
            *body,
            scope,
            references,
            calls,
            unknown_call_owners,
        );
        return;
    }
    if matches!(ast.node(node), Node::Name(_)) {
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            references.entry(bid).or_default().push(Reference { owner });
        }
    }
    if let Node::Call(function, _, _) = ast.node(node) {
        let mut known = false;
        match ast.node(*function) {
            Node::Name(symbol) => {
                if let Some(bid) = res.node_bid.get(*function as usize).copied().flatten() {
                    let binding = &res.bindings[bid as usize];
                    if let Some(function_node) = binding.function_node {
                        if let Some(callee_scope) = res
                            .node_scope_id
                            .get(function_node as usize)
                            .copied()
                            .flatten()
                        {
                            calls.entry(owner).or_default().insert(callee_scope);
                            known = true;
                        }
                    } else if binding.fixed || api_roots(ast.strings.get(*symbol)) {
                        known = true;
                    }
                } else if api_roots(ast.strings.get(*symbol)) {
                    known = true;
                }
            }
            Node::Index(object, _, _) => {
                if let Node::Name(symbol) = ast.node(*object) {
                    if api_roots(ast.strings.get(*symbol)) {
                        known = true;
                    }
                }
            }
            _ => {}
        }
        if !known {
            unknown_call_owners.insert(owner);
        }
    }
    let mut children = Vec::new();
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| children.push(child));
    for child in children {
        scan_owner(
            ast,
            res,
            child,
            owner,
            references,
            calls,
            unknown_call_owners,
        );
    }
}

fn read_expression(
    ast: &Ast,
    res: &Resolution,
    node: NodeId,
    assigned: &HashSet<BindingId>,
    confined: &HashMap<BindingId, ScopeId>,
    unsafe_bids: &mut HashSet<BindingId>,
) {
    if matches!(ast.node(node), Node::Name(_)) {
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            if confined.contains_key(&bid)
                && !res.node_write.get(node as usize).copied().unwrap_or(false)
                && !assigned.contains(&bid)
            {
                unsafe_bids.insert(bid);
            }
        }
    }
    let mut children = Vec::new();
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| children.push(child));
    for child in children {
        read_expression(ast, res, child, assigned, confined, unsafe_bids);
    }
}

fn intersection(sets: &[HashSet<BindingId>]) -> HashSet<BindingId> {
    let Some(first) = sets.first() else {
        return HashSet::new();
    };
    first
        .iter()
        .copied()
        .filter(|bid| sets.iter().skip(1).all(|set| set.contains(bid)))
        .collect()
}

fn analyze_block(
    ast: &Ast,
    res: &Resolution,
    block: NodeId,
    input: &HashSet<BindingId>,
    confined: &HashMap<BindingId, ScopeId>,
    unsafe_bids: &mut HashSet<BindingId>,
) -> HashSet<BindingId> {
    let Node::Block(statements) = ast.node(block) else {
        return input.clone();
    };
    let mut assigned = input.clone();
    for statement in statements {
        match ast.node(*statement) {
            Node::Assign(targets, expressions) => {
                for expression in expressions {
                    read_expression(ast, res, *expression, &assigned, confined, unsafe_bids);
                }
                for target in targets {
                    if matches!(ast.node(*target), Node::Name(_)) {
                        if let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() {
                            if confined.contains_key(&bid) {
                                assigned.insert(bid);
                            }
                        }
                    } else {
                        read_expression(ast, res, *target, &assigned, confined, unsafe_bids);
                    }
                }
            }
            Node::Local(_, expressions) => {
                for expression in expressions {
                    read_expression(ast, res, *expression, &assigned, confined, unsafe_bids);
                }
            }
            Node::If(arms, else_block) => {
                let mut outputs = Vec::new();
                for arm in arms {
                    read_expression(ast, res, arm.cond, &assigned, confined, unsafe_bids);
                    outputs.push(analyze_block(
                        ast,
                        res,
                        arm.body,
                        &assigned,
                        confined,
                        unsafe_bids,
                    ));
                }
                outputs.push(match else_block {
                    Some(block) => {
                        analyze_block(ast, res, *block, &assigned, confined, unsafe_bids)
                    }
                    None => assigned.clone(),
                });
                assigned = intersection(&outputs);
            }
            Node::Do(body) => {
                assigned = analyze_block(ast, res, *body, &assigned, confined, unsafe_bids);
            }
            Node::While(condition, body) => {
                read_expression(ast, res, *condition, &assigned, confined, unsafe_bids);
                let _ = analyze_block(ast, res, *body, &assigned, confined, unsafe_bids);
            }
            Node::Repeat(body, condition) => {
                let body_output = analyze_block(ast, res, *body, &assigned, confined, unsafe_bids);
                read_expression(ast, res, *condition, &body_output, confined, unsafe_bids);
            }
            Node::Fornum(_, start, end, step, body) => {
                read_expression(ast, res, *start, &assigned, confined, unsafe_bids);
                read_expression(ast, res, *end, &assigned, confined, unsafe_bids);
                if let Some(step) = step {
                    read_expression(ast, res, *step, &assigned, confined, unsafe_bids);
                }
                let _ = analyze_block(ast, res, *body, &assigned, confined, unsafe_bids);
            }
            Node::Forin(_, expressions, body) => {
                for expression in expressions {
                    read_expression(ast, res, *expression, &assigned, confined, unsafe_bids);
                }
                let _ = analyze_block(ast, res, *body, &assigned, confined, unsafe_bids);
            }
            Node::Return(expressions) => {
                for expression in expressions {
                    read_expression(ast, res, *expression, &assigned, confined, unsafe_bids);
                }
            }
            Node::Callstat(expression) => {
                read_expression(ast, res, *expression, &assigned, confined, unsafe_bids);
            }
            _ => read_expression(ast, res, *statement, &assigned, confined, unsafe_bids),
        }
    }
    assigned
}

fn can_reach(calls: &HashMap<ScopeId, HashSet<ScopeId>>, from: ScopeId, target: ScopeId) -> bool {
    let mut seen = HashSet::from([from]);
    let mut stack = vec![from];
    while let Some(current) = stack.pop() {
        for next in calls.get(&current).into_iter().flatten() {
            if *next == target {
                return true;
            }
            if seen.insert(*next) {
                stack.push(*next);
            }
        }
    }
    false
}

pub fn pack_temporary_globals(ast: &mut Ast, root: NodeId) -> PassResult {
    let source = clone_ast(ast);
    let res = resolve(&source, root);

    let mut function_bodies = HashMap::<ScopeId, NodeId>::new();
    for node in 0..source.nodes.len() as NodeId {
        if let Node::Function(_, _, body) = source.node(node) {
            if let Some(scope) = res.node_scope_id.get(node as usize).copied().flatten() {
                function_bodies.insert(scope, *body);
            }
        }
    }

    let mut references = HashMap::<BindingId, Vec<Reference>>::new();
    let mut calls = HashMap::<ScopeId, HashSet<ScopeId>>::new();
    let mut unknown_call_owners = HashSet::<ScopeId>::new();
    scan_owner(
        &source,
        &res,
        root,
        0,
        &mut references,
        &mut calls,
        &mut unknown_call_owners,
    );

    let mut confined = HashMap::<BindingId, ScopeId>::new();
    for (bid, binding) in res.bindings.iter().enumerate().skip(1) {
        let bid = bid as BindingId;
        if binding.kind != BindingKind::Global
            || binding.fixed
            || binding.function_node.is_some()
            || source.strings.get(binding.name).starts_with("__fs")
        {
            continue;
        }
        let owners = references
            .get(&bid)
            .into_iter()
            .flatten()
            .map(|reference| reference.owner)
            .collect::<HashSet<_>>();
        let mut owners = owners.into_iter();
        if let (Some(owner), None) = (owners.next(), owners.next()) {
            if owner != 0 {
                confined.insert(bid, owner);
            }
        }
    }
    if confined.is_empty() {
        return PassResult {
            root,
            saved: None,
            details: Some(vec!["packed=0;slots=0".to_string()]),
        };
    }

    let mut unsafe_bids = HashSet::new();
    let relevant_owners = confined.values().copied().collect::<HashSet<_>>();
    for owner in relevant_owners {
        if let Some(body) = function_bodies.get(&owner) {
            let _ = analyze_block(
                &source,
                &res,
                *body,
                &HashSet::new(),
                &confined,
                &mut unsafe_bids,
            );
        }
    }
    let temporaries = confined
        .keys()
        .copied()
        .filter(|bid| !unsafe_bids.contains(bid))
        .collect::<HashSet<_>>();
    if temporaries.is_empty() {
        return PassResult {
            root,
            saved: None,
            details: Some(vec!["packed=0;slots=0".to_string()]),
        };
    }

    // TS iterates resolver.bindings (in BindingId insertion order), then the
    // confined Map in the same binding order. Keep this order because DSATUR
    // uses stable sort tie-breaking and therefore color numbers are observable.
    let scratch = res
        .bindings
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(bid, binding)| {
            (binding.kind == BindingKind::Global
                && source.strings.get(binding.name).starts_with("__fs"))
            .then_some(bid as BindingId)
        })
        .collect::<Vec<_>>();
    let mut temporary_order = temporaries.iter().copied().collect::<Vec<_>>();
    temporary_order.sort_unstable();
    let mut nodes = scratch;
    nodes.extend(temporary_order);

    let owner_sets = nodes
        .iter()
        .copied()
        .map(|bid| {
            let owners = references
                .get(&bid)
                .into_iter()
                .flatten()
                .map(|reference| reference.owner)
                .collect::<HashSet<_>>();
            (bid, owners)
        })
        .collect::<HashMap<_, _>>();

    let owners_interfere = |a: ScopeId, b: ScopeId| {
        a == b
            || unknown_call_owners.contains(&a)
            || unknown_call_owners.contains(&b)
            || can_reach(&calls, a, b)
            || can_reach(&calls, b, a)
    };
    let bindings_interfere = |a: BindingId, b: BindingId| {
        let aa = &res.bindings[a as usize];
        let bb = &res.bindings[b as usize];
        for owner_a in owner_sets.get(&a).into_iter().flatten() {
            for owner_b in owner_sets.get(&b).into_iter().flatten() {
                if owner_a == owner_b {
                    if !(aa.last < bb.decl || bb.last < aa.decl) {
                        return true;
                    }
                } else if owners_interfere(*owner_a, *owner_b) {
                    return true;
                }
            }
        }
        false
    };

    let mut adjacency = nodes
        .iter()
        .copied()
        .map(|bid| (bid, HashSet::<BindingId>::new()))
        .collect::<HashMap<_, _>>();
    for i in 0..nodes.len() {
        for j in i + 1..nodes.len() {
            if bindings_interfere(nodes[i], nodes[j]) {
                #[expect(
                    clippy::unwrap_used,
                    reason = "The adjacency map was constructed from this exact node list immediately before the pair loop"
                )]
                adjacency.get_mut(&nodes[i]).unwrap().insert(nodes[j]);
                #[expect(
                    clippy::unwrap_used,
                    reason = "The adjacency map was constructed from this exact node list immediately before the pair loop"
                )]
                adjacency.get_mut(&nodes[j]).unwrap().insert(nodes[i]);
            }
        }
    }

    let mut colors = HashMap::<BindingId, usize>::new();
    let mut uncolored = nodes.iter().copied().collect::<HashSet<_>>();
    while !uncolored.is_empty() {
        let saturation = |bid: BindingId, colors: &HashMap<BindingId, usize>| {
            adjacency[&bid]
                .iter()
                .filter_map(|neighbor| colors.get(neighbor).copied())
                .collect::<HashSet<_>>()
                .len()
        };
        let mut options = uncolored.iter().copied().collect::<Vec<_>>();
        // JS stable sort preserves insertion order on exact ties. `nodes` order is
        // scratch insertion then temporaries Set insertion; reproduce with position.
        let position = nodes
            .iter()
            .enumerate()
            .map(|(index, bid)| (*bid, index))
            .collect::<HashMap<_, _>>();
        options.sort_by(|a, b| {
            saturation(*b, &colors)
                .cmp(&saturation(*a, &colors))
                .then_with(|| adjacency[b].len().cmp(&adjacency[a].len()))
                .then_with(|| {
                    res.bindings[*b as usize]
                        .freq
                        .cmp(&res.bindings[*a as usize].freq)
                })
                .then_with(|| position[a].cmp(&position[b]))
        });
        let bid = options[0];
        let blocked = adjacency[&bid]
            .iter()
            .filter_map(|neighbor| colors.get(neighbor).copied())
            .collect::<HashSet<_>>();
        let mut color = 0usize;
        while blocked.contains(&color) {
            color += 1;
        }
        colors.insert(bid, color);
        uncolored.remove(&bid);
    }

    let mut target = clone_ast(&source);
    let mut color_symbols = HashMap::new();
    for color in colors.values().copied().collect::<HashSet<_>>() {
        color_symbols.insert(color, target.strings.intern(&format!("__gs{color}")));
    }
    for node in 0..source.nodes.len() as NodeId {
        if matches!(source.node(node), Node::Name(_)) {
            if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
                if let Some(color) = colors.get(&bid) {
                    target.nodes.rewrite(
                        node,
                        Node::Name(color_symbols[color]),
                        "temporary-global-packing",
                    );
                    target.nodes.copy_name_from(
                        node,
                        storm_lua_syntax::NameSite::Reference,
                        &source.nodes,
                        node,
                        storm_lua_syntax::NameSite::Reference,
                    );
                }
            }
        }
    }
    let slots = colors.values().copied().collect::<HashSet<_>>().len();
    *ast = target;
    PassResult {
        root,
        saved: None,
        details: Some(vec![format!("packed={};slots={slots}", temporaries.len())]),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    #[test]
    fn packs_function_confined_globals() {
        let source = "function a()x=1 y=x+1 output.setNumber(1,y)end function b()z=2 w=z+1 output.setNumber(2,w)end";
        let (mut ast, root) = parse_source(source).unwrap();
        let result = pack_temporary_globals(&mut ast, root);
        assert!(result.details.as_ref().unwrap()[0].contains("packed="));
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("__gs"), "{out}");
    }
}
