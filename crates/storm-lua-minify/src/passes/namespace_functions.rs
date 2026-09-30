//! Scalar replacement of private, once-created module namespaces.
//!
//! Unlike field renaming this removes the namespace container and its aliases.
//! Only unconditional chunk-level empty tables, single-definition aliases, and
//! static fields are admitted. Every bare namespace reference must be an alias
//! definition or a proven redundant nil guard. Functions/values themselves are
//! still allocated/assigned at their original sites, in the original order.
//! This runs AFTER the ordinary optimizer: exposing direct functions must not
//! accidentally enable an unrelated speculative inlining transformation.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::pass::PassResult;
use crate::scope_rename::scope_rename_fast;
use storm_lua_analysis::resolver::{resolve, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId};
use storm_lua_syntax::numeric::decode_lua_string;
use storm_lua_syntax::size::measure_size;

#[derive(Clone, Copy)]
struct Definition {
    statement: NodeId,
    source: Option<BindingId>,
    order: usize,
    assignment: bool,
}

fn bid(ast: &Ast, res: &Resolution, node: NodeId) -> Option<BindingId> {
    if matches!(ast.node(node), Node::Name(_)) {
        res.node_bid[node as usize]
    } else {
        None
    }
}

fn equality_nil(ast: &Ast, res: &Resolution, node: NodeId) -> Option<BindingId> {
    let Node::Bin(op, a, b) = ast.node(node) else {
        return None;
    };
    if op != "==" {
        return None;
    }
    if matches!(ast.node(*b), Node::Nil) {
        bid(ast, res, *a)
    } else if matches!(ast.node(*a), Node::Nil) {
        bid(ast, res, *b)
    } else {
        None
    }
}

// Both linker form `if x==nil then x=true end` and the existing conditional
// lowering's `x=x==nil and true or x` have no effect once x is a known table.
fn nil_guard(ast: &Ast, res: &Resolution, node: NodeId) -> Option<BindingId> {
    match ast.node(node) {
        Node::If(arms, None) if arms.len() == 1 => {
            let b = equality_nil(ast, res, arms[0].cond)?;
            let Node::Block(stmts) = ast.node(arms[0].body) else {
                return None;
            };
            if stmts.len() != 1 {
                return None;
            }
            let Node::Assign(targets, values) = ast.node(stmts[0]) else {
                return None;
            };
            (targets.len() == 1
                && values.len() == 1
                && bid(ast, res, targets[0]) == Some(b)
                && matches!(ast.node(values[0]), Node::Bool(true)))
            .then_some(b)
        }
        Node::Assign(targets, values) if targets.len() == 1 && values.len() == 1 => {
            let b = bid(ast, res, targets[0])?;
            let Node::Bin(op, left, right) = ast.node(values[0]) else {
                return None;
            };
            if op != "or" || bid(ast, res, *right) != Some(b) {
                return None;
            }
            let Node::Bin(op, cond, value) = ast.node(*left) else {
                return None;
            };
            (op == "and"
                && equality_nil(ast, res, *cond) == Some(b)
                && matches!(ast.node(*value), Node::Bool(true)))
            .then_some(b)
        }
        _ => None,
    }
}

struct Inventory {
    order: HashMap<NodeId, usize>,
    unconditional: HashSet<NodeId>,
    definitions: BTreeMap<BindingId, Definition>,
    guards: HashMap<NodeId, BindingId>,
    guard_counts: HashMap<BindingId, u32>,
    chunk_locals: usize,
    unsupported_control: bool,
}

impl Inventory {
    fn new() -> Self {
        Self {
            order: HashMap::new(),
            unconditional: HashSet::new(),
            definitions: BTreeMap::new(),
            guards: HashMap::new(),
            guard_counts: HashMap::new(),
            chunk_locals: 0,
            unsupported_control: false,
        }
    }

