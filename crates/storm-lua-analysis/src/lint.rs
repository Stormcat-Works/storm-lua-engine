//! v1 リント規則5種（設計 §5.2、luacheck Tier A 相当）。
//!
//! 実装は resolver（`crate::resolver::resolve`）の既存出力（binding の kind/decl/freq、
//! scope の親子関係、node_bid/node_write の並列配列）の読み出しが主体。
//! resolver 本体・minify 経路の挙動は一切変更しない（読み出し専用）。
//!
//! **位置の注意**: AST は識別子を `SymbolId`（Interner 添字）として持ち、Name ノードのような
//! 個別の位置つきノードを local/param/for 変数の宣言側には持たない（`ast.rs` 参照）。
//! そのため宣言系リント（unused-*/shadowed-local）の診断位置は「宣言を含む文（decl_node）」
//! までが取得できる最も細かい粒度であり、識別子そのものの列位置ではない
//! （読み取り側は Name ノードとして実在するため、`undefined-global` は識別子位置そのもの）。

use std::collections::HashSet;

use crate::diagnostic::{codes, Diagnostic, Range};
use crate::project::ModuleAnalysis;
use crate::resolver::{
    api_roots, component_api_roots, reserved, resolve, BindingId, BindingKind, Resolution,
};
use crate::sw_restrict::is_unavailable_builtin;
use storm_lua_syntax::ast::{Ast, Node};

/// モジュール群のグローバル書き込み名を収集する。実行時のグローバル環境は
/// プロジェクト全体で共有される（リンク後は1チャンク）ため、`undefined-global` の
/// 「書き込みあり」判定はモジュール単位ではなくプロジェクト全体で行う。
pub fn collect_written_global_names<'a>(
    modules: impl Iterator<Item = &'a ModuleAnalysis>,
) -> HashSet<String> {
    let mut names = HashSet::new();
    for analysis in modules {
        let resolution = resolve(&analysis.ast, analysis.root);
        for &(symbol, bid) in &resolution.globals {
            if resolution.binding_write_counts[bid as usize] > 0 {
                names.insert(analysis.ast.strings.get(symbol).to_string());
            }
        }
    }
    names
}

/// モジュール `key` に対する v1 リント5規則をすべて実行する。
/// `written_globals` は `collect_written_global_names` によるプロジェクト全体の書き込み集合。
/// `ambient_roots` は `LuaProject.ambient` のルート名集合（`sim` 等）。ambient は
/// スクリプト側の write なしに提供される既知グローバルであり、`undefined-global` の
/// 対象外にする（参照規則違反そのものは `ambient_scan` が別途診断する）。
pub fn lint_module(
    key: &str,
    analysis: &ModuleAnalysis,
    written_globals: &HashSet<String>,
    ambient_roots: &HashSet<String>,
) -> Vec<Diagnostic> {
    lint_module_with_roots(key, analysis, written_globals, ambient_roots, api_roots)
}

/// Component Mod プロファイルに対する [`lint_module`]。既知ルートの判定だけが
/// Component のルート集合（`component` / `parser` / `matrix` 等）に切り替わります。
/// コールバック名は `reserved` 側で両プロファイル分が予約済みです。
pub fn lint_module_component(
    key: &str,
    analysis: &ModuleAnalysis,
    written_globals: &HashSet<String>,
    ambient_roots: &HashSet<String>,
) -> Vec<Diagnostic> {
    lint_module_with_roots(
        key,
        analysis,
        written_globals,
        ambient_roots,
        component_api_roots,
    )
}

fn lint_module_with_roots(
    key: &str,
    analysis: &ModuleAnalysis,
    written_globals: &HashSet<String>,
    ambient_roots: &HashSet<String>,
    roots: fn(&str) -> bool,
) -> Vec<Diagnostic> {
    let resolution = resolve(&analysis.ast, analysis.root);
    let mut diagnostics = Vec::new();
    diagnostics.extend(undefined_global_diagnostics(
        key,
        analysis,
        &resolution,
        written_globals,
        ambient_roots,
        roots,
    ));
    diagnostics.extend(unused_binding_diagnostics(key, analysis, &resolution));
    diagnostics.extend(shadowed_local_diagnostics(key, analysis, &resolution));
    diagnostics
}

