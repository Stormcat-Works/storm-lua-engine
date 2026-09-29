//! Coarse source and project compilation API. Compilation never executes Lua.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::link::{analyze_structure, link_project, lookup_source_line, LinkedRange};
use crate::source_map::generate_source_map;
use storm_lua_analysis::diagnostic::{codes, Diagnostic, Range, Severity};
use storm_lua_analysis::lint::collect_written_global_names;
use storm_lua_analysis::project::LuaProject;
use storm_lua_analysis::sw_restrict;
use storm_lua_minify::config::{
    pass_enabled, resolve_pass_toggles, CompileMode, CompileOptions, NumericMode, NumericTolerance,
    PropertyConfig, PropertyMode, SearchMode,
};
use storm_lua_minify::pass_ids::{OPTIMIZATION_PASS_IDS, PASS_RECORD_NAMES};
use storm_lua_minify::search::{compile_code, CandidateSize, CompileCodeResult, SearchStats};
use storm_lua_syntax::parser::{parse_source, parse_source_with_positions};
use storm_lua_syntax::print::token_minify;

/// Optimization pipeline selection; numeric precision is configured separately.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiCompileMode {
    /// Use the conservative optimization pipeline.
    Safe,
    /// Enable the more aggressive whole-program size pipeline.
    Smallest,
}

/// Numeric transformation contract requested by the host.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiNumericMode {
    /// Allow the numeric approximations permitted by the configured tolerance.
    Tolerant,
    /// Disable approximate numeric transformations.
    Exact,
}

/// Deterministic search strategy when no size target is supplied.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiSearchMode {
    /// Explore the full canonical structural candidate set.
    Exhaustive,
    /// Use the bounded deterministic search strategy.
    Fast,
}

/// Absolute and relative limits for permitted numeric transformations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ApiNumericTolerance {
    /// Absolute numeric tolerance.
    pub abs: f64,
    /// Relative numeric tolerance.
    pub rel: f64,
}

/// Property reads can remain runtime calls or be explicitly specialized.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiPropertyMode {
    /// Keep property reads at runtime.
    Runtime,
    /// Replace only property reads for explicitly supplied values.
    Hardcode,
}

/// Host-supplied property specialization settings; unspecified properties remain dynamic.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiPropertyConfig {
    /// Property read behavior.
    pub mode: Option<ApiPropertyMode>,
    /// Numeric property values keyed by case-sensitive property name.
    #[serde(default)]
    pub numbers: BTreeMap<String, f64>,
    /// Boolean property values keyed by case-sensitive property name.
    #[serde(default)]
    pub bools: BTreeMap<String, bool>,
    /// Text property values keyed by case-sensitive property name.
    #[serde(default)]
    pub texts: BTreeMap<String, String>,
}

/// Vehicle Lua compiler settings; omitted fields use the documented compiler defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiCompileOptions {
    /// Game-facing or explicitly extended Lua environment.
    #[serde(default)]
    pub environment: storm_lua_spec::environment::EnvironmentProfile,
    /// Host function/value paths, matching the runtime binding configuration.
    #[serde(default)]
    pub host_bindings: Vec<String>,
    /// Language/API target. Addon optimization is not implemented in this release.
    pub target: Option<storm_lua_analysis::CompilerTarget>,
    /// Optimization pipeline selection.
    pub mode: Option<ApiCompileMode>,
    /// Optional explicit property specialization settings.
    pub property: Option<ApiPropertyConfig>,
    /// Whether separator newlines are uncharged by the size objective.
    pub zero_cost_newlines: Option<bool>,
    /// Per-pass escape hatches; unknown and retired identifiers are errors.
    #[serde(default)]
    pub pass_toggles: BTreeMap<String, bool>,
    /// Limits for numeric transformations allowed by the selected mode.
    pub numeric_tolerance: Option<ApiNumericTolerance>,
    /// Requested or resolved numeric transformation mode.
    pub numeric_mode: Option<ApiNumericMode>,
    /// Search strategy; a supplied target size activates target-driven search.
    pub search_mode: Option<ApiSearchMode>,
    /// Requested bound for fast search; public numeric input is truncated and clamped.
    pub search_beam_width: Option<f64>,
    /// Requested size objective; exceeding it is not a syntax or compilation error.
    pub target_size: Option<usize>,
}

