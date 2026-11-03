//! Redundant output-store elimination.
//!
//! Within one block, `output.setNumber` / `output.setBool` calls with
//! literal channels and literal or proven-name values are dead when a later call in the
//! same block writes the same channel with the same kind: `output` has
//! no read API (the runner only exposes `setNumber` / `setBool`), the
//! host keeps the written values in Rust-side state, and the values are
//! only observed after the callback returns. A store is therefore
//! unobservable when another store to the same channel follows it
//! before anything else can observe or disturb the overwrite.
//!
//! This pass is intentionally conservative (v2 adds proven-name values):
//! - Each block is scanned backwards, keeping the last store per
//!   (kind, channel). A statement between two stores is harmless only
//!   when it is a pure local binding (literal or name right-hand sides,
//!   name targets only): it cannot observe output state, change it, or
//!   abort the block. Anything else (calls, index operations, control
//!   flow, labels, non-literal output stores, ...) is a barrier that
//!   clears the remembered stores, so control flow, errors, and
//!   intervening writes cannot change what the removal means.
//! - Direct, object-alias and callee-alias host calls count: the
//!   callee must resolve to `output.setNumber` / `output.setBool`
//!   through the effect analyzer or the alias proofs below, with no
//!   method call and exactly two arguments. If `output` (or any alias
//!   of it) is written or passed to unknown code anywhere in the
//!   program, resolution fails and the pass does nothing, so a script
//!   that replaces the host functions is never rewritten.
//! - Channels must be integer literals (parsed as `i64`, so host argument
//!   conversion is exact and cannot error). Values must be literals of
//!   the matching type (`Num` for `setNumber`, `Bool` for `setBool`), or
//!   names proven to hold such a value (v2, see below), so argument
//!   evaluation itself has no side effects and the removed call cannot
//!   be the one that errors on a wrong-typed value.
//! - v2 name-value proof: a name value is eligible only when the same
//!   block proves it, by scanning backwards from the store to the
//!   nearest binding of that name: a `local` binding or an assignment
//!   whose right-hand side is a literal of the matching type, or another
//!   name proven the same way (chains). Any statement between the
//!   binding and the store that is not a pure local binding or another
//!   output store breaks the proof (a call could mutate the name through
//!   a captured closure), as does a binding to a non-literal value.
//!   Names bound outside the block (parameters, globals, loop variables)
//!   are never proven. An unproven name store stays a barrier, exactly
//!   as in v1.
//! - v3 call-return proof: a name bound by a direct call whose return
//!   type is statically fixed also proves the type: host reads
//!   (`input.getNumber` / `property.getNumber` / `screen.getWidth` /
//!   `screen.getHeight` return numbers, `input.getBool` /
//!   `property.getBool` return booleans) and single-number `math.*`
//!   calls (`abs`, `acos`, `asin`, `atan`, `atan2`, `ceil`, `cos`, `cosh`,
//!   `deg`, `exp`, `floor`, `fmod`, `log`, `log10`, `max`, `min`, `pow`,
//!   `rad`, `random`, `sin`, `sinh`, `sqrt`, `tan`, `tanh`). The callee
//!   must resolve through the effect analyzer as a direct member call,
//!   and its root must not appear in the program-wide
//!   `mutated_builtin_roots` set, so a replaced host/math function can
//!   never be mistaken for the builtin. The binding call must also be
//!   total: its arguments must be statically error-free (integer
//!   literals for input channels, string literals for property labels,
//!   no arguments for screen size, and number literals or nested total
//!   calls for math arguments, with matching arity). Otherwise the
//!   binding call could be the one that errors, and removing the store
//!   could let a later dead-binding elimination drop that error.
//!   Alias-form calls (a name bound to a builtin, then called) are
//!   proven in v5 (see below).
//! - v4 call-return proof: `math.ult` (boolean, but only with integer
//!   arguments: non-integer arguments error, and nested calls are not
//!   tracked as integer-typed, so integer literals, optionally negated,
//!   are the only accepted form) and the two-value `math.modf` call
//!   (both results are numbers, so a binding position past the value
//!   list also proves a number when the call is the last, expanding
//!   value). `math.frexp` is deliberately excluded: it is absent from
//!   the Lua 5.3 math library, so a `math.frexp` call errors and can
//!   never be total. Number-or-nil calls (`math.tointeger`,
//!   `tonumber`) and string calls (`math.type`) prove nothing here and
//!   stay out of scope.
//! - v5 alias-form call proof: a name bound to a builtin in an earlier
//!   statement of the same block, then called, proves the same return
//!   type as a direct call (`local g = math.floor` then `g(2)` is a
//!   number, `local g = input.getNumber` then `g(1)` is a number).
//!   The binding must be a local or name-only assignment whose
//!   right-hand side is a builtin member access, a bare builtin root
//!   followed by member access (`local m = math` then `local g =
//!   m.floor`), or another alias name, and statements between the
//!   binding and the call must be output stores or alias-safe bindings
//!   (no calls, tables, functions, or index reads on non-builtin
//!   roots), so nothing between them can reassign the alias. The
//!   analyzer's whole-program alias table is not used for ordering:
//!   only a strictly-earlier same-block binding proves an alias call.
//!   Same-statement aliases (`local g, x = math.floor, g(2)`)
//!   stay out of scope.
//! - v6 alias-form store proof: a store callee through a name bound to
//!   `output` in an earlier statement of the same block (`local o =
//!   output` then `o.setNumber`, chains such as `local p = o` included)
//!   counts as an output store under the same rules as a direct store.
//!   The binding must resolve to the bare root `output` through the
//!   same strictly-earlier same-block scan as in v5: the analyzer's
//!   whole-program alias table is not trusted for ordering, because a
//!   later assignment to the alias must never prove an earlier store
//!   (for example a store through a still-nil alias). The `output` root
//!   must not be mutated anywhere in the program; the pass already
//!   bails out entirely in that case, and the alias branch checks it
//!   again. Aliases bound outside the block other than proven
//!   outer-block aliases (v7, below) are not proven, and a store through
//!   an alias bound in the
//!   same statement as its use stays out of scope. Aliased stores are
//!   transparent inside the proof scans just like direct stores (the
//!   pass bails out entirely when `output` is mutated, so no separate
//!   mutated check is needed there).
//! - v7 outer-block alias proof: a store callee through a name bound to
//!   `output` in an outer block (a chunk local captured by a callback,
//!   the corpus's dominant alias shape) also counts as an output store.
//!   The proof is binding-based, not statement-order-based: the callee
//!   base name must resolve to a `local` binding whose right-hand side
//!   is the bare `output` root or another proven alias name, and that
//!   binding must have zero writes anywhere in the program
//!   (`binding_write_counts == 0`), so it can never be reassigned after
//!   its definition, including through a captured closure. Lexical
//!   scoping supplies the ordering: a use resolving to the binding can
//!   only sit in a function defined after the binding statement, and
//!   blocks execute in order, so the binding has run before any call
//!   of that function; a same-statement alias cannot resolve to the new
//!   binding (a local's scope starts after its statement). Assign-form
//!   bindings are never proven here (the assignment itself is a write).
//!   Stores in different callbacks are still independent: each block
//!   is scanned on its own, so one store per callback is never removed.
//! - v8 callee-alias store proof: a call through a name bound to
//!   `output.setNumber` / `output.setBool` (`local a = output.setNumber`
//!   then `a(1,5)`, chains such as `local b = a`, and `local o = output`
//!   plus `local a = o.setNumber` included) counts as an output store
//!   under the same rules as a direct store. Same-block bindings
//!   resolve through the strictly-earlier scan as in v5/v6; outer-block
//!   bindings (a chunk local captured by a callback) prove through
//!   zero writes as in v7. Method calls never count (the receiver
//!   shifts the argument positions). Callee-alias stores are
//!   transparent inside the same-block proof scans just like direct
//!   and object-alias stores, so a first callee-alias store never
//!   breaks the alias proof of a later one; outer callee-alias stores
//!   stay non-transparent in proof scans (the v7 conservative choice).
//!   A callee-alias binding statement itself reads a host table field,
//!   so it is pure for run purposes (see `is_pure_non_store`) and does
//!   not break a run the way an arbitrary call does.
//! - v9 outer-block alias-form call proof: a name bound to a builtin
//!   in an outer block (a chunk local captured by a callback, the
//!   same shape as the v7 store-callee proof) also proves the return
//!   type of a call through it, when the binding has zero writes
//!   anywhere in the program. The proof is binding-based, not
//!   statement-order-based: the callee name resolves through the
//!   resolver to the outer binding, and lexical scoping plus zero
//!   writes supply the ordering (a use can only sit in a function
//!   defined after the binding statement, and the binding can never
//!   be reassigned, including through a captured closure). Bindings
//!   defined in the same block as the use are excluded from this map:
//!   they stay under the v5 scan, so an intervening call still blocks
//!   them. The root
//!   must still be unmutated program-wide (the total-call check),
//!   and the call arguments must still be total. Same-block aliases
//!   keep precedence: the v5 scan runs first and the outer map is
//!   only a fallback.
//! - The runner ignores out-of-range channels and non-tick phases
//!   without error, so a removed store can never be the one that errors.
//! - The pass only removes statements; it never introduces names, so no
//!   scope renaming is needed afterwards.
//!
//! Unlike `final_stores` (overwritten variable assignments) this pass
//! targets host calls, which that pass does not handle; unlike
//! `output_loops` (loop synthesis for long output sequences) it removes
//! stores instead of restructuring them. It runs before loop synthesis in
//! the phase pipeline so dead stores are not synthesized into loops.

use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use storm_lua_analysis::effects::{mutated_builtin_roots, EffectAnalyzer};
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId};
use storm_lua_syntax::ast_utils;

