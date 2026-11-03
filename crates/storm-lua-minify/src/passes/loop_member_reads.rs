//! Loop member-read caching in loops (target-size layouts only).
//!
//! A repeated value read `B.K` inside a loop pays one table lookup per
//! iteration per occurrence. When the same `(base, key)` pair is read
//! at least twice in one loop body, caching the value once before the
//! loop (`local c = B.K`, then `c` at each site) removes one lookup
//! per iteration per extra occurrence. This is the A group of the
//! cycle-69 census: loop-invariant member reads that neither
//! `loop_invariant` (host calls and arithmetic only), nor
//! `available_expressions` (non-builtin index reads are barriers
//! there), nor `arithmetic_cse` (binary/unary only) absorbs.
//!
//! This transform is wired only into target-size layouts
//! (`joint-api-local-mr` and `joint-api-local-licm-mr` in
//! `search.rs`), never into target-free compilation, and it is a
//! layout-construction transform in the LICM/S1 sense, not a phase
//! pass: it is deliberately not registered in the canonical pass
//! registry, so it adds no `effectivePassToggles` entry and no map
//! churn for files whose winner does not use it. The target budget
//! is the profitability gate: a layout that exceeds the target loses
//! to every fitting candidate through the existing over-target
//! demotion in `select_best_candidates`, and the speed rank orders
//! the MR layouts above their base layouts but below the S1 layouts
//! (so an MR layout can only win when no S1 layout fits, or when
//! the MR layout is the fastest fitting candidate among the
//! non-S1 layouts).
//!
//! The transform is intentionally conservative (cycle-70 v1 design):
//! - Only a single-level value read qualifies: the object must be a
//!   bare `Name` and the key a dot/string literal or a numeric
//!   literal. Reads used as a call callee are skipped (that is S1's
//!   territory); reads in loop headers are scanned for barriers but
//!   never replaced.
//! - The base must be a `local` with write count zero whose
//!   declaration initializer is a table constructor, or a global
//!   with exactly one defining assignment whose value is a table
//!   constructor (chunk-top form, or the cycle-65 same-block form:
//!   the definition precedes the loop in the loop's own block, the
//!   enclosing function has no `goto` / label, and neither the
//!   definition nor the intervening statements contain a call or a
//!   table-field write). A `for` variable also qualifies, but only
//!   through the per-iteration body-entry cache below: a for
//!   variable has no table-constructor initializer to prove, so a
//!   preheader read could error on a zero-trip loop where the
//!   original code never reads. Instead the cache local is inserted
//!   into the loop body block immediately before the statement
//!   containing the group's first read, and only when that first
//!   read sits in a head position of its statement (see
//!   `head_position_ok`): the cache then evaluates exactly where
//!   the first read would, and no call completes before it (see
//!   `calls_before_target`), so the error behaviour of the first
//!   read is preserved. Later reads are dominated by the body-block
//!   local. Parameters, local functions and non-constructor
//!   initializers never qualify, so the base is a plain table at
//!   loop entry: no metatable can be involved,
//!   because installing one requires passing the base to a call
//!   (see the escape rule below). The chunk-top global form follows
//!   the S1 precedent: the assignment executes at load, before host
//!   callbacks run.
//! - The base must never escape anywhere in the program: every
//!   `Name` node resolving to the base must be the object of an
//!   `Index` node. Any other use (an argument, a stored value, a
//!   return value, a call callee, a table field, a key) disqualifies
//!   the base outright, which also closes the alias route to
//!   `setmetatable` and to writes through another variable. A
//!   whole-program scan is used because a nested function closing
//!   over the base could otherwise smuggle it out.
//! - A loop is skipped entirely if its body subtree (nested loops
//!   included, nested functions excluded) contains an unknown call,
//!   a method call, a stateful builtin (`math.random`,
//!   `math.randomseed`, `async.*`, `map.*`, Extended control
//!   setters), or a `goto` / label. Known builtin calls are allowed:
//!   they write host state or command buffers, never Lua table
//!   fields. Loop headers are scanned with the same call rules.
//! - A generic-for loop's header cache additionally requires the
//!   iterator to be field-write-free (cycle 95, Codex finding 6,
//!   MR-header form): the iterator runs once per step between body
//!   executions, so a header cache computed once before the loop goes
//!   stale if the iterator writes any table field -- through a
//!   captured base, through the state/control value passed to it, or
//!   through an alias. Only a directly known iterator function (a
//!   local function, a local initialized with a function literal, or
//!   a single-definition global function) whose body contains no field
//!   write, no unknown/stateful call, no goto/label, and no nested
//!   generic-for is admitted; every other iterator shape bars the
//!   header cache. Proven builtin iterators (a `pairs` / `ipairs`
//!   call, a bare `next` name) are exempt: their implicit iterator
//!   is the `next` family, which never writes a field. The
//!   per-iteration body-entry cache for for-variable bases is
//!   unaffected by the loop's own iterator: it is recomputed each
//!   iteration after the iterator call. The bar also propagates
//!   to enclosing loops: a loop whose body subtree contains a
//!   barring generic-for caches nothing at all, because it claims
//!   reads in the whole subtree while the nested iterator runs
//!   between its body executions (and can sit between a body-entry
//!   insertion and a later read within one iteration).
//! - A group is skipped when the loop body writes exactly that
//!   `(base, key)` pair. Writes to other keys of a plain table
//!   cannot change this key's value, so they do not disqualify the
//!   group (this is deliberately narrower than S1's whole-loop
//!   table-write barrier). A write through a key the analysis
//!   cannot resolve (a dynamic key) closes every group of its
//!   base instead: it may hit any key.
//! - The enclosing function must contain no `goto` / label, so no
//!   jump can skip the base's declaration: scope resolution plus
//!   sequential execution then guarantees the declaration ran
//!   before the loop (Lua also forbids jumping into a local's
//!   scope, which is the same guarantee from the language side).
//! - Calls inside a nested function are never collected, and calls
//!   in nested loops are claimed by the outermost qualifying loop
//!   first, exactly like `loop_invariant` and S1, so one preheader
//!   serves every nesting level.
//! - The preheader evaluation is total: a plain-table read never
//!   errors (a missing key yields nil), so a zero-trip loop and a
//!   `repeat` loop stay safe.
//!
//! The cache local's name is the shortest single-letter identifier
//! absent from the program's string table (falling back to a
//! generated name when every letter is taken), mirroring S1: a name
//! absent from the table cannot be declared or referenced by user
//! code anywhere, so the inserted local can shadow nothing.
//!
//! Deliberately out of scope: deeper base chains (`a.b.c` first
//! levels are still cached, since the inner `a.b` read itself
//! qualifies), bracket keys with dynamic expressions,
//! non-constructor initializers, groups inside loops that call user
//! functions (the unknown-call barrier), for-variable groups whose
//! first read is conditional (inside a branch, a nested loop body,
//! a `repeat` statement, or after a call in its statement -- the
//! body-entry cache cannot preserve the first read's error point
//! there), and a partial variant
//! (first-group-only, S1-partial style) -- the budget fit of the
//! full variant decides whether a partial variant is worth adding.
//! Global bases were out of v1 scope and are covered since the v2
//! extension (cycle-71 diagnosis: 251 of 498 proof-checked groups
//! on winner code had global bases). For-variable bases are covered
//! since the cycle-73 extension (same diagnosis: 197 groups).

use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use crate::passes::speed_analysis::SpeedAnalysis;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId, TableField};
use storm_lua_syntax::ast_utils;

const REASON: &str = "loop-member-read-caching";

/// Builtins that disqualify a loop outright (stateful or opaque),
/// mirroring `single_member_cache` and `loop_invariant`.
fn is_disqualifying_builtin(builtin: &str) -> bool {
    builtin == "math.random"
        || builtin == "math.randomseed"
        || builtin.starts_with("async.")
        || builtin.starts_with("map.")
        || builtin == "self"
        || builtin.starts_with("self.")
        || builtin.contains("setInput")
        || builtin.contains("setProperty")
}

/// Whether a call node violates the loop barriers (unknown, method,
/// or stateful). Known builtin calls are allowed: they never write
/// Lua table fields.
fn call_is_barrier(analyzer: &EffectAnalyzer<'_>, node: NodeId, source: &Ast) -> bool {
    let Node::Call(function, _, method) = source.node(node).clone() else {
        return false;
    };
    if method.is_some() {
        return true;
    }
    let Some(builtin) = analyzer.resolve_builtin_reference(function) else {
        return true;
    };
    is_disqualifying_builtin(&builtin)
}

/// Binding of a `Name` node, if any.
pub(crate) fn name_bid(resolution: &Resolution, node: NodeId) -> Option<BindingId> {
    resolution.node_bid.get(node as usize).copied().flatten()
}