/// `require` は resolver 視点では単なるグローバル関数呼び出しの対象（`require_scan` が
/// 別途トップレベル・文字列リテラル制約を検証する）。`api_roots`/`reserved` はコンパイラ本体
/// の既知集合であり `require` を含まないため、undefined-global 判定では個別に既知扱いする。
/// ambient ルート名（`sim` 等）も同様にプロジェクト固有の既知集合として個別に扱う。
/// `sw_restrict::is_unavailable_builtin` に該当する名前（`pcall` 等）は `sw-unavailable-global`
/// が単独の正本として発報する。ここで二重に `undefined-global` を出さないよう対象外にする。
fn is_known_global(name: &str, ambient_roots: &HashSet<String>, roots: fn(&str) -> bool) -> bool {
    roots(name)
        || reserved(name)
        || name == "require"
        || name == "_ENV"
        || ambient_roots.contains(name)
        || is_unavailable_builtin(name)
}

/// 各 binding の「読み取り」参照数（write ではない参照の数）。
/// `Binding::freq` は読み書き両方を数えるため、unused 判定には使えない
/// （luacheck 慣例: 書き込みのみは未使用扱い。§5.2 の task 指示どおり）。
///
/// **注意**: resolver は Fornum/Localfunc の宣言文自身の NodeId にも便宜上
/// `node_bid` を設定する（`resolve_dump` 向けの露出であり参照ではない。`resolver.rs`
/// の `block_node` 参照）。これは Name ノードではないため、Name ノードのみを実参照として
/// 数えることで誤カウントを避ける。
fn compute_read_counts(ast: &Ast, resolution: &Resolution) -> Vec<u32> {
    let mut counts = vec![0u32; resolution.bindings.len()];
    for (i, bid) in resolution.node_bid.iter().enumerate() {
        if let Some(bid) = bid {
            if !resolution.node_write[i] && matches!(ast.node(i as u32), Node::Name(_)) {
                counts[*bid as usize] += 1;
            }
        }
    }
    counts
}

/// `undefined-global`: 既知グローバル（Stormworks API・予約語・`require`）にも書き込みにも
/// 該当しないグローバル読み取り。1回の読み取りにつき1診断（識別子の実位置で報告）。
fn undefined_global_diagnostics(
    key: &str,
    analysis: &ModuleAnalysis,
    resolution: &Resolution,
    written_globals: &HashSet<String>,
    ambient_roots: &HashSet<String>,
    roots: fn(&str) -> bool,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for &(symbol, bid) in &resolution.globals {
        let name = analysis.ast.strings.get(symbol);
        if is_known_global(name, ambient_roots, roots) {
            continue;
        }
        // プロジェクト中のどこかで書き込まれていれば「定義済み」扱い（グローバル環境は
        // プロジェクト全体で共有されるため、別モジュールでの代入も定義とみなす）。
        // require で束縛された名前は Local binding になる（require_scan が
        // `local NAME = require(...)` として検出するため）のでここには現れない。
        if written_globals.contains(name) {
            continue;
        }
        for (node_id, node_bid) in resolution.node_bid.iter().enumerate() {
            if *node_bid != Some(bid) || resolution.node_write[node_id] {
                continue;
            }
            let range = analysis
                .positions
                .get(node_id as u32)
                .map(|(line, col)| Range::point(line, col));
            out.push(
                Diagnostic::warning(
                    codes::UNDEFINED_GLOBAL,
                    format!("global \"{name}\" is never assigned anywhere in this project."),
                )
                .with_module(key.to_string())
                .with_range(range),
            );
        }
    }
    out
}