fn clone_source(source: &Ast) -> Ast {
    let mut ast = ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

/// `output.setNumber` / `output.setBool` builtin of a call statement, as
/// `is_bool`, or `None` when the statement is not such a direct host call
/// with exactly two arguments. This direct-only form is the first
/// check in `statement_is_output_store`; the proof scans use that
/// helper, so aliased stores are transparent there too (see v6).
fn output_store_builtin(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    statement: NodeId,
) -> Option<bool> {
    let Node::Callstat(call) = source.node(statement) else {
        return None;
    };
    let Node::Call(callee, args, method) = source.node(*call) else {
        return None;
    };
    if method.is_some() || args.len() != 2 {
        return None;
    }
    match analyzer.resolve_builtin_reference(*callee)?.as_str() {
        "output.setNumber" => Some(false),
        "output.setBool" => Some(true),
        _ => None,
    }
}

/// `output.setNumber` / `output.setBool` builtin of the call statement at
/// `statements[index]`, as `is_bool`, or `None` when it is not such a
/// host call with exactly two arguments. Direct callees resolve through
/// the effect analyzer as in `output_store_builtin`; object-alias
/// callees (`local o = output` then `o.setNumber`) resolve only through
/// a strictly-earlier same-block binding whose right-hand side resolves
/// to the bare root `output` (see `find_alias_binding` /
/// `resolve_alias_expr`). Callee-alias callees (`local a =
/// output.setNumber` then `a(1,5)`) resolve through
/// `callee_alias_store_same_block` plus the v8 outer set (see below).
/// The `output` root must not be mutated anywhere in the program.
/// Method calls never resolve. As a fallback, an outer-block alias
/// proven by `outer_output_aliases` (v7) is accepted when the
/// same-block scan finds nothing.
fn output_store_builtin_at(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    ctx: &StoreCtx<'_>,
    statements: &[NodeId],
    index: usize,
) -> Option<bool> {
    let statement = statements[index];
    let Node::Callstat(call) = source.node(statement) else {
        return None;
    };
    let Node::Call(callee, args, method) = source.node(*call) else {
        return None;
    };
    if method.is_some() || args.len() != 2 {
        return None;
    }
    match source.node(*callee) {
        Node::Index(obj, key, _) => {
            let Node::Str(raw) = source.node(*key) else {
                return None;
            };
            let member = raw
                .strip_prefix('"')
                .and_then(|text| text.strip_suffix('"'))
                .unwrap_or(raw);
            let is_bool = match member {
                "setNumber" => false,
                "setBool" => true,
                _ => return None,
            };
            let Node::Name(base) = source.node(*obj) else {
                return None;
            };
            if source.strings.get(*base) == "output" {
                return match analyzer.resolve_builtin_reference(*callee)?.as_str() {
                    "output.setNumber" => Some(false),
                    "output.setBool" => Some(true),
                    _ => None,
                };
            }
            if ctx.mutated.contains("output") {
                return None;
            }
            if let Some((def_index, rhs)) =
                find_alias_binding(source, analyzer, ctx, statements, index, *base)
            {
                if resolve_alias_expr(source, analyzer, ctx, statements, def_index, rhs).as_deref()
                    == Some("output")
                {
                    return Some(is_bool);
                }
            }
            // v7: the callee base name resolves to an outer-block alias binding
            // proven to hold the bare `output` table (see `outer_output_aliases`).
            let base_bid = ctx.resolution.node_bid[*obj as usize]?;
            if ctx.outer.contains(&base_bid) {
                return Some(is_bool);
            }
            None
        }
        Node::Name(base) => {
            if let Some(is_bool) =
                callee_alias_store_same_block(source, analyzer, ctx, statements, index, *base)
            {
                return Some(is_bool);
            }
            // v8: the callee name resolves to an outer-block alias binding
            // proven to hold the `output.setNumber` / `output.setBool`
            // function (see `outer_callee_aliases`).
            let base_bid = ctx.resolution.node_bid[*callee as usize]?;
            ctx.outer_callee.get(&base_bid).copied()
        }
        _ => None,
    }
}

/// `is_bool` for a call through a callee alias name bound in an
/// earlier statement of the same block (v8): the binding's right-hand
/// side must resolve to `output.setNumber` / `output.setBool` (a direct
/// member access, a chain through another alias, or a member access
/// through an object alias). The outer-block proof is deliberately not
/// part of this helper: proof scans use it for transparency, and outer
/// aliases there stay conservative (the v7 choice for object aliases),
/// while `output_store_builtin_at` adds the outer set on top.
fn callee_alias_store_same_block(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    ctx: &StoreCtx<'_>,
    statements: &[NodeId],
    index: usize,
    name: SymbolId,
) -> Option<bool> {
    if ctx.mutated.contains("output") {
        return None;
    }
    let (def_index, rhs) = find_alias_binding(source, analyzer, ctx, statements, index, name)?;
    match resolve_alias_expr(source, analyzer, ctx, statements, def_index, rhs).as_deref() {
        Some("output.setNumber") => Some(false),
        Some("output.setBool") => Some(true),
        _ => None,
    }
}

/// Bindings proven to hold the bare `output` table from an outer block
/// (v7). A `local` binding qualifies when its right-hand side is the
/// bare `output` root or another proven alias name, and it has zero
/// writes anywhere in the program (`binding_write_counts == 0`), so it
/// can never be reassigned after its definition, including through a
/// captured closure. Lexical scoping plus the resolver's binding id
/// gives the ordering proof: a use resolving to the binding can only
/// sit in a function defined after the binding statement, and blocks
/// execute in order, so the binding has run before any call of that
/// function. Same-statement aliases cannot resolve to the new binding
/// (a local's scope starts after its statement). Only `local` bindings
/// are collected: an `Assign` to a name is itself a write and can never
/// satisfy the zero-write condition. The pass-level bail-out on a
/// mutated `output` root already applies before this set is used.
fn outer_output_aliases(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    resolution: &Resolution,
    root: NodeId,
) -> HashSet<BindingId> {
    let mut pairs: Vec<(BindingId, NodeId)> = Vec::new();
    ast_utils::walk(source, root, &mut |node| {
        if let Node::Local(_, values) = source.node(node) {
            if let Some(bids) = resolution.node_bids.get(node as usize) {
                for (bid, value) in bids.iter().zip(values.iter()) {
                    if resolution.binding_write_counts[*bid as usize] == 0 {
                        pairs.push((*bid, *value));
                    }
                }
            }
        }
    });
    let mut proven: HashSet<BindingId> = HashSet::new();
    loop {
        let mut grown = false;
        for (bid, rhs) in &pairs {
            if proven.contains(bid) {
                continue;
            }
            let rhs = unwrap_paren(source, *rhs);
            if let Node::Name(_) = source.node(rhs) {
                if analyzer.resolve_builtin_reference(rhs).as_deref() == Some("output")
                    || resolution.node_bid[rhs as usize]
                        .is_some_and(|rhs_bid| proven.contains(&rhs_bid))
                {
                    proven.insert(*bid);
                    grown = true;
                }
            }
        }
        if !grown {
            break;
        }
    }
    proven
}

/// Shared proof context for output-store recognition: the resolver
/// output, the v7 outer output aliases, the v8 outer callee aliases,
/// and the program-wide mutated builtin roots. Threading one struct
/// keeps callee-alias stores transparent inside every proof scan (a
/// callee-alias store between a binding and its use must not break
/// the scan, exactly as direct and object-alias stores do not),
/// without re-resolving at each step.
struct StoreCtx<'a> {
    resolution: &'a Resolution,
    outer: &'a HashSet<BindingId>,
    outer_callee: &'a HashMap<BindingId, bool>,
    outer_value: &'a HashMap<BindingId, String>,
    mutated: &'a HashSet<String>,
}

/// Bindings proven to hold an `output.setNumber` / `output.setBool`
/// function from an outer block (v8), as `is_bool`. A `local` binding
/// qualifies when its right-hand side is a direct member access
/// resolving to `output.setNumber` / `output.setBool`, a member access
/// through a proven outer output alias (`local o = output` then
/// `local a = o.setNumber`), or another proven callee-alias name, and
/// it has zero writes anywhere in the program
/// (`binding_write_counts == 0`), so it can never be reassigned after
/// its definition, including through a captured closure. The ordering
/// argument is the v7 one: a use resolving to the binding can only sit
/// in a function defined after the binding statement, and blocks
/// execute in order, so the binding has run before any call of that
/// function; a same-statement alias cannot resolve to the new binding.
/// Only `local` bindings are collected: an `Assign` to a name is itself
/// a write and can never satisfy the zero-write condition. The
/// pass-level bail-out on a mutated `output` root already applies
/// before this set is used.
fn outer_callee_aliases(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    resolution: &Resolution,
    root: NodeId,
    outer: &HashSet<BindingId>,
) -> HashMap<BindingId, bool> {
    let mut pairs: Vec<(BindingId, NodeId)> = Vec::new();
    ast_utils::walk(source, root, &mut |node| {
        if let Node::Local(_, values) = source.node(node) {
            if let Some(bids) = resolution.node_bids.get(node as usize) {
                for (bid, value) in bids.iter().zip(values.iter()) {
                    if resolution.binding_write_counts[*bid as usize] == 0 {
                        pairs.push((*bid, *value));
                    }
                }
            }
        }
    });
    let mut proven: HashMap<BindingId, bool> = HashMap::new();
    loop {
        let mut grown = false;
        for (bid, rhs) in &pairs {
            if proven.contains_key(bid) {
                continue;
            }
            let rhs = unwrap_paren(source, *rhs);
            match source.node(rhs) {
                Node::Index(obj, key, _) => {
                    if let Some(builtin) = analyzer.resolve_builtin_reference(rhs) {
                        match builtin.as_str() {
                            "output.setNumber" => {
                                proven.insert(*bid, false);
                                grown = true;
                            }
                            "output.setBool" => {
                                proven.insert(*bid, true);
                                grown = true;
                            }
                            _ => {}
                        }
                        continue;
                    }
                    let Node::Name(_) = source.node(*obj) else {
                        continue;
                    };
                    let Node::Str(raw) = source.node(*key) else {
                        continue;
                    };
                    let member = raw
                        .strip_prefix('"')
                        .and_then(|text| text.strip_suffix('"'))
                        .unwrap_or(raw);
                    let is_bool = match member {
                        "setNumber" => false,
                        "setBool" => true,
                        _ => continue,
                    };
                    if resolution.node_bid[*obj as usize]
                        .is_some_and(|base_bid| outer.contains(&base_bid))
                    {
                        proven.insert(*bid, is_bool);
                        grown = true;
                    }
                }
                Node::Name(_) => {
                    if let Some(rhs_bid) = resolution.node_bid[rhs as usize] {
                        if let Some(is_bool) = proven.get(&rhs_bid).copied() {
                            proven.insert(*bid, is_bool);
                            grown = true;
                        }
                    }
                }
                _ => {}
            }
        }
        if !grown {
            break;
        }
    }
    proven
}

