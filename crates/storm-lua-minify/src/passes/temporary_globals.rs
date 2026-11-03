//! Temporary global packing (`passes/temporary-globals.ts`).

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;

use crate::pass::PassResult;
use storm_lua_analysis::resolver::{
    api_roots, resolve, BindingId, BindingKind, Resolution, ScopeId,
};
use storm_lua_syntax::ast::{Ast, Node, NodeId};

/// Fast deterministic hasher for this pass's integer-keyed tables only.
///
/// Every key used below is a `BindingId` / `ScopeId` (`u32`) or a `usize`
/// color. The w7 c86 audit (see
/// `docs/verification/compile-cpu-parent-index-dense-20261101.md`)
/// established that iteration order in this pass is unobservable in the
/// output: results are consumed as set contents, cardinalities and
/// booleans, the DSATUR tie-break is a total order ending in `position`,
/// and the one order-sensitive side effect (interning the `__gs` names)
/// is serialized as strings and is pinned to ascending color order
/// below. This multiply-xor (FxHash-style) hasher therefore only affects
/// speed, replacing the default SipHash (`hash_one` was ~11% of compile
/// self time in the c83 callgrind). It must stay non-cryptographic and
/// dependency-free: these tables are internal to one pass invocation and
/// never cross a trust boundary. The state is a fixed-width `u64`:
/// the seed does not fit a 32-bit `usize` (wasm32), and the 64-bit
/// hash sequence is unchanged.
#[derive(Default)]
struct FastHasher {
    hash: u64,
}

const FAST_HASH_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FastHasher {
    #[inline]
    fn mix(&mut self, value: u64) {
        self.hash = (self.hash.rotate_left(5) ^ value).wrapping_mul(FAST_HASH_SEED);
    }
}

impl Hasher for FastHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(std::mem::size_of::<u64>()) {
            let mut value = 0u64;
            for (index, &byte) in chunk.iter().enumerate() {
                value |= (byte as u64) << (index * 8);
            }
            self.mix(value);
        }
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.mix(value as u64);
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.mix(value);
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.mix(value as u64);
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.mix(value as u64);
    }
}

type FastBuildHasher = BuildHasherDefault<FastHasher>;
type FastMap<K, V> = HashMap<K, V, FastBuildHasher>;
type FastSet<T> = HashSet<T, FastBuildHasher>;