/// Canonical key text for a literal index key: dot names, bracket
/// strings and numeric literals share one namespace per base, since
/// they denote the same table lookup.
///
/// Numeric keys are canonicalized by *value*, not spelling: Lua
/// treats an integer key and a float key with the same integral
/// value as one key (`t[1]`, `t[1.0]` and `t[1e0]` all read the
/// same slot), so spelling-based keys would let a write through
/// one spelling leave a read cached through another stale. A
/// non-integral float is keyed by its bit pattern. The float range
/// check is strict on the upper end: `i64::MAX as f64` rounds up
/// to 2^63, which has no integer conversion in Lua and must stay
/// a float key, so integral floats are folded only below 2^63.
/// A literal that parses to NaN (unreachable for a well-formed
/// literal) yields `None`, i.e. an unresolved key.
pub(crate) fn key_text(source: &Ast, key: NodeId) -> Option<String> {
    match source.node(key) {
        Node::Str(raw) => {
            let raw: &str = raw;
            let inner = raw
                .strip_prefix('"')
                .and_then(|x| x.strip_suffix('"'))
                .or_else(|| raw.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))?;
            if inner.is_empty() || inner.contains('\\') {
                return None;
            }
            Some(format!("s:{inner}"))
        }
        Node::Num(raw) => {
            use storm_lua_syntax::numeric::{integer_literal_value, num_val};
            if let Some(i) = integer_literal_value(raw) {
                return Some(format!("i:{i}"));
            }
            let v = num_val(raw);
            if v.is_nan() {
                return None;
            }
            if v.fract() == 0.0 && (i64::MIN as f64..9223372036854775808.0).contains(&v) {
                Some(format!("i:{}", v as i64))
            } else {
                Some(format!("f:{}", v.to_bits()))
            }
        }
        _ => None,
    }
}

/// One group of repeated reads of the same `(base, key)` pair (the
/// key itself is only needed while grouping in `analyze_loop`).
struct ReadGroup {
    bid: BindingId,
    occurrences: Vec<NodeId>,
}

/// Mutable state of one body/header scan.
struct ScanState {
    disqualified: bool,
    writes: HashSet<(BindingId, String)>,
    /// Bases written through a key the analysis cannot resolve
    /// (a dynamic key): such a write may hit any key of the base,
    /// so it closes every group of that base.
    wide_writes: HashSet<BindingId>,
    order: Vec<(BindingId, String)>,
    occurrences: HashMap<(BindingId, String), Vec<NodeId>>,
}

impl ScanState {
    fn new() -> Self {
        Self {
            disqualified: false,
            writes: HashSet::new(),
            wide_writes: HashSet::new(),
            order: Vec::new(),
            occurrences: HashMap::new(),
        }
    }

    fn record_occurrence(&mut self, bid: BindingId, key: String, node: NodeId) {
        let group = (bid, key);
        if !self.occurrences.contains_key(&group) {
            self.order.push(group.clone());
            self.occurrences.insert(group.clone(), Vec::new());
        }
        if let Some(list) = self.occurrences.get_mut(&group) {
            list.push(node);
        }
    }
}

/// Record a write through an assignment target: an index target
/// writes its `(base, key)` pair, a method-name target writes its
/// `(base, method)` pair. A write whose key cannot be resolved
/// (a dynamic key, or a literal `key_text` rejects) is recorded
/// as a base-wide write: it may hit any key of the base, so it
/// must close every group of that base. Dropping such a write
/// (the old behaviour) left a cached read stale across it.
/// A method-name target writes the dot-equivalent key, so it
/// uses the same `s:` namespace as occurrence keys.
fn record_write(source: &Ast, resolution: &Resolution, target: NodeId, state: &mut ScanState) {
    match source.node(target) {
        Node::Index(obj, key, _) => {
            if matches!(source.node(*obj), Node::Name(_)) {
                if let Some(bid) = name_bid(resolution, *obj) {
                    match key_text(source, *key) {
                        Some(key) => {
                            state.writes.insert((bid, key));
                        }
                        None => {
                            state.wide_writes.insert(bid);
                        }
                    }
                }
            }
        }
        Node::Methodname(obj, sym) => {
            if matches!(source.node(*obj), Node::Name(_)) {
                if let Some(bid) = name_bid(resolution, *obj) {
                    state
                        .writes
                        .insert((bid, format!("s:{}", source.strings.get(*sym))));
                }
            }
        }
        _ => {}
    }
}

/// Scan a subtree (nested functions excluded). `special` marks an
/// index node that is a call callee or an assignment target: such a
/// node is a write or a callee, never a value read, but its children
/// are still scanned (a dynamic key or a chained base can contain
/// reads and barrier calls). When `collect` is false (loop headers)
/// only barrier violations are recorded.
#[allow(clippy::too_many_arguments)]
fn scan_node(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    resolution: &Resolution,
    node: NodeId,
    special: bool,
    collect: bool,
    state: &mut ScanState,
) {
    if state.disqualified {
        return;
    }
    match source.node(node).clone() {
        Node::Function(..) => return,
        Node::Goto(_) | Node::Label(_) => {
            state.disqualified = true;
            return;
        }
        Node::Call(function, args, _) => {
            if call_is_barrier(analyzer, node, source) {
                state.disqualified = true;
                return;
            }
            scan_node(source, analyzer, resolution, function, true, collect, state);
            for arg in args {
                scan_node(source, analyzer, resolution, arg, false, collect, state);
            }
            return;
        }
        Node::Assign(targets, values) => {
            for target in &targets {
                record_write(source, resolution, *target, state);
                scan_node(source, analyzer, resolution, *target, true, collect, state);
            }
            for value in &values {
                scan_node(source, analyzer, resolution, *value, false, collect, state);
            }
            return;
        }
        Node::Funcstat(target, function) => {
            record_write(source, resolution, target, state);
            scan_node(source, analyzer, resolution, target, true, collect, state);
            scan_node(
                source, analyzer, resolution, function, false, collect, state,
            );
            return;
        }
        Node::Index(obj, key, _) => {
            if !special && collect && matches!(source.node(obj), Node::Name(_)) {
                if let (Some(bid), Some(key)) = (name_bid(resolution, obj), key_text(source, key)) {
                    state.record_occurrence(bid, key, node);
                }
            }
            scan_node(source, analyzer, resolution, obj, false, collect, state);
            scan_node(source, analyzer, resolution, key, false, collect, state);
            return;
        }
        _ => {}
    }
    let mut children = Vec::new();
    ast_utils::for_each_child(source, node, &mut |child| children.push(child));
    for child in children {
        scan_node(source, analyzer, resolution, child, false, collect, state);
    }
}

/// Analyse one loop. Returns `None` when the loop must not be
/// cached, otherwise the groups to cache (possibly empty).
fn analyze_loop(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    resolution: &Resolution,
    body: NodeId,
    headers: &[NodeId],
) -> Option<Vec<ReadGroup>> {
    let mut state = ScanState::new();
    scan_node(source, analyzer, resolution, body, false, true, &mut state);
    if state.disqualified {
        return None;
    }
    for header in headers {
        scan_node(
            source, analyzer, resolution, *header, false, false, &mut state,
        );
        if state.disqualified {
            return None;
        }
    }
    let mut result = Vec::new();
    for group in state.order {
        let Some(occurrences) = state.occurrences.remove(&group) else {
            continue;
        };
        if occurrences.len() < 2 {
            continue;
        }
        if state.writes.contains(&group) || state.wide_writes.contains(&group.0) {
            continue;
        }
        result.push(ReadGroup {
            bid: group.0,
            occurrences,
        });
    }
    Some(result)
}

/// Whether a subtree (nested functions included: a closure can
/// smuggle the base out) contains a `goto` or a label.
pub(crate) fn subtree_has_goto_label(source: &Ast, node: NodeId) -> bool {
    let mut found = false;
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if matches!(source.node(current), Node::Goto(_) | Node::Label(_)) {
            found = true;
            break;
        }
        ast_utils::for_each_child(source, current, &mut |child| stack.push(child));
    }
    found
}

/// Whether a statement subtree (nested functions excluded) contains
/// a call or a table-field write. Either could change what a global
/// alias denotes between its definition and the loop.
fn subtree_has_call_or_table_write(source: &Ast, node: NodeId) -> bool {
    let mut found = false;
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if found {
            break;
        }
        // Nested functions are not descended into: their bodies do
        // not run between the definition and the loop.
        if current != node && matches!(source.node(current), Node::Function(..)) {
            continue;
        }
        match source.node(current) {
            Node::Call(..) => found = true,
            Node::Assign(targets, _) => {
                for target in targets {
                    if matches!(source.node(*target), Node::Index(..) | Node::Methodname(..)) {
                        found = true;
                    }
                }
            }
            Node::Funcstat(target, _) => {
                if matches!(source.node(*target), Node::Index(..) | Node::Methodname(..)) {
                    found = true;
                }
            }
            _ => {}
        }
        if !found {
            ast_utils::for_each_child(source, current, &mut |child| stack.push(child));
        }
    }
    found
}

