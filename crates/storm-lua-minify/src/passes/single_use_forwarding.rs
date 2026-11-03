//! Single-use global forwarding (`passes/global-forwarding.ts`).
//!
//! A stable global assignment is removed when all reads before its next write
//! occur in exactly one later statement and inlining the expression makes the
//! two statements shorter. Dynamic global access, dependency mutation, calls
//! between definition and use, and loop-carried writes are conservative
//! barriers. A call inside the use statement itself is a barrier for the
//! reads after it in evaluation order when it may assign the forwarded
//! global (candidate S6, w6 cycle 72).

use crate::pass::PassResult;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use storm_lua_analysis::effects::{empty_effects, EffectAnalyzer, Effects};
use storm_lua_analysis::resolver::{resolve, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, TableField};
use storm_lua_syntax::print::Printer;
use storm_lua_syntax::size::measure_size;

#[derive(Clone, Copy)]
struct Plan {
    assignment_index: usize,
    part: usize,
    use_index: usize,
    target_bid: BindingId,
    expression: NodeId,
}

/// Fast deterministic hasher for the effect memos below (same
/// multiply-xor shape as `temporary_globals`' file-local hasher; keys
/// are `NodeId`s). The memos are pure caches: they are never iterated,
/// so iteration order is unobservable by construction, and every value
/// stored is the analyzer's own freshly computed result. The state
/// is a fixed-width `u64`: the seed does not fit a 32-bit `usize`
/// (wasm32), and the 64-bit hash sequence is unchanged.
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

type EffectMemo = HashMap<NodeId, Effects, BuildHasherDefault<FastHasher>>;

fn memo_statement_effect<'m>(
    memo: &'m mut EffectMemo,
    analyzer: &EffectAnalyzer<'_>,
    statement: NodeId,
) -> &'m Effects {
    memo.entry(statement)
        .or_insert_with(|| analyzer.effects_for_statement(statement))
}

fn memo_expression_effect<'m>(
    memo: &'m mut EffectMemo,
    analyzer: &EffectAnalyzer<'_>,
    expression: NodeId,
) -> &'m Effects {
    memo.entry(expression)
        .or_insert_with(|| analyzer.effects_for_expr(expression))
}

fn node_bid(resolution: &Resolution, node: NodeId) -> Option<BindingId> {
    resolution.node_bid.get(node as usize).copied().flatten()
}

fn is_write(resolution: &Resolution, node: NodeId) -> bool {
    resolution
        .node_write
        .get(node as usize)
        .copied()
        .unwrap_or(false)
}

fn scoped_read_count(ast: &Ast, resolution: &Resolution, node: NodeId, bid: BindingId) -> usize {
    if matches!(ast.node(node), Node::Function(..)) {
        return 0;
    }
    let mut count = usize::from(
        matches!(ast.node(node), Node::Name(_))
            && node_bid(resolution, node) == Some(bid)
            && !is_write(resolution, node),
    );
    let mut children = Vec::new();
    storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
        children.push(child)
    });
    for child in children {
        count += scoped_read_count(ast, resolution, child, bid);
    }
    count
}

/// Count reads across callback/function boundaries.  The local block scan is
/// intentionally opaque to nested functions, but globals are shared state and
/// a read in `onDraw` must keep an assignment in `onTick` observable.
fn all_read_counts(ast: &Ast, resolution: &Resolution, root: NodeId) -> HashMap<BindingId, usize> {
    fn walk(
        ast: &Ast,
        resolution: &Resolution,
        node: NodeId,
        counts: &mut HashMap<BindingId, usize>,
    ) {
        if matches!(ast.node(node), Node::Name(_)) && !is_write(resolution, node) {
            if let Some(bid) = node_bid(resolution, node) {
                *counts.entry(bid).or_insert(0) += 1;
            }
        }
        let mut children = Vec::new();
        storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
            children.push(child)
        });
        for child in children {
            walk(ast, resolution, child, counts);
        }
    }
    let mut counts = HashMap::new();
    walk(ast, resolution, root, &mut counts);
    counts
}

fn read_follows_effectful_call_argument(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    statement: NodeId,
    bid: BindingId,
) -> bool {
    let Node::Callstat(call) = ast.node(statement) else {
        return false;
    };
    let Node::Call(function, arguments, _) = ast.node(*call) else {
        return false;
    };
    let mut scratch = empty_effects();
    analyzer.effects_for_expr_into(*function, &mut scratch);
    let mut prior_effectful = scratch.calls || scratch.ordered;
    for argument in arguments {
        if scoped_read_count(ast, resolution, *argument, bid) > 0 && prior_effectful {
            return true;
        }
        analyzer.effects_for_expr_into(*argument, &mut scratch);
        prior_effectful |= scratch.calls || scratch.ordered || !scratch.writes.is_empty();
    }
    false
}

/// Whether a call may assign `target_bid` when it executes. The
/// analyzer's interprocedural summary decides for direct calls to
/// chunk functions (its write set is computed by the summary
/// fixpoint and is not one of the bits the totality downgrade
/// clears); a syntactic scan of the callee body backs it up, made
/// transitive over the static call graph by
/// `body_may_write_target`. Builtin callees identified by resolution
/// write no chunk binding. Every other callee shape -- a parameter,
/// an unidentified name, a member or call-result callee -- may be
/// any function, so it is treated as a writer.
fn call_may_write_target(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    call: NodeId,
    target_bid: BindingId,
) -> bool {
    let mut visited = HashSet::new();
    call_may_write_target_inner(ast, resolution, analyzer, call, target_bid, &mut visited)
}