/// `unused-local` / `unused-parameter` / `unused-loop-variable`:
/// 一度も読まれない local/param/for 変数（`_` プレフィックスは param/for のみ除外。
/// luacheck 慣例に合わせ local には適用しない — local はゼロ幅の慣習が無いため）。
fn unused_binding_diagnostics(
    key: &str,
    analysis: &ModuleAnalysis,
    resolution: &Resolution,
) -> Vec<Diagnostic> {
    let read_counts = compute_read_counts(&analysis.ast, resolution);
    let mut out = Vec::new();
    for (bid, binding) in resolution.bindings.iter().enumerate().skip(1) {
        if read_counts[bid] > 0 {
            continue;
        }
        let name = analysis.ast.strings.get(binding.name);
        let (code, message, underscore_exempt) = match binding.kind {
            BindingKind::Local => (
                codes::UNUSED_LOCAL,
                format!("local \"{name}\" is never read."),
                false,
            ),
            BindingKind::Localfunc => (
                codes::UNUSED_LOCAL,
                format!("local function \"{name}\" is never read."),
                false,
            ),
            BindingKind::Param => (
                codes::UNUSED_PARAMETER,
                format!("parameter \"{name}\" is never read."),
                true,
            ),
            BindingKind::For => (
                codes::UNUSED_LOOP_VARIABLE,
                format!("loop variable \"{name}\" is never read."),
                true,
            ),
            BindingKind::Global => continue,
        };
        if underscore_exempt && name.starts_with('_') {
            continue;
        }
        let range = binding
            .decl_node
            .and_then(|n| analysis.positions.get(n))
            .map(|(line, col)| Range::point(line, col));
        out.push(
            Diagnostic::warning(code, message)
                .with_module(key.to_string())
                .with_range(range),
        );
    }
    out
}

/// binding `bid` が、同一スコープまたは外側スコープに存在する「より先に宣言された」
/// 同名 local/param/for/localfunc を隠しているか（global は対象外）。
/// 同一スコープを含めるのは、`function f(x) local x = ... end` のように param と body
/// 直下 local が resolver 上は同じ scope に属するため（`resolver.rs` の block_node 参照）。
fn shadows_earlier_binding(resolution: &Resolution, bid: BindingId) -> bool {
    let binding = resolution.binding(bid);
    let mut scope_id = Some(binding.scope);
    while let Some(sid) = scope_id {
        let scope = resolution.scope(sid);
        for &other_bid in &scope.bindings {
            if other_bid == bid {
                continue;
            }
            let other = resolution.binding(other_bid);
            if matches!(other.kind, BindingKind::Global) {
                continue;
            }
            if other.name == binding.name && other.decl < binding.decl {
                return true;
            }
        }
        scope_id = scope.parent;
    }
    false
}

