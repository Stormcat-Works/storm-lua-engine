//! Loop-invariant pure host-read hoisting, target-size compilation only.
//!
//! A pure host read (`input.getNumber` / `input.getBool` with a numeric
//! literal, `property.get*` with a string literal, `screen.getWidth` /
//! `screen.getHeight` with no arguments) is constant within one callback
//! invocation: the host sets `input` / `properties` / `size` before the
//! call and never changes them during it, and the call cannot error for
//! those literal arguments. Hoisting such a call out of a loop therefore
//! preserves execution results, while replacing every in-loop call with
//! the hoisted local.
//!
//! A pure math call over total numeric operands (numeric literals,
//! numeric pure host reads, and nested total math calls) is likewise
//! constant within one callback invocation and cannot error, so it
//! hoists the same way. A name of unknown type is never a total
//! operand: it could make the call error, and hoisting it would move
//! that error before the loop and before side effects in the loop body.
//! A name proven numeric by the name-operand proof below is total.
//! Multi-value math calls (`modf`, `frexp`) are excluded as well:
//! replacing such a call with a name would change its value count.
//!
//! This pass is intentionally conservative:
//! - Calls in the loop body are hoisted to the outermost loop whose body
//!   contains them, including calls inside nested loops and nested-loop
//!   headers: the outermost loop claims a call first, and an inner loop
//!   never re-hoists a call already claimed. When the outermost loop is
//!   disqualified, an inner loop may still hoist the call to its own
//!   preheader. Calls inside a closure may run after the callback, so
//!   they are never hoisted.
//! - A loop is skipped entirely if its direct body contains an unknown
//!   call, a method call, a stateful builtin (`math.random`,
//!   `math.randomseed`, `async.*`, `map.*`), a host-table write, a host
//!   table passed as an argument, or a `goto` / label. Known builtin host
//!   calls (`output.set*`, `screen.draw*`, pure `math.*`, …) are allowed:
//!   they do not change `input` / `property` / `size` state.
//! - A loop is also skipped if any host root it reads is mutated anywhere
//!   in the program (`mutated_builtin_roots`), so aliasing writes cannot
//!   invalidate the hoist.
//! - Loop headers (conditions, iterator expressions, bounds) may contain
//!   only candidate pure reads or pure `math.*` calls.
//! - The hoist is a plain local declared in the block that contains the
//!   loop, so it executes exactly once before the loop even when the loop
//!   runs zero times. That is safe only because the hoisted call is pure
//!   and cannot error for its literal arguments.
//!
//! A pure arithmetic expression over total operands (numeric literals,
//! total names, numeric pure host reads, total math calls, and nested
//! total arithmetic) is likewise constant within a loop and cannot
//! error, so it hoists the same way: the whole expression is evaluated
//! once in the preheader and every in-loop occurrence is replaced by
//! the hoisted local. An expression made only of literals or math
//! constants is not hoisted (constant folding owns that case).
//!
//! Target-size only: hoisting lengthens the static code (one definition
//! plus one name per use) whenever a call occurs once in the body, so it
//! must never run in size-minimizing compilation. The caller (`search.rs`)
//! builds a `joint-api-local-licm` layout from this pass followed by the
//! localized alias pass, and only that layout may win through target-size
//! winner selection.

use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use storm_lua_analysis::effects::{mutated_builtin_roots, EffectAnalyzer};
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId};
use storm_lua_syntax::ast_utils;