fn call_may_write_target_inner(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    call: NodeId,
    target_bid: BindingId,
    visited: &mut HashSet<BindingId>,
) -> bool {
    if analyzer.effects_for_expr(call).writes.contains(&target_bid) {
        return true;
    }
    let Node::Call(function, _, _) = ast.node(call) else {
        return true;
    };
    if analyzer.resolve_builtin_reference(*function).is_some() {
        return false;
    }
    match ast.node(*function) {
        Node::Function(_, _, body) => {
            // A literal callee (IIFE) has no summary; scan its body,
            // and treat any call inside the body as a writer (the
            // inner callee's writes are not summarized here).
            statement_contains_write(ast, resolution, *body, target_bid)
                || statement_contains_call(ast, *body)
        }
        Node::Name(_) => {
            if let Some(bid) = node_bid(resolution, *function) {
                if let Some(function_node) = resolution.bindings[bid as usize].function_node {
                    if let Node::Function(_, _, body) = ast.node(function_node) {
                        // The body scan is transitive over the static
                        // call graph (T03): a body that calls another
                        // chunk function inherits that function's
                        // may-write verdict, and a call through a
                        // callee the scan cannot resolve (a parameter,
                        // an unidentified name, a member or call
                        // result) may invoke any function in the
                        // program -- including one that writes the
                        // target -- so it counts as a writer. Neither
                        // the analyzer summary nor a direct-write scan
                        // sees a write reached only through such a
                        // higher-order call (`h(f)` writing `x` via
                        // `h -> g -> fn -> f`). `visited` holds the
                        // function bindings already being explored on
                        // this query, so recursive cycles terminate; a
                        // revisited function contributes no writes
                        // beyond what its outer exploration covers.
                        if !visited.insert(bid) {
                            return false;
                        }
                        return body_may_write_target(
                            ast, resolution, analyzer, *body, target_bid, visited,
                        );
                    }
                    return false;
                }
            }
            true
        }
        _ => true,
    }
}

/// The transitive body scan behind `call_may_write_target_inner`: a
/// body may write `target_bid` when it writes it directly, or when
/// any call it makes may write it. Nested function bodies are not
/// crossed -- they run only when invoked, and that invocation is
/// itself a call node seen here.
fn body_may_write_target(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    target_bid: BindingId,
    visited: &mut HashSet<BindingId>,
) -> bool {
    if matches!(ast.node(node), Node::Function(..)) {
        return false;
    }
    if matches!(ast.node(node), Node::Name(_))
        && node_bid(resolution, node) == Some(target_bid)
        && is_write(resolution, node)
    {
        return true;
    }
    if matches!(ast.node(node), Node::Call(..))
        && call_may_write_target_inner(ast, resolution, analyzer, node, target_bid, visited)
    {
        return true;
    }
    let mut children = Vec::new();
    storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
        children.push(child)
    });
    children
        .iter()
        .any(|&child| body_may_write_target(ast, resolution, analyzer, child, target_bid, visited))
}

/// True when `node` contains a call below it, not crossing a nested
/// function boundary (mirrors `statement_contains_write`'s opacity).
fn statement_contains_call(ast: &Ast, node: NodeId) -> bool {
    if matches!(ast.node(node), Node::Function(..)) {
        return false;
    }
    if matches!(ast.node(node), Node::Call(..)) {
        return true;
    }
    let mut found = false;
    storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
        found |= statement_contains_call(ast, child);
    });
    found
}

/// True when `node` contains a may-write call for `target_bid`,
/// not crossing a nested function boundary.
fn subtree_has_writer_call(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    target_bid: BindingId,
) -> bool {
    if matches!(ast.node(node), Node::Function(..)) {
        return false;
    }
    if matches!(ast.node(node), Node::Call(..))
        && call_may_write_target(ast, resolution, analyzer, node, target_bid)
    {
        return true;
    }
    let mut found = false;
    storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
        found |= subtree_has_writer_call(ast, resolution, analyzer, child, target_bid);
    });
    found
}

/// Ordered walk of one expression list: returns true when a read of
/// `target_bid` is reached while `tainted` (a may-write call has
/// already been evaluated). Children are visited in source order,
/// which is Lua's evaluation order for expression lists, binary
/// operands and call arguments; a call taints only after its own
/// function and arguments have been walked (they evaluate before
/// the call executes).
fn ordered_read_after_writer_call(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    target_bid: BindingId,
    tainted: &mut bool,
) -> bool {
    if matches!(ast.node(node), Node::Function(..)) {
        return false;
    }
    if matches!(ast.node(node), Node::Name(_)) {
        return *tainted
            && node_bid(resolution, node) == Some(target_bid)
            && !is_write(resolution, node);
    }
    let mut children = Vec::new();
    storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
        children.push(child)
    });
    for child in children {
        if ordered_read_after_writer_call(ast, resolution, analyzer, child, target_bid, tainted) {
            return true;
        }
    }
    if matches!(ast.node(node), Node::Call(..))
        && call_may_write_target(ast, resolution, analyzer, node, target_bid)
    {
        *tainted = true;
    }
    false
}

/// Candidate S6 (w6 cycle 72): the definition-to-use barrier scan
/// is statement-granular, so a call inside the use statement that
/// assigns the forwarded global was not a barrier for reads in the
/// same statement -- forwarding the definition expression into a
/// read after such a call substitutes the pre-call value where the
/// source observes the call's write. Returns true when any read of
/// `target_bid` in the use statement may follow a may-write call in
/// evaluation order.
fn read_follows_writer_call(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    statement: NodeId,
    target_bid: BindingId,
) -> bool {
    if let Node::Assign(targets, expressions) = ast.node(statement) {
        // Target index expressions and right-hand expressions all
        // evaluate before any store, and their relative order is
        // not guaranteed: a writer call on either side taints reads
        // of the target on the other side.
        let targets_have_writer = targets
            .iter()
            .any(|t| subtree_has_writer_call(ast, resolution, analyzer, *t, target_bid));
        let rhs_have_writer = expressions
            .iter()
            .any(|e| subtree_has_writer_call(ast, resolution, analyzer, *e, target_bid));
        if rhs_have_writer
            && targets
                .iter()
                .any(|t| scoped_read_count(ast, resolution, *t, target_bid) > 0)
        {
            return true;
        }
        let mut tainted = targets_have_writer;
        for expression in expressions {
            if ordered_read_after_writer_call(
                ast,
                resolution,
                analyzer,
                *expression,
                target_bid,
                &mut tainted,
            ) {
                return true;
            }
        }
        let mut tainted = rhs_have_writer;
        for target in targets {
            if ordered_read_after_writer_call(
                ast,
                resolution,
                analyzer,
                *target,
                target_bid,
                &mut tainted,
            ) {
                return true;
            }
        }
        return false;
    }
    let mut tainted = false;
    ordered_read_after_writer_call(
        ast,
        resolution,
        analyzer,
        statement,
        target_bid,
        &mut tainted,
    )
}

