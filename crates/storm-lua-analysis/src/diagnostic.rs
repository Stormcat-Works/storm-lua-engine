//! プロジェクトAPI設計 §7 の新 Diagnostic スキーマ。
//!
//! `compile()` / `compileProject()` / `analyze()` すべてがこの型を共有する。
//! `code` は kebab-case の安定識別子（一度公開したら意味を変えない。廃止は可、再利用は不可）。
//! カタログは §7.1（`docs/design-20260821-stormmin-project-api.md`）が正本。

use serde::Serialize;

/// Diagnostic importance; errors prevent the affected build from succeeding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Invalid source, project, or configuration.
    Error,
    /// A potentially unintended construct that can still be processed.
    Warning,
    /// Informational diagnostic.
    Info,
}

/// 1-based の位置範囲。`end_line`/`end_col` は省略可（単一点の診断など）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Range {
    /// One-based source line.
    pub line: u32,
    /// One-based UTF-8 byte column in the source line.
    pub col: u32,
    /// Optional ending source line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    /// Optional ending UTF-8 byte column.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_col: Option<u32>,
}

impl Range {
    /// NodePositions から得た1点 (line, col) のみの範囲。
    pub fn point(line: u32, col: u32) -> Self {
        Self {
            line,
            col,
            end_line: None,
            end_col: None,
        }
    }
}

/// Machine-readable diagnostic code, display text, and optional source location.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    /// Stable diagnostic identifier for matching and localization.
    pub code: &'static str,
    /// Diagnostic importance.
    pub severity: Severity,
    /// Human-readable diagnostic text.
    pub message: String,
    /// Logical module key, or None for a project-wide diagnostic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// Source position when it is known; never an invented fallback position.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
}

impl Diagnostic {
    /// Create an error diagnostic without a module or location.
    pub fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Error,
            message: message.into(),
            module: None,
            range: None,
        }
    }

    /// Create a warning diagnostic without a module or location.
    pub fn warning(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Warning,
            message: message.into(),
            module: None,
            range: None,
        }
    }

    /// Attach the logical source module key.
    pub fn with_module(mut self, module: impl Into<String>) -> Self {
        self.module = Some(module.into());
        self
    }

    /// Attach a source range when one is available.
    pub fn with_range(mut self, range: Option<Range>) -> Self {
        self.range = range;
        self
    }
}

/// §7.1 コードカタログ v1。実装済み/未実装を問わず、意図した採番の正本をここへ集約する。
pub mod codes {
    /// Reflection/host semantics require exact-token compaction, not whole-program transformations.
    pub const CONSERVATIVE_MINIFICATION: &str = "conservative-minification";
    /// Invalid combination of environment profile and host binding settings.
    pub const INVALID_ENVIRONMENT: &str = "invalid-environment";
    /// Invalid host-supplied source label.
    pub const INVALID_SOURCE_NAME: &str = "invalid-source-name";

    /// Requested optimization identifier is unknown or has been removed.
    pub const UNKNOWN_OPTIMIZATION_PASS: &str = "unknown-optimization-pass";
    // --- プロジェクト構造 (error) ---
    /// The entry module is absent from the supplied source set.
    pub const ENTRY_NOT_FOUND: &str = "entry-not-found";
    /// A module key violates the grammar or a reserved namespace.
    pub const INVALID_MODULE_KEY: &str = "invalid-module-key";
    /// P1b（DFS）で発火。P1aでは定数のみ用意する。
    pub const MODULE_NOT_FOUND: &str = "module-not-found";
    /// P1b（DFS）で発火。P1aでは定数のみ用意する。
    pub const REQUIRE_CYCLE: &str = "require-cycle";
    /// A module is not syntactically valid Lua.
    pub const SYNTAX_ERROR: &str = "syntax-error";

    // --- require 制限 (error) ---
    /// A module dependency appears outside an allowed top-level location.
    pub const REQUIRE_NOT_TOP_LEVEL: &str = "require-not-top-level";
    /// The require form is not an allowed dependency statement.
    pub const REQUIRE_NOT_STATEMENT: &str = "require-not-statement";
    /// A require target is not a statically known module key.
    pub const REQUIRE_DYNAMIC: &str = "require-dynamic";
    /// An injected ambient definition tries to require another module.
    pub const REQUIRE_IN_AMBIENT: &str = "require-in-ambient";

    // --- ambient 規則 (error, §4.1/§4.2) ---
    /// `members` に存在しないメンバー名への `R.name` 参照（typo 検出）。
    pub const UNKNOWN_AMBIENT_MEMBER: &str = "unknown-ambient-member";
    /// `kind: 'environmentOnly'` メンバーをエクスポート対象コードから参照した。
    pub const ENVIRONMENT_ONLY_API: &str = "environment-only-api";
    /// ambient ルート名 `R` 自体を値として使う（`local s = R` 等）。
    pub const AMBIENT_ROOT_ESCAPES: &str = "ambient-root-escapes";
    /// `R[expr]` の動的アクセス。
    pub const AMBIENT_DYNAMIC_ACCESS: &str = "ambient-dynamic-access";
    /// `R` または `R.name` への代入（ambient は読み取り専用）。
    pub const AMBIENT_ASSIGNED: &str = "ambient-assigned";

