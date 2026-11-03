//! Function-local globalization (`scope-and-api.ts::globalizeFunctionLocals`).
//!
//! Eligible function-owned locals become shared `__fsN` scratch globals. Slots
//! are colored by source live intervals within each function and then packed
//! across non-interfering functions using the call graph. Loop-body locals get
//! dedicated colors because linear source intervals do not model backedges.

use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, BindingKind, Resolution, ScopeId};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId};
use storm_lua_syntax::size::measure_size;

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

/// A converted declaration becomes a single assignment statement, and
/// the Lua parser consumes an assignment's target list recursively:
/// each target costs roughly one parser C level against a limit of
/// 200, and each enclosing function costs about one more (measured:
/// a 199-target conversion inside a top-level function fails to load
/// in plain Lua while 198 loads; one more enclosing function lowers
/// the boundary to 198). Cap the targets per converted declaration
/// well below the boundary so the generated assignment stays loadable
/// at any realistic nesting depth (candidate J, w9 cycle 64).
const MAX_GLOBALIZED_DECLARATION_TARGETS: usize = 150;

fn is_compiler_scratch_name(name: &str) -> bool {
    let suffix = name
        .strip_prefix("__fs")
        .or_else(|| name.strip_prefix("__s"));
    suffix.is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn owner_of_scope(
    resolution: &Resolution,
    function_scopes: &HashSet<ScopeId>,
    scope: ScopeId,
) -> ScopeId {
    let mut current = Some(scope);
    while let Some(scope) = current {
        if function_scopes.contains(&scope) {
            return scope;
        }
        current = resolution.scope(scope).parent;
    }
    0
}

fn insert_unique<T: PartialEq + Copy>(values: &mut Vec<T>, value: T) {
    if !values.contains(&value) {
        values.push(value);
    }
}

struct GraphInfo {
    reference_owners: HashMap<BindingId, Vec<ScopeId>>,
    calls: HashMap<ScopeId, Vec<ScopeId>>,
    unknown_call_owners: HashSet<ScopeId>,
}

/// Record one invocation of the callable value produced by `function` from
/// `owner`: an edge when it resolves to a chunk-defined function that cannot
/// have been reassigned, silence for builtin references, and an unknown-call
/// mark otherwise. Used for both explicit Call nodes and the implicit
/// iterator call of a generic-for, which interleaves with the loop body
/// exactly like an explicit call.
fn scan_call_target(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    function: NodeId,
    owner: ScopeId,
    graph: &mut GraphInfo,
) {
    let mut known = false;
    if matches!(ast.node(function), Node::Name(_)) {
        if let Some(bid) = resolution
            .node_bid
            .get(function as usize)
            .copied()
            .flatten()
        {
            let binding = resolution.binding(bid);
            // A function declaration is not a call-target proof once that
            // binding can be reassigned to a different function.
            let definition_writes = u32::from(binding.kind == BindingKind::Global);
            if let Some(function_node) = binding
                .function_node
                .filter(|_| resolution.binding_write_counts[bid as usize] == definition_writes)
            {
                if let Some(callee) = resolution
                    .node_scope_id
                    .get(function_node as usize)
                    .copied()
                    .flatten()
                {
                    insert_unique(graph.calls.entry(owner).or_default(), callee);
                    known = true;
                }
            } else if analyzer.resolve_builtin_reference(function).is_some() {
                known = true;
            }
        } else if analyzer.resolve_builtin_reference(function).is_some() {
            known = true;
        }
    } else if analyzer.resolve_builtin_reference(function).is_some() {
        known = true;
    }
    if !known {
        graph.unknown_call_owners.insert(owner);
    }
}

fn scan_graph(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    owner: ScopeId,
    graph: &mut GraphInfo,
) {
    if let Node::Function(_, _, body) = ast.node(node) {
        let function_owner = resolution
            .node_scope_id
            .get(node as usize)
            .copied()
            .flatten()
            .unwrap_or(owner);
        scan_graph(ast, resolution, analyzer, *body, function_owner, graph);
        return;
    }

    if matches!(ast.node(node), Node::Name(_)) {
        if let Some(bid) = resolution.node_bid.get(node as usize).copied().flatten() {
            insert_unique(graph.reference_owners.entry(bid).or_default(), owner);
        }
    }

    if let Node::Call(function, _, _) = ast.node(node) {
        scan_call_target(ast, resolution, analyzer, *function, owner, graph);
    }

    if let Node::Forin(_, expressions, _) = ast.node(node) {
        // The generic-for protocol calls the iterator value once per step.
        // Without recording that hidden call, the caller looks
        // non-interfering with the iterator's owner, and slot packing can
        // merge a caller local that is live across the loop with one of the
        // iterator's own globalized locals.
        if let Some(iterator) = expressions.first() {
            if matches!(ast.node(*iterator), Node::Call(..)) {
                // A call result used as the iterator is an arbitrary
                // function value; the call node itself is recorded when
                // the walk reaches it, but invoking its result is unknown
                // unless the call is a builtin whose result is a standard
                // iterator (pairs and friends).
                let builtin = match ast.node(*iterator) {
                    Node::Call(function, _, _) => {
                        analyzer.resolve_builtin_reference(*function).is_some()
                    }
                    _ => false,
                };
                if !builtin {
                    graph.unknown_call_owners.insert(owner);
                }
            } else {
                scan_call_target(ast, resolution, analyzer, *iterator, owner, graph);
            }
        }
    }

    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        scan_graph(ast, resolution, analyzer, child, owner, graph)
    });
}

