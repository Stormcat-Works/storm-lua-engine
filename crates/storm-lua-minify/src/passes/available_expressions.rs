//! Available-expression reuse (`passes/postprocess.ts`).
//!
//! Straight-line expressions can be replaced by a shorter reaching carrier
//! when the expression is stable and none of its dependencies have been
//! invalidated. Semantic identity includes resolved BindingIds and expressions
//! that allocate fresh table/function references are never shared.

use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use storm_lua_analysis::effects::{empty_effects, mutated_builtin_roots, EffectAnalyzer, Effects};
use storm_lua_analysis::resolver::{for_each_child, resolve, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId, TableField};
use storm_lua_syntax::size::measure_expr;

use super::immutable_values::{contains_fresh_reference, expression_key, expression_key_hash};

#[derive(Clone)]
struct AvailableExpression {
    carrier: SymbolId,
    carrier_bid: BindingId,
    definition: NodeId,
    reads: Vec<BindingId>,
    pure_host: bool,
    pure_math: bool,
    /// True when the entry's value provably cannot change across any
    /// intervening statement (see `barrier_stable_pure_call`), so the
    /// entry survives a barrier when `barrier_relaxed` is set. The
    /// strict passes compute this only when the flag is set and never
    /// consult it otherwise.
    stable: bool,
    /// True when the expression dispatches through a program-defined
    /// function whose summary reads include table *contents* (Q05).
    /// Effect summaries record only binding-level reads/writes, so a
    /// field store (`t.a = ...`) between the two evaluations is
    /// invisible to the dependency check: such entries must be dropped
    /// by any statement that may store fields or call a function that
    /// may. Pure host/math entries never read Lua fields and keep
    /// this false.
    summary_call: bool,
}

/// Index of available expressions within one block, keyed by the
/// structural hash of each candidate's expression key
/// (`expression_key_hash`). Entries whose hashes collide share a
/// bucket; a bucket hit is always verified against the full
/// materialized key before it counts as a match, so collisions cost a
/// comparison, never a wrong reuse. The former `HashMap<String, _>`
/// index built the full key string for every expression the walk
/// visited (5.3M string builds per corpus); the hash index builds
/// none except on a bucket hit.
type AvailableMap =
    HashMap<u64, Vec<AvailableExpression>, std::hash::BuildHasherDefault<KeyIndexHasher>>;

/// Cheap deterministic hasher for the already well-mixed u64 index
/// keys.
#[derive(Default)]
struct KeyIndexHasher(u64);

impl std::hash::Hasher for KeyIndexHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 =
                (self.0.rotate_left(5) ^ u64::from(byte)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = (self.0.rotate_left(5) ^ value).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    fn write_usize(&mut self, value: usize) {
        self.write_u64(value as u64);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// The available entry whose full expression key equals `node`'s key,
/// if any. Verification materializes both keys and compares them, so
/// the result is exactly the entry the string-keyed map would return.
fn find_available<'m>(
    source: &Ast,
    resolution: &Resolution,
    available: &'m AvailableMap,
    node: NodeId,
) -> Option<&'m AvailableExpression> {
    let bucket = available.get(&expression_key_hash(source, resolution, node))?;
    let key = expression_key(source, resolution, node);
    bucket
        .iter()
        .find(|entry| expression_key(source, resolution, entry.definition) == key)
}

fn is_atom(node: &Node) -> bool {
    matches!(
        node,
        Node::Name(_) | Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
    )
}

fn clone_source(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn api_call_preserves_lua_values(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    statement: NodeId,
) -> bool {
    let Node::Callstat(expression) = source.node(statement) else {
        return false;
    };
    let Node::Call(function, _, _) = source.node(*expression) else {
        return false;
    };
    let Some(builtin) = analyzer.resolve_builtin_reference(*function) else {
        return false;
    };
    builtin.starts_with("output.")
        || (builtin.starts_with("screen.")
            && builtin != "screen.getWidth"
            && builtin != "screen.getHeight")
}

/// この呼出しが、プログラム定義関数のサマリ経由でテーブルの中身を
/// 読む可能性があるか（Q05）。組込みとして解決できる呼出しは Lua の
/// フィールドを読まない。それ以外の呼出し先は常に対象とする: 名前
/// 呼出しは束縛がプログラム定義関数を保持する場合に限らず、引数で
/// 渡された関数値・別名経由の束縛・未識別の大域名など、呼出し先の
/// 実体を静的に確定できない呼出しはフィールドへ触りうるものとして
/// 数える（T04: 高階呼出し経由の書込みの見逃し）。
fn call_may_read_fields(
    source: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    call: NodeId,
) -> bool {
    let Node::Call(function, _, _) = source.node(call) else {
        return false;
    };
    if analyzer.resolve_builtin_reference(*function).is_some() {
        return false;
    }
    // Program-defined functions are covered by their summaries only
    // when the callee resolves to one; every other non-builtin
    // callee -- a parameter or aliased function value, an unresolved
    // name, a function literal, a member or call result -- may run
    // arbitrary code that reads or stores fields.
    let _ = resolution;
    true
}

/// ノードの部分木（関数本体には降りない）に、フィールドを読む可能性の
/// ある呼出しが含まれるか。
fn subtree_has_field_reading_call(
    source: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
) -> bool {
    if matches!(source.node(node), Node::Function(..)) {
        return false;
    }
    if matches!(source.node(node), Node::Call(..))
        && call_may_read_fields(source, resolution, analyzer, node)
    {
        return true;
    }
    let mut found = false;
    for_each_child(source, node, &mut |child| {
        if !found && subtree_has_field_reading_call(source, resolution, analyzer, child) {
            found = true;
        }
    });
    found
}

/// 文（関数本体には降りない）に、フィールドを読む可能性のある呼出し
/// が含まれるか。文内の直接のフィールドストアとは別に、呼出し経由の
/// 書込みは文粒度のバリア位置を持たないため、出現文自身の判定に使う
/// （T04）。
pub(super) fn statement_has_field_touching_call(
    source: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
) -> bool {
    subtree_has_field_reading_call(source, resolution, analyzer, node)
}

/// 文がテーブルの中身を書き換える可能性があるか（Q05）: フィールドへの
/// ストア（Index ターゲットへの代入・関数定義文）か、フィールドを読む
/// 可能性のある呼出し（その呼出し先がストアするかもしれない）を含む。
pub(super) fn statement_may_store_fields(
    source: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
) -> bool {
    match source.node(node) {
        Node::Function(..) => return false,
        Node::Funcstat(..) => return true,
        Node::Assign(targets, _) => {
            if targets
                .iter()
                .any(|target| matches!(source.node(*target), Node::Index(..)))
            {
                return true;
            }
        }
        Node::Call(..) if call_may_read_fields(source, resolution, analyzer, node) => {
            return true;
        }
        _ => {}
    }
    let mut found = false;
    for_each_child(source, node, &mut |child| {
        if !found && statement_may_store_fields(source, resolution, analyzer, child) {
            found = true;
        }
    });
    found
}

pub(super) fn contains_mutable_index_read(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
) -> bool {
    if matches!(source.node(node), Node::Index(..))
        && analyzer.resolve_builtin_reference(node).is_none()
    {
        return true;
    }
    let mut found = false;
    storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| {
        if !found && contains_mutable_index_read(source, analyzer, child) {
            found = true;
        }
    });
    found
}

/// A pure host read, as in LICM (`loop_invariant.rs`): `input.getNumber` /
/// `input.getBool` with a numeric literal, `property.get*` with a string
/// literal, `screen.getWidth` / `screen.getHeight` with no arguments.
/// Within one callback invocation the host sets `input` / `properties` /
/// `size` before the call and never changes them during it, and these
/// calls cannot error for literal arguments, so a second identical read
/// in the same straight-line block has the same value as the first.
/// Returns the host root (`input` / `property` / `screen`).
fn pure_host_read(source: &Ast, analyzer: &EffectAnalyzer<'_>, node: NodeId) -> Option<String> {
    let Node::Call(function, arguments, method) = source.node(node) else {
        return None;
    };
    if method.is_some() {
        return None;
    }
    let builtin = analyzer.resolve_builtin_reference(*function)?;
    match builtin.as_str() {
        "input.getNumber" | "input.getBool" => {
            if arguments.len() != 1 {
                return None;
            }
            matches!(source.node(arguments[0]), Node::Num(_)).then(|| "input".to_string())
        }
        "property.getNumber" | "property.getBool" | "property.getText" => {
            if arguments.len() != 1 {
                return None;
            }
            matches!(source.node(arguments[0]), Node::Str(_)).then(|| "property".to_string())
        }
        "screen.getWidth" | "screen.getHeight" => {
            arguments.is_empty().then(|| "screen".to_string())
        }
        _ => None,
    }
}

/// Whether `node` is a pure `math.*` call usable as a CSE carrier.
/// The effect analysis only treats a subset of `math` (`floor`, `abs`,
/// ...) as pure API, so the remaining pure functions (`ceil`, `exp`,
/// `log`, `asin`, ...) are rejected by the generic call rule today.
/// Every call in the expression tree must be either a pure host read
/// (literal arguments, unmutated root) or a pure math call (unmutated
/// `math` root). Stateful functions are excluded: `random` / `randomseed`
/// read and mutate RNG state, and multi-return functions (`modf` /
/// `frexp`) are excluded because a carrier holds only the first return
/// value while a later call in last-argument position would expand to
/// both. The first call already raises any argument error, and identical
/// arguments give identical results, so reusing the carrier in the same
/// straight-line block preserves semantics.
fn pure_math_call(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    mutated: &HashSet<String>,
    node: NodeId,
) -> bool {
    match source.node(node) {
        Node::Function(..) => true,
        Node::Call(function, arguments, method) => {
            if method.is_some() {
                return false;
            }
            let Some(builtin) = analyzer.resolve_builtin_reference(*function) else {
                return false;
            };
            if let Some(root) = pure_host_read(source, analyzer, node) {
                return !mutated.contains(&root);
            }
            if !builtin.starts_with("math.") || mutated.contains("math") {
                return false;
            }
            if matches!(
                builtin.as_str(),
                "math.random" | "math.randomseed" | "math.modf" | "math.frexp"
            ) {
                return false;
            }
            arguments
                .iter()
                .all(|arg| pure_math_call(source, analyzer, mutated, *arg))
        }
        _ => {
            let mut ok = true;
            storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| {
                if ok && !pure_math_call(source, analyzer, mutated, child) {
                    ok = false;
                }
            });
            ok
        }
    }
}

