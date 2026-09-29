//! Lua project linking, source maps and coarse compiler API.
//! Compiler-only code: no VM, filesystem or scheduler is initialized.

pub mod lifeboat;
mod lifeboat_ambient;
#[doc(hidden)]
pub mod link;
mod lua_pattern;
pub mod public_api;
#[doc(hidden)]
pub mod source_map;

pub use public_api::{
    compile as minify, compile_project as build, ApiCompileOptions, ApiCompileResult,
    ApiProjectCompileOptions, ApiProjectCompileResult,
};
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod analysis_tests;
