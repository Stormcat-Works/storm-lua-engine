//! Vehicle Lua optimization passes and deterministic candidate search.
//! Compiler-only code: no VM, filesystem or scheduler is initialized.

pub mod config;
#[doc(hidden)]
pub mod orchestration;
#[doc(hidden)]
pub mod pass;
#[doc(hidden)]
pub mod pass_ids;
#[doc(hidden)]
pub mod passes;
#[doc(hidden)]
pub mod scope_rename;
#[doc(hidden)]
pub mod search;

pub use config::{CompileMode, CompileOptions, NumericMode, NumericTolerance, SearchMode};
pub use search::{compile_code, CompileCodeResult};

#[cfg(test)]
mod provenance_tests;