fn is_loop_statement(node: &Node) -> bool {
    matches!(
        node,
        Node::Fornum(..) | Node::Forin(..) | Node::While(..) | Node::Repeat(..)
    )
}

fn dynamic_globals(ast: &Ast, root: NodeId) -> bool {
    let mut dynamic = false;
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |node| {
        if dynamic {
            return;
        }
        match ast.node(node) {
            Node::Name(symbol) if ast.strings.get(*symbol) == "_ENV" => dynamic = true,
            Node::Call(function, _, _) => {
                if let Node::Name(symbol) = ast.node(*function) {
                    if matches!(
                        ast.strings.get(*symbol),
                        "rawget" | "rawset" | "load" | "loadstring"
                    ) {
                        dynamic = true;
                    }
                }
            }
            _ => {}
        }
    });
    dynamic
}

/// True when `node` contains a write to `bid` below it, not crossing a
/// nested function boundary (a write inside a nested function body does not
/// execute at this statement). Mirrors `scoped_read_count`'s opacity so the
/// two scans agree on which reads and writes belong to this statement.
fn statement_contains_write(
    ast: &Ast,
    resolution: &Resolution,
    node: NodeId,
    bid: BindingId,
) -> bool {
    if matches!(ast.node(node), Node::Function(..)) {
        return false;
    }
    if matches!(ast.node(node), Node::Name(_))
        && node_bid(resolution, node) == Some(bid)
        && is_write(resolution, node)
    {
        return true;
    }
    let mut children = Vec::new();
    storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
        children.push(child);
    });
    children
        .iter()
        .any(|&child| statement_contains_write(ast, resolution, child, bid))
}

/// True when `bid` appears as the root name of a `function a.b()` /
/// `function a:b()` definition target anywhere below `node`.
///
/// A definition target must stay rooted at a name: forwarding the
/// assigned expression into that position produces a target the printer
/// renders unparenthesized (`function 0.run()`), which no longer parses;
/// and the surviving root read would lose its definition when the
/// assignment statement is removed (candidate K).
fn uses_as_funcstat_target_root(
    ast: &Ast,
    resolution: &Resolution,
    node: NodeId,
    bid: BindingId,
) -> bool {
    let mut nodes = Vec::new();
    storm_lua_syntax::ast_utils::walk(ast, node, &mut |id| nodes.push(id));
    for id in nodes {
        let Node::Funcstat(target, _) = ast.node(id) else {
            continue;
        };
        let mut current = *target;
        loop {
            match ast.node(current) {
                Node::Index(object, _, _) => current = *object,
                Node::Methodname(object, _) => current = *object,
                _ => break,
            }
        }
        if matches!(ast.node(current), Node::Name(_)) && node_bid(resolution, current) == Some(bid)
        {
            return true;
        }
    }
    false
}

/// 式の部分木に組込み呼出しが含まれるか。関数リテラルの本体は式の
/// 評価では実行されないため降りない（IIFE は外側の Call の呼出し先が
/// 関数リテラルであり組込みではないため、別途サマリ判定に従う）。
fn expression_contains_builtin_call(
    ast: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    expression: NodeId,
) -> bool {
    let mut stack = vec![expression];
    while let Some(node) = stack.pop() {
        if matches!(ast.node(node), Node::Function(..)) {
            continue;
        }
        if let Node::Call(function, _, _) = ast.node(node) {
            if analyzer.resolve_builtin_reference(*function).is_some() {
                return true;
            }
        }
        storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
            stack.push(child);
        });
    }
    false
}