/// Whether `node` itself is a pure `math.*` call (not merely an
/// expression that happens to contain no calls).
fn pure_math_root(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    mutated: &HashSet<String>,
    node: NodeId,
) -> bool {
    let Node::Call(function, _, method) = source.node(node) else {
        return false;
    };
    if method.is_some() {
        return false;
    }
    let Some(builtin) = analyzer.resolve_builtin_reference(*function) else {
        return false;
    };
    builtin.starts_with("math.")
        && !matches!(
            builtin.as_str(),
            "math.random" | "math.randomseed" | "math.modf" | "math.frexp"
        )
        && !mutated.contains("math")
        && pure_math_call(source, analyzer, mutated, node)
}

/// Whether every call executed by `statement` provably leaves host state
/// (`input` / `property` / `size`) unchanged: pure host reads, pure math
/// calls (excluding `random` / `randomseed`), `output.*` sets and
/// `screen.*` calls. Unknown calls, method calls and any other builtin
/// clear pure-host entries. Calls inside nested function bodies do not
/// execute here, so they are not walked into.
fn statement_preserves_host_state(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
) -> bool {
    match source.node(node) {
        Node::Function(..) => true,
        Node::Call(function, arguments, method) => {
            if method.is_some() {
                return false;
            }
            let Some(builtin) = analyzer.resolve_builtin_reference(*function) else {
                return false;
            };
            let ok = pure_host_read(source, analyzer, node).is_some()
                || builtin.starts_with("output.")
                || builtin.starts_with("screen.")
                || (builtin.starts_with("math.")
                    && builtin != "math.random"
                    && builtin != "math.randomseed");
            if !ok {
                return false;
            }
            arguments
                .iter()
                .all(|arg| statement_preserves_host_state(source, analyzer, *arg))
        }
        _ => {
            let mut ok = true;
            storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| {
                if ok && !statement_preserves_host_state(source, analyzer, child) {
                    ok = false;
                }
            });
            ok
        }
    }
}

/// Whether a pure call's value is provably unchanged by any intervening
/// statement, including an unknown call, so a carrier for it may survive
/// a barrier that would otherwise split the straight-line segment.
///
/// Only two shapes qualify, and both are independent of every piece of
/// state a statement could change mid-callback. First, `screen.getWidth`
/// and `screen.getHeight`: no script API changes the screen size during
/// a callback (the host sets it before `onDraw`; there is no size setter
/// in any profile, unlike `input` / `property`, which the Extended
/// profile's control namespace can change mid-callback through an
/// unknown call). Second, a pure `math.*` call whose operand tree
/// contains no names and no `input` / `property` reads (literals,
/// parenthesized / negated literals, and nested calls of the same two
/// shapes only): `math` itself is excluded from the carriers when its
/// root is mutated program-wide, and the stateful (`random` /
/// `randomseed`) and multi-value (`modf` / `frexp`) calls are excluded
/// by `pure_math_root`, so the value depends on nothing a statement
/// can change.
/// `input` / `property` reads, and math calls over names or over those
/// reads, are NOT stable: an unknown call could be a control setter
/// (Extended profile) or could reassign a name operand through a
/// captured closure, so they keep the existing barrier rule. A carrier
/// held in a global is likewise never stable: a function from outside
/// the program could reassign it, and effect writes do not record that.
/// Callers additionally require a non-global carrier binding.
fn barrier_stable_operand(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    mutated: &HashSet<String>,
    node: NodeId,
) -> bool {
    match source.node(node) {
        Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil => true,
        Node::Un(_, operand) | Node::Paren(operand) => {
            barrier_stable_operand(source, analyzer, mutated, *operand)
        }
        Node::Call(..) => barrier_stable_pure_call(source, analyzer, mutated, node),
        _ => false,
    }
}

fn barrier_stable_pure_call(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    mutated: &HashSet<String>,
    node: NodeId,
) -> bool {
    if let Some(root) = pure_host_read(source, analyzer, node) {
        return root == "screen" && !mutated.contains("screen");
    }
    if !pure_math_root(source, analyzer, mutated, node) {
        return false;
    }
    let Node::Call(_, arguments, _) = source.node(node) else {
        return false;
    };
    arguments
        .iter()
        .all(|arg| barrier_stable_operand(source, analyzer, mutated, *arg))
}

struct Transformer<'a> {
    source: &'a Ast,
    resolution: &'a Resolution,
    analyzer: EffectAnalyzer<'a>,
    output: Ast,
    reused: usize,
    mutated: HashSet<String>,
    barrier_relaxed: bool,
    expr_scratch: Effects,
}

impl<'a> Transformer<'a> {
    fn new(
        source: &'a Ast,
        root: NodeId,
        resolution: &'a Resolution,
        barrier_relaxed: bool,
    ) -> Self {
        Self {
            source,
            resolution,
            analyzer: EffectAnalyzer::new(source, resolution, root, true),
            output: clone_source(source),
            reused: 0,
            mutated: mutated_builtin_roots(source, resolution, root),
            barrier_relaxed,
            expr_scratch: empty_effects(),
        }
    }

    fn rewrite_expression(
        &mut self,
        node: NodeId,
        available: &mut AvailableMap,
        no_call_reuse: bool,
    ) {
        // A carrier holds only the first value of a call, so a call in an
        // expansion position (last element of a list that expands) or in
        // statement position (where only a call is a valid statement) must
        // never be replaced by its carrier name: the replacement would
        // truncate the call to one value, or drop the call entirely.
        let is_call = matches!(self.source.node(node), Node::Call(..));
        if !is_atom(self.source.node(node)) && !(no_call_reuse && is_call) {
            if let Some(entry) = find_available(self.source, self.resolution, available, node) {
                let carrier_len = self.source.strings.get(entry.carrier).len();
                if carrier_len < measure_expr(self.source, node) {
                    self.output.nodes.rewrite(
                        node,
                        Node::Name(entry.carrier),
                        "available-expression-use",
                    );
                    super::origins::derive(
                        &mut self.output,
                        node,
                        self.source,
                        &[node],
                        "available-expression-use",
                    );
                    self.output.nodes.relate_from_role(
                        node,
                        &self.source.nodes,
                        entry.definition,
                        "available-expression-definition",
                        storm_lua_syntax::explanation::RelationRole::Definition,
                    );
                    self.reused += 1;
                    return;
                }
            }
        }

        match self.source.node(node).clone() {
            Node::Function(_, _, body) => self.transform_block(body),
            Node::Block(_) => self.transform_block(node),
            Node::Do(body) => self.transform_block(body),
            Node::While(condition, body) => {
                // A condition is reevaluated after every iteration.  Values
                // available before the loop cannot stand in for it when the
                // body may mutate one of its dependencies.
                available.clear();
                self.rewrite_expression(condition, available, false);
                self.transform_block(body);
            }
            Node::Repeat(body, condition) => {
                self.transform_block(body);
                available.clear();
                self.rewrite_expression(condition, available, false);
            }
            Node::If(arms, else_block) => {
                for arm in arms {
                    self.rewrite_expression(arm.cond, available, false);
                    self.transform_block(arm.body);
                }
                if let Some(body) = else_block {
                    self.transform_block(body);
                }
            }
            Node::Fornum(_, start, end, step, body) => {
                available.clear();
                self.rewrite_expression(start, available, false);
                self.rewrite_expression(end, available, false);
                if let Some(step) = step {
                    self.rewrite_expression(step, available, false);
                }
                self.transform_block(body);
            }
            Node::Forin(_, expressions, body) => {
                available.clear();
                let expr_count = expressions.len();
                for (i, expression) in expressions.iter().enumerate() {
                    self.rewrite_expression(
                        *expression,
                        available,
                        i + 1 == expr_count && expr_count < 3,
                    );
                }
                self.transform_block(body);
            }
            Node::Funcstat(target, function) => {
                self.rewrite_expression(target, available, false);
                self.rewrite_expression(function, available, false);
            }
            Node::Localfunc(_, function) => self.rewrite_expression(function, available, false),
            Node::Local(names, expressions) => {
                // A Local list expands its last expression only when names
                // outnumber expressions (names == expressions truncates the
                // last call to one value, which is exactly what the carrier
                // holds, so reuse stays safe there).
                let expr_count = expressions.len();
                let expands = names.len() > expr_count;
                for (i, expression) in expressions.iter().enumerate() {
                    self.rewrite_expression(*expression, available, i + 1 == expr_count && expands);
                }
            }
            Node::Return(expressions) => {
                // A Return list always expands to the caller.
                let expr_count = expressions.len();
                for (i, expression) in expressions.iter().enumerate() {
                    self.rewrite_expression(*expression, available, i + 1 == expr_count);
                }
            }
            Node::Callstat(expression) => self.rewrite_expression(expression, available, true),
            Node::Assign(targets, expressions) => {
                let expr_count = expressions.len();
                let expands = targets.len() > expr_count;
                for target in targets {
                    self.rewrite_expression(target, available, false);
                }
                for (i, expression) in expressions.iter().enumerate() {
                    self.rewrite_expression(*expression, available, i + 1 == expr_count && expands);
                }
            }
            Node::Table(fields) => {
                let field_count = fields.len();
                for (i, field) in fields.into_iter().enumerate() {
                    match field {
                        TableField::Arr(value) => {
                            self.rewrite_expression(value, available, i + 1 == field_count)
                        }
                        TableField::Name(_, value) => {
                            self.rewrite_expression(value, available, false)
                        }
                        TableField::KVar(key, value) => {
                            self.rewrite_expression(key, available, false);
                            self.rewrite_expression(value, available, false);
                        }
                    }
                }
            }
            Node::Un(_, expression) | Node::Paren(expression) => {
                self.rewrite_expression(expression, available, false)
            }
            Node::Bin(_, left, right) | Node::Index(left, right, _) => {
                self.rewrite_expression(left, available, false);
                self.rewrite_expression(right, available, false);
            }
            Node::Call(function, arguments, _) => {
                self.rewrite_expression(function, available, false);
                let arg_count = arguments.len();
                for (i, argument) in arguments.into_iter().enumerate() {
                    self.rewrite_expression(argument, available, i + 1 == arg_count);
                }
            }
            Node::Methodname(object, _) => self.rewrite_expression(object, available, false),
            Node::Break
            | Node::Goto(_)
            | Node::Label(_)
            | Node::Nil
            | Node::Bool(_)
            | Node::Vararg
            | Node::Num(_)
            | Node::Str(_)
            | Node::Name(_) => {}
        }
    }

