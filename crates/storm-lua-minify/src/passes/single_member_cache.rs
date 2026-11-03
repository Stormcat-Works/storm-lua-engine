//! Single member-call caching in loops (target-size layouts only).
//!
//! A member call `B.K(...)` inside a loop pays one table lookup per
//! iteration for the callee alone (in addition to the call itself):
//! the base `B` is loaded and `K` is fetched from it every time.
//! When the same member occurs exactly once in a loop body, caching
//! the function value once before the loop (`local f = B.K`, then
//! `f(...)` at the site) removes that per-iteration lookup. This is
//! the S1 group of the cycle-57 census: a loop-local single
//! occurrence whose member alias was not selected (a member alias
//! needs at least two uses to be profitable, so a total single use
//! can never be alias-covered), which is exactly why the M2
//! (repeated-occurrence) form was rejected in cycle 56 — M2 winners
//! are already chunk-level local calls through the member alias,
//! while S1 winners still pay a per-iteration table lookup.
//!
//! This transform is wired only into target-size layouts
//! (`joint-api-local-s1` and `joint-api-local-licm-s1` in
//! `search.rs`), never into target-free compilation, and it is a
//! layout-construction transform in the LICM sense, not a phase
//! pass. A partial variant caches only the first group in traversal
//! order (see `cache_single_member_calls_post_alias_partial`): when
//! the all-groups S1 layout exceeds the target budget, the
//! single-group layout (about ten characters over its base layout)
//! may still fit, so it is offered as a lower-speed-rank layout
//! between the full S1 layout and its base layout. The subset is
//! deliberately the traversal-first group only; ordering groups by
//! expected benefit would need loop trip-count data that is not
//! available statically, and is left as future work.
//! pass: it is deliberately not registered in the canonical pass
//! registry, so it adds no `effectivePassToggles` entry and no map
//! churn for files whose winner does not use it. The target budget
//! is the profitability gate: a layout that exceeds the target
//! loses to every fitting candidate through the existing
//! over-target demotion in `select_best_candidates`, and the
//! speed rank orders the S1 layouts above their non-S1 counterparts
//! (combined LICM+S1 above LICM-only above local-S1 above local),
//! so an S1 layout can only win when it fits and is the fastest
//! fitting candidate.
//!
//! The transform is intentionally conservative, reusing the
//! cycle-56 proof machinery unchanged:
//! - Only a single-level dotted callee qualifies: the function node
//!   must be `Index(Name, Str, dot = true)` resolving through the
//!   effect analyzer to a builtin member (`screen.drawRectF`,
//!   `math.floor`, …), with the root name spelled exactly as the
//!   builtin root, or through a proven single-definition alias base
//!   in the post-alias form (a chunk-top global assignment, a
//!   local binding with no recorded writes, or -- cycle 65 -- a
//!   same-function, same-block global assignment dominating the
//!   loop). Deeper chains are
//!   excluded: an intermediate field could be nil, and evaluating
//!   the chain once before the loop would then error where the
//!   original code might never run the call (a zero-trip loop or an
//!   untaken branch). A builtin root, by contrast, is a plain host
//!   table installed before the script runs, and a plain-table lookup
//!   never errors (a missing key yields nil), so the preheader
//!   evaluation is side-effect-free and total.
//! - The builtin root must not be mutated anywhere in the program
//!   (`mutated_builtin_roots`, which also covers writes through proven
//!   aliases and passing the table to unknown code), and the root
//!   binding must have no recorded writes, so no write can change what
//!   the cached value points to. A root bound by a `local` inside the
//!   loop body is excluded as well: the preheader sits before the loop,
//!   where such a binding is not in scope.
//! - A loop is skipped entirely if its body subtree (nested loops
//!   included, nested functions excluded) contains an unknown call, a
//!   method call, a stateful builtin (`math.random`, `math.randomseed`,
//!   `async.*`, uncertified `map.*` members, Extended control setters), a table-field
//!   write that could reach a cached member, or a `goto` / label, or
//!   if a host table is passed as an argument. A field write is
//!   harmless -- and does not skip the loop -- only when its base is
//!   proven to be a plain, constructor-built table that never escapes
//!   (the `loop_member_reads` base proof, shared with MR/SMC): such a
//!   table is a distinct object from every builtin root, and with no
//!   escape route no alias or `setmetatable` path can make the write
//!   reach a root field. Writes through a root, a root alias, or any
//!   unproven base still skip the loop. Known builtin calls are
//!   allowed: they write host state or command buffers, never Lua
//!   table fields. A call also counts as known when its callee
//!   spells a builtin member only through never-reassigned locals
//!   -- the localized roots and members the api-alias layouts
//!   themselves create (`local H = string.sub`, a member read
//!   through a localized root): the reference is chased through the
//!   locals' initializers (each binding write-free, so its value
//!   was fixed by the initializer's single evaluation), and the
//!   recovered spelling passes through the same stateful list and
//!   argument rules as a directly spelled call. The chase assumes
//!   nothing the direct resolution does not already assume: a
//!   member value captured at the declaration denotes exactly what
//!   the same spelling denotes at that point. Loop headers are scanned with the same call rules
//!   (a header call to the cached member itself stays uncached, which
//!   is still correct).
//! - A call also counts as known when its callee is a global
//!   function defined in the chunk (cycle 86, candidate B3;
//!   relaxed to B3b in cycle 92): the binding must have exactly
//!   one recorded write -- a plain-name `Funcstat`, in any
//!   position -- in a chunk with no dynamic global access
//!   (`_ENV`, `rawget` / `rawset` / `load` / `loadstring`). No
//!   dominance or direct-statement condition applies: at call
//!   time either that one definition has run, and the body that
//!   executes is its body, or it has not, and the callee is nil
//!   so the call errors exactly as it would unoptimized (S1
//!   moves no calls; the preheader adds only a pure field
//!   read). Every chunk-defined function transitively called
//!   from it must be body-safe under the same vocabulary as the
//!   loop scan (no goto/label, field writes only through proven
//!   plain-table bases at the write's own position, no writes
//!   to builtin or closure-callee bindings, calls only to known
//!   builtins or further closure members under the argument
//!   rule, function values only in definition positions). See
//!   the B3 section comment at `b3_rescue_loop` for the full
//!   conditions.
//! - A call also counts as known when its callee is spelled through
//!   never-reassigned locals and single-definition globals that
//!   dominate the loop (cycle 89, the global-alias chase): each
//!   global step needs exactly one recorded write -- a plain-name
//!   `Assign` -- dominating the loop by the chunk-strict form (a
//!   direct chunk statement before the loop's top-level statement,
//!   no chunk-level goto/label) or the cycle-65 same-block form
//!   (same direct block, preceding, no intervening call or
//!   table-field write, no goto/label in the enclosing function);
//!   local steps are the cycle-84 shape, the depth cap is 8, and
//!   the terminal spelling must come from the analyzer's own
//!   resolution. Chunks with dynamic global access (`_ENV`,
//!   `rawget` / `rawset` / `load` / `loadstring`) are excluded:
//!   their static write set is incomplete, so no global step can
//!   be trusted there. Bare names chase the value itself; index
//!   callees chase the base and reattach the member, but a base
//!   spelling that already contains a dot stays unknown (deeper
//!   chains are excluded, as for direct spellings).
//! - Calls inside a nested function are never collected: such a call
//!   may run after the loop, when the cached value would be stale.
//!   Calls in nested loops are claimed by the outermost qualifying
//!   loop first, exactly like `loop_invariant`, so one preheader
//!   serves every nesting level.
//! - Groups with two or more occurrences in one loop body are left
//!   alone: that is the M2 shape, whose winners are already
//!   alias-covered (cycle 56). Occurrences outside the loop do not
//!   disqualify a loop-local single occurrence: each loop gets its
//!   own preheader, and a straight-line occurrence keeps its
//!   original spelling, so caching is per loop, not per file.
//!
//! The transform follows the `loop_invariant` shape: the output arena
//! starts as a clone of the source (same node ids), the preheader
//! local is inserted in the block that contains the loop, and the
//! occurrence's callee index node is rewritten to the cache local.
//! When nothing is cached the input AST is left untouched.
//!
//! The cache local's name is the shortest single-letter identifier
//! absent from the program's string table (falling back to a
//! generated name when every letter is taken), so one cached group
//! costs about nine characters net: `local X=B.K` (8 + the chain
//! length) minus the chain length saved at the site, plus one
//! statement separator in printed code. A name absent from the
//! table cannot be declared or referenced by user code anywhere, so
//! the inserted local can shadow nothing; groups cached later in
//! the same run see earlier names already interned and take the
//! next free letter. (The cycle-56 form used a generated
//! `__membercache` name, which scope renaming never shortens in
//! layout candidates, costing about 34 characters per group
//! instead; that cost is what kept S1 layouts from fitting budgets
//! they should have fit.)

use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::pass::PassResult;
use crate::passes::loop_member_reads::{base_proven_write_use, LoopPos};
use crate::passes::speed_analysis::SpeedAnalysis;
use storm_lua_analysis::effects::{mutated_builtin_roots, EffectAnalyzer};
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId};
use storm_lua_syntax::ast_utils;

const REASON: &str = "single-member-call-caching";

/// Members certified (user decision 2026-10-31) as pure coordinate
/// transforms: the same inputs give the same outputs within a loop,
/// and the call mutates no Lua-visible state. `map.mapToScreen` was
/// certified by the user's own statement ("mapToScreen is just a
/// coordinate transform; the same inputs give the same outputs");
/// `map.screenToMap` is the inverse transform on the same grounds.
/// Every other `map.*` member stays disqualifying, as do `server.*`
/// and `matrix.*` (not certified). The certification argument is the
/// same shape as the math.random proof sketch: S1 moves no calls,
/// only pure field reads, to the preheader, and every mechanism that
/// could change what a cached field denotes is closed by a separate
/// gate (root-write scan, field-write gate, unknown calls,
/// host-table arguments, method calls).
fn is_certified_pure_map_member(builtin: &str) -> bool {
    builtin == "map.mapToScreen" || builtin == "map.screenToMap"
}

/// Builtins that disqualify a loop outright (stateful or opaque),
/// mirroring `loop_invariant` except for the certified map members
/// above (S1-side only; the LICM copy is unchanged).
fn is_disqualifying_builtin(builtin: &str) -> bool {
    builtin == "math.random"
        || builtin == "math.randomseed"
        || builtin.starts_with("async.")
        || (builtin.starts_with("map.") && !is_certified_pure_map_member(builtin))
        || builtin == "self"
        || builtin.starts_with("self.")
        || builtin.contains("setInput")
        || builtin.contains("setProperty")
}

/// Walk a subtree, visiting every node except nodes inside nested
/// functions. Nested loops are descended into.
fn walk_full<F: FnMut(NodeId)>(source: &Ast, node: NodeId, f: &mut F) {
    fn visit<F: FnMut(NodeId)>(source: &Ast, node: NodeId, f: &mut F, is_root: bool) {
        if !is_root && matches!(source.node(node), Node::Function(..)) {
            return;
        }
        f(node);
        let mut children = Vec::new();
        ast_utils::for_each_child(source, node, &mut |child| children.push(child));
        for child in children {
            visit(source, child, f, false);
        }
    }
    visit(source, node, f, true);
}

/// Walk an expression subtree, visiting every node except nodes inside
/// nested functions.
fn walk_expr<F: FnMut(NodeId)>(source: &Ast, node: NodeId, f: &mut F) {
    fn visit<F: FnMut(NodeId)>(source: &Ast, node: NodeId, f: &mut F, is_root: bool) {
        if !is_root && matches!(source.node(node), Node::Function(..)) {
            return;
        }
        f(node);
        let mut children = Vec::new();
        ast_utils::for_each_child(source, node, &mut |child| children.push(child));
        for child in children {
            visit(source, child, f, false);
        }
    }
    visit(source, node, f, true);
}

/// Loop position for the cycle-65 function-internal alias proof: the
/// block directly containing the loop statement, the loop statement
/// itself, and whether the enclosing function (or chunk) contains any
/// goto/label (which could skip a preceding definition).
#[derive(Clone, Copy)]
struct LoopCtx {
    block: NodeId,
    stmt: NodeId,
    fn_has_goto: bool,
}

/// Proof inputs for `alias_table_root`: the chunk root (existing
/// chunk-top rule) plus the loop position (cycle-65 same-block rule).
/// Bundled so the callee signatures keep their argument counts.
#[derive(Clone, Copy)]
struct AliasProof {
    chunk_root: NodeId,
    loop_ctx: LoopCtx,
}

/// Whether a subtree (nested functions excluded) contains a goto or a
/// label.
fn block_has_goto_label(source: &Ast, block: NodeId) -> bool {
    let mut found = false;
    walk_full(source, block, &mut |node| {
        if matches!(source.node(node), Node::Goto(_) | Node::Label(_)) {
            found = true;
        }
    });
    found
}

/// Whether a statement subtree (nested functions excluded) contains a
/// call or a table-field write. Either could change what a global
/// alias denotes between its definition and the loop.
fn subtree_has_call_or_table_write(source: &Ast, node: NodeId) -> bool {
    let mut found = false;
    walk_full(source, node, &mut |n| {
        if found {
            return;
        }
        match source.node(n) {
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
    });
    found
}

/// Shared inputs for the loop barrier checks (kept as one argument
/// so the checks stay under clippy's argument-count limit).
struct BarrierCtx<'a> {
    source: &'a Ast,
    analyzer: &'a EffectAnalyzer<'a>,
    mutated: &'a HashSet<String>,
    resolution: &'a Resolution,
    chunk_root: NodeId,
    post_alias: bool,
    loop_ctx: LoopCtx,
    /// Parent of every node in the chunk (cycle 89: the
    /// global-alias chase walks definition positions and dominance
    /// through it). Sourced from the lazily built B3 chunk facts
    /// in the loop transform, and passed through unchanged in
    /// closure-body scans (whose loop position is the call's own
    /// statement, which the dominance forms are evaluated
    /// against).
    parents: &'a HashMap<NodeId, NodeId>,
    /// Chunk-level dynamic-global gate (cycle 89: the global-alias
    /// chase trusts the static write set only when this is false).
    dynamic: bool,
    /// Any goto/label in the chunk block outside nested functions
    /// (cycle 89: the chunk-strict dominance form needs it).
    chunk_goto: bool,
}

/// The builtin member denoted by an index callee through a proven
/// alias-table root (the pre-cycle-89 third fallback of
/// `call_builtin_for_barrier`, extracted unchanged): a chunk-top or
/// local alias base for a builtin table, with a literal member key.
fn alias_table_spelling(ctx: &BarrierCtx<'_>, function: NodeId) -> Option<String> {
    let BarrierCtx {
        source,
        analyzer,
        mutated,
        resolution,
        chunk_root,
        loop_ctx,
        ..
    } = ctx;
    let Node::Index(obj, key, dot) = source.node(function) else {
        return None;
    };
    if !dot {
        return None;
    }
    let Node::Name(_) = source.node(*obj) else {
        return None;
    };
    let Node::Str(raw) = source.node(*key) else {
        return None;
    };
    let raw: &str = raw;
    let inner = raw
        .strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .or_else(|| raw.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))?;
    if inner.is_empty() || inner.contains('\\') {
        return None;
    }
    let root_bid = resolution.node_bid[*obj as usize]?;
    let proof = AliasProof {
        chunk_root: *chunk_root,
        loop_ctx: *loop_ctx,
    };
    let (table, _) = alias_table_root(source, analyzer, mutated, resolution, &proof, root_bid)?;
    Some(format!("{table}.{inner}"))
}