impl ApiCompileOptions {
    /// Resolve settings and reject unknown or retired optimization identifiers.
    pub fn to_core(&self) -> Result<CompileOptions, String> {
        if !self.host_bindings.is_empty()
            && self.environment != storm_lua_spec::environment::EnvironmentProfile::Extended
        {
            return Err(
                "invalid-environment: host bindings require the extended environment".into(),
            );
        }
        if self
            .host_bindings
            .iter()
            .any(|p| !storm_lua_spec::environment::valid_binding_path(p))
        {
            return Err("invalid-environment: invalid host binding path".into());
        }
        let mut pass_toggles = BTreeMap::new();
        for (key, value) in &self.pass_toggles {
            if let Some(&id) = OPTIMIZATION_PASS_IDS.iter().find(|&&id| id == key) {
                pass_toggles.insert(id, *value);
            } else {
                return Err(format!("unknown optimization pass: {key}"));
            }
        }
        let inferred_numeric_mode = self.numeric_mode.unwrap_or_else(|| {
            if self
                .numeric_tolerance
                .is_some_and(|value| value.abs == 0.0 && value.rel == 0.0)
            {
                ApiNumericMode::Exact
            } else {
                ApiNumericMode::Tolerant
            }
        });
        let numeric_mode = match inferred_numeric_mode {
            ApiNumericMode::Tolerant => NumericMode::Tolerant,
            ApiNumericMode::Exact => NumericMode::Exact,
        };
        let numeric_tolerance = if numeric_mode == NumericMode::Exact {
            Some(NumericTolerance { abs: 0.0, rel: 0.0 })
        } else {
            self.numeric_tolerance.map(|value| NumericTolerance {
                abs: value.abs,
                rel: value.rel,
            })
        };
        Ok(CompileOptions {
            environment: self.environment,
            host_bindings: self.host_bindings.clone(),
            mode: match self.mode.unwrap_or(ApiCompileMode::Smallest) {
                ApiCompileMode::Safe => CompileMode::Safe,
                ApiCompileMode::Smallest => CompileMode::Smallest,
            },
            property: self.property.as_ref().map(|value| PropertyConfig {
                mode: match value.mode.unwrap_or(ApiPropertyMode::Runtime) {
                    ApiPropertyMode::Runtime => PropertyMode::Runtime,
                    ApiPropertyMode::Hardcode => PropertyMode::Hardcode,
                },
                numbers: (!value.numbers.is_empty()).then(|| value.numbers.clone()),
                bools: (!value.bools.is_empty()).then(|| value.bools.clone()),
                texts: (!value.texts.is_empty()).then(|| value.texts.clone()),
            }),
            zero_cost_newlines: self.zero_cost_newlines.unwrap_or(true),
            pass_toggles,
            numeric_tolerance,
            numeric_mode,
            search_mode: match self.search_mode.unwrap_or(ApiSearchMode::Exhaustive) {
                ApiSearchMode::Exhaustive => SearchMode::Exhaustive,
                ApiSearchMode::Fast => SearchMode::Fast,
            },
            search_beam_width: self
                .search_beam_width
                .unwrap_or(4.0)
                .trunc()
                .clamp(1.0, 16.0) as u32,
            target_size: self.target_size,
        })
    }
}

/// One applied transformation and its measured size contribution.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiPassRecord {
    /// Human-readable transformation record name.
    pub name: String,
    /// Character savings relative to the recorded baseline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved: Option<i64>,
    /// Optional transformation-specific diagnostic detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Diagnostic elapsed milliseconds, not a candidate-ranking input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<f64>,
}

/// `pass_ids::PASS_RECORD_NAMES` の1エントリ（wasm/JS 向け camelCase）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiPassMetadataEntry {
    /// Registered optimization identifier.
    pub id: &'static str,
    /// Transformation record name associated with the identifier.
    pub record_name: &'static str,
}

/// Web UI 向けの id ⇔ `PassRecord.name` 対応表。
///
/// `PASS_RECORD_NAMES` をそのまま JSON 化する。同じ id が複数の表示名を
/// 持つ場合はその件数だけエントリが並ぶ（例: `constant-folding`）。
pub fn pass_metadata() -> Vec<ApiPassMetadataEntry> {
    PASS_RECORD_NAMES
        .iter()
        .map(|(id, record_name)| ApiPassMetadataEntry { id, record_name })
        .collect()
}

/// Size objective for one structural/layout candidate.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiCandidateSize {
    /// Selected structural candidate label.
    pub structural: String,
    /// Selected output-layout label.
    pub layout: String,
    /// Measured character count under the associated output or candidate accounting.
    pub size: usize,
}

/// Search diagnostics; timing is not used as a nondeterministic tie-breaker.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiSearchStats {
    /// Resolved search strategy.
    pub mode: &'static str,
    /// Resolved fast-search beam width, when applicable.
    pub beam_width: Option<u32>,
    /// Number of resulting candidates considered.
    pub candidates: usize,
    /// Number of candidates whose evaluation was attempted.
    pub attempted: usize,
    /// Candidates rejected because their generated source did not parse.
    pub parse_rejected: usize,
    /// Candidates rejected by the configured semantic verifier.
    pub semantic_rejected: usize,
    /// Initial optimization variants before structural expansion.
    pub core_variants: usize,
    /// Number of available structural variants.
    pub structural_variants: usize,
    /// Structural variants explored by this run.
    pub structural_explored: usize,
    /// Selected structural candidate label.
    pub structural: String,
    /// Selected output-layout label.
    pub layout: String,
    /// Per-candidate size observations.
    pub candidate_sizes: Vec<ApiCandidateSize>,
    /// Requested size objective; exceeding it is not a syntax or compilation error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_size: Option<usize>,
    /// Whether the generated result meets the requested size objective.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_met: Option<bool>,
    /// Whether target-driven search stopped before full exploration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped_early: Option<bool>,
    /// Number of target-search checkpoints visited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoints: Option<usize>,
    /// Final search stage or early-stop checkpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
}

/// Semantic assumptions and numeric contract attached to generated code.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiAssumptions {
    /// Whether generated code relies on finite-number assumptions.
    pub finite_numbers: bool,
    /// Whether the declared contract permits changing Lua integer/float subtypes.
    pub integer_float_subtype_may_change: bool,
    /// Requested or resolved numeric transformation mode.
    pub numeric_mode: &'static str,
    /// Limits for numeric transformations allowed by the selected mode.
    pub numeric_tolerance: ApiNumericTolerance,
}

/// Single-source compilation result; success and target-size attainment are distinct.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiCompileResult {
    /// Whether a usable generated artifact was produced.
    pub ok: bool,
    /// 後方互換のため保持（1.0 で削除予定）。値は `diagnostics` 内の最初の
    /// `Severity::Error` 診断の `message` と同じ文字列。新規コードは `diagnostics` を見ること。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Generated Lua source; compilation does not execute it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Original source length in UTF-16 code units.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original: Option<usize>,
    /// Measured character count under the associated output or candidate accounting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<usize>,
    /// Character savings relative to the recorded baseline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved: Option<isize>,
    /// Length of the token-only compact baseline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<usize>,
    /// Diagnostic elapsed milliseconds, not a candidate-ranking input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<f64>,
    /// Applied transformation records.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub passes: Vec<ApiPassRecord>,
    /// Structured diagnostics with source locations when available.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
    /// Optional search statistics for a minifying build.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<ApiSearchStats>,
    /// Builtin API aliases selected for the output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_aliases: Vec<String>,
    /// Registered passes disabled by explicit settings or the numeric-mode contract.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_passes: Vec<String>,
    /// Semantic assumptions attached to this generated artifact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assumptions: Option<ApiAssumptions>,
    /// Resolved property specialization mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub property_mode: Option<&'static str>,
    /// Number of property reads replaced by supplied constants.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub property_reads_hardcoded: Option<usize>,
    /// Number of separator newlines emitted using zero-cost accounting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zero_cost_newlines: Option<u32>,
}