fn reaches(
    calls: &HashMap<ScopeId, Vec<ScopeId>>,
    start: ScopeId,
    current: ScopeId,
    seen: &mut HashSet<ScopeId>,
) -> bool {
    for next in calls.get(&current).into_iter().flatten().copied() {
        if next == start {
            return true;
        }
        if seen.insert(next) && reaches(calls, start, next, seen) {
            return true;
        }
    }
    false
}

fn can_reach(calls: &HashMap<ScopeId, Vec<ScopeId>>, from: ScopeId, target: ScopeId) -> bool {
    let mut seen = HashSet::from([from]);
    let mut stack = vec![from];
    while let Some(current) = stack.pop() {
        for next in calls.get(&current).into_iter().flatten().copied() {
            if next == target {
                return true;
            }
            if seen.insert(next) {
                stack.push(next);
            }
        }
    }
    false
}

fn scan_loop_bids(
    ast: &Ast,
    resolution: &Resolution,
    node: NodeId,
    inside_loop: bool,
    out: &mut HashSet<BindingId>,
) {
    if let Node::Function(_, _, body) = ast.node(node) {
        scan_loop_bids(ast, resolution, *body, false, out);
        return;
    }
    if matches!(ast.node(node), Node::Local(..)) && inside_loop {
        for bid in resolution
            .node_bids
            .get(node as usize)
            .into_iter()
            .flatten()
            .copied()
        {
            out.insert(bid);
        }
    }
    let child_inside = inside_loop
        || matches!(
            ast.node(node),
            Node::Fornum(..) | Node::Forin(..) | Node::While(..) | Node::Repeat(..)
        );
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        scan_loop_bids(ast, resolution, child, child_inside, out)
    });
}

fn intervals_overlap(resolution: &Resolution, a: BindingId, b: BindingId) -> bool {
    let a = resolution.binding(a);
    let b = resolution.binding(b);
    a.group == b.group || !(a.last < b.decl || b.last < a.decl)
}

struct Rewrite<'a> {
    source: &'a Ast,
    resolution: &'a Resolution,
    targets: &'a HashSet<BindingId>,
    slot_by_bid: &'a HashMap<BindingId, usize>,
    slot_symbols: &'a HashMap<usize, SymbolId>,
}