/// The builtin denoted by a call's callee, for barrier purposes. In
/// the post-alias form an alias base also counts as known: a call
/// through a chunk-top or local alias for a builtin table is a known
/// builtin call, not an unknown one (otherwise every loop using an
/// alias base would be disqualified before collection).
fn call_builtin_for_barrier(ctx: &BarrierCtx<'_>, function: NodeId) -> Option<String> {
    let BarrierCtx {
        source,
        analyzer,
        resolution,
        post_alias,
        ..
    } = ctx;
    if let Some(builtin) = analyzer.resolve_builtin_reference(function) {
        return Some(builtin);
    }
    if !post_alias {
        return None;
    }
    if let Some(builtin) = resolve_builtin_through_locals(source, analyzer, resolution, function) {
        return Some(builtin);
    }
    if let Some(builtin) = alias_table_spelling(ctx, function) {
        return Some(builtin);
    }
    // Cycle 89 (global-alias chase): the fourth fallback, after the
    // analyzer, the local chase, and the alias-table root. A bare
    // name chases the value itself; an index callee chases its base
    // and reattaches the member, but a base spelling that already
    // contains a dot is a deeper chain and stays unknown.
    match source.node(function) {
        Node::Name(_) => {
            let chased = chase_builtin_through_aliases(ctx, function)?;
            if chased.steps == 0 || !chased.strict_dom {
                return None;
            }
            Some(chased.spelling)
        }
        Node::Index(obj, key, dot) => {
            if !dot {
                return None;
            }
            let member = index_member_literal(source, *key)?;
            let chased = chase_builtin_through_aliases(ctx, *obj)?;
            if chased.steps == 0 || !chased.strict_dom || chased.spelling.contains('.') {
                return None;
            }
            Some(format!("{}.{}", chased.spelling, member))
        }
        _ => None,
    }
}

/// The initializer expression of a local binding with no recorded
/// writes, or `None` for any other binding. Such a binding's value
/// is fixed by the single evaluation of its initializer.
fn local_init_expr(source: &Ast, resolution: &Resolution, bid: BindingId) -> Option<NodeId> {
    use storm_lua_analysis::resolver::BindingKind;
    let binding = resolution.bindings.get(bid as usize)?;
    if binding.kind != BindingKind::Local {
        return None;
    }
    if resolution.binding_write_counts[bid as usize] != 0 {
        return None;
    }
    let decl = binding.decl_node?;
    let Node::Local(_, exprs) = source.node(decl) else {
        return None;
    };
    let idx = resolution
        .node_bids
        .get(decl as usize)?
        .iter()
        .position(|candidate| *candidate == bid)?;
    exprs.get(idx).copied()
}

/// The member name of a dotted string-literal index key, under the
/// same literal rules as the post-alias callee form above.
fn index_member_literal(source: &Ast, key: NodeId) -> Option<String> {
    let Node::Str(raw) = source.node(key) else {
        return None;
    };
    let raw: &str = raw;
    let inner = raw
        .strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .or_else(|| raw.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))?;
    if inner.is_empty() || inner.contains('\\') {
        return None;
    }
    Some(inner.to_string())
}

/// The builtin member a callee denotes when the reference is spelled
/// through never-reassigned locals (the localized roots and members
/// the api-alias layouts create): chase local initializers, trying
/// the analyzer's own resolution at every step, and rebuild the
/// dotted spelling. Only dotted (member) spellings are returned --
/// calling a bare root table is not a builtin call. The chase is
/// depth-capped; anything it cannot resolve stays unknown.
fn resolve_builtin_through_locals(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    resolution: &Resolution,
    function: NodeId,
) -> Option<String> {
    fn chase(
        source: &Ast,
        analyzer: &EffectAnalyzer<'_>,
        resolution: &Resolution,
        expr: NodeId,
        depth: usize,
    ) -> Option<String> {
        if let Some(resolved) = analyzer.resolve_builtin_reference(expr) {
            return Some(resolved);
        }
        if depth >= 8 {
            return None;
        }
        match source.node(expr) {
            Node::Name(_) => {
                let bid = resolution.node_bid[expr as usize]?;
                let init = local_init_expr(source, resolution, bid)?;
                chase(source, analyzer, resolution, init, depth + 1)
            }
            Node::Index(object, key, dot) => {
                if !dot {
                    return None;
                }
                let member = index_member_literal(source, *key)?;
                let base = chase(source, analyzer, resolution, *object, depth + 1)?;
                if base.contains('.') {
                    None
                } else {
                    Some(format!("{base}.{member}"))
                }
            }
            _ => None,
        }
    }
    let resolved = chase(source, analyzer, resolution, function, 0)?;
    if resolved.contains('.') {
        Some(resolved)
    } else {
        None
    }
}

// ================= Cycle 89: global-alias chase =================
// A callee value chased to a builtin spelling through
// never-reassigned locals (the cycle-84 shape) and
// single-definition globals with per-step dominance (the cycle-88
// census shape). The census recomputation (analyzer v2) defines the
// strict form: every global step dominates by the chunk-strict form,
// or every global step dominates by the same-block form *and* the
// cycle-65 form. (The same-block form alone is the recorded "loose"
// candidate and is not adopted.)

/// Enclosing function body block of a node (or the chunk root
/// block), plus the top statement of that block on the node's
/// ancestor chain. Port of the cycle-88 census helper: the block is
/// the innermost enclosing function body, so a definition and a
/// loop in different functions never share a block.
fn enclosing_block_top(
    source: &Ast,
    parents: &HashMap<NodeId, NodeId>,
    chunk_root: NodeId,
    node: NodeId,
) -> Option<(NodeId, NodeId)> {
    let mut cur = node;
    loop {
        let parent = parents.get(&cur)?;
        match source.node(*parent) {
            Node::Function(_, _, body) => {
                let block = *body;
                let mut top = node;
                let mut c = node;
                while let Some(p) = parents.get(&c) {
                    if *p == block {
                        top = c;
                        break;
                    }
                    c = *p;
                }
                return Some((block, top));
            }
            _ => {
                if *parent == chunk_root {
                    let mut top = node;
                    let mut c = node;
                    while let Some(p) = parents.get(&c) {
                        if *p == chunk_root {
                            top = c;
                            break;
                        }
                        c = *p;
                    }
                    return Some((chunk_root, top));
                }
                cur = *parent;
            }
        }
    }
}

/// Same-function-body dominance: def and loop share the enclosing
/// function body block (or the chunk block), def's top statement
/// precedes the loop's, and the block has no goto/label in its
/// non-function subtree.
fn dominates_block_form(
    source: &Ast,
    parents: &HashMap<NodeId, NodeId>,
    chunk_root: NodeId,
    def_stmt: NodeId,
    loop_stmt: NodeId,
) -> bool {
    let Some((block, def_top)) = enclosing_block_top(source, parents, chunk_root, def_stmt) else {
        return false;
    };
    let Some((block2, loop_top)) = enclosing_block_top(source, parents, chunk_root, loop_stmt)
    else {
        return false;
    };
    if block != block2 || block_has_goto_label(source, block) {
        return false;
    }
    let Node::Block(stmts) = source.node(block) else {
        return false;
    };
    let Some(di) = stmts.iter().position(|s| *s == def_top) else {
        return false;
    };
    let Some(li) = stmts.iter().position(|s| *s == loop_top) else {
        return false;
    };
    di < li
}

/// Top-level chunk statement containing `node`, if any.
fn loop_top_stmt(
    parents: &HashMap<NodeId, NodeId>,
    chunk_root: NodeId,
    node: NodeId,
) -> Option<NodeId> {
    let mut cur = node;
    loop {
        let p = parents.get(&cur)?;
        if *p == chunk_root {
            return Some(cur);
        }
        cur = *p;
    }
}

/// Chunk-strict dominance (the B3-D3 form): the definition is a
/// direct chunk statement preceding the loop's top-level statement,
/// and the chunk block has no goto/label.
fn dom_chunk_strict(
    source: &Ast,
    parents: &HashMap<NodeId, NodeId>,
    chunk_root: NodeId,
    chunk_goto: bool,
    def_stmt: NodeId,
    loop_stmt: NodeId,
) -> bool {
    if chunk_goto {
        return false;
    }
    let Node::Block(stmts) = source.node(chunk_root) else {
        return false;
    };
    let Some(di) = stmts.iter().position(|s| *s == def_stmt) else {
        return false;
    };
    let Some(lt) = loop_top_stmt(parents, chunk_root, loop_stmt) else {
        return false;
    };
    let Some(li) = stmts.iter().position(|s| *s == lt) else {
        return false;
    };
    di < li
}

/// Cycle-65 same-block eligibility for one global definition step:
/// same direct block as the loop, preceding it, no call or
/// table-field write in the definition or between it and the loop,
/// and the enclosing function has no goto/label.
fn c65_form(
    ctx: &BarrierCtx<'_>,
    parents: &HashMap<NodeId, NodeId>,
    loop_stmt: NodeId,
    def_stmt: NodeId,
) -> bool {
    if ctx.loop_ctx.fn_has_goto {
        return false;
    }
    let source = ctx.source;
    let mut def_block = None;
    let mut cur = def_stmt;
    while let Some(p) = parents.get(&cur) {
        if matches!(source.node(*p), Node::Block(_)) {
            def_block = Some(*p);
            break;
        }
        cur = *p;
    }
    let mut loop_block = None;
    let mut cur = loop_stmt;
    while let Some(p) = parents.get(&cur) {
        if matches!(source.node(*p), Node::Block(_)) {
            loop_block = Some(*p);
            break;
        }
        cur = *p;
    }
    if def_block != loop_block {
        return false;
    }
    let Some(block) = def_block else {
        return false;
    };
    let Node::Block(stmts) = source.node(block) else {
        return false;
    };
    let Some(di) = stmts.iter().position(|s| *s == def_stmt) else {
        return false;
    };
    let Some(li) = stmts.iter().position(|s| *s == loop_stmt) else {
        return false;
    };
    if di >= li {
        return false;
    }
    if subtree_has_call_or_table_write(source, def_stmt) {
        return false;
    }
    for statement in &stmts[di + 1..li] {
        if subtree_has_call_or_table_write(source, *statement) {
            return false;
        }
    }
    true
}

/// Result of the global-alias chase: the builtin spelling a callee
/// value denotes, the number of alias hops taken, and whether
/// every global step dominates the loop in the strict form
/// (chunk-strict for every step, or same-block + cycle-65 for
/// every step).
struct AliasChase {
    spelling: String,
    steps: usize,
    strict_dom: bool,
}

/// Chase a callee value to a builtin spelling through analyzer
/// resolution, write-count-0 locals (the cycle-84 shape) and
/// write-count-1 globals with per-step dominance (the cycle-88
/// shape). Index values descend into their base with the key
/// appended as a suffix. Returns `None` when the chunk uses
/// dynamic globals (the static write set is then incomplete),
/// when any hop fails, or when the depth cap is hit. The cap
/// counts alias hops only: index descents always move to a
/// strictly smaller subtree, so they cannot loop, while alias
/// hops can cycle through bindings.
fn chase_builtin_through_aliases(ctx: &BarrierCtx<'_>, expr: NodeId) -> Option<AliasChase> {
    use storm_lua_analysis::resolver::BindingKind;
    if ctx.dynamic {
        return None;
    }
    let source = ctx.source;
    let resolution = ctx.resolution;
    let mut cur = expr;
    let mut suffix: Vec<String> = Vec::new();
    let mut steps = 0usize;
    let mut dom_chunk = true;
    let mut dom_block = true;
    let mut c65 = true;
    loop {
        if steps >= 8 {
            return None;
        }
        if let Some(resolved) = ctx.analyzer.resolve_builtin_reference(cur) {
            let mut spelling = resolved;
            for k in suffix.iter().rev() {
                spelling.push('.');
                spelling.push_str(k);
            }
            if steps == 0 {
                return None;
            }
            let strict_dom = dom_chunk || (dom_block && c65);
            return Some(AliasChase {
                spelling,
                steps,
                strict_dom,
            });
        }
        match source.node(cur) {
            Node::Name(_) => {
                let bid = resolution.node_bid.get(cur as usize).copied().flatten()?;
                let binding = resolution.bindings.get(bid as usize)?;
                match binding.kind {
                    BindingKind::Local => {
                        let init = local_init_expr(source, resolution, bid)?;
                        cur = init;
                        steps += 1;
                    }
                    BindingKind::Global => {
                        if resolution.binding_write_counts.get(bid as usize).copied() != Some(1) {
                            return None;
                        }
                        // The single write node: the write count
                        // guarantees uniqueness, so the first match
                        // is the definition. A `Funcstat` definition
                        // is not an alias step (that is the B3
                        // domain), so only `Assign` qualifies.
                        let mut target = None;
                        for i in 0..resolution.node_write.len() {
                            if resolution.node_write[i] && resolution.node_bid[i] == Some(bid) {
                                target = Some(i as NodeId);
                                break;
                            }
                        }
                        let t = target?;
                        let stmt = ctx.parents.get(&t).copied()?;
                        let Node::Assign(targets, values) = source.node(stmt) else {
                            return None;
                        };
                        let pos = targets.iter().position(|x| *x == t)?;
                        let value = values.get(pos).copied()?;
                        let dc = dom_chunk_strict(
                            source,
                            ctx.parents,
                            ctx.chunk_root,
                            ctx.chunk_goto,
                            stmt,
                            ctx.loop_ctx.stmt,
                        );
                        let db = dominates_block_form(
                            source,
                            ctx.parents,
                            ctx.chunk_root,
                            stmt,
                            ctx.loop_ctx.stmt,
                        );
                        let c = c65_form(ctx, ctx.parents, ctx.loop_ctx.stmt, stmt);
                        dom_chunk &= dc;
                        dom_block &= db;
                        c65 &= c;
                        cur = value;
                        steps += 1;
                    }
                    _ => return None,
                }
            }
            Node::Index(obj, key, dot) => {
                if !dot {
                    return None;
                }
                let member = index_member_literal(source, *key)?;
                suffix.push(member);
                cur = *obj;
            }
            _ => return None,
        }
    }
}