/// Loop position for the global same-block proof: the block directly
/// containing the loop statement, and the loop statement itself.
/// Straight-line segments reuse this position with the segment's
/// first statement in place of the loop statement.
#[derive(Clone, Copy)]
pub(crate) struct LoopPos {
    pub(crate) block: NodeId,
    pub(crate) stmt: NodeId,
}

/// The defining assignment of a single-assignment global, if it is a
/// table constructor: returns its target node (exempt from the escape
/// scan: the definition itself is not an escape). Chunk-top form
/// first (S1 precedent), then the cycle-65 same-block form.
fn global_table_def_target(
    source: &Ast,
    resolution: &Resolution,
    chunk_root: NodeId,
    pos: LoopPos,
    fn_has_goto: bool,
    bid: BindingId,
) -> Option<NodeId> {
    if resolution
        .binding_write_counts
        .get(bid as usize)
        .copied()
        .unwrap_or(0)
        != 1
    {
        return None;
    }
    if let Node::Block(statements) = source.node(chunk_root) {
        let mut found: Option<(NodeId, NodeId)> = None;
        for statement in statements {
            let Node::Assign(targets, values) = source.node(*statement) else {
                continue;
            };
            for (target, value) in targets.iter().zip(values) {
                if matches!(source.node(*target), Node::Name(_))
                    && name_bid(resolution, *target) == Some(bid)
                {
                    if found.is_some() {
                        return None;
                    }
                    found = Some((*target, *value));
                }
            }
        }
        if let Some((target, value)) = found {
            if matches!(source.node(value), Node::Table(_)) {
                return Some(target);
            }
            return None;
        }
    }
    if fn_has_goto {
        return None;
    }
    let Node::Block(statements) = source.node(pos.block) else {
        return None;
    };
    let loop_idx = statements.iter().position(|s| *s == pos.stmt)?;
    let mut found: Option<(usize, NodeId, NodeId)> = None;
    for (idx, statement) in statements.iter().enumerate().take(loop_idx) {
        let Node::Assign(targets, values) = source.node(*statement) else {
            continue;
        };
        for (target, value) in targets.iter().zip(values) {
            if matches!(source.node(*target), Node::Name(_))
                && name_bid(resolution, *target) == Some(bid)
            {
                if found.is_some() {
                    return None;
                }
                found = Some((idx, *target, *value));
            }
        }
    }
    let (def_idx, target, value) = found?;
    if !matches!(source.node(value), Node::Table(_)) {
        return None;
    }
    if subtree_has_call_or_table_write(source, statements[def_idx]) {
        return None;
    }
    for statement in &statements[def_idx + 1..loop_idx] {
        if subtree_has_call_or_table_write(source, *statement) {
            return None;
        }
    }
    Some(target)
}

/// Whether the base escapes anywhere in the program: every `Name`
/// node resolving to it must be the object of an `Index` node.
/// Nested functions are descended into, since a closure over the
/// base is an escape route even when it is never called in a loop.
/// `exempt` is a global's defining assignment target, which is a
/// `Name` node by construction but is not an escape.
fn base_escapes(
    source: &Ast,
    resolution: &Resolution,
    root: NodeId,
    bid: BindingId,
    exempt: Option<NodeId>,
) -> bool {
    match exempt {
        Some(target) => base_escapes_exempting(source, resolution, root, bid, &[target]),
        None => base_escapes_exempting(source, resolution, root, bid, &[]),
    }
}

/// `base_escapes` with a set of exempt nodes (the defining targets
/// of a multi-definition global, see `base_proven_write_use`).
fn base_escapes_exempting(
    source: &Ast,
    resolution: &Resolution,
    root: NodeId,
    bid: BindingId,
    exempt: &[NodeId],
) -> bool {
    fn visit(
        source: &Ast,
        resolution: &Resolution,
        bid: BindingId,
        node: NodeId,
        is_index_obj: bool,
        escaped: &mut bool,
        exempt: &[NodeId],
    ) {
        if *escaped {
            return;
        }
        if exempt.contains(&node) {
            return;
        }
        if matches!(source.node(node), Node::Name(_))
            && !is_index_obj
            && name_bid(resolution, node) == Some(bid)
        {
            *escaped = true;
            return;
        }
        if let Node::Index(obj, key, _) = source.node(node) {
            visit(source, resolution, bid, *obj, true, escaped, exempt);
            visit(source, resolution, bid, *key, false, escaped, exempt);
            return;
        }
        let mut children = Vec::new();
        ast_utils::for_each_child(source, node, &mut |child| children.push(child));
        for child in children {
            visit(source, resolution, bid, child, false, escaped, exempt);
        }
    }
    let mut escaped = false;
    visit(source, resolution, bid, root, false, &mut escaped, exempt);
    escaped
}

/// The base proof: a `local` with no recorded writes whose
/// declaration initializer is a table constructor, in a
/// goto/label-free enclosing function, and never escaping; or a
/// global with a single table-constructor defining assignment
/// (chunk-top or same-block form, see `global_table_def_target`),
/// and never escaping except for that definition itself.
pub(crate) fn base_proven(
    source: &Ast,
    resolution: &Resolution,
    root: NodeId,
    bid: BindingId,
    fn_has_goto: bool,
    pos: LoopPos,
) -> bool {
    let Some(binding) = resolution.bindings.get(bid as usize) else {
        return false;
    };
    let exempt = match binding.kind {
        BindingKind::Local => {
            if fn_has_goto {
                return false;
            }
            if resolution
                .binding_write_counts
                .get(bid as usize)
                .copied()
                .unwrap_or(1)
                != 0
            {
                return false;
            }
            let Some(decl) = binding.decl_node else {
                return false;
            };
            let Node::Local(_, exprs) = source.node(decl) else {
                return false;
            };
            let Some(idx) = resolution
                .node_bids
                .get(decl as usize)
                .and_then(|bids| bids.iter().position(|b| *b == bid))
            else {
                return false;
            };
            let Some(init) = exprs.get(idx) else {
                return false;
            };
            if !matches!(source.node(*init), Node::Table(_)) {
                return false;
            }
            None
        }
        BindingKind::Global => {
            let Some(target) =
                global_table_def_target(source, resolution, root, pos, fn_has_goto, bid)
            else {
                return false;
            };
            Some(target)
        }
        _ => return false,
    };
    !base_escapes(source, resolution, root, bid, exempt)
}

/// Every defining target of a global whose recorded writes are
/// two or more assignments, all of them table constructors, or
/// `None` when the binding does not have exactly that shape.
///
/// The enumeration is strict: it walks the whole chunk (nested
/// functions included, since a global can be written from any body)
/// and pairs each assignment target with the value at the same
/// position. A target past the end of the value list receives `nil`
/// (or a call/vararg expansion value), never the constructor, so it
/// fails the shape; a `Funcstat` write is a function value and also
/// fails. The enumerated count must equal the resolver's recorded
/// write count, so no write can hide from the enumeration.
pub(crate) fn global_all_table_def_targets(
    source: &Ast,
    resolution: &Resolution,
    root: NodeId,
    bid: BindingId,
) -> Option<Vec<NodeId>> {
    let recorded = resolution
        .binding_write_counts
        .get(bid as usize)
        .copied()
        .unwrap_or(0);
    if recorded < 2 {
        return None;
    }
    let mut targets: Vec<NodeId> = Vec::new();
    let mut counted = 0u32;
    let mut ok = true;
    ast_utils::walk(source, root, &mut |node| match source.node(node) {
        Node::Assign(assign_targets, values) => {
            for (idx, target) in assign_targets.iter().enumerate() {
                if matches!(source.node(*target), Node::Name(_))
                    && name_bid(resolution, *target) == Some(bid)
                {
                    counted += 1;
                    targets.push(*target);
                    match values.get(idx) {
                        Some(value) if matches!(source.node(*value), Node::Table(_)) => {}
                        _ => ok = false,
                    }
                }
            }
        }
        Node::Funcstat(target, _)
            if matches!(source.node(*target), Node::Name(_))
                && name_bid(resolution, *target) == Some(bid) =>
        {
            counted += 1;
            ok = false;
        }
        _ => {}
    });
    if !ok || counted != recorded {
        return None;
    }
    Some(targets)
}

/// The base proof for *field writes* (S1's `write_base_harmless`):
/// everything `base_proven` admits, plus a global with several
/// table-constructor definitions (`global_all_table_def_targets`)
/// that never escapes except through those definitions.
///
/// A field write through the base is harmless under any of the
/// definitions: whichever executed last, the value written through
/// is a fresh plain table built by one of them (or the write runs
/// before any definition, on `nil`, and errors exactly as it did
/// before the transform -- S1 never moves, adds, or removes a
/// write). Because the table never escapes, no alias or
/// `setmetatable` route can make the write reach a builtin root
/// field.
///
/// Read uses must not use this variant: with several definitions,
/// which table the base denotes can change when a later definition
/// executes, so a cached member *read* could go stale. The member
/// -read passes (`loop_member_reads`, `straightline_member_reads`)
/// keep the single-definition `base_proven`.
pub(crate) fn base_proven_write_use(
    source: &Ast,
    resolution: &Resolution,
    root: NodeId,
    bid: BindingId,
    fn_has_goto: bool,
    pos: LoopPos,
) -> bool {
    if base_proven(source, resolution, root, bid, fn_has_goto, pos) {
        return true;
    }
    let Some(binding) = resolution.bindings.get(bid as usize) else {
        return false;
    };
    if !matches!(binding.kind, BindingKind::Global) {
        return false;
    }
    let Some(targets) = global_all_table_def_targets(source, resolution, root, bid) else {
        return false;
    };
    !base_escapes_exempting(source, resolution, root, bid, &targets)
}