    fn visit(&mut self, ast: &Ast, res: &Resolution, node: NodeId, top: bool, depth: usize) {
        let position = self.order.len();
        self.order.entry(node).or_insert(position);
        if top {
            self.unconditional.insert(node);
        }
        if depth == 0 {
            self.chunk_locals += match ast.node(node) {
                Node::Local(names, _) | Node::Forin(names, ..) => names.len(),
                Node::Localfunc(..) | Node::Fornum(..) => 1,
                _ => 0,
            };
        }
        // Jumps could bypass an otherwise textually unconditional definition.
        self.unsupported_control |= matches!(ast.node(node), Node::Goto(_) | Node::Label(_));
        if top {
            if let Some(b) = nil_guard(ast, res, node) {
                self.guards.insert(node, b);
                *self.guard_counts.entry(b).or_default() += 1;
                // Still record the guard subtree's order, but not its writes as
                // independent alias definitions.
                storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
                    self.visit(ast, res, child, false, depth)
                });
                return;
            }
            let candidate = match ast.node(node) {
                Node::Local(names, values) if names.len() == 1 && values.len() == 1 => res
                    .node_bids[node as usize]
                    .first()
                    .map(|&b| (b, values[0], false)),
                Node::Assign(targets, values) if targets.len() == 1 && values.len() == 1 => {
                    bid(ast, res, targets[0]).map(|b| (b, values[0], true))
                }
                _ => None,
            };
            if let Some((b, value, assignment)) = candidate {
                let binding = res.binding(b);
                let uninitialized = !assignment
                    || binding.kind == BindingKind::Global
                    || binding.decl_node.is_some_and(
                        |decl| matches!(ast.node(decl),Node::Local(_,values) if values.is_empty()),
                    );
                if uninitialized && matches!(binding.kind, BindingKind::Local | BindingKind::Global)
                {
                    let source = bid(ast, res, value);
                    if source.is_some()
                        || matches!(ast.node(value),Node::Table(fields) if fields.is_empty())
                    {
                        self.definitions.insert(
                            b,
                            Definition {
                                statement: node,
                                source,
                                order: position,
                                assignment,
                            },
                        );
                    }
                }
            }
        }
        match ast.node(node) {
            Node::Block(stmts) => {
                for &stmt in stmts {
                    self.visit(ast, res, stmt, top, depth);
                }
            }
            Node::Do(body) => self.visit(ast, res, *body, top, depth),
            Node::Function(_, _, body) => self.visit(ast, res, *body, false, depth + 1),
            _ => storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
                self.visit(ast, res, child, false, depth)
            }),
        }
    }
}

#[derive(Clone)]
struct FieldSite {
    node: NodeId,
    owner: BindingId,
    key: String,
}

struct Uses<'a> {
    ast: &'a Ast,
    res: &'a Resolution,
    inventory: &'a Inventory,
    owners: &'a BTreeMap<BindingId, BindingId>,
    definition_sites: HashSet<NodeId>,
    bad: BTreeSet<BindingId>,
    sites: Vec<FieldSite>,
    stores: Vec<(NodeId, BindingId, String)>,
}

impl Uses<'_> {
    fn namespace(&self, node: NodeId) -> Option<(BindingId, BindingId)> {
        let b = bid(self.ast, self.res, node)?;
        Some((b, *self.owners.get(&b)?))
    }

    fn visit(&mut self, node: NodeId, write: bool, store: Option<NodeId>) {
        // Only these exact statements may use a namespace as a bare value.
        if self
            .inventory
            .guards
            .get(&node)
            .is_some_and(|b| self.owners.contains_key(b))
        {
            return;
        }
        if self.definition_sites.contains(&node) {
            return;
        }
        if let Node::Index(object, key, _) = self.ast.node(node) {
            if let Some((b, owner)) = self.namespace(*object) {
                if self.inventory.order[&node] < self.inventory.definitions[&b].order {
                    self.bad.insert(owner);
                }
                let Node::Str(raw) = self.ast.node(*key) else {
                    self.bad.insert(owner);
                    return;
                };
                let key = decode_lua_string(raw);
                if write {
                    let valid = store.is_some_and(|stmt| {
                        if !self.inventory.unconditional.contains(&stmt) {
                            return false;
                        }
                        match self.ast.node(stmt) {
                            Node::Funcstat(..) => true,
                            Node::Assign(targets, values)
                                if targets.len() == 1 && values.len() == 1 =>
                            {
                                matches!(
                                    self.ast.node(values[0]),
                                    Node::Function(..)
                                        | Node::Num(_)
                                        | Node::Str(_)
                                        | Node::Bool(_)
                                        | Node::Nil
                                )
                            }
                            _ => false,
                        }
                    });
                    if !valid {
                        self.bad.insert(owner);
                    }
                    if let Some(stmt) = store {
                        self.stores.push((stmt, owner, key.clone()));
                    }
                }
                self.sites.push(FieldSite { node, owner, key });
                return;
            }
        }
        if let Some((_, owner)) = self.namespace(node) {
            self.bad.insert(owner); // escape, identity/length test, colon receiver, etc.
            return;
        }
        match self.ast.node(node) {
            Node::Assign(targets, values) => {
                for &target in targets {
                    self.visit(target, true, Some(node));
                }
                for &value in values {
                    self.visit(value, false, None);
                }
            }
            Node::Funcstat(target, function) => {
                self.visit(*target, true, Some(node));
                self.visit(*function, false, None);
            }
            _ => storm_lua_analysis::resolver::for_each_child(self.ast, node, &mut |child| {
                self.visit(child, false, None)
            }),
        }
    }
}

