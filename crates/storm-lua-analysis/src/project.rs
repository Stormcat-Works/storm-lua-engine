//! `LuaProject` 入力スキーマ（設計 §2）とモジュール別検証。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ambient_scan::{scan_ambient_refs, AmbientUsage};
use crate::diagnostic::{codes, Diagnostic};
use crate::require_scan::{find_require_calls, scan_requires, RequireSite};
use storm_lua_syntax::ast::{Ast, NodeId};
use storm_lua_syntax::parser::{parse_source_with_positions, NodePositions};

/// プロジェクト入力（設計 §2）。`modules` は決定的順序のため `BTreeMap`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LuaProject {
    /// `modules` のキーのいずれか。出力チャンクの本体になるモジュール。
    pub entry: String,
    /// モジュールキー → Lua ソース文字列。コアはパス概念を持たない（FS 非依存）。
    pub modules: BTreeMap<String, String>,
    /// ambient 名前空間（標準ライブラリ供給）。ルートグローバル名 → 定義（設計 §2）。
    #[serde(default)]
    pub ambient: BTreeMap<String, AmbientNamespace>,
}

/// ambient 名前空間1つ分の定義（設計 §2）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AmbientNamespace {
    /// Named ambient members supplied by the host.
    pub members: BTreeMap<String, AmbientMember>,
}

/// ambient メンバー1件（設計 §2）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AmbientMember {
    /// 注入可能: `source` は「値を return する Lua チャンク」（require モジュールと同形式）。
    Module {
        /// Lua source chunk that returns the injected value.
        source: String,
    },
    /// シミュレータ等の環境専用: エクスポート対象コードから参照されたらエラー。
    EnvironmentOnly,
}

/// モジュールキー文法（設計 §2.1）:
/// `key = segment ("." segment)*`、`segment = [A-Za-z_][A-Za-z0-9_]*`。
/// 区切りは `.` のみ。大文字小文字を区別する。
pub fn is_valid_module_key(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    key.split('.').all(is_valid_segment)
}