/// Bindings proven to hold a builtin value from an outer block (v9),
/// as the builtin string (`input.getNumber`, `math.floor`, or a bare
/// root such as `math` for a root-alias chain). A `local` binding
/// qualifies when it has zero writes anywhere in the program
/// (`binding_write_counts == 0`), so it can never be reassigned
/// after its definition, including through a captured closure; the
/// ordering argument is the v7 one (lexical scoping: a use resolving
/// to the binding can only sit in a function defined after the
/// binding statement). Only the value-proof consumers use this map,
/// and they match only known (root, member) pairs, so recording
/// other builtins here proves nothing on its own. Only `local`
/// bindings are collected: an `Assign` to a name is itself a write
/// and can never satisfy the zero-write condition.
fn outer_value_aliases(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    resolution: &Resolution,
    root: NodeId,
) -> HashMap<BindingId, String> {
    let mut pairs: Vec<(BindingId, NodeId)> = Vec::new();
    ast_utils::walk(source, root, &mut |node| {
        if let Node::Local(_, values) = source.node(node) {
            if let Some(bids) = resolution.node_bids.get(node as usize) {
                for (bid, value) in bids.iter().zip(values.iter()) {
                    if resolution.binding_write_counts[*bid as usize] == 0 {
                        pairs.push((*bid, *value));
                    }
                }
            }
        }
    });
    let mut proven: HashMap<BindingId, String> = HashMap::new();
    loop {
        let mut grown = false;
        for (bid, rhs) in &pairs {
            if proven.contains_key(bid) {
                continue;
            }
            let rhs = unwrap_paren(source, *rhs);
            match source.node(rhs) {
                Node::Index(obj, key, _) => {
                    if let Some(builtin) = analyzer.resolve_builtin_reference(rhs) {
                        proven.insert(*bid, builtin);
                        grown = true;
                        continue;
                    }
                    let Node::Name(_) = source.node(*obj) else {
                        continue;
                    };
                    let Node::Str(raw) = source.node(*key) else {
                        continue;
                    };
                    let member = raw
                        .strip_prefix('"')
                        .and_then(|text| text.strip_suffix('"'))
                        .unwrap_or(raw);
                    if let Some(base) = resolution.node_bid[*obj as usize]
                        .and_then(|base_bid| proven.get(&base_bid))
                    {
                        if !base.contains('.') {
                            proven.insert(*bid, format!("{base}.{member}"));
                            grown = true;
                        }
                    }
                }
                Node::Name(_) => {
                    if let Some(builtin) = analyzer.resolve_builtin_reference(rhs) {
                        if !builtin.contains('.') {
                            proven.insert(*bid, builtin);
                            grown = true;
                            continue;
                        }
                    }
                    if let Some(rhs_bid) = resolution.node_bid[rhs as usize] {
                        if let Some(builtin) = proven.get(&rhs_bid).cloned() {
                            proven.insert(*bid, builtin);
                            grown = true;
                        }
                    }
                }
                _ => {}
            }
        }
        if !grown {
            break;
        }
    }
    proven
}

/// True when the statement at `statements[index]` is an `output.setNumber`
/// / `output.setBool` call, direct or alias-form (v6, v8 adds the
/// callee-alias form). Used to keep output stores transparent inside
/// the alias-proof scans. This runs only after the pass-level bail-out
/// on a mutated `output` root, so no separate mutated check is needed
/// here. Recursion terminates: the alias branch scans strictly earlier
/// statements only. Outer aliases stay non-transparent here (the v7
/// conservative choice); only same-block callee aliases are
/// transparent, which is what the in-block chain needs.
fn statement_is_output_store(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    ctx: &StoreCtx<'_>,
    statements: &[NodeId],
    index: usize,
) -> bool {
    let statement = statements[index];
    if output_store_builtin(source, analyzer, statement).is_some() {
        return true;
    }
    let Node::Callstat(call) = source.node(statement) else {
        return false;
    };
    let Node::Call(callee, args, method) = source.node(*call) else {
        return false;
    };
    if method.is_some() || args.len() != 2 {
        return false;
    }
    match source.node(*callee) {
        Node::Index(obj, key, _) => {
            let Node::Str(raw) = source.node(*key) else {
                return false;
            };
            let member = raw
                .strip_prefix('"')
                .and_then(|text| text.strip_suffix('"'))
                .unwrap_or(raw);
            if member != "setNumber" && member != "setBool" {
                return false;
            }
            let Node::Name(base) = source.node(*obj) else {
                return false;
            };
            if source.strings.get(*base) == "output" {
                return false;
            }
            let Some((def_index, rhs)) =
                find_alias_binding(source, analyzer, ctx, statements, index, *base)
            else {
                return false;
            };
            resolve_alias_expr(source, analyzer, ctx, statements, def_index, rhs).as_deref()
                == Some("output")
        }
        Node::Name(base) => {
            callee_alias_store_same_block(source, analyzer, ctx, statements, index, *base).is_some()
        }
        _ => false,
    }
}