    fn transform_statement(&mut self, statement: NodeId, available: &mut AvailableMap) {
        match self.source.node(statement).clone() {
            // Assignment LHS is not rewritten by the TS implementation.
            Node::Assign(targets, expressions) => {
                let expr_count = expressions.len();
                let expands = targets.len() > expr_count;
                for (i, expression) in expressions.iter().enumerate() {
                    self.rewrite_expression(*expression, available, i + 1 == expr_count && expands);
                }
            }
            Node::Function(_, _, body) => self.transform_block(body),
            Node::Block(_) => self.transform_block(statement),
            Node::Do(body) => self.transform_block(body),
            Node::While(condition, body) => {
                available.clear();
                self.rewrite_expression(condition, available, false);
                self.transform_block(body);
            }
            Node::Repeat(body, condition) => {
                self.transform_block(body);
                available.clear();
                self.rewrite_expression(condition, available, false);
            }
            Node::If(arms, else_block) => {
                for arm in arms {
                    self.rewrite_expression(arm.cond, available, false);
                    self.transform_block(arm.body);
                }
                if let Some(body) = else_block {
                    self.transform_block(body);
                }
            }
            Node::Fornum(_, start, end, step, body) => {
                available.clear();
                self.rewrite_expression(start, available, false);
                self.rewrite_expression(end, available, false);
                if let Some(step) = step {
                    self.rewrite_expression(step, available, false);
                }
                self.transform_block(body);
            }
            Node::Forin(_, expressions, body) => {
                available.clear();
                let expr_count = expressions.len();
                for (i, expression) in expressions.iter().enumerate() {
                    self.rewrite_expression(
                        *expression,
                        available,
                        i + 1 == expr_count && expr_count < 3,
                    );
                }
                self.transform_block(body);
            }
            Node::Funcstat(target, function) => {
                self.rewrite_expression(target, available, false);
                self.rewrite_expression(function, available, false);
            }
            Node::Localfunc(_, function) => self.rewrite_expression(function, available, false),
            Node::Local(names, expressions) => {
                // A Local list expands its last expression only when names
                // outnumber expressions (names == expressions truncates the
                // last call to one value, which is exactly what the carrier
                // holds, so reuse stays safe there).
                let expr_count = expressions.len();
                let expands = names.len() > expr_count;
                for (i, expression) in expressions.iter().enumerate() {
                    self.rewrite_expression(*expression, available, i + 1 == expr_count && expands);
                }
            }
            Node::Return(expressions) => {
                // A Return list always expands to the caller.
                let expr_count = expressions.len();
                for (i, expression) in expressions.iter().enumerate() {
                    self.rewrite_expression(*expression, available, i + 1 == expr_count);
                }
            }
            Node::Callstat(expression) => self.rewrite_expression(expression, available, true),
            Node::Break | Node::Goto(_) | Node::Label(_) => {}
            // Expression variants only occur recursively.
            _ => self.rewrite_expression(statement, available, false),
        }
    }

    fn transform_block(&mut self, block: NodeId) {
        let Node::Block(statements) = self.source.node(block).clone() else {
            return;
        };
        let mut available = AvailableMap::default();
        let mut effect = empty_effects();

        for statement in statements {
            self.transform_statement(statement, &mut available);
            self.analyzer
                .effects_for_statement_into(statement, &mut effect);
            let unknown_call = effect.calls
                && !api_call_preserves_lua_values(self.source, &self.analyzer, statement);
            let preserves_host =
                statement_preserves_host_state(self.source, &self.analyzer, statement);
            // Q05: フィールドストアは束縛単位の effect.writes に現れない
            // ため、サマリ経由でフィールドを読むエントリはここで落とす。
            let may_store_fields =
                statement_may_store_fields(self.source, self.resolution, &self.analyzer, statement);
            let barrier_relaxed = self.barrier_relaxed;
            available.retain(|_, bucket| {
                bucket.retain(|entry| {
                    let call_ok = if entry.stable && barrier_relaxed {
                        // Screen-size and literal-only math values cannot
                        // change across the statement (see
                        // `barrier_stable_pure_call`); carrier and operand
                        // writes are still checked below.
                        true
                    } else if entry.pure_host || entry.pure_math {
                        preserves_host
                    } else {
                        !unknown_call
                    };
                    call_ok
                        && !(entry.summary_call && may_store_fields)
                        && !effect.writes.contains(&entry.carrier_bid)
                        && !entry.reads.iter().any(|bid| effect.writes.contains(bid))
                });
                !bucket.is_empty()
            });

            match self.source.node(statement) {
                Node::Assign(targets, expressions) => {
                    for (part, target) in targets.iter().copied().enumerate() {
                        if part >= expressions.len() {
                            break;
                        }
                        let Node::Name(symbol) = self.source.node(target) else {
                            continue;
                        };
                        let Some(bid) = self
                            .resolution
                            .node_bid
                            .get(target as usize)
                            .copied()
                            .flatten()
                        else {
                            continue;
                        };
                        self.add_candidate(
                            bid,
                            *symbol,
                            expressions[part],
                            expressions[part],
                            &effect.writes,
                            &mut available,
                        );
                    }
                }
                Node::Local(names, expressions) if names.len() == 1 && expressions.len() == 1 => {
                    let bids = self
                        .resolution
                        .node_bids
                        .get(statement as usize)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    if bids.len() == 1 {
                        self.add_candidate(
                            bids[0],
                            names[0],
                            expressions[0],
                            expressions[0],
                            &effect.writes,
                            &mut available,
                        );
                    }
                }
                _ => {}
            }

            if matches!(
                self.source.node(statement),
                Node::If(..)
                    | Node::While(..)
                    | Node::Repeat(..)
                    | Node::Fornum(..)
                    | Node::Forin(..)
                    | Node::Do(..)
                    // A goto breaks fall-through: definitions after it are
                    // not executed on the way to its label, so nothing may
                    // stay available across it. A label can be entered from
                    // a goto elsewhere, whose path did not execute the
                    // statements the linear walk just processed (and a
                    // backward goto re-enters it after later writes), so
                    // entries must not survive a label either.
                    | Node::Goto(..)
                    | Node::Label(_)
            ) {
                available.clear();
            }
        }
    }

    fn add_candidate(
        &mut self,
        bid: BindingId,
        symbol: SymbolId,
        source_expression: NodeId,
        output_expression: NodeId,
        statement_writes: &[BindingId],
        available: &mut AvailableMap,
    ) {
        let pure_host_root = pure_host_read(self.source, &self.analyzer, source_expression);
        let is_pure_host = match &pure_host_root {
            Some(root) => !self.mutated.contains(root),
            None => false,
        };
        let is_pure_math = pure_math_root(
            self.source,
            &self.analyzer,
            &self.mutated,
            source_expression,
        );
        self.analyzer
            .effects_for_expr_into(source_expression, &mut self.expr_scratch);
        let expression_effect = &self.expr_scratch;
        let calls_blocked = expression_effect.calls && !is_pure_host && !is_pure_math;
        let stable_blocked = !expression_effect.stable && !is_pure_host && !is_pure_math;
        if calls_blocked
            || expression_effect.ordered
            || !expression_effect.writes.is_empty()
            || stable_blocked
            || contains_fresh_reference(self.source, source_expression)
            || contains_mutable_index_read(self.source, &self.analyzer, source_expression)
            || expression_effect
                .reads
                .iter()
                .any(|read| statement_writes.contains(read))
            || is_atom(self.output.node(output_expression))
        {
            return;
        }
        // A stable carrier must be a non-global binding: only a global
        // could be reassigned by a function from outside the program,
        // whose writes effect analysis does not record. Locals,
        // parameters, and loop variables can only be written by direct
        // statements (recorded in effect writes) or by functions defined
        // in this program (recorded in their summaries), both of which
        // the retain checks below still enforce.
        let carrier_is_non_global = self
            .resolution
            .bindings
            .get(bid as usize)
            .map(|binding| binding.kind != BindingKind::Global)
            .unwrap_or(false);
        let stable = self.barrier_relaxed
            && carrier_is_non_global
            && barrier_stable_pure_call(
                self.source,
                &self.analyzer,
                &self.mutated,
                source_expression,
            );
        let hash = expression_key_hash(self.source, self.resolution, source_expression);
        if is_pure_host || is_pure_math {
            if let Some(existing) =
                find_available(self.source, self.resolution, available, source_expression)
            {
                let existing_len = self.source.strings.get(existing.carrier).len();
                if existing_len <= self.source.strings.get(symbol).len() {
                    return;
                }
            }
        }
        // A cached call result depends on the identity of every callee
        // it dispatches through, not only on the values the callee's
        // summary reads: redefining the callee binding between the two
        // evaluations changes what the same syntactic call computes.
        let mut dependency_reads = expression_effect.reads.clone();
        collect_callee_bindings(
            self.source,
            self.resolution,
            source_expression,
            &mut dependency_reads,
        );
        // Q05: サマリ経由のフィールド読みは束縛単位の依存に現れない。
        // フィールドを読む可能性のある呼出しを含み、依存束縛を持つ
        // エントリに印を付け、フィールドストアを挟む文で失効させる。
        let summary_call = !is_pure_host
            && !is_pure_math
            && !dependency_reads.is_empty()
            && subtree_has_field_reading_call(
                self.source,
                self.resolution,
                &self.analyzer,
                source_expression,
            );
        let entry = AvailableExpression {
            carrier: symbol,
            carrier_bid: bid,
            definition: source_expression,
            reads: dependency_reads,
            pure_host: is_pure_host,
            pure_math: is_pure_math,
            stable,
            summary_call,
        };
        let bucket = available.entry(hash).or_default();
        if bucket.is_empty() {
            bucket.push(entry);
        } else {
            // Preserve the string-keyed map's insert semantics: an
            // entry with the same full key is replaced; anything else
            // in the bucket (a hash collision) is kept alongside.
            let key = expression_key(self.source, self.resolution, source_expression);
            match bucket
                .iter_mut()
                .find(|slot| expression_key(self.source, self.resolution, slot.definition) == key)
            {
                Some(slot) => *slot = entry,
                None => bucket.push(entry),
            }
        }
    }
}

