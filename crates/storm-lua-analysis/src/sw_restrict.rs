//! Stormworks 固有制限の検出（v0.6.0）。
//!
//! Stormworks のサンドボックスは標準 Lua の一部しか提供しない（利用可能なルートは
//! `resolver::API_ROOTS` が正本）。また `input.`/`output.` は `onTick` コールバックの
//! 中でしか使えない（onDraw やトップレベルから触っても実機では効果がない）。
//!
//! 検出ロジックはここに一箇所だけ実装し、severity は呼び出し側が指定する:
//! - `analyze()` → `Severity::Warning`（リンター警告。実行は妨げない。`--@storm ignore`/
//!   `disabledRules` で抑制可能）
//! - `compile_project()` → `Severity::Error`（既存の「error が1件でもあれば ok:false」に乗る。
//!   Minify を要求された場合は失敗させる）
//!
//! 標準機能の可否は単一ソース・プロジェクト双方の共通environment契約で検査する。

use std::collections::{HashMap, HashSet, VecDeque};

use crate::diagnostic::{codes, Diagnostic, Range, Severity};
use crate::project::ModuleAnalysis;
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::ast_utils::for_each_child_key;
use storm_lua_syntax::numeric::decode_lua_string;
use storm_lua_syntax::parser::NodePositions;

/// Known absent game-facing globals; shared with runtime environment construction.
pub use storm_lua_spec::environment::GAME_UNAVAILABLE as UNAVAILABLE_LUA_BUILTINS;

/// Whether a standard global is absent from the game profile.
pub fn is_unavailable_builtin(name: &str) -> bool {
    storm_lua_spec::environment::EnvironmentProfile::Game.is_unavailable(name)
}

fn diagnostic(
    severity: Severity,
    code: &'static str,
    message: impl Into<String>,
    module: &str,
    range: Option<Range>,
) -> Diagnostic {
    Diagnostic {
        code,
        severity,
        message: message.into(),
        module: Some(module.to_string()),
        range,
    }
}

/// `sw-unavailable-global`: 既知の Stormworks 非搭載ビルトインへのグローバル読み取り参照。
/// プロジェクト全体のどこかで自前定義（write）されている名前は対象外にする
/// （`lint::collect_written_global_names` と同じ判定基準を共有する）。
fn unavailable_global_diagnostics(
    key: &str,
    analysis: &ModuleAnalysis,
    written_globals: &HashSet<String>,
    severity: Severity,
) -> Vec<Diagnostic> {
    crate::environment_checks::diagnostics(
        &analysis.ast,
        analysis.root,
        Some(&analysis.positions),
        storm_lua_spec::environment::EnvironmentProfile::Game,
        &[],
        written_globals,
        severity,
        Some(key),
    )
}

/// トップレベルで関数リテラルに束縛される名前のキー。
/// `Name` は `g` （`function g()end` / `local function g` / `g = function`）、
/// `Field` は `M.f` （`M.f = function`/`function M.f()`、`M` はトップレベルの単純名）。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum BindKey {
    Name(String),
    Field(String, String),
}