/// Whether a call node violates the loop barriers (unknown, method,
/// stateful, or passing a whole host table as an argument).
fn call_is_barrier(ctx: &BarrierCtx<'_>, node: NodeId) -> bool {
    let BarrierCtx {
        source,
        analyzer,
        mutated,
        resolution,
        chunk_root,
        post_alias,
        loop_ctx,
        ..
    } = ctx;
    let Node::Call(function, args, method) = source.node(node) else {
        return false;
    };
    if method.is_some() {
        return true;
    }
    let Some(builtin) = call_builtin_for_barrier(ctx, *function) else {
        return true;
    };
    if is_disqualifying_builtin(&builtin) {
        return true;
    }
    for arg in args {
        if let Node::Name(_) | Node::Index(..) = source.node(*arg) {
            if let Some(passed) = analyzer.resolve_builtin_reference(*arg) {
                if !passed.contains('.') {
                    return true;
                }
            }
            // Post-alias form: a bare alias name denoting a host
            // table is a table passed as an argument, too.
            if *post_alias {
                if let Node::Name(_) = source.node(*arg) {
                    if let Some(bid) = resolution.node_bid[*arg as usize] {
                        let proof = AliasProof {
                            chunk_root: *chunk_root,
                            loop_ctx: *loop_ctx,
                        };
                        if alias_table_root(source, analyzer, mutated, resolution, &proof, bid)
                            .is_some()
                        {
                            return true;
                        }
                    }
                }
            }
            // Cycle 89: a bare alias name denoting a host table
            // through the global-alias chase is a table passed as
            // an argument, too. Dominance is not required here:
            // flagging the argument is the conservative direction,
            // and this mirrors the cycle-88 census argument rule.
            // (The B3 closure argument rule is unchanged.)
            if *post_alias {
                if let Node::Name(_) = source.node(*arg) {
                    if let Some(chased) = chase_builtin_through_aliases(ctx, *arg) {
                        if !chased.spelling.contains('.') {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// The base node of a table-field write target (`base.key`,
/// `base[key]`, or a method definition on `base`), or `None` for a
/// plain name target.
fn field_write_base(source: &Ast, target: NodeId) -> Option<NodeId> {
    match source.node(target) {
        Node::Index(obj, _, _) | Node::Methodname(obj, _) => Some(*obj),
        _ => None,
    }
}

/// Whether a table-field write through `base` provably cannot change
/// any cached member value (cycle 83). Only a bare `Name` base can be
/// proven: it must denote neither a builtin value (a root or a
/// member) nor a proven builtin-root alias, and it must satisfy the
/// plain-table proof shared with `loop_member_reads` and
/// `straightline_member_reads` -- a constructor-built table (a
/// write-free local, or a global whose definitions are all table
/// constructors) that never escapes anywhere in the program. The
/// multi-definition form is admitted here only, for writes; the
/// member-read passes keep the single-definition proof (see
/// `base_proven_write_use`). A plain table is a distinct
/// object from every builtin root, and because it never escapes, no
/// alias route and no `setmetatable` route can make a write through
/// it reach a root field. Every other shape stays disqualifying.
fn write_base_harmless(ctx: &BarrierCtx<'_>, base: NodeId) -> bool {
    let source = ctx.source;
    if !matches!(source.node(base), Node::Name(_)) {
        return false;
    }
    if ctx.analyzer.resolve_builtin_reference(base).is_some() {
        return false;
    }
    let Some(bid) = ctx.resolution.node_bid[base as usize] else {
        return false;
    };
    let proof = AliasProof {
        chunk_root: ctx.chunk_root,
        loop_ctx: ctx.loop_ctx,
    };
    if alias_table_root(
        source,
        ctx.analyzer,
        ctx.mutated,
        ctx.resolution,
        &proof,
        bid,
    )
    .is_some()
    {
        return false;
    }
    base_proven_write_use(
        source,
        ctx.resolution,
        ctx.chunk_root,
        bid,
        ctx.loop_ctx.fn_has_goto,
        LoopPos {
            block: ctx.loop_ctx.block,
            stmt: ctx.loop_ctx.stmt,
        },
    )
}

/// `write_base_harmless` with a per-loop memo: the plain-table proof
/// scans the whole chunk for escapes, and one loop body often writes
/// the same base many times. The verdict depends on the loop
/// position, which is fixed for one `loop_disqualified` call, so the
/// memo is local to the call (the same key design as
/// `straightline_member_reads`' proof cache).
fn write_base_harmless_memo(
    ctx: &BarrierCtx<'_>,
    base: NodeId,
    memo: &mut HashMap<BindingId, bool>,
) -> bool {
    if matches!(ctx.source.node(base), Node::Name(_)) {
        if let Some(bid) = ctx.resolution.node_bid[base as usize] {
            if let Some(verdict) = memo.get(&bid) {
                return *verdict;
            }
            let verdict = write_base_harmless(ctx, base);
            memo.insert(bid, verdict);
            return verdict;
        }
    }
    write_base_harmless(ctx, base)
}

// ================= Candidate B3 (cycle 86) =================
// A call whose callee is a global function defined in this chunk
// counts as known -- instead of disqualifying the loop as an unknown
// call -- when the callee's identity and body safety are provable.
// This extends the cycle-84 local chase (B1) to global functions;
// the cycle-84 census counted 680 such calls in opportunity loops.
// Cycle 86 implemented the strict form (single definition plus
// dominance: 575 call sites by identity, 36 loops released);
// cycle 92 relaxes identity to the B3b form below (user-approved
// 2026-10-31), which the cycle-86 census measured at 66 loops /
// 59 files.
//
// Identity (single definition; B3b -- no dominance):
// - The chunk must be free of dynamic global access: no `_ENV`
//   reference and no call spelled `rawget` / `rawset` / `load` /
//   `loadstring` anywhere (the `dynamic_globals` gate shared with
//   single_use_forwarding / closed_fields / global_stores). On the
//   AST path `_ENV` cannot appear at all (it forces the lexical
//   path), so every write to a global binding is syntactic and the
//   resolver's write counts are complete.
// - The callee binding has exactly one recorded write, and that
//   write is a `Funcstat` with a plain name target, in any
//   position (a direct chunk statement, a nested block, or a
//   function body). The strict form additionally required the
//   definition to be a direct chunk statement positioned before
//   the loop's top-level statement, in a chunk block free of
//   goto/label; B3b drops both, by call-error equivalence: at
//   any call, either the single definition has executed -- then
//   the body that runs is that definition's body, and no other
//   write exists anywhere (the write count is complete under
//   the dynamic-globals gate) -- or it has not executed, the
//   binding is still nil, and the call itself errors before any
//   body runs. S1 never moves, adds, or removes a call, and the
//   preheader it inserts is a pure read of a root-table field,
//   total under the existing grounds, so that error occurs
//   identically before and after the transform; if a pcall-style
//   wrapper catches it, the cached value is likewise unchanged.
//   A chunk-level goto re-executing the definition only creates
//   a fresh closure of the same body, and recursion or a loop
//   inside the callee's own body keeps the same body set. This
//   argument depends on the VM's error semantics (a call of a
//   nil value errors immediately, with no other observable
//   difference); if those semantics change, this relaxation
//   must be re-examined (see docs/verification/
//   s1-global-function-chase-b3b-20261031.md).
//
// Body safety (the closure): starting from the loop's B3 callees,
// every chunk-defined function transitively called must satisfy:
// - no goto/label in its body (outside nested functions);
// - field writes only through bases proven harmless by
//   `write_base_harmless`, evaluated at the write's own statement
//   position inside its own function (the cycle-83/65 position
//   generalization: the proof is about the write site, so the
//   site's own block/statement/function context is the right one);
// - global name writes only to bindings that are neither builtin
//   roots nor reserved names nor any callee of the closure (so no
//   body can rebind a root, a builtin, or a fellow callee);
// - every call is a known builtin (directly, through the cycle-84
//   local chase, or through a proven alias base) that is not
//   stateful and passes no bare root/alias table as an argument,
//   or another chunk-defined function admitted to the closure
//   (a B3 global, or a write-free local function read from its
//   declaration), under the same argument rule; method calls are
//   disqualifying;
// - function values occur only in definition positions (a
//   `local function`, a local initializer, a `Funcstat` body):
//   a function value passed, returned, or stored could run later
//   through paths the call graph cannot follow (higher-order
//   builtins such as `table.sort` comparators included).
// Calls in the loop body itself keep the plain argument rule: a B3
// call that passes a bare root or alias table stays a barrier.

/// Chunk-level facts for the B3 checks, built lazily on first use
/// (most chunks never present a B3 candidate). Shared by all loops
/// of one transform run; per-body facts are memoized alongside.
#[derive(Default)]
struct B3Cells {
    chunk: OnceCell<B3Chunk>,
    bodies: RefCell<HashMap<BindingId, Rc<B3BodyFacts>>>,
}

struct B3Chunk {
    /// The dynamic-global gate (see the section comment).
    dynamic: bool,
    /// Any goto/label in the chunk block outside nested functions.
    chunk_goto: bool,
    /// Parent of every node in the chunk (ancestry queries only).
    parents: HashMap<NodeId, NodeId>,
    /// Direct statements of the chunk root block, in order.
    stmts: Vec<NodeId>,
}

impl B3Cells {
    fn chunk_facts(&self, source: &Ast, root: NodeId) -> &B3Chunk {
        self.chunk.get_or_init(|| B3Chunk::build(source, root))
    }

    fn body_facts(&self, ctx: &BarrierCtx<'_>, bid: BindingId, fn_node: NodeId) -> Rc<B3BodyFacts> {
        if let Some(facts) = self.bodies.borrow().get(&bid) {
            return facts.clone();
        }
        let facts = Rc::new(b3_scan_body(ctx, fn_node));
        self.bodies.borrow_mut().insert(bid, facts.clone());
        facts
    }
}

impl B3Chunk {
    fn build(source: &Ast, root: NodeId) -> B3Chunk {
        let dynamic = b3_dynamic_globals(source, root);
        let chunk_goto = block_has_goto_label(source, root);
        let mut parents = HashMap::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            let mut children = Vec::new();
            ast_utils::for_each_child(source, node, &mut |child| children.push(child));
            for child in children {
                parents.insert(child, node);
                stack.push(child);
            }
        }
        let stmts = match source.node(root) {
            Node::Block(stmts) => stmts.clone(),
            _ => Vec::new(),
        };
        B3Chunk {
            dynamic,
            chunk_goto,
            parents,
            stmts,
        }
    }

    /// Index (in the chunk block) of the top-level statement whose
    /// subtree contains `node`, if any.
    fn top_index(&self, node: NodeId) -> Option<usize> {
        let mut cur = node;
        loop {
            if let Some(pos) = self.stmts.iter().position(|s| *s == cur) {
                return Some(pos);
            }
            cur = *self.parents.get(&cur)?;
        }
    }
}

/// Mirror of `dynamic_globals` in single_use_forwarding.rs: whether
/// the chunk can touch globals dynamically (`_ENV`, or a call to
/// `rawget` / `rawset` / `load` / `loadstring` spelled as a plain
/// name anywhere, nested functions included).
fn b3_dynamic_globals(source: &Ast, root: NodeId) -> bool {
    let mut dynamic = false;
    ast_utils::walk(source, root, &mut |node| {
        if dynamic {
            return;
        }
        match source.node(node) {
            Node::Name(symbol) if source.strings.get(*symbol) == "_ENV" => dynamic = true,
            Node::Call(function, _, _) => {
                if let Node::Name(symbol) = source.node(*function) {
                    let name = source.strings.get(*symbol);
                    if name == "rawget"
                        || name == "rawset"
                        || name == "load"
                        || name == "loadstring"
                    {
                        dynamic = true;
                    }
                }
            }
            _ => {}
        }
    });
    dynamic
}

/// The function body a chunk-defined binding denotes, if any: the
/// analyzer-recorded body (local function declarations and
/// `Funcstat` definitions), else a write-free local whose
/// initializer is a function literal (read from the declaration, as
/// in the cycle-84 B2 design).
fn b3_body_of(ctx: &BarrierCtx<'_>, bid: BindingId) -> Option<NodeId> {
    if let Some(f) = ctx.analyzer.functions.get(bid as usize).copied().flatten() {
        return Some(f);
    }
    let init = local_init_expr(ctx.source, ctx.resolution, bid)?;
    if matches!(ctx.source.node(init), Node::Function(..)) {
        Some(init)
    } else {
        None
    }
}

enum B3Callee {
    Builtin(String),
    ChunkFn(BindingId),
    Dirty,
}

/// Callee resolution past builtins: a name (possibly through
/// write-free local alias hops, the cycle-84 chase shape) denoting
/// a chunk-defined function whose binding identity is fixed (a
/// global -- identity is checked separately -- or a write-free
/// local function).
fn b3_resolve_callee(ctx: &BarrierCtx<'_>, function: NodeId) -> B3Callee {
    use storm_lua_analysis::resolver::BindingKind;
    if let Some(b) = call_builtin_for_barrier(ctx, function) {
        return B3Callee::Builtin(b);
    }
    if !matches!(ctx.source.node(function), Node::Name(_)) {
        return B3Callee::Dirty;
    }
    let mut bid = match ctx.resolution.node_bid[function as usize] {
        Some(b) => b,
        None => return B3Callee::Dirty,
    };
    for _ in 0..=8 {
        let binding = &ctx.resolution.bindings[bid as usize];
        match binding.kind {
            BindingKind::Global => {
                return if b3_body_of(ctx, bid).is_some() {
                    B3Callee::ChunkFn(bid)
                } else {
                    B3Callee::Dirty
                };
            }
            BindingKind::Local | BindingKind::Localfunc => {
                if b3_body_of(ctx, bid).is_some() {
                    return if ctx.resolution.binding_write_counts[bid as usize] == 0 {
                        B3Callee::ChunkFn(bid)
                    } else {
                        B3Callee::Dirty
                    };
                }
                let Some(init) = local_init_expr(ctx.source, ctx.resolution, bid) else {
                    return B3Callee::Dirty;
                };
                match ctx.source.node(init) {
                    Node::Name(_) => {
                        bid = match ctx.resolution.node_bid[init as usize] {
                            Some(b) => b,
                            None => return B3Callee::Dirty,
                        };
                    }
                    _ => return B3Callee::Dirty,
                }
            }
            BindingKind::Param => return B3Callee::Dirty,
            _ => return B3Callee::Dirty,
        }
    }
    B3Callee::Dirty
}

/// The `call_is_barrier` argument rule (no bare root / bare alias
/// table passed as an argument), against an arbitrary ctx.
fn b3_args_clean(ctx: &BarrierCtx<'_>, args: &[NodeId]) -> bool {
    let source = ctx.source;
    for arg in args {
        if let Node::Name(_) | Node::Index(..) = source.node(*arg) {
            if let Some(passed) = ctx.analyzer.resolve_builtin_reference(*arg) {
                if !passed.contains('.') {
                    return false;
                }
            }
            if ctx.post_alias {
                if let Node::Name(_) = source.node(*arg) {
                    if let Some(bid) = ctx.resolution.node_bid[*arg as usize] {
                        let proof = AliasProof {
                            chunk_root: ctx.chunk_root,
                            loop_ctx: ctx.loop_ctx,
                        };
                        if alias_table_root(
                            source,
                            ctx.analyzer,
                            ctx.mutated,
                            ctx.resolution,
                            &proof,
                            bid,
                        )
                        .is_some()
                        {
                            return false;
                        }
                    }
                }
            }
        }
    }
    true
}

/// Whether a global callee's identity is fixed (cycle 92, B3b):
/// exactly one recorded write, and that write is a plain-name
/// `Funcstat`, in any position. The strict form (cycle 86) also
/// required the definition to be a direct chunk statement before
/// the loop's top-level statement index `anc`, in a chunk block
/// free of goto/label; B3b drops both by the call-error-equivalence
/// argument in the section comment above, so `anc` is retained
/// only because the call site computes it as a structural check
/// (the loop must lie under the chunk root) and is not consulted
/// here.
fn b3_identity_ok(ctx: &BarrierCtx<'_>, chunk: &B3Chunk, bid: BindingId, _anc: usize) -> bool {
    let resolution = ctx.resolution;
    if resolution.binding_write_counts.get(bid as usize).copied() != Some(1) {
        return false;
    }
    let mut target = None;
    for i in 0..resolution.node_write.len() {
        if resolution.node_write[i] && resolution.node_bid[i] == Some(bid) {
            target = Some(i as NodeId);
            break;
        }
    }
    let Some(t) = target else {
        return false;
    };
    let Some(stmt) = chunk.parents.get(&t).copied() else {
        return false;
    };
    let Node::Funcstat(ft, _) = ctx.source.node(stmt) else {
        return false;
    };
    *ft == t
}

struct B3BodyFacts {
    dirty: bool,
    /// Global bindings written by name, with their names.
    gwrites: Vec<(BindingId, String)>,
    /// Chunk-defined callees admitted from this body.
    callees: Vec<BindingId>,
}

/// Syntactic scan of one closure body. Nested functions are
/// boundaries: a nested function node is allowed only in a
/// definition position and is scanned separately if it is called.
struct B3Scan<'a> {
    ctx: &'a BarrierCtx<'a>,
    parents: HashMap<NodeId, NodeId>,
    body_block: NodeId,
    fn_goto: bool,
    dirty: bool,
    gwrites: Vec<(BindingId, String)>,
    callees: Vec<BindingId>,
}

impl<'a> B3Scan<'a> {
    /// A BarrierCtx whose loop position is `node`'s own containing
    /// statement inside this body (see the section comment for why
    /// the write/call site's own position is the right context).
    fn own_ctx(&self, node: NodeId) -> BarrierCtx<'a> {
        let source = self.ctx.source;
        let mut block = self.body_block;
        let mut stmt = node;
        let mut cur = node;
        while let Some(p) = self.parents.get(&cur) {
            if matches!(source.node(*p), Node::Block(_)) {
                block = *p;
                stmt = cur;
                break;
            }
            cur = *p;
        }
        BarrierCtx {
            source: self.ctx.source,
            analyzer: self.ctx.analyzer,
            mutated: self.ctx.mutated,
            resolution: self.ctx.resolution,
            chunk_root: self.ctx.chunk_root,
            post_alias: self.ctx.post_alias,
            loop_ctx: LoopCtx {
                block,
                stmt,
                fn_has_goto: self.fn_goto,
            },
            // Cycle 89: chunk-level facts pass through unchanged.
            // Note the parent map here is the loop ctx's
            // chunk-wide map, not the scan's body-local one: the
            // chase walks dominance up to the chunk root, and the
            // loop position (the call's own statement) is what the
            // dominance forms are evaluated against.
            parents: self.ctx.parents,
            dynamic: self.ctx.dynamic,
            chunk_goto: self.ctx.chunk_goto,
        }
    }

    fn def_position(&self, node: NodeId) -> bool {
        let source = self.ctx.source;
        let Some(parent) = self.parents.get(&node) else {
            return false;
        };
        match source.node(*parent) {
            Node::Local(_, exprs) => exprs.contains(&node),
            Node::Funcstat(_, f) | Node::Localfunc(_, f) => *f == node,
            _ => false,
        }
    }

    fn write_target(&mut self, stmt: NodeId, target: NodeId) {
        use storm_lua_analysis::resolver::BindingKind;
        let source = self.ctx.source;
        if let Some(base) = field_write_base(source, target) {
            let own = self.own_ctx(stmt);
            if !write_base_harmless(&own, base) {
                self.dirty = true;
            }
            return;
        }
        if matches!(source.node(target), Node::Name(_)) {
            match self.ctx.resolution.node_bid[target as usize] {
                Some(bid) => {
                    let binding = &self.ctx.resolution.bindings[bid as usize];
                    if binding.kind == BindingKind::Global {
                        let name = source.strings.get(binding.name).to_string();
                        self.gwrites.push((bid, name));
                    }
                }
                None => self.dirty = true,
            }
        }
    }

    fn call(&mut self, node: NodeId, function: NodeId, args: &[NodeId], is_method: bool) {
        if is_method {
            self.dirty = true;
            return;
        }
        let own = self.own_ctx(node);
        match b3_resolve_callee(&own, function) {
            B3Callee::Builtin(b) => {
                if is_disqualifying_builtin(&b) || !b3_args_clean(&own, args) {
                    self.dirty = true;
                }
            }
            B3Callee::ChunkFn(bid) => {
                if !b3_args_clean(&own, args) {
                    self.dirty = true;
                } else {
                    self.callees.push(bid);
                }
            }
            B3Callee::Dirty => self.dirty = true,
        }
    }

    fn visit(&mut self, node: NodeId, is_root: bool) {
        let source = self.ctx.source;
        if !is_root && matches!(source.node(node), Node::Function(..)) {
            if !self.def_position(node) {
                self.dirty = true;
            }
            return;
        }
        match source.node(node) {
            Node::Goto(_) | Node::Label(_) => self.dirty = true,
            Node::Assign(targets, _) => {
                let targets = targets.clone();
                for t in targets {
                    self.write_target(node, t);
                }
            }
            Node::Funcstat(target, _) => {
                let target = *target;
                self.write_target(node, target);
            }
            Node::Call(function, args, method) => {
                let (function, args, is_method) = (*function, args.clone(), method.is_some());
                self.call(node, function, &args, is_method);
            }
            _ => {}
        }
        let mut children = Vec::new();
        ast_utils::for_each_child(source, node, &mut |child| children.push(child));
        for child in children {
            self.visit(child, false);
        }
    }
}

fn b3_scan_body(ctx: &BarrierCtx<'_>, fn_node: NodeId) -> B3BodyFacts {
    let source = ctx.source;
    let Node::Function(_, _, body_block) = source.node(fn_node) else {
        return B3BodyFacts {
            dirty: true,
            gwrites: Vec::new(),
            callees: Vec::new(),
        };
    };
    let body_block = *body_block;
    let fn_goto = block_has_goto_label(source, body_block);
    let mut parents = HashMap::new();
    let mut stack = vec![fn_node];
    while let Some(node) = stack.pop() {
        let mut children = Vec::new();
        ast_utils::for_each_child(source, node, &mut |child| children.push(child));
        for child in children {
            parents.insert(child, node);
            stack.push(child);
        }
    }
    let mut scan = B3Scan {
        ctx,
        parents,
        body_block,
        fn_goto,
        dirty: fn_goto,
        gwrites: Vec::new(),
        callees: Vec::new(),
    };
    scan.visit(body_block, true);
    B3BodyFacts {
        dirty: scan.dirty,
        gwrites: scan.gwrites,
        callees: scan.callees,
    }
}

enum B3Call {
    Clean,
    Candidate(BindingId),
    Bad,
}

/// Barrier classification for the B3 rescue: a call that
/// `call_is_barrier` rejects counts as a candidate only when the
/// rejection is solely "unknown callee" and the callee resolves to
/// a chunk-defined function under the plain argument rule. A
/// rejection for any other reason (method, stateful, host-table
/// argument) stays bad.
fn b3_classify_call(ctx: &BarrierCtx<'_>, node: NodeId) -> B3Call {
    if !call_is_barrier(ctx, node) {
        return B3Call::Clean;
    }
    let Node::Call(function, args, method) = ctx.source.node(node) else {
        return B3Call::Bad;
    };
    if method.is_some() {
        return B3Call::Bad;
    }
    if call_builtin_for_barrier(ctx, *function).is_some() {
        return B3Call::Bad;
    }
    match b3_resolve_callee(ctx, *function) {
        B3Callee::ChunkFn(bid) if b3_args_clean(ctx, args) => B3Call::Candidate(bid),
        _ => B3Call::Bad,
    }
}

/// Whether a loop that `loop_disqualified` rejects is rescued by
/// B3: every barrier in the body and headers is a B3 candidate
/// call, every global callee's identity is fixed for this loop,
/// and the transitive closure of chunk-defined callees is body-safe
/// (see the section comment).
fn b3_rescue_loop(ctx: &BarrierCtx<'_>, cells: &B3Cells, body: NodeId, headers: &[NodeId]) -> bool {
    use storm_lua_analysis::resolver::BindingKind;
    let source = ctx.source;
    let mut bids: Vec<BindingId> = Vec::new();
    let mut bad = false;
    let mut harmless_memo: HashMap<BindingId, bool> = HashMap::new();
    walk_full(source, body, &mut |node| {
        if bad {
            return;
        }
        match source.node(node) {
            Node::Goto(_) | Node::Label(_) => bad = true,
            Node::Assign(targets, _) => {
                for target in targets {
                    if let Some(base) = field_write_base(source, *target) {
                        if !write_base_harmless_memo(ctx, base, &mut harmless_memo) {
                            bad = true;
                        }
                    }
                }
            }
            Node::Funcstat(target, _) => {
                if let Some(base) = field_write_base(source, *target) {
                    if !write_base_harmless_memo(ctx, base, &mut harmless_memo) {
                        bad = true;
                    }
                }
            }
            Node::Call(..) => match b3_classify_call(ctx, node) {
                B3Call::Clean => {}
                B3Call::Candidate(bid) => bids.push(bid),
                B3Call::Bad => bad = true,
            },
            _ => {}
        }
    });
    if bad {
        return false;
    }
    for header in headers {
        let mut header_bids: Vec<BindingId> = Vec::new();
        walk_expr(source, *header, &mut |node| {
            if bad {
                return;
            }
            if matches!(source.node(node), Node::Call(..)) {
                match b3_classify_call(ctx, node) {
                    B3Call::Clean => {}
                    B3Call::Candidate(bid) => header_bids.push(bid),
                    B3Call::Bad => bad = true,
                }
            }
        });
        bids.append(&mut header_bids);
    }
    if bad || bids.is_empty() {
        return false;
    }
    let chunk = cells.chunk_facts(source, ctx.chunk_root);
    if chunk.dynamic {
        return false;
    }
    let Some(anc) = chunk.top_index(ctx.loop_ctx.stmt) else {
        return false;
    };
    let mut seen: HashSet<BindingId> = HashSet::new();
    let mut worklist = bids;
    let mut bodies: Vec<Rc<B3BodyFacts>> = Vec::new();
    while let Some(bid) = worklist.pop() {
        if !seen.insert(bid) {
            continue;
        }
        let binding = &ctx.resolution.bindings[bid as usize];
        if binding.kind == BindingKind::Global && !b3_identity_ok(ctx, chunk, bid, anc) {
            return false;
        }
        let Some(fnode) = b3_body_of(ctx, bid) else {
            return false;
        };
        let facts = cells.body_facts(ctx, bid, fnode);
        if facts.dirty {
            return false;
        }
        for c in &facts.callees {
            worklist.push(*c);
        }
        bodies.push(facts);
    }
    for facts in &bodies {
        for (gbid, gname) in &facts.gwrites {
            if seen.contains(gbid) {
                return false;
            }
            if storm_lua_analysis::resolver::api_roots(gname)
                || storm_lua_analysis::resolver::reserved(gname)
            {
                return false;
            }
        }
    }
    true
}

/// Whether a loop must not be cached: the disqualification scan over
/// the body subtree plus the same call rules applied to headers.
fn loop_disqualified(ctx: &BarrierCtx<'_>, body: NodeId, headers: &[NodeId]) -> bool {
    let source = ctx.source;
    let mut disqualified = false;
    let mut harmless_memo: HashMap<BindingId, bool> = HashMap::new();
    walk_full(source, body, &mut |node| {
        if disqualified {
            return;
        }
        match source.node(node) {
            Node::Goto(_) | Node::Label(_) => disqualified = true,
            Node::Assign(targets, _) => {
                for target in targets {
                    if let Some(base) = field_write_base(source, *target) {
                        if !write_base_harmless_memo(ctx, base, &mut harmless_memo) {
                            disqualified = true;
                        }
                    }
                }
            }
            Node::Funcstat(target, _) => {
                if let Some(base) = field_write_base(source, *target) {
                    if !write_base_harmless_memo(ctx, base, &mut harmless_memo) {
                        disqualified = true;
                    }
                }
            }
            Node::Call(..) if call_is_barrier(ctx, node) => {
                disqualified = true;
            }
            Node::Call(..) => {}
            _ => {}
        }
    });
    if disqualified {
        return true;
    }
    for header in headers {
        let mut header_bad = false;
        walk_expr(source, *header, &mut |node| {
            if header_bad {
                return;
            }
            if matches!(source.node(node), Node::Call(..)) && call_is_barrier(ctx, node) {
                header_bad = true;
            }
        });
        if header_bad {
            return true;
        }
    }
    false
}

/// One occurrence of a cacheable member call: the call node, its
/// callee index node, the callee root's binding, and the printed
/// length of the callee base spelling (used by the post-alias gate).
struct Occurrence {
    call: NodeId,
    function: NodeId,
    root_bid: BindingId,
}

struct CacheGroup {
    occurrences: Vec<Occurrence>,
    /// Callee index node of the first occurrence whose root is not
    /// bound inside the loop body; the preheader clones this node.
    template: NodeId,
}

/// The builtin table root denoted by a callee base name, for the
/// post-alias form: either the name is itself a builtin root, or it
/// is a single-definition alias whose initializer is a builtin root
/// (a chunk-top global assignment, a local binding, or -- cycle 65 --
/// a global assignment in the same block as, and preceding, the loop,
/// inside the same function). A parameter base is never accepted:
/// its value is not provable from this AST.
/// The write-count rule is what makes the alias single-definition: a
/// local alias has no recorded writes, a global alias has exactly one
/// (its defining assignment, which must be a direct chunk statement
/// or, for the cycle-65 form, a direct statement of the loop's own
/// block). The cycle-65 form additionally requires: the enclosing
/// function contains no goto/label (so no jump can skip the
/// definition), and the statements between the definition and the
/// loop -- plus the definition statement itself -- contain no call
/// and no table-field write (so nothing between them can change what
/// the alias denotes). A definition inside a branch block does not
/// qualify: it is not a direct statement of the loop's block, so it
/// does not dominate the loop.
/// Whether `target` occurs in the subtree rooted at `node`
/// (nested functions included: a loop inside a function defined
/// at chunk level is still contained in its chunk statement).
fn subtree_contains(source: &Ast, node: NodeId, target: NodeId) -> bool {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current == target {
            return true;
        }
        let mut children = Vec::new();
        ast_utils::for_each_child(source, current, &mut |child| children.push(child));
        for child in children {
            stack.push(child);
        }
    }
    false
}

/// Position at which an alias value is captured (read), for
/// alias-chain link-order verification in `alias_chain_ordered`.
#[derive(Clone, Copy)]
enum ChainPos {
    /// Index of a statement in the chunk block.
    Chunk(usize),
    /// A block (never the chunk root) and an index into it.
    Block(NodeId, usize),
    /// Inside the loop's enclosing function; only chunk-domain
    /// links anchored before the loop's chunk statement qualify.
    /// Payload: the loop's chunk statement index.
    #[allow(dead_code)]
    Func(usize),
}

/// Chunk-statement index of the chunk statement containing the
/// loop statement (nested functions included), if any.
fn loop_top_chunk_idx(source: &Ast, proof: &AliasProof) -> Option<usize> {
    let Node::Block(chunk_statements) = source.node(proof.chunk_root) else {
        return None;
    };
    chunk_statements
        .iter()
        .position(|s| subtree_contains(source, *s, proof.loop_ctx.stmt))
}

/// The unique chunk-statement `Assign` writing `bid`, as (chunk
/// statement index, RHS value). Callers check write counts first;
/// this is a position lookup that also fails on duplicates.
fn chunk_single_def(
    source: &Ast,
    resolution: &Resolution,
    chunk_root: NodeId,
    bid: BindingId,
) -> Option<(usize, NodeId)> {
    let Node::Block(chunk_statements) = source.node(chunk_root) else {
        return None;
    };
    let mut found = None;
    for (idx, statement) in chunk_statements.iter().enumerate() {
        let Node::Assign(targets, values) = source.node(*statement) else {
            continue;
        };
        for (target, value) in targets.iter().zip(values) {
            if matches!(source.node(*target), Node::Name(_))
                && resolution.node_bid[*target as usize] == Some(bid)
            {
                if found.is_some() {
                    return None;
                }
                found = Some((idx, *value));
            }
        }
    }
    found
}

/// The unique direct-statement `Assign` writing `bid` in `block`,
/// as (statement index, RHS value).
fn block_single_def(
    source: &Ast,
    resolution: &Resolution,
    block: NodeId,
    bid: BindingId,
) -> Option<(usize, NodeId)> {
    let Node::Block(statements) = source.node(block) else {
        return None;
    };
    let mut found = None;
    for (idx, statement) in statements.iter().enumerate() {
        let Node::Assign(targets, values) = source.node(*statement) else {
            continue;
        };
        for (target, value) in targets.iter().zip(values) {
            if matches!(source.node(*target), Node::Name(_))
                && resolution.node_bid[*target as usize] == Some(bid)
            {
                if found.is_some() {
                    return None;
                }
                found = Some((idx, *value));
            }
        }
    }
    found
}

/// Position of a statement that is a direct statement of the chunk
/// block or of the loop's block, for chain-order verification.
fn direct_stmt_pos(source: &Ast, proof: &AliasProof, stmt: NodeId) -> Option<ChainPos> {
    if let Node::Block(chunk_statements) = source.node(proof.chunk_root) {
        if let Some(idx) = chunk_statements.iter().position(|s| *s == stmt) {
            return Some(ChainPos::Chunk(idx));
        }
    }
    if proof.loop_ctx.block != proof.chunk_root {
        if let Node::Block(statements) = source.node(proof.loop_ctx.block) {
            if let Some(idx) = statements.iter().position(|s| *s == stmt) {
                return Some(ChainPos::Block(proof.loop_ctx.block, idx));
            }
        }
    }
    None
}

/// Verify that an alias value's name chain is temporally ordered.
/// When a statement captures `B = A`, the value `A` contributes is
/// the one established by the write of `A` that executed *before*
/// that statement, so resolving `B` through `A`'s defining write
/// is sound only when that write precedes the capturing statement
/// -- and recursively down the chain. (Cycle 94, Codex finding 1
/// chain form: `B = A` followed by `A = screen` leaves `B` holding
/// the pre-write value, not `screen`; the effect analyzer's alias
/// fixpoint is order-insensitive and resolves it anyway, so the
/// check lives here, at the hoisting consumer.) `pos` is where
/// `value` is read. A link through a global with a single
/// definition must be defined before the current position (chunk
/// statement order with no chunk goto, same-block order, or a
/// chunk definition preceding the loop's chunk statement for
/// in-function positions); a link through a local must be declared
/// before it in a comparable position. Anything unprovable
/// returns false.
fn alias_chain_ordered(
    source: &Ast,
    resolution: &Resolution,
    proof: &AliasProof,
    value: NodeId,
    pos: ChainPos,
) -> bool {
    use storm_lua_analysis::resolver::BindingKind;
    let chunk_goto = block_has_goto_label(source, proof.chunk_root);
    let Some(loop_top) = loop_top_chunk_idx(source, proof) else {
        return false;
    };
    let mut value = value;
    let mut pos = pos;
    for _ in 0..64 {
        match source.node(value) {
            Node::Index(obj, _, _) => {
                value = *obj;
            }
            Node::Name(_) => {
                let Some(bid) = resolution.node_bid[value as usize] else {
                    return false;
                };
                let Some(binding) = resolution.bindings.get(bid as usize) else {
                    return false;
                };
                match binding.kind {
                    BindingKind::Global => {
                        let writes = resolution.binding_write_counts[bid as usize];
                        if writes == 0 {
                            // The name itself is the (host) builtin
                            // table: a terminal link needing no order.
                            return true;
                        }
                        if writes != 1 {
                            return false;
                        }
                        // A definition in the loop's block is
                        // visible from a Block position; otherwise
                        // the definition must be a chunk statement
                        // preceding the position's chunk anchor.
                        let mut next = None;
                        if let ChainPos::Block(block, idx) = pos {
                            if let Some((def_idx, def_value)) =
                                block_single_def(source, resolution, block, bid)
                            {
                                if def_idx >= idx {
                                    return false;
                                }
                                next = Some((ChainPos::Block(block, def_idx), def_value));
                            }
                        }
                        if next.is_none() {
                            let Some((def_idx, def_value)) =
                                chunk_single_def(source, resolution, proof.chunk_root, bid)
                            else {
                                return false;
                            };
                            if chunk_goto {
                                return false;
                            }
                            let limit = match pos {
                                ChainPos::Chunk(idx) => idx,
                                ChainPos::Block(_, _) | ChainPos::Func(_) => loop_top,
                            };
                            if def_idx >= limit {
                                return false;
                            }
                            next = Some((ChainPos::Chunk(def_idx), def_value));
                        }
                        // Both arms above either record a
                        // definition in `next` or return outright,
                        // so a definition is always present here;
                        // the `else` keeps the chase conservative
                        // (and panic-free) if that ever changed.
                        let Some((npos, nvalue)) = next else {
                            return false;
                        };
                        pos = npos;
                        value = nvalue;
                    }
                    BindingKind::Local => {
                        let Some(decl) = binding.decl_node else {
                            return false;
                        };
                        let Node::Local(_, exprs) = source.node(decl) else {
                            return false;
                        };
                        let Some(bids) = resolution.node_bids.get(decl as usize) else {
                            return false;
                        };
                        let Some(idx) = bids.iter().position(|b| *b == bid) else {
                            return false;
                        };
                        let Some(init) = exprs.get(idx) else {
                            return false;
                        };
                        let Some(decl_pos) = direct_stmt_pos(source, proof, decl) else {
                            return false;
                        };
                        let ok = match (pos, decl_pos) {
                            (ChainPos::Chunk(c), ChainPos::Chunk(d)) => d < c,
                            (ChainPos::Block(b1, i), ChainPos::Block(b2, j)) => b1 == b2 && j < i,
                            (ChainPos::Block(_, _), ChainPos::Chunk(d)) => {
                                !chunk_goto && d < loop_top
                            }
                            (ChainPos::Func(_), ChainPos::Chunk(d)) => !chunk_goto && d < loop_top,
                            _ => false,
                        };
                        if !ok {
                            return false;
                        }
                        pos = decl_pos;
                        value = *init;
                    }
                    _ => return false,
                }
            }
            _ => return true,
        }
    }
    false
}

/// Resolve the builtin table denoted by an alias binding.
///
/// The returned flag says whether the alias definition is known
/// to dominate the loop: a local declaration always dominates its
/// uses (lexical scope), and the cycle-65 same-block form
/// guarantees it by construction (preceding statement, no goto,
/// no intervening call or table write). The chunk-top form only
/// dominates when the single definition is a chunk statement
/// preceding the chunk statement that contains the loop, and the
/// chunk block has no goto/label outside functions (which could
/// jump over the definition). Callers that hoist a read into a
/// preheader must require the flag: the preheader executes at
/// loop entry, before any later definition. Callers that only
/// classify (barrier checks, write harmlessness) ignore the flag
/// and stay conservative by treating any chunk-wide alias as
/// the table.
fn alias_table_root(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    mutated: &HashSet<String>,
    resolution: &Resolution,
    proof: &AliasProof,
    root_bid: BindingId,
) -> Option<(String, bool)> {
    use storm_lua_analysis::resolver::BindingKind;
    let binding = resolution.bindings.get(root_bid as usize)?;
    match binding.kind {
        BindingKind::Local => {
            if resolution.binding_write_counts[root_bid as usize] != 0 {
                return None;
            }
            let decl = binding.decl_node?;
            let Node::Local(_, exprs) = source.node(decl) else {
                return None;
            };
            let idx = resolution
                .node_bids
                .get(decl as usize)?
                .iter()
                .position(|bid| *bid == root_bid)?;
            // A multi-symbol local may have fewer initializers than
            // symbols (the rest are nil); such a binding is not a
            // table alias, and indexing past the initializer list
            // would panic.
            let init = exprs.get(idx)?;
            let table = analyzer.resolve_builtin_reference(*init)?;
            if table.contains('.') || mutated.contains(&table) {
                return None;
            }
            // Cycle 94 (Codex finding 1, chain form): the
            // initializer's name chain must be temporally ordered
            // (see `alias_chain_ordered`); the analyzer's alias
            // fixpoint alone would resolve `B = A` through a later
            // `A = screen` and misname the captured value.
            let decl_pos = direct_stmt_pos(source, proof, decl)
                .unwrap_or(ChainPos::Func(loop_top_chunk_idx(source, proof)?));
            if !alias_chain_ordered(source, resolution, proof, *init, decl_pos) {
                return None;
            }
            Some((table, true))
        }
        BindingKind::Global => {
            if resolution.binding_write_counts[root_bid as usize] != 1 {
                return None;
            }
            let Node::Block(chunk_statements) = source.node(proof.chunk_root) else {
                return None;
            };
            let mut found = None;
            for statement in chunk_statements {
                let Node::Assign(targets, values) = source.node(*statement) else {
                    continue;
                };
                for (target, value) in targets.iter().zip(values) {
                    if matches!(source.node(*target), Node::Name(_))
                        && resolution.node_bid[*target as usize] == Some(root_bid)
                    {
                        if found.is_some() {
                            return None;
                        }
                        found = Some((*statement, *value));
                    }
                }
            }
            if let Some((def_stmt, value)) = found {
                let table = analyzer.resolve_builtin_reference(value)?;
                if table.contains('.') || mutated.contains(&table) {
                    return None;
                }
                // Cycle 94 (Codex finding 1): a chunk-top alias
                // defined after the loop must not be hoisted: the
                // preheader read would execute before the
                // definition and index nil. Dominance here is the
                // definition preceding the chunk statement that
                // contains the loop, with no chunk-level goto that
                // could skip it. When it fails, fall through to
                // the cycle-65 arm below instead of returning.
                let def_idx = chunk_statements.iter().position(|s| *s == def_stmt)?;
                // Cycle 94 (Codex finding 1, chain form): every
                // link of the alias chain must be written before
                // the link that captures it (see
                // `alias_chain_ordered`); a reversed chain leaves
                // the alias holding a pre-write value, so the
                // resolution itself is unsound for every caller.
                if !alias_chain_ordered(source, resolution, proof, value, ChainPos::Chunk(def_idx))
                {
                    return None;
                }
                let loop_top_idx = chunk_statements
                    .iter()
                    .position(|s| subtree_contains(source, *s, proof.loop_ctx.stmt))?;
                let dominates =
                    def_idx < loop_top_idx && !block_has_goto_label(source, proof.chunk_root);
                return Some((table, dominates));
            }
            // Cycle-65 form: a single definition in the loop's own
            // block, preceding the loop, in a goto/label-free
            // enclosing function. The write count of exactly one
            // already guarantees this is the program's only write to
            // the global; the block position guarantees it dominates
            // the loop (same-block sequential execution, no jump can
            // skip it, no intervening call or table write).
            if proof.loop_ctx.fn_has_goto {
                return None;
            }
            let Node::Block(statements) = source.node(proof.loop_ctx.block) else {
                return None;
            };
            let loop_idx = statements.iter().position(|s| *s == proof.loop_ctx.stmt)?;
            let mut found_idx = None;
            let mut found_value = None;
            for (idx, statement) in statements.iter().enumerate().take(loop_idx) {
                let Node::Assign(targets, values) = source.node(*statement) else {
                    continue;
                };
                for (target, value) in targets.iter().zip(values) {
                    if matches!(source.node(*target), Node::Name(_))
                        && resolution.node_bid[*target as usize] == Some(root_bid)
                    {
                        if found_idx.is_some() {
                            return None;
                        }
                        found_idx = Some(idx);
                        found_value = Some(*value);
                    }
                }
            }
            let def_idx = found_idx?;
            let value = found_value?;
            let def_stmt = statements[def_idx];
            if subtree_has_call_or_table_write(source, def_stmt) {
                return None;
            }
            for statement in &statements[def_idx + 1..loop_idx] {
                if subtree_has_call_or_table_write(source, *statement) {
                    return None;
                }
            }
            let table = analyzer.resolve_builtin_reference(value)?;
            if table.contains('.') || mutated.contains(&table) {
                return None;
            }
            // Cycle 94 (Codex finding 1, chain form): same-block
            // chains must be temporally ordered as well; a def in
            // the chunk block is a Chunk position.
            let def_pos = if proof.loop_ctx.block == proof.chunk_root {
                ChainPos::Chunk(def_idx)
            } else {
                ChainPos::Block(proof.loop_ctx.block, def_idx)
            };
            if !alias_chain_ordered(source, resolution, proof, value, def_pos) {
                return None;
            }
            Some((table, true))
        }
        _ => None,
    }
}

/// Canonical key for a cacheable member call, or `None`. The key is
/// the builtin table root plus the member key, so different spellings
/// of the same member (a global root and an alias local for it) share
/// a group. In the direct-spelling form only global-root spellings
/// qualify; the post-alias form additionally accepts alias bases (see
/// `alias_table_root`).
fn candidate_key(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    mutated: &HashSet<String>,
    resolution: &Resolution,
    proof: &AliasProof,
    post_alias: bool,
    node: NodeId,
) -> Option<(String, Occurrence)> {
    let Node::Call(function, _, method) = source.node(node) else {
        return None;
    };
    if method.is_some() {
        return None;
    }
    let Node::Index(obj, key, dot) = source.node(*function) else {
        return None;
    };
    if !dot {
        return None;
    }
    let Node::Name(root_sym) = source.node(*obj) else {
        return None;
    };
    let Node::Str(raw) = source.node(*key) else {
        return None;
    };
    let raw: &str = raw;
    let inner = raw
        .strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .or_else(|| raw.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))?;
    if inner.is_empty() || inner.contains('\\') {
        return None;
    }
    let base_text = source.strings.get(*root_sym).to_string();
    let root_bid = resolution.node_bid[*obj as usize]?;
    // Direct builtin-root spelling. The analyzer resolves a global
    // root only when it is never written, so a successful resolution
    // already proves the root is stable.
    let direct = analyzer
        .resolve_builtin_reference(*function)
        .and_then(|builtin| {
            let (root, member) = builtin.split_once('.')?;
            if builtin.matches('.').count() != 1
                || root.is_empty()
                || member.is_empty()
                || mutated.contains(root)
                || base_text != root
            {
                return None;
            }
            Some(builtin)
        });
    if let Some(builtin) = direct {
        return Some((
            builtin,
            Occurrence {
                call: node,
                function: *function,
                root_bid,
            },
        ));
    }
    if !post_alias {
        return None;
    }
    // A hoisted preheader read executes at loop entry, before any
    // definition that follows the loop, so a candidate through an
    // alias is sound only when the alias definition dominates the
    // loop (see `alias_table_root`). Barrier classification callers
    // deliberately ignore this flag: they stay conservative by
    // treating any chunk-wide alias as the table.
    let (table, dominates) =
        alias_table_root(source, analyzer, mutated, resolution, proof, root_bid)?;
    if !dominates {
        return None;
    }
    Some((
        format!("{table}.{inner}"),
        Occurrence {
            call: node,
            function: *function,
            root_bid,
        },
    ))
}

/// Binding ids bound by statements in the loop body subtree (nested
/// loops included, nested functions excluded): loop-body `local`s,
/// `local function`s, loop variables, and assignment targets. An
/// alias defined inside the loop body must not be cached to a
/// preheader before the loop, where it is not in scope.
fn body_defined_bids(source: &Ast, resolution: &Resolution, body: NodeId) -> HashSet<BindingId> {
    let mut defined = HashSet::new();
    walk_full(source, body, &mut |node| {
        if matches!(
            source.node(node),
            Node::Local(..) | Node::Localfunc(..) | Node::Fornum(..) | Node::Forin(..)
        ) {
            if let Some(bids) = resolution.node_bids.get(node as usize) {
                defined.extend(bids.iter().copied());
            }
        }
        if let Node::Assign(targets, _) = source.node(node) {
            for target in targets {
                if matches!(source.node(*target), Node::Name(_)) {
                    if let Some(bid) = resolution.node_bid[*target as usize] {
                        defined.insert(bid);
                    }
                }
            }
        }
    });
    defined
}

/// Shared inputs for candidate collection (kept as one argument so
/// the collector stays under clippy's argument-count limit).
struct CollectCtx<'a> {
    source: &'a Ast,
    analyzer: &'a EffectAnalyzer<'a>,
    mutated: &'a HashSet<String>,
    resolution: &'a Resolution,
    chunk_root: NodeId,
    post_alias: bool,
    loop_ctx: LoopCtx,
}

fn collect_candidates(
    ctx: &CollectCtx<'_>,
    node: NodeId,
    is_root: bool,
    order: &mut Vec<String>,
    groups: &mut HashMap<String, Vec<Occurrence>>,
) {
    if !is_root && matches!(ctx.source.node(node), Node::Function(..)) {
        return;
    }
    if matches!(ctx.source.node(node), Node::Call(..)) {
        let proof = AliasProof {
            chunk_root: ctx.chunk_root,
            loop_ctx: ctx.loop_ctx,
        };
        if let Some((key, occurrence)) = candidate_key(
            ctx.source,
            ctx.analyzer,
            ctx.mutated,
            ctx.resolution,
            &proof,
            ctx.post_alias,
            node,
        ) {
            if !groups.contains_key(&key) {
                order.push(key.clone());
                groups.insert(key.clone(), Vec::new());
            }
            if let Some(group) = groups.get_mut(&key) {
                group.push(occurrence);
            }
        }
    }
    let mut children = Vec::new();
    ast_utils::for_each_child(ctx.source, node, &mut |child| children.push(child));
    for child in children {
        collect_candidates(ctx, child, false, order, groups);
    }
}

/// Analyse one loop. Returns `None` when the loop must not be cached,
/// otherwise the groups to cache (possibly empty).
fn analyze_loop(
    ctx: &BarrierCtx<'_>,
    body: NodeId,
    headers: &[NodeId],
    b3: &B3Cells,
) -> Option<Vec<CacheGroup>> {
    if loop_disqualified(ctx, body, headers) && !b3_rescue_loop(ctx, b3, body, headers) {
        return None;
    }
    let BarrierCtx {
        source,
        analyzer,
        mutated,
        resolution,
        chunk_root,
        post_alias,
        loop_ctx,
        ..
    } = ctx;
    let defined = body_defined_bids(source, resolution, body);
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<Occurrence>> = HashMap::new();
    let ctx = CollectCtx {
        source,
        analyzer,
        mutated,
        resolution,
        chunk_root: *chunk_root,
        post_alias: *post_alias,
        loop_ctx: *loop_ctx,
    };
    collect_candidates(&ctx, body, true, &mut order, &mut groups);
    let mut result = Vec::new();
    for key in order {
        let Some(occurrences) = groups.remove(&key) else {
            continue;
        };
        // The template must come from an occurrence whose root is not
        // bound inside the loop body; every occurrence is still
        // replaced, since each root denotes the same builtin value.
        let Some(template_occurrence) = occurrences
            .iter()
            .find(|occurrence| !defined.contains(&occurrence.root_bid))
        else {
            continue;
        };
        let template = template_occurrence.function;
        // S1 gate (see module docs): exactly one occurrence in this
        // loop body. Groups with two or more occurrences are the M2
        // shape and are left alone; the target budget, not a static
        // character gate, decides whether an S1 layout wins.
        let occ = occurrences.len();
        if occ != 1 {
            continue;
        }
        result.push(CacheGroup {
            occurrences,
            template,
        });
    }
    Some(result)
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

/// Allocate the cache local's name: the shortest single-letter
/// identifier absent from the program's string table, so the
/// preheader costs about nine characters net (see module docs).
/// A name absent from the table cannot be declared or referenced by
/// user code anywhere, so no shadowing is possible; groups cached
/// later in the same run see the earlier names already interned and
/// pick the next free letter. Falls back to the cycle-56 generated
/// name when every single letter is taken.
fn fresh_cache_symbol(output: &mut Ast) -> SymbolId {
    for candidate in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ".chars() {
        let name = candidate.to_string();
        if !output.strings.contains(&name) {
            return output.strings.intern(&name);
        }
    }
    output.strings.fresh("__membercache")
}

/// Lua refuses to load a function with more than 200
/// simultaneously active local variables, so a cache local
/// inserted by S1 must not push a function over that limit
/// (cycle 94, Codex finding 7: 196 parameters plus one cache
/// local plus a numeric-for loop failed to load). The count is
/// deliberately conservative: it is the maximum over the whole
/// function body rather than at the insertion point, and a
/// numeric for contributes its loop variable plus three control
/// variables, a generic for its loop variables plus three
/// control variables. Nested functions are separate functions
/// and are never counted into their enclosing function.
pub(crate) const MAX_LOCALS: usize = 200;

pub(crate) fn max_active_locals(source: &Ast, params: usize, body: NodeId) -> usize {
    fn walk_block(source: &Ast, block: NodeId, active: usize, max: &mut usize) -> usize {
        let Node::Block(statements) = source.node(block).clone() else {
            return active;
        };
        let mut cur = active;
        for statement in &statements {
            cur = walk_statement(source, *statement, cur, max);
        }
        cur
    }
    fn walk_statement(source: &Ast, statement: NodeId, active: usize, max: &mut usize) -> usize {
        match source.node(statement).clone() {
            Node::Local(symbols, _) => {
                let n = active + symbols.len();
                *max = (*max).max(n);
                n
            }
            Node::Localfunc(_, _) => {
                let n = active + 1;
                *max = (*max).max(n);
                n
            }
            Node::Do(body) => {
                walk_block(source, body, active, max);
                active
            }
            Node::Block(_) => {
                walk_block(source, statement, active, max);
                active
            }
            Node::While(_, body) | Node::Repeat(body, _) => {
                walk_block(source, body, active, max);
                active
            }
            Node::If(arms, otherwise) => {
                for arm in arms {
                    walk_block(source, arm.body, active, max);
                }
                if let Some(body) = otherwise {
                    walk_block(source, body, active, max);
                }
                active
            }
            Node::Fornum(_, _, _, _, body) => {
                let inner = active + 4;
                *max = (*max).max(inner);
                walk_block(source, body, inner, max);
                active
            }
            Node::Forin(vars, _, body) => {
                let inner = active + vars.len() + 3;
                *max = (*max).max(inner);
                walk_block(source, body, inner, max);
                active
            }
            _ => active,
        }
    }
    let mut max = params;
    walk_block(source, body, params, &mut max);
    max
}

struct Transformer<'a> {
    source: &'a Ast,
    analyzer: &'a EffectAnalyzer<'a>,
    mutated: &'a HashSet<String>,
    resolution: &'a Resolution,
    root: NodeId,
    post_alias: bool,
    output: Ast,
    cached_groups: usize,
    replaced: usize,
    /// Call nodes already cached by an outer loop (top-down claiming).
    claimed: HashSet<NodeId>,
    /// Maximum groups to cache in total (`None` = all groups). Used by
    /// the partial variant; groups beyond the limit keep their
    /// original spelling, which is always sound.
    group_limit: Option<usize>,
    /// Lazily built B3 chunk facts and memoized closure-body
    /// facts (cycle 86), shared by all loops of this run.
    b3: B3Cells,
    /// Blocks on the current transform path (innermost last); the
    /// top is the block directly containing the loop being analyzed.
    block_stack: Vec<NodeId>,
    /// Enclosing function bodies on the current transform path
    /// (innermost last); empty at the chunk top level.
    fn_stack: Vec<NodeId>,
    /// Parameter counts parallel to `fn_stack` (the chunk level
    /// has none).
    fn_param_stack: Vec<usize>,
    /// Cache locals already inserted per enclosing function body
    /// (chunk root for the chunk level), for the local budget.
    cache_locals_by_fn: HashMap<NodeId, usize>,
    /// Memoized `max_active_locals` per enclosing function body.
    fn_max_locals: HashMap<NodeId, usize>,
}