fn options_error_code(error: &str) -> &'static str {
    if error.starts_with("invalid-environment:") {
        codes::INVALID_ENVIRONMENT
    } else {
        codes::UNKNOWN_OPTIMIZATION_PASS
    }
}

impl ApiCompileResult {
    fn failure(source: &str, error: String) -> Self {
        let diagnostics = failure_diagnostics(source, &error);
        Self::with_error(error, diagnostics)
    }

    /// Return an invalid configuration without compiling or executing the source.
    pub fn invalid_options(error: String) -> Self {
        let diagnostics = vec![Diagnostic::error(options_error_code(&error), error.clone())];
        Self::with_error(error, diagnostics)
    }

    fn with_error(error: String, diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            ok: false,
            error: Some(error),
            diagnostics,
            code: None,
            original: None,
            size: None,
            saved: None,
            baseline: None,
            elapsed_ms: None,
            passes: Vec::new(),
            search: None,
            api_aliases: Vec::new(),
            disabled_passes: Vec::new(),
            assumptions: None,
            property_mode: None,
            property_reads_hardcoded: None,
            zero_cost_newlines: None,
        }
    }
}

/// `compile()` の致命失敗を新 Diagnostic スキーマへ変換する。
///
/// `compile_code` は `Result<_, String>` を返すため、失敗理由の型情報はこの時点で
/// 失われている。`parse_source` を独立に再実行して構文エラーかどうかを判定する
/// （`compile_code` 内部の `prepare_search` も同じ `parse_source(source)` を最初に
/// 呼ぶため、結果は決定的に一致する）。構文エラーなら `SYNTAX_ERROR`、それ以外の
/// パイプライン内部失敗（意味的に妥当な候補が生き残らない等）は `COMPILE_FAILED`。
///
/// 位置情報: `ParserError`/`ParseError`/`LexError` は文字列のみを保持し、構造化された
/// line/col を公開していないため、`range` は `None` のまま返す
/// （`project.rs` の同種の `SYNTAX_ERROR` 診断も同じ制約で `range: None`）。
fn failure_diagnostics(source: &str, error: &str) -> Vec<Diagnostic> {
    let code = if parse_source(source).is_err() {
        codes::SYNTAX_ERROR
    } else {
        codes::COMPILE_FAILED
    };
    vec![Diagnostic::error(code, error.to_string())]
}

/// `compile()` の既知ハザード診断。位置サイドテーブル付きで再パースする
/// （検索ホットパスの `parse_source` とは別経路。`success()` で1回だけ呼ばれる非ホットパス）。
fn detect_hazards(source: &str, options: &CompileOptions) -> Result<Vec<Diagnostic>, String> {
    let (ast, root, positions) = parse_source_with_positions(source).map_err(|e| e.to_string())?;
    let mut diagnostics = storm_lua_analysis::environment_checks::diagnostics(
        &ast,
        root,
        Some(&positions),
        options.environment,
        &options.host_bindings,
        &Default::default(),
        Severity::Error,
        None,
    );
    if let Some(reason) = storm_lua_analysis::environment_checks::lexical_reason(
        &ast,
        options.environment,
        &options.host_bindings,
    ) {
        let range = ast.nodes.iter().enumerate().find_map(|(id,node)| {
            if matches!(node, storm_lua_syntax::Node::Name(symbol) if ast.strings.get(*symbol)=="_ENV") {
                positions.get(id as u32).map(|(line,col)| Range::point(line,col))
            } else { None }
        });
        diagnostics.push(Diagnostic::warning(codes::CONSERVATIVE_MINIFICATION,
            format!("Token-preserving minification only: {reason}. Names, literals, globals, token line positions and evaluation order are unchanged; property specialization and AST passes are not applied.")).with_range(range));
    }
    Ok(diagnostics)
}

fn search_stats(stats: SearchStats, options: &CompileOptions) -> ApiSearchStats {
    let satisficing = options.target_size.is_some();
    ApiSearchStats {
        mode: if stats.winner_structural == "lexical" {
            "lexical"
        } else if satisficing {
            "satisficing"
        } else if options.search_mode == SearchMode::Fast {
            "fast"
        } else {
            "exhaustive"
        },
        beam_width: (!satisficing && options.search_mode == SearchMode::Fast)
            .then_some(options.search_beam_width.clamp(1, 16)),
        candidates: stats.candidates,
        attempted: stats.attempted,
        parse_rejected: stats.parse_rejected,
        semantic_rejected: stats.semantic_rejected,
        core_variants: stats.core_variants,
        structural_variants: stats.structural_variants,
        structural_explored: stats.structural_explored,
        structural: stats.winner_structural,
        layout: stats.winner_layout,
        candidate_sizes: stats
            .candidate_sizes
            .into_iter()
            .map(
                |CandidateSize {
                     structural,
                     layout,
                     size,
                 }| ApiCandidateSize {
                    structural,
                    layout,
                    size,
                },
            )
            .collect(),
        target_size: options
            .target_size
            .map(|target| stats.target_size.unwrap_or(target)),
        target_met: satisficing.then_some(stats.target_met),
        stopped_early: satisficing.then_some(stats.stopped_early),
        checkpoints: satisficing.then_some(stats.checkpoints),
        stage: satisficing.then(|| stats.stage.unwrap_or_else(|| "full-search".into())),
    }
}