/// `target` （呼び出し式の被呼び出し部、または代入/関数宣言の左辺）が
/// トップレベル束縛のキーとして解釈できるなら返す。`M.f` 形式（`.` アクセス、
/// 静的な文字列キー）のみ対応し、`M[f]` や動的キーは対象外（安全側）。
fn bind_key(ast: &Ast, target: NodeId) -> Option<BindKey> {
    match ast.node(target) {
        Node::Name(sym) => Some(BindKey::Name(ast.strings.get(*sym).to_string())),
        Node::Index(base, key, true) => {
            if let (Node::Name(bsym), Node::Str(raw)) = (ast.node(*base), ast.node(*key)) {
                Some(BindKey::Field(
                    ast.strings.get(*bsym).to_string(),
                    decode_lua_string(raw)?,
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn insert_binding(
    map: &mut HashMap<BindKey, NodeId>,
    ambiguous: &mut HashSet<BindKey>,
    key: BindKey,
    fn_id: NodeId,
) {
    if ambiguous.contains(&key) {
        return;
    }
    match map.get(&key) {
        None => {
            map.insert(key, fn_id);
        }
        Some(existing) if *existing == fn_id => {}
        Some(_) => {
            // 同じ名前へのトップレベル束縛が複数見つかった → 一意に決定できないので
            // 呼び出しグラフのエッジ解決対象から外す（安全側: 追跡できないものは
            // 従来どおり違反として扱われる）。
            map.remove(&key);
            ambiguous.insert(key);
        }
    }
}

/// モジュールのトップレベル文だけを見て、名前 → 関数リテラルの束縛表を作る。
/// `function g()end` / `local function g` / `g = function` / `function M.f()end` /
/// `M.f = function` の5形態のみ対象（`local g = function()end` は対象外。
/// トップレベルで同名が複数回束縛される場合は解決不能として除外する）。
fn collect_top_level_bindings(ast: &Ast, root: NodeId) -> HashMap<BindKey, NodeId> {
    let mut map = HashMap::new();
    let mut ambiguous = HashSet::new();
    let stmts = match ast.node(root) {
        Node::Block(ss) => ss.clone(),
        _ => Vec::new(),
    };
    for stmt in stmts {
        match ast.node(stmt) {
            Node::Funcstat(target, fn_) => {
                let (target_id, fn_id) = (*target, *fn_);
                if matches!(ast.node(fn_id), Node::Function(..)) {
                    if let Some(key) = bind_key(ast, target_id) {
                        insert_binding(&mut map, &mut ambiguous, key, fn_id);
                    }
                }
            }
            Node::Localfunc(sym, fn_) => {
                let fn_id = *fn_;
                if matches!(ast.node(fn_id), Node::Function(..)) {
                    let key = BindKey::Name(ast.strings.get(*sym).to_string());
                    insert_binding(&mut map, &mut ambiguous, key, fn_id);
                }
            }
            Node::Assign(vs, es) => {
                for (v, e) in vs.iter().zip(es.iter()) {
                    if matches!(ast.node(*e), Node::Function(..)) {
                        if let Some(key) = bind_key(ast, *v) {
                            insert_binding(&mut map, &mut ambiguous, key, *e);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    map
}

/// `id` から到達可能な直接呼び出し `g(...)` / `M.f(...)` を、関数境界（ネストした
/// 関数リテラルの内部）を跨がずに収集する。呼び出し以外の式（callee/引数）も
/// 再帰的に走査し、さらに内側の直接呼び出しを見つける。
fn collect_calls(ast: &Ast, id: NodeId, out: &mut Vec<BindKey>) {
    match ast.node(id) {
        Node::Function(..) => {
            // 関数境界: ここで止める（ネストした関数リテラルの本体は呼ばれたかどうか
            // 分からないので、静的呼び出しグラフのエッジ元にはしない）。
        }
        Node::Call(callee, args, method) => {
            let (callee_id, args) = (*callee, args.clone());
            if method.is_none() {
                if let Some(key) = bind_key(ast, callee_id) {
                    out.push(key);
                }
            }
            collect_calls(ast, callee_id, out);
            for a in args {
                collect_calls(ast, a, out);
            }
        }
        _ => {
            let mut children = Vec::new();
            for_each_child_key(ast, id, &mut |_key, child| children.push(child));
            for child in children {
                collect_calls(ast, child, out);
            }
        }
    }
}

/// モジュール内の `onTick` に代入される関数リテラルの本体（複数あり得る）を
/// 位置を問わず収集する。呼び出しグラフの探索起点。
fn collect_on_tick_bodies(ast: &Ast, id: NodeId, out: &mut Vec<NodeId>) {
    match ast.node(id) {
        Node::Funcstat(target, fn_) => {
            let (target_id, fn_id) = (*target, *fn_);
            if is_name(ast, target_id, "onTick") {
                if let Node::Function(_, _, body) = ast.node(fn_id) {
                    out.push(*body);
                }
            }
            collect_on_tick_bodies(ast, target_id, out);
            collect_on_tick_bodies(ast, fn_id, out);
        }
        Node::Assign(vs, es) => {
            for (v, e) in vs.iter().zip(es.iter()) {
                if is_name(ast, *v, "onTick") {
                    if let Node::Function(_, _, body) = ast.node(*e) {
                        out.push(*body);
                    }
                }
                collect_on_tick_bodies(ast, *v, out);
                collect_on_tick_bodies(ast, *e, out);
            }
        }
        _ => {
            let mut children = Vec::new();
            for_each_child_key(ast, id, &mut |_key, child| children.push(child));
            for child in children {
                collect_on_tick_bodies(ast, child, out);
            }
        }
    }
}

/// `onTick` に代入される関数（複数あり得る）から静的に到達可能な、トップレベル束縛
/// された関数のキー集合を計算する（BFS）。動的呼び出し・関数値の持ち回りは追わない
/// （追えないものは呼び出しグラフのエッジにしない = 安全側）。
fn compute_reachable_from_on_tick(ast: &Ast, root: NodeId) -> HashSet<BindKey> {
    let bindings = collect_top_level_bindings(ast, root);
    let mut seeds = Vec::new();
    collect_on_tick_bodies(ast, root, &mut seeds);

    let mut reachable_keys: HashSet<BindKey> = HashSet::new();
    let mut visited_bodies: HashSet<NodeId> = HashSet::new();
    let mut queue: VecDeque<NodeId> = VecDeque::new();
    for body in seeds {
        if visited_bodies.insert(body) {
            queue.push_back(body);
        }
    }
    while let Some(body) = queue.pop_front() {
        let mut calls = Vec::new();
        collect_calls(ast, body, &mut calls);
        for key in calls {
            let Some(&fn_id) = bindings.get(&key) else {
                continue;
            };
            let Node::Function(_, _, callee_body) = ast.node(fn_id) else {
                continue;
            };
            reachable_keys.insert(key);
            if visited_bodies.insert(*callee_body) {
                queue.push_back(*callee_body);
            }
        }
    }
    reachable_keys
}

/// `input.`/`output.` メンバー参照を、レキシカルスコープが `onTick` に代入される関数リテラルの
/// 本体（ネストした内側関数リテラルを含む）、または `onTick` から静的に到達可能な
/// トップレベル関数の本体の中かどうかで判定するための走査コンテキスト。
struct OnTickCtx<'a> {
    ast: &'a Ast,
    positions: &'a NodePositions,
    module: &'a str,
    severity: Severity,
    reachable: &'a HashSet<BindKey>,
    diagnostics: Vec<Diagnostic>,
}

impl OnTickCtx<'_> {
    fn push(&mut self, code: &'static str, id: NodeId, message: impl Into<String>) {
        let range = self
            .positions
            .get(id)
            .map(|(line, col)| Range::point(line, col));
        self.diagnostics
            .push(diagnostic(self.severity, code, message, self.module, range));
    }
}

fn is_name(ast: &Ast, id: NodeId, target: &str) -> bool {
    matches!(ast.node(id), Node::Name(sym) if ast.strings.get(*sym) == target)
}

/// `id` を走査する。`in_on_tick` は現在位置が `onTick` に代入される関数リテラルの本体
/// （ネスト含む）かどうか。第一段階は直接参照のみを追跡する（関数呼び出し経由の間接的な
/// `input`/`output` 参照は対象外。投機的な汎用化を避ける）。
fn scan_node(ctx: &mut OnTickCtx, id: NodeId, in_on_tick: bool) {
    match ctx.ast.node(id) {
        Node::Index(obj, key, dot) => {
            let (obj_id, key_id, dot) = (*obj, *key, *dot);
            if dot && !in_on_tick {
                if is_name(ctx.ast, obj_id, "input") {
                    ctx.push(
                        codes::INPUT_OUTSIDE_ONTICK,
                        id,
                        "\"input.*\" can only be referenced inside onTick().",
                    );
                } else if is_name(ctx.ast, obj_id, "output") {
                    ctx.push(
                        codes::OUTPUT_OUTSIDE_ONTICK,
                        id,
                        "\"output.*\" can only be referenced inside onTick().",
                    );
                }
            }
            scan_node(ctx, obj_id, in_on_tick);
            scan_node(ctx, key_id, in_on_tick);
        }
        Node::Function(_, _, body) => {
            // ネストした関数リテラルは、その時点の in_on_tick をそのまま引き継ぐ
            // （onTick 本体内で定義された内側関数は onTick 内とみなす。§ネスト規則）。
            scan_node(ctx, *body, in_on_tick);
        }
        Node::Funcstat(target, fn_) => {
            let (target_id, fn_id) = (*target, *fn_);
            scan_node(ctx, target_id, in_on_tick);
            let sets_on_tick = is_name(ctx.ast, target_id, "onTick");
            if sets_on_tick {
                if let Node::Function(_, _, body) = ctx.ast.node(fn_id) {
                    scan_node(ctx, *body, true);
                    return;
                }
            }
            if let Node::Function(_, _, body) = ctx.ast.node(fn_id) {
                if bind_key(ctx.ast, target_id).is_some_and(|k| ctx.reachable.contains(&k)) {
                    scan_node(ctx, *body, true);
                    return;
                }
            }
            scan_node(ctx, fn_id, in_on_tick);
        }
        Node::Localfunc(sym, fn_) => {
            let fn_id = *fn_;
            if let Node::Function(_, _, body) = ctx.ast.node(fn_id) {
                let key = BindKey::Name(ctx.ast.strings.get(*sym).to_string());
                if ctx.reachable.contains(&key) {
                    scan_node(ctx, *body, true);
                    return;
                }
            }
            scan_node(ctx, fn_id, in_on_tick);
        }
        Node::Assign(vs, es) => {
            let vs = vs.clone();
            let es = es.clone();
            for v in &vs {
                scan_node(ctx, *v, in_on_tick);
            }
            for (i, e) in es.iter().enumerate() {
                let sets_on_tick = vs.get(i).is_some_and(|v| is_name(ctx.ast, *v, "onTick"));
                if sets_on_tick {
                    if let Node::Function(_, _, body) = ctx.ast.node(*e) {
                        scan_node(ctx, *body, true);
                        continue;
                    }
                }
                if let Node::Function(_, _, body) = ctx.ast.node(*e) {
                    let reachable = vs.get(i).is_some_and(|v| {
                        bind_key(ctx.ast, *v).is_some_and(|k| ctx.reachable.contains(&k))
                    });
                    if reachable {
                        scan_node(ctx, *body, true);
                        continue;
                    }
                }
                scan_node(ctx, *e, in_on_tick);
            }
        }
        _ => {
            let mut children = Vec::new();
            for_each_child_key(ctx.ast, id, &mut |_key, child| children.push(child));
            for child in children {
                scan_node(ctx, child, in_on_tick);
            }
        }
    }
}

fn on_tick_scope_diagnostics(
    key: &str,
    analysis: &ModuleAnalysis,
    severity: Severity,
) -> Vec<Diagnostic> {
    let reachable = compute_reachable_from_on_tick(&analysis.ast, analysis.root);
    let mut ctx = OnTickCtx {
        ast: &analysis.ast,
        positions: &analysis.positions,
        module: key,
        severity,
        reachable: &reachable,
        diagnostics: Vec::new(),
    };
    scan_node(&mut ctx, analysis.root, false);
    ctx.diagnostics
}

/// モジュール `key` に Stormworks 固有制限の検出をすべて実行する。
/// `written_globals` は `lint::collect_written_global_names` によるプロジェクト全体の
/// グローバル書き込み集合（自前定義された名前を `sw-unavailable-global` の対象外にするため）。
pub fn scan_module(
    key: &str,
    analysis: &ModuleAnalysis,
    written_globals: &HashSet<String>,
    severity: Severity,
) -> Vec<Diagnostic> {
    let mut out = unavailable_global_diagnostics(key, analysis, written_globals, severity);
    out.extend(on_tick_scope_diagnostics(key, analysis, severity));
    out
}

/// Profile-aware restrictions used by both editing diagnostics and project builds.
pub fn scan_module_in_environment(
    key: &str,
    analysis: &ModuleAnalysis,
    written_globals: &HashSet<String>,
    severity: Severity,
    environment: storm_lua_spec::environment::EnvironmentProfile,
    host_bindings: &[String],
) -> Vec<Diagnostic> {
    let mut out = crate::environment_checks::diagnostics(
        &analysis.ast,
        analysis.root,
        Some(&analysis.positions),
        environment,
        host_bindings,
        written_globals,
        severity,
        Some(key),
    );
    out.extend(on_tick_scope_diagnostics(key, analysis, severity));
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::require_scan::scan_requires;
    use std::collections::HashSet;
    use storm_lua_syntax::parser::parse_source_with_positions;

    fn analysis(source: &str) -> ModuleAnalysis {
        let (ast, root, positions) = parse_source_with_positions(source).expect("parse ok");
        let (requires, require_diagnostics) = scan_requires(&ast, root, &positions, "m");
        assert!(
            require_diagnostics.is_empty(),
            "unexpected require diagnostics: {:?}",
            require_diagnostics
                .iter()
                .map(|d| d.code)
                .collect::<Vec<_>>()
        );
        ModuleAnalysis {
            ast,
            root,
            positions,
            requires,
            ambient_usages: Vec::new(),
        }
    }

    fn codes_of(diagnostics: &[Diagnostic]) -> Vec<&'static str> {
        diagnostics.iter().map(|d| d.code).collect()
    }

    fn scan_warning(source: &str) -> Vec<Diagnostic> {
        let a = analysis(source);
        scan_module("m", &a, &HashSet::new(), Severity::Warning)
    }

    // --- sw-unavailable-global ---

    #[test]
    fn flags_pcall_reference_as_warning() {
        let diagnostics = scan_warning("function onTick()\n  pcall(function() end)\nend\n");
        assert_eq!(codes_of(&diagnostics), vec![codes::SW_UNAVAILABLE_GLOBAL]);
        assert_eq!(diagnostics[0].severity, Severity::Warning);
    }

    #[test]
    fn typo_global_is_not_flagged_by_sw_restrict() {
        // タイポ等の未知名は sw_restrict の対象外（従来通り undefined-global 側の責務）。
        let diagnostics = scan_warning("function onTick()\n  local x = pcaal\n  return x\nend\n");
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    #[test]
    fn self_defined_global_of_same_name_is_not_flagged() {
        let a = analysis("print = function(x) end\nprint(1)\n");
        let written: HashSet<String> = ["print".to_string()].into_iter().collect();
        let diagnostics = scan_module("m", &a, &written, Severity::Warning);
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    #[test]
    fn each_read_occurrence_is_reported() {
        let diagnostics = scan_warning(
            "function onTick()\n  local a = pcall\n  local b = pcall\n  return a, b\nend\n",
        );
        assert_eq!(
            codes_of(&diagnostics),
            vec![codes::SW_UNAVAILABLE_GLOBAL, codes::SW_UNAVAILABLE_GLOBAL]
        );
    }

    // --- input/output outside onTick ---

    #[test]
    fn flags_input_reference_at_top_level() {
        let diagnostics = scan_warning("local x = input.getNumber(1)\nreturn x\n");
        assert_eq!(codes_of(&diagnostics), vec![codes::INPUT_OUTSIDE_ONTICK]);
    }

    #[test]
    fn flags_output_reference_in_on_draw() {
        let diagnostics = scan_warning("function onDraw()\n  output.setNumber(1, 2)\nend\n");
        assert_eq!(codes_of(&diagnostics), vec![codes::OUTPUT_OUTSIDE_ONTICK]);
    }

    #[test]
    fn does_not_flag_input_inside_on_tick_funcstat_form() {
        let diagnostics =
            scan_warning("function onTick()\n  local x = input.getNumber(1)\n  return x\nend\n");
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    #[test]
    fn does_not_flag_output_inside_on_tick_assign_form() {
        let diagnostics = scan_warning("onTick = function()\n  output.setNumber(1, 2)\nend\n");
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    #[test]
    fn does_not_flag_reference_inside_nested_function_within_on_tick() {
        let diagnostics = scan_warning(
            "function onTick()\n  local function helper()\n    return input.getNumber(1)\n  end\n  helper()\nend\n",
        );
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    #[test]
    fn does_not_flag_reference_inside_function_reachable_from_on_tick() {
        // helper() は onTick から呼ばれる（call-graph で到達可能）ので許可される。
        let diagnostics = scan_warning(
            "function helper()\n  return input.getNumber(1)\nend\nfunction onTick()\n  helper()\nend\n",
        );
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    #[test]
    fn does_not_flag_reference_reachable_via_two_hop_call() {
        let diagnostics = scan_warning(
            "function inner()\n  return input.getNumber(1)\nend\nfunction outer()\n  return inner()\nend\nfunction onTick()\n  outer()\nend\n",
        );
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    #[test]
    fn does_not_flag_reference_reachable_via_local_function_and_local_form() {
        let diagnostics = scan_warning(
            "local function helper()\n  return input.getNumber(1)\nend\nonTick = function()\n  helper()\nend\n",
        );
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    #[test]
    fn does_not_flag_reference_reachable_via_table_field_call() {
        let diagnostics = scan_warning(
            "M = {}\nfunction M.helper()\n  return input.getNumber(1)\nend\nfunction onTick()\n  M.helper()\nend\n",
        );
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    #[test]
    fn flags_reference_inside_function_never_called_from_on_tick() {
        // helper は定義されているが onTick から (直接にも間接にも) 呼ばれない。
        let diagnostics = scan_warning(
            "function helper()\n  return input.getNumber(1)\nend\nfunction onTick()\nend\n",
        );
        assert_eq!(codes_of(&diagnostics), vec![codes::INPUT_OUTSIDE_ONTICK]);
    }

    #[test]
    fn flags_reference_when_function_value_is_passed_around_instead_of_called_by_name() {
        // g への直接呼び出し `helper()` ではなく、関数値を変数経由で渡して呼ぶケースは
        // 静的呼び出しグラフでは追跡しない（安全側で違反のまま）。
        let diagnostics = scan_warning(
            "function helper()\n  return input.getNumber(1)\nend\nfunction onTick()\n  local f = helper\n  f()\nend\n",
        );
        assert_eq!(codes_of(&diagnostics), vec![codes::INPUT_OUTSIDE_ONTICK]);
    }

    #[test]
    fn severity_error_is_honored_for_compile_project_call_site() {
        let a = analysis("local x = input.getNumber(1)\nreturn x\n");
        let diagnostics = scan_module("m", &a, &HashSet::new(), Severity::Error);
        assert_eq!(diagnostics[0].severity, Severity::Error);
    }
}