fn is_valid_segment(segment: &str) -> bool {
    let mut chars = segment.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// 正常にパースできたモジュール（通常モジュール・ambient `source` の両方）の解析結果。
pub struct ModuleAnalysis {
    /// Parsed syntax arena for this module.
    pub ast: Ast,
    /// Root block in the module arena.
    pub root: NodeId,
    /// Source-position table associated with this parsed arena.
    pub positions: NodePositions,
    /// 通常モジュールの require site。ambient `source` は require 自体が禁止（`require-in-ambient`）
    /// のため常に空（違反があれば structural エラーとなり、その ambient メンバーは
    /// `ambient_modules` に載らない — リンク段が空の requires を前提にできる）。
    pub requires: Vec<RequireSite>,
    /// このモジュール（または ambient `source`）内で検出された正当な ambient メンバー使用
    /// （tree shaking・ambient 内相互依存の閉包解決の入力。設計 §4.2）。
    pub ambient_usages: Vec<AmbientUsage>,
}

/// ambient `kind: module` メンバーのキー（ルート名, メンバー名）。
pub type AmbientModuleKey = (String, String);

/// `validate_project` の結果。構文エラー等でパースできなかったモジュールは
/// `modules` に現れず、代わりに `diagnostics` に `syntax-error` 等が積まれる
/// （構文エラーで throw しない設計 §5.1 の方針を P1a の時点から踏襲する）。
pub struct ProjectValidation {
    /// Diagnostics collected while processing the input.
    pub diagnostics: Vec<Diagnostic>,
    /// Successfully parsed modules keyed by their logical names.
    pub modules: BTreeMap<String, ModuleAnalysis>,
    /// `kind: module` の ambient メンバーの解析結果。`source` の構文解析に成功した分は
    /// （`require-in-ambient` 等の違反診断が同時に出ていても）ここに載る。
    /// リンク段が実際に注入するのは `diagnostics` に error が1件も無い場合のみなので、
    /// 違反ケースでこのマップが使われることはない（`link_project` は has_error() で早期リターンする）。
    pub ambient_modules: BTreeMap<AmbientModuleKey, ModuleAnalysis>,
}

/// モジュールキーの先頭セグメント（`.` 区切りの最初の1つ）。
fn first_segment(key: &str) -> &str {
    key.split('.').next().unwrap_or(key)
}

/// `LuaProject` を検証する:
/// - entry が modules に存在するか（`entry-not-found`）
/// - 各モジュールキーの文法（`invalid-module-key`）と ambient ルート名予約との衝突
/// - 各モジュールの構文解析（失敗は `syntax-error`）
/// - require 検出と制限チェック（`require-not-top-level` / `require-not-statement` / `require-dynamic`）
/// - ambient 参照規則違反（`unknown-ambient-member` 等、設計 §4.1）
/// - ambient `kind: module` メンバーの `source` 自体の解析（`require-in-ambient` を含む）
///
/// `module-not-found` / `require-cycle` は依存 DFS（P1b）側の責務であり、ここでは発火しない。
pub fn validate_project(project: &LuaProject) -> ProjectValidation {
    validate_project_mode(project, true)
}

/// Validate source chunks for execution, not for the static linker.
/// The host owns dynamic include resolution; no module return values are assumed.
pub(crate) fn validate_runtime_project(project: &LuaProject) -> ProjectValidation {
    validate_project_mode(project, false)
}

fn validate_project_mode(project: &LuaProject, static_link: bool) -> ProjectValidation {
    let mut diagnostics = Vec::new();
    let mut modules = BTreeMap::new();

    if !project.modules.contains_key(&project.entry) {
        diagnostics.push(Diagnostic::error(
            codes::ENTRY_NOT_FOUND,
            format!(
                "entry module \"{}\" is not present in modules.",
                project.entry
            ),
        ));
    }

    for (key, source) in &project.modules {
        let key_valid = is_valid_module_key(key);
        if !key_valid {
            diagnostics.push(
                Diagnostic::error(
                    codes::INVALID_MODULE_KEY,
                    format!("module key \"{key}\" does not match the module key grammar."),
                )
                .with_module(key.clone()),
            );
        }
        // ambient のルート名（例: `sim`）を先頭セグメントに持つキーは予約（設計 §2.1）。
        // 文法違反と重複しても両方報告する（別々の原因なのでどちらも隠さない）。
        if project.ambient.contains_key(first_segment(key)) {
            diagnostics.push(
                Diagnostic::error(
                    codes::INVALID_MODULE_KEY,
                    format!(
                        "module key \"{key}\" is reserved by the ambient namespace \"{}\".",
                        first_segment(key)
                    ),
                )
                .with_module(key.clone()),
            );
        }

        match parse_source_with_positions(source) {
            Ok((ast, root, positions)) => {
                let (requires, require_diagnostics) = if static_link {
                    scan_requires(&ast, root, &positions, key)
                } else {
                    (Vec::new(), Vec::new())
                };
                diagnostics.extend(require_diagnostics);
                let (ambient_usages, ambient_diagnostics) =
                    scan_ambient_refs(&ast, root, &positions, key, &project.ambient);
                diagnostics.extend(ambient_diagnostics);
                modules.insert(
                    key.clone(),
                    ModuleAnalysis {
                        ast,
                        root,
                        positions,
                        requires,
                        ambient_usages,
                    },
                );
            }
            Err(error) => {
                diagnostics.push(
                    Diagnostic::error(codes::SYNTAX_ERROR, error.to_string())
                        .with_module(key.clone()),
                );
            }
        }
    }

    let ambient_modules = validate_ambient_modules(project, &mut diagnostics);

    ProjectValidation {
        diagnostics,
        modules,
        ambient_modules,
    }
}

/// ambient `kind: module` メンバーの `source` を個別に検証する（設計 §4.2）:
/// - 構文解析（失敗は `syntax-error`、診断上のモジュール名は `"root.member"`）
/// - `source` 内の require 呼び出しは形式を問わず全て `require-in-ambient`
/// - 参照規則違反（sibling ambient メンバーの誤用も含め、通常モジュールと同じ規則。設計 §4.2）
fn validate_ambient_modules(
    project: &LuaProject,
    diagnostics: &mut Vec<Diagnostic>,
) -> BTreeMap<AmbientModuleKey, ModuleAnalysis> {
    let mut ambient_modules = BTreeMap::new();
    for (root, namespace) in &project.ambient {
        for (member, definition) in &namespace.members {
            let AmbientMember::Module { source } = definition else {
                continue;
            };
            let label = format!("{root}.{member}");
            match parse_source_with_positions(source) {
                Ok((ast, node_root, positions)) => {
                    for call_id in find_require_calls(&ast, node_root) {
                        let range = positions
                            .get(call_id)
                            .map(|(line, col)| crate::diagnostic::Range::point(line, col));
                        diagnostics.push(
                            Diagnostic::error(
                                codes::REQUIRE_IN_AMBIENT,
                                "require() is not allowed inside an ambient namespace member's source.",
                            )
                            .with_module(label.clone())
                            .with_range(range),
                        );
                    }
                    let (ambient_usages, ambient_diagnostics) =
                        scan_ambient_refs(&ast, node_root, &positions, &label, &project.ambient);
                    diagnostics.extend(ambient_diagnostics);
                    ambient_modules.insert(
                        (root.clone(), member.clone()),
                        ModuleAnalysis {
                            ast,
                            root: node_root,
                            positions,
                            requires: Vec::new(),
                            ambient_usages,
                        },
                    );
                }
                Err(error) => {
                    diagnostics.push(
                        Diagnostic::error(codes::SYNTAX_ERROR, error.to_string())
                            .with_module(label.clone()),
                    );
                }
            }
        }
    }
    ambient_modules
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::diagnostic::codes;

    fn project(entry: &str, modules: &[(&str, &str)]) -> LuaProject {
        LuaProject {
            entry: entry.to_string(),
            modules: modules
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ambient: BTreeMap::new(),
        }
    }

    // --- モジュールキー文法（§2.1）: 有効/無効テーブルテスト ---

    #[test]
    fn module_key_grammar_accepts_valid_keys() {
        for key in ["main", "a", "_priv", "lib.util", "a.b.c", "a1.b2", "_.a"] {
            assert!(is_valid_module_key(key), "should accept: {key}");
        }
    }

    #[test]
    fn module_key_grammar_rejects_invalid_keys() {
        for key in [
            "",       // 空
            ".",      // セグメントが空のみ
            "a.",     // 末尾ドット→空セグメント
            ".a",     // 先頭ドット→空セグメント
            "a..b",   // 連続ドット→空セグメント
            "1abc",   // セグメント先頭が数字
            "a-b",    // ハイフン不可
            "a/b",    // スラッシュ区切り不可
            "a b",    // 空白不可
            "a.b.1c", // いずれかのセグメントが数字始まり
        ] {
            assert!(!is_valid_module_key(key), "should reject: {key}");
        }
    }

    // --- entry / modules 検証 ---

    #[test]
    fn entry_not_found_when_missing_from_modules() {
        let p = project("main", &[("other", "return 1")]);
        let result = validate_project(&p);
        assert!(result
            .diagnostics
            .iter()
            .any(|d| d.code == codes::ENTRY_NOT_FOUND && d.module.is_none()));
    }

    #[test]
    fn entry_present_does_not_report_entry_not_found() {
        let p = project("main", &[("main", "return 1")]);
        let result = validate_project(&p);
        assert!(!result
            .diagnostics
            .iter()
            .any(|d| d.code == codes::ENTRY_NOT_FOUND));
    }

    #[test]
    fn invalid_module_key_is_reported_with_module_field() {
        let p = project("main", &[("main", "return 1"), ("bad-key", "return 2")]);
        let result = validate_project(&p);
        let diag = result
            .diagnostics
            .iter()
            .find(|d| d.code == codes::INVALID_MODULE_KEY)
            .expect("invalid-module-key diagnostic");
        assert_eq!(diag.module.as_deref(), Some("bad-key"));
    }

    #[test]
    fn syntax_error_is_reported_per_module_and_other_modules_still_analyzed() {
        let p = project("main", &[("main", "local a = ("), ("ok", "local b = 1")]);
        let result = validate_project(&p);
        let diag = result
            .diagnostics
            .iter()
            .find(|d| d.code == codes::SYNTAX_ERROR)
            .expect("syntax-error diagnostic");
        assert_eq!(diag.module.as_deref(), Some("main"));
        // 構文エラーのモジュールは modules に現れない
        assert!(!result.modules.contains_key("main"));
        // 他モジュールは解析が続行される
        assert!(result.modules.contains_key("ok"));
    }

    #[test]
    fn valid_project_collects_module_analysis_for_every_module() {
        let p = project(
            "main",
            &[
                ("main", "local a = require(\"lib.util\")"),
                ("lib.util", "return {}"),
            ],
        );
        let result = validate_project(&p);
        assert!(
            result.diagnostics.is_empty(),
            "{:?}",
            diag_summary(&result.diagnostics)
        );
        assert_eq!(result.modules.len(), 2);
        let main = &result.modules["main"];
        assert_eq!(main.requires.len(), 1);
        assert_eq!(main.requires[0].key, "lib.util");
        assert_eq!(main.requires[0].binding.as_deref(), Some("a"));
    }

    fn diag_summary(diagnostics: &[Diagnostic]) -> Vec<&str> {
        diagnostics.iter().map(|d| d.code).collect()
    }
}