impl<'a> Rewrite<'a> {
    fn rewrite(&self, target: &mut Ast, node: NodeId) -> NodeId {
        if matches!(self.source.node(node), Node::Name(_)) {
            if let Some(bid) = self
                .resolution
                .node_bid
                .get(node as usize)
                .copied()
                .flatten()
            {
                if self.targets.contains(&bid) {
                    let rewritten =
                        target.push(Node::Name(self.slot_symbols[&self.slot_by_bid[&bid]]));
                    target.nodes.derive_from(
                        rewritten,
                        &self.source.nodes,
                        node,
                        "function-local-globalization",
                    );
                    target.nodes.copy_name_from(
                        rewritten,
                        storm_lua_syntax::NameSite::Reference,
                        &self.source.nodes,
                        node,
                        storm_lua_syntax::NameSite::Reference,
                    );
                    return rewritten;
                }
            }
        }

        if let Node::Local(_, expressions) = self.source.node(node) {
            let bids = self
                .resolution
                .node_bids
                .get(node as usize)
                .cloned()
                .unwrap_or_default();
            if !bids.is_empty() && bids.iter().all(|bid| self.targets.contains(bid)) {
                let mut values = expressions
                    .iter()
                    .map(|expression| self.rewrite(target, *expression))
                    .collect::<Vec<_>>();
                if values.is_empty() {
                    let implicit = target.push(Node::Nil);
                    target
                        .nodes
                        .mark_synthetic(implicit, "globalized-local-implicit-nil");
                    values.push(implicit);
                }
                let names = bids
                    .iter()
                    .enumerate()
                    .map(|(index, bid)| {
                        let name =
                            target.push(Node::Name(self.slot_symbols[&self.slot_by_bid[bid]]));
                        target.nodes.derive_from(
                            name,
                            &self.source.nodes,
                            node,
                            "function-local-globalization",
                        );
                        target.nodes.copy_name_from(
                            name,
                            storm_lua_syntax::NameSite::Reference,
                            &self.source.nodes,
                            node,
                            storm_lua_syntax::NameSite::Binding(index as u32),
                        );
                        name
                    })
                    .collect();
                target.nodes.rewrite(
                    node,
                    Node::Assign(names, values),
                    "function-local-globalization",
                );
                return node;
            }
        }

        let original = self.source.node(node).clone();
        let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
            self.rewrite(target, child)
        });
        target
            .nodes
            .rewrite(node, mapped, "function-local-globalization");
        node
    }
}