/// References inside a never-observed exported function do not make its
/// private callees live. Track field-reference dependencies, not just calls:
/// escaping a function value or comparing its identity must keep it too.
fn live_fields(
    ast: &Ast,
    root: NodeId,
    eligible: &BTreeSet<BindingId>,
    sites: &[FieldSite],
    stores: &[(NodeId, BindingId, String)],
) -> BTreeSet<(BindingId, String)> {
    type Field = (BindingId, String);
    let site_fields: HashMap<NodeId, Field> = sites
        .iter()
        .filter(|s| eligible.contains(&s.owner))
        .map(|s| (s.node, (s.owner, s.key.clone())))
        .collect();
    let store_fields: HashMap<NodeId, Field> = stores
        .iter()
        .filter(|(_, owner, _)| eligible.contains(owner))
        .map(|(stmt, owner, key)| (*stmt, (*owner, key.clone())))
        .collect();
    let mut roots = BTreeSet::new();
    let mut edges = BTreeMap::<Field, BTreeSet<Field>>::new();
    let mut stack = vec![(root, None::<Field>)];
    while let Some((node, context)) = stack.pop() {
        if let Some(field) = store_fields.get(&node) {
            // Eligibility already proves that this store's RHS is a function
            // literal or scalar literal. Creating a closure does not execute
            // its body. Preserve dependencies for every redefinition of a field.
            let value = match ast.node(node) {
                Node::Funcstat(_, function) => *function,
                Node::Assign(_, values) => values[0],
                _ => unreachable!("eligible field stores are assignments"),
            };
            if matches!(ast.node(value), Node::Function(..)) {
                stack.push((value, Some(field.clone())));
            }
            continue;
        }
        if let Some(field) = site_fields.get(&node) {
            if let Some(owner) = context {
                edges.entry(owner).or_default().insert(field.clone());
            } else {
                roots.insert(field.clone());
            }
            continue;
        }
        storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
            stack.push((child, context.clone()));
        });
    }
    let mut pending = roots.iter().cloned().collect::<Vec<_>>();
    while let Some(field) = pending.pop() {
        for dependency in edges.get(&field).into_iter().flatten() {
            if roots.insert(dependency.clone()) {
                pending.push(dependency.clone());
            }
        }
    }
    roots
}

// Removing the namespace/aliases often leaves linker `do` wrappers with no
// locals of their own. Splice only those wrappers, never a local-bearing scope.
// Goto/label are rejected before this cleanup, so no jump boundary is changed.
fn remove_vacuous_scopes(ast: &mut Ast, node: NodeId) {
    let mut children = Vec::new();
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| children.push(child));
    for child in children {
        remove_vacuous_scopes(ast, child);
    }
    let Node::Block(stmts) = ast.node(node).clone() else {
        return;
    };
    let mut flattened = Vec::new();
    let last = stmts.len().saturating_sub(1);
    for (index, stmt) in stmts.into_iter().enumerate() {
        if let Node::Do(body) = ast.node(stmt) {
            if let Node::Block(inner) = ast.node(*body) {
                // A return must remain the last statement of its Lua block.
                // `do return x end; f()` uses the do block for grammar, not
                // variable lifetime; removing it would produce invalid Lua.
                let exposes_early_return = index != last
                    && inner
                        .iter()
                        .any(|&s| matches!(ast.node(s), Node::Return(_)));
                if !exposes_early_return
                    && !inner
                        .iter()
                        .any(|&s| matches!(ast.node(s), Node::Local(..) | Node::Localfunc(..)))
                {
                    flattened.extend_from_slice(inner);
                    continue;
                }
            }
        }
        flattened.push(stmt);
    }
    ast.nodes[node as usize] = Node::Block(flattened);
}