fn success(source: &str, options: &CompileOptions, result: CompileCodeResult) -> ApiCompileResult {
    let diagnostics = match detect_hazards(source, options) {
        Ok(diagnostics) => diagnostics,
        Err(error) => return ApiCompileResult::failure(source, error),
    };
    if let Some(error) = diagnostics.iter().find(|d| d.severity == Severity::Error) {
        return ApiCompileResult::with_error(error.message.clone(), diagnostics);
    }
    let lexical = result.stats.winner_structural == "lexical";
    let original = source.encode_utf16().count();
    let baseline = token_minify(source)
        .map(|value| value.encode_utf16().count())
        .unwrap_or(original);
    let size = result.code.encode_utf16().count();
    let resolved_toggles = resolve_pass_toggles(&options.pass_toggles, options.numeric_mode);
    let tolerance = options.numeric_tolerance.unwrap_or(NumericTolerance {
        abs: if options.numeric_mode == NumericMode::Exact {
            0.0
        } else {
            1e-6
        },
        rel: if options.numeric_mode == NumericMode::Exact {
            0.0
        } else {
            1e-6
        },
    });
    ApiCompileResult {
        ok: true,
        error: None,
        code: Some(result.code),
        original: Some(original),
        size: Some(size),
        saved: Some(original as isize - size as isize),
        baseline: Some(baseline),
        elapsed_ms: None,
        passes: result
            .passes
            .into_iter()
            .map(|pass| ApiPassRecord {
                name: pass.name,
                saved: pass.saved,
                detail: pass.detail,
                elapsed_ms: pass.elapsed_ms,
            })
            .collect(),
        diagnostics,
        search: Some(search_stats(result.stats, options)),
        api_aliases: result.api_aliases,
        disabled_passes: OPTIMIZATION_PASS_IDS
            .iter()
            .filter(|&&id| {
                let exact_safe_subset_enabled = id == "constant-folding"
                    && options.numeric_mode == NumericMode::Exact
                    && pass_enabled(&options.pass_toggles, id);
                lexical || (!pass_enabled(&resolved_toggles, id) && !exact_safe_subset_enabled)
            })
            .map(|&id| id.to_string())
            .collect(),
        assumptions: Some(ApiAssumptions {
            finite_numbers: !lexical && options.mode == CompileMode::Smallest,
            integer_float_subtype_may_change: !lexical && options.mode == CompileMode::Smallest,
            numeric_mode: if options.numeric_mode == NumericMode::Exact {
                "exact"
            } else {
                "tolerant"
            },
            numeric_tolerance: ApiNumericTolerance {
                abs: tolerance.abs,
                rel: tolerance.rel,
            },
        }),
        property_mode: Some(
            match options
                .property
                .as_ref()
                .filter(|_| !lexical)
                .map(|value| value.mode)
            {
                Some(PropertyMode::Hardcode) => "hardcode",
                _ => "runtime",
            },
        ),
        property_reads_hardcoded: Some(result.property_reads_hardcoded),
        zero_cost_newlines: Some(result.zero_cost_newlines),
    }
}

/// Convert a selected search result into public diagnostics and artifact metadata.
pub fn finish_compile_result(
    source: &str,
    options: &CompileOptions,
    result: Result<CompileCodeResult, String>,
) -> ApiCompileResult {
    match detect_hazards(source, options) {
        Ok(diagnostics) => {
            if let Some(error) = diagnostics.iter().find(|d| d.severity == Severity::Error) {
                return ApiCompileResult::with_error(error.message.clone(), diagnostics);
            }
        }
        Err(error) => return ApiCompileResult::failure(source, error),
    }
    match result {
        Ok(result) => success(source, options, result),
        Err(error) => ApiCompileResult::failure(source, error),
    }
}

/// Optimize one vehicle Lua source chunk without executing it.
pub fn compile(source: &str, options: &ApiCompileOptions) -> ApiCompileResult {
    let core_options = match options.to_core() {
        Ok(options) => options,
        Err(error) => return ApiCompileResult::invalid_options(error),
    };
    finish_compile_result(source, &core_options, compile_code(source, &core_options))
}

/// `compileProject()` のオプション（設計 §6）。既存 `ApiCompileOptions` を全部そのまま内包する
/// （`#[serde(flatten)]`）。`minify` のみ追加（既定 `true`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiProjectCompileOptions {
    /// Compilation options shared with the single-source API.
    #[serde(flatten)]
    pub compile: ApiCompileOptions,
    /// `false` = リンクのみ（可読出力 + Source Map）。既定 `true`（設計 §6）。
    pub minify: Option<bool>,
}

/// `compileProject()` の結果（設計 §6）。エラー時は `code`/`map` を返さない。
/// `minify:true` のときのみ既存 `ApiCompileResult` と同じ統計フィールド群を持つ
/// （`map` は v1 では `minify:true` で常に `None` — v2 で由来タグ伝播が入るまでの予約。§6 参照）。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiProjectCompileResult {
    /// Whether a usable generated artifact was produced.
    pub ok: bool,
    /// Display text of the first fatal error; use diagnostics for structured handling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Generated Lua source; compilation does not execute it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Source Map v3 (JSON 文字列)。`minify:false` のときのみ存在する（設計 §6）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub map: Option<String>,
    /// Linked module keys in execution order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub used_modules: Vec<String>,
    /// Injected ambient members grouped by namespace.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub injected_ambient: BTreeMap<String, Vec<String>>,
    /// Structured diagnostics with source locations when available.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
    // 以下、`minify:true` のとき既存 `ApiCompileResult` と同じ統計フィールド群。
    /// Original source length in UTF-16 code units.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original: Option<usize>,
    /// Measured character count under the associated output or candidate accounting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<usize>,
    /// Character savings relative to the recorded baseline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved: Option<isize>,
    /// Length of the token-only compact baseline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<usize>,
    /// Diagnostic elapsed milliseconds, not a candidate-ranking input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<f64>,
    /// Applied transformation records.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub passes: Vec<ApiPassRecord>,
    /// Optional search statistics for a minifying build.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<ApiSearchStats>,
    /// Builtin API aliases selected for the output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_aliases: Vec<String>,
    /// Registered passes disabled by explicit settings or the numeric-mode contract.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_passes: Vec<String>,
    /// Semantic assumptions attached to this generated artifact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assumptions: Option<ApiAssumptions>,
    /// Resolved property specialization mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub property_mode: Option<&'static str>,
    /// Number of property reads replaced by supplied constants.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub property_reads_hardcoded: Option<usize>,
    /// Number of separator newlines emitted using zero-cost accounting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zero_cost_newlines: Option<u32>,
}