/// Pure host reads that may be hoisted, with their literal-argument rule.
fn pure_read_key(source: &Ast, builtin: &str, args: &[NodeId]) -> Option<String> {
    match builtin {
        "input.getNumber" | "input.getBool" => {
            if args.len() != 1 {
                return None;
            }
            let Node::Num(text) = source.node(args[0]) else {
                return None;
            };
            Some(format!("{builtin}({text})"))
        }
        "property.getNumber" | "property.getBool" | "property.getText" => {
            if args.len() != 1 {
                return None;
            }
            let Node::Str(text) = source.node(args[0]) else {
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

/// Single-value pure math calls that may be hoisted when every operand
/// is total (see `math_operand_key`). Multi-value calls (`modf`,
/// `frexp`) are excluded: replacing such a call with a name would
/// change its value count. Stateful calls (`random`, `randomseed`)
/// and `ult` (integer-only semantics) are excluded too.
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

/// Canonical key for a total numeric operand of a math call: a numeric
/// literal, a numeric pure host read, a nested total math call, or a
/// name proven total numeric by `proof`. A name operand that fails the
/// proof makes the whole call a non-candidate, while a pure host read
/// nested inside it is still collected on its own (the collector
/// descends into non-candidates).
///
/// Proof that a math-call operand is a total numeric value, for names
/// bound by a single-assignment `local` in the enclosing function body.
///
/// A name operand is total only when all of the following hold:
/// - its binding comes from a `local` statement that is a direct
///   statement of the enclosing function body block or of an ancestor
///   block (the chunk block or an enclosing function body; the scope
///   argument in `transform_loop` explains why such a binding is
///   already assigned when the loop runs);
/// - it is never reassigned (`binding_write_counts == 0`); and
/// - its initializer is itself total numeric under this proof
///   (numeric literal, total host read with literal arguments, total
///   math call, a numeric `math` constant (`pi`/`huge`/`maxinteger`/
///   `mininteger`), unary minus, or arithmetic over total operands).
///
/// Ancestor bindings are included, not excluded: a name inside a
/// function body can resolve to an ancestor local only when that
/// local is in scope at the function's definition point, which
/// means the binding statement precedes the definition in block
/// order. The function body cannot run before its definition, so
/// the binding has executed by then (and, by the write-count filter
/// in `bindings_for_body`, is never reassigned afterwards). A chunk
/// local declared after the function definition is not in scope
/// inside the function at all, so it resolves as a global instead
/// and stays non-total, as before.
/// Function parameters are excluded too (no `local` binding).
/// Loop-body locals are excluded by construction: only direct
/// statements of ancestor blocks (the chunk block and enclosing
/// function bodies) are collected, never loop bodies.
///
/// This proof is intentionally stricter than the shared
/// `numeric_proof` domain and is not shared with it: that domain's
/// host-read arm ignores call arguments (so a non-literal channel
/// would count as numeric), and it has no notion of which block a
/// binding belongs to, which is exactly the dominance question a
/// loop hoist must answer.
struct NameOperandProof<'a> {
    source: &'a Ast,
    analyzer: &'a EffectAnalyzer<'a>,
    mutated: &'a HashSet<String>,
    resolution: &'a Resolution,
    /// Single-assignment bindings of the current function body block:
    /// binding -> initializer expression.
    bindings: HashMap<BindingId, NodeId>,
}

/// Collect the single-assignment `local` bindings that are direct
/// statements of `block` (the enclosing function body block).
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

impl<'a> NameOperandProof<'a> {
    /// Whether `node` evaluates to a number without erroring, at any
    /// point after its enclosing function body block has started.
    /// Arithmetic on numbers never errors in Lua (division by zero
    /// yields infinity, out-of-range math yields NaN), so numeric
    /// here is exactly total for the admitted call set.
    fn is_total_numeric(&self, node: NodeId, depth: usize) -> bool {
        if depth > 32 {
            return false;
        }
        match self.source.node(node) {
            Node::Num(_) => true,
            Node::Paren(inner) => self.is_total_numeric(*inner, depth + 1),
            Node::Name(_) => {
                let Some(bid) = self.resolution.node_bid[node as usize] else {
                    return false;
                };
                let Some(initializer) = self.bindings.get(&bid) else {
                    return false;
                };
                self.is_total_numeric(*initializer, depth + 1)
            }
            Node::Index(..)
                if matches!(
                    self.analyzer.resolve_builtin_reference(node).as_deref(),
                    Some("math.pi")
                        | Some("math.huge")
                        | Some("math.maxinteger")
                        | Some("math.mininteger")
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

fn math_operand_key(proof: &NameOperandProof<'_>, node: NodeId) -> Option<String> {
    match proof.source.node(node) {
        Node::Num(text) => Some(format!("N({text})")),
        Node::Name(_) => {
            if !proof.is_total_numeric(node, 0) {
                return None;
            }
            let bid = proof.resolution.node_bid[node as usize]?;
            Some(format!("V({bid:?})"))
        }
        Node::Index(..) => {
            let builtin = proof.analyzer.resolve_builtin_reference(node)?;
            let b: &str = &builtin;
            if matches!(
                b,
                "math.pi" | "math.huge" | "math.maxinteger" | "math.mininteger"
            ) && !proof.mutated.contains("math")
            {
                Some(format!("C({b})"))
            } else {
                None
            }
        }
        Node::Call(function, args, method) => {
            if method.is_some() {
                return None;
            }
            let builtin = proof.analyzer.resolve_builtin_reference(*function)?;
            let b: &str = &builtin;
            match b {
                "input.getNumber" | "property.getNumber" | "screen.getWidth"
                | "screen.getHeight" => {
                    if proof.mutated.contains(pure_read_root(b)) {
                        return None;
                    }
                    pure_read_key(proof.source, b, args)
                }
                _ => {
                    if proof.mutated.contains("math") || !math_arity_ok(b, args.len()) {
                        return None;
                    }
                    let mut parts = Vec::new();
                    for arg in args {
                        parts.push(math_operand_key(proof, *arg)?);
                    }
                    Some(format!("{b}({})", parts.join(",")))
                }
            }
        }
        _ => None,
    }
}

/// Canonical key for a total pure math call, or `None` when the call
/// is not a hoistable math call.
fn math_call_key(proof: &NameOperandProof<'_>, node: NodeId) -> Option<String> {
    let Node::Call(function, args, method) = proof.source.node(node) else {
        return None;
    };
    if method.is_some() {
        return None;
    }
    let builtin = proof.analyzer.resolve_builtin_reference(*function)?;
    let b: &str = &builtin;
    if proof.mutated.contains("math") || !math_arity_ok(b, args.len()) {
        return None;
    }
    let mut parts = Vec::new();
    for arg in args {
        parts.push(math_operand_key(proof, *arg)?);
    }
    Some(format!("{b}({})", parts.join(",")))
}

/// Canonical key for a total pure arithmetic expression, or `None`
/// when the expression is not hoistable arithmetic. Leaves are numeric
/// literals, total names, numeric math constants, numeric pure host
/// reads, and total math calls; interior nodes are `+ - * / ^` Bin
/// nodes and unary `-`. `%`, concatenation, comparisons, strings,
/// tables, and unknown calls are never total arithmetic here.
/// A key is `Some` exactly when the expression is total: name leaves
/// go through `NameOperandProof::is_total_numeric`, call leaves
/// through the pure-read / math-call keys, so a hoisted expression
/// cannot error when evaluated once before the loop, even for a
/// zero-trip loop.
fn arith_key(proof: &NameOperandProof<'_>, node: NodeId, depth: usize) -> Option<String> {
    if depth > 32 {
        return None;
    }
    match proof.source.node(node) {
        Node::Num(text) => Some(format!("N({text})")),
        Node::Paren(inner) => arith_key(proof, *inner, depth + 1),
        Node::Name(_) => {
            if !proof.is_total_numeric(node, 0) {
                return None;
            }
            let bid = proof.resolution.node_bid[node as usize]?;
            Some(format!("V({bid:?})"))
        }
        Node::Index(..) => {
            let builtin = proof.analyzer.resolve_builtin_reference(node)?;
            let b: &str = &builtin;
            if matches!(
                b,
                "math.pi" | "math.huge" | "math.maxinteger" | "math.mininteger"
            ) && !proof.mutated.contains("math")
            {
                Some(format!("C({b})"))
            } else {
                None
            }
        }
        Node::Call(function, args, method) => {
            if method.is_some() {
                return None;
            }
            let builtin = proof.analyzer.resolve_builtin_reference(*function)?;
            let b: &str = &builtin;
            match b {
                "input.getNumber" | "property.getNumber" | "screen.getWidth"
                | "screen.getHeight" => {
                    if proof.mutated.contains(pure_read_root(b)) {
                        return None;
                    }
                    pure_read_key(proof.source, b, args)
                }
                _ => math_call_key(proof, node),
            }
        }
        Node::Bin(op, left, right) if matches!(op.as_str(), "+" | "-" | "*" | "/" | "^") => {
            let l = arith_key(proof, *left, depth + 1)?;
            let r = arith_key(proof, *right, depth + 1)?;
            Some(format!("B({op},{l},{r})"))
        }
        Node::Un(op, inner) if op == "-" => {
            let inner = arith_key(proof, *inner, depth + 1)?;
            Some(format!("U(-,{inner})"))
        }
        _ => None,
    }
}

/// Whether an arithmetic candidate contains a name or call leaf.
/// All-literal (and all-math-constant) expressions are constant
/// folding's case, not a hoist: they cost a preheader local for no
/// per-iteration saving beyond what folding already removes.
fn has_dynamic_leaf(source: &Ast, node: NodeId) -> bool {
    let mut found = false;
    walk_expr(source, node, &mut |n| {
        if matches!(source.node(n), Node::Name(_) | Node::Call(..)) {
            found = true;
        }
    });
    found
}

fn is_pure_math(builtin: &str) -> bool {
    builtin.starts_with("math.") && builtin != "math.random" && builtin != "math.randomseed"
}

/// Builtins that disqualify a loop outright (stateful or opaque).
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

/// Walk a subtree, visiting every node except nodes inside nested
/// functions. Nested loops are descended into: a write or unknown call
/// inside a nested loop can still invalidate a hoist out of the
/// enclosing loop, and a pure read inside a nested loop may hoist to
/// the enclosing loop.
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
/// nested functions. Loops cannot occur inside expressions.
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

#[derive(Clone)]
struct HoistGroup {
    /// Occurrence node ids: call nodes for call candidates, Bin/Un
    /// nodes for arithmetic candidates.
    occurrences: Vec<NodeId>,
    template: NodeId,
}

/// Collect hoist candidates over the whole body subtree (nested loops
/// included): a call inside a nested loop is hoisted to this loop when
/// this loop claims it first. The transformer filters out calls already
/// claimed by an outer loop, so each call hoists at most once, to the
/// outermost qualifying loop. Calls inside a nested function may run
/// after the callback and are never collected.
///
/// A candidate call is not descended into for further collection: its
/// whole subtree (including nested pure reads inside a total math call)
/// is hoisted as one unit, so a nested read is never hoisted twice.
/// The same holds for a candidate arithmetic expression: arithmetic
/// inside it, and calls inside it, are never hoisted separately.
/// Shared inputs for candidate collection (kept as one argument so the
/// collector stays under clippy's argument-count limit).
struct LoopAnalysis<'a> {
    source: &'a Ast,
    analyzer: &'a EffectAnalyzer<'a>,
    mutated: &'a HashSet<String>,
    proof: &'a NameOperandProof<'a>,
}

fn collect_candidates(
    ctx: &LoopAnalysis<'_>,
    node: NodeId,
    is_root: bool,
    order: &mut Vec<String>,
    groups: &mut HashMap<String, HoistGroup>,
) {
    if !is_root && matches!(ctx.source.node(node), Node::Function(..)) {
        return;
    }
    if let Node::Call(function, args, _) = ctx.source.node(node) {
        if let Some(builtin) = ctx.analyzer.resolve_builtin_reference(*function) {
            let b: &str = &builtin;
            let key = pure_read_key(ctx.source, b, args)
                .filter(|_| !ctx.mutated.contains(pure_read_root(b)))
                .or_else(|| math_call_key(ctx.proof, node));
            if let Some(key) = key {
                if !groups.contains_key(&key) {
                    order.push(key.clone());
                    groups.insert(
                        key.clone(),
                        HoistGroup {
                            occurrences: Vec::new(),
                            template: node,
                        },
                    );
                }
                if let Some(group) = groups.get_mut(&key) {
                    group.occurrences.push(node);
                }
                return;
            }
        }
    }
    // Pure arithmetic over total operands hoists as one unit, claimed
    // outermost-first exactly like a call: the collector returns here
    // without descending, so arithmetic inside a hoisted expression
    // (or a call inside one) is never hoisted a second time. Paren
    // nodes are not candidates themselves; descending through them
    // lets the inner Bin/Un be claimed with the same key.
    if matches!(ctx.source.node(node), Node::Bin(..) | Node::Un(..)) {
        if let Some(key) = arith_key(ctx.proof, node, 0) {
            if has_dynamic_leaf(ctx.source, node) {
                if !groups.contains_key(&key) {
                    order.push(key.clone());
                    groups.insert(
                        key.clone(),
                        HoistGroup {
                            occurrences: Vec::new(),
                            template: node,
                        },
                    );
                }
                if let Some(group) = groups.get_mut(&key) {
                    group.occurrences.push(node);
                }
                return;
            }
        }
    }
    let mut children = Vec::new();
    ast_utils::for_each_child(ctx.source, node, &mut |child| children.push(child));
    for child in children {
        collect_candidates(ctx, child, false, order, groups);
    }
}

/// Analyse one loop. Returns `None` when the loop must not be hoisted,
/// otherwise the groups to hoist (possibly empty).
fn analyze_loop(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    mutated: &HashSet<String>,
    proof: &NameOperandProof<'_>,
    body: NodeId,
    headers: &[NodeId],
) -> Option<Vec<HoistGroup>> {
    // Disqualification scan over the whole body subtree (nested loops
    // included, nested functions excluded): unstructured flow, host-table
    // writes, unknown or method calls, stateful builtins, and host tables
    // passed as arguments all veto the hoist, because they can run during
    // the loop and change what a hoisted read would return.
    let mut disqualified = false;
    walk_full(source, body, &mut |node| {
        if disqualified {
            return;
        }
        match source.node(node) {
            Node::Goto(_) | Node::Label(_) => disqualified = true,
            Node::Assign(targets, _) => {
                for target in targets {
                    if matches!(source.node(*target), Node::Index(..) | Node::Methodname(..))
                        && analyzer.resolve_builtin_reference(*target).is_some()
                    {
                        disqualified = true;
                    }
                }
            }
            Node::Funcstat(target, _) => {
                if matches!(source.node(*target), Node::Index(..) | Node::Methodname(..))
                    && analyzer.resolve_builtin_reference(*target).is_some()
                {
                    disqualified = true;
                }
            }
            Node::Call(function, args, method) => {
                if method.is_some() {
                    disqualified = true;
                    return;
                }
                let Some(builtin) = analyzer.resolve_builtin_reference(*function) else {
                    disqualified = true;
                    return;
                };
                if is_disqualifying_builtin(&builtin) {
                    disqualified = true;
                    return;
                }
                if pure_read_key(source, &builtin, args).is_some() {
                    return;
                }
                for arg in args {
                    if let Node::Name(_) | Node::Index(..) = source.node(*arg) {
                        if let Some(passed) = analyzer.resolve_builtin_reference(*arg) {
                            if !passed.contains('.') {
                                disqualified = true;
                                return;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    });
    if disqualified {
        return None;
    }
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, HoistGroup> = HashMap::new();
    let ctx = LoopAnalysis {
        source,
        analyzer,
        mutated,
        proof,
    };
    collect_candidates(&ctx, body, true, &mut order, &mut groups);
    // A mutated root vetoes its candidates even when the disqualification
    // scan passed (the scan only sees direct host writes, while the
    // mutated set also covers aliasing writes and table passing).
    let groups: Vec<HoistGroup> = order
        .into_iter()
        .filter_map(|key| groups.remove(&key))
        .filter(|group| {
            let Node::Call(function, _, _) = source.node(group.template) else {
                // Arithmetic templates carry no call root of their own;
                // their leaves already veto mutated roots in arith_key.
                return true;
            };
            let Some(builtin) = analyzer.resolve_builtin_reference(*function) else {
                return false;
            };
            !mutated.contains(pure_read_root(&builtin))
        })
        .collect();
    // Headers may contain only candidate pure reads or pure math calls.
    for header in headers {
        let mut header_bad = false;
        walk_expr(source, *header, &mut |node| {
            if header_bad {
                return;
            }
            let Node::Call(function, args, method) = source.node(node) else {
                return;
            };
            if method.is_some() {
                header_bad = true;
                return;
            }
            let Some(builtin) = analyzer.resolve_builtin_reference(*function) else {
                header_bad = true;
                return;
            };
            if pure_read_key(source, &builtin, args).is_none() && !is_pure_math(&builtin) {
                header_bad = true;
            }
        });
        if header_bad {
            return None;
        }
    }
    Some(groups)
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

struct Transformer<'a> {
    source: &'a Ast,
    analyzer: EffectAnalyzer<'a>,
    mutated: HashSet<String>,
    resolution: &'a Resolution,
    root: NodeId,
    /// Enclosing function bodies, outermost first, each with its
    /// parameter count (the chunk root is the implicit outermost
    /// context and is not pushed).
    body_stack: Vec<(NodeId, usize)>,
    output: Ast,
    hoisted: usize,
    /// Occurrences already hoisted by an outer loop (top-down claiming,
    /// so each occurrence hoists at most once, to the outermost loop).
    claimed: HashSet<NodeId>,
    /// Memoized maximum simultaneously-active locals per enclosing
    /// function body (chunk root included), and how many locals this
    /// pass has already inserted into each function.
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
            root,
            body_stack: Vec::new(),
            output: clone_source(source),
            hoisted: 0,
            claimed: HashSet::new(),
            fn_local_max: HashMap::new(),
            fn_inserted: HashMap::new(),
        }
    }

    fn transform_block(&mut self, block: NodeId) {
        let Node::Block(statements) = self.source.node(block).clone() else {
            return;
        };
        let mut rewritten = Vec::with_capacity(statements.len());
        for statement in &statements {
            rewritten.append(&mut self.transform_statement(*statement));
        }
        if rewritten != statements {
            self.output
                .nodes
                .rewrite(block, Node::Block(rewritten), "loop-invariant-hoisting");
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
                    // context (transform_expr's Function arm cannot
                    // see the Funcstat target from inside).
                    let extra =
                        usize::from(matches!(self.source.node(target), Node::Methodname(..)));
                    self.body_stack.push((body, params.len() + extra));
                    self.transform_block(body);
                    self.body_stack.pop();
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
        // body, so it claims invariant reads in its whole subtree
        // (including nested loops) before any inner loop can claim them.
        // An inner loop later filters out already-claimed occurrences,
        // so each occurrence hoists at most once, to the outermost loop.
        // When this loop is disqualified, nothing is claimed and the
        // body transform below still lets inner loops hoist on their own.
        let groups = {
            // Ancestor bindings: the chunk block plus every enclosing
            // function body. A name inside a loop can resolve to an
            // ancestor binding only when that binding is in scope at
            // the enclosing function's definition point, which means
            // the binding statement precedes the definition in block
            // order; the function body cannot run before its
            // definition, so the binding has executed (and, by the
            // write-count filter in bindings_for_body, never changes
            // afterwards).
            let mut bindings = bindings_for_body(self.source, self.resolution, self.root);
            for (body, _) in &self.body_stack {
                bindings.extend(bindings_for_body(self.source, self.resolution, *body));
            }
            let proof = NameOperandProof {
                source: self.source,
                analyzer: &self.analyzer,
                mutated: &self.mutated,
                resolution: self.resolution,
                bindings,
            };
            analyze_loop(
                self.source,
                &self.analyzer,
                &self.mutated,
                &proof,
                body,
                headers,
            )
        };
        let mut hoists = Vec::new();
        if let Some(groups) = groups {
            for group in groups {
                let HoistGroup {
                    occurrences,
                    template,
                } = group;
                let occurrences: Vec<NodeId> = occurrences
                    .into_iter()
                    .filter(|occurrence| !self.claimed.contains(occurrence))
                    .collect();
                if occurrences.is_empty() {
                    continue;
                }
                // Local budget: the hoisted local is inserted into the
                // enclosing function. Skip this group when the function
                // is already at the active-local limit, counting locals
                // this pass already inserted into the same function --
                // otherwise the generated code exceeds the VM's
                // 200-local limit and fails to load (the member-cache
                // passes guard the same class). The accounting is the
                // shared conservative whole-function maximum.
                let (fn_body, fn_params) =
                    self.body_stack.last().copied().unwrap_or((self.root, 0));
                let fn_max = *self.fn_local_max.entry(fn_body).or_insert_with(|| {
                    super::single_member_cache::max_active_locals(self.source, fn_params, fn_body)
                });
                let inserted = self.fn_inserted.entry(fn_body).or_insert(0);
                if fn_max + *inserted + 1 > super::single_member_cache::MAX_LOCALS {
                    continue;
                }
                *inserted += 1;
                let symbol: SymbolId = self.output.strings.fresh("__licm");
                let cloned = clone_subtree(&mut self.output, self.source, template);
                let mut cloned_nodes = Vec::new();
                ast_utils::walk(&self.output, cloned, &mut |id| cloned_nodes.push(id));
                for id in cloned_nodes {
                    self.output
                        .nodes
                        .mark_synthetic(id, "loop-invariant-hoisting");
                }
                let local = self.output.push(Node::Local(vec![symbol], vec![cloned]));
                self.output
                    .nodes
                    .mark_synthetic(local, "loop-invariant-hoisting");
                for occurrence in &occurrences {
                    self.output.nodes.rewrite(
                        *occurrence,
                        Node::Name(symbol),
                        "loop-invariant-hoisting",
                    );
                    super::origins::derive(
                        &mut self.output,
                        *occurrence,
                        self.source,
                        &[*occurrence],
                        "loop-invariant-hoisting",
                    );
                    self.claimed.insert(*occurrence);
                }
                self.hoisted += occurrences.len();
                hoists.push(local);
            }
        }
        for header in headers {
            self.transform_expr(*header);
        }
        self.transform_block(body);
        hoists.push(statement);
        hoists
    }

    fn transform_expr(&mut self, node: NodeId) {
        match self.source.node(node).clone() {
            Node::Function(params, _, body) => {
                self.body_stack.push((body, params.len()));
                self.transform_block(body);
                self.body_stack.pop();
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

/// Hoist loop-invariant pure host reads. Returns `hoisted=N` in `details`
/// (0 when nothing changed, in which case `ast` is left untouched).
pub fn hoist_loop_invariant_host_reads(ast: &mut Ast, root: NodeId) -> PassResult {
    if !matches!(ast.node(root), Node::Block(_)) {
        return PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["hoisted=0".to_string()]),
        };
    }
    let source = clone_source(ast);
    let resolution = resolve(&source, root);
    let mut transformer = Transformer::new(&source, root, &resolution);
    transformer.transform_block(root);
    let hoisted = transformer.hoisted;
    if hoisted == 0 {
        return PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["hoisted=0".to_string()]),
        };
    }
    let output = transformer.output;
    let renamed = crate::scope_rename::scope_rename_fast(&output, root);
    *ast = renamed.ast;
    PassResult {
        root: renamed.root,
        saved: None,
        details: Some(vec![format!("hoisted={hoisted}")]),
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
        let result = hoist_loop_invariant_host_reads(&mut ast, root);
        let code = Printer::new(&ast, false).output(result.root);
        let hoisted = result
            .details
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.strip_prefix("hoisted="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        (code, hoisted)
    }

    #[test]
    fn hoists_single_pure_read_from_while_loop() {
        let (code, hoisted) = run(
            "function onTick() local s=0 while true do s=s+input.getNumber(1) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 1);
        assert_eq!(code.matches("input.getNumber(1)").count(), 1);
        let hoist_pos = code.find("input.getNumber(1)").expect("hoist present");
        let while_pos = code.find("while").expect("loop present");
        assert!(hoist_pos < while_pos, "hoist must precede the loop: {code}");
    }

    #[test]
    fn hoists_fornum_loop_read() {
        let (code, hoisted) =
            run("function onTick() local s=0 for i=1,10 do s=s+property.getText(\"a\") end end");
        // property.getText takes a string literal, so it is hoistable; the
        // test only asserts the hoist happened, not the renamed spelling.
        assert_eq!(hoisted, 1);
        assert_eq!(code.matches("property.getText").count(), 1);
    }

    #[test]
    fn skips_loop_with_unknown_call() {
        let (_, hoisted) = run(
            "function onTick() local s=0 while true do s=s+input.getNumber(1) foo() if s>10 then break end end end",
        );
        assert_eq!(hoisted, 0);
    }

    #[test]
    fn skips_loop_when_host_root_is_mutated() {
        let (_, hoisted) = run(
            "input.getNumber=function() return 0 end function onTick() local s=0 while true do s=s+input.getNumber(1) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 0);
    }

    #[test]
    fn hoists_nested_loop_read_to_outer_loop() {
        let (code, hoisted) = run(
            "function onTick() local s=0 while true do while true do s=s+input.getNumber(2) if s>10 then break end end if s>100 then break end end end",
        );
        assert_eq!(hoisted, 1);
        assert_eq!(code.matches("input.getNumber(2)").count(), 1);
        let hoist_pos = code.find("input.getNumber(2)").expect("hoist present");
        let outer_pos = code.find("while").expect("loop present");
        assert!(
            hoist_pos < outer_pos,
            "nested read must hoist before the outer loop: {code}"
        );
    }

    #[test]
    fn inner_loop_still_hoists_when_outer_disqualified() {
        let (code, hoisted) = run(
            "function onTick() local s=0 while true do foo() while true do s=s+input.getNumber(2) if s>10 then break end end if s>100 then break end end end",
        );
        assert_eq!(hoisted, 1);
        assert_eq!(code.matches("input.getNumber(2)").count(), 1);
        let hoist_pos = code.find("input.getNumber(2)").expect("hoist present");
        let outer_pos = code.find("while").expect("loop present");
        assert!(
            hoist_pos > outer_pos,
            "inner hoist must stay inside the outer loop: {code}"
        );
    }

    #[test]
    fn hoists_direct_and_nested_reads_to_outer_loop() {
        let (code, hoisted) = run(
            "function onTick() local s=0 while true do s=s+input.getNumber(1) while true do s=s+input.getNumber(2) if s>10 then break end end if s>100 then break end end end",
        );
        assert_eq!(hoisted, 2, "code: {code}");
        assert_eq!(
            code.matches("input.getNumber(1)").count(),
            1,
            "code: {code}"
        );
        assert_eq!(
            code.matches("input.getNumber(2)").count(),
            1,
            "code: {code}"
        );
        let outer_pos = code.find("while").expect("loop present");
        assert!(
            code.find("input.getNumber(1)").expect("hoist 1") < outer_pos,
            "code: {code}"
        );
        assert!(
            code.find("input.getNumber(2)").expect("hoist 2") < outer_pos,
            "code: {code}"
        );
    }

    #[test]
    fn hoists_nested_loop_header_read_to_outer_loop() {
        let (code, hoisted) = run(
            "function onTick() local s=0 while true do for i=1,input.getNumber(3) do s=s+i end if s>100 then break end end end",
        );
        assert_eq!(hoisted, 1, "code: {code}");
        assert_eq!(
            code.matches("input.getNumber(3)").count(),
            1,
            "code: {code}"
        );
        let hoist_pos = code.find("input.getNumber(3)").expect("hoist present");
        let outer_pos = code.find("while").expect("loop present");
        assert!(
            hoist_pos < outer_pos,
            "nested header read must hoist before the outer loop: {code}"
        );
    }

    #[test]
    fn hoists_arith_nocache_source() {
        let source = "local s=0 function onTick() local t=0 for i=1,200 do t=t+input.getNumber(1)*2+input.getNumber(2)*3+math.min(i,50)*0.5 end s=t output.setNumber(1,t) output.setBool(1,t>100) end";
        let (code, hoisted) = run(source);
        assert_eq!(hoisted, 2, "code: {code}");
        assert_eq!(code.matches("input.getNumber").count(), 2, "code: {code}");
    }

    #[test]
    fn skips_loop_with_variable_argument_read() {
        let (_, hoisted) = run(
            "function onTick() local s=0 local i=1 while true do s=s+input.getNumber(i) i=i+1 if s>10 then break end end end",
        );
        assert_eq!(hoisted, 0);
    }

    #[test]
    fn hoists_total_math_call_from_while_loop() {
        let (code, hoisted) = run(
            "function onTick() local s=0 while true do s=s+math.floor(input.getNumber(1)) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 1, "code: {code}");
        assert_eq!(code.matches("math.floor").count(), 1, "code: {code}");
        let hoist_pos = code.find("math.floor").expect("hoist present");
        let while_pos = code.find("while").expect("loop present");
        assert!(hoist_pos < while_pos, "hoist must precede the loop: {code}");
    }

    #[test]
    fn hoists_nested_total_math_call_once() {
        let (code, hoisted) = run(
            "function onTick() local s=0 while true do s=s+math.floor(math.sqrt(input.getNumber(1))) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 1, "code: {code}");
        assert_eq!(code.matches("math.floor").count(), 1, "code: {code}");
        assert_eq!(code.matches("math.sqrt").count(), 1, "code: {code}");
        assert_eq!(code.matches("input.getNumber").count(), 1, "code: {code}");
    }

    #[test]
    fn shares_one_hoist_for_repeated_total_math_call() {
        let (code, hoisted) = run(
            "function onTick() local s=0 while true do s=s+math.floor(input.getNumber(1))+math.floor(input.getNumber(1)) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 2, "code: {code}");
        assert_eq!(code.matches("math.floor").count(), 1, "code: {code}");
    }

    #[test]
    fn hoists_two_arg_total_math_call() {
        let (code, hoisted) = run(
            "function onTick() local s=0 while true do s=s+math.max(input.getNumber(1),input.getNumber(2)) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 1, "code: {code}");
        assert_eq!(code.matches("math.max").count(), 1, "code: {code}");
    }

    #[test]
    fn hoists_math_call_with_numeric_name_operand() {
        let (code, hoisted) = run(
            "function onTick() local s=0 local x=1.5 while true do s=s+math.floor(x) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 1, "code: {code}");
        assert_eq!(code.matches("math.floor").count(), 1, "code: {code}");
        let hoist_pos = code.find("math.floor").expect("hoist present");
        let while_pos = code.find("while").expect("loop present");
        assert!(hoist_pos < while_pos, "hoist must precede the loop: {code}");
    }

    #[test]
    fn skips_math_call_with_string_name_operand() {
        let (_, hoisted) = run(
            "function onTick() local s=0 local x=\"a\" while true do s=s+math.floor(x) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 0);
    }

    #[test]
    fn skips_math_call_with_reassigned_name_operand() {
        let (_, hoisted) = run(
            "function onTick() local s=0 local x=1.5 x=2.5 while true do s=s+math.floor(x) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 0);
    }

    #[test]
    fn skips_math_call_with_loop_body_name_operand() {
        let (_, hoisted) = run(
            "function onTick() local s=0 while true do local x=1.5 s=s+math.floor(x) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 0);
    }

    #[test]
    fn skips_math_call_with_parameter_operand() {
        let (_, hoisted) = run(
            "function onTick(x) local s=0 while true do s=s+math.floor(x) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 0);
    }

    #[test]
    fn skips_math_call_with_upvalue_operand() {
        let (_, hoisted) = run(
            "function onTick() local s=0 while true do s=s+math.floor(x) if s>10 then break end end end local x=1.5",
        );
        assert_eq!(hoisted, 0);
    }

    #[test]
    fn hoists_math_call_with_chunk_upvalue_operand() {
        let (code, hoisted) = run(
            "local x=1.5 function onTick() local s=0 while true do s=s+math.floor(x) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 1, "code: {code}");
        assert_eq!(code.matches("math.floor").count(), 1, "code: {code}");
    }

    #[test]
    fn hoists_math_call_with_outer_function_upvalue_operand() {
        let (code, hoisted) = run(
            "function outer() local x=1.5 local function inner() local s=0 while true do s=s+math.floor(x) if s>10 then break end end end inner() end",
        );
        assert_eq!(hoisted, 1, "code: {code}");
        assert_eq!(code.matches("math.floor").count(), 1, "code: {code}");
    }

    #[test]
    fn skips_math_call_with_reassigned_upvalue_operand() {
        let (_, hoisted) = run(
            "local x=1.5 x=2.5 function onTick() local s=0 while true do s=s+math.floor(x) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 0);
    }

    #[test]
    fn hoists_math_call_with_constant_operand() {
        let (code, hoisted) = run(
            "function onTick() local s=0 while true do s=s+math.floor(math.pi) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 1, "code: {code}");
        assert_eq!(code.matches("math.floor").count(), 1, "code: {code}");
    }

    #[test]
    fn hoists_math_call_with_huge_operand() {
        let (code, hoisted) = run(
            "function onTick() local s=0 while true do s=s+math.floor(math.huge) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 1, "code: {code}");
        assert_eq!(code.matches("math.floor").count(), 1, "code: {code}");
    }

    #[test]
    fn skips_math_call_with_constant_field_name_operand() {
        // A name bound to a builtin field (math.pi here) is provable by
        // is_total_numeric, but the loop is still not hoisted: the shared
        // mutated-root analysis records the field-bound local as an alias
        // of the math root, and passing it as a call argument marks math
        // as mutated, so the disqualification scan can no longer resolve
        // math.floor itself. This documents that analyzer limitation; it
        // is not a LICM proof failure, and fixing the shared analysis
        // is out of this pass's scope.
        let (_, hoisted) = run(
            "function onTick() local s=0 local x=math.pi while true do s=s+math.floor(x) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 0);
    }

    #[test]
    fn hoists_math_call_with_host_read_name_operand() {
        let (code, hoisted) = run(
            "function onTick() local s=0 local x=input.getNumber(1) while true do s=s+math.floor(x) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 1, "code: {code}");
        assert_eq!(code.matches("math.floor").count(), 1, "code: {code}");
    }

    #[test]
    fn hoists_math_call_with_chained_name_operand() {
        let (code, hoisted) = run(
            "function onTick() local s=0 local a=1.5 local b=math.floor(a) while true do s=s+math.sqrt(b) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 1, "code: {code}");
        assert_eq!(code.matches("math.sqrt").count(), 1, "code: {code}");
        assert_eq!(code.matches("math.floor").count(), 1, "code: {code}");
    }

    #[test]
    fn hoists_host_read_nested_in_nontotal_math_call() {
        let (code, hoisted) = run(
            "function onTick() local s=0 while true do s=s+math.floor(property.getText(\"a\")) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 1, "code: {code}");
        assert_eq!(code.matches("property.getText").count(), 1, "code: {code}");
        let hoist_pos = code.find("property.getText").expect("hoist present");
        let while_pos = code.find("while").expect("loop present");
        assert!(hoist_pos < while_pos, "code: {code}");
        assert!(
            code.find("math.floor").expect("math call present") > while_pos,
            "math call itself must stay in the loop: {code}"
        );
    }

    #[test]
    fn hoists_host_read_nested_in_multivalue_math_call() {
        let (code, hoisted) = run(
            "function onTick() local s=0 while true do s=s+math.modf(input.getNumber(1)) if s>10 then break end end end",
        );
        assert_eq!(hoisted, 1, "code: {code}");
        assert_eq!(code.matches("input.getNumber").count(), 1, "code: {code}");
        assert_eq!(code.matches("math.modf").count(), 1, "code: {code}");
        let hoist_pos = code.find("input.getNumber").expect("hoist present");
        let while_pos = code.find("while").expect("loop present");
        assert!(hoist_pos < while_pos, "code: {code}");
        assert!(
            code.find("math.modf").expect("math call present") > while_pos,
            "math call itself must stay in the loop: {code}"
        );
    }

    #[test]
    fn hoists_loop_invariant_arithmetic_from_while_loop() {
        let (code, hoisted) = run(
            "function onTick() local x=3 local s=0 while true do s=s+x*2 if s>10 then break end end end",
        );
        assert_eq!(hoisted, 1);
        let (before, after) = code.split_once("while").expect("loop present");
        assert!(before.contains('*'), "hoist must precede the loop: {code}");
        assert!(!after.contains('*'), "loop body must not recompute: {code}");
    }

    #[test]
    fn shares_one_hoist_for_repeated_loop_invariant_arithmetic() {
        let (code, hoisted) = run(
            "function onTick() local x=3 local s=0 while true do s=s+x*2+x*2 if s>10 then break end end end",
        );
        assert_eq!(hoisted, 2);
        let (before, after) = code.split_once("while").expect("loop present");
        assert_eq!(
            before.matches('*').count(),
            1,
            "one hoist definition: {code}"
        );
        assert!(!after.contains('*'), "loop body must not recompute: {code}");
    }

    #[test]
    fn hoists_arithmetic_with_host_read_operand() {
        let (code, hoisted) = run(
            "function onTick() local s=0 while true do s=s+input.getNumber(1)*2 if s>10 then break end end end",
        );
        assert_eq!(hoisted, 1);
        assert_eq!(code.matches("input.getNumber(1)").count(), 1);
        let (_, after) = code.split_once("while").expect("loop present");
        assert!(
            !after.contains("input.getNumber"),
            "read must not repeat: {code}"
        );
        assert!(!after.contains('*'), "loop body must not recompute: {code}");
    }

    #[test]
    fn hoists_arithmetic_operand_of_non_candidate_math_call() {
        let (code, hoisted) = run(
            "function onTick() local x=3 local s=0 while true do s=s+math.sin(x*2) if s>10 then break end end end",
        );
        // math.sin(x*2) is not a math-call candidate (its operand is
        // arithmetic, not a math operand key), so only the inner
        // arithmetic hoists and the sin call stays in the loop.
        assert_eq!(hoisted, 1);
        assert_eq!(code.matches("sin").count(), 1);
        let (before, after) = code.split_once("while").expect("loop present");
        assert!(before.contains('*'), "hoist must precede the loop: {code}");
        assert!(!after.contains('*'), "loop body must not recompute: {code}");
    }

    #[test]
    fn skips_arithmetic_with_loop_assigned_name_operand() {
        let (_, hoisted) = run(
            "function onTick() local x=3 local s=0 while true do x=x+1 s=s+x*2 if s>10 then break end end end",
        );
        assert_eq!(hoisted, 0);
    }

    #[test]
    fn skips_arithmetic_with_parameter_operand() {
        let (_, hoisted) = run(
            "function onTick(a) local s=0 while true do s=s+a*2 if s>10 then break end end end",
        );
        assert_eq!(hoisted, 0);
    }

    #[test]
    fn skips_hoist_when_function_at_local_limit() {
        // 195 params + the `s` local + numeric-for (variable + 3
        // control values) = 200 simultaneously-active locals: one more
        // local would make the generated function fail to load.
        let params = (1..=195)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "local function big({params}) local s = 0 for i = 1, 3 do s = s + input.getNumber(1) end return s end"
        );
        let (code, hoisted) = run(&source);
        assert_eq!(hoisted, 0);
        assert!(code.contains("input.getNumber(1)"));
    }

    #[test]
    fn hoists_when_function_one_below_local_limit() {
        // 194 params: 199 active locals, so the hoisted local is the
        // 200th and still loads.
        let params = (1..=194)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "local function big({params}) local s = 0 for i = 1, 3 do s = s + input.getNumber(1) end return s end"
        );
        let (_, hoisted) = run(&source);
        assert_eq!(hoisted, 1);
    }

    #[test]
    fn skips_hoist_when_chunk_at_local_limit() {
        // 195 chunk locals + the `s` local + numeric-for = 200 active
        // locals in the chunk function.
        let decls = (1..=195)
            .map(|i| format!("local c{i} = {i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let source = format!("{decls} local s = 0 for i = 1, 3 do s = s + input.getNumber(1) end");
        let (_, hoisted) = run(&source);
        assert_eq!(hoisted, 0);
    }

    #[test]
    fn second_hoist_group_respects_budget_filled_by_first() {
        // 194 params (199 active) with two distinct invariant reads:
        // only the first group may insert its local.
        let params = (1..=194)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "local function big({params}) local s = 0 for i = 1, 3 do s = s + input.getNumber(1) + input.getNumber(2) end return s end"
        );
        let (_, hoisted) = run(&source);
        assert_eq!(hoisted, 1);
    }

    #[test]
    fn skips_arithmetic_with_string_name_operand() {
        let (_, hoisted) = run(
            "function onTick() local t=\"x\" local s=0 while true do s=s+t*2 if s>10 then break end end end",
        );
        assert_eq!(hoisted, 0);
    }
    #[test]
    fn skips_hoist_when_method_at_local_limit_counting_self() {
        // Method with 194 explicit parameters: with the implicit
        // `self`, plus the `s` local and the numeric-for controls, the
        // function is at 200 active locals; the hoisted local would
        // be the 201st.
        let params = (1..=194)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "local obj = {{}} function obj:m({params}) local s = 0 for i = 1, 3 do s = s + input.getNumber(1) end return s end"
        );
        let (_, hoisted) = run(&source);
        assert_eq!(hoisted, 0);
    }

    #[test]
    fn hoists_when_method_one_below_local_limit_counting_self() {
        let params = (1..=193)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "local obj = {{}} function obj:m({params}) local s = 0 for i = 1, 3 do s = s + input.getNumber(1) end return s end"
        );
        let (_, hoisted) = run(&source);
        assert_eq!(hoisted, 1);
    }
}