/// Parent map for a subtree, built from `body` downward. Reads
/// inside nested functions never occur in a loop scan, and any
/// chain passing through a function node is rejected by
/// `head_position_ok` anyway, so no descent filtering is needed.
fn parent_map(source: &Ast, body: NodeId) -> HashMap<NodeId, NodeId> {
    let mut map = HashMap::new();
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        let mut children = Vec::new();
        ast_utils::for_each_child(source, node, &mut |child| children.push(child));
        for child in children {
            map.insert(child, node);
            stack.push(child);
        }
    }
    map
}

/// Ancestor chain from `body` down to `node`, inclusive at both
/// ends: `chain[0]` is the body block, `chain[1]` is the top-level
/// body-block statement containing the node, and the last element
/// is the node itself. `None` when the node is not under the body.
fn ancestor_chain(
    parents: &HashMap<NodeId, NodeId>,
    body: NodeId,
    node: NodeId,
) -> Option<Vec<NodeId>> {
    let mut chain = vec![node];
    let mut current = node;
    while current != body {
        let parent = parents.get(&current)?;
        current = *parent;
        chain.push(current);
    }
    chain.reverse();
    Some(chain)
}

/// Whether the read at the end of `chain` is evaluated
/// unconditionally when the top-level body-block statement
/// (`chain[1]`) starts executing, so a cache local inserted before
/// that statement evaluates exactly where the first read would.
/// Each parent-to-child step must be a head position: statement
/// heads (local initializers, plain-name assignment values, call
/// statements, returns, `if`/`while` conditions), transparent
/// wrappers (`do` blocks, bare blocks), and expression positions
/// whose evaluation precedes any branching. Branch bodies, loop
/// bodies, `repeat` conditions, nested loop headers' bodies, and
/// function bodies are never head positions. A short-circuit
/// (`and`/`or`) right operand is conditional, so only its left
/// operand qualifies.
fn head_position_ok(source: &Ast, chain: &[NodeId]) -> bool {
    for pair in chain.windows(2) {
        let (parent, child) = (pair[0], pair[1]);
        let ok = match source.node(parent) {
            Node::Block(_) => true,
            Node::Do(_) => true,
            Node::Local(_, exprs) => exprs.contains(&child),
            Node::Assign(targets, values) => {
                values.contains(&child)
                    && targets
                        .iter()
                        .all(|t| matches!(source.node(*t), Node::Name(_)))
            }
            Node::Callstat(expr) => *expr == child,
            Node::Return(exprs) => exprs.contains(&child),
            Node::If(arms, _) => arms.iter().any(|arm| arm.cond == child),
            Node::While(cond, _) => *cond == child,
            Node::Call(func, args, _) => *func == child || args.contains(&child),
            Node::Index(obj, key, _) => *obj == child || *key == child,
            Node::Methodname(obj, _) => *obj == child,
            Node::Bin(op, left, right) => {
                if op == "and" || op == "or" {
                    *left == child
                } else {
                    *left == child || *right == child
                }
            }
            Node::Un(_, operand) => *operand == child,
            Node::Paren(inner) => *inner == child,
            Node::Table(fields) => fields.iter().any(|f| match f {
                TableField::Arr(v) => *v == child,
                TableField::Name(_, v) => *v == child,
                TableField::KVar(k, v) => *k == child || *v == child,
            }),
            Node::Fornum(_, a, b, c, _) => {
                *a == child || *b == child || c.map(|c| c == child).unwrap_or(false)
            }
            Node::Forin(_, exprs, _) => exprs.contains(&child),
            // Repeat bodies and conditions, loop bodies, function
            // bodies, and declaration targets are never head
            // positions for a body-entry cache.
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    true
}

/// Children of `node` in evaluation order, for the
/// calls-before-target walk. Control bodies are not traversed:
/// a call inside a branch or loop body does not complete before a
/// head-position read, and head-position reads never sit inside
/// such bodies. Function bodies never execute at the definition
/// point, so they are leaves.
fn eval_children(source: &Ast, node: NodeId) -> Vec<NodeId> {
    match source.node(node) {
        Node::While(cond, _) => vec![*cond],
        Node::Repeat(_, _) => Vec::new(),
        Node::If(arms, _) => arms.iter().map(|arm| arm.cond).collect(),
        Node::Function(..) => Vec::new(),
        _ => {
            let mut children = Vec::new();
            ast_utils::for_each_child(source, node, &mut |child| children.push(child));
            children
        }
    }
}

/// Whether `node`'s subtree contains a call, excluding nested
/// function bodies (whose calls do not run at this point).
fn subtree_has_call(source: &Ast, node: NodeId) -> bool {
    let mut found = false;
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if found {
            break;
        }
        if current != node && matches!(source.node(current), Node::Function(..)) {
            continue;
        }
        if matches!(source.node(current), Node::Call(..)) {
            found = true;
            break;
        }
        let mut children = Vec::new();
        ast_utils::for_each_child(source, current, &mut |child| children.push(child));
        for child in children {
            stack.push(child);
        }
    }
    found
}

enum Walk {
    Found(bool),
    Missing,
}

/// Walk `node` in evaluation order. Returns `Found(true)` when a
/// call completes before `target` is reached, `Found(false)` when
/// the target is reached with no earlier completed call, and
/// `Missing` when the target is not under this node. A call counts
/// as completed once its whole subtree has been visited without
/// finding the target; ancestor calls of the target execute after
/// it (their arguments are evaluated first), so they never count.
fn walk_calls_before(source: &Ast, node: NodeId, target: NodeId) -> Walk {
    if node == target {
        return Walk::Found(false);
    }
    if matches!(source.node(node), Node::Function(..)) {
        return Walk::Missing;
    }
    let mut completed = false;
    for child in eval_children(source, node) {
        match walk_calls_before(source, child, target) {
            Walk::Found(c) => return Walk::Found(completed || c),
            Walk::Missing => {
                if subtree_has_call(source, child) {
                    completed = true;
                }
            }
        }
    }
    Walk::Missing
}

/// Whether any call completes before `target` inside statement
/// `stmt` (evaluation order). A completed call before the first
/// read would run host side effects that the cache-first order
/// would skip on the error path, so such groups are disqualified.
fn calls_before_target(source: &Ast, stmt: NodeId, target: NodeId) -> bool {
    matches!(walk_calls_before(source, stmt, target), Walk::Found(true))
}

/// The for-variable base proof. Returns the body-block statement
/// before which a per-iteration cache local may be inserted, or
/// `None` when the group does not qualify. The binding must be a
/// `for` variable in a goto/label-free enclosing function, never
/// written anywhere, and never escaping (every `Name` resolving to
/// it must be an index object, which also closes the assignment
/// and alias routes). The group's first read (scan order) must sit
/// in a head position of its top-level body-block statement with no
/// call completing before it; later reads are then dominated by
/// the body-block cache local, which executes once per iteration
/// at exactly the first read's point.
fn for_base_insertion(
    source: &Ast,
    resolution: &Resolution,
    root: NodeId,
    bid: BindingId,
    fn_has_goto: bool,
    body: NodeId,
    occurrences: &[NodeId],
) -> Option<NodeId> {
    let binding = resolution.bindings.get(bid as usize)?;
    if binding.kind != BindingKind::For {
        return None;
    }
    if fn_has_goto {
        return None;
    }
    if resolution
        .binding_write_counts
        .get(bid as usize)
        .copied()
        .unwrap_or(1)
        != 0
    {
        return None;
    }
    if base_escapes(source, resolution, root, bid, None) {
        return None;
    }
    if !matches!(source.node(body), Node::Block(_)) {
        return None;
    }
    let first = *occurrences.first()?;
    let parents = parent_map(source, body);
    let chain = ancestor_chain(&parents, body, first)?;
    if chain.len() < 2 {
        return None;
    }
    if !head_position_ok(source, &chain) {
        return None;
    }
    if calls_before_target(source, chain[1], first) {
        return None;
    }
    Some(chain[1])
}

/// Clone a subtree from `source` into `output` with fresh node ids.
/// Whether a generic-for loop's implicit iterator calls bar the
/// header cache. The iterator runs once per step, between body
/// executions, so a header cache computed once before the loop
/// goes stale if the iterator writes any table field. The write
/// may hit the cached base directly (a captured base), through
/// the state or control value passed to the iterator, or through
/// an alias, so the scan is deliberately coarse: any field write
/// in the iterator body bars the header cache for every group,
/// and only a directly known iterator function is admitted at
/// all. Anything else (a field, a call result, a parameter, a
/// reassigned name) may invoke an unknown function each step.
/// Proven builtin iterators are exempt: a `pairs` / `ipairs`
/// call yields a `next`-family iterator and a bare `next` name
/// iterates directly; neither can write a table field (the
/// effects analyzer classifies a proven builtin iterator like a
/// direct builtin call). A shadowed spelling does not resolve to
/// the builtin and stays barred by the checks below.
fn forin_iterator_bars_header(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    resolution: &Resolution,
    expressions: &[NodeId],
) -> bool {
    let Some(&first) = expressions.first() else {
        return true;
    };
    // Proven builtin iterators are inert (see the doc comment):
    // they resolve through the API-alias analysis, so a shadowed
    // spelling never takes this arm.
    match source.node(first).clone() {
        Node::Call(function, _, method) => {
            if method.is_none() {
                if let Some(builtin) = analyzer.resolve_builtin_reference(function) {
                    if matches!(builtin.as_str(), "pairs" | "ipairs") {
                        return false;
                    }
                }
            }
            return true;
        }
        Node::Name(_) => {
            if let Some(builtin) = analyzer.resolve_builtin_reference(first) {
                if builtin == "next" {
                    return false;
                }
            }
        }
        _ => return true,
    }
    let Some(bid) = name_bid(resolution, first) else {
        return true;
    };
    let Some(binding) = resolution.bindings.get(bid as usize) else {
        return true;
    };
    let writes = resolution
        .binding_write_counts
        .get(bid as usize)
        .copied()
        .unwrap_or(1);
    let func = if let Some(func) = binding.function_node {
        // A Localfunc definition is not a write; a Funcstat
        // definition is exactly one write. Any other count means
        // the name was reassigned, so the function value at loop
        // time is not the recorded definition.
        let single = match binding.kind {
            BindingKind::Localfunc => writes == 0,
            _ => writes == 1,
        };
        if !single {
            return true;
        }
        func
    } else if binding.kind == BindingKind::Local {
        // `local it = function() ... end`: the initializer at the
        // binding's position in its declaration statement.
        if writes != 0 {
            return true;
        }
        let Some(decl) = binding.decl_node else {
            return true;
        };
        let Node::Local(_, exprs) = source.node(decl) else {
            return true;
        };
        let Some(idx) = resolution
            .node_bids
            .get(decl as usize)
            .and_then(|bids| bids.iter().position(|b| *b == bid))
        else {
            return true;
        };
        let Some(init) = exprs.get(idx) else {
            return true;
        };
        if !matches!(source.node(*init), Node::Function(..)) {
            return true;
        }
        *init
    } else {
        return true;
    };
    let Node::Function(_, _, body) = source.node(func) else {
        return true;
    };
    iterator_body_has_hazard(source, analyzer, *body)
}

/// Walk an iterator function body for anything that bars the
/// header cache: a field write, an unknown/stateful call, a
/// goto/label, or a nested generic-for (whose own implicit
/// iterator call is invisible to this walk). Nested function
/// bodies are boundaries: they do not run during the iterator
/// call, and a call to one is itself a Call node above, hence a
/// hazard unless it is a known builtin.
fn iterator_body_has_hazard(source: &Ast, analyzer: &EffectAnalyzer<'_>, node: NodeId) -> bool {
    if matches!(source.node(node), Node::Function(..)) {
        return false;
    }
    match source.node(node).clone() {
        Node::Assign(targets, _) => {
            if targets
                .iter()
                .any(|t| matches!(source.node(*t), Node::Index(..) | Node::Methodname(..)))
            {
                return true;
            }
        }
        Node::Funcstat(target, _) => {
            if matches!(source.node(target), Node::Index(..) | Node::Methodname(..)) {
                return true;
            }
        }
        Node::Call(..) => {
            if call_is_barrier(analyzer, node, source) {
                return true;
            }
        }
        Node::Goto(_) | Node::Label(_) => return true,
        Node::Forin(..) => return true,
        _ => {}
    }
    let mut children = Vec::new();
    ast_utils::for_each_child(source, node, &mut |child| children.push(child));
    children
        .iter()
        .any(|child| iterator_body_has_hazard(source, analyzer, *child))
}

/// Whether a loop body subtree contains a generic-for whose
/// implicit iterator calls bar caching for the enclosing loop
/// (cycle 95, Codex finding 6, MR-header form): the enclosing
/// loop claims reads in its whole subtree before any inner loop
/// can claim them, and the nested generic-for's iterator runs
/// between the enclosing loop's body executions -- and can sit
/// between a body-entry cache insertion and a later read within
/// one iteration -- while its calls are invisible to the
/// enclosing loop's own scan. Nested function bodies are
/// boundaries, mirroring `scan_node`: they do not run during the
/// enclosing loop unless called, and a call to one in the body
/// is itself a barrier that disqualifies the loop.
fn body_contains_barring_forin(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    resolution: &Resolution,
    node: NodeId,
) -> bool {
    if matches!(source.node(node), Node::Function(..)) {
        return false;
    }
    if let Node::Forin(_, exprs, _) = source.node(node) {
        if forin_iterator_bars_header(source, analyzer, resolution, exprs) {
            return true;
        }
    }
    let mut children = Vec::new();
    ast_utils::for_each_child(source, node, &mut |child| children.push(child));
    children
        .iter()
        .any(|child| body_contains_barring_forin(source, analyzer, resolution, *child))
}

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

/// Allocate the cache local's name: the shortest single-letter
/// identifier absent from the program's string table, mirroring S1.
fn fresh_cache_symbol(output: &mut Ast) -> SymbolId {
    for candidate in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ".chars() {
        let name = candidate.to_string();
        if !output.strings.contains(&name) {
            return output.strings.intern(&name);
        }
    }
    output.strings.fresh("__memberreadcache")
}

