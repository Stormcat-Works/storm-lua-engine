//! Shared first analyses for the target-mode speed passes (S1 / MR /
//! SMC), built once per base layout in `search.rs`.
//!
//! The layout construction in `search.rs` applies up to five speed
//! runs to one base AST (S1 full / S1 partial / MR / SMC full / SMC
//! partial, on both the localized base and the LICM base). Each run
//! used to redo the same pure analyses of the identical base:
//! `resolve`, an `EffectAnalyzer` construction (which itself
//! recomputes `mutated_builtin_roots` plus the alias and function
//! summary fixpoints), and — for S1 — a second
//! `mutated_builtin_roots`. This module lets the caller build the
//! trio once and lend it to every run on that base through
//! [`SpeedAnalysis`].
//!
//! Equivalence: `resolve`, `EffectAnalyzer::new` and
//! `mutated_builtin_roots` are deterministic pure functions of the
//! AST arena contents, and every `EffectAnalyzer` query takes
//! `&self` over tables precomputed at construction (there is no
//! interior mutability). Each pass internally works on
//! `clone_source(base)`, which copies the node and string arenas
//! verbatim, so an analysis built over the base returns exactly the
//! values an analysis built over the pass's private clone would.
//! The passes only read the shared tables through the view, so
//! sharing cannot change any pass result, layout, or winner. The
//! composite runs (SMC applied to an S1 / MR output) transform a
//! different AST and keep building their own analyses.

use std::collections::HashSet;

use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::Resolution;

/// Borrowing view over the pure analyses of one base AST, shared by
/// the speed passes applied to that base. The caller owns the three
/// values (built in the order `resolve` → `EffectAnalyzer::new` with
/// `stable_reads = true` → `mutated_builtin_roots`, exactly as each
/// pass builds them today) and keeps them alive next to the view.
pub struct SpeedAnalysis<'a> {
    pub resolution: &'a Resolution,
    pub effects: &'a EffectAnalyzer<'a>,
    pub mutated: &'a HashSet<String>,
}