pub fn devirtualize_closed_namespaces(ast: &mut Ast, root: NodeId, rename: bool) -> PassResult {
    let unchanged = || PassResult {
        root,
        saved: Some(0),
        details: None,
    };
    let res = resolve(ast, root);
    let mut inventory = Inventory::new();
    inventory.visit(ast, &res, root, true, 0);
    if inventory.unsupported_control || inventory.definitions.is_empty() {
        return unchanged();
    }
    inventory.definitions.retain(|b, d| {
        let guards = inventory.guard_counts.get(b).copied().unwrap_or(0);
        res.binding_write_counts[*b as usize] == u32::from(d.assignment) + guards
            && inventory
                .guards
                .iter()
                .all(|(stmt, guard_bid)| guard_bid != b || inventory.order[stmt] > d.order)
    });
    let mut owners = BTreeMap::new();
    loop {
        let mut changed = false;
        for (&b, def) in &inventory.definitions {
            if owners.contains_key(&b) {
                continue;
            }
            let owner = match def.source {
                None => Some(b),
                Some(from) => owners
                    .get(&from)
                    .copied()
                    .filter(|_| inventory.definitions[&from].order < def.order),
            };
            if let Some(owner) = owner {
                owners.insert(b, owner);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    if owners.is_empty() {
        return unchanged();
    }
    let mut uses = Uses {
        ast,
        res: &res,
        inventory: &inventory,
        owners: &owners,
        definition_sites: inventory
            .definitions
            .iter()
            .filter(|(b, _)| owners.contains_key(b))
            .map(|(_, d)| d.statement)
            .collect(),
        bad: BTreeSet::new(),
        sites: Vec::new(),
        stores: Vec::new(),
    };
    uses.visit(root, false, None);
    let eligible = owners
        .values()
        .copied()
        .filter(|b| !uses.bad.contains(b))
        .collect::<BTreeSet<_>>();
    if eligible.is_empty() || !super::closed_fields::has_closed_key_space(ast, root) {
        return unchanged();
    }
    let retained = live_fields(ast, root, &eligible, &uses.sites, &uses.stores)
        .into_iter()
        .collect::<Vec<_>>();
    // Conservative Lua 5.3 register/upvalue budget. Count all chunk-local
    // declarations (even disjoint do blocks) rather than risking >200 locals.
    // A namespace upvalue can expand into several scalar upvalues. Bound the
    // entire lexical ancestor chain, not just this function's own registers.
    let mut inherited_locals = vec![0usize; res.scopes.len()];
    for scope in &res.scopes {
        inherited_locals[scope.id as usize] = scope
            .parent
            .map_or(0, |parent| inherited_locals[parent as usize])
            + scope
                .bindings
                .iter()
                .filter(|&&b| res.binding(b).kind != BindingKind::Global)
                .count();
    }
    if inherited_locals.iter().copied().max().unwrap_or(0) + retained.len() > 245 {
        return unchanged();
    }
    if inventory.chunk_locals + retained.len() > 180 || retained.len() > 100 {
        return unchanged();
    }
    let mut trial = ast.clone();
    let mut names = HashMap::<(BindingId, String), SymbolId>::new();
    let mut taken = ast
        .strings
        .all_strings()
        .into_iter()
        .collect::<HashSet<_>>();
    let mut serial = 0;
    let mut symbols = Vec::new();
    for field in &retained {
        let spelling = loop {
            let candidate = format!("__stormmin_ns_{serial}");
            serial += 1;
            if taken.insert(candidate.clone()) {
                break candidate;
            }
        };
        let symbol = trial.strings.intern(&spelling);
        names.insert(field.clone(), symbol);
        symbols.push(symbol);
    }
    let mut removed = HashSet::new();
    for (b, def) in &inventory.definitions {
        if owners.get(b).is_some_and(|owner| eligible.contains(owner)) {
            removed.insert(def.statement);
        }
    }
    for (stmt, b) in &inventory.guards {
        if owners.get(b).is_some_and(|owner| eligible.contains(owner)) {
            removed.insert(*stmt);
        }
    }
    let mut dropped_fields = 0;
    for (stmt, owner, key) in &uses.stores {
        if eligible.contains(owner) && !names.contains_key(&(*owner, key.clone())) {
            removed.insert(*stmt);
            dropped_fields += 1;
        }
    }
    for site in &uses.sites {
        if let Some(symbol) = names.get(&(site.owner, site.key.clone())) {
            trial.nodes[site.node as usize] = Node::Name(*symbol);
        }
    }
    // Namespace predeclarations have no values/effects. Remove only their
    // eligible bindings; preserve unrelated names and all other declarations.
    for &node in inventory.order.keys() {
        if let Node::Local(symbols, values) = ast.node(node) {
            if values.is_empty() {
                let kept = symbols
                    .iter()
                    .zip(&res.node_bids[node as usize])
                    .filter(|(_, b)| !owners.get(b).is_some_and(|owner| eligible.contains(owner)))
                    .map(|(sym, _)| *sym)
                    .collect::<Vec<_>>();
                if kept.is_empty() {
                    removed.insert(node);
                } else {
                    trial.nodes[node as usize] = Node::Local(kept, Vec::new());
                }
            }
        }
    }
    trial.nodes.retain_block_statements(
        |statement| !removed.contains(&statement),
        "namespace-declaration-removal",
    );
    if !symbols.is_empty() {
        let declaration = trial.push(Node::Local(symbols, Vec::new()));
        let Node::Block(stmts) = &mut trial.nodes[root as usize] else {
            return unchanged();
        };
        stmts.insert(0, declaration);
    }
    remove_vacuous_scopes(&mut trial, root);
    let before = measure_size(ast, root);
    if rename {
        trial = scope_rename_fast(&trial, root).ast;
    }
    let after = measure_size(&trial, root);
    if after >= before {
        return unchanged();
    }
    *ast = trial;
    PassResult {
        root,
        saved: Some((before - after) as u64),
        details: Some(vec![format!(
            "namespaces={};fields={};unused_stores={dropped_fields}",
            eligible.len(),
            retained.len()
        )]),
    }
}