struct Transformer<'a> {
    source: &'a Ast,
    analyzer: &'a EffectAnalyzer<'a>,
    resolution: &'a Resolution,
    root: NodeId,
    output: Ast,
    cached_groups: usize,
    replaced: usize,
    /// Index nodes already cached by an outer loop (top-down claiming).
    claimed: HashSet<NodeId>,
    /// Blocks on the current transform path (innermost last); the
    /// top is the block directly containing the loop being analyzed.
    block_stack: Vec<NodeId>,
    /// Enclosing function bodies on the current transform path
    /// (innermost last); empty at the chunk top level.
    fn_stack: Vec<NodeId>,
    fn_params: Vec<usize>,
    fn_local_max: std::collections::HashMap<NodeId, usize>,
    fn_inserted: std::collections::HashMap<NodeId, usize>,
    /// Base proofs, keyed by (binding, enclosing-function goto flag,
    /// loop block, loop statement): the global same-block form
    /// depends on the loop's position.
    proof_cache: HashMap<(BindingId, bool, NodeId, NodeId), bool>,
    /// Per-iteration cache locals pending insertion into a loop
    /// body block: body block id -> (insertion statement, cache
    /// local), in group order. Consumed by `transform_block`.
    pending_body_inserts: HashMap<NodeId, Vec<(NodeId, NodeId)>>,
}

impl<'a> Transformer<'a> {
    fn new(
        source: &'a Ast,
        root: NodeId,
        resolution: &'a Resolution,
        effects: &'a EffectAnalyzer<'a>,
    ) -> Self {
        Self {
            source,
            analyzer: effects,
            resolution,
            root,
            output: clone_source(source),
            cached_groups: 0,
            replaced: 0,
            claimed: HashSet::new(),
            block_stack: Vec::new(),
            fn_stack: Vec::new(),
            fn_params: Vec::new(),
            fn_local_max: HashMap::new(),
            fn_inserted: HashMap::new(),
            proof_cache: HashMap::new(),
            pending_body_inserts: HashMap::new(),
        }
    }

    fn proven(&mut self, bid: BindingId, fn_has_goto: bool, pos: LoopPos) -> bool {
        let key = (bid, fn_has_goto, pos.block, pos.stmt);
        if let Some(proven) = self.proof_cache.get(&key) {
            return *proven;
        }
        let proven = base_proven(
            self.source,
            self.resolution,
            self.root,
            bid,
            fn_has_goto,
            pos,
        );
        self.proof_cache.insert(key, proven);
        proven
    }