pub fn globalize_function_locals(
    ast: &mut Ast,
    root: NodeId,
    minimum_frequency: u32,
) -> PassResult {
    if super::has_unstructured_flow(ast, root) {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }

    let original_size = measure_size(ast, root);
    let source = clone_ast(ast);
    // Generated slots live in the global namespace.  If the user already owns
    // one of the reserved spellings, using it would alias unrelated state;
    // reject the whole transformation rather than attempting fragile renaming
    // after resolution.
    if source.strings.iter().any(is_compiler_scratch_name) {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let resolution = resolve(&source, root);
    let analyzer = EffectAnalyzer::new(&source, &resolution, root, true);

    // Function scopes in AST walk order; membership is separate for owner lookup.
    let mut function_scopes = Vec::<ScopeId>::new();
    storm_lua_syntax::ast_utils::walk(&source, root, &mut |node| {
        if matches!(source.node(node), Node::Function(..)) {
            if let Some(scope) = resolution
                .node_scope_id
                .get(node as usize)
                .copied()
                .flatten()
            {
                insert_unique(&mut function_scopes, scope);
            }
        }
    });
    let function_scope_set = function_scopes.iter().copied().collect::<HashSet<_>>();

    let mut owner_by_binding = HashMap::<BindingId, ScopeId>::new();
    for bid in 0..resolution.bindings.len() as BindingId {
        owner_by_binding.insert(
            bid,
            owner_of_scope(
                &resolution,
                &function_scope_set,
                resolution.binding(bid).scope,
            ),
        );
    }

    let mut graph = GraphInfo {
        reference_owners: HashMap::new(),
        calls: HashMap::new(),
        unknown_call_owners: HashSet::new(),
    };
    scan_graph(&source, &resolution, &analyzer, root, 0, &mut graph);

    // An indirect call hidden behind a known callee is still unknown to its
    // caller. Without this closure, A -> B -> table.f can let A and f share
    // scratch slots even though both activations are live at the same time.
    // Propagate the unknown-call barrier backwards through the known graph.
    loop {
        let newly_unknown = graph
            .calls
            .iter()
            .filter_map(|(caller, callees)| {
                (!graph.unknown_call_owners.contains(caller)
                    && callees
                        .iter()
                        .any(|callee| graph.unknown_call_owners.contains(callee)))
                .then_some(*caller)
            })
            .collect::<Vec<_>>();
        if newly_unknown.is_empty() {
            break;
        }
        graph.unknown_call_owners.extend(newly_unknown);
    }

    let mut recursive_owners = HashSet::<ScopeId>::new();
    for owner in &function_scopes {
        let mut seen = HashSet::from([*owner]);
        if reaches(&graph.calls, *owner, *owner, &mut seen) {
            recursive_owners.insert(*owner);
        }
    }

    let mut preliminary = HashSet::<BindingId>::new();
    for bid in 0..resolution.bindings.len() as BindingId {
        let binding = resolution.binding(bid);
        if binding.kind != BindingKind::Local
            || binding.scope == 0
            || binding.freq < minimum_frequency
        {
            continue;
        }
        let owner = owner_by_binding.get(&bid).copied().unwrap_or(0);
        let owners = graph.reference_owners.get(&bid);
        let all_owned = owners
            .map(|owners| owners.iter().all(|ref_owner| *ref_owner == owner))
            .unwrap_or(true);
        if owner != 0
            && !recursive_owners.contains(&owner)
            && !graph.unknown_call_owners.contains(&owner)
            && all_owned
        {
            preliminary.insert(bid);
        }
    }

    // Target Set insertion order is irrelevant after the explicit per-owner sort,
    // but all-or-none local declaration grouping is contractual.
    let mut targets = HashSet::<BindingId>::new();
    storm_lua_syntax::ast_utils::walk(&source, root, &mut |node| {
        if matches!(source.node(node), Node::Local(..)) {
            let bids = resolution
                .node_bids
                .get(node as usize)
                .cloned()
                .unwrap_or_default();
            if !bids.is_empty()
                && bids.len() <= MAX_GLOBALIZED_DECLARATION_TARGETS
                && bids.iter().all(|bid| preliminary.contains(bid))
            {
                targets.extend(bids);
            }
        }
    });
    if targets.is_empty() {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }

    let mut loop_bids = HashSet::<BindingId>::new();
    scan_loop_bids(&source, &resolution, root, false, &mut loop_bids);

    let mut local_color_by_bid = HashMap::<BindingId, usize>::new();
    let mut slot_count_by_owner = HashMap::<ScopeId, usize>::new();
    for owner in &function_scopes {
        let mut bindings = targets
            .iter()
            .copied()
            .filter(|bid| owner_by_binding.get(bid).copied() == Some(*owner))
            .collect::<Vec<_>>();
        bindings.sort_by(|a, b| {
            let a_binding = resolution.binding(*a);
            let b_binding = resolution.binding(*b);
            b_binding
                .freq
                .cmp(&a_binding.freq)
                .then_with(|| a_binding.decl.cmp(&b_binding.decl))
        });
        let mut colors = Vec::<Vec<BindingId>>::new();
        for bid in bindings {
            if loop_bids.contains(&bid) {
                local_color_by_bid.insert(bid, colors.len());
                colors.push(vec![bid]);
                continue;
            }
            let color = colors
                .iter()
                .position(|members| {
                    members.iter().all(|other| {
                        // A dedicated loop color must also stay dedicated
                        // when a less-frequent non-loop binding is colored
                        // later; source intervals do not model backedges.
                        !loop_bids.contains(other) && !intervals_overlap(&resolution, bid, *other)
                    })
                })
                .unwrap_or_else(|| {
                    colors.push(Vec::new());
                    colors.len() - 1
                });
            colors[color].push(bid);
            local_color_by_bid.insert(bid, color);
        }
        if !colors.is_empty() {
            slot_count_by_owner.insert(*owner, colors.len());
        }
    }

    let interferes = |a: ScopeId, b: ScopeId| {
        a == b
            || graph.unknown_call_owners.contains(&a)
            || graph.unknown_call_owners.contains(&b)
            || can_reach(&graph.calls, a, b)
            || can_reach(&graph.calls, b, a)
    };

    let mut owners = slot_count_by_owner.keys().copied().collect::<Vec<_>>();
    owners.sort_by(|a, b| {
        slot_count_by_owner[b]
            .cmp(&slot_count_by_owner[a])
            .then_with(|| a.cmp(b))
    });
    let mut base_by_owner = HashMap::<ScopeId, usize>::new();
    let mut based_owners = Vec::<ScopeId>::new();
    for owner in owners {
        let count = slot_count_by_owner[&owner];
        let mut base = 0usize;
        loop {
            let blocked = based_owners.iter().copied().any(|other| {
                if !interferes(owner, other) {
                    return false;
                }
                let other_base = base_by_owner[&other];
                let other_count = slot_count_by_owner[&other];
                base < other_base + other_count && other_base < base + count
            });
            if !blocked {
                break;
            }
            base += 1;
        }
        base_by_owner.insert(owner, base);
        based_owners.push(owner);
    }

    let mut slot_by_bid = HashMap::<BindingId, usize>::new();
    for bid in &targets {
        let owner = owner_by_binding[bid];
        slot_by_bid.insert(*bid, base_by_owner[&owner] + local_color_by_bid[bid]);
    }

    let max_slot = slot_by_bid.values().copied().max().unwrap_or(0);
    let mut target = clone_ast(&source);
    let mut slot_symbols = HashMap::<usize, SymbolId>::new();
    for slot in 0..=max_slot {
        slot_symbols.insert(slot, target.strings.fresh(&format!("__fs{slot}")));
    }
    let rewrite = Rewrite {
        source: &source,
        resolution: &resolution,
        targets: &targets,
        slot_by_bid: &slot_by_bid,
        slot_symbols: &slot_symbols,
    };
    rewrite.rewrite(&mut target, root);
    let final_size = measure_size(&target, root);
    *ast = target;

    PassResult {
        root,
        saved: Some(original_size.saturating_sub(final_size) as u64),
        details: Some(vec![format!(
            "bindings={};slots={}",
            targets.len(),
            max_slot + 1
        )]),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn run(source: &str, minimum_frequency: u32) -> String {
        let (mut ast, root) = parse_source(source).unwrap();
        globalize_function_locals(&mut ast, root, minimum_frequency);
        Printer::new(&ast, false).output(root)
    }

    #[test]
    fn threshold_retains_rare_local() {
        let source = "function onTick()local frequent=input.getNumber(1)frequent=frequent+1 local rare=input.getNumber(2)output.setNumber(1,frequent)output.setNumber(2,rare)end";
        let all = run(source, 0);
        let hybrid = run(source, 3);
        assert!(!all.contains("local"), "{all}");
        assert!(hybrid.contains("local rare"), "{hybrid}");
        assert!(!hybrid.contains("local frequent"), "{hybrid}");
    }

    #[test]
    fn known_non_recursive_calls_can_still_globalize() {
        let output = run("function leaf(x)local y=x+1 return y end function outer(x)local state=x local z=leaf(x)return state+z end", 0);
        assert!(!output.contains("local state"), "{output}");
    }

    #[test]
    fn generic_for_iterator_locals_get_distinct_slots_from_caller() {
        // The caller invokes the iterator once per step through the
        // generic-for protocol while its own accumulator is live across
        // the loop. That invocation is not a Call node; a call graph
        // built from Call nodes alone judges the two owners
        // non-interfering and merges their slots into one scratch
        // global, so the iterator's local corrupts the accumulator.
        let output = run(
            "function onTick() local t = { 0 } local function it() local x = t[1] + 1 t[1] = x if t[1] > 1 then return nil end return x end local s = 0 for v in it do s = s + 1 end output.setNumber(1, s) end",
            0,
        );
        assert!(output.contains("__fs0"), "{output}");
        assert!(output.contains("__fs1"), "{output}");
        assert!(!output.contains("local s"), "{output}");
        assert!(!output.contains("local x"), "{output}");
    }

    #[test]
    fn builtin_iterator_generic_for_still_globalizes() {
        // A generic-for over a builtin iterator (pairs) performs no
        // user-code call, so globalization is unaffected.
        let output = run(
            "function onTick() local t = { a = 1 } local s = 0 for k, v in pairs(t) do s = s + 1 end output.setNumber(1, s) end",
            0,
        );
        assert!(output.contains("__fs0"), "{output}");
        assert!(!output.contains("local s"), "{output}");
    }

    #[test]
    fn recursive_function_locals_stay_local() {
        let output = run(
            "function f(n)local x=n if n>0 then return f(n-1)+x end return x end",
            0,
        );
        assert!(output.contains("local x"), "{output}");
    }

    // Candidate J (w9 cycle 64): a converted declaration is emitted as
    // one assignment whose target list the Lua parser consumes
    // recursively (~1 C level per target, limit 200). A declaration
    // above the cap keeps its locals instead of becoming an unloadable
    // assignment.
    #[test]
    fn oversized_declaration_stays_local() {
        let names = (0..160)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source =
            format!("local function f(t) local {names};return t.a+t.b end return f({{a=2,b=3}})");
        let output = run(&source, 0);
        assert!(output.contains("local p0"), "{output}");
    }

    #[test]
    fn declaration_at_cap_still_globalizes() {
        let names = (0..150)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source =
            format!("local function f(t) local {names};return t.a+t.b end return f({{a=2,b=3}})");
        let output = run(&source, 0);
        assert!(!output.contains("local p0"), "{output}");
    }
}
