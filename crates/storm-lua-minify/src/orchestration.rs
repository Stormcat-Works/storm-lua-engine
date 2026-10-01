//! Phase 5a: TS `coreOptimize` orchestration parity.
//!
//! Individual pass semantics are Phase 4c's responsibility. This module fixes
//! their exact application order, 16-round fixpoint, seen-signature guard,
//! scope-renamed size gate, rollback, and final pass sequence.

use std::collections::HashSet;

use crate::config::{pass_enabled, NumericTolerance as ConfigTolerance, PassRecord};
use crate::pass::{PassResult, TraceEntry};
use crate::pass_ids::PassToggles;
use crate::passes;
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::print::Printer;
use storm_lua_syntax::size::measure_size;

fn folding_tolerance(t: Option<ConfigTolerance>) -> passes::literal_folding::NumericTolerance {
    let t = crate::config::resolve_folding_tolerance(t);
    passes::literal_folding::NumericTolerance {
        abs: t.abs,
        rel: t.rel,
    }
}

fn literal_tolerance(t: Option<ConfigTolerance>) -> ConfigTolerance {
    crate::config::resolve_literal_tolerance(t)
}

fn measured_size(ast: &Ast, root: NodeId, toggles: &PassToggles) -> usize {
    if pass_enabled(toggles, "scope-renaming") {
        crate::scope_rename::measure_renamed_size(ast, root)
    } else {
        measure_size(ast, root)
    }
}

fn assert_no_empty_if(ast: &Ast, root: NodeId, name: &str) {
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |id| {
        assert!(
            !matches!(ast.node(id), Node::If(arms, _) if arms.is_empty()),
            "invalid empty if after {name}"
        );
    });
}

#[allow(clippy::too_many_arguments)]
fn apply<F>(
    ast: &mut Ast,
    root: &mut NodeId,
    raw_size: &mut usize,
    id: &'static str,
    name: &'static str,
    toggles: &PassToggles,
    passes_out: &mut Vec<PassRecord>,
    trace: &mut Option<&mut dyn FnMut(TraceEntry)>,
    context: &str,
    f: F,
) where
    F: FnOnce(&mut Ast, NodeId) -> PassResult,
{
    if !pass_enabled(toggles, id) {
        return;
    }
    let before = *raw_size;
    // 表示専用の実測経過時間（CLAUDE.md §6: 探索判断・順位付けには使用しない）。
    // 意味論は `core::run_pass` のドキュメントコメントを参照。
    let started_at = web_time::Instant::now();
    let result = f(ast, *root);
    let elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0;
    *root = result.root;
    assert_no_empty_if(ast, *root, name);
    // Unlike TS object identity, a Rust NodeId can stay equal while its arena
    // slot changes in-place; always measuring is required for correctness.
    let after = measure_size(ast, *root);
    let saved = result
        .saved
        .map(|v| v as i64)
        .unwrap_or(before as i64 - after as i64);
    if after != before || result.details.as_ref().is_some_and(|d| !d.is_empty()) {
        passes_out.push(PassRecord {
            name: name.to_string(),
            saved: Some(saved),
            detail: result.details.map(|d| format!("{d:?}")),
            elapsed_ms: Some(elapsed_ms),
        });
    }
    if let Some(callback) = trace.as_deref_mut() {
        callback(TraceEntry {
            context: format!("core {context}"),
            name: name.to_string(),
            before_size: before,
            after_size: after,
            code: Printer::new(ast, false).output(*root),
        });
    }
    *raw_size = after;
}

fn constant_fold_with(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    tolerance: passes::literal_folding::NumericTolerance,
) -> PassResult {
    let (folded, folded_root) = passes::literal_folding::fold_expressions_with_options(
        ast, root, aggressive, tolerance, aggressive,
    );
    let (simplified, simplified_root) =
        passes::literal_folding::simplify_control_flow(&folded, folded_root, aggressive);
    *ast = simplified;
    PassResult {
        root: simplified_root,
        saved: None,
        details: None,
    }
}

/// Canonical OBJ-1 core optimizer. The 16-round limit is part of the frozen
/// deterministic search contract.
pub fn core_optimize(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    passes_out: &mut Vec<PassRecord>,
    toggles: &PassToggles,
    numeric_tolerance: Option<ConfigTolerance>,
    trace: Option<&mut dyn FnMut(TraceEntry)>,
) -> NodeId {
    core_optimize_with_round_limit(
        ast,
        root,
        aggressive,
        passes_out,
        toggles,
        numeric_tolerance,
        16,
        trace,
    )
}