fn collect_plans(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    block: NodeId,
    root: NodeId,
    read_counts: &mut Option<HashMap<BindingId, usize>>,
) -> Vec<Plan> {
    let Node::Block(statements) = ast.node(block) else {
        return Vec::new();
    };
    let mut plans = Vec::new();
    // Effect queries repeat heavily inside this scan: the forward scan
    // for each (assignment, part) pair re-reads the same statements, and
    // the barrier re-check re-reads the scanned range once more. The
    // analyzer is shared-borrowed for the whole call and its analysis
    // state has no interior mutability, so no query result can change
    // while these memos live; they are dropped with the call and never
    // shared across analyzers or ASTs.
    let mut statement_effects_memo = EffectMemo::default();
    let mut expression_effects_memo = EffectMemo::default();
    for (from, assignment_id) in statements.iter().copied().enumerate() {
        let Node::Assign(targets, expressions) = ast.node(assignment_id) else {
            continue;
        };
        if targets.len() != expressions.len()
            || !targets
                .iter()
                .all(|target| matches!(ast.node(*target), Node::Name(_)))
        {
            continue;
        }
        // Only the write set is observed (in each part's guard below),
        // so snapshot it once per assignment instead of holding a memo
        // borrow across the per-part scans.
        let assignment_writes: Vec<BindingId> =
            memo_statement_effect(&mut statement_effects_memo, analyzer, assignment_id)
                .writes
                .clone();
        // A name targeted twice by the same assignment has no single
        // defining expression: Lua performs the stores of one
        // assignment right-to-left, so the FIRST part's value is the
        // one the binding holds afterwards, not the last part's
        // (candidate R, w9 cycle 66). Forwarding either part would
        // substitute the wrong value, so repeated targets are skipped.
        let target_bids = targets
            .iter()
            .map(|target| node_bid(resolution, *target))
            .collect::<Vec<_>>();
        for part in 0..targets.len() {
            let target = targets[part];
            let expression = expressions[part];
            let Some(target_bid) = node_bid(resolution, target) else {
                continue;
            };
            if target_bids
                .iter()
                .filter(|bid| **bid == Some(target_bid))
                .count()
                > 1
            {
                continue;
            }
            let binding = &resolution.bindings[target_bid as usize];
            let expression_effect =
                memo_expression_effect(&mut expression_effects_memo, analyzer, expression);
            if binding.kind != BindingKind::Global
                || binding.fixed
                || binding.function_node.is_some()
                || expression_effect.calls
                || expression_effect.ordered
                || !expression_effect.writes.is_empty()
                || expression_effect
                    .reads
                    .iter()
                    .any(|bid| *bid != target_bid && assignment_writes.contains(bid))
                || (expression_effect.may_throw
                    && !crate::passes::global_stores::total_numeric_expression(
                        ast, analyzer, expression,
                    ))
                // 組込み呼出しを含む式は、解析器が「投げない」とした
                // 場合でも引数の型によっては実行時に投げる
                // （`math.floor(t)` 等。解析器の may_throw は静的に
                // 証明できる場合しか立たない）。使用箇所への移動は
                // 評価時点を後ろへずらすため、全域的と証明できる式
                // 以外は組込み呼出しを含む式を転送しない（Q02）。
                // プログラム定義関数の呼出しはサマリが送出可能性を
                // 含むため、従来の may_throw 判定に委ねる。
                || (expression_contains_builtin_call(ast, analyzer, expression)
                    && !crate::passes::global_stores::total_numeric_expression(
                        ast, analyzer, expression,
                    ))
                || matches!(ast.node(expression), Node::Table(_) | Node::Function(..))
                || !expression_effect.stable
            {
                continue;
            }

            let mut use_statement = None;
            let mut segment_reads = 0usize;
            let mut multiple_use_statements = false;
            for (to, statement) in statements.iter().copied().enumerate().skip(from + 1) {
                let use_count = scoped_read_count(ast, resolution, statement, target_bid);
                // A control statement can both read and overwrite the target:
                // a guard read, then an assignment inside its body, then
                // reads after that assignment. Forwarding the definition
                // into every read of such a statement would also rewrite
                // the post-overwrite reads, which must keep the new value
                // (seen with a function-globalization slot reused for a
                // signed-factoring carrier: the carrier definition survived
                // as a dead store while its uses were rewritten to the
                // guard variable). Breaking here also keeps such a
                // statement from counting as the use statement, and keeps
                // a write-only control statement a barrier for later uses.
                // Plain Assign statements are exempt: their reads (RHS)
                // are evaluated before their writes (targets), and the
                // replacement already skips write targets.
                let is_control = matches!(
                    ast.node(statement),
                    Node::If(..)
                        | Node::Do(_)
                        | Node::While(..)
                        | Node::Repeat(..)
                        | Node::Fornum(..)
                        | Node::Forin(..)
                );
                if is_control && statement_contains_write(ast, resolution, statement, target_bid) {
                    break;
                }
                if use_count > 0 {
                    segment_reads += use_count;
                    if let Some(previous) = use_statement {
                        if previous != to {
                            multiple_use_statements = true;
                        }
                    } else {
                        use_statement = Some(to);
                    }
                }
                let effect =
                    memo_statement_effect(&mut statement_effects_memo, analyzer, statement);
                if use_count == 0 && effect.reads.contains(&target_bid) {
                    multiple_use_statements = true;
                }
                if effect.writes.contains(&target_bid) {
                    break;
                }
            }
            let Some(use_index) = use_statement else {
                continue;
            };
            if !(1..=8).contains(&segment_reads) || multiple_use_statements {
                continue;
            }
            // `scoped_read_count` deliberately stops at nested functions.  A
            // global, however, can be observed by another callback, so only
            // forward when this is the global's sole read in the whole chunk.
            let total_reads = read_counts
                .get_or_insert_with(|| all_read_counts(ast, resolution, root))
                .get(&target_bid)
                .copied()
                .unwrap_or(0);
            if total_reads != segment_reads {
                continue;
            }
            if uses_as_funcstat_target_root(ast, resolution, statements[use_index], target_bid) {
                continue;
            }
            let use_effect =
                memo_statement_effect(&mut statement_effects_memo, analyzer, statements[use_index]);
            // The read is a snapshot established by the assignment.  Moving
            // its expression into a call can place evaluation after an
            // earlier argument or inside an effectful API, changing the value
            // observed by the call (for example `output.setNumber(g(), x)`).
            if read_follows_effectful_call_argument(
                ast,
                resolution,
                analyzer,
                statements[use_index],
                target_bid,
            ) {
                continue;
            }
            // The barrier scan above is statement-granular: a call
            // inside the use statement itself that assigns the
            // forwarded global invalidates every read after it in
            // evaluation order (candidate S6).
            if read_follows_writer_call(
                ast,
                resolution,
                analyzer,
                statements[use_index],
                target_bid,
            ) {
                continue;
            }
            if is_loop_statement(ast.node(statements[use_index]))
                && use_effect.writes.contains(&target_bid)
            {
                continue;
            }
            let blocked = statements[from + 1..use_index].iter().any(|statement| {
                let effect =
                    memo_statement_effect(&mut statement_effects_memo, analyzer, *statement);
                effect.calls
                    || expression_effect
                        .reads
                        .iter()
                        .any(|bid| effect.writes.contains(bid))
            });
            if blocked {
                continue;
            }
            plans.push(Plan {
                assignment_index: from,
                part,
                use_index,
                target_bid,
                expression,
            });
        }
    }
    plans
}

fn clone_subtree(ast: &mut Ast, node: NodeId) -> NodeId {
    let origin = ast.nodes.capture_origin(node);
    let original = ast.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_subtree(ast, child)
    });
    let copied = ast.push(mapped);
    ast.nodes
        .finish_rewrite(copied, origin, "global-forwarding-copy");
    copied
}