    /// Build a cache local for one group (`local c = <first read>`)
    /// and rewrite every occurrence to the cache name. Returns the
    /// cache local node; the caller places it (loop preheader or
    /// loop body block). Callers pass at least two occurrences.
    fn make_cache(&mut self, occurrences: &[NodeId]) -> Option<NodeId> {
        let symbol = fresh_cache_symbol(&mut self.output);
        let template = occurrences[0];
        let cloned = clone_subtree(&mut self.output, self.source, template);
        let mut cloned_nodes = Vec::new();
        ast_utils::walk(&self.output, cloned, &mut |id| cloned_nodes.push(id));
        for id in cloned_nodes {
            self.output.nodes.mark_synthetic(id, REASON);
        }
        let local = self.output.push(Node::Local(vec![symbol], vec![cloned]));
        self.output.nodes.mark_synthetic(local, REASON);
        for occurrence in occurrences {
            self.output
                .nodes
                .rewrite(*occurrence, Node::Name(symbol), REASON);
            super::origins::derive(
                &mut self.output,
                *occurrence,
                self.source,
                &[*occurrence],
                REASON,
            );
            self.claimed.insert(*occurrence);
        }
        self.cached_groups += 1;
        self.replaced += occurrences.len();
        Some(local)
    }

    fn transform_block(&mut self, block: NodeId) {
        let Node::Block(statements) = self.source.node(block).clone() else {
            return;
        };
        self.block_stack.push(block);
        // Per-iteration (for-base) cache locals for this body
        // block, inserted before their statements in statement
        // order. The insertion statement is a direct child of this
        // block by construction, so every entry is consumed here.
        let pending = self.pending_body_inserts.remove(&block).unwrap_or_default();
        let mut rewritten = Vec::with_capacity(statements.len());
        for statement in &statements {
            for (insert_stmt, local) in &pending {
                if *insert_stmt == *statement {
                    rewritten.push(*local);
                }
            }
            rewritten.append(&mut self.transform_statement(*statement));
        }
        self.block_stack.pop();
        if rewritten != statements {
            self.output
                .nodes
                .rewrite(block, Node::Block(rewritten), REASON);
        }
    }

    fn transform_statement(&mut self, statement: NodeId) -> Vec<NodeId> {
        match self.source.node(statement).clone() {
            Node::Block(_) => {
                self.transform_block(statement);
                vec![statement]
            }
            Node::Do(body) => {
                self.transform_block(body);
                vec![statement]
            }
            Node::While(condition, body) => self.transform_loop(statement, &[condition], body),
            Node::Repeat(body, condition) => self.transform_loop(statement, &[condition], body),
            Node::If(arms, otherwise) => {
                for arm in arms {
                    self.transform_expr(arm.cond);
                    self.transform_block(arm.body);
                }
                if let Some(body) = otherwise {
                    self.transform_block(body);
                }
                vec![statement]
            }
            Node::Fornum(_, start, end, step, body) => {
                let mut headers = vec![start, end];
                if let Some(step) = step {
                    headers.push(step);
                }
                self.transform_loop(statement, &headers, body)
            }
            Node::Forin(_, expressions, body) => self.transform_loop(statement, &expressions, body),
            Node::Funcstat(target, function) => {
                self.transform_expr(target);
                if let Node::Function(params, _, body) = self.source.node(function).clone() {
                    // A method definition's implicit `self` is a real
                    // parameter at load time; count it in the budget
                    // context (the Function node cannot see the
                    // Funcstat target). Same accounting as Round 218
                    // (LICM / carrier synthesis); found by the w6
                    // cycle-52 completion audit (a1/a7/a10 shapes).
                    let extra =
                        usize::from(matches!(self.source.node(target), Node::Methodname(..)));
                    self.fn_stack.push(body);
                    self.fn_params.push(params.len() + extra);
                    self.transform_block(body);
                    self.fn_params.pop();
                    self.fn_stack.pop();
                } else {
                    self.transform_expr(function);
                }
                vec![statement]
            }
            Node::Localfunc(_, function) => {
                self.transform_expr(function);
                vec![statement]
            }
            Node::Local(_, expressions) | Node::Return(expressions) => {
                for expression in expressions {
                    self.transform_expr(expression);
                }
                vec![statement]
            }
            Node::Callstat(expression) => {
                self.transform_expr(expression);
                vec![statement]
            }
            Node::Assign(targets, expressions) => {
                for target in targets {
                    self.transform_expr(target);
                }
                for expression in expressions {
                    self.transform_expr(expression);
                }
                vec![statement]
            }
            _ => vec![statement],
        }
    }

    fn transform_loop(
        &mut self,
        statement: NodeId,
        headers: &[NodeId],
        body: NodeId,
    ) -> Vec<NodeId> {
        // Top-down claiming: analyse this loop before transforming
        // its body, so it claims reads in its whole subtree
        // (including nested loops) before any inner loop can claim
        // them. When this loop is disqualified, nothing is claimed
        // and the body transform below still lets inner loops cache
        // on their own.
        let fn_body = self.fn_stack.last().copied().unwrap_or(self.root);
        let fn_has_goto = subtree_has_goto_label(self.source, fn_body);
        let pos = LoopPos {
            block: self.block_stack.last().copied().unwrap_or(self.root),
            stmt: statement,
        };
        let groups = analyze_loop(self.source, self.analyzer, self.resolution, body, headers);
        // Cycle 95 (Codex finding 6, MR-header form, enclosing
        // loops): this loop claims reads in its whole subtree, so
        // a nested generic-for whose iterator bars its own header
        // cache bars this loop's caches (preheader and body-entry)
        // as well; inner loops still cache on their own below.
        let body_bars_cache =
            body_contains_barring_forin(self.source, self.analyzer, self.resolution, body);
        let groups = if body_bars_cache { None } else { groups };
        // Cycle 95 (Codex finding 6, MR-header form): a
        // generic-for loop's implicit iterator calls run between
        // body executions, so the preheader cache is valid only
        // when the iterator is proven field-write-free (see
        // `forin_iterator_bars_header`). The per-iteration
        // body-entry cache below is unaffected.
        let forin_bars_header = match self.source.node(statement) {
            Node::Forin(..) => {
                forin_iterator_bars_header(self.source, self.analyzer, self.resolution, headers)
            }
            _ => false,
        };
        // Cycle 94 (Codex finding 7, MR form): per-function local
        // budget for cache insertions, mirroring the S1 check in
        // single_member_cache (w6 cycle 47 found functions already
        // at 200 active locals, where one cache local makes the
        // generated code fail to load).
        let fn_params = self.fn_params.last().copied().unwrap_or(0);
        let source = self.source;
        let local_max = *self.fn_local_max.entry(fn_body).or_insert_with(|| {
            super::single_member_cache::max_active_locals(source, fn_params, fn_body)
        });
        let mut inserted = self.fn_inserted.get(&fn_body).copied().unwrap_or(0);
        let mut caches = Vec::new();
        if let Some(groups) = groups {
            for group in groups {
                let occurrences: Vec<NodeId> = group
                    .occurrences
                    .into_iter()
                    .filter(|occurrence| !self.claimed.contains(occurrence))
                    .collect();
                if occurrences.len() < 2 {
                    continue;
                }
                if local_max + inserted + 1 > super::single_member_cache::MAX_LOCALS {
                    continue;
                }
                if !forin_bars_header && self.proven(group.bid, fn_has_goto, pos) {
                    if let Some(local) = self.make_cache(&occurrences) {
                        caches.push(local);
                        inserted += 1;
                    }
                } else if let Some(insert_stmt) = for_base_insertion(
                    self.source,
                    self.resolution,
                    self.root,
                    group.bid,
                    fn_has_goto,
                    body,
                    &occurrences,
                ) {
                    // Per-iteration cache: the local is inserted
                    // into the loop body block by transform_block
                    // below, before the first read's statement.
                    if let Some(local) = self.make_cache(&occurrences) {
                        self.pending_body_inserts
                            .entry(body)
                            .or_default()
                            .push((insert_stmt, local));
                        inserted += 1;
                    }
                }
            }
        }
        self.fn_inserted.insert(fn_body, inserted);
        for header in headers {
            self.transform_expr(*header);
        }
        self.transform_block(body);
        caches.push(statement);
        caches
    }

    fn transform_expr(&mut self, node: NodeId) {
        match self.source.node(node).clone() {
            Node::Function(params, _, body) => {
                self.fn_stack.push(body);
                self.fn_params.push(params.len());
                self.transform_block(body);
                self.fn_stack.pop();
                self.fn_params.pop();
            }
            Node::Block(_) => self.transform_block(node),
            _ => {
                let mut children = Vec::new();
                ast_utils::for_each_child(self.source, node, &mut |child| children.push(child));
                for child in children {
                    self.transform_expr(child);
                }
            }
        }
    }
}

