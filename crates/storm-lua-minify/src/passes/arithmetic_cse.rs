//! Straight-line arithmetic common-subexpression elimination.
//!
//! Within one straight-line segment (a run of `local` / assignment
//! statements with no control flow and no calls), a repeated pure
//! arithmetic expression is evaluated once into a fresh local carrier
//! and every later occurrence is replaced by that carrier. This removes
//! both characters (when the carrier pays for itself) and executed
//! instructions (one evaluation instead of many), so it is a win-win
//! transform for the VM-speed axis under the character constraint:
//! it never lengthens the code it touches.
//!
//! This pass is intentionally conservative:
//! - Candidate shape: `Bin` with `+`, `-`, `*`, `/`, `^` or `Un` with
//!   `-`, built only from `Num`, `Paren`, and `Name` leaves, with at
//!   least one `Name` leaf. Calls, tables, functions, varargs, index
//!   reads, strings, and booleans never appear in a candidate, so no
//!   evaluation inside a candidate can have a side effect or read
//!   mutable state.
//! - Totality: every `Name` leaf must be proven numeric by the
//!   single-assignment proof below (a `local` binding that is never
//!   reassigned, whose initializer is itself total numeric) or by the
//!   segment-assignment proof (a global name assigned earlier in the
//!   same straight-line segment by an assignment whose right-hand
//!   side is itself total numeric at that point, with no intervening
//!   reassignment, call, or control flow). The segment proof exists
//!   because compiler scratch globals (`__fsN`, produced when
//!   function-owned locals are globalized) have a global binding,
//!   not a single-assignment `local` binding, for the
//!   single-assignment proof to use, even though their
//!   defining assignment dominates later uses in the same segment.
//!   Arithmetic
//!   on numbers never errors for the admitted operators (integer
//!   `+`/`-`/`*` wrap, `/` and `^` produce floats), so evaluating the
//!   candidate once at the carrier definition cannot move, drop, or
//!   change an error. A name of unknown type is never a total operand:
//!   it could make the expression error or invoke an arithmetic
//!   metamethod, and caching it would change that behavior.
//! - Segments: only `local` / assignment statements and call-free
//!   `Return` lists belong to a segment. Control flow, call
//!   statements, returns containing calls, and statements whose
//!   right-hand sides (or assignment targets) contain a call split
//!   the segment, because a call could mutate a name through a
//!   captured closure. A block containing a `goto` / label anywhere
//!   outside a nested function is skipped entirely: unstructured jumps
//!   break the straight-line execution order this pass relies on.
//!   Function bodies are never scanned as part of an enclosing
//!   segment; each function body block is processed on its own.
//! - Invalidation: after a statement's right-hand sides are counted,
//!   every group whose name set contains an assigned or newly bound
//!   name is flushed, so a later occurrence always refers to the same
//!   binding as the earlier ones it shares a carrier with. An
//!   assignment to a non-name target flushes every group, because an
//!   index write could alias any name the candidate reads through.
//!   (Candidates contain no index reads, but the flush-all rule keeps
//!   the invariant trivially true for future shape extensions.)
//! - Occurrence level: candidates are collected at every nesting
//!   level (the walk descends into candidates as well), because the
//!   same shape can occur both exposed and nested inside another
//!   candidate. Groups are then applied largest-saving first, with
//!   occurrences inside, or containing, an already applied occurrence
//!   removed and the gate recomputed on the survivors. A
//!   single-statement inner repeat such as `(expr)^2 + (expr)`
//!   therefore still gets a carrier even though the whole right-hand
//!   side occurs only once. Groups are formed per segment only, never
//!   across a segment boundary.
//! - Profitability gate: a group is applied only when its measured
//!   saving is strictly positive: `n * expr_size` must exceed
//!   `decl_size + 1 + n * name_len`, where `decl_size` is the measured size of the carrier
//!   declaration itself and the `+1` covers one statement separator.
//!   The gate uses the pre-rename carrier spelling, so it stays valid
//!   whether or not scope renaming later shortens the carrier name.
//!   As a final safety net, the whole pass reverts itself when the
//!   measured size of the root grows.
//!
//! The totality proof mirrors the name-operand proof of
//! `loop_invariant` (same admitted host reads and math calls, same
//! single-assignment binding rule), restricted here to the bindings
//! that dominate the segment: locals of enclosing blocks plus locals
//! bound earlier in the same block, plus globals assigned earlier in
//! the same segment (see the segment-assignment rules below). It is
//! deliberately stricter than the shared `numeric_proof` domain,
//! which has no notion of which block a binding belongs to.
//!
//! Segment-assignment rules: a segment binding maps a global spelling
//! to the right-hand-side node of its defining assignment, recorded
//! only when that right-hand side is proven total with the bindings
//! in force just before the assignment and does not mention the
//! assigned name itself (a self-referential right-hand side would
//! make the proof recurse on its own definition, so the binding is
//! dropped instead). The binding is dropped again on any later
//! assignment to the same name, and all segment bindings are dropped
//! on an assignment to a non-name target (an index write could alias
//! a global through `_ENV`). Segment bindings never cross a segment
//! boundary, a function boundary, or a `local` binding of the same
//! spelling: a name bound to a local is always resolved
//! through the single-assignment map, never through the segment map
//! (the resolver gives globals their own global binding kind, which
//! is what selects the segment map here), so a local can never be
//! mistaken for the global of the same
//! spelling.

use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use storm_lua_analysis::effects::{mutated_builtin_roots, EffectAnalyzer};
use storm_lua_analysis::resolver::{resolve, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId};
use storm_lua_syntax::ast_utils::{self, for_each_child};
use storm_lua_syntax::size::{measure_expr, measure_size, measure_stmt};

const REASON: &str = "straightline-arithmetic-cse";
const CARRIER_PREFIX: &str = "__cse";