    // --- 既存 compile() 診断の新スキーマ移行分（新規採番。カタログv1と非衝突） ---
    /// `_ENV` への直接アクセス（closed-world global 前提を崩す）。
    pub const ENV_ACCESS: &str = "env-access";
    /// `rawget`/`rawset` の動的キー使用（field transformation 対象外）。
    pub const DYNAMIC_TABLE_KEY: &str = "dynamic-table-key";
    /// Stormworks 実行環境で利用不可/未サポートの API 呼び出し。
    pub const UNSUPPORTED_API_CALL: &str = "unsupported-api-call";
    /// `compile()` の探索/検証パイプライン内部失敗（構文は妥当だが、意味的に妥当な
    /// 候補が生き残らない等）。構文エラーは `SYNTAX_ERROR` を使う。
    pub const COMPILE_FAILED: &str = "compile-failed";

    // --- リント (warning, §5.2) ---
    /// 既知グローバル（API・予約語）にも書き込みにも該当しない読み取り。
    pub const UNDEFINED_GLOBAL: &str = "undefined-global";
    /// 一度も読まれない local（localfunc を含む）。
    pub const UNUSED_LOCAL: &str = "unused-local";
    /// 一度も読まれない仮引数（`_` プレフィックスは除外）。
    pub const UNUSED_PARAMETER: &str = "unused-parameter";
    /// 一度も読まれないループ変数（`_` プレフィックスは除外）。
    pub const UNUSED_LOOP_VARIABLE: &str = "unused-loop-variable";
    /// 同一/外側スコープの local を同名で再宣言（パラメータ・ループ変数を含む）。
    pub const SHADOWED_LOCAL: &str = "shadowed-local";

    // --- 抑制アノテーション (error, §7.2) ---
    /// `--@storm` プレフィックスの行コメントが未知の指示・引数を持つ（サイレント無視しない）。
    pub const UNKNOWN_STORM_DIRECTIVE: &str = "unknown-storm-directive";

    // --- Stormworks 固有制限検出 (v0.6.0, severity は呼び出し側が指定。`sw_restrict` 参照) ---
    // 検出ロジックは `sw_restrict` に一箇所だけ実装し、severity は呼び出し側が選ぶ:
    // `analyze()` は warning（リンター警告。実行は妨げない）、`compile_project()` は
    // error（`ok:false` に反映。Minify 経路は失敗させる）。
    /// 標準 Lua ビルトインとして既知だが Stormworks サンドボックス（`resolver::API_ROOTS`）
    /// には存在しないグローバルへの参照（自前で同名グローバルを定義した場合を除く）。
    pub const SW_UNAVAILABLE_GLOBAL: &str = "sw-unavailable-global";
    /// `onTick` に代入される関数リテラルの本体（ネストした内側関数を含む）以外での
    /// `input.<member>` 参照。
    pub const INPUT_OUTSIDE_ONTICK: &str = "input-outside-ontick";
    /// `onTick` に代入される関数リテラルの本体（ネストした内側関数を含む）以外での
    /// `output.<member>` 参照。
    pub const OUTPUT_OUTSIDE_ONTICK: &str = "output-outside-ontick";
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// §7 のスキーマ通りに camelCase でシリアライズされ、省略可フィールドは
    /// None のとき出力から消えること。
    #[test]
    fn serializes_full_shape_with_camel_case() {
        let diag = Diagnostic::error(
            codes::REQUIRE_DYNAMIC,
            "require() argument must be a string literal.",
        )
        .with_module("lib.util")
        .with_range(Some(Range {
            line: 3,
            col: 5,
            end_line: Some(3),
            end_col: Some(20),
        }));
        let json = serde_json::to_value(&diag).unwrap();
        assert_eq!(json["code"], "require-dynamic");
        assert_eq!(json["severity"], "error");
        assert_eq!(
            json["message"],
            "require() argument must be a string literal."
        );
        assert_eq!(json["module"], "lib.util");
        assert_eq!(json["range"]["line"], 3);
        assert_eq!(json["range"]["col"], 5);
        assert_eq!(json["range"]["endLine"], 3);
        assert_eq!(json["range"]["endCol"], 20);
    }

    /// `module` / `range` を省略した診断は、キー自体が JSON から消える
    /// （プロジェクト全体診断で「省略」を表現するため null ではなく absent が必要）。
    #[test]
    fn omits_module_and_range_when_absent() {
        let diag = Diagnostic::error(
            codes::ENTRY_NOT_FOUND,
            "entry module \"main\" is not present in modules.",
        );
        let json = serde_json::to_value(&diag).unwrap();
        assert!(json.get("module").is_none());
        assert!(json.get("range").is_none());
    }

    /// severity: error は診断カタログの想定どおり(kebab-case・lowercase severity)。
    #[test]
    fn warning_severity_serializes_lowercase() {
        let diag = Diagnostic::warning(codes::ENV_ACCESS, "msg");
        let json = serde_json::to_value(&diag).unwrap();
        assert_eq!(json["severity"], "warning");
    }
}