/// Shared implementation for the post-alias entry point. Returns
/// `cached=N` (groups) and `replaced=M` (read sites) in `details`
/// (0 when nothing changed, in which case `ast` is left untouched).
fn run_cached(ast: &mut Ast, root: NodeId, shared: Option<&SpeedAnalysis<'_>>) -> PassResult {
    if !matches!(ast.node(root), Node::Block(_)) {
        return PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["cached=0".to_string()]),
        };
    }
    let source = clone_source(ast);
    let resolution;
    let effects;
    let (resolution, effects) = match shared {
        Some(shared) => (shared.resolution, shared.effects),
        None => {
            resolution = resolve(&source, root);
            effects = EffectAnalyzer::new(&source, &resolution, root, true);
            (&resolution, &effects)
        }
    };
    let mut transformer = Transformer::new(&source, root, resolution, effects);
    transformer.transform_block(root);
    let cached_groups = transformer.cached_groups;
    let replaced = transformer.replaced;
    if cached_groups == 0 {
        return PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["cached=0".to_string()]),
        };
    }
    *ast = transformer.output;
    PassResult {
        root,
        saved: None,
        details: Some(vec![
            format!("cached={cached_groups}"),
            format!("replaced={replaced}"),
        ]),
    }
}

/// Cache repeated loop member reads, post-alias form: runs on an
/// api-aliased layout, where base names are the layout's own locals
/// (see module docs).
pub fn cache_loop_member_reads(ast: &mut Ast, root: NodeId) -> PassResult {
    run_cached(ast, root, None)
}