impl ApiProjectCompileResult {
    /// リンク段（構造パリティ規則）で error 診断が出た場合。`code`/`map` は無い。
    fn structural_failure(diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            ok: false,
            diagnostics,
            ..Default::default()
        }
    }

    /// `minify:true` 経路で既存 `compile()` パイプライン自体が失敗した場合
    /// （リンク済みソースの再パース不能等、内部不変条件が崩れた異常系）。
    fn compile_failure(error: String) -> Self {
        Self {
            ok: false,
            error: Some(error),
            ..Default::default()
        }
    }

    /// `minify:true` 経路の成功時。既存 `ApiCompileResult` の統計フィールドをそのまま引き継ぐ。
    /// `map` は v1 では出さない（§6: v2 予約）。
    fn from_minified(
        compiled: ApiCompileResult,
        used_modules: Vec<String>,
        injected_ambient: BTreeMap<String, Vec<String>>,
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        if !compiled.ok {
            return Self::compile_failure(
                compiled
                    .error
                    .unwrap_or_else(|| "compile() failed with no error message".to_string()),
            );
        }
        Self {
            ok: true,
            error: None,
            code: compiled.code,
            map: None,
            used_modules,
            injected_ambient,
            diagnostics,
            original: compiled.original,
            size: compiled.size,
            saved: compiled.saved,
            baseline: compiled.baseline,
            elapsed_ms: compiled.elapsed_ms,
            passes: compiled.passes,
            search: compiled.search,
            api_aliases: compiled.api_aliases,
            disabled_passes: compiled.disabled_passes,
            assumptions: compiled.assumptions,
            property_mode: compiled.property_mode,
            property_reads_hardcoded: compiled.property_reads_hardcoded,
            zero_cost_newlines: compiled.zero_cost_newlines,
        }
    }
}

/// `minify:true` 経路の診断（`compile()` が返す既存ハザード診断。位置はリンク済みソース上）を
/// 元モジュール・元行へ逆引きする（設計 §6.1 実装ノート相当。区間表 `ranges` が正本）。
/// 逆引きできた場合は `module`/`range.line` を元モジュール側へ書き換える（col はリンクが
/// テキストをそのまま転写するため、対応する行内では元ソースと同じ意味を保つ）。
/// **逆引きできない場合は位置を捏造せず、`module`/`range` を落として位置なし診断にする**
/// （悪性フォールバック回避: 誤った位置を出すくらいなら位置なしの方が安全）。
fn remap_diagnostics_to_modules(
    diagnostics: Vec<Diagnostic>,
    ranges: &[LinkedRange],
) -> Vec<Diagnostic> {
    diagnostics
        .into_iter()
        .map(|diag| {
            let Some(range) = diag.range else {
                return diag;
            };
            match lookup_source_line(ranges, range.line) {
                Some((module, source_line)) => Diagnostic {
                    module: Some(module.to_string()),
                    range: Some(Range {
                        line: source_line,
                        ..range
                    }),
                    ..diag
                },
                None => Diagnostic {
                    module: None,
                    range: None,
                    ..diag
                },
            }
        })
        .collect()
}