/// Clone `expression` in place of every read of `target_bid` below `node`.
///
/// `expanding` marks a direct multi-value slot of the parent: the last
/// argument of a call, the last positional field of a constructor, the last
/// value of a return list, the last initializer of a local/assign, or the
/// last iterator expression of a generic for. Any deeper nesting (operands,
/// indexes, non-last slots, parentheses) already adjusts to one value, so
/// only the direct slot matters, and each level recomputes it for its own
/// children.
fn clone_replacing_reads(
    ast: &mut Ast,
    resolution: &Resolution,
    node: NodeId,
    target_bid: BindingId,
    expression: NodeId,
    expanding: bool,
) -> NodeId {
    let original = ast.node(node).clone();
    if matches!(original, Node::Name(_))
        && node_bid(resolution, node) == Some(target_bid)
        && !is_write(resolution, node)
    {
        let mut replacement = clone_subtree(ast, expression);
        // A forwarded call or vararg is multi-value capable, but the local
        // it replaces held exactly one adjusted value. In an expanding slot
        // the bare expression would yield more values than the original
        // (e.g. `x = pair(); count(x)` becoming `count(pair())` with three
        // arguments). Parenthesizing forces single-value adjustment, which
        // matches the local in every position; the parentheses pass keeps
        // parens around calls for exactly this reason.
        if expanding && matches!(ast.node(expression), Node::Call(..) | Node::Vararg) {
            let origin = ast.nodes.capture_origin(node);
            let paren = ast.push(Node::Paren(replacement));
            ast.nodes
                .finish_rewrite(paren, origin, "single-use-global-forwarding");
            replacement = paren;
        }
        ast.nodes
            .relate_within(replacement, node, "single-use-global-forwarding");
        return replacement;
    }
    let origin = ast.nodes.capture_origin(node);
    let rec = |ast: &mut Ast, child: NodeId, expanding: bool| {
        clone_replacing_reads(ast, resolution, child, target_bid, expression, expanding)
    };
    let mapped = match original {
        Node::Call(function, arguments, method) => {
            let last = arguments.len().saturating_sub(1);
            let function = rec(ast, function, false);
            let arguments = arguments
                .iter()
                .enumerate()
                .map(|(index, argument)| rec(ast, *argument, index == last))
                .collect();
            Node::Call(function, arguments, method)
        }
        Node::Table(fields) => {
            let last = fields.len().saturating_sub(1);
            let fields = fields
                .iter()
                .enumerate()
                .map(|(index, field)| match field {
                    TableField::Arr(value) => TableField::Arr(rec(ast, *value, index == last)),
                    TableField::Name(key, value) => TableField::Name(*key, rec(ast, *value, false)),
                    TableField::KVar(key, value) => {
                        TableField::KVar(rec(ast, *key, false), rec(ast, *value, false))
                    }
                })
                .collect();
            Node::Table(fields)
        }
        Node::Return(values) => {
            let last = values.len().saturating_sub(1);
            let values = values
                .iter()
                .enumerate()
                .map(|(index, value)| rec(ast, *value, index == last))
                .collect();
            Node::Return(values)
        }
        Node::Local(names, expressions) => {
            let last = expressions.len().saturating_sub(1);
            let expressions = expressions
                .iter()
                .enumerate()
                .map(|(index, value)| rec(ast, *value, index == last))
                .collect();
            Node::Local(names, expressions)
        }
        Node::Assign(targets, expressions) => {
            let last = expressions.len().saturating_sub(1);
            let targets = targets
                .iter()
                .map(|target| rec(ast, *target, false))
                .collect();
            let expressions = expressions
                .iter()
                .enumerate()
                .map(|(index, value)| rec(ast, *value, index == last))
                .collect();
            Node::Assign(targets, expressions)
        }
        Node::Forin(bindings, expressions, body) => {
            let last = expressions.len().saturating_sub(1);
            let expressions = expressions
                .iter()
                .enumerate()
                .map(|(index, value)| rec(ast, *value, index == last))
                .collect();
            Node::Forin(bindings, expressions, rec(ast, body, false))
        }
        other => {
            let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&other, &mut |child| {
                rec(ast, child, false)
            });
            mapped
        }
    };
    let copied = ast.push(mapped);
    ast.nodes
        .finish_rewrite(copied, origin, "single-use-global-forwarding");
    copied
}

fn statement_size(ast: &Ast, statement: NodeId) -> usize {
    Printer::new(ast, false).stat_public(statement).len()
}

// An assignment without a following statement cannot have a later use in
// this block. This is only a syntactic necessary condition, not an effect test.
fn candidate_shape(ast: &Ast, block: NodeId) -> bool {
    let Node::Block(statements) = ast.node(block) else {
        return false;
    };
    statements
        .iter()
        .take(statements.len().saturating_sub(1))
        .any(|&statement| {
            matches!(ast.node(statement),Node::Assign(targets,values)
            if !targets.is_empty() && targets.len()==values.len()
                && targets.iter().all(|&target|matches!(ast.node(target),Node::Name(_))))
        })
}

fn apply_plans(ast: &mut Ast, resolution: &Resolution, block: NodeId, plans: Vec<Plan>) -> bool {
    let Node::Block(base_statements) = ast.node(block).clone() else {
        return false;
    };

    for plan in plans {
        let assignment_id = base_statements[plan.assignment_index];
        let Node::Assign(targets, expressions) = ast.node(assignment_id).clone() else {
            continue;
        };
        let checkpoint = ast.nodes.len();
        let replaced = clone_replacing_reads(
            ast,
            resolution,
            base_statements[plan.use_index],
            plan.target_bid,
            plan.expression,
            false,
        );
        let kept = (0..targets.len())
            .filter(|index| *index != plan.part)
            .collect::<Vec<_>>();
        let replacement_assignment = if kept.is_empty() {
            None
        } else {
            Some(ast.assign(
                kept.iter().map(|index| targets[*index]).collect(),
                kept.iter().map(|index| expressions[*index]).collect(),
            ))
        };
        if let Some(replacement) = replacement_assignment {
            let origin = ast.nodes.capture_origin(assignment_id);
            ast.nodes
                .finish_rewrite(replacement, origin, "single-use-global-forwarding");
        }
        let old_cost = statement_size(ast, assignment_id)
            + statement_size(ast, base_statements[plan.use_index]);
        let new_cost = replacement_assignment.map_or(0, |statement| statement_size(ast, statement))
            + statement_size(ast, replaced);
        if new_cost < old_cost {
            let mut statements = base_statements.clone();
            statements[plan.use_index] = replaced;
            if let Some(statement) = replacement_assignment {
                statements[plan.assignment_index] = statement;
            } else {
                statements.remove(plan.assignment_index);
            }
            ast.nodes.rewrite(
                block,
                Node::Block(statements),
                "single-use-global-forwarding",
            );
            return true;
        }
        ast.nodes.truncate(checkpoint);
    }
    false
}