/// Form over a caller-built shared analysis (see
/// `passes::speed_analysis`): `analysis` must have been built over
/// an AST whose node and string arenas are identical to `ast`'s
/// (a verbatim clone qualifies). The result is identical to
/// [`cache_loop_member_reads`]; only the repeated analysis
/// construction is saved.
pub fn cache_loop_member_reads_with_analysis(
    ast: &mut Ast,
    root: NodeId,
    analysis: &SpeedAnalysis<'_>,
) -> PassResult {
    run_cached(ast, root, Some(analysis))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn run(source: &str) -> (String, usize) {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = cache_loop_member_reads(&mut ast, root);
        let code = Printer::new(&ast, false).output(result.root);
        let cached = result
            .details
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.strip_prefix("cached="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        (code, cached)
    }

    fn run_shared(source: &str) -> (String, usize) {
        let (ast, root) = parse_source(source).expect("parse");
        let resolution = resolve(&ast, root);
        let effects = EffectAnalyzer::new(&ast, &resolution, root, true);
        let mutated = storm_lua_analysis::effects::mutated_builtin_roots(&ast, &resolution, root);
        let analysis = SpeedAnalysis {
            resolution: &resolution,
            effects: &effects,
            mutated: &mutated,
        };
        let mut shared_ast = ast.clone();
        let result = cache_loop_member_reads_with_analysis(&mut shared_ast, root, &analysis);
        let code = Printer::new(&shared_ast, false).output(result.root);
        let cached = result
            .details
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.strip_prefix("cached="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        (code, cached)
    }

    #[test]
    fn shared_analysis_matches_standalone() {
        // See the S1 test of the same name: the shared analysis is a
        // pure function of the arena contents, so results must match
        // the standalone entry exactly.
        let firing = "function onTick() local t={x=1} while true do local a=t.x local b=t.x if a==b then break end end end";
        let standalone = run(firing);
        assert!(standalone.1 > 0, "input must fire MR: {standalone:?}");
        assert_eq!(run_shared(firing), standalone);
        let quiet = "function onTick() local x=1 end";
        assert_eq!(run_shared(quiet), run(quiet));
    }

    #[test]
    fn caches_repeated_member_reads_in_while_loop() {
        let (code, cached) = run(
            "function onTick() local t={x=1} while true do local a=t.x local b=t.x if a==b then break end end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t.x").count(), 1, "code: {code}");
        assert!(code.contains("local c=t.x"), "code: {code}");
        let cache_pos = code.find("t.x").expect("cache present");
        let while_pos = code.find("while").expect("loop present");
        assert!(cache_pos < while_pos, "cache must precede the loop: {code}");
    }

    fn many_param_mr_source(n: usize) -> String {
        let params: Vec<String> = (1..=n).map(|i| format!("p{i}")).collect();
        format!(
            "function onTick({}) local t={{x=1}} local u=0 for i=1,10 do u=t.x+t.x end return u end",
            params.join(",")
        )
    }

    #[test]
    fn skips_cache_when_locals_would_exceed_limit() {
        // Cycle 94 (Codex finding 7, MR form found by w6 cycle
        // 47): 194 parameters plus two locals and a numeric for
        // (loop variable + internal controls) already use 200
        // active locals, so one more cache local would make the
        // generated code unloadable. MR must skip the group.
        let (_, cached) = run(&many_param_mr_source(194));
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_when_locals_stay_at_limit() {
        // Boundary control: 193 parameters leave room for exactly
        // one cache local (193 + 2 + 4 + 1 = 200), so caching
        // stays.
        let (code, cached) = run(&many_param_mr_source(193));
        assert_eq!(cached, 1, "code: {code}");
    }

    #[test]
    fn skips_cache_in_method_at_limit_counting_self() {
        // Cycle 95 (w6 cycle-52 audit, a7/k1 shape): 193 explicit
        // parameters plus the implicit `self`, two locals and the
        // numeric-for controls already reach 200 active locals;
        // the cache local would be the 201st. The pre-fix
        // accounting missed `self` (199) and fired.
        let params: Vec<String> = (1..=193).map(|i| format!("p{i}")).collect();
        let source = format!(
            "local obj = {{}} function obj:m({}) local t={{x=1}} local u=0 for i=1,10 do u=t.x+t.x end return u end",
            params.join(",")
        );
        let (_, cached) = run(&source);
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_in_method_one_below_limit_counting_self() {
        // Boundary control: 192 explicit parameters plus `self`,
        // two locals and the for controls peak at 199, leaving
        // room for exactly one cache local.
        let params: Vec<String> = (1..=192).map(|i| format!("p{i}")).collect();
        let source = format!(
            "local obj = {{}} function obj:m({}) local t={{x=1}} local u=0 for i=1,10 do u=t.x+t.x end return u end",
            params.join(",")
        );
        let (code, cached) = run(&source);
        assert_eq!(cached, 1, "code: {code}");
    }

    #[test]
    fn caches_repeated_member_reads_in_fornum_loop() {
        let (code, cached) =
            run("function onTick() local t={x=1} for i=1,10 do local a=t.x local b=t.x end end");
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t.x").count(), 1, "code: {code}");
    }

    #[test]
    fn skips_forin_header_cache_when_iterator_writes_base() {
        // Cycle 95 (Codex finding 6, MR-header form; w6 cycle 47
        // v14-gf-iter-mutate): the iterator runs between body
        // executions and mutates the cached base, so the
        // preheader cache would go stale after the first step.
        let (_, cached) = run(
            "function onTick() local t={1} local function it() t[1]=t[1]+1 if t[1]>3 then return nil end return t[1] end local s=0 for v in it do s=s+t[1]+t[1] end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_forin_when_iterator_writes_only_scalars() {
        // Positive control: a counter iterator writes no table
        // field, so the header cache stays valid.
        let (code, cached) = run(
            "function onTick() local t={1} local c=0 local function it() c=c+1 if c>3 then return nil end return c end local s=0 for v in it do s=s+t[1]+t[1] end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t[1]").count(), 1, "code: {code}");
    }

    #[test]
    fn caches_forin_when_iterator_is_local_function_literal() {
        // `local it = function() ... end` resolves through the
        // declaration initializer, not through `function_node`.
        let (code, cached) = run(
            "function onTick() local t={1} local it=function() return 1 end local s=0 for v in it do s=s+t[1]+t[1] end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t[1]").count(), 1, "code: {code}");
    }

    #[test]
    fn skips_forin_header_cache_when_iterator_calls_unknown() {
        // An unknown call inside the iterator may write any
        // field, so the header cache is barred.
        let (_, cached) = run(
            "function onTick() local t={1} local function it() helper() return t[1] end local s=0 for v in it do s=s+t[1]+t[1] end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_forin_header_cache_when_iterator_is_unknown() {
        // A parameter (or any other non-function value) as the
        // iterator may invoke an unknown function each step.
        let (_, cached) =
            run("function onTick(u) local t={1} local s=0 for v in u do s=s+t[1]+t[1] end end");
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_forin_header_cache_when_iterator_has_nested_forin() {
        // A nested generic-for inside the iterator body performs
        // its own implicit iterator call, invisible to the field
        // scan, so it bars the header cache.
        let (_, cached) = run(
            "function onTick() local t={1} local function it() for v in u do end return 1 end local s=0 for v in it do s=s+t[1]+t[1] end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_forin_header_cache_when_global_iterator_writes_base() {
        // Single-definition global iterator writing the global
        // base: the Funcstat definition is the binding's single
        // write, and its body scan finds the field write.
        let (_, cached) = run(
            "g={0} function it() g[1]=g[1]+1 if g[1]>3 then return nil end return g[1] end local s=0 for i in it do s=s+g[1]+g[1] end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_forin_when_iterator_is_pairs_call() {
        // A `pairs` call over another table yields the inert
        // `next` iterator: reads of a proven unrelated base stay
        // cacheable.
        let (code, cached) = run(
            "function onTick() local u={a=1} local t={1} local s=0 for k,v in pairs(u) do s=s+t[1]+t[1] end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t[1]").count(), 1, "code: {code}");
    }

    #[test]
    fn caches_forin_when_iterator_is_next_name() {
        // The explicit `(next, state)` form is the same inert
        // iterator.
        let (code, cached) = run(
            "function onTick() local u={a=1} local t={1} local s=0 for k,v in next,u do s=s+t[1]+t[1] end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t[1]").count(), 1, "code: {code}");
    }

    #[test]
    fn skips_forin_header_cache_when_pairs_is_shadowed() {
        // A local function named `pairs` does not resolve to the
        // builtin, so the exemption must not fire: its returned
        // iterator mutates the base.
        let (_, cached) = run(
            "function onTick() local u={a=1} local t={1} local function pairs(x) return function() t[1]=t[1]+1 if t[1]>3 then return nil end return t[1] end end local s=0 for k,v in pairs(u) do s=s+t[1]+t[1] end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_enclosing_fornum_cache_when_nested_forin_iterator_writes_base() {
        // Cycle 95 (enclosing propagation): the fornum loop claims
        // the reads inside the nested generic-for's body (top-down
        // claiming), but the nested iterator mutates the base
        // between the fornum's body executions, so the enclosing
        // preheader cache would go stale.
        let (_, cached) = run(
            "function onTick() local t={1} local s=0 local function it() t[1]=t[1]+1 if t[1]>3 then return nil end return t[1] end for i=1,2 do for v in it do s=s+t[1]+t[1] end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_enclosing_while_cache_when_nested_forin_iterator_writes_base() {
        // Same propagation through an enclosing while loop.
        let (_, cached) = run(
            "function onTick() local t={1} local s=0 local function it() t[1]=t[1]+1 if t[1]>3 then return nil end return t[1] end local i=0 while i<2 do i=i+1 for v in it do s=s+t[1]+t[1] end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_enclosing_loop_when_nested_forin_iterator_is_hazard_free() {
        // Positive control: a nested iterator that writes only
        // scalars does not bar the enclosing loop's cache.
        let (code, cached) = run(
            "function onTick() local t={1} local s=0 local c=0 local function it() c=c+1 if c>3 then return nil end return c end for i=1,2 do for v in it do s=s+t[1]+t[1] end end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t[1]").count(), 1, "code: {code}");
    }

    #[test]
    fn caches_enclosing_loop_when_nested_forin_uses_pairs() {
        // A nested `pairs` loop is inert (builtin-iterator
        // exemption), so the enclosing loop still caches.
        let (code, cached) = run(
            "function onTick() local u={a=1} local t={1} local s=0 for i=1,2 do for k,v in pairs(u) do s=s+t[1]+t[1] end end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t[1]").count(), 1, "code: {code}");
    }

    #[test]
    fn skips_forin_header_cache_when_iterator_is_call_result_local() {
        // The call-result iterator form (w6 cycle 50
        // f50-mr-callret): in the target pipeline the iterator
        // call is bound to a local before the loop, and a local
        // initialized by a call is not a known function, so the
        // header cache is barred.
        let (_, cached) = run(
            "function onTick() local t={1} local function make() return function() t[1]=t[1]+1 if t[1]>3 then return nil end return t[1] end end local it=make() local s=0 for v in it do s=s+t[1]+t[1] end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_single_occurrence() {
        let (_, cached) = run(
            "function onTick() local t={x=1} while true do local a=t.x if a==1 then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_when_base_reassigned() {
        let (_, cached) = run(
            "function onTick() local t={x=1} t={x=2} while true do local a=t.x local b=t.x end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_when_loop_writes_same_key() {
        let (_, cached) = run(
            "function onTick() local t={x=1} while true do local a=t.x t.x=5 local b=t.x end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_when_loop_writes_through_dynamic_key() {
        // Cycle 94 (Codex finding 2): a write through a key the
        // analysis cannot resolve may hit any key of the base, so
        // it must close the cached group instead of being dropped.
        let (_, cached) = run(
            "function onTick() local t={9} local s=0 for i=1,3 do s=s+t[1]+t[1] t[i]=i end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn preserves_when_loop_writes_another_key() {
        // Control for the dynamic-key fix: a write to a different
        // resolved key still leaves the group intact.
        let (_, cached) = run(
            "function onTick() local t={9} local s=0 for i=1,3 do s=s+t[1]+t[1] t[2]=5 end end",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn equivalent_numeric_key_write_closes_group() {
        // Cycle 94 (Codex finding 3): `t[1.0]` writes the same slot
        // as `t[1]`, so it must close the group; spelling-based
        // keys used to leave the cached read stale.
        let (_, cached) = run(
            "function onTick() local t={9} for i=1,3 do local a=t[1] local b=t[1] t[1.0]=3 end end",
        );
        assert_eq!(cached, 0);
        let (_, cached) = run(
            "function onTick() local t={9} for i=1,3 do local a=t[1] local b=t[1] t[1e0]=3 end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_when_base_escapes() {
        let (_, cached) = run(
            "function onTick() local t={x=1} local u=t while true do local a=t.x local b=t.x end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_when_loop_calls_unknown_function() {
        let (_, cached) = run(
            "function onTick() local t={x=1} while true do foo() local a=t.x local b=t.x end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn keeps_group_when_loop_writes_other_key() {
        let (code, cached) = run(
            "function onTick() local t={x=1,y=2} while true do local a=t.x t.y=5 local b=t.x end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t.x").count(), 1, "code: {code}");
    }

    #[test]
    fn caches_repeated_member_reads_through_chunk_global() {
        let (code, cached) = run(
            "g={x=1} function onTick() while true do local a=g.x local b=g.x if a==b then break end end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("g.x").count(), 1, "code: {code}");
    }

    #[test]
    fn caches_repeated_member_reads_through_function_internal_global() {
        let (code, cached) = run(
            "function onTick() g={x=1} while true do local a=g.x local b=g.x if a==b then break end end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("g.x").count(), 1, "code: {code}");
    }

    #[test]
    fn skips_reassigned_global() {
        let (_, cached) =
            run("g={x=1} g={x=2} function onTick() while true do local a=g.x local b=g.x end end");
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_global_initialized_from_call() {
        let (_, cached) =
            run("g=make() function onTick() while true do local a=g.x local b=g.x end end");
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_function_internal_global_after_loop() {
        let (_, cached) =
            run("function onTick() while true do local a=g.x local b=g.x end g={x=1} end");
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_for_var_base_in_forin_loop() {
        let (code, cached) =
            run("function onTick() for _, v in iter do local a=v.x local b=v.x end end");
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("v.x").count(), 1, "code: {code}");
        assert!(code.contains("local c=v.x"), "code: {code}");
        // The cache sits inside the loop body, before the first read.
        assert!(
            code.find("local c=v.x")
                .is_some_and(|c| code.find("do").is_some_and(|d| c > d)),
            "cache must be inside the loop: {code}"
        );
    }

    #[test]
    fn caches_enclosing_for_var_in_inner_while_loop() {
        // The outer forin loop does not cache (its first read sits
        // inside the nested loop body), but the inner loop caches
        // the outer for variable on its own.
        let (code, cached) = run(
            "function onTick() for _, v in pairs(t) do while true do local a=v.x local b=v.x if a==b then break end end end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("v.x").count(), 1, "code: {code}");
    }

    #[test]
    fn skips_for_var_when_first_read_is_conditional() {
        // The first read sits inside a then-branch: a cache before
        // the if statement could error when the branch is skipped.
        let (_, cached) = run(
            "function onTick() for _, v in iter do if c then local a=v.x end local b=v.x end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_for_var_when_first_read_is_in_nested_loop() {
        let (_, cached) = run(
            "function onTick() for _, v in iter do while true do local a=v.x if a==1 then break end end local b=v.x end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_for_var_when_call_precedes_first_read() {
        let (_, cached) = run(
            "function onTick() for _, v in iter do local a, b = math.max(1, 2), v.x local c=v.x end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_for_var_when_reassigned_in_body() {
        let (_, cached) =
            run("function onTick() for _, v in iter do local a=v.x v={x=2} local b=v.x end end");
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_for_var_when_passed_as_argument() {
        // Escape through a nested closure (not a loop-barrier call:
        // the closure body is never scanned for barriers).
        let (_, cached) = run(
            "function onTick() for _, v in iter do local a=v.x local f=function() return v end local b=v.x end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_for_var_when_call_follows_first_read() {
        // A known-builtin call after the first read does not
        // disqualify: the walk stops at the first read.
        let (code, cached) = run(
            "function onTick() for _, v in iter do local a, b = v.x, math.max(1, 2) local c=v.x end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("v.x").count(), 1, "code: {code}");
    }

    #[test]
    fn caches_for_var_when_first_read_is_in_condition() {
        let (code, cached) =
            run("function onTick() for _, v in iter do if v.x then local a=v.x end end end");
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("v.x").count(), 1, "code: {code}");
    }
}