/// Collect the bindings a call in `node` dispatches through. A call
/// through a name denotes whatever function that binding holds when the
/// call runs; effect summaries substitute a known callee's own
/// reads/writes for the call, so the callee binding itself never appears
/// in the call's `reads`. Cached call results record these bindings as
/// dependencies so that redefining a callee invalidates the cache.
/// Nested function bodies are not entered: their calls do not run when
/// the expression itself evaluates.
fn collect_callee_bindings(
    ast: &Ast,
    resolution: &Resolution,
    node: NodeId,
    out: &mut Vec<BindingId>,
) {
    match ast.node(node) {
        Node::Function(..) => {}
        Node::Call(function, arguments, _) => {
            if matches!(ast.node(*function), Node::Name(_)) {
                if let Some(bid) = resolution
                    .node_bid
                    .get(*function as usize)
                    .copied()
                    .flatten()
                {
                    if !out.contains(&bid) {
                        out.push(bid);
                    }
                }
            }
            collect_callee_bindings(ast, resolution, *function, out);
            for argument in arguments {
                collect_callee_bindings(ast, resolution, *argument, out);
            }
        }
        Node::Bin(_, left, right) | Node::Index(left, right, _) => {
            collect_callee_bindings(ast, resolution, *left, out);
            collect_callee_bindings(ast, resolution, *right, out);
        }
        Node::Un(_, expression) | Node::Paren(expression) | Node::Methodname(expression, _) => {
            collect_callee_bindings(ast, resolution, *expression, out);
        }
        Node::Table(fields) => {
            for field in fields {
                match field {
                    TableField::Arr(value) => collect_callee_bindings(ast, resolution, *value, out),
                    TableField::Name(_, value) => {
                        collect_callee_bindings(ast, resolution, *value, out)
                    }
                    TableField::KVar(key, value) => {
                        collect_callee_bindings(ast, resolution, *key, out);
                        collect_callee_bindings(ast, resolution, *value, out);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Whether `text` parses as an integer-valued numeric literal. `input`
/// channel arguments must be integers: the host converts the Lua number
/// to i64 and errors otherwise, so a non-integer channel would make a
/// synthesized carrier error where the original call might never run.
fn integer_literal(text: &str) -> bool {
    text.parse::<f64>()
        .map(|v| v.is_finite() && v.fract() == 0.0 && v.abs() < 9223372036854775808.0)
        .unwrap_or(false)
}

/// Single-value pure math builtins usable as synthesized carriers, with
/// the same arity table as LICM's total-math hoisting. `random` /
/// `randomseed` (stateful), `modf` / `frexp` (multi-value), `ult`
/// (integer-only semantics), and `type` / `tointeger` (outside LICM's
/// total set) are excluded.
fn synth_math_arity_ok(builtin: &str, nargs: usize) -> bool {
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

/// Whether `node` is a pure call usable as a synthesized carrier, and its
/// canonical key. Host reads take literal arguments (input channels must
/// be integer-valued); math arguments must be numeric literals, unary
/// minus over numeric literals, parenthesized forms of those, or nested
/// synth-pure calls. Name operands are excluded in v1: a name could make
/// the call error, and synthesizing would evaluate the call before the
/// first occurrence's statement, moving that error earlier. Roots must
/// be unmutated program-wide, as for available-expression reuse.
fn synth_pure_call(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    mutated: &HashSet<String>,
    resolution: &Resolution,
    node: NodeId,
) -> Option<String> {
    let Node::Call(function, arguments, method) = source.node(node) else {
        return None;
    };
    if method.is_some() {
        return None;
    }
    let builtin = analyzer.resolve_builtin_reference(*function)?;
    if let Some(root) = pure_host_read(source, analyzer, node) {
        if mutated.contains(&root) {
            return None;
        }
        if root == "input" {
            let Node::Num(text) = source.node(arguments[0]) else {
                return None;
            };
            if !integer_literal(text) {
                return None;
            }
        }
        return Some(expression_key(source, resolution, node));
    }
    if !builtin.starts_with("math.") || mutated.contains("math") {
        return None;
    }
    if !synth_math_arity_ok(&builtin, arguments.len()) {
        return None;
    }
    for arg in arguments {
        if !synth_pure_operand(source, analyzer, mutated, resolution, *arg) {
            return None;
        }
    }
    Some(expression_key(source, resolution, node))
}

/// Whether `node` is a total numeric operand for a synthesized math
/// carrier: a numeric literal, unary minus over one, a parenthesized
/// form, or a nested synth-pure call.
fn synth_pure_operand(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    mutated: &HashSet<String>,
    resolution: &Resolution,
    node: NodeId,
) -> bool {
    match source.node(node) {
        Node::Num(_) => true,
        Node::Un(op, operand) => {
            (op == "-" || op == "+")
                && synth_pure_operand(source, analyzer, mutated, resolution, *operand)
        }
        Node::Paren(inner) => synth_pure_operand(source, analyzer, mutated, resolution, *inner),
        Node::Call(..) => synth_pure_call(source, analyzer, mutated, resolution, node).is_some(),
        _ => false,
    }
}

/// Clone a subtree from `source` into `target` with fresh node ids.
fn clone_subtree(target: &mut Ast, source: &Ast, node: NodeId) -> NodeId {
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_subtree(target, source, child)
    });
    let copied = target.push(mapped);
    target
        .nodes
        .derive_from_clone(copied, &source.nodes, node, "pure-call-carrier-copy");
    copied
}

struct SynthGroup {
    template: NodeId,
    first_stmt: Option<usize>,
    occurrences: Vec<NodeId>,
    /// True when the template call is barrier-stable (see
    /// `barrier_stable_pure_call`): the group then survives a
    /// non-preserving statement when `barrier_relaxed` is set.
    stable: bool,
}

/// Characters saved by api-aliasing inside a template tree: a host
/// function reference `<root>.<method>` becomes a one-character alias
/// (saving `len(root) + len(method)`), and the `math` table becomes a
/// one-character alias with the method kept (saving 3). Nested calls
/// save independently, so the total is summed over the tree. A
/// one-character alias is the shortest possible, so this understates
/// the saving when a longer alias is assigned and the gate stays
/// conservative; when aliasing does not apply at all, calls stay longer
/// and synthesis only saves more.
fn alias_saving(source: &Ast, analyzer: &EffectAnalyzer<'_>, node: NodeId) -> i64 {
    let mut saving = 0i64;
    if let Node::Call(function, _, method) = source.node(node) {
        if method.is_none() {
            if let Some(builtin) = analyzer.resolve_builtin_reference(*function) {
                if let Some((root, method)) = builtin.split_once('.') {
                    match root {
                        "input" | "screen" | "property" => {
                            saving += root.len() as i64 + method.len() as i64;
                        }
                        "math" => {
                            saving += 3;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    if !matches!(source.node(node), Node::Function(..)) {
        for_each_child(source, node, &mut |child| {
            saving += alias_saving(source, analyzer, child);
        });
    }
    saving
}

struct SynthTransformer<'a> {
    source: &'a Ast,
    resolution: &'a Resolution,
    analyzer: EffectAnalyzer<'a>,
    mutated: HashSet<String>,
    output: Ast,
    synthesized: usize,
    allow_positive_net: bool,
    barrier_relaxed: bool,
    /// Chunk root, the implicit outermost function context.
    root: NodeId,
    /// Enclosing function bodies, outermost first, each with its
    /// parameter count (the chunk root is not pushed).
    fn_stack: Vec<(NodeId, usize)>,
    /// Memoized maximum simultaneously-active locals per enclosing
    /// function body (chunk root included), and how many locals this
    /// pass has already inserted into each function.
    fn_local_max: HashMap<NodeId, usize>,
    fn_inserted: HashMap<NodeId, usize>,
}

impl<'a> SynthTransformer<'a> {
    fn new(
        source: &'a Ast,
        root: NodeId,
        resolution: &'a Resolution,
        allow_positive_net: bool,
        barrier_relaxed: bool,
    ) -> Self {
        Self {
            source,
            resolution,
            analyzer: EffectAnalyzer::new(source, resolution, root, true),
            mutated: mutated_builtin_roots(source, resolution, root),
            output: clone_source(source),
            synthesized: 0,
            allow_positive_net,
            barrier_relaxed,
            root,
            fn_stack: Vec::new(),
            fn_local_max: HashMap::new(),
            fn_inserted: HashMap::new(),
        }
    }

    /// Collect synth-pure calls in `node` into `groups`, without
    /// descending into a candidate call (the outer tree claims its inner
    /// reads) or into function bodies (a body is processed as its own
    /// block when reached through `process_block` recursion).
    fn collect_expr(
        &mut self,
        node: NodeId,
        groups: &mut Vec<SynthGroup>,
        index: &mut HashMap<String, usize>,
        stmt_idx: usize,
    ) {
        if let Node::Function(params, _, body) = self.source.node(node).clone() {
            self.process_function_body(&params, body);
            return;
        }
        if let Some(key) = synth_pure_call(
            self.source,
            &self.analyzer,
            &self.mutated,
            self.resolution,
            node,
        ) {
            let group_idx = match index.get(&key) {
                Some(idx) => *idx,
                None => {
                    let idx = groups.len();
                    index.insert(key.clone(), idx);
                    let stable = self.barrier_relaxed
                        && barrier_stable_pure_call(
                            self.source,
                            &self.analyzer,
                            &self.mutated,
                            node,
                        );
                    groups.push(SynthGroup {
                        template: node,
                        first_stmt: None,
                        occurrences: Vec::new(),
                        stable,
                    });
                    idx
                }
            };
            let group = &mut groups[group_idx];
            if group.first_stmt.is_none() {
                group.first_stmt = Some(stmt_idx);
            }
            group.occurrences.push(node);
            return;
        }
        let mut children = Vec::new();
        for_each_child(self.source, node, &mut |child| children.push(child));
        for child in children {
            self.collect_expr(child, groups, index, stmt_idx);
        }
    }

    /// Expression nodes of a straight-line statement to collect from:
    /// assignment targets and values, local initializers, return values,
    /// call statements, and function declarations (whose bodies are
    /// processed separately through `collect_expr`).
    fn stmt_expressions(&self, statement: NodeId) -> Vec<NodeId> {
        match self.source.node(statement).clone() {
            Node::Assign(targets, expressions) => {
                let mut out = targets;
                out.extend(expressions);
                out
            }
            Node::Local(_, expressions) | Node::Return(expressions) => expressions,
            Node::Callstat(expression) => vec![expression],
            Node::Funcstat(target, function) => vec![target, function],
            Node::Localfunc(_, function) => vec![function],
            _ => Vec::new(),
        }
    }

    /// Whether `statement` ends the current straight-line segment:
    /// control-flow statements and scoped blocks are processed as their
    /// own blocks, and their headers and conditions are never collected
    /// (headers re-evaluate; conditions belong to a different segment).
    fn is_segment_break(&self, statement: NodeId) -> bool {
        matches!(
            self.source.node(statement),
            Node::Block(_)
                | Node::Do(_)
                | Node::While(..)
                | Node::Repeat(..)
                | Node::If(..)
                | Node::Fornum(..)
                | Node::Forin(..)
        )
    }

    /// Process every function body nested in `node` (used for loop
    /// headers and conditions, which are never collected themselves,
    /// and for statements of blocks skipped for `goto` / labels).
    /// Process a function body with its (body, parameter count)
    /// pushed as the current local-budget context.
    fn process_function_body(&mut self, params: &[SymbolId], body: NodeId) {
        self.process_function_body_with_extra(params, body, 0);
    }

    /// Like `process_function_body`, but adds `extra` implicit
    /// parameters to the budget context (a method definition's
    /// `self`, which is a real parameter at load time but absent from
    /// the Function node's parameter list).
    fn process_function_body_with_extra(
        &mut self,
        params: &[SymbolId],
        body: NodeId,
        extra: usize,
    ) {
        self.fn_stack.push((body, params.len() + extra));
        self.process_block(body);
        self.fn_stack.pop();
    }

    /// Process a Funcstat statement's function with its true
    /// parameter count, collecting the target expression into the
    /// current segment like the generic statement path would.
    fn collect_funcstat(
        &mut self,
        target: NodeId,
        function: NodeId,
        groups: &mut Vec<SynthGroup>,
        index: &mut HashMap<String, usize>,
        stmt_idx: usize,
    ) {
        self.collect_expr(target, groups, index, stmt_idx);
        if let Node::Function(params, _, body) = self.source.node(function).clone() {
            let extra = usize::from(matches!(self.source.node(target), Node::Methodname(..)));
            self.process_function_body_with_extra(&params, body, extra);
        } else {
            self.collect_expr(function, groups, index, stmt_idx);
        }
    }

    fn recurse_functions(&mut self, node: NodeId) {
        if let Node::Function(params, _, body) = self.source.node(node).clone() {
            self.process_function_body(&params, body);
            return;
        }
        let mut children = Vec::new();
        for_each_child(self.source, node, &mut |child| children.push(child));
        for child in children {
            self.recurse_functions(child);
        }
    }

    /// Recurse into the sub-blocks of a segment-breaking statement, and
    /// into function bodies nested in its headers and conditions.
    fn recurse_break(&mut self, statement: NodeId) {
        match self.source.node(statement).clone() {
            Node::Block(_) => self.process_block(statement),
            Node::Do(body) => self.process_block(body),
            Node::While(condition, body) => {
                self.recurse_functions(condition);
                self.process_block(body);
            }
            Node::Repeat(body, condition) => {
                self.process_block(body);
                self.recurse_functions(condition);
            }
            Node::If(arms, otherwise) => {
                for arm in arms {
                    self.recurse_functions(arm.cond);
                    self.process_block(arm.body);
                }
                if let Some(body) = otherwise {
                    self.process_block(body);
                }
            }
            Node::Fornum(_, start, end, step, body) => {
                self.recurse_functions(start);
                self.recurse_functions(end);
                if let Some(step) = step {
                    self.recurse_functions(step);
                }
                self.process_block(body);
            }
            Node::Forin(_, expressions, body) => {
                for expression in expressions {
                    self.recurse_functions(expression);
                }
                self.process_block(body);
            }
            _ => {}
        }
    }

    /// Synthesize carriers for every valid group of this segment,
    /// appending insertions to the block's accumulated list (the block
    /// node itself is rewritten once, at the end of `process_block`,
    /// so earlier segments' insertions are never overwritten). A group
    /// qualifies with at least two occurrences in the segment and a
    /// non-positive net character delta in the winning (aliased and
    /// renamed) form. This pass runs before api-alias and
    /// scope-renaming, so the gate must not use the pre-alias key
    /// length: aliasing shrinks `screen.getHeight()` to a 3-character
    /// call, which turns a pre-alias win into a post-alias loss. The
    /// effective key is therefore the pre-alias length minus
    /// `alias_saving`, the carrier name is modeled as one character
    /// (the renamed form), and the gate additionally absorbs a
    /// two-character renamed name (shifting the net by `1 + n`) plus
    /// the one-char `;` separator Lua needs when the following
    /// statement starts with `(`: it requires
    /// (key + 8) - n * (key - 1) + (1 + n) <= 0.
    fn flush(&mut self, groups: &[SynthGroup], insertions: &mut Vec<(usize, NodeId)>) {
        for group in groups {
            let Some(first_stmt) = group.first_stmt else {
                continue;
            };
            if group.occurrences.len() < 2 {
                continue;
            }
            let key_len = measure_expr(self.source, group.template) as i64
                - alias_saving(self.source, &self.analyzer, group.template);
            let occurrences = group.occurrences.len() as i64;
            let net = (key_len + 8) - occurrences * (key_len - 1) + (1 + occurrences);
            if net > 0 && !self.allow_positive_net {
                continue;
            }
            // Local budget: the carrier is a new local in the
            // enclosing function. Skip this group when the function
            // is already at the active-local limit, counting carriers
            // this pass already inserted into the same function --
            // otherwise the generated code exceeds the VM's 200-local
            // limit and fails to load (the member-cache passes guard
            // the same class). The accounting is the shared
            // conservative whole-function maximum.
            let (fn_body, fn_params) = self.fn_stack.last().copied().unwrap_or((self.root, 0));
            let fn_max = *self.fn_local_max.entry(fn_body).or_insert_with(|| {
                super::single_member_cache::max_active_locals(self.source, fn_params, fn_body)
            });
            let inserted = self.fn_inserted.entry(fn_body).or_insert(0);
            if fn_max + *inserted + 1 > super::single_member_cache::MAX_LOCALS {
                continue;
            }
            *inserted += 1;
            let symbol = self.output.strings.fresh("__cse");
            let cloned = clone_subtree(&mut self.output, self.source, group.template);
            let mut cloned_nodes = Vec::new();
            storm_lua_syntax::ast_utils::walk(&self.output, cloned, &mut |id| {
                cloned_nodes.push(id)
            });
            for id in cloned_nodes {
                self.output
                    .nodes
                    .mark_synthetic(id, "pure-call-carrier-synthesis");
            }
            let local = self.output.push(Node::Local(vec![symbol], vec![cloned]));
            self.output
                .nodes
                .mark_synthetic(local, "pure-call-carrier-synthesis");
            for occurrence in &group.occurrences {
                self.output
                    .nodes
                    .rewrite(*occurrence, Node::Name(symbol), "pure-call-carrier-use");
                super::origins::derive(
                    &mut self.output,
                    *occurrence,
                    self.source,
                    &[*occurrence],
                    "pure-call-carrier-use",
                );
            }
            self.synthesized += 1;
            insertions.push((first_stmt, local));
        }
    }

    fn process_block(&mut self, block: NodeId) {
        let Node::Block(statements) = self.source.node(block).clone() else {
            return;
        };
        // v1 scope guard: a `goto` jumping over a synthesized definition
        // would land inside the new local's scope and invalidate the
        // output, so blocks mentioning `goto` or labels are left alone
        // (their nested blocks are still processed).
        let has_jump = statements
            .iter()
            .any(|s| matches!(self.source.node(*s), Node::Goto(_) | Node::Label(_)));
        let mut groups: Vec<SynthGroup> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut insertions: Vec<(usize, NodeId)> = Vec::new();
        for (stmt_idx, statement) in statements.iter().enumerate() {
            let statement = *statement;
            if self.is_segment_break(statement) {
                if !has_jump {
                    self.flush(&groups, &mut insertions);
                }
                groups.clear();
                index.clear();
                self.recurse_break(statement);
                continue;
            }
            if !has_jump {
                if let Node::Funcstat(target, function) = self.source.node(statement).clone() {
                    self.collect_funcstat(target, function, &mut groups, &mut index, stmt_idx);
                } else {
                    let exprs = self.stmt_expressions(statement);
                    for expr in exprs {
                        self.collect_expr(expr, &mut groups, &mut index, stmt_idx);
                    }
                }
                // Barrier update, in the same order as available-expression
                // reuse: a statement that does not preserve host state
                // splits the segment, so later occurrences start a fresh
                // group instead of joining the earlier one.
                if !statement_preserves_host_state(self.source, &self.analyzer, statement) {
                    for group in groups.iter_mut() {
                        if !group.stable {
                            group.occurrences.clear();
                            group.first_stmt = None;
                        }
                    }
                }
            } else {
                // Still recurse into nested function bodies for their
                // own blocks, without collecting into this block.
                if let Node::Funcstat(target, function) = self.source.node(statement).clone() {
                    self.recurse_functions(target);
                    if let Node::Function(params, _, body) = self.source.node(function).clone() {
                        let extra =
                            usize::from(matches!(self.source.node(target), Node::Methodname(..)));
                        self.process_function_body_with_extra(&params, body, extra);
                    } else {
                        self.recurse_functions(function);
                    }
                } else {
                    let exprs = self.stmt_expressions(statement);
                    for expr in exprs {
                        self.recurse_functions(expr);
                    }
                }
            }
        }
        if !has_jump {
            self.flush(&groups, &mut insertions);
        }
        if !insertions.is_empty() {
            let mut rewritten = Vec::with_capacity(statements.len() + insertions.len());
            for (idx, statement) in statements.iter().enumerate() {
                for (at, local) in &insertions {
                    if *at == idx {
                        rewritten.push(*local);
                    }
                }
                rewritten.push(*statement);
            }
            self.output
                .nodes
                .rewrite(block, Node::Block(rewritten), "pure-call-carrier-synthesis");
        }
    }
}

/// Synthesize carriers for repeated pure calls in straight-line code.
///
/// Available-expression reuse only reuses a local that already exists.
/// When the same pure host read or pure math call (literal-only operands,
/// see `synth_pure_call`) repeats in one straight-line segment with no
/// existing carrier, this pass inserts one fresh local before the
/// statement of the first occurrence and replaces every occurrence with
/// it. Both modes benefit: calls and characters both decrease, so no
/// target-size restriction applies. The segment and barrier rules match
/// available-expression reuse; loop headers and conditions are never
/// collected, and blocks mentioning `goto` or labels are skipped.
pub fn synthesize_pure_call_carriers(ast: &mut Ast, root: NodeId) -> PassResult {
    let source = clone_source(ast);
    let resolution = resolve(&source, root);
    let mut transformer = SynthTransformer::new(&source, root, &resolution, false, false);
    transformer.process_block(root);
    let synthesized = transformer.synthesized;
    *ast = transformer.output;
    PassResult {
        root,
        saved: None,
        details: Some(vec![format!("synthesized={synthesized}")]),
    }
}

/// Strict-gate, barrier-relaxed variant of `synthesize_pure_call_carriers`:
/// the character gate stays strict (a group is synthesized only when its
/// modeled net character delta is non-positive), but barrier-stable groups
/// survive a non-preserving statement, including an unknown call. This
/// entry must only run on a scratch clone that becomes an *additional*
/// layout candidate in `search.rs`, never as a replacement of the phase
/// AST: cycles 42/44 measured that applying the same relaxation in place
/// moves the size-minimal winner by +14 characters on two corpus files
/// (and -6 on one), because the new carrier perturbs downstream alias
/// selection. As an additional candidate the size-minimal selection can
/// only adopt it when it wins, so the winner size never regresses.
pub fn synthesize_pure_call_carriers_barrier_relaxed(ast: &mut Ast, root: NodeId) -> PassResult {
    let source = clone_source(ast);
    let resolution = resolve(&source, root);
    let mut transformer = SynthTransformer::new(&source, root, &resolution, false, true);
    transformer.process_block(root);
    let synthesized = transformer.synthesized;
    *ast = transformer.output;
    PassResult {
        root,
        saved: None,
        details: Some(vec![format!("synthesized={synthesized}")]),
    }
}

/// Target-size only variant of `synthesize_pure_call_carriers`: the
/// character gate is relaxed, so a group is synthesized even when its
/// net character delta is positive. The semantics are unchanged (same
/// candidate set, same segment and barrier rules, literal-only
/// operands); only profitability changes. This is safe only where the
/// winner selection can still reject an over-target result, i.e.
/// inside the target-size layouts in `search.rs`, which keep the
/// strict-gate candidates alongside. It must never run on the
/// target-free path or as a phase pass: without a target budget the
/// positive delta would be pure loss.
pub fn synthesize_pure_call_carriers_relaxed(ast: &mut Ast, root: NodeId) -> PassResult {
    let source = clone_source(ast);
    let resolution = resolve(&source, root);
    let mut transformer = SynthTransformer::new(&source, root, &resolution, true, false);
    transformer.process_block(root);
    let synthesized = transformer.synthesized;
    *ast = transformer.output;
    PassResult {
        root,
        saved: None,
        details: Some(vec![format!("synthesized={synthesized}")]),
    }
}

/// Target-size only variant combining `synthesize_pure_call_carriers_relaxed`
/// (positive character delta allowed) with barrier relaxation: groups
/// whose call is barrier-stable (screen size, or literal-only pure math;
/// see `barrier_stable_pure_call`) survive a non-preserving statement,
/// including an unknown call. Same safety scope as the relaxed variant:
/// only inside the target-size layouts in `search.rs`, which keep the
/// strict-gate candidates alongside. It must never run on the
/// target-free path or as a phase pass: without a target budget the
/// barrier relaxation could change the size-minimal winner (cycle 42
/// measured +22 characters of alias churn from the same relaxation
/// applied target-free).
pub fn synthesize_pure_call_carriers_relaxed_barrier(ast: &mut Ast, root: NodeId) -> PassResult {
    let source = clone_source(ast);
    let resolution = resolve(&source, root);
    let mut transformer = SynthTransformer::new(&source, root, &resolution, true, true);
    transformer.process_block(root);
    let synthesized = transformer.synthesized;
    *ast = transformer.output;
    PassResult {
        root,
        saved: None,
        details: Some(vec![format!("synthesized={synthesized}")]),
    }
}

/// Barrier-relaxed variant of `reuse_available_expressions`: carriers
/// for barrier-stable pure calls survive an intervening statement that
/// does not preserve host state, including an unknown call, because
/// their value cannot change across it. Other carriers keep the
/// existing barrier rule. Since cycle 44 the strict entry applies the
/// same relaxation, so this entry is equivalent to it and is kept so
/// the target-size layouts keep their explicit form. (Synthesis
/// relaxation, unlike reuse relaxation, stays target-size only.)
pub fn reuse_available_expressions_barrier_relaxed(ast: &mut Ast, root: NodeId) -> PassResult {
    let source = clone_source(ast);
    let resolution = resolve(&source, root);
    let mut transformer = Transformer::new(&source, root, &resolution, true);
    transformer.transform_block(root);
    let reused = transformer.reused;
    *ast = transformer.output;
    PassResult {
        root,
        saved: None,
        details: Some(vec![format!("reused={reused}")]),
    }
}

/// Strict reuse: carriers for barrier-stable pure calls (screen size,
/// literal-only pure math; see `barrier_stable_pure_call`) survive an
/// intervening non-preserving statement, including an unknown call.
/// Cycle 44 dissection: relaxing reuse alone changed no corpus winner
/// (0/4,155 files in all 4 conditions) and wins on the screen-size
/// probe, while relaxing synthesis alone reproduced the cycle 42
/// +22-character alias churn, so synthesis relaxation stays
/// target-size only and reuse relaxation applies here too.
pub fn reuse_available_expressions(ast: &mut Ast, root: NodeId) -> PassResult {
    let source = clone_source(ast);
    let resolution = resolve(&source, root);
    let mut transformer = Transformer::new(&source, root, &resolution, true);
    transformer.transform_block(root);
    let reused = transformer.reused;
    *ast = transformer.output;
    PassResult {
        root,
        saved: None,
        details: Some(vec![format!("reused={reused}")]),
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
        let result = reuse_available_expressions(&mut ast, root);
        let reused = result
            .details
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.strip_prefix("reused="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        (Printer::new(&ast, false).output(root), reused)
    }

    #[test]
    fn does_not_reuse_expression_defined_behind_a_forward_goto() {
        // The only definition of the carrier sits between an unconditional
        // goto and its label, so it never executes before the use; the
        // expression must not be replaced by the (nil) carrier.
        let source = "function onTick()local t={x=1}goto skip g=#t::skip::output.setNumber(1,math.max(#t,5))end";
        let (out, reused) = run(source);
        assert_eq!(reused, 0, "{out}");
        assert!(out.contains("#t"), "{out}");
    }

    #[test]
    fn does_not_reuse_summary_call_across_a_field_store() {
        // Q05: the cached result of `read()` depends on the *contents*
        // of t through the callee's summary; binding-level dependency
        // tracking cannot see the intervening field store `t.a=...`,
        // so the second call must not be replaced by the carrier.
        let source = "function onTick()local t={a=3}local function read()return t.a+2 end local b=read() t.a=t.a+4 local c=read() output.setNumber(1,c)end";
        let (out, reused) = run(source);
        assert_eq!(reused, 0, "{out}");
        assert!(out.contains("c=read()"), "{out}");
        // Without the intervening store the reuse is still performed.
        let source = "function onTick()local t={a=3}local function read()return t.a+2 end local b=read() local c=read() output.setNumber(1,c)end";
        let (out, reused) = run(source);
        assert!(reused >= 1, "{out}");
        assert!(out.contains("c=b"), "{out}");
    }

    #[test]
    fn does_not_reuse_across_a_label_reached_by_a_backward_goto() {
        // The carrier is defined before the label; on the second arrival
        // (via the backward goto) its reads may have changed, so the entry
        // must not survive the label.
        let source = "function onTick()local t={x=1}local c=0 g=#t::top::output.setNumber(1,math.max(#t,5))t={}c=c+1 if c<3 then goto top end end";
        let (out, reused) = run(source);
        assert_eq!(reused, 0, "{out}");
    }

    #[test]
    fn still_reuses_in_straight_line_code_without_goto() {
        let source = "function onTick()local t={x=1}g=#t output.setNumber(1,math.max(#t,5))end";
        let (out, reused) = run(source);
        assert!(reused > 0, "{out}");
        assert!(out.contains("math.max(g,5)"), "{out}");
    }

    #[test]
    fn reuses_reaching_expression() {
        let source = "function onTick()x=math.abs(input.getNumber(1))y=math.abs(input.getNumber(1))output.setNumber(1,x+y)end";
        let (out, reused) = run(source);
        assert!(reused > 0, "{out}");
        assert!(out.contains("y=x"), "{out}");
    }

    #[test]
    fn reuses_pure_host_read_in_straight_line_code() {
        let source = "function onTick()local a=input.getNumber(1)local b=input.getNumber(1)output.setNumber(1,a+b)end";
        let (out, reused) = run(source);
        assert!(reused > 0, "{out}");
        assert!(out.contains("local b=a"), "{out}");
    }

    #[test]
    fn reuses_pure_host_read_across_pure_math_statement() {
        let source = "function onTick()local a=input.getNumber(1)local y=math.floor(a)local b=input.getNumber(1)output.setNumber(1,y+b)end";
        let (out, reused) = run(source);
        assert!(reused > 0, "{out}");
        // The printer merges adjacent locals, so check the call count
        // rather than the exact local layout.
        assert_eq!(out.matches("input.getNumber(1)").count(), 1, "{out}");
    }

    #[test]
    fn does_not_reuse_pure_host_read_across_unknown_call() {
        let source = "function onTick()local a=input.getNumber(1)foo()local b=input.getNumber(1)output.setNumber(1,a+b)end";
        let (out, reused) = run(source);
        assert_eq!(reused, 0, "{out}");
        assert!(out.contains("input.getNumber(1)"), "{out}");
    }

    fn run_reuse_barrier(source: &str) -> (String, usize) {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = reuse_available_expressions_barrier_relaxed(&mut ast, root);
        let reused = result
            .details
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.strip_prefix("reused="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        (Printer::new(&ast, false).output(root), reused)
    }

    #[test]
    fn barrier_relaxed_reuses_screen_size_across_unknown_call() {
        // Screen size has no setter in any profile, so an unknown call
        // cannot change it; the carrier survives the barrier.
        let source = "function onDraw()local w=screen.getWidth()foo()local h=screen.getWidth()screen.drawRect(0,0,w,h)end";
        let (out, reused) = run_reuse_barrier(source);
        assert!(reused > 0, "{out}");
        assert_eq!(out.matches("screen.getWidth()").count(), 1, "{out}");
    }

    #[test]
    fn barrier_relaxed_reuses_literal_math_across_unknown_call() {
        // A literal-only math call depends on nothing a statement can
        // change (math root mutation is excluded program-wide).
        let source = "function onTick()local a=math.fmod(5.5,2)foo()local b=math.fmod(5.5,2)output.setNumber(1,a+b)end";
        let (out, reused) = run_reuse_barrier(source);
        assert!(reused > 0, "{out}");
        assert_eq!(out.matches("math.fmod(5.5,2)").count(), 1, "{out}");
    }

    #[test]
    fn barrier_relaxed_does_not_reuse_global_carrier_across_unknown_call() {
        // A global carrier could be reassigned by a function from
        // outside the program, whose writes are not recorded, so the
        // barrier rule still applies even for screen size.
        let source = "function onDraw()w=screen.getWidth()foo()h=screen.getWidth()screen.drawRect(0,0,w,h)end";
        let (out, reused) = run_reuse_barrier(source);
        assert_eq!(reused, 0, "{out}");
        assert_eq!(out.matches("screen.getWidth()").count(), 2, "{out}");
    }

    #[test]
    fn barrier_relaxed_does_not_reuse_nested_host_read_math_across_unknown_call() {
        // A math call over an input read depends on host state, which
        // an unknown call could change (Extended control setter).
        let source = "function onTick()local a=math.ceil(input.getNumber(1))foo()local b=math.ceil(input.getNumber(1))output.setNumber(1,a+b)end";
        let (out, reused) = run_reuse_barrier(source);
        assert_eq!(reused, 0, "{out}");
        assert_eq!(
            out.matches("math.ceil(input.getNumber(1))").count(),
            2,
            "{out}"
        );
    }

    #[test]
    fn strict_reuse_reuses_screen_size_across_unknown_call() {
        // Cycle 44: barrier-stable reuse now applies on the strict path
        // too (reuse-only relaxation changed no corpus winner, while
        // synth-only relaxation reproduced the cycle 42 churn).
        let source = "function onDraw()local w=screen.getWidth()foo()local h=screen.getWidth()screen.drawRect(0,0,w,h)end";
        let (out, reused) = run(source);
        assert!(reused > 0, "{out}");
        assert_eq!(out.matches("screen.getWidth()").count(), 1, "{out}");
    }

    #[test]
    fn strict_reuse_reuses_literal_math_across_unknown_call() {
        // Same relaxation for literal-only pure math calls.
        let source = "function onTick()local a=math.fmod(5.5,2)foo()local b=math.fmod(5.5,2)output.setNumber(1,a+b)end";
        let (out, reused) = run(source);
        assert!(reused > 0, "{out}");
        assert_eq!(out.matches("math.fmod(5.5,2)").count(), 1, "{out}");
    }

    #[test]
    fn does_not_reuse_mutated_host_read() {
        let source = "function onTick()local a=input.getNumber(1)input.getNumber=function() return 0 end local b=input.getNumber(1)output.setNumber(1,a+b)end";
        let (out, reused) = run(source);
        assert_eq!(reused, 0, "{out}");
    }

    #[test]
    fn does_not_reuse_different_host_read_channel() {
        let source = "function onTick()local a=input.getNumber(1)local b=input.getNumber(2)output.setNumber(1,a+b)end";
        let (out, reused) = run(source);
        assert_eq!(reused, 0, "{out}");
    }

    #[test]
    fn rejects_mutable_table_field_reads() {
        let source = "function onTick()t={value=7}local value=t.value t.value=9 output.setNumber(1,t.value)end";
        let (out, reused) = run(source);
        assert_eq!(reused, 0, "{out}");
        assert!(out.contains("output.setNumber(1,t.value)"), "{out}");
    }

    #[test]
    fn does_not_reuse_call_in_constructor_expansion_position() {
        // A carrier holds only the first return value, so reusing it for
        // a trailing constructor call would truncate {pair()} to one value.
        let source = "local function pair() return 7, 8 end function onTick()local c=pair()local t={pair()}output.setNumber(1,#t)output.setNumber(2,c)end";
        let (out, _) = run(source);
        assert_eq!(out.matches("pair()").count(), 3, "{out}");
    }

    #[test]
    fn does_not_reuse_call_in_last_argument_position() {
        let source = "local function pair() return 7, 8 end local function take(a,b) output.setNumber(1,a) output.setNumber(2,b) end function onTick()local c=pair()output.setNumber(3,c)take(pair())end";
        let (out, _) = run(source);
        assert_eq!(out.matches("pair()").count(), 3, "{out}");
    }

    #[test]
    fn does_not_reuse_call_in_statement_position() {
        // Replacing a statement call by its carrier name would drop the
        // call (and is not a valid statement), so the call must stay.
        let source = "local function pair() return 7, 8 end function onTick()local c=pair()output.setNumber(1,c)pair()output.setNumber(2,c)end";
        let (out, _) = run(source);
        assert_eq!(out.matches("pair()").count(), 3, "{out}");
    }

    #[test]
    fn preserves_fresh_table_identity() {
        let source = "function onTick()local a={x=1}local b={x=1}a.x=2 output.setBool(1,a~=b)output.setNumber(1,b.x)end";
        let (out, reused) = run(source);
        assert_eq!(reused, 0, "{out}");
        assert!(out.contains("local a,b={x=1},{x=1}"), "{out}");
    }
    #[test]
    fn reuses_pure_math_call_in_straight_line_code() {
        let source =
            "function onTick()local a=math.ceil(x)local b=math.ceil(x)output.setNumber(1,a+b)end";
        let (out, reused) = run(source);
        assert!(reused > 0, "{out}");
        assert_eq!(out.matches("math.ceil(x)").count(), 1, "{out}");
    }

    #[test]
    fn reuses_pure_math_call_across_pure_host_statement() {
        let source = "function onTick()local a=math.ceil(x)local y=input.getNumber(1)local b=math.ceil(x)output.setNumber(1,a+b+y)end";
        let (out, reused) = run(source);
        assert!(reused > 0, "{out}");
        assert_eq!(out.matches("math.ceil(x)").count(), 1, "{out}");
    }

    #[test]
    fn reuses_nested_pure_math_call_with_pure_host_argument() {
        let source = "function onTick()local a=math.ceil(input.getNumber(1))local b=math.ceil(input.getNumber(1))output.setNumber(1,a+b)end";
        let (out, reused) = run(source);
        assert!(reused > 0, "{out}");
        assert_eq!(
            out.matches("math.ceil(input.getNumber(1))").count(),
            1,
            "{out}"
        );
    }

    #[test]
    fn does_not_reuse_pure_math_call_across_unknown_call() {
        let source = "function onTick()local a=math.ceil(x)foo()local b=math.ceil(x)output.setNumber(1,a+b)end";
        let (out, reused) = run(source);
        assert_eq!(reused, 0, "{out}");
        assert_eq!(out.matches("math.ceil(x)").count(), 2, "{out}");
    }

    #[test]
    fn does_not_reuse_mutated_math_call() {
        let source = "function onTick()local a=math.ceil(x)math.ceil=function(v) return 0 end local b=math.ceil(x)output.setNumber(1,a+b)end";
        let (out, reused) = run(source);
        assert_eq!(reused, 0, "{out}");
    }

    #[test]
    fn does_not_reuse_random_call() {
        let source = "function onTick()local a=math.random(10)local b=math.random(10)output.setNumber(1,a+b)end";
        let (out, reused) = run(source);
        assert_eq!(reused, 0, "{out}");
    }

    #[test]
    fn does_not_reuse_multi_return_math_call() {
        let source =
            "function onTick()local a=math.modf(x)local b=math.modf(x)output.setNumber(1,a+b)end";
        let (out, reused) = run(source);
        assert_eq!(reused, 0, "{out}");
    }

    #[test]
    fn synth_skips_carrier_when_chunk_at_local_limit() {
        // 200 chunk locals declared from repeated pure calls: the
        // chunk function is exactly at the active-local limit, so any
        // synthesized carrier would make the output fail to load.
        let decls = (1..=200)
            .map(|i| format!("local c{i} = input.getNumber({})", i % 25))
            .collect::<Vec<_>>()
            .join(" ");
        let source = format!("{decls} R = c1 + c200");
        let (_, synthesized) = run_synth(&source);
        assert_eq!(synthesized, 0);
    }

    #[test]
    fn synth_inserts_carrier_well_below_local_limit() {
        // Same repeated-call shape with ample local headroom: the
        // carrier is synthesized as before.
        let decls = (1..=64)
            .map(|i| format!("local c{i} = input.getNumber({})", i % 8))
            .collect::<Vec<_>>()
            .join(" ");
        let source = format!("{decls} R = c1 + c64");
        let (_, synthesized) = run_synth(&source);
        assert!(synthesized >= 1);
    }

    #[test]
    fn synth_skips_carrier_when_function_at_local_limit() {
        // 198 params + the two call-result locals = 200 active
        // locals in the function body.
        let params = (1..=198)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "local function big({params}) local a = input.getNumber(1) + input.getNumber(1) + input.getNumber(1) + input.getNumber(1) local b = input.getNumber(1) + input.getNumber(1) + input.getNumber(1) + input.getNumber(1) return a + b end"
        );
        let (_, synthesized) = run_synth(&source);
        assert_eq!(synthesized, 0);
    }

    #[test]
    fn synth_second_group_respects_budget_filled_by_first() {
        // 197 params + two call-result locals = 199 active locals:
        // the first carrier fills the budget to exactly 200, so the
        // second group must be skipped.
        let params = (1..=197)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "local function big({params}) local a = input.getNumber(1) + input.getNumber(1) + input.getNumber(1) + input.getNumber(1) + input.getNumber(2) + input.getNumber(2) + input.getNumber(2) + input.getNumber(2) local b = input.getNumber(1) + input.getNumber(1) + input.getNumber(1) + input.getNumber(1) + input.getNumber(2) + input.getNumber(2) + input.getNumber(2) + input.getNumber(2) return a + b end"
        );
        let (_, synthesized) = run_synth(&source);
        assert_eq!(synthesized, 1);
    }

    fn run_synth(source: &str) -> (String, usize) {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = synthesize_pure_call_carriers(&mut ast, root);
        let synthesized = result
            .details
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.strip_prefix("synthesized="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        (Printer::new(&ast, false).output(root), synthesized)
    }

    #[test]
    fn synthesizes_duplicate_host_read_in_straight_line_code() {
        // Post-alias key `G(123456)` is 9 chars, 3 occurrences:
        // (9 + 8) - 3 * (9 - 1) + (1 + 3) = -3, so synthesis pays.
        let source = "function onTick()x=input.getNumber(123456)+input.getNumber(123456)+input.getNumber(123456)output.setNumber(1,x)end";
        let (out, synthesized) = run_synth(source);
        assert_eq!(synthesized, 1, "{out}");
        assert_eq!(out.matches("input.getNumber(123456)").count(), 1, "{out}");
        assert!(out.contains("__cse"), "{out}");
    }

    #[test]
    fn does_not_synthesize_short_key_with_two_occurrences() {
        // Post-alias key `G(1)` is 4 chars, 2 occurrences:
        // (4 + 8) - 2 * (4 - 1) + (1 + 2) = +9, so synthesis would grow
        // the output and must not happen.
        let source =
            "function onTick()x=input.getNumber(1)+input.getNumber(1)output.setNumber(1,x)end";
        let (out, synthesized) = run_synth(source);
        assert_eq!(synthesized, 0, "{out}");
        assert_eq!(out.matches("input.getNumber(1)").count(), 2, "{out}");
    }

    #[test]
    fn does_not_synthesize_get_height_triple_in_straight_line_code() {
        // Post-alias key `a()` is 3 chars, 3 occurrences:
        // (3 + 8) - 3 * (3 - 1) + (1 + 3) = +9. Aliasing already
        // shrinks the call below any carrier profit, so no synthesis.
        let source = "function onTick()x=screen.getHeight()+screen.getHeight()+screen.getHeight()output.setNumber(1,x)end";
        let (out, synthesized) = run_synth(source);
        assert_eq!(synthesized, 0, "{out}");
        assert_eq!(out.matches("screen.getHeight()").count(), 3, "{out}");
    }

    #[test]
    fn does_not_synthesize_single_occurrence() {
        let source = "function onTick()x=input.getNumber(1)output.setNumber(1,x)end";
        let (out, synthesized) = run_synth(source);
        assert_eq!(synthesized, 0, "{out}");
        assert_eq!(out.matches("input.getNumber(1)").count(), 1, "{out}");
    }

    #[test]
    fn does_not_synthesize_across_unknown_call() {
        let source = "function onTick()x=input.getNumber(1)foo()y=input.getNumber(1)output.setNumber(1,x+y)end";
        let (out, synthesized) = run_synth(source);
        assert_eq!(synthesized, 0, "{out}");
        assert_eq!(out.matches("input.getNumber(1)").count(), 2, "{out}");
    }

    fn run_synth_barrier(source: &str) -> (String, usize) {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = synthesize_pure_call_carriers_relaxed_barrier(&mut ast, root);
        let synthesized = result
            .details
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.strip_prefix("synthesized="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        (Printer::new(&ast, false).output(root), synthesized)
    }

    #[test]
    fn barrier_relaxed_synthesizes_literal_math_across_unknown_call() {
        // The literal math value cannot change across foo(), so the
        // group survives the barrier and one carrier is synthesized.
        let source =
            "function onTick()x=math.fmod(5.5,2)foo()y=math.fmod(5.5,2)output.setNumber(1,x+y)end";
        let (out, synthesized) = run_synth_barrier(source);
        assert_eq!(synthesized, 1, "{out}");
        assert_eq!(out.matches("math.fmod(5.5,2)").count(), 1, "{out}");
    }

    #[test]
    fn strict_synth_does_not_synthesize_literal_math_across_unknown_call() {
        // The strict pass keeps the barrier rule even for literal math.
        let source =
            "function onTick()x=math.fmod(5.5,2)foo()y=math.fmod(5.5,2)output.setNumber(1,x+y)end";
        let (out, synthesized) = run_synth(source);
        assert_eq!(synthesized, 0, "{out}");
        assert_eq!(out.matches("math.fmod(5.5,2)").count(), 2, "{out}");
    }

    fn run_synth_barrier_strict(source: &str) -> (String, usize) {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = synthesize_pure_call_carriers_barrier_relaxed(&mut ast, root);
        let synthesized = result
            .details
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.strip_prefix("synthesized="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        (Printer::new(&ast, false).output(root), synthesized)
    }

    #[test]
    fn barrier_relaxed_strict_gate_synthesizes_literal_math_across_unknown_call() {
        // Cycle 45: the strict character gate passes for two literal math
        // calls (modeled net -1), so only the barrier differs from the
        // strict pass: the group survives foo() and one carrier is made.
        let source =
            "function onTick()x=math.fmod(5.5,2)foo()y=math.fmod(5.5,2)output.setNumber(1,x+y)end";
        let (out, synthesized) = run_synth_barrier_strict(source);
        assert_eq!(synthesized, 1, "{out}");
        assert_eq!(out.matches("math.fmod(5.5,2)").count(), 1, "{out}");
    }

    #[test]
    fn barrier_relaxed_strict_gate_rejects_positive_net_across_unknown_call() {
        // Three screen.getHeight() calls across barriers form one stable
        // group, but the modeled net is +9 (post-alias key 3 chars), so
        // the strict gate still rejects it even with barrier relaxation.
        let source = "function onTick()x=screen.getHeight()foo()y=screen.getHeight()foo()z=screen.getHeight()output.setNumber(1,x+y+z)end";
        let (out, synthesized) = run_synth_barrier_strict(source);
        assert_eq!(synthesized, 0, "{out}");
        assert_eq!(out.matches("screen.getHeight()").count(), 3, "{out}");
    }

    #[test]
    fn does_not_synthesize_mutated_host_read() {
        let source = "function onTick()x=input.getNumber(1)input.getNumber=function() return 0 end y=input.getNumber(1)output.setNumber(1,x+y)end";
        let (out, synthesized) = run_synth(source);
        assert_eq!(synthesized, 0, "{out}");
        assert_eq!(out.matches("input.getNumber(1)").count(), 2, "{out}");
    }

    #[test]
    fn synthesizes_nested_math_call_once_for_outer_tree() {
        let source = "function onTick()x=math.floor(input.getNumber(1))+math.floor(input.getNumber(1))output.setNumber(1,x)end";
        let (out, synthesized) = run_synth(source);
        assert_eq!(synthesized, 1, "{out}");
        assert_eq!(
            out.matches("math.floor(input.getNumber(1))").count(),
            1,
            "{out}"
        );
    }

    #[test]
    fn does_not_synthesize_non_integer_channel() {
        let source =
            "function onTick()x=input.getNumber(1.5)+input.getNumber(1.5)output.setNumber(1,x)end";
        let (out, synthesized) = run_synth(source);
        assert_eq!(synthesized, 0, "{out}");
        assert_eq!(out.matches("input.getNumber(1.5)").count(), 2, "{out}");
    }

    #[test]
    fn does_not_synthesize_wrong_arity_math_call() {
        let source = "function onTick()x=math.floor()+math.floor()output.setNumber(1,x)end";
        let (out, synthesized) = run_synth(source);
        assert_eq!(synthesized, 0, "{out}");
        assert_eq!(out.matches("math.floor()").count(), 2, "{out}");
    }

    #[test]
    fn does_not_synthesize_random_call() {
        let source = "function onTick()x=math.random(10)+math.random(10)output.setNumber(1,x)end";
        let (out, synthesized) = run_synth(source);
        assert_eq!(synthesized, 0, "{out}");
        assert_eq!(out.matches("math.random(10)").count(), 2, "{out}");
    }

    #[test]
    fn does_not_synthesize_name_operand_math_call() {
        let source = "function onTick()x=math.floor(x)+math.floor(x)output.setNumber(1,x)end";
        let (out, synthesized) = run_synth(source);
        assert_eq!(synthesized, 0, "{out}");
        assert_eq!(out.matches("math.floor(x)").count(), 2, "{out}");
    }

    #[test]
    fn does_not_synthesize_across_if_statement() {
        let source = "function onTick()x=input.getNumber(1)if flag then y=1 end z=input.getNumber(1)output.setNumber(1,x+z)end";
        let (out, synthesized) = run_synth(source);
        assert_eq!(synthesized, 0, "{out}");
        assert_eq!(out.matches("input.getNumber(1)").count(), 2, "{out}");
    }

    #[test]
    fn does_not_synthesize_in_block_with_goto_label() {
        let source = "function onTick()x=input.getNumber(1)goto skip y=input.getNumber(1)::skip::output.setNumber(1,x+y)end";
        let (out, synthesized) = run_synth(source);
        assert_eq!(synthesized, 0, "{out}");
        assert_eq!(out.matches("input.getNumber(1)").count(), 2, "{out}");
    }

    #[test]
    fn synthesizes_fresh_group_after_barrier() {
        let source = "function onTick()x=input.getNumber(1)foo()y=input.getNumber(123456)+input.getNumber(123456)+input.getNumber(123456)output.setNumber(1,x+y)end";
        let (out, synthesized) = run_synth(source);
        assert_eq!(synthesized, 1, "{out}");
        assert_eq!(out.matches("input.getNumber(123456)").count(), 1, "{out}");
        assert_eq!(out.matches("input.getNumber(1)").count(), 1, "{out}");
    }

    #[test]
    fn funcstat_redefinition_between_evaluations_invalidates_cached_call() {
        // The second `h()` denotes a different function than the first:
        // the Funcstat between them redefines the callee, so its result
        // must not be replaced by the first call's carrier.
        let source = "function h()\n return 1\nend\nlocal a = h()\nfunction h()\n return 2\nend\nlocal b = h()\n";
        let (printed, reused) = run(source);
        assert_eq!(reused, 0, "{printed}");
        assert_eq!(printed.matches("=h()").count(), 2, "{printed}");
    }

    #[test]
    fn reassignment_between_evaluations_invalidates_cached_call() {
        let source = "function h()\n return 1\nend\nlocal a = h()\nh = function()\n return 2\nend\nlocal b = h()\n";
        let (printed, reused) = run(source);
        assert_eq!(reused, 0, "{printed}");
        assert_eq!(printed.matches("=h()").count(), 2, "{printed}");
    }

    #[test]
    fn unchanged_callee_still_reuses_cached_call() {
        let source = "function h()\n return 1\nend\nlocal a = h()\nlocal b = h()\n";
        let (_printed, reused) = run(source);
        assert_eq!(reused, 1);
    }
    #[test]
    fn synth_skips_carrier_when_method_at_local_limit_counting_self() {
        // Method with 197 explicit parameters plus two call-result
        // locals: counting the implicit `self`, the function is at
        // 200 active locals, so no carrier may be synthesized.
        let params = (1..=197)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "local obj = {{}} function obj:m({params}) local a = input.getNumber(1) + input.getNumber(1) + input.getNumber(1) + input.getNumber(1) local b = input.getNumber(1) + input.getNumber(1) + input.getNumber(1) + input.getNumber(1) return a + b end"
        );
        let (_, synthesized) = run_synth(&source);
        assert_eq!(synthesized, 0);
    }

    #[test]
    fn synth_inserts_carrier_when_method_below_local_limit_counting_self() {
        let params = (1..=196)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "local obj = {{}} function obj:m({params}) local a = input.getNumber(1) + input.getNumber(1) + input.getNumber(1) + input.getNumber(1) local b = input.getNumber(1) + input.getNumber(1) + input.getNumber(1) + input.getNumber(1) return a + b end"
        );
        let (_, synthesized) = run_synth(&source);
        assert!(synthesized >= 1);
    }
}