/// `compileProject()`: リンク + minify（設計 §6）。
///
/// - リンク段（パリティ規則）で entry から到達するモジュールの error 診断が1件でもあれば
///   `ok:false` + 診断のみを返す（`code`/`map` は返さない）。診断そのものは全モジュール分を
///   `analyze` と同一集合で返す（到達不能モジュールの error も診断には残る。`link_project` が
///   土台のため）が、`ok`/失敗判定は到達集合基準（タスク2: §7-2、`analyze` とのパリティは
///   診断集合のみ、`ok` 判定はここだけの独自ロジック）。
/// - `minify:false`: リンク済み可読ソース + Source Map(v3) + `usedModules`/`injectedAmbient`。
/// - `minify:true`: リンク済みソースを既存 `compile()` の minify パイプラインへそのまま通す。
///   `map` は返さない（v1 は minify 後の Source Map を作らない。§6/§11 非目標・v2 予約）。
///   `compile()` 由来の診断（`env-access` 等）はオフセット区間表で元モジュール・元行へ
///   逆引きしてから合流する（逆引き不能なら位置なし診断。位置は捏造しない）。
pub fn compile_project(
    project: &LuaProject,
    options: &ApiProjectCompileOptions,
) -> ApiProjectCompileResult {
    let core_options = match options.compile.to_core() {
        Ok(options) => options,
        Err(error) => {
            return ApiProjectCompileResult::structural_failure(vec![Diagnostic::error(
                options_error_code(&error),
                error,
            )])
        }
    };
    let mut link = link_project(project);
    if link.linked_source.is_none() {
        // `link_project` が到達可能モジュールの error（またはモジュール無しのプロジェクト
        // 全体診断）を検出済み。到達不能モジュールの error だけが残るケースはここには来ない
        // （`link_project` はその場合リンクを継続する）。
        return ApiProjectCompileResult::structural_failure(link.diagnostics);
    }

    // Stormworks 固有制限の検出（`analyze()` と同じロジック。severity のみ error）。
    // `sw_restrict` は `analyze_structure` の外側にある独立レイヤーのため、ここで改めて
    // 構造解析を行う（`analyze()` は warning として同じ検出を行うため、severity だけが
    // 両者の違いになる）。診断は到達可否を問わず全モジュール分を追加する（`analyze` との
    // パリティ維持）。
    let structural = analyze_structure(project);
    let written_globals = collect_written_global_names(structural.modules.values());
    for (key, analysis) in &structural.modules {
        link.diagnostics
            .extend(sw_restrict::scan_module_in_environment(
                key,
                analysis,
                &written_globals,
                Severity::Error,
                core_options.environment,
                &core_options.host_bindings,
            ));
    }
    // ok/失敗判定は entry から到達するモジュールの error のみで決める（タスク2）。
    // 到達不能モジュールの error は診断に残したまま、リンクは成功扱いを続ける。
    if link
        .diagnostics
        .iter()
        .any(|d| structural.is_blocking(project, d))
    {
        return ApiProjectCompileResult::structural_failure(link.diagnostics);
    }
    #[expect(
        clippy::expect_used,
        reason = "link_project produced this result and the preceding gate rejected every blocking diagnostic"
    )]
    let linked_source = link
        .linked_source
        .clone()
        .expect("no error diagnostics -> link_project always returns linked_source");

    if !options.minify.unwrap_or(true) {
        let map = generate_source_map(project, &link);
        return ApiProjectCompileResult {
            ok: true,
            code: Some(linked_source),
            map,
            used_modules: link.used_modules,
            injected_ambient: link.injected_ambient,
            diagnostics: link.diagnostics,
            ..Default::default()
        };
    }

    let compiled = finish_compile_result(
        &linked_source,
        &core_options,
        compile_code(&linked_source, &core_options),
    );
    let mut diagnostics = remap_diagnostics_to_modules(compiled.diagnostics.clone(), &link.ranges);
    // 構造/sw_restrict 診断（到達不能モジュールの Error を含む）を合流する。ok:true でも
    // 到達不能モジュールの Error はここまで捨てずに残っている（タスク2: §7-2、
    // `analyze` とのパリティ維持のため minify:true でも diagnostics は落とさない）。
    diagnostics.extend(link.diagnostics);
    ApiProjectCompileResult::from_minified(
        compiled,
        link.used_modules,
        link.injected_ambient,
        diagnostics,
    )
}