fn collect_blocks_postorder(
    ast: &Ast,
    node: NodeId,
    seen: &mut HashSet<NodeId>,
    output: &mut Vec<NodeId>,
) {
    if !seen.insert(node) {
        return;
    }
    let mut children = Vec::new();
    storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
        children.push(child)
    });
    for child in children {
        collect_blocks_postorder(ast, child, seen, output);
    }
    if matches!(ast.node(node), Node::Block(_)) {
        output.push(node);
    }
}

pub fn forward_single_use_globals(ast: &mut Ast, root: NodeId) -> PassResult {
    if super::has_unstructured_flow(ast, root) {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }

    let original_size = measure_size(ast, root);
    let mut forwarded = 0usize;
    for _ in 0..16 {
        if dynamic_globals(ast, root) {
            break;
        }
        let mut blocks = Vec::new();
        collect_blocks_postorder(ast, root, &mut HashSet::new(), &mut blocks);
        let mut applied = 0usize;
        let mut next = 0;
        // A run of blocks without eligible plans observes exactly the same AST.
        // Resolve/analyze it once, rather than rebuilding the whole chunk for
        // every such block. Drop the borrowed analysis before any trial mutation,
        // then rebuild it for the next block even when the trial rolled back.
        while let Some(first) = blocks[next..]
            .iter()
            .position(|&block| candidate_shape(ast, block))
        {
            next += first;
            let resolution = resolve(ast, root);
            // Chunk-wide read counts depend only on the AST and this
            // resolution, and the tree is not mutated while a scan is in
            // flight, so one lazily built map serves every plan query in
            // the scan. It is dropped before apply_plans mutates the tree.
            let mut read_counts: Option<HashMap<BindingId, usize>> = None;
            let candidate = {
                let analyzer = EffectAnalyzer::new(ast, &resolution, root, true);
                blocks[next..]
                    .iter()
                    .enumerate()
                    .find_map(|(offset, &block)| {
                        if !candidate_shape(ast, block) {
                            return None;
                        }
                        let plans = collect_plans(
                            ast,
                            &resolution,
                            &analyzer,
                            block,
                            root,
                            &mut read_counts,
                        );
                        (!plans.is_empty()).then_some((next + offset, block, plans))
                    })
            };
            let Some((index, block, plans)) = candidate else {
                break;
            };
            next = index + 1;
            if apply_plans(ast, &resolution, block, plans) {
                applied += 1;
            }
        }
        if applied == 0 {
            break;
        }
        forwarded += applied;
    }
    PassResult {
        root,
        saved: Some(original_size.saturating_sub(measure_size(ast, root)) as u64),
        details: Some(vec![format!("forwarded={forwarded}")]),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::{parse_source, Printer};

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = forward_single_use_globals(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn all_read_counts_match_per_binding_counts() {
        // Reference: the per-binding whole-chunk walk the shared map
        // replaced. Every binding's map entry must equal it exactly,
        // including reads across function boundaries and shadowed locals.
        fn reference(ast: &Ast, resolution: &Resolution, node: NodeId, bid: BindingId) -> usize {
            let mut count = usize::from(
                matches!(ast.node(node), Node::Name(_))
                    && node_bid(resolution, node) == Some(bid)
                    && !is_write(resolution, node),
            );
            let mut children = Vec::new();
            storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
                children.push(child)
            });
            for child in children {
                count += reference(ast, resolution, child, bid);
            }
            count
        }
        let source = "shared=0 function onTick() shared=shared+1 local x=shared output.setNumber(1,shared+x) end function onDraw() screen.drawText(1,1,tostring(shared)) local shared=5 output.setNumber(2,shared) end";
        let (ast, root) = parse_source(source).expect("parse");
        let resolution = resolve(&ast, root);
        let counts = all_read_counts(&ast, &resolution, root);
        for bid in 0..resolution.bindings.len() as BindingId {
            assert_eq!(
                counts.get(&bid).copied().unwrap_or(0),
                reference(&ast, &resolution, root, bid),
                "bid {bid}"
            );
        }
    }

    #[test]
    fn does_not_forward_into_funcstat_target_root() {
        // Candidate K: the base of a definition target must stay a name;
        // forwarding the value made every candidate unparseable.
        let out = output("g=0 function g.run() return 1 end");
        assert!(out.contains("function g.run()"), "{out}");
        assert!(out.contains("g=0"), "{out}");
        let out = output("g=\"\" function g.run() return 1 end");
        assert!(out.contains("function g.run()"), "{out}");
        let out = output("g=0 function g:b() return 1 end");
        assert!(out.contains("function g:b()"), "{out}");
        // A read inside the definition body is not a target root, but a
        // root read in the same statement still blocks the plan.
        let out = output("g=0 function g.run() return g end");
        assert!(out.contains("function g.run()return g"), "{out}");
    }

    #[test]
    fn does_not_forward_into_control_statement_with_nested_overwrite() {
        // A function-globalization slot reused for a signed-factoring
        // carrier: the guard read precedes an overwrite of the same
        // slot, and the draws after the overwrite must keep the new
        // value. Forwarding the pre-overwrite value into those draws
        // changed draw coordinates (6 -> 10 in the corpus case).
        let source = "function onTick()g=input.getNumber(1)if g==4 then c=input.getNumber(3)i=input.getNumber(6)*-1 end end function onDraw()if g==4 then if c>0 then __fs1,__fs0=i,c if __fs0>0 then __fs0=25+__fs1 screen.drawRect(__fs0,1,2,8)screen.drawLine(27+__fs1,18,__fs0,16)screen.drawLine(__fs0,15,__fs0,22)screen.drawLine(22+__fs1,19,__fs0,19)end end end end";
        let out = output(source);
        assert!(
            out.contains("screen.drawRect(__fs0,1,2,8)"),
            "draws must keep the overwritten carrier: {out}"
        );
        assert!(
            !out.contains("screen.drawRect(c,1,2,8)"),
            "draws must not be rewritten to the pre-overwrite value: {out}"
        );
    }

    #[test]
    fn control_statement_write_is_a_barrier_for_later_uses() {
        let source = "x=5 if input.getBool(1) then x=9 end output.setNumber(1,x)";
        let out = output(source);
        assert!(
            out.contains("output.setNumber(1,x)"),
            "a later read after a conditional overwrite must not be forwarded: {out}"
        );
        assert!(
            !out.contains("output.setNumber(1,5)"),
            "the pre-overwrite value must not reach the later read: {out}"
        );
    }

    #[test]
    fn does_not_forward_a_repeated_assignment_target() {
        // Candidate R (w9 cycle 66): Lua stores one assignment's
        // targets right-to-left, so after `T, T, G = {}, -2, "true"`
        // the binding T holds the FIRST value ({}), not -2. Neither
        // part may be forwarded into the later read.
        let out = output("T, T, G = {}, -2, \"true\" local v = (T.v or 0) output.setNumber(1, v)");
        assert!(out.contains("T.v"), "{out}");
        assert!(!out.contains("(-2).v"), "{out}");
    }

    #[test]
    fn does_not_forward_across_a_writer_call_inside_the_use_statement() {
        // Candidate S6 (w6 cycle 72): the barrier scan is
        // statement-granular, so a call inside the use statement
        // whose body assigns the forwarded global was not a barrier
        // for the reads after it in the same statement. Forwarding
        // `v = 2` into both reads yields 12 where the source reads
        // 303 (the second read observes the call's write).
        let source = "v=2 local function bump()v=99 return 0 end local s=(v*3)+bump()+(v*3)output.setNumber(1,s)";
        assert_eq!(output(source), source);
        // Only the post-call read: still unsound to forward.
        let source =
            "v=2 local function bump()v=99 return 0 end local s=bump()+(v*3)output.setNumber(1,s)";
        assert_eq!(output(source), source);
        // An unidentified callee (a parameter) may write the global.
        let source = "v=2 local function g(cb)local s=cb()+(v*3)output.setNumber(1,s)end";
        assert_eq!(output(source), source);
    }

    #[test]
    fn still_forwards_when_the_intervening_call_cannot_write_the_global() {
        // A builtin call identified by the analyzer does not write
        // chunk globals, and neither does a chunk function whose
        // summary shows no write to the forwarded global.
        let out = output("v=2 local s=math.floor(1.5)+(v*3)output.setNumber(1,s)");
        assert!(out.contains("math.floor(1.5)+(2*3)"), "{out}");
        let out = output(
            "v=2 w=0 local function bump()w=99 return 0 end local s=bump()+(v*3)output.setNumber(1,s)",
        );
        assert!(out.contains("bump()+(2*3)"), "{out}");
    }

    #[test]
    fn does_not_forward_across_higher_order_write() {
        // T03: the writer call inside the use statement reaches the
        // write through a higher-order hop: `h` calls its parameter,
        // and the argument passed here is a function that assigns the
        // forwarded global. Neither the analyzer summary nor a direct
        // body scan of `h` sees the write (the summary's write set
        // does not propagate through an unresolved callee, and `h`'s
        // body contains no direct write), so forwarding substituted
        // the stale initial value for the post-call read. The
        // may-write scan is transitive over the static call graph and
        // treats a call through an unresolved callee as a writer, so
        // the definition now survives. (The end-to-end shape -- the
        // forwarded read in the statement after a chain of chunk
        // functions `h(f) -> g(fn) -> fn() -> f` -- is covered by the
        // run8 verification driver at pipeline level: this pass alone
        // forms no plan there because the intervening definition
        // statements are effect barriers until the pipeline's earlier
        // rounds reshape them.)
        let source = "v=2 local function h(fn)return fn()end local s=h(function()v=99 return 0 end)+(v*3)output.setNumber(1,s)";
        assert_eq!(output(source), source);
        // A resolved call chain with no unresolved callee and no
        // write still forwards: transitivity alone is not a barrier.
        let out = output(
            "v=2 local function f() return 0 end local function h() return f() end local s=h()+(v*3)output.setNumber(1,s)",
        );
        assert!(out.contains("h()+(2*3)"), "{out}");
        // One named hop to a direct write is also a barrier.
        let source = "v=2 local function f()v=99 return 0 end local function h()return f()end local s=h()+(v*3)output.setNumber(1,s)";
        assert_eq!(output(source), source);
    }

    #[test]
    fn forwards_global_into_its_only_use_statement() {
        assert_eq!(
            output("sensor=0 function onTick()sensor=input.getNumber(1)output.setNumber(1,sensor+2)end"),
            "sensor=0 function onTick()output.setNumber(1,input.getNumber(1)+2)end"
        );
    }

    #[test]
    fn rejects_multiple_use_statements() {
        let source = "x=1 output.setNumber(1,x)output.setNumber(2,x)";
        assert_eq!(output(source), source);
    }

    #[test]
    fn rejects_dependency_mutation_between_definition_and_use() {
        let source =
            "function onTick()local x=input.getNumber(1)sensor=x x=2 output.setNumber(1,sensor)end";
        assert_eq!(output(source), source);
    }

    #[test]
    fn dynamic_global_access_disables_forwarding() {
        let source = "sensor=1 output.setNumber(1,sensor)rawget(_ENV,\"sensor\")";
        assert_eq!(output(source), source);
    }

    #[test]
    fn preserves_loop_carried_accumulation() {
        let source = "sum=0 for i=1,4 do sum=sum+input.getNumber(i)end output.setNumber(1,sum)";
        assert_eq!(output(source), source.replace(" then ", "then"));
    }

    #[test]
    fn forwards_call_into_expanding_position_parenthesized() {
        // A forwarded call held exactly one adjusted value as a local; in a
        // last-argument slot the bare call would expand to all its values.
        assert_eq!(
            output("function pair()return 7,8,9 end x=pair() output.setNumber(1,count(x))"),
            "function pair()return 7,8,9 end output.setNumber(1,count((pair())))"
        );
    }

    #[test]
    fn forwards_call_into_non_expanding_position_bare() {
        // A non-last argument already adjusts to one value, so no parens
        // are needed there.
        assert_eq!(
            output("function pair()return 7,8,9 end x=pair() output.setNumber(x,1)"),
            "function pair()return 7,8,9 end output.setNumber(pair(),1)"
        );
    }

    #[test]
    fn forwards_vararg_into_expanding_position_parenthesized() {
        assert_eq!(
            output("function f(...)x=... output.setNumber(1,g(x))end"),
            "function f(...)output.setNumber(1,g((...)))end"
        );
    }

    #[test]
    fn does_not_forward_possibly_throwing_builtin_call() {
        // Q02: `math.floor(t)`（t は表）は実行時に投げるが、解析器の
        // may_throw は引数が静的に非数値と証明できないため立たない。
        // 使用箇所（f() の後ろ）へ転送すると、元の定義時点より後に
        // 評価され、f の出力が先に出てしまう。転送を止め、定義文を
        // そのまま残すこと。
        let out = output(
            "t={} function f() output.setNumber(1,99) return 4 end \
             y=math.floor(t) return f()+y",
        );
        assert!(out.contains("y=math.floor(t)"), "{out}");
        let return_at = out.rfind("return").expect("return present");
        assert!(!out[return_at..].contains("math.floor"), "{out}");
        // プログラム定義関数の呼出しはサマリで送出可能性が判定できる
        // ため、従来どおり転送される（組込み呼出しだけが対象）。
        let out = output("function pair() return 7 end y=pair() output.setNumber(1,y)");
        assert!(out.contains("output.setNumber(1,(pair()))"), "{out}");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod analysis_reuse_tests {
    use super::*;
    use storm_lua_syntax::provenance::GeneratedOrigins;
    use storm_lua_syntax::{parse_source_with_origins, Printer};
    fn reference_block(ast: &mut Ast, root: NodeId, block: NodeId) -> bool {
        let resolution = resolve(ast, root);
        let plans = {
            let analyzer = EffectAnalyzer::new(ast, &resolution, root, true);
            let mut read_counts = None;
            collect_plans(ast, &resolution, &analyzer, block, root, &mut read_counts)
        };
        apply_plans(ast, &resolution, block, plans)
    }
    fn reference(ast: &mut Ast, root: NodeId) -> PassResult {
        let original_size = measure_size(ast, root);
        let mut forwarded = 0usize;
        for _ in 0..16 {
            if dynamic_globals(ast, root) {
                break;
            }
            let mut blocks = Vec::new();
            collect_blocks_postorder(ast, root, &mut HashSet::new(), &mut blocks);
            let mut applied = 0usize;
            for block in blocks {
                if reference_block(ast, root, block) {
                    applied += 1;
                }
            }
            if applied == 0 {
                break;
            }
            forwarded += applied;
        }
        PassResult {
            root,
            saved: Some(original_size.saturating_sub(measure_size(ast, root)) as u64),
            details: Some(vec![format!("forwarded={forwarded}")]),
        }
    }

    fn compare(ast: &Ast, root: NodeId) {
        let before = serde_json::to_vec(ast).unwrap();
        let mut a = ast.clone();
        let mut b = ast.clone();
        let expected = reference(&mut a, root);
        let actual = forward_single_use_globals(&mut b, root);
        assert_eq!(actual.root, expected.root);
        assert_eq!(actual.saved, expected.saved);
        assert_eq!(actual.details, expected.details);
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::to_value(&b).unwrap(),
            "node/symbol order and full provenance differ"
        );
        for newlines in [false, true] {
            let x = Printer::new(&a, newlines).output_with_positions(expected.root);
            let y = Printer::new(&b, newlines).output_with_positions(actual.root);
            assert_eq!(x.code, y.code);
            assert_eq!(
                GeneratedOrigins::from_print(&a, &x),
                GeneratedOrigins::from_print(&b, &y)
            );
        }
        assert_eq!(serde_json::to_vec(ast).unwrap(), before, "source mutated");
    }
    #[test]
    fn repeated_block_analysis_matches_the_original_on_all_pass_fixture_states() {
        for id in crate::pass_ids::OPTIMIZATION_PASS_IDS {
            let source = crate::provenance_pass_matrix::fixture(id);
            let (ast, root) = parse_source_with_origins("reference.lua", &source).unwrap();
            compare(&ast, root);
            let mut transformed = ast.clone();
            let root = crate::provenance_pass_matrix::apply(id, &mut transformed, root);
            compare(&transformed, root);
            compare(
                &storm_lua_syntax::ast_utils::clone_without_origins(&transformed),
                root,
            );
            let decoded: Ast =
                serde_json::from_slice(&serde_json::to_vec(&transformed).unwrap()).unwrap();
            compare(&decoded, root);
        }
    }
    #[test]
    fn earlier_changes_rebuild_analysis_before_later_blocks() {
        for blocks in [1, 2, 8, 33] {
            for variant in 0..5 {
                let body=(0..blocks).map(|i|match variant {
                    0=>format!("do global{i}=12345 output.setNumber(1,global{i})end "),
                    1=>format!("do __s{} ,__s{}=2,2 output.setNumber(1,__s{})end ",i*2,i*2+1,i*2+1),
                    2=>format!("do local shared{i}=input.getNumber(1) __fs{} ,__fs{}=shared{i}*2,shared{i}*2 output.setNumber(1,__fs{}) output.setNumber(2,__fs{})end ",i*2,i*2+1,i*2+1,i*2),
                    3=>format!("do global{i}=input.getNumber(1) if input.getBool(1)then global{i}=3 end output.setNumber(1,global{i})end "),
                    _=>format!("do local shadow=3 __s{} ,__s{}=shadow,shadow for j=1,3 do output.setNumber(j,__s{})end end ",i*2,i*2+1,i*2+1),
                }).collect::<String>();
                let source = format!(
                    "function onTick(){body}end function onDraw()screen.drawText(0,0,global0)end"
                );
                let (mut ast, root) = parse_source_with_origins("blocks.lua", &source).unwrap();
                compare(&ast, root);
                let second = forward_single_use_globals(&mut ast, root);
                compare(&ast, second.root);
                for node in &mut ast.nodes {
                    let _ = node;
                }
                compare(&ast, second.root);
            }
        }
    }
}
