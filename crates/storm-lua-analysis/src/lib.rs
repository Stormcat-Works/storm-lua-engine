//! Lua binding/effect analysis, logical project validation and diagnostics.
//! Compiler-only code: no VM, filesystem or scheduler is initialized.

#[doc(hidden)]
pub mod ambient_scan;
pub mod analyze;
pub mod diagnostic;
#[doc(hidden)]
pub mod effects;
#[doc(hidden)]
pub mod lint;
pub mod project;
pub mod property_scan;
#[doc(hidden)]
pub mod require_scan;
#[doc(hidden)]
pub mod resolve_dump;
#[doc(hidden)]
pub mod resolver;
#[doc(hidden)]
pub mod structure;
#[doc(hidden)]
pub mod sw_restrict;

pub use analyze::{analyze, AnalyzeMode, AnalyzeOptions, AnalyzeResult};
pub use diagnostic::{Diagnostic, Range, Severity};
pub use project::{AmbientMember, AmbientNamespace, LuaProject};
pub use property_scan::{scan_properties, PropertyScanResult};

/// Compilation/analysis profiles implemented by this compiler release.
/// Runtime Addon support does not imply Addon optimization support.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompilerTarget {
    /// Stormworks vehicle microcontroller Lua.
    #[default]
    Vehicle,
}

/// Shared environment checks and conservative-compilation requirements.
pub mod environment_checks;