/// Core optimizer with a deterministic outer-round budget. Target-driven and
/// target-free compilation share the canonical core; timing never changes it.
#[allow(clippy::redundant_closure, clippy::too_many_arguments)]
pub fn core_optimize_with_round_limit(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    passes_out: &mut Vec<PassRecord>,
    toggles: &PassToggles,
    numeric_tolerance: Option<ConfigTolerance>,
    max_outer_rounds: usize,
    mut trace: Option<&mut dyn FnMut(TraceEntry)>,
) -> NodeId {
    let tolerance = folding_tolerance(numeric_tolerance);
    let numeric_literal_tolerance = literal_tolerance(numeric_tolerance);
    let mut root = root;
    let mut raw_size = measure_size(ast, root);
    let mut renamed_size_cache: Option<usize> = None;
    let mut seen = HashSet::<String>::new();

    for outer in 0..max_outer_rounds {
        let signature = Printer::new(ast, false).output(root);
        if !seen.insert(signature) {
            break;
        }
        let iter_ast = ast.clone();
        let iter_root = root;
        let iter_raw_size = raw_size;
        let before = renamed_size_cache
            .take()
            .unwrap_or_else(|| measured_size(ast, root, toggles));
        let passes_start = passes_out.len();
        let context = format!("outer={outer}");
        macro_rules! ap {
            ($id:literal, $name:literal, $body:expr) => {
                apply(
                    ast,
                    &mut root,
                    &mut raw_size,
                    $id,
                    $name,
                    toggles,
                    passes_out,
                    &mut trace,
                    &context,
                    $body,
                )
            };
        }

        ap!(
            "unwritten-global-nil-propagation",
            "unwritten global nil propagation",
            |a, r| passes::global_stores::propagate_unwritten_globals_as_nil(a, r)
        );
        ap!("constant-folding", "constant folding", |a, r| {
            constant_fold_with(a, r, aggressive, tolerance)
        });
        ap!(
            "integer-loop-call-folding",
            "integer numeric-for call folding",
            |a, r| passes::induction_values::fold_integer_induction_calls(a, r)
        );
        ap!(
            "color-unpack-specialization",
            "color-unpack helper specialization",
            |a, r| passes::color_unpack::specialize_color_unpack_helpers(a, r)
        );
        ap!(
            "constant-argument-specialization",
            "constant-argument specialization",
            |a, r| passes::inline_functions::specialize_constant_arguments(
                a,
                r,
                aggressive && pass_enabled(toggles, "constant-folding"),
            )
        );
        ap!(
            "literal-call-folding",
            "literal user-function call folding",
            |a, r| passes::inline_functions::fold_literal_function_calls(a, r, aggressive)
        );
        ap!(
            "split-sign-recomposition-elimination",
            "split-sign recomposition elimination",
            |a, r| passes::split_sign::eliminate_split_sign_recomposition(a, r, aggressive)
        );
        ap!(
            "equivalent-trailing-argument-omission",
            "equivalent trailing argument omission",
            |a, r| passes::omit_arguments::omit_equivalent_trailing_arguments(a, r)
        );
        ap!(
            "table-parameter-scalarization",
            "table-parameter scalarization",
            |a, r| passes::tables::scalarize_table_literal_parameters(a, r, 16)
        );
        ap!(
            "sparse-boolean-decode-scalarization",
            "proof-driven sparse loop-array scalarization",
            |a, r| passes::sparse_boolean::sparse_boolean_decode_scalarization(a, r, aggressive)
        );
        ap!(
            "numeric-boolean-bit-specialization",
            "numeric boolean bit specialization",
            |a, r| passes::boolean_numerics::specialize_numeric_booleans(a, r)
        );
        ap!(
            "quotient-remainder-fusion",
            "quotient/remainder decode fusion",
            |a, r| passes::quotient_remainder::fuse_quotient_remainder_with_options(
                a, r, aggressive
            )
        );
        ap!(
            "quotient-chain-fusion",
            "unused-radix quotient chain fusion",
            |a, r| passes::decode_chains::fuse_quotient_chains(a, r)
        );
        ap!(
            "captured-single-use-hoisting",
            "captured single-use hoisting",
            |a, r| passes::captured_use::hoist_captured_single_uses_with_options(a, r, aggressive)
        );
        ap!(
            "immutable-global-expression-reuse",
            "immutable global expression reuse",
            |a, r| passes::immutable_values::reuse_immutable_values(a, r)
        );
        ap!(
            "multiplicative-carrier-reassociation",
            "multiplicative carrier reassociation",
            |a, r| passes::multiplicative_carriers::reuse_multiplicative_carriers(a, r)
        );
        ap!(
            "write-only-table-field-cleanup",
            "unread nil field cleanup",
            |a, r| passes::tables::remove_write_only_nil_table_fields(a, r)
        );
        ap!(
            "immutable-table-flattening",
            "immutable constant-table flattening",
            |a, r| passes::tables::flatten_immutable_tables(a, r, 8)
        );
        ap!(
            "closed-namespace-scalarization",
            "closed namespace scalar replacement",
            |a, r| passes::tables::scalarize_closed_namespaces(a, r, 8)
        );
        ap!(
            "one-use-function-fusion",
            "single-use statement/tail function fusion",
            |a, r| passes::inline_functions::inline_one_use_statement_and_tail_functions(a, r, 32)
        );
        ap!(
            "expression-helper-inlining",
            "whole-program expression helper inlining",
            |a, r| passes::inline_functions::inline_expression_functions(a, r, aggressive, true)
        );
        ap!(
            "immutable-table-flattening",
            "immutable constant-table flattening",
            |a, r| passes::tables::flatten_immutable_tables(a, r, 8)
        );
        ap!(
            "closed-namespace-scalarization",
            "closed namespace scalar replacement",
            |a, r| passes::tables::scalarize_closed_namespaces(a, r, 8)
        );
        ap!(
            "tiny-literal-inlining",
            "binding-aware tiny literal inlining",
            |a, r| passes::locals::inline_tiny_literal_bindings(a, r)
        );
        ap!(
            "single-use-local-sinking",
            "binding-aware single-use sinking",
            |a, r| passes::locals::inline_single_use_locals_with_options(a, r, aggressive, 16)
        );
        ap!(
            "signed-expression-factoring",
            "signed repeated expression factoring",
            |a, r| passes::signed_factoring::factor_signed_expressions(a, r, aggressive, 16)
        );
        ap!(
            "dead-local-elimination",
            "dead local elimination",
            |a, r| passes::locals::eliminate_dead_locals(a, r)
        );
        ap!(
            "terminal-scope-flattening",
            "terminal scope flattening",
            |a, r| passes::locals::flatten_terminal_do_blocks(a, r)
        );

        let after = measured_size(ast, root, toggles);
        if after > before {
            *ast = iter_ast;
            root = iter_root;
            raw_size = iter_raw_size;
            passes_out.truncate(passes_start);
            break;
        }
        if after == before {
            break;
        }
        renamed_size_cache = Some(after);
    }

    let context = "final";
    macro_rules! final_ap {
        ($id:literal, $name:literal, $body:expr) => {
            apply(
                ast,
                &mut root,
                &mut raw_size,
                $id,
                $name,
                toggles,
                passes_out,
                &mut trace,
                context,
                $body,
            )
        };
    }
    final_ap!(
        "ordered-screen-call-factoring",
        "ordered screen-call factoring",
        |a, r| passes::screen_call_factoring::factor_repeated_screen_calls(a, r)
    );
    let measure_renamed = pass_enabled(toggles, "scope-renaming");
    final_ap!(
        "binding-unit-rescaling",
        "numeric binding unit rescaling",
        |a, r| passes::binding_rescaling::rescale_bindings_with_options(
            a,
            r,
            aggressive,
            measure_renamed,
            4
        )
    );
    final_ap!(
        "interval-origin-shifting",
        "interval origin shifting",
        |a, r| passes::interval_shifting::shift_interval_origins_with_options(
            a,
            r,
            aggressive,
            measure_renamed,
            4
        )
    );
    final_ap!(
        "constant-wrapper-merging",
        "translated constant wrapper merging",
        |a, r| passes::wrapper_functions::merge_translated_constant_wrappers(a, r)
    );
    final_ap!(
        "affine-wrapper-merging",
        "affine constant wrapper merging",
        |a, r| passes::wrapper_functions::merge_affine_constant_wrappers(a, r)
    );
    final_ap!("constant-folding", "constant folding (final)", |a, r| {
        constant_fold_with(a, r, aggressive, tolerance)
    });
    final_ap!(
        "numeric-literal-approximation",
        "aggressive numeric literal approximation",
        |a, r| passes::numeric_literals::shorten_numeric_literals_with_options(
            a,
            r,
            aggressive,
            numeric_literal_tolerance
        )
    );
    root
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::config::resolve_pass_toggles;
    use crate::pass_ids::pass_id_for_record_name;
    use storm_lua_syntax::parser::parse_source;

    /// `core_optimize` が発行する全 `PassRecord.name` は
    /// `pass_ids::PASS_RECORD_NAMES` で id へ解決できなければならない
    /// （`ap!`/`final_ap!` 呼び出しサイトと表の drift を検出する）。
    #[test]
    fn all_produced_pass_record_names_resolve_to_pass_ids() {
        let source = "local value=3 function onTick() local x=input.getNumber(1) local unused=5 output.setNumber(1,x+value+0) end";
        for aggressive in [false, true] {
            let (mut ast, root) = parse_source(source).expect("parse ok");
            let toggles =
                resolve_pass_toggles(&Default::default(), crate::config::NumericMode::Tolerant);
            let mut passes_out = Vec::new();
            core_optimize(
                &mut ast,
                root,
                aggressive,
                &mut passes_out,
                &toggles,
                None,
                None,
            );
            assert!(!passes_out.is_empty(), "expected at least one pass record");
            for record in &passes_out {
                assert!(
                    pass_id_for_record_name(&record.name).is_some(),
                    "PassRecord.name {:?} has no entry in PASS_RECORD_NAMES (aggressive={aggressive})",
                    record.name
                );
            }
        }
    }
}
