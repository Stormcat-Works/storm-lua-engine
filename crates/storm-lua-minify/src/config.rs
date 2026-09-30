//! コンパイル設定（TS `config.ts` の移植）。
//!
//! パス個別 ON/OFF の既定値・明示トグル解決、および最適化対象外の
//! 仕様動的パス集合を保持する。決定性（NFR-2）に直結するため、
//! 既定値と解決規則は TS と byte 一致させる（`pass_ids_parity` で検証）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::pass_ids::PassId;
use crate::pass_ids::PassToggles;

/// exact 数値モードで無効化される計算誤差を伴うパス（TS `EXACT_UNSAFE_PASSES`）。
pub const EXACT_UNSAFE_PASSES: &[&str] = &[
    "constant-folding",
    "numeric-literal-approximation",
    "binding-unit-rescaling",
    "interval-origin-shifting",
    "literal-call-folding",
    "split-sign-recomposition-elimination",
    "multiplicative-carrier-reassociation",
    "coefficient-carrier-synthesis",
    "common-offset-absorption",
    "constant-wrapper-merging",
    "affine-wrapper-merging",
];

/// TS `resolvePassToggles`: exact モードでは `EXACT_UNSAFE_PASSES` を明示的に無効化する。
/// `toggles` の明示指定は保持しつつ、exact のとき誤差パスを `false` で上書きする。
pub fn resolve_pass_toggles(toggles: &PassToggles, numeric_mode: NumericMode) -> PassToggles {
    let mut resolved = toggles.clone();
    if numeric_mode == NumericMode::Exact {
        for id in EXACT_UNSAFE_PASSES {
            resolved.insert(id, false);
        }
    }
    resolved
}

/// Registered passes are enabled unless explicitly disabled. Retired or unknown
/// identifiers are never executable, even through a low-level explicit toggle.
pub fn pass_enabled(toggles: &PassToggles, id: PassId) -> bool {
    crate::pass_ids::is_valid_pass_id(id) && toggles.get(id).copied().unwrap_or(true)
}

/// TS `CompileOptions.numericMode`: `'tolerant' | 'exact'`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericMode {
    /// Allow the numeric approximations permitted by the configured tolerance.
    Tolerant,
    /// Disable approximate numeric transformations.
    Exact,
}

/// 数値リテラル折りたたみの許容誤差（TS `NumericTolerance`）。`abs`/`rel` 双方 0 で exact。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NumericTolerance {
    /// Absolute numeric tolerance.
    pub abs: f64,
    /// Relative numeric tolerance.
    pub rel: f64,
}

/// プロパティ読みの扱い（TS `PropertyConfig`）。
#[derive(Debug, Clone)]
pub struct PropertyConfig {
    /// Property read behavior.
    pub mode: PropertyMode,
    /// Numeric property values keyed by case-sensitive property name.
    pub numbers: Option<BTreeMap<String, f64>>,
    /// Boolean property values keyed by case-sensitive property name.
    pub bools: Option<BTreeMap<String, bool>>,
    /// `property.getText("key")` のハードコード値（追加機能）。未指定時は
    /// 既存の numbers/bools のみのフローと byte-identical。
    pub texts: Option<BTreeMap<String, String>>,
}

/// Whether property reads remain dynamic or use explicitly supplied constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyMode {
    /// Keep property reads at runtime.
    Runtime,
    /// Replace only property reads for explicitly supplied values.
    Hardcode,
}

/// 最適化モード（TS `CompileOptions.mode`: `'safe' | 'smallest'`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileMode {
    /// Use the conservative optimization pipeline.
    Safe,
    /// Enable the more aggressive whole-program size pipeline.
    Smallest,
}

/// 探索モード（TS `CompileOptions.searchMode`: `'exhaustive' | 'fast'`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Explore the full canonical structural candidate set.
    Exhaustive,
    /// Use the bounded deterministic search strategy.
    Fast,
}

/// 公開コンパイル設定（TS `CompileOptions`）。現時点で Phase 4 が読む
/// 最小限のフィールドのみ反映し、残りは移植対象パス進行に応じて追加する。
#[derive(Debug, Clone)]
pub struct CompileOptions {
    /// Opt-in original source label for internal provenance tracing. None keeps the
    /// existing no-tracking path. This is not yet the build SDK's Source Map API.
    pub origin_source: Option<String>,

    /// Script-visible profile shared with the runtime.
    pub environment: storm_lua_spec::environment::EnvironmentProfile,
    /// Dot-separated host-provided binding paths; replacing builtins requires conservative compilation.
    pub host_bindings: Vec<String>,
    /// Optimization pipeline selection.
    pub mode: CompileMode,
    /// Optional explicit property specialization settings.
    pub property: Option<PropertyConfig>,
    /// Whether separator newlines are uncharged by the size objective.
    pub zero_cost_newlines: bool,
    /// Per-pass escape hatches; unknown and retired identifiers are errors.
    pub pass_toggles: PassToggles,
    /// Limits for numeric transformations allowed by the selected mode.
    pub numeric_tolerance: Option<NumericTolerance>,
    /// Requested or resolved numeric transformation mode.
    pub numeric_mode: NumericMode,
    /// Search strategy; a supplied target size activates target-driven search.
    pub search_mode: SearchMode,
    /// Requested bound for fast search; public numeric input is truncated and clamped.
    pub search_beam_width: u32,
    /// When set, activate OBJ-2 and stop once a valid output is at or below this size.
    pub target_size: Option<usize>,
}

impl Default for CompileOptions {
    fn default() -> Self {
        CompileOptions {
            origin_source: None,
            environment: Default::default(),
            host_bindings: Vec::new(),
            mode: CompileMode::Smallest,
            property: None,
            zero_cost_newlines: true,
            pass_toggles: BTreeMap::new(),
            numeric_tolerance: None,
            numeric_mode: NumericMode::Tolerant,
            search_mode: SearchMode::Exhaustive,
            search_beam_width: 4,
            target_size: None,
        }
    }
}

/// 適用パスの記録（TS `PassRecord`。`name` のみ Phase 4 で必須）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PassRecord {
    /// Human-readable transformation record name.
    pub name: String,
    /// Character savings relative to the recorded baseline.
    pub saved: Option<i64>,
    /// Optional transformation-specific diagnostic detail.
    pub detail: Option<String>,
    /// Diagnostic elapsed milliseconds, not a candidate-ranking input.
    pub elapsed_ms: Option<f64>,
}

impl CompileOptions {
    /// Validate low-level callers; removed IDs are not silently ignored.
    pub(crate) fn validate_pass_toggles(&self) -> Result<(), String> {
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
        for id in self.pass_toggles.keys() {
            if !crate::pass_ids::is_valid_pass_id(id) {
                return Err(format!("unknown optimization pass: {id}"));
            }
        }
        Ok(())
    }
}

/// Shared effective default used by constant expression folding and recorded metadata.
pub fn resolve_folding_tolerance(value: Option<NumericTolerance>) -> NumericTolerance {
    value.unwrap_or(NumericTolerance {
        abs: 1e-12,
        rel: 1e-12,
    })
}
/// Shared effective default used by literal approximation before its per-literal caps.
pub fn resolve_literal_tolerance(value: Option<NumericTolerance>) -> NumericTolerance {
    value.unwrap_or(NumericTolerance {
        abs: 1e-6,
        rel: 1e-6,
    })
}