#[derive(Clone, Copy)]
struct Reference {
    owner: ScopeId,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

/// Record one invocation of the callable value produced by `function` from
/// `owner`: an edge when it resolves to a chunk-defined function, silence
/// for fixed/API targets, and an unknown-call mark otherwise. Used for both
/// explicit Call nodes and the implicit iterator call of a generic-for,
/// which interleaves with the loop body exactly like an explicit call.
fn scan_call_target(
    ast: &Ast,
    res: &Resolution,
    function: NodeId,
    owner: ScopeId,
    calls: &mut FastMap<ScopeId, FastSet<ScopeId>>,
    unknown_call_owners: &mut FastSet<ScopeId>,
) {
    let mut known = false;
    match ast.node(function) {
        Node::Name(symbol) => {
            if let Some(bid) = res.node_bid.get(function as usize).copied().flatten() {
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

/// Whether a Call node is a builtin invocation whose result is a standard
/// iterator function, so that using the result as a generic-for iterator
/// performs no user-code call. Mirrors the fixed/API conditions under
/// which `scan_call_target` treats a call target as known, minus the
/// chunk-function case: a chunk function can return any closure.
fn call_result_is_builtin_iterator(ast: &Ast, res: &Resolution, call: NodeId) -> bool {
    let Node::Call(function, _, _) = ast.node(call) else {
        return false;
    };
    match ast.node(*function) {
        Node::Name(symbol) => {
            if let Some(bid) = res.node_bid.get(*function as usize).copied().flatten() {
                let binding = &res.bindings[bid as usize];
                binding.function_node.is_none()
                    && (binding.fixed || api_roots(ast.strings.get(*symbol)))
            } else {
                api_roots(ast.strings.get(*symbol))
            }
        }
        Node::Index(object, _, _) => match ast.node(*object) {
            Node::Name(symbol) => api_roots(ast.strings.get(*symbol)),
            _ => false,
        },
        _ => false,
    }
}

fn scan_owner(
    ast: &Ast,
    res: &Resolution,
    node: NodeId,
    owner: ScopeId,
    references: &mut FastMap<BindingId, Vec<Reference>>,
    calls: &mut FastMap<ScopeId, FastSet<ScopeId>>,
    unknown_call_owners: &mut FastSet<ScopeId>,
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
        scan_call_target(ast, res, *function, owner, calls, unknown_call_owners);
    }
    if let Node::Forin(_, expressions, _) = ast.node(node) {
        // The generic-for protocol calls the iterator value once per step.
        // Without recording that hidden call, an owner joined to the
        // iterator's owner only by a generic-for looks non-interfering, and
        // packing can merge two values that are both live across the loop
        // (a caller slot live over the loop and the iterator's own slots).
        if let Some(iterator) = expressions.first() {
            if matches!(ast.node(*iterator), Node::Call(..)) {
                // A call result used as the iterator is an arbitrary
                // function value; the call node itself is recorded when
                // the walk reaches it, but invoking its result is unknown
                // unless the call is a builtin whose result is a standard
                // iterator (pairs and friends).
                if !call_result_is_builtin_iterator(ast, res, *iterator) {
                    unknown_call_owners.insert(owner);
                }
            } else {
                scan_call_target(ast, res, *iterator, owner, calls, unknown_call_owners);
            }
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
    assigned: &FastSet<BindingId>,
    confined: &FastMap<BindingId, ScopeId>,
    unsafe_bids: &mut FastSet<BindingId>,
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

fn intersection(sets: &[FastSet<BindingId>]) -> FastSet<BindingId> {
    let Some(first) = sets.first() else {
        return FastSet::default();
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
    input: &FastSet<BindingId>,
    confined: &FastMap<BindingId, ScopeId>,
    unsafe_bids: &mut FastSet<BindingId>,
) -> FastSet<BindingId> {
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

fn can_reach(calls: &FastMap<ScopeId, FastSet<ScopeId>>, from: ScopeId, target: ScopeId) -> bool {
    let mut seen = FastSet::default();
    seen.insert(from);
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

/// Collect bindings referenced inside a loop body of one function body.
///
/// Linear source intervals do not model loop backedges: a binding read
/// inside a loop is read again on the next iteration, after any assignment
/// in the loop body. Such bindings must never share a packing slot with
/// another binding of the same function (see `function_globalization`'s
/// dedicated loop colors for the same reason). Nested functions are not
/// descended into; each function body is scanned under its own owner.
/// Loop bounds (Fornum/Forin start/end/step and iterator expressions) are
/// evaluated once, so only loop bodies and While/Repeat conditions count
/// as inside the loop.
fn scan_loop_involved(
    ast: &Ast,
    res: &Resolution,
    node: NodeId,
    inside_loop: bool,
    out: &mut FastSet<BindingId>,
) {
    match ast.node(node) {
        Node::Function(..) => {
            return;
        }
        Node::While(condition, body) => {
            scan_loop_involved(ast, res, *condition, true, out);
            scan_loop_involved(ast, res, *body, true, out);
            return;
        }
        Node::Repeat(body, condition) => {
            scan_loop_involved(ast, res, *body, true, out);
            scan_loop_involved(ast, res, *condition, true, out);
            return;
        }
        Node::Fornum(_, start, end, step, body) => {
            scan_loop_involved(ast, res, *start, inside_loop, out);
            scan_loop_involved(ast, res, *end, inside_loop, out);
            if let Some(step) = step {
                scan_loop_involved(ast, res, *step, inside_loop, out);
            }
            scan_loop_involved(ast, res, *body, true, out);
            return;
        }
        Node::Forin(_, expressions, body) => {
            for expression in expressions {
                scan_loop_involved(ast, res, *expression, inside_loop, out);
            }
            scan_loop_involved(ast, res, *body, true, out);
            return;
        }
        _ => {}
    }
    if inside_loop && matches!(ast.node(node), Node::Name(_)) {
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            out.insert(bid);
        }
    }
    let mut children = Vec::new();
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| children.push(child));
    for child in children {
        scan_loop_involved(ast, res, child, inside_loop, out);
    }
}

pub fn pack_temporary_globals(ast: &mut Ast, root: NodeId) -> PassResult {
    let source = clone_ast(ast);
    let res = resolve(&source, root);

    let mut function_bodies = FastMap::<ScopeId, NodeId>::default();
    for node in 0..source.nodes.len() as NodeId {
        if let Node::Function(_, _, body) = source.node(node) {
            if let Some(scope) = res.node_scope_id.get(node as usize).copied().flatten() {
                function_bodies.insert(scope, *body);
            }
        }
    }

    let mut loop_involved = FastMap::<ScopeId, FastSet<BindingId>>::default();
    for (scope, body) in &function_bodies {
        let mut bids = FastSet::default();
        scan_loop_involved(&source, &res, *body, false, &mut bids);
        loop_involved.insert(*scope, bids);
    }

    let mut references = FastMap::<BindingId, Vec<Reference>>::default();
    let mut calls = FastMap::<ScopeId, FastSet<ScopeId>>::default();
    let mut unknown_call_owners = FastSet::<ScopeId>::default();
    scan_owner(
        &source,
        &res,
        root,
        0,
        &mut references,
        &mut calls,
        &mut unknown_call_owners,
    );

    let mut confined = FastMap::<BindingId, ScopeId>::default();
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
            .collect::<FastSet<_>>();
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

    let mut unsafe_bids = FastSet::default();
    let relevant_owners = confined.values().copied().collect::<FastSet<_>>();
    for owner in relevant_owners {
        if let Some(body) = function_bodies.get(&owner) {
            let _ = analyze_block(
                &source,
                &res,
                *body,
                &FastSet::default(),
                &confined,
                &mut unsafe_bids,
            );
        }
    }
    let temporaries = confined
        .keys()
        .copied()
        .filter(|bid| !unsafe_bids.contains(bid))
        .collect::<FastSet<_>>();
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
                .collect::<FastSet<_>>();
            (bid, owners)
        })
        .collect::<FastMap<_, _>>();

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
                    if loop_involved
                        .get(owner_a)
                        .is_some_and(|bids| bids.contains(&a) || bids.contains(&b))
                    {
                        return true;
                    }
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
        .map(|bid| (bid, FastSet::<BindingId>::default()))
        .collect::<FastMap<_, _>>();
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

    // Targets of a single multi-target Assign share one statement, so their
    // assignments happen together. The linear decl/last interval check above
    // can call them disjoint when one target is never read (its interval ends
    // at its own assignment, before the next target's decl), yet packing them
    // into one slot is unsound: a global multi-assign keeps the FIRST value
    // for a duplicated slot name, so a later use of the second target would
    // read the first target's value. Force interference between bindings
    // assigned by the same Assign statement.
    {
        let mut assign_targets: Vec<Vec<BindingId>> = Vec::new();
        storm_lua_syntax::ast_utils::walk(&source, root, &mut |id| {
            if let Node::Assign(targets, _) = source.node(id) {
                let mut bids = Vec::new();
                for target in targets {
                    if matches!(source.node(*target), Node::Name(_)) {
                        if let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() {
                            if !bids.contains(&bid) {
                                bids.push(bid);
                            }
                        }
                    }
                }
                if bids.len() > 1 {
                    assign_targets.push(bids);
                }
            }
        });
        for bids in assign_targets {
            for i in 0..bids.len() {
                for j in i + 1..bids.len() {
                    if let Some(set) = adjacency.get_mut(&bids[i]) {
                        set.insert(bids[j]);
                    }
                    if let Some(set) = adjacency.get_mut(&bids[j]) {
                        set.insert(bids[i]);
                    }
                }
            }
        }
    }

    let mut colors = FastMap::<BindingId, usize>::default();
    let mut uncolored = nodes.iter().copied().collect::<FastSet<_>>();
    while !uncolored.is_empty() {
        let saturation = |bid: BindingId, colors: &FastMap<BindingId, usize>| {
            adjacency[&bid]
                .iter()
                .filter_map(|neighbor| colors.get(neighbor).copied())
                .collect::<FastSet<_>>()
                .len()
        };
        let mut options = uncolored.iter().copied().collect::<Vec<_>>();
        // JS stable sort preserves insertion order on exact ties. `nodes` order is
        // scratch insertion then temporaries Set insertion; reproduce with position.
        let position = nodes
            .iter()
            .enumerate()
            .map(|(index, bid)| (*bid, index))
            .collect::<FastMap<_, _>>();
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
            .collect::<FastSet<_>>();
        let mut color = 0usize;
        while blocked.contains(&color) {
            color += 1;
        }
        colors.insert(bid, color);
        uncolored.remove(&bid);
    }

    let mut target = clone_ast(&source);
    // One transformation code string for every rewritten name in this pass.
    // The plain rewrite API would otherwise allocate a fresh code string
    // per node; the recorded origins are identical either way.
    let packing_code: Arc<str> = Arc::from("temporary-global-packing");
    let mut color_symbols = FastMap::default();
    // Intern colors in ascending order. Which SymbolId each `__gs` name
    // receives is not observable in the output (names are serialized as
    // strings), but pinning the order removes the one remaining
    // iteration-order side effect in this pass (c86 audit precondition
    // P2; see docs/verification/compile-cpu-parent-index-dense-20261101.md).
    let mut used_colors = colors.values().copied().collect::<Vec<_>>();
    used_colors.sort_unstable();
    used_colors.dedup();
    for color in used_colors {
        color_symbols.insert(color, target.strings.fresh(&format!("__gs{color}")));
    }
    for node in 0..source.nodes.len() as NodeId {
        if matches!(source.node(node), Node::Name(_)) {
            if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
                if let Some(color) = colors.get(&bid) {
                    target.nodes.rewrite_with_code(
                        node,
                        Node::Name(color_symbols[color]),
                        &packing_code,
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
    let slots = colors.values().copied().collect::<FastSet<_>>().len();
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
    fn loop_read_and_loop_written_globals_are_not_packed() {
        // A string read inside a loop and a row written inside the same
        // loop look disjoint in linear source intervals (the read precedes
        // the write in traversal order because Assign visits its RHS first),
        // but the string is read again on the next iteration, after the row
        // assignment. Sharing one slot miscompiles: the second iteration
        // indexes the row instead of the string.
        let source = "t = {h = 1, e = 2} function onTick() s = \"hello\" for i = 1, 3 do c = t[string.sub(s, i, i)] output.setNumber(1, c) end end";
        let (mut ast, root) = parse_source(source).unwrap();
        let result = pack_temporary_globals(&mut ast, root);
        assert_eq!(result.details.as_ref().unwrap()[0], "packed=2;slots=2");
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("__gs0"), "{out}");
        assert!(out.contains("__gs1"), "{out}");
    }

    #[test]
    fn straight_line_globals_still_pack() {
        // Without a loop backedge, the same read-before-write shape is safe
        // to pack into one slot; the loop guard must not disable packing.
        let source = "t = {h = 1, e = 2} function onTick() s = \"hello\" c = t[string.sub(s, 1, 1)] output.setNumber(1, c) end";
        let (mut ast, root) = parse_source(source).unwrap();
        let result = pack_temporary_globals(&mut ast, root);
        assert_eq!(result.details.as_ref().unwrap()[0], "packed=2;slots=1");
    }
    #[test]
    fn same_assign_targets_are_not_packed() {
        // Two globals assigned by one multi-target Assign must not share a
        // slot, even when the first target is never read: its linear interval
        // ends at its own assignment (before the second target's decl), but
        // the assignments happen together, and a global multi-assign keeps
        // the FIRST value for a duplicated slot name. Sharing would make a
        // later use of the second target read the first target's value.
        let source = "function onDraw() a, b = 1, 2 output.setNumber(1, b) end";
        let (mut ast, root) = parse_source(source).unwrap();
        let result = pack_temporary_globals(&mut ast, root);
        assert_eq!(result.details.as_ref().unwrap()[0], "packed=2;slots=2");
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("__gs0"), "{out}");
        assert!(out.contains("__gs1"), "{out}");
    }

    #[test]
    fn same_assign_targets_are_not_packed_when_live_is_first() {
        // The live-first order is harmless by accident (the first value
        // survives), but packing is still refused conservatively so the
        // result does not depend on target order.
        let source = "function onDraw() b, a = 1, 2 output.setNumber(1, b) end";
        let (mut ast, root) = parse_source(source).unwrap();
        let result = pack_temporary_globals(&mut ast, root);
        assert_eq!(result.details.as_ref().unwrap()[0], "packed=2;slots=2");
    }

    #[test]
    fn generic_for_iterator_owner_interferes_with_caller() {
        // The only connection between the two owners is the generic-for:
        // onTick invokes `it` once per step while H is live across the
        // loop. The iterator expression is not a Call node, so a call
        // graph built from Call nodes alone sees no interference and
        // packs G (owned by the iterator) and H (owned by the caller)
        // into one slot; the iterator's writes then corrupt H.
        let source = "function onTick() local t = { 0 } H = 7 local function it() t[1] = t[1] + 1 G = t[1] if t[1] > 1 then return nil end return t[1] end for v in it do end output.setNumber(1, H) end";
        let (mut ast, root) = parse_source(source).unwrap();
        let result = pack_temporary_globals(&mut ast, root);
        assert_eq!(result.details.as_ref().unwrap()[0], "packed=2;slots=2");
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("__gs0"), "{out}");
        assert!(out.contains("__gs1"), "{out}");
    }

    #[test]
    fn generic_for_call_result_iterator_is_an_unknown_call() {
        // The iterator is produced by calling make(); the returned
        // closure's owner is not reachable through the call graph (make
        // never calls it), so only treating the invocation of a call
        // result as an unknown call keeps H and G in separate slots.
        let source = "function make() local t = { 0 } return function() t[1] = t[1] + 1 G = t[1] if t[1] > 1 then return nil end return t[1] end end function onTick() H = 7 for v in make() do end output.setNumber(1, H) end";
        let (mut ast, root) = parse_source(source).unwrap();
        let result = pack_temporary_globals(&mut ast, root);
        assert_eq!(result.details.as_ref().unwrap()[0], "packed=2;slots=2");
    }

    #[test]
    fn builtin_iterator_generic_for_does_not_block_packing() {
        // A generic-for over a builtin iterator (pairs) performs no
        // user-code call, so two genuinely disjoint owners still share
        // one slot.
        let source = "function onTick() local t = { a = 1 } H = 7 for k, v in pairs(t) do end output.setNumber(1, H) end function other() G = 1 output.setNumber(2, G) end";
        let (mut ast, root) = parse_source(source).unwrap();
        let result = pack_temporary_globals(&mut ast, root);
        assert_eq!(result.details.as_ref().unwrap()[0], "packed=2;slots=1");
    }

    #[test]
    fn flg_output_packed_by_tgp_keeps_iterator_and_caller_slots_distinct() {
        // The candidate-C interaction end to end: FLG turns the caller's
        // accumulator into a scratch global, then TGP packs the
        // iterator's never-read global. The two scratch values are both
        // live across the generic-for and must land in distinct slots.
        let source = "function onTick() local t = { 1 } local function it() t[1] = t[1] + 1 G = t[1] if t[1] > 3 then return nil end return t[1] end local s = 0 for v in it do s = s + 1 end output.setNumber(1, s) end";
        let (mut ast, root) = parse_source(source).unwrap();
        crate::passes::function_globalization::globalize_function_locals(&mut ast, root, 0);
        let result = pack_temporary_globals(&mut ast, root);
        assert_eq!(result.details.as_ref().unwrap()[0], "packed=1;slots=2");
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("__gs0"), "{out}");
        assert!(out.contains("__gs1"), "{out}");
    }

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