/// LB include-once build. Separate from the existing return-valued static linker.
pub fn compile_lifeboat(
    project: &LuaProject,
    options: &ApiProjectCompileOptions,
) -> ApiProjectCompileResult {
    let core = match options.compile.to_core() {
        Ok(value) => value,
        Err(error) => return ApiProjectCompileResult::compile_failure(error),
    };
    let link = crate::lifeboat::link_lifeboat(project);
    let Some(source) = link.linked_source.as_deref() else {
        return ApiProjectCompileResult::structural_failure(link.diagnostics);
    };
    let analysis = storm_lua_analysis::analyze(
        &LuaProject {
            entry: "linked".into(),
            modules: BTreeMap::from([("linked".into(), source.into())]),
            ambient: BTreeMap::new(),
        },
        &storm_lua_analysis::AnalyzeOptions {
            mode: storm_lua_analysis::AnalyzeMode::Runtime,
            environment: core.environment,
            host_bindings: core.host_bindings.clone(),
            ..Default::default()
        },
    );
    let mut diagnostics = remap_diagnostics_to_modules(analysis.diagnostics, &link.ranges);
    for diagnostic in &mut diagnostics {
        if matches!(
            diagnostic.code,
            "sw-unavailable-global"
                | "syntax-error"
                | "sw-screen-outside-ondraw"
                | "sw-input-outside-ontick"
        ) {
            diagnostic.severity = Severity::Error;
        }
    }
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return ApiProjectCompileResult::structural_failure(diagnostics);
    }
    if !options.minify.unwrap_or(true) {
        return ApiProjectCompileResult {
            ok: true,
            code: Some(source.into()),
            map: generate_source_map(project, &link),
            used_modules: link.used_modules,
            injected_ambient: link.injected_ambient,
            diagnostics,
            ..Default::default()
        };
    }
    let compiled = finish_compile_result(source, &core, compile_code(source, &core));
    diagnostics.extend(remap_diagnostics_to_modules(
        compiled.diagnostics.clone(),
        &link.ranges,
    ));
    ApiProjectCompileResult::from_minified(
        compiled,
        link.used_modules,
        link.injected_ambient,
        diagnostics,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn exact_is_inferred_from_zero_tolerance() {
        let options = ApiCompileOptions {
            numeric_tolerance: Some(ApiNumericTolerance { abs: 0.0, rel: 0.0 }),
            ..ApiCompileOptions::default()
        };
        let core = options.to_core().unwrap();
        assert_eq!(core.numeric_mode, NumericMode::Exact);
        assert_eq!(core.numeric_tolerance.unwrap().abs, 0.0);
    }

    #[test]
    fn public_options_preserve_migration_beam_truncation_and_clamp() {
        let low = ApiCompileOptions {
            search_beam_width: Some(-2.9),
            ..ApiCompileOptions::default()
        };
        assert_eq!(low.to_core().unwrap().search_beam_width, 1);
        let fractional = ApiCompileOptions {
            search_beam_width: Some(7.9),
            ..ApiCompileOptions::default()
        };
        assert_eq!(fractional.to_core().unwrap().search_beam_width, 7);
        let high = ApiCompileOptions {
            search_beam_width: Some(99.0),
            ..ApiCompileOptions::default()
        };
        assert_eq!(high.to_core().unwrap().search_beam_width, 16);
    }

    #[test]
    fn public_compile_syntax_error_is_reported_via_new_diagnostic_schema() {
        let options = ApiCompileOptions::default();
        let result = compile("local a = (", &options);
        assert!(!result.ok);
        assert_eq!(result.diagnostics.len(), 1);
        let diag = &result.diagnostics[0];
        assert_eq!(diag.code, codes::SYNTAX_ERROR);
        assert_eq!(diag.severity, Severity::Error);
        assert_eq!(result.error.as_deref(), Some(diag.message.as_str()));
    }

    #[test]
    fn exact_safe_constant_folding_is_reported_as_enabled() {
        let options: ApiCompileOptions = serde_json::from_value(serde_json::json!({
            "numericMode": "exact"
        }))
        .expect("exact options");
        let result = compile("if true then a=1 else a=2 end", &options);
        assert!(result.ok, "{:?}", result.error);
        assert!(
            !result
                .disabled_passes
                .iter()
                .any(|id| id == "constant-folding"),
            "exact-safe constant folding ran but public metadata marked it disabled: {:?}",
            result.disabled_passes
        );
        assert!(
            result
                .passes
                .iter()
                .any(|pass| pass.name == "constant folding"),
            "expected exact-safe constant-folding pass record: {:?}",
            result.passes
        );

        let disabled_options: ApiCompileOptions = serde_json::from_value(serde_json::json!({
            "numericMode": "exact",
            "passToggles": {"constant-folding": false}
        }))
        .expect("disabled exact options");
        let disabled = compile("if true then a=1 else a=2 end", &disabled_options);
        assert!(disabled.ok, "{:?}", disabled.error);
        assert!(
            disabled
                .disabled_passes
                .iter()
                .any(|id| id == "constant-folding"),
            "explicit escape hatch must report constant-folding disabled: {:?}",
            disabled.disabled_passes
        );
        assert!(
            !disabled
                .passes
                .iter()
                .any(|pass| pass.name == "constant folding"),
            "explicit escape hatch must suppress the exact-safe pass: {:?}",
            disabled.passes
        );
    }

    #[test]
    fn public_target_size_activates_satisficing_metadata() {
        let options = ApiCompileOptions {
            target_size: Some(41),
            ..ApiCompileOptions::default()
        };
        let result = compile("function onTick()output.setNumber(1,1+2)end", &options);
        assert!(result.ok, "{:?}", result.error);
        assert_eq!(result.size, Some(41));
        let search = result.search.expect("search metadata");
        assert_eq!(search.mode, "satisficing");
        assert_eq!(search.target_size, Some(41));
        assert_eq!(search.target_met, Some(true));
        assert_eq!(search.stopped_early, Some(true));
        assert_ne!(search.stage.as_deref(), Some("full-search"));
    }

    #[test]
    fn public_compile_hardcodes_properties_and_reports_metadata() {
        let options = ApiCompileOptions {
            property: Some(ApiPropertyConfig {
                mode: Some(ApiPropertyMode::Hardcode),
                numbers: BTreeMap::from([("X".into(), 12.5)]),
                bools: BTreeMap::new(),
                texts: BTreeMap::new(),
            }),
            ..ApiCompileOptions::default()
        };
        let result = compile(
            "x=property.getNumber(\"X\") function onTick()output.setNumber(1,x)end",
            &options,
        );
        assert!(result.ok, "{:?}", result.error);
        assert_eq!(result.property_reads_hardcoded, Some(1));
        assert!(!result.code.unwrap().contains("property"));
        assert!(result.search.is_some());
    }

    #[test]
    fn public_compile_hardcodes_property_get_text() {
        let options = ApiCompileOptions {
            property: Some(ApiPropertyConfig {
                mode: Some(ApiPropertyMode::Hardcode),
                numbers: BTreeMap::new(),
                bools: BTreeMap::new(),
                texts: BTreeMap::from([("Name".into(), "Foo".into())]),
            }),
            ..ApiCompileOptions::default()
        };
        let result = compile(
            "x=property.getText(\"Name\") function onTick()output.setText(1,x)end",
            &options,
        );
        assert!(result.ok, "{:?}", result.error);
        assert_eq!(result.property_reads_hardcoded, Some(1));
        assert!(!result.code.unwrap().contains("property"));
    }

    // --- compile_project（設計 §6） ---

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

    #[test]
    fn project_compile_minify_false_matches_linked_source_and_has_map() {
        let p = project(
            "main",
            &[
                ("main", "local util = require(\"util\")\nreturn util.f()\n"),
                (
                    "util",
                    "local M = {}\nfunction M.f() return 1 end\nreturn M\n",
                ),
            ],
        );
        let options = ApiProjectCompileOptions {
            minify: Some(false),
            ..ApiProjectCompileOptions::default()
        };
        let result = compile_project(&p, &options);
        assert!(result.ok, "{:?}", result.error);
        let expected = crate::link::link_project(&p)
            .linked_source
            .expect("linked source");
        assert_eq!(result.code.as_deref(), Some(expected.as_str()));
        assert!(result.map.is_some(), "minify:false must produce a map");
        assert_eq!(
            result.used_modules,
            vec!["main".to_string(), "util".to_string()]
        );
        assert!(result.diagnostics.is_empty());
        // minify:true 統計フィールドは出さない
        assert!(result.size.is_none());
        assert!(result.search.is_none());
    }

    #[test]
    fn project_compile_minify_true_has_no_map_but_has_stats() {
        let p = project("main", &[("main", "return 1 + 2\n")]);
        let options = ApiProjectCompileOptions::default(); // minify: None -> 既定 true
        let result = compile_project(&p, &options);
        assert!(result.ok, "{:?}", result.error);
        assert!(
            result.map.is_none(),
            "minify:true must not produce a map (v1, §6)"
        );
        assert!(result.code.is_some());
        assert!(result.size.is_some());
        assert!(result.search.is_some());
    }

    #[test]
    fn project_compile_structural_error_has_no_code_or_map_and_matches_analyze() {
        let p = project("main", &[("main", "local x = require(\"missing\")\n")]);
        let options = ApiProjectCompileOptions::default();
        let result = compile_project(&p, &options);
        assert!(!result.ok);
        assert!(result.code.is_none());
        assert!(result.map.is_none());
        let analyzed = storm_lua_analysis::analyze::analyze(
            &p,
            &storm_lua_analysis::analyze::AnalyzeOptions::default(),
        );
        let mut compile_codes: Vec<&str> = result.diagnostics.iter().map(|d| d.code).collect();
        let mut analyze_codes: Vec<&str> = analyzed
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.code)
            .collect();
        compile_codes.sort_unstable();
        analyze_codes.sort_unstable();
        assert_eq!(compile_codes, analyze_codes);
    }

    #[test]
    fn project_compile_structural_error_same_for_minify_true_and_false() {
        let p = project("main", &[("other", "return 1\n")]);
        let minify_true = compile_project(&p, &ApiProjectCompileOptions::default());
        let minify_false = compile_project(
            &p,
            &ApiProjectCompileOptions {
                minify: Some(false),
                ..ApiProjectCompileOptions::default()
            },
        );
        assert!(!minify_true.ok && !minify_false.ok);
        let codes_of = |r: &ApiProjectCompileResult| -> Vec<&str> {
            r.diagnostics.iter().map(|d| d.code).collect()
        };
        assert_eq!(codes_of(&minify_true), codes_of(&minify_false));
    }

    #[test]
    fn project_compile_minify_true_remaps_hazard_diagnostic_to_source_module() {
        // "util" モジュール側で _ENV アクセス(env-access)を起こす。minify 後の診断は
        // リンク済みソース上の位置ではなく、元モジュール "util" とその元の行を指すはず。
        let p = project(
            "main",
            &[
                ("main", "local util = require(\"util\")\nreturn util\n"),
                ("util", "local M = {}\nM.x = _ENV\nreturn M\n"),
            ],
        );
        let result = compile_project(&p, &ApiProjectCompileOptions::default());
        assert!(result.ok, "{:?}", result.error);
        let diag = result
            .diagnostics
            .iter()
            .find(|d| d.code == codes::CONSERVATIVE_MINIFICATION)
            .expect("conservative-minification diagnostic");
        assert_eq!(diag.module.as_deref(), Some("util"));
        let range = diag
            .range
            .expect("conservative-minification diagnostic should carry a range");
        assert_eq!(range.line, 2, "_ENV appears on util's 2nd source line");
    }

    // --- Stormworks 固有制限検出 (v0.6.0): compile_project は error / ok:false ---

    #[test]
    fn compile_project_reports_sw_unavailable_global_as_error_and_fails() {
        let p = project(
            "main",
            &[("main", "function onTick()\n  pcall(function() end)\nend\n")],
        );
        let result = compile_project(&p, &ApiProjectCompileOptions::default());
        assert!(!result.ok);
        let diag = result
            .diagnostics
            .iter()
            .find(|d| d.code == codes::SW_UNAVAILABLE_GLOBAL)
            .expect("sw-unavailable-global diagnostic");
        assert_eq!(diag.severity, Severity::Error);
    }

    #[test]
    fn compile_project_reports_input_outside_ontick_as_error_and_fails() {
        let p = project(
            "main",
            &[("main", "local x = input.getNumber(1)\nreturn x\n")],
        );
        let result = compile_project(&p, &ApiProjectCompileOptions::default());
        assert!(!result.ok);
        let diag = result
            .diagnostics
            .iter()
            .find(|d| d.code == codes::INPUT_OUTSIDE_ONTICK)
            .expect("input-outside-ontick diagnostic");
        assert_eq!(diag.severity, Severity::Error);
    }

    #[test]
    fn compile_project_succeeds_when_input_is_only_used_inside_ontick() {
        let p = project(
            "main",
            &[(
                "main",
                "function onTick()\n  local x = input.getNumber(1)\n  output.setNumber(1, x)\nend\n",
            )],
        );
        let result = compile_project(&p, &ApiProjectCompileOptions::default());
        assert!(result.ok, "{:?}", result.error);
    }

    // --- タスク2: entry から到達不能なモジュールの診断は ok 判定から除外する ---

    #[test]
    fn compile_project_ok_true_with_syntax_error_in_unreachable_module() {
        let p = project("main", &[("main", "return 1\n"), ("dead", "local a = (\n")]);
        let result = compile_project(&p, &ApiProjectCompileOptions::default());
        assert!(result.ok, "{:?}", result.error);
        assert!(result.code.is_some());
        assert!(result
            .diagnostics
            .iter()
            .any(|d| d.code == codes::SYNTAX_ERROR && d.module.as_deref() == Some("dead")));
    }

    #[test]
    fn compile_project_ok_true_with_input_outside_ontick_in_unreachable_module() {
        let p = project(
            "main",
            &[
                ("main", "return 1\n"),
                ("dead", "local x = input.getNumber(1)\nreturn x\n"),
            ],
        );
        let result = compile_project(&p, &ApiProjectCompileOptions::default());
        assert!(result.ok, "{:?}", result.error);
        assert!(result.code.is_some());
        let diag = result
            .diagnostics
            .iter()
            .find(|d| d.code == codes::INPUT_OUTSIDE_ONTICK && d.module.as_deref() == Some("dead"))
            .expect("input-outside-ontick diagnostic for unreachable module");
        assert_eq!(diag.severity, Severity::Error);
    }

    #[test]
    fn compile_project_ok_false_when_reachable_module_has_error() {
        // 到達可能なモジュール自身の error は従来どおり ok:false のまま。
        let p = project(
            "main",
            &[("main", "local x = input.getNumber(1)\nreturn x\n")],
        );
        let result = compile_project(&p, &ApiProjectCompileOptions::default());
        assert!(!result.ok);
        assert!(result.code.is_none());
    }
}