/// `shadowed-local`: 外側（自スコープ含む）の local/param/for/localfunc を同名で再宣言。
fn shadowed_local_diagnostics(
    key: &str,
    analysis: &ModuleAnalysis,
    resolution: &Resolution,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (bid, binding) in resolution.bindings.iter().enumerate().skip(1) {
        if matches!(binding.kind, BindingKind::Global) {
            continue;
        }
        if !shadows_earlier_binding(resolution, bid as BindingId) {
            continue;
        }
        let name = analysis.ast.strings.get(binding.name);
        let range = binding
            .decl_node
            .and_then(|n| analysis.positions.get(n))
            .map(|(line, col)| Range::point(line, col));
        out.push(
            Diagnostic::warning(
                codes::SHADOWED_LOCAL,
                format!("local \"{name}\" shadows an outer local of the same name."),
            )
            .with_module(key.to_string())
            .with_range(range),
        );
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::require_scan::scan_requires;
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

    fn lint(source: &str) -> Vec<Diagnostic> {
        let a = analysis(source);
        let written = collect_written_global_names(std::iter::once(&a));
        lint_module("m", &a, &written, &HashSet::new())
    }

    /// 別モジュールでのグローバル代入も「定義済み」扱いになる
    /// （グローバル環境はプロジェクト全体で共有されるため）。
    #[test]
    fn global_written_in_another_module_is_not_undefined() {
        let writer = analysis("shared = 1\n");
        let reader = analysis("function onTick()\n  output.setNumber(1, shared)\nend\n");
        let written = collect_written_global_names([&writer, &reader].into_iter());
        let diagnostics = lint_module("reader", &reader, &written, &HashSet::new());
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    // --- undefined-global ---

    #[test]
    fn flags_read_of_never_written_global() {
        let diagnostics = lint("function onTick()\n  output.setNumber(1, foo)\nend\n");
        assert_eq!(codes_of(&diagnostics), vec![codes::UNDEFINED_GLOBAL]);
    }

    #[test]
    fn does_not_flag_global_written_before_read() {
        let diagnostics = lint("foo = 1\nfunction onTick()\n  output.setNumber(1, foo)\nend\n");
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    #[test]
    fn does_not_flag_known_api_roots_or_callbacks() {
        let diagnostics = lint(
            "function onTick()\n  local x = math.floor(1)\n  output.setNumber(1, x)\nend\nfunction onDraw() end\n",
        );
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    #[test]
    fn component_profile_knows_component_roots_and_callbacks() {
        let source = "function onParse()\n  local v = component.getInputLogicSlotBool(1)\n  local n = parser.parseNumber(\"k\", 1)\n  if v then debug.log(n) end\nend\nfunction onRender() end\n";
        let a = analysis(source);
        let written = collect_written_global_names(std::iter::once(&a));
        let diagnostics = lint_module_component("m", &a, &written, &HashSet::new());
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
        // 車両プロファイルでは component / parser は既知ルートではない。
        let vehicle = lint_module("m", &a, &written, &HashSet::new());
        assert!(
            codes_of(&vehicle).contains(&codes::UNDEFINED_GLOBAL),
            "{:?}",
            codes_of(&vehicle)
        );
    }

    #[test]
    fn does_not_flag_require_bound_name_as_undefined_global() {
        // require で束縛された名前は resolver 上 Local binding になる（Global ではない）。
        let diagnostics = lint("local util = require(\"util\")\nutil.f()\n");
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    #[test]
    fn reports_one_diagnostic_per_undefined_global_read_occurrence() {
        // a/b は自身も参照される（unused-local を誘発させないため）。
        let diagnostics = lint(
            "function onTick()\n  local a = foo\n  local b = foo\n  output.setNumber(1, a)\n  output.setNumber(2, b)\nend\n",
        );
        assert_eq!(
            codes_of(&diagnostics),
            vec![codes::UNDEFINED_GLOBAL, codes::UNDEFINED_GLOBAL]
        );
    }

    // --- unused-local ---

    #[test]
    fn flags_local_never_read() {
        let diagnostics = lint("local a = 1\n");
        assert_eq!(codes_of(&diagnostics), vec![codes::UNUSED_LOCAL]);
    }

    #[test]
    fn does_not_flag_local_that_is_read() {
        let diagnostics = lint("local a = 1\nfunction onTick()\n  local b = a\nend\n");
        // b も未使用なので b 分は出る。a は使われているので出ない。
        assert_eq!(codes_of(&diagnostics), vec![codes::UNUSED_LOCAL]);
    }

    #[test]
    fn flags_write_only_local_as_unused() {
        // luacheck 慣例: 書き込みのみ（再代入のみ）は未使用扱い（読み取りが一度もない）。
        let diagnostics = lint("local a = 1\na = 2\n");
        assert_eq!(codes_of(&diagnostics), vec![codes::UNUSED_LOCAL]);
    }

    #[test]
    fn does_not_exempt_underscore_prefixed_local() {
        // '_' 除外は param/loop 変数の慣習であり、local には適用しない。
        let diagnostics = lint("local _unused = 1\n");
        assert_eq!(codes_of(&diagnostics), vec![codes::UNUSED_LOCAL]);
    }

    // --- unused-parameter ---

    #[test]
    fn flags_unread_parameter() {
        let diagnostics = lint("function f(x) end\nf(1)\n");
        assert_eq!(codes_of(&diagnostics), vec![codes::UNUSED_PARAMETER]);
    }

    #[test]
    fn does_not_flag_read_parameter() {
        let diagnostics = lint("function f(x) return x end\nf(1)\n");
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    #[test]
    fn exempts_underscore_prefixed_parameter() {
        let diagnostics = lint("function f(_x) end\nf(1)\n");
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    // --- unused-loop-variable ---

    #[test]
    fn flags_unread_fornum_variable() {
        let diagnostics = lint("function f()\n  for i = 1, 10 do end\nend\nf()\n");
        assert_eq!(codes_of(&diagnostics), vec![codes::UNUSED_LOOP_VARIABLE]);
    }

    #[test]
    fn flags_unread_forin_variable() {
        let diagnostics = lint("function f(t)\n  for k, v in pairs(t) do end\nend\nf({})\n");
        assert_eq!(
            codes_of(&diagnostics),
            vec![codes::UNUSED_LOOP_VARIABLE, codes::UNUSED_LOOP_VARIABLE]
        );
    }

    #[test]
    fn does_not_flag_read_loop_variable() {
        let diagnostics = lint(
            "function f()\n  local sum = 0\n  for i = 1, 10 do\n    sum = sum + i\n  end\nend\nf()\n",
        );
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    #[test]
    fn exempts_underscore_prefixed_loop_variable() {
        let diagnostics = lint("function f(t)\n  for _k, v in pairs(t) do end\nend\nf({})\n");
        assert_eq!(codes_of(&diagnostics), vec![codes::UNUSED_LOOP_VARIABLE]);
    }

    // --- shadowed-local ---

    #[test]
    fn flags_nested_block_shadowing_outer_local() {
        let diagnostics =
            lint("local x = 1\nfunction f()\n  local x = 2\n  return x\nend\nf()\nreturn x\n");
        assert_eq!(codes_of(&diagnostics), vec![codes::SHADOWED_LOCAL]);
    }

    #[test]
    fn flags_parameter_shadowed_by_same_scope_local() {
        let diagnostics = lint("function f(x)\n  local x = x + 1\n  return x\nend\nf(1)\n");
        assert_eq!(codes_of(&diagnostics), vec![codes::SHADOWED_LOCAL]);
    }

    #[test]
    fn flags_loop_variable_shadowed_by_inner_local() {
        let diagnostics = lint(
            "function f()\n  for i = 1, 10 do\n    local i = i * 2\n    return i\n  end\nend\nf()\n",
        );
        assert_eq!(codes_of(&diagnostics), vec![codes::SHADOWED_LOCAL]);
    }

    #[test]
    fn does_not_flag_sibling_scopes_reusing_the_same_name() {
        // 2つの独立した for ループはどちらも「外側」の関係にない。
        let diagnostics = lint(
            "function f()\n  local sum = 0\n  for i = 1, 10 do sum = sum + i end\n  for i = 1, 10 do sum = sum + i end\n  return sum\nend\nf()\n",
        );
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    #[test]
    fn does_not_flag_unrelated_names_in_nested_scope() {
        let diagnostics =
            lint("local x = 1\nfunction f()\n  local y = 2\n  return y\nend\nf()\nreturn x\n");
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    // --- sw_restrict との重複回避 ---

    #[test]
    fn does_not_flag_known_unavailable_builtin_as_undefined_global() {
        // pcall は sw_restrict::UNAVAILABLE_LUA_BUILTINS 側の単独責務
        // （`sw-unavailable-global` が発報するので、ここで `undefined-global` は出ない）。
        let diagnostics = lint("function onTick()\n  pcall(function() end)\nend\n");
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    #[test]
    fn still_flags_typo_of_a_known_builtin_name_as_undefined_global() {
        // "pcaal" は既知ビルトインリストに載っていないタイポなので、従来通り undefined-global。
        let diagnostics = lint("function onTick()\n  local x = pcaal\n  return x\nend\n");
        assert_eq!(codes_of(&diagnostics), vec![codes::UNDEFINED_GLOBAL]);
    }
}