/// Pure host reads that may appear in a total numeric initializer,
/// with their literal-argument rule (same rule as `loop_invariant`).
fn pure_read_key(source: &Ast, builtin: &str, args: &[NodeId]) -> Option<String> {
    match builtin {
        "input.getNumber" | "property.getNumber" => {
            if args.len() != 1 {
                return None;
            }
            let Node::Num(text) = source.node(args[0]) else {
                return None;
            };
            Some(format!("{builtin}({text})"))
        }
        "screen.getWidth" | "screen.getHeight" => {
            if args.is_empty() {
                Some(builtin.to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn pure_read_root(builtin: &str) -> &str {
    builtin.split('.').next().unwrap_or("")
}

/// Single-value pure math calls that may appear in a total numeric
/// initializer (same list as `loop_invariant`, minus stateful calls).
fn math_arity_ok(builtin: &str, nargs: usize) -> bool {
    match builtin {
        "math.abs" | "math.acos" | "math.asin" | "math.atan" | "math.ceil" | "math.cos"
        | "math.cosh" | "math.deg" | "math.exp" | "math.floor" | "math.log10" | "math.rad"
        | "math.sin" | "math.sinh" | "math.sqrt" | "math.tan" | "math.tanh" => nargs == 1,
        "math.log" => nargs == 1 || nargs == 2,
        "math.atan2" | "math.fmod" | "math.pow" => nargs == 2,
        "math.max" | "math.min" => nargs >= 1,
        _ => false,
    }
}

/// Proof that a node evaluates to a number without erroring, for names
/// bound by single-assignment `local` statements that dominate the
/// point of use. See the module documentation for the dominance rule.
struct TotalProof<'a> {
    source: &'a Ast,
    analyzer: &'a EffectAnalyzer<'a>,
    mutated: &'a HashSet<String>,
    resolution: &'a Resolution,
    bindings: &'a HashMap<BindingId, NodeId>,
    /// Globals assigned earlier in the current segment, by spelling.
    /// Only consulted for global-kind names.
    segment: &'a HashMap<SymbolId, NodeId>,
}

impl<'a> TotalProof<'a> {
    fn is_total_numeric(&self, node: NodeId, depth: usize) -> bool {
        if depth > 32 {
            return false;
        }
        match self.source.node(node) {
            Node::Num(_) => true,
            Node::Paren(inner) => self.is_total_numeric(*inner, depth + 1),
            Node::Name(symbol) => {
                let Some(bid) = self.resolution.node_bid[node as usize] else {
                    return false;
                };
                if self.resolution.binding(bid).kind == BindingKind::Global {
                    // A global name: only a segment binding proves
                    // the value assigned earlier in this segment.
                    let Some(initializer) = self.segment.get(symbol) else {
                        return false;
                    };
                    self.is_total_numeric(*initializer, depth + 1)
                } else {
                    let Some(initializer) = self.bindings.get(&bid) else {
                        return false;
                    };
                    self.is_total_numeric(*initializer, depth + 1)
                }
            }
            Node::Index(..)
                if matches!(
                    self.analyzer.resolve_builtin_reference(node).as_deref(),
                    Some("math.pi" | "math.huge" | "math.maxinteger" | "math.mininteger")
                ) =>
            {
                !self.mutated.contains("math")
            }
            Node::Call(function, args, method) => {
                if method.is_some() {
                    return false;
                }
                let Some(builtin) = self.analyzer.resolve_builtin_reference(*function) else {
                    return false;
                };
                let b: &str = &builtin;
                match b {
                    "input.getNumber" | "property.getNumber" | "screen.getWidth"
                    | "screen.getHeight" => {
                        if self.mutated.contains(pure_read_root(b)) {
                            return false;
                        }
                        pure_read_key(self.source, b, args).is_some()
                    }
                    _ => {
                        if self.mutated.contains("math") || !math_arity_ok(b, args.len()) {
                            return false;
                        }
                        args.iter()
                            .all(|arg| self.is_total_numeric(*arg, depth + 1))
                    }
                }
            }
            Node::Un(op, inner) if op == "-" => self.is_total_numeric(*inner, depth + 1),
            Node::Bin(op, left, right) if matches!(op.as_str(), "+" | "-" | "*" | "/" | "^") => {
                self.is_total_numeric(*left, depth + 1) && self.is_total_numeric(*right, depth + 1)
            }
            _ => false,
        }
    }
}

/// Structural identity of a candidate expression, with the set of name
/// spellings it reads (used for invalidation) and its name-leaf count.
struct CandidateShape {
    key: String,
    names: HashSet<SymbolId>,
    name_count: usize,
}

fn candidate_shape(proof: &TotalProof<'_>, node: NodeId, depth: usize) -> Option<CandidateShape> {
    if depth > 64 {
        return None;
    }
    match proof.source.node(node) {
        Node::Paren(inner) => candidate_shape(proof, *inner, depth + 1),
        Node::Num(raw) => Some(CandidateShape {
            key: format!("N{raw}"),
            names: HashSet::new(),
            name_count: 0,
        }),
        Node::Name(symbol) => {
            if !proof.is_total_numeric(node, 0) {
                return None;
            }
            let spelling = proof.source.strings.get(*symbol).to_string();
            Some(CandidateShape {
                key: format!("V{spelling}"),
                names: HashSet::from([*symbol]),
                name_count: 1,
            })
        }
        Node::Un(op, inner) if op == "-" => {
            let inner_shape = candidate_shape(proof, *inner, depth + 1)?;
            Some(CandidateShape {
                key: format!("(-{})", inner_shape.key),
                names: inner_shape.names,
                name_count: inner_shape.name_count,
            })
        }
        Node::Bin(op, left, right) if matches!(op.as_str(), "+" | "-" | "*" | "/" | "^") => {
            let left_shape = candidate_shape(proof, *left, depth + 1)?;
            let right_shape = candidate_shape(proof, *right, depth + 1)?;
            let mut names = left_shape.names;
            names.extend(right_shape.names);
            Some(CandidateShape {
                key: format!("({}{}{})", left_shape.key, op, right_shape.key),
                names,
                name_count: left_shape.name_count + right_shape.name_count,
            })
        }
        _ => None,
    }
}

/// One repeated expression within a segment.
struct Group {
    names: HashSet<SymbolId>,
    template: NodeId,
    // (occurrence node, statement index), in scan order.
    occurrences: Vec<(NodeId, usize)>,
    size: usize,
}

/// Clone a subtree from `source` into `output` with fresh node ids.
fn clone_subtree(output: &mut Ast, source: &Ast, node: NodeId) -> NodeId {
    let (mapped, _) = ast_utils::map_children(source.node(node), &mut |child| {
        clone_subtree(output, source, child)
    });
    output.push(mapped)
}

fn clone_source(source: &Ast) -> Ast {
    let mut ast = ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

/// Collect the single-assignment `local` bindings that are direct
/// statements of `block` (same rule as `loop_invariant`).
fn bindings_for_body(
    source: &Ast,
    resolution: &Resolution,
    block: NodeId,
) -> HashMap<BindingId, NodeId> {
    let mut bindings = HashMap::new();
    let Node::Block(statements) = source.node(block) else {
        return bindings;
    };
    for statement in statements {
        if let Node::Local(_, expressions) = source.node(*statement) {
            for (bid, expression) in resolution.node_bids[*statement as usize]
                .iter()
                .zip(expressions)
            {
                if resolution.binding_write_counts[*bid as usize] == 0 {
                    bindings.insert(*bid, *expression);
                }
            }
        }
    }
    bindings
}

/// Whether the subtree contains a call, without descending into nested
/// function bodies (calls there do not execute at this point).
fn contains_call(source: &Ast, node: NodeId) -> bool {
    match source.node(node) {
        Node::Function(..) => false,
        Node::Call(..) => true,
        _ => {
            let mut found = false;
            for_each_child(source, node, &mut |child| {
                found |= contains_call(source, child);
            });
            found
        }
    }
}

/// Whether the subtree mentions a `Name` with the given spelling,
/// without descending into nested function bodies (names there refer
/// to a different execution point, and a function value assigned here
/// does not evaluate its body here).
fn subtree_contains_name(source: &Ast, node: NodeId, symbol: SymbolId) -> bool {
    match source.node(node) {
        Node::Function(..) => false,
        Node::Name(other) => *other == symbol,
        _ => {
            let mut found = false;
            for_each_child(source, node, &mut |child| {
                found |= subtree_contains_name(source, child, symbol);
            });
            found
        }
    }
}

/// Whether the block subtree contains a `goto` / label outside nested
/// function bodies. Such a block is skipped entirely.
fn has_unstructured_flow_here(source: &Ast, node: NodeId) -> bool {
    match source.node(node) {
        Node::Function(..) => false,
        Node::Goto(_) | Node::Label(_) => true,
        _ => {
            let mut found = false;
            for_each_child(source, node, &mut |child| {
                found |= has_unstructured_flow_here(source, child);
            });
            found
        }
    }
}

struct Transformer<'a> {
    source: &'a Ast,
    analyzer: EffectAnalyzer<'a>,
    mutated: HashSet<String>,
    resolution: &'a Resolution,
    output: Ast,
    block_stack: Vec<NodeId>,
    replaced: usize,
    root: NodeId,
    fn_stack: Vec<(NodeId, usize)>,
    fn_local_max: HashMap<NodeId, usize>,
    fn_inserted: HashMap<NodeId, usize>,
}

impl<'a> Transformer<'a> {
    fn new(source: &'a Ast, root: NodeId, resolution: &'a Resolution) -> Self {
        Self {
            source,
            analyzer: EffectAnalyzer::new(source, resolution, root, true),
            mutated: mutated_builtin_roots(source, resolution, root),
            resolution,
            output: clone_source(source),
            block_stack: Vec::new(),
            replaced: 0,
            root,
            fn_stack: Vec::new(),
            fn_local_max: HashMap::new(),
            fn_inserted: HashMap::new(),
        }
    }

    fn proof<'b>(
        &'b self,
        bindings: &'b HashMap<BindingId, NodeId>,
        segment: &'b HashMap<SymbolId, NodeId>,
    ) -> TotalProof<'b> {
        TotalProof {
            source: self.source,
            analyzer: &self.analyzer,
            mutated: &self.mutated,
            resolution: self.resolution,
            bindings,
            segment,
        }
    }

    /// Record every candidate occurrence in `node` into the active
    /// groups of the current segment, descending into candidates as
    /// well: the same shape can occur both exposed and nested inside
    /// another candidate, and only a full multi-level collection sees
    /// both. Overlap between applied groups is resolved at decide
    /// time. `keys` runs parallel to `groups` so a flush can rebuild
    /// the active map afterwards.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn collect_expr(
        &self,
        bindings: &HashMap<BindingId, NodeId>,
        segment: &HashMap<SymbolId, NodeId>,
        node: NodeId,
        stmt_idx: usize,
        groups: &mut Vec<Group>,
        keys: &mut Vec<String>,
        active: &mut HashMap<String, usize>,
    ) {
        match self.source.node(node) {
            Node::Function(..) => {}
            Node::Paren(inner) => {
                let inner = *inner;
                self.collect_expr(bindings, segment, inner, stmt_idx, groups, keys, active);
            }
            Node::Bin(..) | Node::Un(..) => {
                let proof = self.proof(bindings, segment);
                if let Some(shape) = candidate_shape(&proof, node, 0) {
                    if shape.name_count > 0 {
                        if let Some(&index) = active.get(&shape.key) {
                            groups[index].occurrences.push((node, stmt_idx));
                        } else {
                            let size = measure_expr(self.source, node);
                            active.insert(shape.key.clone(), groups.len());
                            groups.push(Group {
                                names: shape.names,
                                template: node,
                                occurrences: vec![(node, stmt_idx)],
                                size,
                            });
                            keys.push(shape.key);
                        }
                    }
                }
                // Descend regardless: inner candidates inside this
                // candidate are collected too, and decide time sorts
                // out which level wins.
                let mut children = Vec::new();
                for_each_child(self.source, node, &mut |child| children.push(child));
                for child in children {
                    self.collect_expr(bindings, segment, child, stmt_idx, groups, keys, active);
                }
            }
            _ => {
                let mut children = Vec::new();
                for_each_child(self.source, node, &mut |child| children.push(child));
                for child in children {
                    self.collect_expr(bindings, segment, child, stmt_idx, groups, keys, active);
                }
            }
        }
    }

    /// Decide the groups of one finished segment and record carrier
    /// insertions and occurrence replacements for them. Groups are
    /// considered largest-saving first; a group whose remaining
    /// occurrences (after removing ones inside, or containing, an
    /// already applied occurrence) number fewer than two, or whose
    /// recomputed gate fails, is skipped. This resolves overlap
    /// between nesting levels: an applied outer expression voids its
    /// inner occurrences, and an applied inner expression voids the
    /// outer occurrences that contain it.
    fn decide_segment(
        &mut self,
        groups: Vec<Group>,
        inserts: &mut HashMap<usize, Vec<NodeId>>,
        replacements: &mut Vec<(NodeId, SymbolId)>,
    ) {
        let mut order: Vec<usize> = (0..groups.len()).collect();
        order.sort_by(|&a, &b| {
            let sa = (groups[a].occurrences.len().saturating_sub(1)) * groups[a].size;
            let sb = (groups[b].occurrences.len().saturating_sub(1)) * groups[b].size;
            sb.cmp(&sa)
        });
        let mut consumed_roots: HashSet<NodeId> = HashSet::new();
        let mut consumed_subtree: HashSet<NodeId> = HashSet::new();
        for index in order {
            let group = &groups[index];
            let mut remaining: Vec<(NodeId, usize)> = Vec::new();
            for &(occurrence, stmt_idx) in &group.occurrences {
                if consumed_subtree.contains(&occurrence) {
                    continue;
                }
                let mut subtree = Vec::new();
                ast_utils::walk(self.source, occurrence, &mut |id| subtree.push(id));
                if subtree.iter().any(|id| consumed_roots.contains(id)) {
                    continue;
                }
                remaining.push((occurrence, stmt_idx));
            }
            let n = remaining.len();
            if n < 2 {
                continue;
            }
            let symbol = self.output.strings.fresh(CARRIER_PREFIX);
            let cloned = clone_subtree(&mut self.output, self.source, group.template);
            let mut cloned_nodes = Vec::new();
            ast_utils::walk(&self.output, cloned, &mut |id| cloned_nodes.push(id));
            for id in cloned_nodes {
                self.output.nodes.mark_synthetic(id, REASON);
            }
            let local = self.output.push(Node::Local(vec![symbol], vec![cloned]));
            self.output.nodes.mark_synthetic(local, REASON);
            let decl_size = measure_stmt(&self.output, local);
            let name_len = self.output.strings.get(symbol).len();
            let net = n * group.size;
            let cost = decl_size + 1 + n * name_len;
            if net <= cost {
                continue;
            }
            // Local budget: the carrier is a new local in the
            // enclosing function. Skip this group when the function
            // is already at the active-local limit, counting carriers
            // this pass already inserted into the same function --
            // otherwise the generated code exceeds the VM's
            // 200-local limit and fails to load (the member-cache
            // passes guard the same class). The accounting is the
            // shared conservative whole-function maximum.
            let (fn_body, fn_params) = self.fn_stack.last().copied().unwrap_or((self.root, 0));
            let fn_max = *self.fn_local_max.entry(fn_body).or_insert_with(|| {
                super::single_member_cache::max_active_locals(self.source, fn_params, fn_body)
            });
            let inserted = self.fn_inserted.entry(fn_body).or_insert(0);
            if fn_max + *inserted + 1 > super::single_member_cache::MAX_LOCALS {
                continue;
            }
            *inserted += 1;
            let first_stmt = remaining[0].1;
            inserts.entry(first_stmt).or_default().push(local);
            for &(occurrence, _) in &remaining {
                replacements.push((occurrence, symbol));
                consumed_roots.insert(occurrence);
                ast_utils::walk(self.source, occurrence, &mut |id| {
                    consumed_subtree.insert(id);
                });
            }
            self.replaced += n;
        }
    }

    /// Scan one segment and return all of its groups (active plus
    /// flushed), collected at every nesting level.
    fn scan_segment(
        &self,
        statements: &[NodeId],
        segment: &[usize],
        bindings_start: &HashMap<BindingId, NodeId>,
    ) -> Vec<Group> {
        let mut bindings = bindings_start.clone();
        // Globals assigned earlier in this segment, by spelling.
        // Reset per segment: a boundary (call / control flow) may
        // have reassigned any global in between.
        let mut segment_bindings: HashMap<SymbolId, NodeId> = HashMap::new();
        let mut groups: Vec<Group> = Vec::new();
        let mut keys: Vec<String> = Vec::new();
        let mut active: HashMap<String, usize> = HashMap::new();
        let mut completed: Vec<Group> = Vec::new();
        for &idx in segment {
            let statement = statements[idx];
            match self.source.node(statement).clone() {
                Node::Local(targets, expressions) => {
                    if expressions
                        .iter()
                        .any(|expr| contains_call(self.source, *expr))
                    {
                        break;
                    }
                    for expression in &expressions {
                        self.collect_expr(
                            &bindings,
                            &segment_bindings,
                            *expression,
                            idx,
                            &mut groups,
                            &mut keys,
                            &mut active,
                        );
                    }
                    let target_set: HashSet<SymbolId> = targets.iter().copied().collect();
                    let mut kept: Vec<Group> = Vec::new();
                    let mut kept_keys: Vec<String> = Vec::new();
                    for (group, key) in groups.drain(..).zip(keys.drain(..)) {
                        if group.names.intersection(&target_set).next().is_some() {
                            completed.push(group);
                        } else {
                            kept.push(group);
                            kept_keys.push(key);
                        }
                    }
                    groups = kept;
                    keys = kept_keys;
                    active.clear();
                    for (index, key) in keys.iter().enumerate() {
                        active.insert(key.clone(), index);
                    }
                    for (bid, expression) in self.resolution.node_bids[statement as usize]
                        .iter()
                        .zip(&expressions)
                    {
                        if self.resolution.binding_write_counts[*bid as usize] == 0 {
                            bindings.insert(*bid, *expression);
                        }
                    }
                    // Later references to these spellings in this
                    // block resolve to the locals, not to any global
                    // of the same spelling.
                    for symbol in &targets {
                        segment_bindings.remove(symbol);
                    }
                }
                Node::Assign(targets, expressions) => {
                    if targets
                        .iter()
                        .chain(expressions.iter())
                        .any(|node| contains_call(self.source, *node))
                    {
                        break;
                    }
                    for expression in &expressions {
                        self.collect_expr(
                            &bindings,
                            &segment_bindings,
                            *expression,
                            idx,
                            &mut groups,
                            &mut keys,
                            &mut active,
                        );
                    }
                    let non_name_target = targets
                        .iter()
                        .any(|target| !matches!(self.source.node(*target), Node::Name(_)));
                    if non_name_target {
                        completed.append(&mut groups);
                        keys.clear();
                    } else {
                        let target_set: HashSet<SymbolId> = targets
                            .iter()
                            .filter_map(|target| match self.source.node(*target) {
                                Node::Name(symbol) => Some(*symbol),
                                _ => None,
                            })
                            .collect();
                        let mut kept: Vec<Group> = Vec::new();
                        let mut kept_keys: Vec<String> = Vec::new();
                        for (group, key) in groups.drain(..).zip(keys.drain(..)) {
                            if group.names.intersection(&target_set).next().is_some() {
                                completed.push(group);
                            } else {
                                kept.push(group);
                                kept_keys.push(key);
                            }
                        }
                        groups = kept;
                        keys = kept_keys;
                    }
                    active.clear();
                    for (index, key) in keys.iter().enumerate() {
                        active.insert(key.clone(), index);
                    }
                    // Maintain the segment bindings from this
                    // assignment. All decisions use the pre-statement
                    // bindings: a right-hand side is proven with the
                    // values in force before this statement runs.
                    if non_name_target {
                        segment_bindings.clear();
                    } else {
                        let updates: Vec<(SymbolId, Option<NodeId>)> = {
                            let proof = self.proof(&bindings, &segment_bindings);
                            let mut updates = Vec::new();
                            for (target, expression) in targets.iter().zip(expressions.iter()) {
                                let Node::Name(symbol) = self.source.node(*target) else {
                                    continue;
                                };
                                // Only global targets feed the
                                // segment map; a local write is
                                // tracked through the local bindings.
                                let Some(bid) = self.resolution.node_bid[*target as usize] else {
                                    continue;
                                };
                                if self.resolution.binding(bid).kind != BindingKind::Global {
                                    continue;
                                }
                                let total = proof.is_total_numeric(*expression, 0)
                                    && !subtree_contains_name(self.source, *expression, *symbol);
                                updates.push((*symbol, total.then_some(*expression)));
                            }
                            updates
                        };
                        for (symbol, value) in updates {
                            match value {
                                Some(expression) => {
                                    segment_bindings.insert(symbol, expression);
                                }
                                None => {
                                    segment_bindings.remove(&symbol);
                                }
                            }
                        }
                    }
                }
                Node::Return(expressions) => {
                    if expressions
                        .iter()
                        .any(|expr| contains_call(self.source, *expr))
                    {
                        break;
                    }
                    for expression in &expressions {
                        self.collect_expr(
                            &bindings,
                            &segment_bindings,
                            *expression,
                            idx,
                            &mut groups,
                            &mut keys,
                            &mut active,
                        );
                    }
                    break;
                }
                _ => break,
            }
        }
        groups.append(&mut completed);
        groups
    }

    /// Apply one segment: a single multi-level scan followed by
    /// greedy largest-saving-first application with overlap
    /// filtering (see `decide_segment`).
    fn process_segment(
        &mut self,
        statements: &[NodeId],
        segment: &[usize],
        bindings_start: &HashMap<BindingId, NodeId>,
        inserts: &mut HashMap<usize, Vec<NodeId>>,
        replacements: &mut Vec<(NodeId, SymbolId)>,
    ) {
        let groups = self.scan_segment(statements, segment, bindings_start);
        self.decide_segment(groups, inserts, replacements);
    }

    fn transform_block(&mut self, block: NodeId) {
        let Node::Block(statements) = self.source.node(block).clone() else {
            return;
        };
        if has_unstructured_flow_here(self.source, block) {
            self.recurse_nesting(block, &statements);
            return;
        }
        // Ancestor bindings: locals of every enclosing block.
        let mut bindings: HashMap<BindingId, NodeId> = HashMap::new();
        for enclosing in &self.block_stack {
            bindings.extend(bindings_for_body(self.source, self.resolution, *enclosing));
        }
        let mut inserts: HashMap<usize, Vec<NodeId>> = HashMap::new();
        let mut replacements: Vec<(NodeId, SymbolId)> = Vec::new();
        let mut segment: Vec<usize> = Vec::new();
        let mut segment_bindings_start = bindings.clone();
        for (idx, statement) in statements.iter().enumerate() {
            match self.source.node(*statement).clone() {
                Node::Local(_, expressions) => {
                    let has_call = expressions
                        .iter()
                        .any(|expr| contains_call(self.source, *expr));
                    if has_call {
                        // Boundary: the segment ends, but the binding
                        // still dominates later statements, so record
                        // it after flushing the segment.
                        if !segment.is_empty() {
                            self.process_segment(
                                &statements,
                                &segment,
                                &segment_bindings_start,
                                &mut inserts,
                                &mut replacements,
                            );
                            segment.clear();
                        }
                        for (bid, expression) in self.resolution.node_bids[*statement as usize]
                            .iter()
                            .zip(&expressions)
                        {
                            if self.resolution.binding_write_counts[*bid as usize] == 0 {
                                bindings.insert(*bid, *expression);
                            }
                        }
                    } else {
                        if segment.is_empty() {
                            segment_bindings_start = bindings.clone();
                        }
                        segment.push(idx);
                        for (bid, expression) in self.resolution.node_bids[*statement as usize]
                            .iter()
                            .zip(&expressions)
                        {
                            if self.resolution.binding_write_counts[*bid as usize] == 0 {
                                bindings.insert(*bid, *expression);
                            }
                        }
                    }
                }
                Node::Assign(targets, expressions) => {
                    let has_call = targets
                        .iter()
                        .chain(expressions.iter())
                        .any(|node| contains_call(self.source, *node));
                    if has_call {
                        if !segment.is_empty() {
                            self.process_segment(
                                &statements,
                                &segment,
                                &segment_bindings_start,
                                &mut inserts,
                                &mut replacements,
                            );
                            segment.clear();
                        }
                    } else {
                        if segment.is_empty() {
                            segment_bindings_start = bindings.clone();
                        }
                        segment.push(idx);
                    }
                }
                Node::Return(expressions) => {
                    let has_call = expressions
                        .iter()
                        .any(|expr| contains_call(self.source, *expr));
                    if has_call {
                        if !segment.is_empty() {
                            self.process_segment(
                                &statements,
                                &segment,
                                &segment_bindings_start,
                                &mut inserts,
                                &mut replacements,
                            );
                            segment.clear();
                        }
                    } else {
                        // A call-free Return still evaluates its list
                        // left to right, so its occurrences are
                        // countable; it ends the segment afterwards.
                        if segment.is_empty() {
                            segment_bindings_start = bindings.clone();
                        }
                        segment.push(idx);
                        self.process_segment(
                            &statements,
                            &segment,
                            &segment_bindings_start,
                            &mut inserts,
                            &mut replacements,
                        );
                        segment.clear();
                    }
                }
                _ => {
                    if !segment.is_empty() {
                        self.process_segment(
                            &statements,
                            &segment,
                            &segment_bindings_start,
                            &mut inserts,
                            &mut replacements,
                        );
                        segment.clear();
                    }
                }
            }
        }
        if !segment.is_empty() {
            self.process_segment(
                &statements,
                &segment,
                &segment_bindings_start,
                &mut inserts,
                &mut replacements,
            );
        }
        if !inserts.is_empty() {
            let mut rewritten: Vec<NodeId> = Vec::with_capacity(statements.len() + inserts.len());
            for (idx, statement) in statements.iter().enumerate() {
                if let Some(decls) = inserts.get(&idx) {
                    rewritten.extend(decls.iter().copied());
                }
                rewritten.push(*statement);
            }
            self.output
                .nodes
                .rewrite(block, Node::Block(rewritten), REASON);
        }
        for (occurrence, symbol) in replacements {
            self.output
                .nodes
                .rewrite(occurrence, Node::Name(symbol), REASON);
            super::origins::derive(
                &mut self.output,
                occurrence,
                self.source,
                &[occurrence],
                REASON,
            );
        }
        self.recurse_nesting(block, &statements);
    }

    fn recurse_block(&mut self, parent: NodeId, child: NodeId) {
        self.block_stack.push(parent);
        self.transform_block(child);
        self.block_stack.pop();
    }

    fn recurse_expr_functions(&mut self, parent: NodeId, node: NodeId) {
        match self.source.node(node).clone() {
            Node::Function(params, _, body) => {
                self.fn_stack.push((body, params.len()));
                self.recurse_block(parent, body);
                self.fn_stack.pop();
            }
            _ => {
                let mut children = Vec::new();
                for_each_child(self.source, node, &mut |child| children.push(child));
                for child in children {
                    self.recurse_expr_functions(parent, child);
                }
            }
        }
    }

    fn recurse_nesting(&mut self, block: NodeId, statements: &[NodeId]) {
        for statement in statements {
            match self.source.node(*statement).clone() {
                Node::Block(_) => self.recurse_block(block, *statement),
                Node::Do(body) => self.recurse_block(block, body),
                Node::While(_, body) => self.recurse_block(block, body),
                Node::Repeat(body, _) => self.recurse_block(block, body),
                Node::If(arms, otherwise) => {
                    for arm in arms {
                        self.recurse_block(block, arm.body);
                    }
                    if let Some(body) = otherwise {
                        self.recurse_block(block, body);
                    }
                }
                Node::Fornum(_, _, _, _, body) => self.recurse_block(block, body),
                Node::Forin(_, _, body) => self.recurse_block(block, body),
                Node::Funcstat(target, function) => {
                    self.recurse_expr_functions(block, target);
                    if let Node::Function(params, _, body) = self.source.node(function).clone() {
                        // A method definition's implicit `self` is a
                        // real parameter at load time.
                        let extra =
                            usize::from(matches!(self.source.node(target), Node::Methodname(..)));
                        self.fn_stack.push((body, params.len() + extra));
                        self.recurse_block(block, body);
                        self.fn_stack.pop();
                    } else {
                        self.recurse_expr_functions(block, function);
                    }
                }
                Node::Localfunc(_, function) => {
                    self.recurse_expr_functions(block, function);
                }
                Node::Local(_, expressions) | Node::Assign(_, expressions) => {
                    for expression in expressions {
                        self.recurse_expr_functions(block, expression);
                    }
                }
                Node::Callstat(expression) => {
                    self.recurse_expr_functions(block, expression);
                }
                Node::Return(expressions) => {
                    for expression in expressions {
                        self.recurse_expr_functions(block, expression);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Eliminate repeated straight-line arithmetic expressions (see the
/// module documentation). Returns the (unchanged) root; `details`
/// reports `cse=<replaced occurrences>`.
pub fn cse_straightline_arithmetic(ast: &mut Ast, root: NodeId) -> PassResult {
    if !matches!(ast.node(root), Node::Block(_)) {
        return PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["cse=0".to_string()]),
        };
    }
    let before = measure_size(ast, root);
    let original = ast.clone();
    let source = clone_source(ast);
    let resolution = resolve(&source, root);
    let mut transformer = Transformer::new(&source, root, &resolution);
    transformer.transform_block(root);
    let replaced = transformer.replaced;
    if replaced == 0 {
        return PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["cse=0".to_string()]),
        };
    }
    let output = transformer.output;
    *ast = output;
    let after = measure_size(ast, root);
    if after > before {
        // Safety net: never let this pass lengthen the code.
        *ast = original;
        return PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["cse=0".to_string()]),
        };
    }
    PassResult {
        root,
        saved: Some(before.saturating_sub(after) as u64),
        details: Some(vec![format!("cse={replaced}")]),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn run(source: &str) -> (String, usize) {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = cse_straightline_arithmetic(&mut ast, root);
        let code = Printer::new(&ast, false).output(result.root);
        let cse = result
            .details
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.strip_prefix("cse="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        (code, cse)
    }

    #[test]
    fn carrierizes_three_occurrences_of_a_long_expression() {
        let (code, cse) = run("local alpha = 2 local beta = 3 \
             local x = alpha*100+beta*200 \
             local y = alpha*100+beta*200 \
             local z = alpha*100+beta*200 \
             return x+y+z");
        assert_eq!(cse, 3, "code: {code}");
        assert!(
            code.contains("local __cse=alpha*100+beta*200"),
            "code: {code}"
        );
        assert_eq!(
            code.matches("alpha*100+beta*200").count(),
            1,
            "code: {code}"
        );
    }

    #[test]
    fn carrierizes_two_occurrences_of_a_very_long_expression() {
        let (code, cse) = run("local alpha = 2 local beta = 3 local gamma = 4 local x = alpha*1000+beta*2000+gamma local y = alpha*1000+beta*2000+gamma return x+y");
        assert_eq!(cse, 2, "code: {code}");
        assert!(
            code.contains("local __cse=alpha*1000+beta*2000+gamma"),
            "code: {code}"
        );
    }

    #[test]
    fn carrierizes_a_maximal_expression_but_not_its_inner_part() {
        let (code, cse) = run("local alpha = 2 local beta = 3 local x = (alpha*100+beta*200)*alpha local y = (alpha*100+beta*200)*alpha return x+y");
        assert_eq!(cse, 2, "code: {code}");
        assert!(
            code.contains("local __cse=(alpha*100+beta*200)*alpha"),
            "code: {code}"
        );
        // The printer packs the adjacent x/y locals; what matters is
        // that the expression survives only in the carrier definition.
        assert_eq!(
            code.matches("alpha*100+beta*200").count(),
            1,
            "code: {code}"
        );
    }

    #[test]
    fn carrierizes_an_inner_repeat_inside_a_single_statement() {
        let (code, cse) = run("local alpha = 2 local beta = 3 local gamma = 4 local s = (alpha*1000+beta*2000+gamma*3000)^2 + (alpha*1000+beta*2000+gamma*3000) return s");
        assert_eq!(cse, 2, "code: {code}");
        assert!(
            code.contains("local __cse=alpha*1000+beta*2000+gamma*3000"),
            "code: {code}"
        );
        assert_eq!(
            code.matches("alpha*1000+beta*2000+gamma*3000").count(),
            1,
            "code: {code}"
        );
    }

    #[test]
    fn carrierizes_a_repeat_spanning_into_a_return_list() {
        let (code, cse) = run("local alpha = 2 local beta = 3 local gamma = 4 local x = alpha*1000+beta*2000+gamma*3000 return alpha*1000+beta*2000+gamma*3000, x");
        assert_eq!(cse, 2, "code: {code}");
        assert!(
            code.contains("local __cse=alpha*1000+beta*2000+gamma*3000"),
            "code: {code}"
        );
        assert_eq!(
            code.matches("alpha*1000+beta*2000+gamma*3000").count(),
            1,
            "code: {code}"
        );
    }

    #[test]
    fn reassignment_flushes_the_group() {
        let (code, cse) = run("local alpha = 2 local beta = 3 \
             local x = alpha*100+beta*200 \
             alpha = 5 \
             local y = alpha*100+beta*200 \
             return x+y");
        assert_eq!(cse, 0, "code: {code}");
        assert!(!code.contains("__cse"), "code: {code}");
    }

    #[test]
    fn shadowing_splits_the_group() {
        let (code, cse) = run("local alpha = 2 local beta = 3 \
             local x = alpha*100+beta*200 \
             local alpha = 9 \
             local y = alpha*100+beta*200 \
             local z = alpha*100+beta*200 \
             local w = alpha*100+beta*200 \
             return x+y+z+w");
        assert_eq!(cse, 3, "code: {code}");
        // The expression survives in the carrier definition and in x
        // (bound before the shadowing local); y/z/w use the carrier.
        // The printer packs adjacent locals, so count, don't match a
        // single-statement spelling.
        assert_eq!(
            code.matches("alpha*100+beta*200").count(),
            2,
            "code: {code}"
        );
    }

    #[test]
    fn unproven_names_are_not_carrierized() {
        let (code, cse) = run("local x = foo*100+bar*200 \
             local y = foo*100+bar*200 \
             local z = foo*100+bar*200 \
             return x+y+z");
        assert_eq!(cse, 0, "code: {code}");
        assert!(!code.contains("__cse"), "code: {code}");
    }

    #[test]
    fn a_segment_assigned_global_proves_later_uses() {
        let (code, cse) = run("local alpha = 2 local beta = 3 __fs0 = alpha*100+beta*200 local x = __fs0*1000+alpha*2000 local y = __fs0*1000+alpha*2000 local z = __fs0*1000+alpha*2000 return x+y+z");
        assert_eq!(cse, 3, "code: {code}");
        assert!(
            code.contains("local __cse=__fs0*1000+alpha*2000"),
            "code: {code}"
        );
    }

    #[test]
    fn a_segment_assigned_global_from_a_host_read_binding_proves_later_uses() {
        let (code, cse) = run("local t = input.getNumber(1) __fs0 = t*100+200 local x = __fs0*1000+t*2000 local y = __fs0*1000+t*2000 local z = __fs0*1000+t*2000 return x+y+z");
        assert_eq!(cse, 3, "code: {code}");
        assert!(
            code.contains("local __cse=__fs0*1000+t*2000"),
            "code: {code}"
        );
    }

    #[test]
    fn a_multi_target_segment_assignment_proves_both_globals() {
        let (code, cse) = run("local alpha = 2 local beta = 3 __fs0,__fs1 = alpha*100,beta*200 local x = __fs0*1000+__fs1*2000 local y = __fs0*1000+__fs1*2000 local z = __fs0*1000+__fs1*2000 return x+y+z");
        assert_eq!(cse, 3, "code: {code}");
        assert!(
            code.contains("local __cse=__fs0*1000+__fs1*2000"),
            "code: {code}"
        );
    }

    #[test]
    fn a_non_total_reassignment_drops_the_segment_proof() {
        let (code, cse) = run("local alpha = 2 local beta = 3 __fs0 = alpha*100+beta*200 __fs0 = foo local x = __fs0*1000+alpha*2000 local y = __fs0*1000+alpha*2000 local z = __fs0*1000+alpha*2000 return x+y+z");
        assert_eq!(cse, 0, "code: {code}");
        assert!(!code.contains("__cse"), "code: {code}");
    }

    #[test]
    fn a_self_referential_assignment_does_not_prove_later_uses() {
        let (code, cse) = run("local alpha = 2 __fs0 = __fs0*100+alpha*200 local x = __fs0*1000+alpha*2000 local y = __fs0*1000+alpha*2000 local z = __fs0*1000+alpha*2000 return x+y+z");
        assert_eq!(cse, 0, "code: {code}");
        assert!(!code.contains("__cse"), "code: {code}");
    }

    #[test]
    fn a_call_defined_global_is_not_proven_across_the_boundary() {
        // The defining assignment contains a call, so it ends the
        // segment and its binding is deliberately not carried over.
        let (code, cse) = run("local alpha = 2 __fs0 = input.getNumber(1) local x = __fs0*1000+alpha*2000 local y = __fs0*1000+alpha*2000 local z = __fs0*1000+alpha*2000 return x+y+z");
        assert_eq!(cse, 0, "code: {code}");
        assert!(!code.contains("__cse"), "code: {code}");
    }

    #[test]
    fn an_index_write_clears_segment_proofs() {
        let (code, cse) = run("local alpha = 2 local beta = 3 __fs0 = alpha*100+beta*200 _ENV.__fs0 = 9 local x = __fs0*1000+alpha*2000 local y = __fs0*1000+alpha*2000 local z = __fs0*1000+alpha*2000 return x+y+z");
        assert_eq!(cse, 0, "code: {code}");
        assert!(!code.contains("__cse"), "code: {code}");
    }

    #[test]
    fn a_call_statement_splits_the_segment() {
        let (code, cse) = run("local alpha = 2 local beta = 3 \
             local x = alpha*100+beta*200 \
             print(alpha) \
             local y = alpha*100+beta*200 \
             return x+y");
        assert_eq!(cse, 0, "code: {code}");
        assert!(!code.contains("__cse"), "code: {code}");
    }

    #[test]
    fn short_expressions_fail_the_profitability_gate() {
        let (code, cse) = run("local a = 2 \
             local x = a+1 local y = a+1 local z = a+1 \
             return x+y+z");
        assert_eq!(cse, 0, "code: {code}");
        assert!(!code.contains("__cse"), "code: {code}");
    }

    #[test]
    fn non_arithmetic_operators_are_not_candidates() {
        let (code, cse) = run("local alpha = 2 local beta = 3 \
             local x = alpha..beta \
             local y = alpha..beta \
             local z = alpha..beta \
             return x");
        assert_eq!(cse, 0, "code: {code}");
        assert!(!code.contains("__cse"), "code: {code}");
    }

    #[test]
    fn a_call_inside_the_expression_blocks_the_candidate() {
        let (code, cse) = run("local alpha = 2 \
             local x = alpha+input.getNumber(1) \
             local y = alpha+input.getNumber(1) \
             return x+y");
        assert_eq!(cse, 0, "code: {code}");
        assert!(!code.contains("__cse"), "code: {code}");
    }

    #[test]
    fn a_host_read_binding_proves_later_uses() {
        let (code, cse) = run("local alpha = 2 \
             local t = input.getNumber(1) \
             local x = t*1000+alpha*2000 \
             local y = t*1000+alpha*2000 \
             local z = t*1000+alpha*2000 \
             return x+y+z");
        assert_eq!(cse, 3, "code: {code}");
        assert!(
            code.contains("local __cse=t*1000+alpha*2000"),
            "code: {code}"
        );
    }

    #[test]
    fn unary_minus_operands_are_candidates() {
        let (code, cse) = run("local alpha = 2 local beta = 3 \
             local x = -alpha*100+beta*200 \
             local y = -alpha*100+beta*200 \
             local z = -alpha*100+beta*200 \
             return x+y+z");
        assert_eq!(cse, 3, "code: {code}");
        assert!(
            code.contains("local __cse=-alpha*100+beta*200"),
            "code: {code}"
        );
    }
    #[test]
    fn carrier_skips_function_at_the_local_limit() {
        // 198 parameters + alpha + beta already reach the limit; the
        // carrier would be the 201st local.
        let params = (1..=198)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "function onTick({params}) local alpha=input.getNumber(1) local beta=input.getNumber(2) G1=alpha*100+beta*200+1 G2=alpha*100+beta*200+2 G3=alpha*100+beta*200+3 output.setNumber(1,alpha+beta) end"
        );
        let (code, _) = run(&source);
        assert!(!code.contains("__cse"), "{code}");
    }

    #[test]
    fn carrier_fires_below_the_local_limit() {
        let params = (1..=197)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "function onTick({params}) local alpha=input.getNumber(1) local beta=input.getNumber(2) G1=alpha*100+beta*200+1 G2=alpha*100+beta*200+2 G3=alpha*100+beta*200+3 output.setNumber(1,alpha+beta) end"
        );
        let (code, _) = run(&source);
        assert!(code.contains("__cse"), "{code}");
    }
}