impl<'a> Transformer<'a> {
    fn new(source: &'a Ast, root: NodeId, analysis: &SpeedAnalysis<'a>, post_alias: bool) -> Self {
        Self {
            source,
            analyzer: analysis.effects,
            mutated: analysis.mutated,
            resolution: analysis.resolution,
            root,
            post_alias,
            output: clone_source(source),
            group_limit: None,
            b3: B3Cells::default(),
            cached_groups: 0,
            replaced: 0,
            claimed: HashSet::new(),
            block_stack: Vec::new(),
            fn_stack: Vec::new(),
            fn_param_stack: Vec::new(),
            cache_locals_by_fn: HashMap::new(),
            fn_max_locals: HashMap::new(),
        }
    }

    fn transform_block(&mut self, block: NodeId) {
        let Node::Block(statements) = self.source.node(block).clone() else {
            return;
        };
        self.block_stack.push(block);
        let mut rewritten = Vec::with_capacity(statements.len());
        for statement in &statements {
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
                    self.fn_param_stack.push(params.len() + extra);
                    self.transform_block(body);
                    self.fn_param_stack.pop();
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
        // Top-down claiming: analyze this loop before transforming its
        // body, so it claims member calls in its whole subtree
        // (including nested loops) before any inner loop can claim them.
        // When this loop is disqualified, nothing is claimed and the
        // body transform below still lets inner loops cache on their own.
        let groups = {
            let loop_block = self.block_stack.last().copied().unwrap_or(self.root);
            let fn_body = self.fn_stack.last().copied().unwrap_or(self.root);
            let loop_ctx = LoopCtx {
                block: loop_block,
                stmt: statement,
                fn_has_goto: block_has_goto_label(self.source, fn_body),
            };
            // Cycle 89: the global-alias chase needs the chunk
            // parent map and the chunk-level gates; both come from
            // the lazily built B3 chunk facts, shared by all loops
            // of this run (one build per run, memoized in the
            // OnceCell, exactly like the B3 rescue path uses).
            let chunk = self.b3.chunk_facts(self.source, self.root);
            let ctx = BarrierCtx {
                source: self.source,
                analyzer: self.analyzer,
                mutated: self.mutated,
                resolution: self.resolution,
                chunk_root: self.root,
                post_alias: self.post_alias,
                loop_ctx,
                parents: &chunk.parents,
                dynamic: chunk.dynamic,
                chunk_goto: chunk.chunk_goto,
            };
            analyze_loop(&ctx, body, headers, &self.b3)
        };
        let mut caches = Vec::new();
        if let Some(groups) = groups {
            for group in groups {
                let occurrences: Vec<Occurrence> = group
                    .occurrences
                    .into_iter()
                    .filter(|occurrence| !self.claimed.contains(&occurrence.call))
                    .collect();
                if occurrences.is_empty() {
                    continue;
                }
                if let Some(limit) = self.group_limit {
                    if self.cached_groups >= limit {
                        continue;
                    }
                }
                // Cycle 94 (Codex finding 7): inserting another
                // cache local must not push the enclosing function
                // over Lua's 200-active-locals load limit. Skip
                // the group (keeping its original spelling, which
                // is always sound) when the budget is exhausted.
                // The maximum is over the whole function body and
                // counts cache locals already inserted in this
                // run, so the check stays conservative even where
                // a cache local's scope is narrower than the whole
                // function.
                let fn_body = self.fn_stack.last().copied().unwrap_or(self.root);
                let fn_params = self.fn_param_stack.last().copied().unwrap_or(0);
                let fn_max = *self
                    .fn_max_locals
                    .entry(fn_body)
                    .or_insert_with(|| max_active_locals(self.source, fn_params, fn_body));
                let inserted = self.cache_locals_by_fn.get(&fn_body).copied().unwrap_or(0);
                if fn_max + inserted + 1 > MAX_LOCALS {
                    continue;
                }
                *self.cache_locals_by_fn.entry(fn_body).or_insert(0) += 1;
                let symbol = fresh_cache_symbol(&mut self.output);
                let cloned = clone_subtree(&mut self.output, self.source, group.template);
                let mut cloned_nodes = Vec::new();
                ast_utils::walk(&self.output, cloned, &mut |id| cloned_nodes.push(id));
                for id in cloned_nodes {
                    self.output.nodes.mark_synthetic(id, REASON);
                }
                let local = self.output.push(Node::Local(vec![symbol], vec![cloned]));
                self.output.nodes.mark_synthetic(local, REASON);
                for occurrence in &occurrences {
                    self.output
                        .nodes
                        .rewrite(occurrence.function, Node::Name(symbol), REASON);
                    super::origins::derive(
                        &mut self.output,
                        occurrence.function,
                        self.source,
                        &[occurrence.function],
                        REASON,
                    );
                    self.claimed.insert(occurrence.call);
                }
                self.cached_groups += 1;
                self.replaced += occurrences.len();
                caches.push(local);
            }
        }
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
                self.fn_param_stack.push(params.len());
                self.transform_block(body);
                self.fn_param_stack.pop();
                self.fn_stack.pop();
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
/// `cached=N` (groups) and `replaced=M` (call sites) in `details`
/// (0 when nothing changed, in which case `ast` is left untouched).
/// `group_limit` caps the total groups cached (`None` = all groups).
fn run_cached(
    ast: &mut Ast,
    root: NodeId,
    post_alias: bool,
    group_limit: Option<usize>,
    shared: Option<&SpeedAnalysis<'_>>,
) -> PassResult {
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
    let mutated;
    let local;
    let analysis = match shared {
        Some(shared) => shared,
        None => {
            resolution = resolve(&source, root);
            effects = EffectAnalyzer::new(&source, &resolution, root, true);
            mutated = mutated_builtin_roots(&source, &resolution, root);
            local = SpeedAnalysis {
                resolution: &resolution,
                effects: &effects,
                mutated: &mutated,
            };
            &local
        }
    };
    let mut transformer = Transformer::new(&source, root, analysis, post_alias);
    transformer.group_limit = group_limit;
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

/// Cache single member calls in loops, post-alias form: runs on the
/// api-aliased layout, where callee bases may be root aliases as well
/// as direct global spellings (see module docs).
pub fn cache_single_member_calls_post_alias(ast: &mut Ast, root: NodeId) -> PassResult {
    run_cached(ast, root, true, None, None)
}

/// Post-alias form over a caller-built shared analysis (see
/// `passes::speed_analysis`): `analysis` must have been built over
/// an AST whose node and string arenas are identical to `ast`'s
/// (a verbatim clone qualifies). The result is identical to
/// [`cache_single_member_calls_post_alias`]; only the repeated
/// analysis construction is saved.
pub fn cache_single_member_calls_post_alias_with_analysis(
    ast: &mut Ast,
    root: NodeId,
    analysis: &SpeedAnalysis<'_>,
) -> PassResult {
    run_cached(ast, root, true, None, Some(analysis))
}

/// Partial post-alias form over a caller-built shared analysis;
/// see [`cache_single_member_calls_post_alias_with_analysis`].
pub fn cache_single_member_calls_post_alias_partial_with_analysis(
    ast: &mut Ast,
    root: NodeId,
    analysis: &SpeedAnalysis<'_>,
) -> PassResult {
    run_cached(ast, root, true, Some(1), Some(analysis))
}

/// Partial post-alias form: cache only the first group in traversal
/// order. Used by the `*-s1-partial` target-size layouts in
/// `search.rs`; the caller offers it only when the full S1 run
/// cached more than one group, so the partial layout always differs
/// from the full one. Soundness is unchanged: uncached groups keep
/// their original spelling, and the cached group is proven exactly
/// as in the full form.
pub fn cache_single_member_calls_post_alias_partial(ast: &mut Ast, root: NodeId) -> PassResult {
    run_cached(ast, root, true, Some(1), None)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn run_post_alias(source: &str) -> (String, usize) {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = cache_single_member_calls_post_alias(&mut ast, root);
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

    fn run_post_alias_shared(source: &str, partial: bool) -> (String, usize) {
        let (ast, root) = parse_source(source).expect("parse");
        let resolution = resolve(&ast, root);
        let effects = EffectAnalyzer::new(&ast, &resolution, root, true);
        let mutated = mutated_builtin_roots(&ast, &resolution, root);
        let analysis = SpeedAnalysis {
            resolution: &resolution,
            effects: &effects,
            mutated: &mutated,
        };
        let mut shared_ast = ast.clone();
        let result = if partial {
            cache_single_member_calls_post_alias_partial_with_analysis(
                &mut shared_ast,
                root,
                &analysis,
            )
        } else {
            cache_single_member_calls_post_alias_with_analysis(&mut shared_ast, root, &analysis)
        };
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
    fn caches_loop_calling_global_helper_defined_before() {
        // Cycle 86 (B3): a global helper defined by a single chunk
        // Funcstat before the loop's function, whose body only calls
        // plain builtins, no longer disqualifies the loop.
        let (_, cached) = run_post_alias(
            "function helper(x) return math.floor(x) end function onTick() while true do helper(1) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn caches_loop_with_transitive_global_helpers() {
        // Cycle 86 (B3): the closure is transitive -- helper calls
        // h2, whose only write is to a constructor-built local table.
        let (_, cached) = run_post_alias(
            "function h2() local t={} t.x=1 return t.x end function helper() return h2() end function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn caches_loop_when_helper_writes_plain_global() {
        // Cycle 86 (B3): a global write to a non-builtin,
        // non-callee name inside a helper body is harmless.
        let (_, cached) = run_post_alias(
            "function helper() state=1 return state end function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn caches_loop_when_helper_uses_local_function() {
        // Cycle 86 (B3): a write-free local function defined and
        // called inside a helper body joins the closure.
        let (_, cached) = run_post_alias(
            "function helper() local f=function() return 1 end return f() end function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn caches_loop_when_helper_defined_after() {
        // Cycle 92 (B3b): the definition no longer needs to
        // dominate the loop's top-level statement. A helper
        // defined after onTick qualifies by call-error
        // equivalence: callbacks run after the chunk has loaded,
        // and a call before the definition would error on the
        // nil callee identically with or without the cache.
        let (_, cached) = run_post_alias(
            "function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end function helper() return 1 end",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn skips_loop_calling_alias_defined_after_loop() {
        // Cycle 94 (Codex finding 1): S1 must not hoist a call
        // through a global alias whose single definition follows
        // the loop: the preheader read would execute before the
        // definition and index nil. The chunk-level loop here runs
        // during chunk load, so the definition has not run yet.
        let (_, cached) =
            run_post_alias("for i=1,property.getNumber('n') do S.drawLine(0,0,1,1) end S=screen");
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_loop_calling_alias_defined_before_loop() {
        // Control for the finding-1 fix: the same shape with the
        // definition preceding the loop still caches.
        let (_, cached) =
            run_post_alias("S=screen for i=1,property.getNumber('n') do S.drawLine(0,0,1,1) end");
        assert_eq!(cached, 1);
    }

    #[test]
    fn skips_loop_calling_chain_reversed_global_alias() {
        // Cycle 94 (Codex finding 1, chain form found by w6 cycle
        // 47): `B = A` executes while `A` is still nil, so `B`
        // never denotes `screen` even though `A = screen` precedes
        // the loop. Resolving the chain by final values would
        // hoist a `B.drawRect` read that indexes nil.
        let (_, cached) = run_post_alias(
            "B=A A=screen function onTick() for i=1,property.getNumber('n') do B.drawRect(i,0,1,1) end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_calling_mid_reversed_global_alias_chain() {
        // Three links with the middle one reversed: `C = B`
        // captures B's pre-write value, so C does not denote
        // screen even though every link precedes the loop.
        let (_, cached) = run_post_alias(
            "A=screen C=B B=A function onTick() for i=1,property.getNumber('n') do C.drawRect(i,0,1,1) end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_toplevel_loop_calling_chain_reversed_alias() {
        // Top-level loop form: the alias definition sits in the
        // chunk block itself (the cycle-65 arm), so the chain
        // check must apply there too.
        let (_, cached) = run_post_alias(
            "B=A A=screen for i=1,property.getNumber('n') do B.drawRect(i,0,1,1) end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_loop_calling_ordered_global_alias_chain() {
        // Control: an ordered two-link chain still caches.
        let (_, cached) = run_post_alias(
            "A=screen B=A function onTick() for i=1,property.getNumber('n') do B.drawRect(i,0,1,1) end end",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn skips_loop_calling_alias_defined_after_enclosing_function() {
        // Conservative trade-off, recorded deliberately: the loop
        // sits inside a function defined before the alias, so at
        // callback time the alias would already be defined and
        // hoisting would be safe in practice. The dominance check
        // still rejects it, because the same shape is unsound when
        // the function is called during chunk load before the
        // definition. Revisit only with a call-site proof.
        let (_, cached) = run_post_alias(
            "function onTick() while true do S.drawLine(0,0,1,1) if true then break end end end S=screen",
        );
        assert_eq!(cached, 0);
    }

    fn many_param_source(n: usize) -> String {
        let params: Vec<String> = (1..=n).map(|i| format!("p{i}")).collect();
        format!(
            "function onTick({}) for i=1,2 do output.setBool(1,true) end end",
            params.join(",")
        )
    }

    #[test]
    fn skips_cache_when_locals_would_exceed_limit() {
        // Cycle 94 (Codex finding 7): 196 parameters plus a
        // numeric-for loop (loop variable + three controls) already
        // use 200 active locals, so one more cache local would
        // make the generated code unloadable. S1 must skip it.
        let source = many_param_source(196);
        let (_, cached) = run_post_alias(&source);
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_cache_when_locals_stay_at_limit() {
        // Boundary control: 195 parameters leave room for exactly
        // one cache local (195 + 4 + 1 = 200), so caching stays.
        let source = many_param_source(195);
        let (_, cached) = run_post_alias(&source);
        assert_eq!(cached, 1);
    }

    #[test]
    fn skips_cache_in_method_at_limit_counting_self() {
        // Cycle 95 (w6 cycle-52 audit, a1 shape): a method's
        // implicit `self` is a real parameter at load time. 195
        // explicit parameters plus `self` and the numeric-for
        // controls already reach 200, so the cache local would be
        // the 201st. The pre-fix accounting passed only the
        // explicit parameter count (199) and fired.
        let params: Vec<String> = (1..=195).map(|i| format!("p{i}")).collect();
        let source = format!(
            "local obj = {{}} function obj:m({}) for i=1,2 do output.setBool(1,true) end end",
            params.join(",")
        );
        let (_, cached) = run_post_alias(&source);
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_in_method_one_below_limit_counting_self() {
        // Boundary control: 194 explicit parameters plus `self`
        // and the for controls peak at 199, leaving room for
        // exactly one cache local (200 with the cache).
        let params: Vec<String> = (1..=194).map(|i| format!("p{i}")).collect();
        let source = format!(
            "local obj = {{}} function obj:m({}) for i=1,2 do output.setBool(1,true) end end",
            params.join(",")
        );
        let (_, cached) = run_post_alias(&source);
        assert_eq!(cached, 1);
    }

    #[test]
    fn caches_loop_when_helper_defined_in_function_body_after_loop() {
        // Cycle 92 (B3b): the single Funcstat may sit in any
        // position, including a function body after the loop
        // statement itself; until it runs, the callee is nil and
        // the call errors identically.
        let (_, cached) = run_post_alias(
            "function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end function helper() return 1 end end",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn caches_loop_when_chunk_has_goto_and_helper_defined_after() {
        // Cycle 92 (B3b): a chunk-level goto/label no longer
        // blocks identity -- re-executing the definition only
        // creates a fresh closure of the same body.
        let (_, cached) = run_post_alias(
            "function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end function helper() return 1 end if true then goto done end ::done::",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn caches_loop_when_helper_defined_in_block() {
        // Cycle 92 (B3b): the definition need not be a direct
        // chunk statement; a single Funcstat nested in a branch
        // block qualifies (if the branch never runs, the callee
        // is nil and the call errors identically).
        let (_, cached) = run_post_alias(
            "if true then function helper() return 1 end end function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn skips_loop_when_helper_reassigned() {
        // Cycle 86 (B3): a second write to the helper binding
        // breaks the single-definition identity.
        let (_, cached) = run_post_alias(
            "function helper() return 1 end helper=nil function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_when_helper_defined_twice() {
        // Cycle 86 (B3): two definitions are two writes.
        let (_, cached) = run_post_alias(
            "function helper() return 1 end function helper() return 2 end function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_when_helper_calls_stateful() {
        // Cycle 86 (B3): the stateful list applies inside helper
        // bodies exactly as in loop bodies.
        let (_, cached) = run_post_alias(
            "function helper() return math.random() end function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_when_helper_calls_unknown_global() {
        // Cycle 86 (B3): a call to a global with no chunk
        // definition inside a helper body keeps the loop barred.
        let (_, cached) = run_post_alias(
            "function helper() return mystery() end function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_when_helper_passes_root_to_builtin() {
        // Cycle 86 (B3): the argument rule applies inside helper
        // bodies: passing a whole host table stays a barrier.
        let (_, cached) = run_post_alias(
            "function helper() return tostring(screen) end function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_when_helper_call_passes_root() {
        // Cycle 86 (B3): the argument rule also applies to the B3
        // call in the loop body itself.
        let (_, cached) = run_post_alias(
            "function helper(t) return 1 end function onTick() while true do helper(screen) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_when_helper_writes_root_field() {
        // Cycle 86 (B3): a field write through a builtin root
        // inside a helper body is not harmless.
        let (_, cached) = run_post_alias(
            "function helper() screen.drawRectF=nil end function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_when_helper_writes_builtin_name() {
        // Cycle 86 (B3): a helper body must not rebind a builtin
        // root/global name.
        let (_, cached) = run_post_alias(
            "function helper() pairs=nil return 1 end function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_when_helper_has_goto() {
        // Cycle 86 (B3): goto/label inside a helper body keeps the
        // body outside the closure.
        let (_, cached) = run_post_alias(
            "function helper() goto x ::x:: return 1 end function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_when_helper_passes_function_value() {
        // Cycle 86 (B3): a function value in a non-definition
        // position (here a gsub replacement function) could run
        // through paths the closure cannot follow.
        let (_, cached) = run_post_alias(
            "function helper() return string.gsub(\"a\",\"a\",function() return \"b\" end) end function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_when_helper_uses_method_call() {
        // Cycle 86 (B3): method calls inside helper bodies are
        // disqualifying, as in loop bodies.
        let (_, cached) = run_post_alias(
            "function helper() local t={} return t:m() end function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_when_chunk_uses_rawset() {
        // Cycle 86 (B3): a rawset call anywhere in the chunk opens
        // dynamic global writes, so no B3 identity is provable.
        let (_, cached) = run_post_alias(
            "function helper() return 1 end function onTick() rawset(_G,\"x\",1) while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn shared_analysis_matches_standalone() {
        // The shared-analysis entries must reproduce the standalone
        // entries exactly: the analysis is a pure function of the
        // arena contents, and the pass input is a verbatim clone of
        // the AST the analysis was built over (as in search.rs).
        let firing =
            "function onTick() while true do screen.drawRectF(1,2,3,4) if true then break end end end";
        let standalone = run_post_alias(firing);
        assert!(standalone.1 > 0, "input must fire S1: {standalone:?}");
        assert_eq!(run_post_alias_shared(firing, false), standalone);
        assert_eq!(run_post_alias_shared(firing, true), run_partial(firing));
        let quiet = "function onTick() local x=1 end";
        assert_eq!(run_post_alias_shared(quiet, false), run_post_alias(quiet));
        assert_eq!(run_post_alias_shared(quiet, true), run_partial(quiet));
    }

    #[test]
    fn caches_single_member_call_in_while_loop() {
        let (code, cached) = run_post_alias(
            "function onTick() while true do screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("screen.drawRectF").count(), 1, "code: {code}");
        assert!(
            code.contains("local a=screen.drawRectF"),
            "cache local must use the shortest absent name: {code}"
        );
        let cache_pos = code.find("screen.drawRectF").expect("cache present");
        let while_pos = code.find("while").expect("loop present");
        assert!(cache_pos < while_pos, "cache must precede the loop: {code}");
    }

    #[test]
    fn caches_single_member_call_in_fornum_loop() {
        let (code, cached) =
            run_post_alias("function onTick() for i=1,10 do screen.drawRectF(i,0,2,2) end end");
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("screen.drawRectF").count(), 1, "code: {code}");
    }

    #[test]
    fn post_alias_caches_through_chunk_global_alias() {
        let (code, cached) = run_post_alias(
            "a=screen function onDraw() for i=1,128 do a.drawRectF(i,0,2,2) end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("a.drawRectF").count(), 1, "code: {code}");
        assert!(
            code.contains("local b=a.drawRectF"),
            "cache local must skip the taken name a: {code}"
        );
    }

    #[test]
    fn post_alias_caches_through_chunk_local_alias() {
        let (code, cached) = run_post_alias(
            "local a=screen function onDraw() for i=1,128 do a.drawRectF(i,0,2,2) end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("a.drawRectF").count(), 1, "code: {code}");
    }

    #[test]
    fn post_alias_skips_nil_initialized_multi_symbol_local() {
        // Regression (cycle 56): `local a, b = screen` binds b to nil,
        // and the initializer list is shorter than the symbol list;
        // looking up b's initializer must return no alias, not panic.
        let (_, cached) = run_post_alias(
            "local a, b = screen function onDraw() for i=1,128 do b.drawRectF(i,0,2,2) end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn post_alias_skips_reassigned_alias() {
        let (_, cached) = run_post_alias(
            "a=screen a=output function onDraw() for i=1,128 do a.drawRectF(i,0,2,2) end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn post_alias_skips_when_table_root_mutated() {
        let (_, cached) = run_post_alias(
            "a=screen screen.drawRectF=function() return end function onDraw() for i=1,128 do a.drawRectF(i,0,2,2) end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn post_alias_caches_through_function_internal_global_alias() {
        let (code, cached) = run_post_alias(
            "function onDraw() l=screen for i=1,128 do l.drawRectF(i,0,2,2) end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("l.drawRectF").count(), 1, "code: {code}");
        assert!(
            code.contains("local a=l.drawRectF"),
            "cache local must use the shortest absent name: {code}"
        );
    }

    #[test]
    fn post_alias_skips_function_internal_alias_after_loop() {
        let (_, cached) = run_post_alias(
            "function onDraw() for i=1,128 do l.drawRectF(i,0,2,2) end l=screen end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn post_alias_skips_function_internal_alias_in_branch() {
        let (_, cached) = run_post_alias(
            "function onDraw() if true then l=screen end for i=1,128 do l.drawRectF(i,0,2,2) end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn post_alias_skips_function_internal_alias_with_call_between() {
        let (_, cached) = run_post_alias(
            "function onDraw() l=screen foo() for i=1,128 do l.drawRectF(i,0,2,2) end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn post_alias_skips_function_internal_alias_with_goto() {
        let (_, cached) = run_post_alias(
            "function onDraw() goto skip l=screen ::skip:: for i=1,128 do l.drawRectF(i,0,2,2) end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_repeated_occurrences_in_one_loop() {
        // M2 shape: left alone (alias-covered in winners, cycle 56).
        let (code, cached) = run_post_alias(
            "function onTick() while true do screen.drawRectF(1,2,3,4) screen.drawRectF(5,6,7,8) screen.drawRectF(9,10,11,12) if true then break end end end",
        );
        assert_eq!(cached, 0, "code: {code}");
        let (_, cached) = run_post_alias(
            "function onTick() while true do screen.setColor(1,2,3) screen.setColor(4,5,6) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_alias_defined_inside_loop() {
        let (_, cached) = run_post_alias(
            "function onTick() while true do local s=screen s.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_with_unknown_call() {
        let (_, cached) = run_post_alias(
            "function onTick() while true do foo() screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }
    #[test]
    fn caches_loop_with_localized_member_call() {
        // Cycle 84 (B1): a call through a never-reassigned local
        // holding a builtin member is a known builtin call, not an
        // unknown one.
        let (_, cached) = run_post_alias(
            "local sub=string.sub function onTick() while true do local x=sub('abc',1,2) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn caches_loop_with_chained_local_member_call() {
        // The member is read through a localized root: the chase
        // follows `sub`'s initializer into `str`, and `str`'s into
        // the root name.
        let (_, cached) = run_post_alias(
            "local str=string local sub=str.sub function onTick() while true do local x=sub('abc',1,2) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn caches_loop_with_member_call_through_aliased_root_local() {
        // Index callee over a root local that is itself an alias of
        // another root local (the member call occurs twice, so it is
        // the M2 shape and only the drawRectF group is cached).
        let (_, cached) = run_post_alias(
            "local a=string local b=a function onTick() while true do local x=b.sub('abc',1,2) local y=b.sub('abc',2,3) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn skips_loop_with_reassigned_member_local() {
        // A reassigned local can hold anything at the call site: the
        // chase stops and the call stays unknown.
        let (_, cached) = run_post_alias(
            "local sub=string.sub sub=string.find function onTick() while true do local x=sub('abc',1,2) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_with_localized_stateful_call() {
        // The recovered spelling passes through the stateful list:
        // math.random stays disqualifying through a local.
        let (_, cached) = run_post_alias(
            "local rnd=math.random function onTick() while true do local x=rnd(3) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_loop_with_map_to_screen_bystander() {
        // Cycle 93 (map.* certification, user decision 2026-10-31):
        // map.mapToScreen is a pure coordinate transform, so the loop
        // is no longer disqualified: both the mapToScreen group and
        // the drawRectF group cache.
        let (_, cached) = run_post_alias(
            "function onTick() while true do local x,y=map.mapToScreen(1,2) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 2);
    }

    #[test]
    fn caches_loop_with_screen_to_map_bystander() {
        // Cycle 93: the inverse transform is certified on the same
        // grounds as mapToScreen.
        let (_, cached) = run_post_alias(
            "function onTick() while true do local x,y=map.screenToMap(1,2) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 2);
    }

    #[test]
    fn skips_loop_with_uncertified_map_call() {
        // Cycle 93: every other map.* member stays disqualifying.
        let (_, cached) = run_post_alias(
            "function onTick() while true do local x=map.other(1,2) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_with_map_member_write() {
        // Cycle 93: writing through the map root still disqualifies,
        // so the cached member can never be rewritten by the loop.
        let (_, cached) = run_post_alias(
            "function onTick() while true do map.mapToScreen=1 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_loop_with_helper_calling_map_to_screen() {
        // Cycle 93: the stateful list applies inside helper bodies
        // exactly as in loop bodies, so the certified member does not
        // dirty a B3-eligible helper either.
        let (_, cached) = run_post_alias(
            "function helper() return map.mapToScreen(1,2) end function onTick() while true do helper() screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn skips_loop_with_localized_call_passing_root() {
        // The argument rule applies to the recovered call as well:
        // passing a whole host table stays a barrier.
        let (_, cached) = run_post_alias(
            "local sub=string.sub function onTick() while true do local x=sub(screen,1,2) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_loop_with_write_to_plain_local_table() {
        // Cycle 83: a field write through a plain, never-escaping
        // constructor table cannot reach a cached root member, so it
        // no longer disqualifies the loop.
        let (code, cached) = run_post_alias(
            "function onTick() local t={} while true do t.x=1 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert!(
            code.contains("t.x=1"),
            "the write itself is untouched: {code}"
        );
    }

    #[test]
    fn caches_loop_with_write_to_plain_global_table() {
        let (code, cached) = run_post_alias(
            "state={} function onTick() while true do state.x=1 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1, "code: {code}");
    }

    #[test]
    fn caches_loop_with_write_to_multi_defined_global_table() {
        // Cycle 98: every definition of the global is a table
        // constructor, so whichever definition executed last, a
        // field write through it lands on a fresh plain table.
        let (code, cached) = run_post_alias(
            "state={} state={} function onTick() while true do state.x=1 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1, "code: {code}");
    }

    #[test]
    fn caches_loop_with_write_to_multi_defined_global_table_multiassign() {
        // One definition arrives through a multi-assignment, zipped
        // positionally with its constructor value.
        let (code, cached) = run_post_alias(
            "state,other={},0 state={} function onTick() while true do state.x=1 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1, "code: {code}");
    }

    #[test]
    fn caches_loop_with_write_to_multi_defined_global_table_def_in_function() {
        // A definition inside another function body still counts:
        // the enumeration covers the whole chunk.
        let (code, cached) = run_post_alias(
            "state={} function onDraw() state={} end function onTick() while true do state.x=1 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1, "code: {code}");
    }

    #[test]
    fn skips_loop_with_write_to_multi_defined_global_with_nontable_def() {
        let (_, cached) = run_post_alias(
            "state={} state=other function onTick() while true do state.x=1 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_with_write_to_multi_defined_global_with_funcstat_def() {
        let (_, cached) = run_post_alias(
            "state={} function state() end function onTick() while true do state.x=1 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_with_write_to_multi_defined_global_trailing_nil_def() {
        // The first statement assigns past the end of its value
        // list, so `state` receives nil there, not a constructor.
        let (_, cached) = run_post_alias(
            "other,state=1 state={} function onTick() while true do state.x=1 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_with_write_to_multi_defined_global_escaped() {
        // Reading the base as a value (here, storing it into
        // another global) is an escape even when every definition
        // is a constructor.
        let (_, cached) = run_post_alias(
            "state={} state={} sink=state function onTick() while true do state.x=1 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_loop_with_write_to_table_declared_in_body() {
        let (code, cached) = run_post_alias(
            "function onTick() while true do local t={} t.x=1 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1, "code: {code}");
    }

    #[test]
    fn caches_loop_with_method_definition_on_plain_table() {
        let (code, cached) = run_post_alias(
            "function onTick() local t={} while true do function t.m() end screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1, "code: {code}");
    }

    #[test]
    fn skips_loop_with_root_table_write() {
        // A write through the builtin root itself can rewrite the
        // cached member (or any sibling field), so it disqualifies.
        let (_, cached) = run_post_alias(
            "function onTick() while true do screen.x=1 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_with_alias_table_write() {
        // A write through a proven root alias denotes the root table.
        let (_, cached) = run_post_alias(
            "local s=screen function onTick() while true do s.x=1 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_with_write_to_escaped_plain_table() {
        // Storing the table into another table is an escape: the
        // stored reference is an alias route (and a setmetatable
        // route) to the same object, so the base is unproven.
        let (_, cached) = run_post_alias(
            "function onTick() local t={} local u={} u.ref=t while true do t.x=1 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_with_write_to_reassigned_table() {
        let (_, cached) = run_post_alias(
            "function onTick() local t={} t={} while true do t.x=1 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_with_write_to_nonconstructor_local() {
        let (_, cached) = run_post_alias(
            "function onTick() local t=make() while true do t.x=1 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_with_write_through_deeper_chain() {
        // The outer write's base is `t.sub`, not a bare name: the
        // intermediate field could denote anything.
        let (_, cached) = run_post_alias(
            "function onTick() local t={} while true do t.sub.x=1 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_with_mixed_harmless_and_root_writes() {
        let (_, cached) = run_post_alias(
            "function onTick() local t={} while true do t.x=1 screen.y=2 screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_with_method_call() {
        let (_, cached) = run_post_alias(
            "function onTick() local o={} while true do o:draw(1) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_when_root_is_mutated() {
        let (_, cached) = run_post_alias(
            "screen.drawRectF=function() return end function onTick() while true do screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn outer_loop_claims_single_call_in_nested_loop() {
        // One preheader before the outer loop serves the nested site.
        let (code, cached) = run_post_alias(
            "function onTick() for i=1,10 do for j=1,10 do screen.drawRectF(i,j,2,2) end end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        let cache_pos = code.find("screen.drawRectF").expect("cache present");
        let first_for = code.find("for").expect("loop present");
        assert!(
            cache_pos < first_for,
            "cache must precede the outer loop: {code}"
        );
    }

    #[test]
    fn straight_line_occurrence_does_not_block_loop_caching() {
        // Per-loop scope: a straight-line occurrence keeps its
        // spelling while the loop occurrence is cached.
        let (code, cached) = run_post_alias(
            "screen.drawRectF(0,0,1,1) function onTick() for i=1,10 do screen.drawRectF(i,0,2,2) end end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("screen.drawRectF").count(), 2, "code: {code}");
    }

    fn run_partial(source: &str) -> (String, usize) {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = cache_single_member_calls_post_alias_partial(&mut ast, root);
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

    #[test]
    fn partial_caches_only_the_first_group() {
        let source = "function onTick() for i=1,10 do screen.drawRectF(i,0,2,2) end for j=1,10 do screen.setColor(1,2,3) end end";
        let (code, cached) = run_partial(source);
        assert_eq!(cached, 1, "code: {code}");
        assert!(
            code.contains("local a=screen.drawRectF"),
            "first group must be cached: {code}"
        );
        assert!(
            !code.contains("local b=screen.setColor"),
            "second group must keep its original spelling: {code}"
        );
        assert_eq!(code.matches("screen.setColor").count(), 1, "code: {code}");
    }

    #[test]
    fn partial_matches_full_for_a_single_group() {
        let source =
            "function onTick() while true do screen.drawRectF(1,2,3,4) if true then break end end end";
        let (full_code, full_cached) = run_post_alias(source);
        let (partial_code, partial_cached) = run_partial(source);
        assert_eq!(full_cached, 1);
        assert_eq!(partial_cached, 1);
        assert_eq!(partial_code, full_code);
    }

    #[test]
    fn caches_each_loop_separately() {
        let (code, cached) = run_post_alias(
            "function onTick() for i=1,10 do screen.drawRectF(i,0,2,2) end for j=1,10 do screen.setColor(1,2,3) end end",
        );
        assert_eq!(cached, 2, "code: {code}");
        assert_eq!(code.matches("screen.drawRectF").count(), 1, "code: {code}");
        assert_eq!(code.matches("screen.setColor").count(), 1, "code: {code}");
    }

    #[test]
    fn caches_loop_with_bare_name_global_alias_member() {
        // Cycle 89 (global-alias chase): a chunk-top global alias
        // for a builtin member, used as a bare-name call, no longer
        // disqualifies the loop.
        let (_, cached) = run_post_alias(
            "m = math.floor function onTick() while true do m(1) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn caches_loop_with_bare_name_local_alias_to_bare_builtin() {
        // Cycle 89: a bare-name call through a local alias to a
        // bare builtin (`pairs` itself, not a member) is a known
        // call -- the cycle-84 chase required a dotted spelling and
        // could not rescue it.
        let (_, cached) = run_post_alias(
            "local p = pairs function onTick() while true do p(1) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn caches_loop_with_same_block_global_alias() {
        // Cycle 89: a global alias defined in the loop's own block,
        // preceding it with no intervening call or table write,
        // dominates by the cycle-65 same-block form.
        let (_, cached) = run_post_alias(
            "function onTick() m = math.floor while true do m(1) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn caches_loop_with_chained_local_global_alias_index_base() {
        // Cycle 89: a chained alias base (local over a same-block
        // global alias for a builtin table) rescues an index
        // barrier call the single-stage paths cannot resolve.
        let (_, cached) = run_post_alias(
            "function onTick() S = screen local H = S while true do H.drawCircleF(1,2,3) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 1);
    }

    #[test]
    fn skips_loop_with_reassigned_global_alias() {
        // Cycle 89: a second write to the alias binding breaks the
        // single-definition identity.
        let (_, cached) = run_post_alias(
            "m = math.floor m = math.ceil function onTick() while true do m(1) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_with_global_alias_defined_after_loop() {
        // Cycle 89: a definition after the loop in the same block
        // does not dominate it. Pinned on a chase-only form (a
        // function-body alias as a bare-name call): a chunk-top
        // alias defined after the loop's function still resolves
        // through the analyzer's direct path -- sound there,
        // because the whole chunk executes before any tick, and
        // pre-existing behavior the chase does not change.
        let (_, cached) = run_post_alias(
            "function onTick() while true do m(1) screen.drawRectF(1,2,3,4) if true then break end end m = math.floor end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_with_global_alias_in_branch_block() {
        // Cycle 89: a definition in a branch block does not
        // dominate the loop. Same-function-body dominance alone
        // (the recorded "loose" form) is not adopted: the
        // cycle-65 intervening condition is required with it.
        let (_, cached) = run_post_alias(
            "function onTick() if true then m = math.floor end while true do m(1) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_with_dynamic_chunk_global_alias() {
        // Cycle 89: dynamic global access voids the static write
        // set, so no global chase step can be trusted. Pinned on a
        // chase-only form (a function-body alias as a bare-name
        // call): a chunk-top alias resolves through the analyzer's
        // direct path, and a function-body alias as an index base
        // through the alias-table path, neither of which carries a
        // dynamic gate -- pre-existing behavior, not something the
        // chase changes.
        let (_, cached) = run_post_alias(
            "function onTick() m = math.floor while true do m(1) screen.drawRectF(1,2,3,4) if true then break end end end rawset({}, 'x', 1)",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_with_noncatalog_global_alias() {
        // Cycle 89: an alias for a non-catalog root (`server`)
        // resolves to nothing the gate knows.
        let (_, cached) = run_post_alias(
            "S = server function onTick() while true do S.get(1) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_loop_passing_chased_alias_table_as_argument() {
        // Cycle 89: passing a host table through a chained alias
        // is still a barrier under the chase-aware argument rule.
        let (_, cached) = run_post_alias(
            "function onTick() S = screen local H = S while true do math.floor(H) screen.drawRectF(1,2,3,4) if true then break end end end",
        );
        assert_eq!(cached, 0);
    }
}