/// (is_bool, channel) key for an output-store statement, or `None` when
/// the statement is not an eligible `output.setNumber` / `output.setBool`
/// call: a literal channel and a literal value of the matching type, or
/// a name value proven by `prove_name_type` to hold such a value at this
/// point in the block.
fn store_key(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    ctx: &StoreCtx<'_>,
    statements: &[NodeId],
    index: usize,
) -> Option<(bool, i64)> {
    let statement = statements[index];
    let is_bool = output_store_builtin_at(source, analyzer, ctx, statements, index)?;
    let Node::Callstat(call) = source.node(statement) else {
        return None;
    };
    let Node::Call(_, args, _) = source.node(*call) else {
        return None;
    };
    let Node::Num(channel_text) = source.node(args[0]) else {
        return None;
    };
    let channel: i64 = channel_text.parse().ok()?;
    match (is_bool, source.node(args[1])) {
        (false, Node::Num(_)) => Some((false, channel)),
        (true, Node::Bool(_)) => Some((true, channel)),
        (false, Node::Name(name)) | (true, Node::Name(name)) => {
            if prove_name_type(source, analyzer, ctx, statements, index, *name, is_bool) {
                Some((is_bool, channel))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Unwrap parentheses around an expression node.
fn unwrap_paren(source: &Ast, mut node: NodeId) -> NodeId {
    while let Node::Paren(inner) = source.node(node) {
        node = *inner;
    }
    node
}

/// True when `node` is an integer literal (possibly parenthesized).
fn is_int_literal(source: &Ast, node: NodeId) -> bool {
    let node = unwrap_paren(source, node);
    match source.node(node) {
        Node::Num(text) => text.parse::<i64>().is_ok(),
        _ => false,
    }
}

/// True when `node` is a string literal (possibly parenthesized).
fn is_str_literal(source: &Ast, node: NodeId) -> bool {
    let node = unwrap_paren(source, node);
    matches!(source.node(node), Node::Str(_))
}

/// True when `node` is a statically known integer: an integer literal,
/// possibly parenthesized or negated. Float literals (even integral
/// ones such as `3.0`) are excluded because `math.ult` rejects
/// non-integer arguments and the literal spelling alone does not
/// settle the conversion.
fn is_int_value(source: &Ast, node: NodeId) -> bool {
    let node = unwrap_paren(source, node);
    match source.node(node) {
        Node::Num(text) => text.parse::<i64>().is_ok(),
        Node::Un(op, operand) => op.as_str() == "-" && is_int_literal(source, *operand),
        _ => false,
    }
}

/// True when `node` is a total two-value number call (`math.modf`):
/// exactly one statically error-free number argument, and the `math`
/// root not mutated anywhere in the program. Both results of `modf`
/// are numbers, so a binding position past the value list (the call
/// expands when it is the last value) is also number-typed.
fn is_multi_number_call(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    ctx: &StoreCtx<'_>,
    node: NodeId,
    alias: AliasCtx<'_>,
) -> bool {
    let Node::Call(_, args, _) = source.node(node) else {
        return false;
    };
    if args.len() != 1 {
        return false;
    }
    if call_builtin(source, analyzer, ctx, node, alias).as_deref() != Some("math.modf") {
        return false;
    }
    if ctx.mutated.contains("math") {
        return false;
    }
    is_total_number(source, analyzer, ctx, args[0], alias)
}

/// Alias context for call proofs: the block's statements and the index
/// of the statement holding the call being proven. Alias bindings must
/// sit strictly before that statement in the same block.
type AliasCtx<'a> = (&'a [NodeId], usize);

/// Resolve a call's callee to a `root.member` builtin string: a direct
/// member call through the effect analyzer, or an alias-form call
/// (`local g = math.floor` then `g(...)`) through the same-block alias
/// proof in `resolve_alias_builtin`. Method calls never resolve.
fn call_builtin(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    ctx: &StoreCtx<'_>,
    call: NodeId,
    alias: AliasCtx<'_>,
) -> Option<String> {
    let Node::Call(callee, _, method) = source.node(call) else {
        return None;
    };
    if method.is_some() {
        return None;
    }
    match source.node(*callee) {
        Node::Index(..) => analyzer.resolve_builtin_reference(*callee),
        Node::Name(name) => resolve_alias_builtin(source, analyzer, ctx, alias.0, alias.1, *name)
            .or_else(|| {
                let bid = ctx.resolution.node_bid[*callee as usize]?;
                // Same-block bindings stay under the v5 scan: an
                // intervening call blocks them even when no write
                // to the alias exists anywhere, so the outer map
                // must not rescue a blocked same-block alias. Only
                // bindings defined outside this block's statement
                // list use the outer map.
                let same_block = alias.0.iter().any(|stmt| {
                    ctx.resolution
                        .node_bids
                        .get(*stmt as usize)
                        .is_some_and(|bids| bids.contains(&bid))
                });
                if same_block {
                    return None;
                }
                let builtin = ctx.outer_value.get(&bid)?;
                if builtin.contains('.') {
                    Some(builtin.clone())
                } else {
                    None
                }
            }),
        _ => None,
    }
}

/// Resolve `name` to a `root.member` builtin string through a same-block
/// alias binding strictly before `bound` (see `find_alias_binding`).
/// The analyzer's own whole-program alias table is deliberately not
/// trusted here: it does not prove the binding precedes the call, and
/// a later binding must never prove an earlier call.
fn resolve_alias_builtin(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    ctx: &StoreCtx<'_>,
    statements: &[NodeId],
    bound: usize,
    name: SymbolId,
) -> Option<String> {
    let (def_index, rhs) = find_alias_binding(source, analyzer, ctx, statements, bound, name)?;
    let builtin = resolve_alias_expr(source, analyzer, ctx, statements, def_index, rhs)?;
    if builtin.contains('.') {
        Some(builtin)
    } else {
        None
    }
}

/// Resolve an alias right-hand side to a builtin string: a bare global
/// root (`math`) through the analyzer, a member access whose object
/// resolves the same way (`m.floor` with `local m = math`), or another
/// name followed through its own earlier binding. Anything else
/// (calls, tables, literals) is not a builtin alias.
fn resolve_alias_expr(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    ctx: &StoreCtx<'_>,
    statements: &[NodeId],
    bound: usize,
    node: NodeId,
) -> Option<String> {
    let node = unwrap_paren(source, node);
    match source.node(node) {
        Node::Name(symbol) => {
            if let Some(builtin) = analyzer.resolve_builtin_reference(node) {
                if !builtin.contains('.') {
                    return Some(builtin);
                }
                // A proven member alias from the analyzer's table carries
                // no ordering proof, so fall through to the block scan.
            }
            let (def_index, rhs) =
                find_alias_binding(source, analyzer, ctx, statements, bound, *symbol)?;
            resolve_alias_expr(source, analyzer, ctx, statements, def_index, rhs)
        }
        Node::Index(obj, key, _) => {
            if let Some(builtin) = analyzer.resolve_builtin_reference(node) {
                return Some(builtin);
            }
            let Node::Str(raw) = source.node(*key) else {
                return None;
            };
            let base = resolve_alias_expr(source, analyzer, ctx, statements, bound, *obj)?;
            if base.contains('.') {
                return None;
            }
            let member = raw
                .strip_prefix('"')
                .and_then(|text| text.strip_suffix('"'))
                .unwrap_or(raw);
            Some(format!("{base}.{member}"))
        }
        _ => None,
    }
}

/// Find the nearest binding of `name` in `statements[..bound]`, as
/// (statement index, right-hand-side node). Statements between the
/// binding and `bound` must be output stores or alias-safe bindings
/// (see `is_alias_safe_intervening`); anything else breaks the proof.
/// A binding whose value is not a single expression (a multi-value
/// expansion position) proves nothing and ends the scan: an earlier
/// binding of the same name is shadowed by it.
fn find_alias_binding(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    ctx: &StoreCtx<'_>,
    statements: &[NodeId],
    bound: usize,
    name: SymbolId,
) -> Option<(usize, NodeId)> {
    let mut index = bound;
    while index > 0 {
        index -= 1;
        let statement = statements[index];
        match source.node(statement) {
            Node::Local(targets, values) => {
                if let Some(pos) = targets.iter().position(|target| *target == name) {
                    return Some((index, values.get(pos).copied()?));
                }
                if !is_alias_safe_intervening(source, analyzer, ctx, statements, index, statement) {
                    return None;
                }
            }
            Node::Assign(targets, values) => {
                let mut pos = None;
                for (i, target) in targets.iter().enumerate() {
                    if matches!(source.node(*target), Node::Name(symbol) if *symbol == name) {
                        pos = Some(i);
                        break;
                    }
                }
                if let Some(pos) = pos {
                    return Some((index, values.get(pos).copied()?));
                }
                if !is_alias_safe_intervening(source, analyzer, ctx, statements, index, statement) {
                    return None;
                }
            }
            _ => {
                if statement_is_output_store(source, analyzer, ctx, statements, index) {
                    continue;
                }
                if is_alias_safe_intervening(source, analyzer, ctx, statements, index, statement) {
                    continue;
                }
                return None;
            }
        }
    }
    None
}

/// True when a statement between an alias binding and its use cannot
/// change the bound value: a local binding or a name-only assignment
/// whose values mention only names, member accesses on builtin roots,
/// parentheses and literals. No calls, tables, functions, binops or
/// control flow: nothing here can run code that reassigns the alias
/// through a captured closure. An index read on any other root breaks
/// the proof, since only host tables are known to be plain.
fn is_alias_safe_intervening(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    ctx: &StoreCtx<'_>,
    statements: &[NodeId],
    bound: usize,
    statement: NodeId,
) -> bool {
    let mut safe = true;
    ast_utils::walk(source, statement, &mut |node| {
        if !safe {
            return;
        }
        match source.node(node) {
            Node::Local(..)
            | Node::Paren(_)
            | Node::Name(_)
            | Node::Num(_)
            | Node::Str(_)
            | Node::Bool(_)
            | Node::Nil => {}
            Node::Assign(targets, _) => {
                if !targets
                    .iter()
                    .all(|target| matches!(source.node(*target), Node::Name(_)))
                {
                    safe = false;
                }
            }
            Node::Index(obj, _, _) => {
                if !index_base_is_builtin_root(source, analyzer, ctx, statements, bound, *obj) {
                    safe = false;
                }
            }
            _ => safe = false,
        }
    });
    safe
}

/// True when `node` reads through to a bare builtin root (`math`,
/// `input`, ...): the base name resolves through the analyzer or
/// through an earlier alias chain to a root-only string. Indexing
/// further into a member value (`math.floor.x`) is not a root read.
fn index_base_is_builtin_root(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    ctx: &StoreCtx<'_>,
    statements: &[NodeId],
    bound: usize,
    node: NodeId,
) -> bool {
    let node = unwrap_paren(source, node);
    match source.node(node) {
        Node::Name(_) => {
            matches!(
                resolve_alias_expr(source, analyzer, ctx, statements, bound, node),
                Some(builtin) if !builtin.contains('.')
            )
        }
        Node::Index(obj, _, _) => {
            index_base_is_builtin_root(source, analyzer, ctx, statements, bound, *obj)
        }
        _ => false,
    }
}

/// Return type of a total builtin call, as `is_bool`, or `None` when
/// the call is not a known total call: the callee must resolve as a
/// direct member call, a same-block alias-form call (see the v5
/// documentation above), or an outer-block alias-form call (v9),
/// its root must not be mutated anywhere in the program, and
/// its arguments must be statically error-free. Arity is checked per
/// function because a missing argument is an error even for a pure
/// function.
fn total_call_is_bool(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    ctx: &StoreCtx<'_>,
    call: NodeId,
    alias: AliasCtx<'_>,
) -> Option<bool> {
    let Node::Call(_, args, _) = source.node(call) else {
        return None;
    };
    let builtin = call_builtin(source, analyzer, ctx, call, alias)?;
    let (root, member) = builtin.split_once('.')?;
    if ctx.mutated.contains(root) {
        return None;
    }
    let all_numbers = || {
        args.iter()
            .all(|arg| is_total_number(source, analyzer, ctx, *arg, alias))
    };
    match (root, member) {
        ("input", "getNumber") if args.len() == 1 && is_int_literal(source, args[0]) => Some(false),
        ("input", "getBool") if args.len() == 1 && is_int_literal(source, args[0]) => Some(true),
        ("property", "getNumber") if args.len() == 1 && is_str_literal(source, args[0]) => {
            Some(false)
        }
        ("property", "getBool") if args.len() == 1 && is_str_literal(source, args[0]) => Some(true),
        ("screen", "getWidth" | "getHeight") if args.is_empty() => Some(false),
        ("math", "log") if (1..=2).contains(&args.len()) && all_numbers() => Some(false),
        ("math", "atan2" | "fmod" | "pow") if args.len() == 2 && all_numbers() => Some(false),
        ("math", "max" | "min") if !args.is_empty() && all_numbers() => Some(false),
        ("math", "random")
            if args.is_empty() || args.iter().all(|arg| is_int_literal(source, *arg)) =>
        {
            Some(false)
        }
        (
            "math",
            "abs" | "acos" | "asin" | "atan" | "ceil" | "cos" | "cosh" | "deg" | "exp" | "floor"
            | "log10" | "rad" | "sin" | "sinh" | "sqrt" | "tan" | "tanh",
        ) if args.len() == 1 && all_numbers() => Some(false),
        ("math", "modf") if args.len() == 1 && all_numbers() => Some(false),
        ("math", "ult") if args.len() == 2 && args.iter().all(|arg| is_int_value(source, *arg)) => {
            Some(true)
        }
        _ => None,
    }
}

/// True when `node` is a statically error-free number-valued expression:
/// a number literal, a negated total number, or a total call returning
/// a number. Names are never total here: a name could hold a value of
/// any type at run time, and a wrong-typed argument would make the
/// enclosing call error.
fn is_total_number(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    ctx: &StoreCtx<'_>,
    node: NodeId,
    alias: AliasCtx<'_>,
) -> bool {
    let node = unwrap_paren(source, node);
    match source.node(node) {
        Node::Num(_) => true,
        Node::Un(op, operand) => {
            op.as_str() == "-" && is_total_number(source, analyzer, ctx, *operand, alias)
        }
        Node::Call(..) => total_call_is_bool(source, analyzer, ctx, node, alias) == Some(false),
        _ => false,
    }
}

/// True when the right-hand-side expression at `values[pos]` proves a
/// value of the type `is_bool` expects: a matching literal, a
/// parenthesized or sign-flipped form of one, or another name proven by
/// scanning backwards from the defining statement at `def_index`.
#[allow(clippy::too_many_arguments)]
fn prove_rhs(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    ctx: &StoreCtx<'_>,
    statements: &[NodeId],
    def_index: usize,
    values: &[NodeId],
    pos: usize,
    is_bool: bool,
) -> bool {
    if pos >= values.len() {
        // Multi-value expansion: when the last value is a total
        // `math.modf` call, binding positions past the value list
        // receive the call's further number results.
        if let Some(last) = values.last() {
            if is_multi_number_call(source, analyzer, ctx, *last, (statements, def_index)) {
                return !is_bool;
            }
        }
        return false;
    }
    let node = unwrap_paren(source, values[pos]);
    match source.node(node) {
        Node::Num(_) => !is_bool,
        Node::Bool(_) => is_bool,
        Node::Un(op, operand) => {
            let operand = unwrap_paren(source, *operand);
            match (op.as_str(), source.node(operand)) {
                ("-", Node::Num(_)) => !is_bool,
                ("not", _) => is_bool,
                _ => false,
            }
        }
        Node::Name(other) => prove_name_type(
            source, analyzer, ctx, statements, def_index, *other, is_bool,
        ),
        Node::Call(..) => {
            total_call_is_bool(source, analyzer, ctx, node, (statements, def_index))
                == Some(is_bool)
        }
        _ => false,
    }
}

/// True when `name` provably holds a value of the expected type at the
/// statement just before `bound` in `statements` (same block only).
/// Scans backwards to the nearest binding of `name`: a `local` binding
/// or an assignment whose right-hand side proves the type (see
/// `prove_rhs`). Statements between the binding and the store must be
/// pure local bindings or other output stores: any other statement (a
/// call, an index write, control flow, ...) could change the name's
/// value through a captured closure or an alias, so it breaks the proof.
fn prove_name_type(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    ctx: &StoreCtx<'_>,
    statements: &[NodeId],
    bound: usize,
    name: SymbolId,
    is_bool: bool,
) -> bool {
    let mut index = bound;
    while index > 0 {
        index -= 1;
        let statement = statements[index];
        match source.node(statement) {
            Node::Local(targets, values) => {
                if let Some(pos) = targets.iter().position(|target| *target == name) {
                    return prove_rhs(
                        source, analyzer, ctx, statements, index, values, pos, is_bool,
                    );
                }
                if !is_pure_non_store(source, analyzer, ctx, statements, index, statement) {
                    return false;
                }
            }
            Node::Assign(targets, values) => {
                let mut pos = None;
                for (i, target) in targets.iter().enumerate() {
                    if matches!(source.node(*target), Node::Name(symbol) if *symbol == name) {
                        pos = Some(i);
                        break;
                    }
                }
                if let Some(pos) = pos {
                    return prove_rhs(
                        source, analyzer, ctx, statements, index, values, pos, is_bool,
                    );
                }
                if !is_pure_non_store(source, analyzer, ctx, statements, index, statement) {
                    return false;
                }
            }
            _ => {
                if statement_is_output_store(source, analyzer, ctx, statements, index) {
                    continue;
                }
                if is_pure_non_store(source, analyzer, ctx, statements, index, statement) {
                    continue;
                }
                return false;
            }
        }
    }
    false
}

/// True when the program textually contains a direct or alias-form
/// `output.setNumber` / `output.setBool` call with a literal channel and
/// a matching literal value or a name value. This cheap syntactic
/// pre-filter lets the pass skip the resolver entirely on inputs where
/// it cannot fire; alias bases are recognised through a binding closure
/// over `output` (chains included). Callee-alias calls (`local a =
/// output.setNumber` then `a(1,5)`) are recognised through a
/// member-binding closure over `output` (v8). Resolution in `store_key`
/// still confirms every candidate, so shadowing or reassignment cannot
/// fool the filter.
fn has_syntactic_candidate(source: &Ast, root: NodeId) -> bool {
    // (target, value-name) pairs from `local` / assignment bindings, and
    // candidate store calls (base symbol, member name, channel node,
    // value node), collected in one walk. Alias names are closed over
    // `output` afterwards: a name bound to `output`, or to another alias
    // name, counts as an alias base. Resolution in `store_key` still
    // confirms every candidate (same-block ordering, shadowing, and the
    // mutated-root bail-out), so this wider filter can only cost a
    // resolver run, never a wrong removal.
    let mut pairs: Vec<(SymbolId, SymbolId)> = Vec::new();
    let mut calls: Vec<(SymbolId, String, NodeId, NodeId)> = Vec::new();
    let mut member_pairs: Vec<(SymbolId, SymbolId, String)> = Vec::new();
    let mut name_calls: Vec<(SymbolId, NodeId, NodeId)> = Vec::new();
    ast_utils::walk(source, root, &mut |node| {
        match source.node(node) {
            Node::Local(targets, values) => {
                for (i, target) in targets.iter().enumerate() {
                    let Some(value) = values.get(i) else { continue };
                    let value = unwrap_paren(source, *value);
                    if let Node::Name(symbol) = source.node(value) {
                        pairs.push((*target, *symbol));
                    }
                    if let Node::Index(obj, key, _) = source.node(value) {
                        if let Node::Name(base) = source.node(*obj) {
                            if let Node::Str(field) = source.node(*key) {
                                let field_text = field.as_ref();
                                let field_text = field_text
                                    .strip_prefix('"')
                                    .and_then(|text| text.strip_suffix('"'))
                                    .unwrap_or(field_text);
                                member_pairs.push((*target, *base, field_text.to_string()));
                            }
                        }
                    }
                }
            }
            Node::Assign(targets, values) => {
                for (i, target) in targets.iter().enumerate() {
                    let Node::Name(symbol) = source.node(*target) else {
                        continue;
                    };
                    let Some(value) = values.get(i) else { continue };
                    let value = unwrap_paren(source, *value);
                    if let Node::Name(value_symbol) = source.node(value) {
                        pairs.push((*symbol, *value_symbol));
                    }
                    if let Node::Index(obj, key, _) = source.node(value) {
                        if let Node::Name(base) = source.node(*obj) {
                            if let Node::Str(field) = source.node(*key) {
                                let field_text = field.as_ref();
                                let field_text = field_text
                                    .strip_prefix('"')
                                    .and_then(|text| text.strip_suffix('"'))
                                    .unwrap_or(field_text);
                                member_pairs.push((*symbol, *base, field_text.to_string()));
                            }
                        }
                    }
                }
            }
            Node::Callstat(call) => {
                let Node::Call(callee, args, method) = source.node(*call) else {
                    return;
                };
                if method.is_some() || args.len() != 2 {
                    return;
                }
                if let Node::Name(base) = source.node(*callee) {
                    name_calls.push((*base, args[0], args[1]));
                    return;
                }
                let Node::Index(obj, key, _) = source.node(*callee) else {
                    return;
                };
                let Node::Name(base) = source.node(*obj) else {
                    return;
                };
                let Node::Str(field) = source.node(*key) else {
                    return;
                };
                // The parser stores a dot-access key as a quoted Lua
                // string (for example `"setNumber"`); compare the
                // unquoted member name.
                let field_text = field.as_ref();
                let field_text = field_text
                    .strip_prefix('"')
                    .and_then(|text| text.strip_suffix('"'))
                    .unwrap_or(field_text);
                calls.push((*base, field_text.to_string(), args[0], args[1]));
            }
            _ => {}
        }
    });
    // Seed: every symbol whose name is `output` (direct bases and
    // binding values alike; interning normally makes this one symbol).
    let mut aliases: HashSet<SymbolId> = HashSet::new();
    for (_, value) in &pairs {
        if source.strings.get(*value) == "output" {
            aliases.insert(*value);
        }
    }
    for (base, _, _, _) in &calls {
        if source.strings.get(*base) == "output" {
            aliases.insert(*base);
        }
    }
    // Fixpoint: a name bound to an alias name is itself an alias.
    loop {
        let mut grown = false;
        for (target, value) in &pairs {
            if aliases.contains(value) && aliases.insert(*target) {
                grown = true;
            }
        }
        if !grown {
            break;
        }
    }
    // Callee aliases (v8): a name bound to `output.setNumber` /
    // `output.setBool`, directly, through an output alias, or through
    // another callee alias. This is a filter-only closure; resolution
    // in `store_key` still confirms every candidate.
    let mut callee_aliases: HashMap<SymbolId, bool> = HashMap::new();
    loop {
        let mut grown = false;
        for (target, base, member) in &member_pairs {
            let is_bool = match member.as_str() {
                "setNumber" => false,
                "setBool" => true,
                _ => continue,
            };
            if (source.strings.get(*base) == "output" || aliases.contains(base))
                && !callee_aliases.contains_key(target)
            {
                callee_aliases.insert(*target, is_bool);
                grown = true;
            }
        }
        for (target, value) in &pairs {
            if let Some(is_bool) = callee_aliases.get(value).copied() {
                if !callee_aliases.contains_key(target) {
                    callee_aliases.insert(*target, is_bool);
                    grown = true;
                }
            }
        }
        if !grown {
            break;
        }
    }
    calls.iter().any(|(base, field, channel, value)| {
        if source.strings.get(*base) != "output" && !aliases.contains(base) {
            return false;
        }
        if !matches!(source.node(*channel), Node::Num(_)) {
            return false;
        }
        matches!(
            (field.as_str(), source.node(*value)),
            ("setNumber", Node::Num(_))
                | ("setNumber", Node::Name(_))
                | ("setBool", Node::Bool(_))
                | ("setBool", Node::Name(_))
        )
    }) || name_calls.iter().any(|(base, channel, value)| {
        let Some(is_bool) = callee_aliases.get(base).copied() else {
            return false;
        };
        if !matches!(source.node(*channel), Node::Num(_)) {
            return false;
        }
        matches!(
            (is_bool, source.node(*value)),
            (false, Node::Num(_))
                | (false, Node::Name(_))
                | (true, Node::Bool(_))
                | (true, Node::Name(_))
        )
    })
}

/// True when a statement cannot observe output state, change it, or
/// abort the block: a local binding whose subtree holds only locals,
/// name assignments, literal or name values, and member reads on
/// builtin roots (v8: a callee-alias binding such as
/// `local a = output.setNumber` reads a host table field, which cannot
/// change output state, observe a store, or abort the block, so it must
/// not break a run the way an arbitrary call does). Calls, other index
/// operations, control flow, labels, and nested blocks are all barriers.
fn is_pure_non_store(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    ctx: &StoreCtx<'_>,
    statements: &[NodeId],
    bound: usize,
    statement: NodeId,
) -> bool {
    let mut pure = true;
    ast_utils::walk(source, statement, &mut |node| {
        if !pure {
            return;
        }
        match source.node(node) {
            Node::Local(_, _) => {}
            Node::Assign(targets, _) => {
                if !targets
                    .iter()
                    .all(|target| matches!(source.node(*target), Node::Name(_)))
                {
                    pure = false;
                }
            }
            Node::Num(_) | Node::Bool(_) | Node::Str(_) | Node::Nil | Node::Name(_) => {}
            Node::Paren(_) => {}
            Node::Index(obj, _, _) => {
                if !index_base_is_builtin_root(source, analyzer, ctx, statements, bound, *obj) {
                    pure = false;
                }
            }
            _ => pure = false,
        }
    });
    pure
}

struct Transformer<'a> {
    source: &'a Ast,
    resolution: &'a Resolution,
    analyzer: EffectAnalyzer<'a>,
    outer_aliases: HashSet<BindingId>,
    outer_callee_aliases: HashMap<BindingId, bool>,
    outer_value_aliases: HashMap<BindingId, String>,
    output: Ast,
    removed: usize,
    mutated: HashSet<String>,
}

impl<'a> Transformer<'a> {
    fn new(source: &'a Ast, root: NodeId, resolution: &'a Resolution) -> Self {
        let analyzer = EffectAnalyzer::new(source, resolution, root, true);
        let outer_aliases = outer_output_aliases(source, &analyzer, resolution, root);
        let outer_callee_aliases =
            outer_callee_aliases(source, &analyzer, resolution, root, &outer_aliases);
        let outer_value_aliases = outer_value_aliases(source, &analyzer, resolution, root);
        Self {
            source,
            resolution,
            analyzer,
            outer_aliases,
            outer_callee_aliases,
            outer_value_aliases,
            output: clone_source(source),
            removed: 0,
            mutated: mutated_builtin_roots(source, resolution, root),
        }
    }

    fn transform_block(&mut self, block: NodeId) {
        let Node::Block(statements) = self.source.node(block).clone() else {
            return;
        };
        for statement in &statements {
            self.transform_statement(*statement);
        }
        // Backwards scan: `seen` holds the (kind, channel) pairs for
        // which a later literal store exists with no barrier between it
        // and the current position. A store whose key is already seen is
        // overwritten before anything can observe it, so it is dead.
        let ctx = StoreCtx {
            resolution: self.resolution,
            outer: &self.outer_aliases,
            outer_callee: &self.outer_callee_aliases,
            outer_value: &self.outer_value_aliases,
            mutated: &self.mutated,
        };
        let mut seen: HashSet<(bool, i64)> = HashSet::new();
        let mut keep = vec![true; statements.len()];
        for (index, statement) in statements.iter().enumerate().rev() {
            match store_key(self.source, &self.analyzer, &ctx, &statements, index) {
                Some(key) => {
                    if seen.contains(&key) {
                        keep[index] = false;
                        self.removed += 1;
                    } else {
                        seen.insert(key);
                    }
                }
                None => {
                    if !is_pure_non_store(
                        self.source,
                        &self.analyzer,
                        &ctx,
                        &statements,
                        index,
                        *statement,
                    ) {
                        seen.clear();
                    }
                }
            }
        }
        let kept: Vec<NodeId> = statements
            .iter()
            .zip(keep.iter())
            .filter(|(_, kept)| **kept)
            .map(|(statement, _)| *statement)
            .collect();
        if kept != statements {
            self.output.nodes.rewrite(
                block,
                Node::Block(kept),
                "redundant-output-store-elimination",
            );
        }
    }

    fn transform_statement(&mut self, statement: NodeId) {
        match self.source.node(statement).clone() {
            Node::Block(_) => self.transform_block(statement),
            Node::Do(body) => self.transform_block(body),
            Node::While(condition, body) => {
                self.transform_expr(condition);
                self.transform_block(body);
            }
            Node::Repeat(body, condition) => {
                self.transform_block(body);
                self.transform_expr(condition);
            }
            Node::If(arms, otherwise) => {
                for arm in arms {
                    self.transform_expr(arm.cond);
                    self.transform_block(arm.body);
                }
                if let Some(body) = otherwise {
                    self.transform_block(body);
                }
            }
            Node::Fornum(_, start, end, step, body) => {
                self.transform_expr(start);
                self.transform_expr(end);
                if let Some(step) = step {
                    self.transform_expr(step);
                }
                self.transform_block(body);
            }
            Node::Forin(_, expressions, body) => {
                for expression in expressions {
                    self.transform_expr(expression);
                }
                self.transform_block(body);
            }
            Node::Funcstat(target, function) => {
                self.transform_expr(target);
                self.transform_expr(function);
            }
            Node::Localfunc(_, function) => self.transform_expr(function),
            Node::Local(_, expressions) | Node::Return(expressions) => {
                for expression in expressions {
                    self.transform_expr(expression);
                }
            }
            Node::Callstat(expression) => self.transform_expr(expression),
            Node::Assign(targets, expressions) => {
                for target in targets {
                    self.transform_expr(target);
                }
                for expression in expressions {
                    self.transform_expr(expression);
                }
            }
            _ => {}
        }
    }

    fn transform_expr(&mut self, node: NodeId) {
        match self.source.node(node).clone() {
            Node::Function(_, _, body) => self.transform_block(body),
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

/// Remove redundant output stores. Returns `removed=N` in `details`
/// (0 when nothing changed, in which case `ast` is left untouched).
pub fn eliminate_redundant_output_stores(ast: &mut Ast, root: NodeId) -> PassResult {
    if !matches!(ast.node(root), Node::Block(_)) {
        return PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["removed=0".to_string()]),
        };
    }
    let source = clone_source(ast);
    if !has_syntactic_candidate(&source, root) {
        return PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["removed=0".to_string()]),
        };
    }
    let resolution = resolve(&source, root);
    if mutated_builtin_roots(&source, &resolution, root).contains("output") {
        return PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["removed=0".to_string()]),
        };
    }
    let mut transformer = Transformer::new(&source, root, &resolution);
    transformer.transform_block(root);
    let removed = transformer.removed;
    if removed == 0 {
        return PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["removed=0".to_string()]),
        };
    }
    *ast = transformer.output;
    PassResult {
        root,
        saved: None,
        details: Some(vec![format!("removed={removed}")]),
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
        let result = eliminate_redundant_output_stores(&mut ast, root);
        let code = Printer::new(&ast, false).output(result.root);
        let removed = result
            .details
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.strip_prefix("removed="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        (code, removed)
    }

    #[test]
    fn removes_earlier_store_for_same_channel() {
        let (code, removed) = run(
            "function onTick() output.setNumber(1,5) output.setNumber(1,9) output.setNumber(2,3) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
        assert!(!code.contains("setNumber(1,5)"), "code: {code}");
        assert!(code.contains("setNumber(1,9)"), "code: {code}");
    }

    #[test]
    fn keeps_last_per_channel_and_kind() {
        let (code, removed) = run(
            "function onTick() output.setNumber(1,1) output.setBool(1,true) output.setNumber(1,2) output.setBool(1,false) end",
        );
        assert_eq!(removed, 2, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
        assert_eq!(code.matches("output.setBool").count(), 1, "code: {code}");
        assert!(code.contains("setNumber(1,2)"), "code: {code}");
        // The printer shortens booleans in argument position (`false` -> `1>2`).
        assert!(code.contains("setBool(1,1>2)"), "code: {code}");
        assert!(!code.contains("setBool(1,1>0)"), "code: {code}");
    }

    #[test]
    fn intervening_statement_breaks_the_run() {
        let (code, removed) = run(
            "function onTick() output.setNumber(1,5) local x=input.getNumber(1) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn removes_across_pure_local_binding() {
        let (code, removed) =
            run("function onTick() output.setNumber(1,5) local x=3 output.setNumber(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
        assert!(code.contains("setNumber(1,9)"), "code: {code}");
        assert!(code.contains("local x=3"), "code: {code}");
    }

    #[test]
    fn barrier_call_blocks_removal() {
        let (code, removed) =
            run("function onTick() output.setNumber(1,5) foo() output.setNumber(1,9) end");
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn barrier_if_blocks_removal() {
        let (code, removed) = run(
            "function onTick() output.setNumber(1,5) if flag then x=1 end output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn barrier_non_literal_output_store_blocks_removal() {
        let (code, removed) = run(
            "function onTick() output.setNumber(1,5) output.setNumber(ch,9) output.setNumber(1,7) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 3, "code: {code}");
    }

    #[test]
    fn name_value_is_not_a_candidate() {
        // A name value could hold a wrong-typed value that makes the host
        // call error, so it neither counts as a covering store nor as a
        // removable one; it is a barrier like any other call statement.
        let (code, removed) =
            run("function onTick() output.setNumber(1,value) output.setNumber(1,9) end");
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn skips_non_literal_channel_and_value() {
        let (code, removed) = run(
            "function onTick() output.setNumber(1,5) output.setNumber(i,9) output.setNumber(1,7) output.setBool(1,input.getBool(1)) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 3, "code: {code}");
    }

    #[test]
    fn skips_mutated_output_root() {
        let (code, removed) = run(
            "output.setNumber=function() end function onTick() output.setNumber(1,5) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 3, "code: {code}");
    }

    #[test]
    fn aliased_form_is_in_scope() {
        // v6: an alias bound to `output` in an earlier statement of
        // the same block counts as a store (the v1 limit this test
        // used to document is lifted; see the v6 module docs).
        let (code, removed) =
            run("function onTick() local o=output o.setNumber(1,5) o.setNumber(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("o.setNumber").count(), 1, "code: {code}");
    }

    #[test]
    fn removes_inside_loop_body_blocks() {
        let (code, removed) = run(
            "function onTick() for i=1,3 do output.setNumber(1,5) output.setNumber(1,9) end end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
    }

    #[test]
    fn removes_proven_name_value_earlier_store() {
        let (code, removed) =
            run("function onTick() local v=5 output.setNumber(1,v) output.setNumber(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
        assert!(!code.contains("setNumber(1,v)"), "code: {code}");
        assert!(code.contains("setNumber(1,9)"), "code: {code}");
    }

    #[test]
    fn proven_name_value_covers_earlier_literal_store() {
        let (code, removed) =
            run("function onTick() output.setNumber(1,5) local v=9 output.setNumber(1,v) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
        assert!(code.contains("setNumber(1,v)"), "code: {code}");
    }

    #[test]
    fn wrong_typed_binding_is_not_proven() {
        let (code, removed) =
            run("function onTick() local v=true output.setNumber(1,v) output.setNumber(1,9) end");
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn numeric_reassignment_stays_proven() {
        let (code, removed) =
            run("function onTick() local v=5 v=9 output.setNumber(1,v) output.setNumber(1,9) end");
        // The nearest binding is the assignment `v=9` (a number), so the
        // name is still proven; only a wrong-typed reassignment breaks it.
        assert_eq!(removed, 1, "code: {code}");
    }

    #[test]
    fn wrong_typed_reassignment_breaks_the_proof() {
        let (code, removed) = run(
            "function onTick() local v=5 v=true output.setNumber(1,v) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn intervening_call_breaks_the_proof() {
        let (code, removed) = run(
            "function onTick() local v=5 foo() output.setNumber(1,v) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn chained_name_binding_is_proven() {
        let (code, removed) = run(
            "function onTick() local a=5 local b=a output.setNumber(1,b) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
    }

    #[test]
    fn multi_target_local_binding_is_proven() {
        let (code, removed) = run(
            "function onTick() local a,b=5,true output.setNumber(1,a) output.setBool(1,b) output.setNumber(1,9) output.setBool(1,false) end",
        );
        assert_eq!(removed, 2, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
        assert_eq!(code.matches("output.setBool").count(), 1, "code: {code}");
    }

    #[test]
    fn host_call_binding_is_proven() {
        let (code, removed) = run(
            "function onTick() local v=input.getNumber(1) output.setNumber(1,v) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
        assert!(code.contains("setNumber(1,9)"), "code: {code}");
    }

    #[test]
    fn host_bool_call_binding_is_proven() {
        let (code, removed) = run(
            "function onTick() local b=input.getBool(1) output.setBool(1,b) output.setBool(1,false) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setBool").count(), 1, "code: {code}");
    }

    #[test]
    fn math_call_binding_is_proven() {
        let (code, removed) = run(
            "function onTick() local v=math.floor(3.7) output.setNumber(1,v) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
        assert!(code.contains("setNumber(1,9)"), "code: {code}");
    }

    #[test]
    fn screen_call_binding_is_proven() {
        let (code, removed) = run(
            "function onDraw() local w=screen.getWidth() output.setNumber(1,w) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
    }

    #[test]
    fn call_bound_name_covers_earlier_literal_store() {
        let (code, removed) = run(
            "function onTick() local v=property.getNumber(\"x\") output.setNumber(1,5) output.setNumber(1,v) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert!(code.contains("setNumber(1,v)"), "code: {code}");
    }

    #[test]
    fn wrong_typed_call_binding_is_not_proven() {
        let (code, removed) = run(
            "function onTick() local t=property.getText(\"x\") output.setNumber(1,t) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn unknown_call_binding_is_not_proven() {
        let (code, removed) =
            run("function onTick() local v=foo() output.setNumber(1,v) output.setNumber(1,9) end");
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn mutated_host_root_breaks_call_proof() {
        let (code, removed) = run(
            "input.getNumber=function() return 1 end function onTick() local v=input.getNumber(1) output.setNumber(1,v) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn mutated_math_root_breaks_call_proof() {
        let (code, removed) = run(
            "math.floor=function() return true end function onTick() local v=math.floor(3.7) output.setNumber(1,v) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn non_total_call_binding_is_not_proven() {
        let (code, removed) = run(
            "function onTick() local v=input.getNumber(i) output.setNumber(1,v) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn erroring_call_binding_is_not_proven() {
        let (code, removed) = run(
            "function onTick() local v=math.floor({}) output.setNumber(1,v) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn missing_argument_call_binding_is_not_proven() {
        let (code, removed) = run(
            "function onTick() local v=math.floor() output.setNumber(1,v) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn wrong_typed_label_call_binding_is_not_proven() {
        let (code, removed) = run(
            "function onTick() local v=property.getNumber(1) output.setNumber(1,v) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn nested_total_call_binding_is_proven() {
        let (code, removed) = run(
            "function onTick() local v=math.max(input.getNumber(1),input.getNumber(2)) output.setNumber(1,v) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
        assert!(code.contains("setNumber(1,9)"), "code: {code}");
    }

    #[test]
    fn ult_binding_proves_bool() {
        let (code, removed) = run(
            "function onTick() local v=math.ult(1,2) output.setBool(1,v) output.setBool(1,true) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setBool").count(), 1, "code: {code}");
    }

    #[test]
    fn ult_binding_with_non_integer_argument_is_not_proven() {
        let (code, removed) = run(
            "function onTick() local v=math.ult(1.5,2) output.setBool(1,v) output.setBool(1,true) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setBool").count(), 2, "code: {code}");
    }

    #[test]
    fn ult_binding_with_name_argument_is_not_proven() {
        let (code, removed) = run(
            "function onTick() local v=math.ult(x,2) output.setBool(1,v) output.setBool(1,true) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setBool").count(), 2, "code: {code}");
    }

    #[test]
    fn ult_binding_with_wrong_arity_is_not_proven() {
        let (code, removed) = run(
            "function onTick() local v=math.ult(1) output.setBool(1,v) output.setBool(1,true) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setBool").count(), 2, "code: {code}");
    }

    #[test]
    fn modf_binding_proves_number() {
        let (code, removed) = run(
            "function onTick() local v=math.modf(3.7) output.setNumber(1,v) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
        assert!(code.contains("setNumber(1,9)"), "code: {code}");
    }

    #[test]
    fn modf_multi_binding_proves_both_results() {
        let (code, removed) = run(
            "function onTick() local a,b=math.modf(3.7) output.setNumber(1,a) output.setNumber(1,9) output.setNumber(2,b) output.setNumber(2,9) end",
        );
        assert_eq!(removed, 2, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn frexp_binding_is_not_proven() {
        // `math.frexp` is absent from the Lua 5.3 math library, so the
        // call errors and can never be a total binding: it must stay
        // unproven even though it looks like a two-value math call.
        let (code, removed) = run(
            "function onTick() local m,e=math.frexp(8) output.setNumber(1,m) output.setNumber(1,9) output.setNumber(2,e) output.setNumber(2,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 4, "code: {code}");
    }

    #[test]
    fn modf_second_result_chain_is_proven() {
        let (code, removed) = run(
            "function onTick() local a,b=math.modf(3.7) local c=b output.setNumber(1,c) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
    }

    #[test]
    fn modf_binding_with_erroring_argument_is_not_proven() {
        let (code, removed) = run(
            "function onTick() local v=math.modf({}) output.setNumber(1,v) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn modf_binding_with_wrong_arity_is_not_proven() {
        let (code, removed) = run(
            "function onTick() local v=math.modf(3.7,2) output.setNumber(1,v) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn alias_math_call_binding_is_proven() {
        let (code, removed) = run(
            "function onTick() local g=math.floor local x=g(2) output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
        assert!(code.contains("setNumber(1,9)"), "code: {code}");
    }

    #[test]
    fn alias_chain_binding_is_proven() {
        let (code, removed) = run(
            "function onTick() local f=math.floor local g=f local x=g(2) output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
    }

    #[test]
    fn alias_host_call_binding_is_proven() {
        let (code, removed) = run(
            "function onTick() local g=input.getNumber local x=g(1) output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
    }

    #[test]
    fn alias_host_bool_call_binding_is_proven() {
        let (code, removed) = run(
            "function onTick() local b=input.getBool local x=b(1) output.setBool(1,x) output.setBool(1,true) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setBool").count(), 1, "code: {code}");
    }

    #[test]
    fn outer_alias_host_call_binding_is_proven() {
        // v9: a chunk local captured by the callback counts as a
        // value-proof callee alias.
        let (code, removed) = run(
            "local g=input.getNumber function onTick() local x=g(1) output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
    }

    #[test]
    fn outer_alias_math_call_binding_is_proven() {
        let (code, removed) = run(
            "local g=math.floor function onTick() local x=g(2) output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
    }

    #[test]
    fn outer_alias_root_chain_binding_is_proven() {
        let (code, removed) = run(
            "local m=math local g=m.floor function onTick() local x=g(2) output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
    }

    #[test]
    fn outer_alias_reassigned_elsewhere_blocks_value_proof() {
        // A write to the alias anywhere disqualifies the outer proof.
        let (code, removed) = run(
            "local g=input.getNumber function onTick() local x=g(1) output.setNumber(1,x) output.setNumber(1,9) end function onDraw() g=input.getBool end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn outer_alias_wrong_type_call_binding_is_not_proven() {
        let (code, removed) = run(
            "local g=input.getBool function onTick() local x=g(1) output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn alias_screen_call_binding_is_proven() {
        let (code, removed) = run(
            "function onTick() local g=screen.getWidth local x=g() output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
    }

    #[test]
    fn alias_call_wrong_type_is_not_proven() {
        let (code, removed) = run(
            "function onTick() local g=math.floor local x=g(2) output.setBool(1,x) output.setBool(1,true) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setBool").count(), 2, "code: {code}");
    }

    #[test]
    fn alias_call_with_non_total_argument_is_not_proven() {
        let (code, removed) = run(
            "function onTick() local g=math.floor local x=g(y) output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn alias_call_wrong_arity_is_not_proven() {
        let (code, removed) = run(
            "function onTick() local g=math.floor local x=g() output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn alias_reassigned_to_unknown_is_not_proven() {
        let (code, removed) = run(
            "function onTick() local g=math.floor g=other local x=g(2) output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn alias_call_blocked_by_intervening_call_is_not_proven() {
        let (code, removed) = run(
            "function onTick() local g=math.floor print(1) local x=g(2) output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn alias_defined_after_use_is_not_proven() {
        let (code, removed) = run(
            "function onTick() local x=g(2) local g=math.floor output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn alias_through_root_table_is_proven() {
        let (code, removed) = run(
            "function onTick() local m=math local g=m.floor local x=g(2) output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
    }

    #[test]
    fn alias_call_nested_in_total_call_is_proven() {
        let (code, removed) = run(
            "function onTick() local g=input.getNumber local x=math.floor(g(1)) output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
    }

    #[test]
    fn alias_call_with_nested_total_argument_is_proven() {
        let (code, removed) = run(
            "function onTick() local g=math.floor local x=g(math.ceil(2.5)) output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
    }

    #[test]
    fn alias_modf_binding_proves_both_results() {
        let (code, removed) = run(
            "function onTick() local g=math.modf local a,b=g(3.7) output.setNumber(1,a) output.setNumber(1,9) output.setNumber(2,b) output.setNumber(2,9) end",
        );
        assert_eq!(removed, 2, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn alias_to_nonexistent_builtin_is_not_proven() {
        let (code, removed) = run(
            "function onTick() local g=math.frexp local x=g(8) output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn same_statement_alias_is_not_proven() {
        // Documented v5 limit: an alias bound in the same statement as
        // its use (`local g, x = math.floor, g(2)`) is not resolved,
        // because the alias proof only scans strictly earlier statements.
        let (code, removed) = run(
            "function onTick() local g,x=math.floor,g(2) output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn intervening_alias_binding_does_not_break_proof() {
        // A clustered alias declaration between the proven alias's
        // binding and its use is alias-safe: it binds a fresh name
        // through a member access on a builtin root and runs no code.
        let (code, removed) = run(
            "function onTick() local g=math.floor local h=math.sqrt local x=g(2) output.setNumber(1,x) output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("output.setNumber").count(), 1, "code: {code}");
    }

    #[test]
    fn alias_store_removes_earlier_alias_store() {
        let (code, removed) =
            run("function onTick() local o=output o.setNumber(1,5) o.setNumber(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("setNumber").count(), 1, "code: {code}");
        assert!(code.contains("setNumber(1,9)"), "code: {code}");
        assert!(!code.contains("setNumber(1,5)"), "code: {code}");
    }

    #[test]
    fn alias_chain_store_removes_earlier() {
        let (code, removed) =
            run("function onTick() local o=output local p=o p.setNumber(1,5) p.setNumber(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("setNumber").count(), 1, "code: {code}");
        assert!(code.contains("setNumber(1,9)"), "code: {code}");
    }

    #[test]
    fn alias_assignment_store_removes_earlier() {
        let (code, removed) =
            run("function onTick() o=output o.setNumber(1,5) o.setNumber(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("setNumber").count(), 1, "code: {code}");
        assert!(code.contains("setNumber(1,9)"), "code: {code}");
    }

    #[test]
    fn alias_store_killed_by_later_direct_store() {
        let (code, removed) =
            run("function onTick() local o=output o.setNumber(1,5) output.setNumber(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("setNumber").count(), 1, "code: {code}");
        assert!(code.contains("output.setNumber(1,9)"), "code: {code}");
    }

    #[test]
    fn direct_store_killed_by_later_alias_store() {
        let (code, removed) =
            run("function onTick() output.setNumber(1,5) local o=output o.setNumber(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("setNumber").count(), 1, "code: {code}");
        assert!(code.contains("o.setNumber(1,9)"), "code: {code}");
    }

    #[test]
    fn alias_bound_after_store_is_not_proven() {
        // Documented v6 limit: a store through an alias bound only
        // after the store must not be proven by that later binding
        // (the earlier store reads a different, outer `o`).
        let (code, removed) =
            run("function onTick() o.setNumber(1,5) local o=output o.setNumber(1,9) end");
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn alias_shadowed_output_is_not_proven() {
        // A local named `output` holding a table is not the host
        // table, so stores through its alias must not be removed.
        let (code, removed) = run(
            "function onTick() local output={} local o=output o.setNumber(1,5) o.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn alias_store_with_proven_name_value() {
        let (code, removed) =
            run("function onTick() local o=output local v=9 o.setNumber(1,v) o.setNumber(1,3) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("setNumber").count(), 1, "code: {code}");
        assert!(code.contains("setNumber(1,3)"), "code: {code}");
    }

    #[test]
    fn alias_store_setbool_removes_earlier() {
        let (code, removed) =
            run("function onTick() local o=output o.setBool(1,true) o.setBool(1,false) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("setBool").count(), 1, "code: {code}");
        // The printer shortens booleans in argument position (`false` -> `1>2`).
        assert!(code.contains("setBool(1,1>2)"), "code: {code}");
    }

    #[test]
    fn zero_write_alias_survives_intervening_call() {
        // v7 supersedes the v6 limit for this shape: a call between the
        // alias binding and the store no longer breaks the proof when
        // the binding is never written anywhere in the program. The call
        // sits before the first store (not between the two stores), so
        // it can only matter by reassigning the alias, and any such
        // reassignment -- including through a captured closure the call
        // might reach -- is a write that disqualifies the v7 proof.
        // Table-field mutation of `output` itself is covered separately
        // by the pass-level mutated-output bail-out.
        let (code, removed) =
            run("function onTick() local o=output foo() o.setNumber(1,5) o.setNumber(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("setNumber").count(), 1, "code: {code}");
        assert!(code.contains("setNumber(1,9)"), "code: {code}");
    }

    #[test]
    fn outer_alias_store_removes_earlier() {
        // v7: a chunk local captured by the callback counts as a store.
        let (code, removed) =
            run("local o=output function onTick() o.setNumber(1,5) o.setNumber(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("o.setNumber").count(), 1, "code: {code}");
        assert!(!code.contains("setNumber(1,5)"), "code: {code}");
        assert!(code.contains("setNumber(1,9)"), "code: {code}");
    }

    #[test]
    fn outer_alias_chain_store_removes_earlier() {
        let (code, removed) =
            run("local o=output local p=o function onTick() p.setNumber(1,5) p.setNumber(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("p.setNumber").count(), 1, "code: {code}");
        assert!(code.contains("setNumber(1,9)"), "code: {code}");
    }

    #[test]
    fn outer_alias_reassigned_elsewhere_blocks_removal() {
        // A write to the alias anywhere (here in another callback)
        // disqualifies the outer proof, so nothing is removed.
        let (code, removed) = run(
            "local o=output function onTick() o.setNumber(1,5) o.setNumber(1,9) end function onDraw() o=nil end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("o.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn outer_alias_stores_in_different_callbacks_are_independent() {
        // One store per callback invocation is never dead, even though
        // both go through the same outer alias.
        let (code, removed) = run(
            "local o=output function onTick() o.setNumber(1,5) end function onDraw() o.setNumber(1,9) end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("o.setNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn outer_alias_call_before_callback_does_not_break_proof() {
        // v7: unlike the same-block scan, an intervening call cannot
        // reassign a zero-write alias (any closure assignment would be
        // a write and disqualify the binding), so the proof stands.
        let (code, removed) =
            run("local o=output foo() function onTick() o.setNumber(1,5) o.setNumber(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("o.setNumber").count(), 1, "code: {code}");
    }

    #[test]
    fn outer_alias_store_inside_nested_function_removes_earlier() {
        // v7: the alias is bound in the enclosing function body and
        // used by a nested function defined after the binding, so the
        // binding has run before any call of that nested function.
        let (code, removed) = run(
            "function onTick() local o=output local function f() o.setNumber(1,5) o.setNumber(1,9) end f() end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("o.setNumber").count(), 1, "code: {code}");
        assert!(code.contains("setNumber(1,9)"), "code: {code}");
    }

    #[test]
    fn callee_alias_store_removes_earlier() {
        // v8: a call through a name bound to `output.setNumber`
        // counts as an output store.
        let (code, removed) = run("function onTick() local a=output.setNumber a(1,5) a(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("a(1,").count(), 1, "code: {code}");
        assert!(code.contains("a(1,9)"), "code: {code}");
        assert!(!code.contains("a(1,5)"), "code: {code}");
    }

    #[test]
    fn callee_alias_chain_store_removes_earlier() {
        let (code, removed) =
            run("function onTick() local a=output.setNumber local b=a b(1,5) b(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("b(1,").count(), 1, "code: {code}");
        assert!(code.contains("b(1,9)"), "code: {code}");
    }

    #[test]
    fn callee_alias_through_object_alias_store_removes_earlier() {
        let (code, removed) =
            run("function onTick() local o=output local a=o.setNumber a(1,5) a(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("a(1,").count(), 1, "code: {code}");
        assert!(code.contains("a(1,9)"), "code: {code}");
    }

    #[test]
    fn callee_alias_setbool_store_removes_earlier() {
        let (code, removed) =
            run("function onTick() local a=output.setBool a(1,true) a(1,false) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("a(1,").count(), 1, "code: {code}");
        // The printer shortens booleans in argument position (`false` -> `1>2`).
        assert!(code.contains("a(1,1>2)"), "code: {code}");
    }

    #[test]
    fn direct_store_killed_by_later_callee_alias_store() {
        let (code, removed) =
            run("function onTick() output.setNumber(1,5) local a=output.setNumber a(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert!(code.contains("a(1,9)"), "code: {code}");
        assert!(!code.contains("setNumber(1,5)"), "code: {code}");
    }

    #[test]
    fn callee_alias_store_killed_by_later_direct_store() {
        let (code, removed) =
            run("function onTick() local a=output.setNumber a(1,5) output.setNumber(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert!(code.contains("output.setNumber(1,9)"), "code: {code}");
        assert!(!code.contains("a(1,5)"), "code: {code}");
    }

    #[test]
    fn callee_alias_reassigned_is_not_proven() {
        // The nearest binding of `a` before the stores is the
        // reassignment to an unknown value, so neither store counts.
        let (code, removed) =
            run("function onTick() local a=output.setNumber a=other a(1,5) a(1,9) end");
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("a(1,").count(), 2, "code: {code}");
    }

    #[test]
    fn callee_alias_bound_after_store_is_not_proven() {
        // Documented v8 limit, same shape as v6: a store through an
        // alias bound only after the store reads a different, outer
        // `a`, so it must not be proven by that later binding.
        let (code, removed) = run("function onTick() a(1,5) local a=output.setNumber a(1,9) end");
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("a(1,").count(), 2, "code: {code}");
    }

    #[test]
    fn outer_callee_alias_store_removes_earlier() {
        // v8: a chunk local bound to `output.setNumber` and captured
        // by the callback counts as a store.
        let (code, removed) = run("local a=output.setNumber function onTick() a(1,5) a(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert_eq!(code.matches("a(1,").count(), 1, "code: {code}");
        assert!(code.contains("a(1,9)"), "code: {code}");
        assert!(!code.contains("a(1,5)"), "code: {code}");
    }

    #[test]
    fn outer_callee_alias_reassigned_elsewhere_blocks_removal() {
        // A write to the alias anywhere (here in another callback)
        // disqualifies the outer proof, so nothing is removed.
        let (code, removed) = run(
            "local a=output.setNumber function onTick() a(1,5) a(1,9) end function onDraw() a=nil end",
        );
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("a(1,").count(), 2, "code: {code}");
    }

    #[test]
    fn callee_alias_binding_between_stores_is_transparent() {
        // v8: a callee-alias binding statement reads a host table
        // field, so it is pure for run purposes and must not break a
        // run of direct stores the way an arbitrary call does.
        let (code, removed) = run(
            "function onTick() output.setNumber(1,5) local a=output.setNumber output.setNumber(1,9) end",
        );
        assert_eq!(removed, 1, "code: {code}");
        assert!(code.contains("setNumber(1,9)"), "code: {code}");
        assert!(!code.contains("setNumber(1,5)"), "code: {code}");
    }

    #[test]
    fn callee_alias_binding_between_value_and_store_is_transparent() {
        // The value proof scan must cross the callee-alias binding
        // statement to reach the value binding.
        let (code, removed) =
            run("function onTick() local v=5 local a=output.setNumber a(1,v) a(1,9) end");
        assert_eq!(removed, 1, "code: {code}");
        assert!(code.contains("a(1,9)"), "code: {code}");
        assert!(!code.contains("a(1,v)"), "code: {code}");
    }

    #[test]
    fn callee_alias_non_literal_channel_is_not_proven() {
        let (code, removed) = run("function onTick() local a=output.setNumber a(ch,5) a(1,9) end");
        assert_eq!(removed, 0, "code: {code}");
        assert!(code.contains("a(ch,5)"), "code: {code}");
        assert!(code.contains("a(1,9)"), "code: {code}");
    }

    #[test]
    fn callee_alias_wrong_arg_count_is_not_proven() {
        let (code, removed) = run("function onTick() local a=output.setNumber a(1) a(1,9) end");
        assert_eq!(removed, 0, "code: {code}");
        assert!(code.contains("a(1)"), "code: {code}");
        assert!(code.contains("a(1,9)"), "code: {code}");
    }

    #[test]
    fn callee_alias_shadowed_output_is_not_proven() {
        // A local named `output` holding a table is not the host
        // table, so calls through its member alias must not be removed.
        let (code, removed) =
            run("function onTick() local output={} local a=output.setNumber a(1,5) a(1,9) end");
        assert_eq!(removed, 0, "code: {code}");
        assert_eq!(code.matches("a(1,").count(), 2, "code: {code}");
    }
}
