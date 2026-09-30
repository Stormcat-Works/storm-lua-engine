//! Phase 5b: whole-program structural/candidate search parity with TS `compileSource`.
//! Public packaging/report metadata belongs to Phase 6; this module owns the
//! candidate graph, beam pruning, reparse validation, and deterministic winner.

use crate::config::{
    pass_enabled, resolve_pass_toggles, CompileMode, CompileOptions, NumericMode, NumericTolerance,
    PassRecord, SearchMode,
};
use crate::orchestration::core_optimize;
use crate::pass_ids::PassToggles;
use crate::pass_ids::OPTIMIZATION_PASS_IDS;
use crate::passes;
use crate::scope_rename::scope_rename_fast;
use serde::{Deserialize, Serialize};
use storm_lua_syntax::ast::{Ast, NodeId};
use storm_lua_syntax::parser::parse_source;
use storm_lua_syntax::print::Printer;
use storm_lua_syntax::provenance::GeneratedOrigins;
use storm_lua_syntax::size::measure_size;

#[derive(Clone, Serialize, Deserialize)]
struct Variant {
    name: String,
    ast: Ast,
    root: NodeId,
    passes: Vec<PassRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Candidate {
    origins: Option<GeneratedOrigins>,
    structural: String,
    layout: String,
    code: String,
    size: usize,
    order: usize,
    passes: Vec<PassRecord>,
    aliases: Vec<String>,
    zero_cost_newlines: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateSize {
    pub structural: String,
    pub layout: String,
    pub size: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchStats {
    pub candidates: usize,
    pub attempted: usize,
    pub parse_rejected: usize,
    pub semantic_rejected: usize,
    pub core_variants: usize,
    pub structural_variants: usize,
    pub structural_explored: usize,
    pub structural_names: Vec<String>,
    pub winner_structural: String,
    pub winner_layout: String,
    pub candidate_sizes: Vec<CandidateSize>,
    /// OBJ-2 target. `None` means canonical OBJ-1 size minimization.
    pub target_size: Option<usize>,
    pub target_met: bool,
    /// True when OBJ-2 returned before exhaustive search completed.
    pub stopped_early: bool,
    /// Deterministic anytime checkpoints completed by the OBJ-2 scheduler.
    pub checkpoints: usize,
    /// Last OBJ-2 stage reached. `None` for OBJ-1.
    pub stage: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompileCodeResult {
    /// Final-code provenance when requested. Unknown ranges remain explicit;
    /// not every transformation has been annotated yet, so this is not a full map.
    pub origins: Option<GeneratedOrigins>,
    pub code: String,
    pub stats: SearchStats,
    pub passes: Vec<PassRecord>,
    pub api_aliases: Vec<String>,
    pub property_reads_hardcoded: usize,
    pub zero_cost_newlines: u32,
}

fn folding_tolerance(t: Option<NumericTolerance>) -> passes::literal_folding::NumericTolerance {
    let t = crate::config::resolve_folding_tolerance(t);
    passes::literal_folding::NumericTolerance {
        abs: t.abs,
        rel: t.rel,
    }
}

fn constant_fold_with(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    tolerance: Option<NumericTolerance>,
) -> NodeId {
    let (folded, folded_root) = passes::literal_folding::fold_expressions_with_options(
        ast,
        root,
        aggressive,
        folding_tolerance(tolerance),
        aggressive,
    );
    let (simplified, simplified_root) =
        passes::literal_folding::simplify_control_flow(&folded, folded_root, aggressive);
    *ast = simplified;
    simplified_root
}

fn renamed_size(ast: &Ast, root: NodeId, toggles: &PassToggles) -> usize {
    if pass_enabled(toggles, "scope-renaming") {
        let renamed = scope_rename_fast(ast, root);
        measure_size(&renamed.ast, renamed.root)
    } else {
        measure_size(ast, root)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct PassMask {
    low: u64,
    high: u64,
}

impl PassMask {
    fn from_toggles(toggles: &PassToggles) -> Self {
        let mut mask = Self { low: 0, high: 0 };
        for (index, &id) in OPTIMIZATION_PASS_IDS.iter().enumerate() {
            if pass_enabled(toggles, id) {
                if index < 64 {
                    mask.low |= 1_u64 << index;
                } else {
                    mask.high |= 1_u64 << (index - 64);
                }
            }
        }
        mask
    }

    #[inline]
    fn enabled(&self, id: &str) -> bool {
        let Some(index) = OPTIMIZATION_PASS_IDS
            .iter()
            .position(|candidate| *candidate == id)
        else {
            return false;
        };
        if index < 64 {
            self.low & (1_u64 << index) != 0
        } else {
            self.high & (1_u64 << (index - 64)) != 0
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CandidateJob {
    lexical_origins: Option<GeneratedOrigins>,
    lexical_source: Option<String>,
    variant: Variant,
    aggressive: bool,
    toggles: PassMask,
    numeric_tolerance: Option<NumericTolerance>,
    exact_safe_constant_folding: bool,
    zero_cost_newlines: bool,
    order_base: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchContext {
    lexical: bool,
    lexical_target: Option<usize>,
    property_reads_hardcoded: usize,
    core_variants: usize,
    structural_variants: usize,
    structural_names: Vec<String>,
    // Canonical jobs already evaluated by target search. The encoded context
    // carries these across the Worker boundary so they are never run twice.
    completed_batches: Vec<CandidateBatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateBatch {
    candidates: Vec<Candidate>,
    attempted: usize,
    parse_rejected: usize,
    semantic_rejected: usize,
}

/// Result of target search over the canonical candidate set. `NeedsFullSearch`
/// exports only remaining jobs plus the already evaluated batches. `Stopped`
/// means a checkpoint budget ended or the canonical search completed unmet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SatisficingStatus {
    Satisfied,
    Stopped,
    NeedsFullSearch,
}

#[derive(Clone)]
pub struct SatisficingAttempt {
    pub status: SatisficingStatus,
    pub result: CompileCodeResult,
    fallback: Option<(SearchContext, Vec<CandidateJob>)>,
}

impl std::fmt::Debug for SatisficingAttempt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SatisficingAttempt")
            .field("status", &self.status)
            .field("result", &self.result)
            .finish()
    }
}

impl PartialEq for SatisficingAttempt {
    fn eq(&self, other: &Self) -> bool {
        self.status == other.status && self.result == other.result
    }
}

// Probe a bounded prefix in the coordinator, then send only the remaining
// canonical jobs to the Worker pool. This is a deterministic work budget, not
// a wall-clock limit. Completed batches stay in the search context.
const SATISFICING_MAX_CANDIDATE_JOBS: usize = 2;

fn parse_for_search(source: &str, options: &CompileOptions) -> Result<(Ast, NodeId), String> {
    if let Some(name) = &options.origin_source {
        storm_lua_syntax::parse_source_with_origins(name, source)
    } else {
        parse_source(source).map_err(|error| error.to_string())
    }
}

fn final_output(ast: &Ast, root: NodeId, zero: bool) -> (String, u32, Option<GeneratedOrigins>) {
    let mut printer = Printer::new(ast, zero);
    if ast.nodes.tracks_origins() {
        let printed = printer.output_with_positions(root);
        let origins = GeneratedOrigins::from_print(ast, &printed);
        (printed.code, printer.newline_count(), origins)
    } else {
        let code = printer.output(root);
        (code, printer.newline_count(), None)
    }
}

pub fn prepare_search(
    source: &str,
    options: &CompileOptions,
) -> Result<(SearchContext, Vec<CandidateJob>), String> {
    prepare_search_with_dedup(source, options, true)
}

fn prepare_lexical(
    source: &str,
    ast: Ast,
    root: NodeId,
    options: &CompileOptions,
) -> Result<(SearchContext, Vec<CandidateJob>), String> {
    let (code, lexical_origins) = if let Some(name) = &options.origin_source {
        let (code, origins) = storm_lua_syntax::print::lexical_minify_with_origins(name, source)
            .map_err(|error| error.to_string())?;
        (code, Some(origins))
    } else {
        (
            storm_lua_syntax::print::lexical_minify(source).map_err(|error| error.to_string())?,
            None,
        )
    };
    parse_source(&code).map_err(|e| e.to_string())?;
    Ok((
        SearchContext {
            lexical: true,
            lexical_target: options.target_size,
            property_reads_hardcoded: 0,
            core_variants: 0,
            structural_variants: 1,
            structural_names: vec!["lexical".into()],
            completed_batches: Vec::new(),
        },
        vec![CandidateJob {
            lexical_origins,
            lexical_source: Some(code),
            variant: Variant {
                name: "lexical".into(),
                ast,
                root,
                passes: Vec::new(),
            },
            aggressive: false,
            toggles: PassMask { low: 0, high: 0 },
            numeric_tolerance: None,
            exact_safe_constant_folding: false,
            zero_cost_newlines: false,
            order_base: 0,
        }],
    ))
}

fn prepare_search_with_dedup(
    source: &str,
    options: &CompileOptions,
    deduplicate: bool,
) -> Result<(SearchContext, Vec<CandidateJob>), String> {
    options.validate_pass_toggles()?;
    let (parsed_ast, parsed_root) = parse_for_search(source, options)?;
    storm_lua_analysis::environment_checks::validate(
        &parsed_ast,
        parsed_root,
        options.environment,
        &options.host_bindings,
    )?;
    if storm_lua_analysis::environment_checks::lexical_reason(
        &parsed_ast,
        options.environment,
        &options.host_bindings,
    )
    .is_some()
    {
        return prepare_lexical(source, parsed_ast, parsed_root, options);
    }
    let (base_ast, base_root, property_reads_hardcoded) =
        passes::property_reads::transform_property_reads(
            &parsed_ast,
            parsed_root,
            options.property.as_ref(),
        );
    prepare_search_from_base(
        options,
        base_ast,
        base_root,
        property_reads_hardcoded,
        deduplicate,
    )
}

fn prepare_search_from_base(
    options: &CompileOptions,
    mut base_ast: storm_lua_syntax::ast::Ast,
    base_root: NodeId,
    property_reads_hardcoded: usize,
    deduplicate: bool,
) -> Result<(SearchContext, Vec<CandidateJob>), String> {
    let aggressive = options.mode == CompileMode::Smallest;
    let exact_safe_constant_folding = options.numeric_mode == NumericMode::Exact
        && pass_enabled(&options.pass_toggles, "constant-folding");
    let toggles = resolve_pass_toggles(&options.pass_toggles, options.numeric_mode);
    let mut initial_passes = if property_reads_hardcoded > 0 {
        vec![PassRecord {
            name: "property hardcoding".into(),
            saved: None,
            detail: Some(format!("{{\"replaced\":{property_reads_hardcoded}}}")),
            elapsed_ms: None,
        }]
    } else {
        Vec::new()
    };
    if pass_enabled(&toggles, "exact-numeric-literal-pooling") {
        let canonical =
            passes::numeric_literals::canonicalize_exact_float_literals(&mut base_ast, base_root);
        if canonical.saved.unwrap_or(0) > 0 {
            initial_passes.push(PassRecord {
                name: "exact numeric literal pooling".into(),
                saved: canonical.saved.map(|saved| saved as i64),
                detail: canonical.details.map(|parts| parts.join(";")),
                elapsed_ms: None,
            });
        }
    }

    let mut optimized_ast = base_ast.clone();
    let mut optimized_passes = initial_passes.clone();
    let optimized_root = core_optimize(
        &mut optimized_ast,
        base_root,
        aggressive,
        &mut optimized_passes,
        &toggles,
        options.numeric_tolerance,
        None,
    );
    let core_variants = vec![Variant {
        name: "optimized".into(),
        ast: optimized_ast,
        root: optimized_root,
        passes: optimized_passes.clone(),
    }];

    let mut structural = Vec::<Variant>::new();
    for variant in &core_variants {
        structural.push(Variant {
            name: format!("{}:locals", variant.name),
            ast: variant.ast.clone(),
            root: variant.root,
            passes: variant.passes.clone(),
        });

        let root_global = if pass_enabled(&toggles, "root-local-globalization") {
            let mut ast = variant.ast.clone();
            let result = passes::root_globals::globalize_root_locals(&mut ast, variant.root);
            Some((ast, result.root))
        } else {
            None
        };
        if let Some((ast, root)) = &root_global {
            structural.push(Variant {
                name: format!("{}:root-global", variant.name),
                ast: ast.clone(),
                root: *root,
                passes: variant.passes.clone(),
            });
        }

        if pass_enabled(&toggles, "function-local-globalization") {
            let mut ast = variant.ast.clone();
            let result = passes::function_globalization::globalize_function_locals(
                &mut ast,
                variant.root,
                0,
            );
            structural.push(Variant {
                name: format!("{}:function-global", variant.name),
                ast,
                root: result.root,
                passes: variant.passes.clone(),
            });

            if let Some((root_ast, root_root)) = &root_global {
                let mut all_ast = root_ast.clone();
                let all = passes::function_globalization::globalize_function_locals(
                    &mut all_ast,
                    *root_root,
                    0,
                );
                let all_root = all.root;
                structural.push(Variant {
                    name: format!("{}:all-global", variant.name),
                    ast: all_ast.clone(),
                    root: all_root,
                    passes: variant.passes.clone(),
                });

                if pass_enabled(&toggles, "hybrid-function-local-globalization") {
                    for minimum_frequency in [2_u32, 3, 5, 8] {
                        let mut hybrid_ast = root_ast.clone();
                        let hybrid = passes::function_globalization::globalize_function_locals(
                            &mut hybrid_ast,
                            *root_root,
                            minimum_frequency,
                        );
                        structural.push(Variant {
                            name: format!("{}:hybrid-f{minimum_frequency}", variant.name),
                            ast: hybrid_ast,
                            root: hybrid.root,
                            passes: variant.passes.clone(),
                        });
                    }
                }
            }
        }
    }

    let requested = options.search_beam_width.clamp(1, 16) as usize;
    let structural_total = structural.len();
    let mut searched_structural = structural;
    // A target is a size objective, not permission to discard candidates.
    // Unmet targets must retain the exhaustive winner even when the caller
    // carries a fast-search preference. Target-free fast mode keeps its bound.
    if options.target_size.is_none()
        && options.search_mode == SearchMode::Fast
        && searched_structural.len() > requested
    {
        let mut ranked = searched_structural
            .into_iter()
            .enumerate()
            .map(|(index, candidate)| {
                let estimated = renamed_size(&candidate.ast, candidate.root, &toggles);
                (estimated, index, candidate)
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        searched_structural = ranked
            .into_iter()
            .take(requested)
            .map(|(_, _, candidate)| candidate)
            .collect();
    }
    // Deduplicate structurally identical jobs after beam selection. Compare
    // the complete arena and interner, not printed text or an approximate hash.
    // Options are shared by all jobs here. The first occurrence retains the
    // original tie order; we skip duplicate work, not a distinct optimization.
    let mut unique = Vec::<Variant>::with_capacity(searched_structural.len());
    for variant in searched_structural {
        if !deduplicate
            || !unique
                .iter()
                .any(|v| v.root == variant.root && v.ast == variant.ast)
        {
            unique.push(variant);
        }
    }
    let searched_structural = unique;
    let structural_names = searched_structural
        .iter()
        .map(|variant| variant.name.clone())
        .collect::<Vec<_>>();
    let job_toggles = PassMask::from_toggles(&toggles);
    let layouts_per_job = if job_toggles.enabled("api-alias-optimization") {
        2
    } else {
        1
    };
    let jobs = searched_structural
        .into_iter()
        .enumerate()
        .map(|(index, variant)| CandidateJob {
            lexical_origins: None,
            lexical_source: None,
            variant,
            aggressive,
            toggles: job_toggles,
            numeric_tolerance: options.numeric_tolerance,
            exact_safe_constant_folding,
            zero_cost_newlines: options.zero_cost_newlines,
            order_base: index * layouts_per_job,
        })
        .collect::<Vec<_>>();
    Ok((
        SearchContext {
            lexical: false,
            lexical_target: None,
            property_reads_hardcoded,
            core_variants: core_variants.len(),
            structural_variants: structural_total,
            structural_names,
            completed_batches: Vec::new(),
        },
        jobs,
    ))
}

fn evaluate_candidate_with_verifier(
    job: CandidateJob,
    verify_candidate: &mut Option<&mut dyn FnMut(&str) -> bool>,
) -> Result<CandidateBatch, String> {
    if let Some(code) = job.lexical_source.as_ref() {
        if let Some(verify) = verify_candidate.as_deref_mut() {
            if !verify(code) {
                return Ok(CandidateBatch {
                    candidates: Vec::new(),
                    attempted: 1,
                    parse_rejected: 0,
                    semantic_rejected: 1,
                });
            }
        }
        return Ok(CandidateBatch {
            candidates: vec![Candidate {
                origins: job.lexical_origins.clone(),
                structural: "lexical".into(),
                layout: "tokens".into(),
                code: code.clone(),
                size: target_char_size(code),
                order: 0,
                passes: Vec::new(),
                aliases: Vec::new(),
                zero_cost_newlines: 0,
            }],
            attempted: 1,
            parse_rejected: 0,
            semantic_rejected: 0,
        });
    }
    let mut candidates = Vec::<Candidate>::new();
    let mut attempted = 0usize;
    let mut parse_rejected = 0usize;
    let mut semantic_rejected = 0usize;

    let CandidateJob {
        lexical_origins: _,
        lexical_source: _,
        variant: structural_candidate,
        aggressive,
        toggles,
        numeric_tolerance,
        exact_safe_constant_folding,
        zero_cost_newlines,
        order_base,
    } = job;
    let structural_name = structural_candidate.name;
    let structural_passes = structural_candidate.passes;
    let mut ast = structural_candidate.ast;
    let mut root = structural_candidate.root;

    if toggles.enabled("destructive-radix-helper-synthesis") {
        let r = passes::radix_helpers::synthesize_destructive_radix_helpers(
            &mut ast, root, aggressive, 4,
        );
        root = r.root;
    }
    if toggles.enabled("destructive-radix-terminal-reuse") {
        let r =
            passes::radix_helpers::reuse_terminal_radix_quotients(&mut ast, root, aggressive, 8);
        root = r.root;
    }
    if toggles.enabled("coefficient-carrier-synthesis") {
        let r = passes::coefficient_carriers::synthesize_coefficient_carriers(
            &mut ast, root, aggressive, 8,
        );
        root = r.root;
    }
    if toggles.enabled("destructive-result-globalization") {
        let r = passes::destructive_results::globalize_destructive_results_with_options(
            &mut ast, root, aggressive, 8,
        );
        root = r.root;
    }
    if toggles.enabled("ordered-screen-loop-synthesis") {
        let periodic = toggles.enabled("periodic-screen-loop-synthesis");
        let r = passes::screen_loops::synthesize_screen_loops(&mut ast, root, periodic);
        root = r.root;
    }
    if toggles.enabled("output-sequence-loop-synthesis") {
        let r = passes::output_loops::synthesize_output_loops(&mut ast, root);
        root = r.root;
    }
    if toggles.enabled("uniform-table-fill-scalarization") {
        let r = passes::uniform_tables::scalarize_uniform_fill_tables(&mut ast, root);
        root = r.root;
    }
    if toggles.enabled("global-store-cleanup") {
        let r =
            passes::global_stores::cleanup_global_stores_with_options(&mut ast, root, aggressive);
        root = r.root;
    }
    if toggles.enabled("equal-scratch-value-coalescing") {
        let r = passes::scratch_coalescing::coalesce_equal_scratch_values(&mut ast, root);
        root = r.root;
    }
    if toggles.enabled("single-use-global-forwarding") {
        let r = passes::single_use_forwarding::forward_single_use_globals(&mut ast, root);
        root = r.root;
    }
    if toggles.enabled("temporary-global-packing") {
        let r = passes::temporary_globals::pack_temporary_globals(&mut ast, root);
        root = r.root;
    }
    if toggles.enabled("global-store-cleanup") {
        let r =
            passes::global_stores::cleanup_global_stores_with_options(&mut ast, root, aggressive);
        root = r.root;
    }
    if toggles.enabled("immutable-carrier-synthesis") {
        let r = passes::immutable_synthesis::synthesize_immutable_carriers_with_options(
            &mut ast, root, aggressive, 8,
        );
        root = r.root;
    }
    if toggles.enabled("available-expression-reuse") {
        let r = passes::available_expressions::reuse_available_expressions(&mut ast, root);
        root = r.root;
    }
    if toggles.enabled("literal-call-folding") {
        let r = passes::inline_functions::fold_literal_function_calls(&mut ast, root, aggressive);
        root = r.root;
    }
    if toggles.enabled("split-sign-recomposition-elimination") {
        let r = passes::split_sign::eliminate_split_sign_recomposition(&mut ast, root, aggressive);
        root = r.root;
    }
    if toggles.enabled("else-default-hoisting") {
        let r = passes::default_hoisting::hoist_else_defaults_with_options(
            &mut ast, root, aggressive, 8,
        );
        root = r.root;
    }
    if toggles.enabled("screen-button-outlining") {
        let r = passes::screen_buttons::outline_screen_buttons(&mut ast, root, aggressive);
        root = r.root;
    }
    if toggles.enabled("callback-exclusive-function-slotting") {
        let r = passes::callback_slots::slot_callback_exclusive_functions(&mut ast, root);
        root = r.root;
    }

    let phase_ast = ast;
    let phase_root = root;
    let mut layouts = Vec::<(&'static str, Ast, NodeId, Vec<String>)>::new();
    if toggles.enabled("scope-renaming") {
        let renamed = scope_rename_fast(&phase_ast, phase_root);
        layouts.push(("none", renamed.ast, renamed.root, Vec::new()));
    } else {
        layouts.push(("none", phase_ast.clone(), phase_root, Vec::new()));
    }
    if toggles.enabled("api-alias-optimization") {
        let mut alias_ast = phase_ast.clone();
        let alias = passes::api_aliases::optimize_api_aliases(&mut alias_ast, phase_root);
        let aliases = alias.details.clone().unwrap_or_default();
        layouts.push(("joint-api", alias_ast, alias.root, aliases));
    }

    for (layout_index, (layout, mut candidate_ast, mut candidate_root, aliases)) in
        layouts.into_iter().enumerate()
    {
        let mut candidate_passes = structural_passes.clone();
        attempted += 1;
        let order = order_base + layout_index;
        if toggles.enabled("one-use-expression-helper-reversal") {
            let r = passes::inline_functions::inline_one_use_expression_helpers(
                &mut candidate_ast,
                candidate_root,
                aggressive,
            );
            candidate_root = r.root;
        }
        if toggles.enabled("constant-folding") {
            candidate_root = constant_fold_with(
                &mut candidate_ast,
                candidate_root,
                aggressive,
                numeric_tolerance,
            );
        } else if exact_safe_constant_folding {
            let result = passes::literal_folding::fold_exact_safe_control_flow(
                &mut candidate_ast,
                candidate_root,
            );
            candidate_root = result.root;
            if result.saved.unwrap_or(0) > 0 {
                candidate_passes.push(PassRecord {
                    name: "constant folding".into(),
                    saved: result.saved.map(|saved| saved as i64),
                    detail: Some("exact-safe literal truthiness/control-flow subset".into()),
                    elapsed_ms: None,
                });
            }
            // Exact-safe branch pruning runs after core optimization and may
            // introduce `do ... end` solely to preserve the selected branch's
            // lexical scope. Once that body has no directly scoped declaration
            // or control-flow boundary, the wrapper is redundant even before a
            // following statement. Keep this cleanup on the exact-safe path so
            // tolerant/default candidates pay no extra whole-AST traversal.
            if toggles.enabled("terminal-scope-flattening") {
                let r =
                    passes::locals::flatten_terminal_do_blocks(&mut candidate_ast, candidate_root);
                candidate_root = r.root;
            }
        }
        if toggles.enabled("split-sign-recomposition-elimination") {
            let r = passes::split_sign::eliminate_split_sign_recomposition(
                &mut candidate_ast,
                candidate_root,
                aggressive,
            );
            candidate_root = r.root;
        }
        if toggles.enabled("conditional-call-lowering") {
            let r =
                passes::conditionals::lower_conditional_calls(&mut candidate_ast, candidate_root);
            candidate_root = r.root;
        }
        if toggles.enabled("conditional-assignment-lowering") {
            let r = passes::conditionals::lower_conditional_assignments(
                &mut candidate_ast,
                candidate_root,
            );
            candidate_root = r.root;
        }
        if toggles.enabled("conditional-return-lowering") {
            let r =
                passes::conditionals::lower_conditional_returns(&mut candidate_ast, candidate_root);
            candidate_root = r.root;
        }
        if toggles.enabled("final-dead-store-elimination") {
            let r = passes::final_stores::eliminate_overwritten_assignments_with_options(
                &mut candidate_ast,
                candidate_root,
                aggressive,
            );
            candidate_root = r.root;
        }
        if toggles.enabled("global-store-cleanup") {
            let r = passes::global_stores::remove_unread_global_stores(
                &mut candidate_ast,
                candidate_root,
            );
            candidate_root = r.root;
        }
        if toggles.enabled("quotient-chain-fusion") {
            let r = passes::decode_chains::fuse_quotient_chains(&mut candidate_ast, candidate_root);
            candidate_root = r.root;
        }
        if toggles.enabled("split-sign-recomposition-elimination") {
            let r = passes::split_sign::eliminate_split_sign_recomposition(
                &mut candidate_ast,
                candidate_root,
                aggressive,
            );
            candidate_root = r.root;
        }
        if toggles.enabled("available-expression-reuse") {
            let r = passes::available_expressions::reuse_available_expressions(
                &mut candidate_ast,
                candidate_root,
            );
            candidate_root = r.root;
        }
        if toggles.enabled("binding-unit-rescaling") {
            let r = passes::binding_rescaling::rescale_bindings_with_options(
                &mut candidate_ast,
                candidate_root,
                aggressive,
                false,
                4,
            );
            candidate_root = r.root;
        }
        if toggles.enabled("final-dead-store-elimination") {
            let r = passes::final_stores::eliminate_overwritten_assignments_with_options(
                &mut candidate_ast,
                candidate_root,
                aggressive,
            );
            candidate_root = r.root;
        }
        if toggles.enabled("redundant-parentheses-elimination") {
            let r = passes::parentheses::remove_redundant_parentheses(
                &mut candidate_ast,
                candidate_root,
            );
            candidate_root = r.root;
        }

        if aggressive && toggles.enabled("closed-table-field-renaming") {
            let r = passes::closed_fields::rename_closed_fields(&mut candidate_ast, candidate_root);
            candidate_root = r.root;
            if r.saved.unwrap_or(0) > 0 {
                candidate_passes.push(PassRecord {
                    name: "closed table field renaming".into(),
                    saved: r.saved.map(|saved| saved as i64),
                    detail: r.details.map(|parts| parts.join(";")),
                    elapsed_ms: None,
                });
            }
        }
        if aggressive && toggles.enabled("closed-namespace-devirtualization") {
            let r = passes::namespace_functions::devirtualize_closed_namespaces(
                &mut candidate_ast,
                candidate_root,
                toggles.enabled("scope-renaming"),
            );
            candidate_root = r.root;
            if r.saved.unwrap_or(0) > 0 {
                candidate_passes.push(PassRecord {
                    name: "closed namespace devirtualization".into(),
                    saved: r.saved.map(|saved| saved as i64),
                    detail: r.details.map(|parts| parts.join(";")),
                    elapsed_ms: None,
                });
            }
        }
        if aggressive && toggles.enabled("repeated-draw-sequence-outlining") {
            let r = passes::draw_sequences::outline_draw_sequences(
                &mut candidate_ast,
                candidate_root,
                toggles.enabled("scope-renaming"),
            );
            candidate_root = r.root;
            if r.saved.unwrap_or(0) > 0 {
                candidate_passes.push(PassRecord {
                    name: "repeated draw sequence outlining".into(),
                    saved: r.saved.map(|x| x as i64),
                    detail: r.details.map(|parts| parts.join(";")),
                    elapsed_ms: None,
                });
            }
        }
        let drawing_variants = if aggressive && toggles.enabled("ordered-draw-record-packing") {
            passes::draw_records::record_variants(
                &candidate_ast,
                candidate_root,
                toggles.enabled("scope-renaming"),
            )
        } else {
            vec![(
                candidate_ast,
                crate::pass::PassResult {
                    root: candidate_root,
                    saved: Some(0),
                    details: None,
                },
            )]
        };
        let mut final_variant = None;
        let mut final_size = usize::MAX;
        for (mut candidate_ast, r) in drawing_variants {
            let mut candidate_root = r.root;
            let mut candidate_passes = candidate_passes.clone();
            if r.saved.unwrap_or(0) > 0 {
                candidate_passes.push(PassRecord {
                    name: "ordered draw record packing".into(),
                    saved: r.saved.map(|x| x as i64),
                    detail: r.details.map(|parts| parts.join(";")),
                    elapsed_ms: None,
                });
            }
            // Drawing compression creates helpers after structural search. Give
            // those new bindings the same two existing cleanup opportunities, but
            // retain the unpolished candidate unless its FINAL renamed text wins.
            // No recursive compile/search and no additional optimization toggle.
            let drawing_changed = candidate_passes.iter().any(|p| {
                p.name == "ordered draw record packing"
                    || p.name == "repeated draw sequence outlining"
            });
            if aggressive && drawing_changed {
                for (id, display) in [
                    (
                        "root-local-globalization",
                        "post-draw root local globalization",
                    ),
                    (
                        "one-use-function-fusion",
                        "post-draw single-use function fusion",
                    ),
                ] {
                    if !toggles.enabled(id) {
                        continue;
                    }
                    let before = target_char_size(
                        &Printer::new(&candidate_ast, false).output(candidate_root),
                    );
                    let mut polished = candidate_ast.clone();
                    let result = if id == "root-local-globalization" {
                        passes::root_globals::globalize_root_locals(&mut polished, candidate_root)
                    } else {
                        passes::inline_functions::inline_one_use_statement_and_tail_functions(
                            &mut polished,
                            candidate_root,
                            32,
                        )
                    };
                    let mut root = result.root;
                    if toggles.enabled("scope-renaming") {
                        let renamed = crate::scope_rename::scope_rename_fast(&polished, root);
                        polished = renamed.ast;
                        root = renamed.root;
                    }
                    let after = target_char_size(&Printer::new(&polished, false).output(root));
                    if after < before {
                        candidate_ast = polished;
                        candidate_root = root;
                        candidate_passes.push(PassRecord {
                            name: display.into(),
                            saved: Some((before - after) as i64),
                            detail: None,
                            elapsed_ms: None,
                        });
                    }
                }
            }
            if aggressive && toggles.enabled("exact-numeric-literal-pooling") {
                let r = passes::literal_pool::pool_numeric_literals(
                    &mut candidate_ast,
                    candidate_root,
                    toggles.enabled("scope-renaming"),
                );
                candidate_root = r.root;
                if r.saved.unwrap_or(0) > 0 {
                    candidate_passes.push(PassRecord {
                        name: "exact numeric literal pooling".into(),
                        saved: r.saved.map(|saved| saved as i64),
                        detail: r.details.map(|parts| parts.join(";")),
                        elapsed_ms: None,
                    });
                }
            }
            if aggressive && toggles.enabled("adjacent-local-declaration-packing") {
                let r = passes::adjacent_locals::pack_adjacent_locals(
                    &mut candidate_ast,
                    candidate_root,
                    toggles.enabled("scope-renaming"),
                );
                candidate_root = r.root;
                if r.saved.unwrap_or(0) > 0 {
                    candidate_passes.push(PassRecord {
                        name: "adjacent local declaration packing".into(),
                        saved: r.saved.map(|n| n as i64),
                        detail: r.details.map(|d| d.join(";")),
                        elapsed_ms: None,
                    });
                }
            }
            let size =
                target_char_size(&Printer::new(&candidate_ast, false).output(candidate_root));
            if size < final_size {
                final_size = size;
                final_variant = Some((candidate_ast, candidate_root, candidate_passes));
            }
        }
        #[expect(
            clippy::expect_used,
            reason = "The final-variant list includes the unmodified valid candidate; a realizable source size is below usize::MAX"
        )]
        let (candidate_ast, candidate_root, candidate_passes) =
            final_variant.expect("at least one final variant");
        let compact = Printer::new(&candidate_ast, false).output(candidate_root);
        if parse_source(&compact).is_err() {
            parse_rejected += 1;
            continue;
        }
        if let Some(verify) = verify_candidate.as_deref_mut() {
            if !verify(&compact) {
                semantic_rejected += 1;
                continue;
            }
        }
        let size = target_char_size(&compact);
        let (code, newline_count, origins) =
            final_output(&candidate_ast, candidate_root, zero_cost_newlines);
        if target_char_size(&code) != size {
            return Err("zero-cost newline printer changed size".into());
        }
        candidates.push(Candidate {
            origins,
            structural: structural_name.clone(),
            layout: layout.to_string(),
            code,
            size,
            order,
            passes: candidate_passes,
            aliases,
            zero_cost_newlines: newline_count,
        });
    }

    Ok(CandidateBatch {
        candidates,
        attempted,
        parse_rejected,
        semantic_rejected,
    })
}

pub fn evaluate_candidate(job: CandidateJob) -> Result<CandidateBatch, String> {
    let mut verifier = None;
    evaluate_candidate_with_verifier(job, &mut verifier)
}

fn select_best_candidates(
    context: SearchContext,
    mut candidates: Vec<Candidate>,
    attempted: usize,
    parse_rejected: usize,
    semantic_rejected: usize,
) -> Result<CompileCodeResult, String> {
    let candidate_count = candidates.len();
    if candidates.is_empty() {
        return Err("No semantically valid optimization candidate survived verification.".into());
    }
    candidates.sort_by(|a, b| a.size.cmp(&b.size).then(a.order.cmp(&b.order)));
    let candidate_sizes = candidates
        .iter()
        .map(|candidate| CandidateSize {
            structural: candidate.structural.clone(),
            layout: candidate.layout.clone(),
            size: candidate.size,
        })
        .collect::<Vec<_>>();
    let best = candidates.remove(0);
    if let Some(origins) = &best.origins {
        origins.validate_for_code(&best.code)?;
    }
    let lexical_target_met = context
        .lexical_target
        .is_some_and(|target| best.size <= target);
    Ok(CompileCodeResult {
        origins: best.origins,
        code: best.code,
        passes: best.passes,
        api_aliases: best.aliases,
        property_reads_hardcoded: context.property_reads_hardcoded,
        zero_cost_newlines: best.zero_cost_newlines,
        stats: SearchStats {
            candidates: candidate_count,
            attempted,
            parse_rejected,
            semantic_rejected,
            core_variants: context.core_variants,
            structural_variants: context.structural_variants,
            structural_explored: context.structural_names.len(),
            structural_names: context.structural_names,
            winner_structural: best.structural,
            winner_layout: best.layout,
            candidate_sizes,
            target_size: context.lexical_target,
            target_met: lexical_target_met,
            stopped_early: false,
            checkpoints: usize::from(context.lexical),
            stage: context.lexical.then(|| "lexical".into()),
        },
    })
}

pub fn select_best(
    mut context: SearchContext,
    mut batches: Vec<CandidateBatch>,
) -> Result<CompileCodeResult, String> {
    batches.append(&mut context.completed_batches);
    let attempted = batches.iter().map(|batch| batch.attempted).sum();
    let parse_rejected = batches.iter().map(|batch| batch.parse_rejected).sum();
    let semantic_rejected = batches.iter().map(|batch| batch.semantic_rejected).sum();
    let candidates = batches
        .into_iter()
        .flat_map(|batch| batch.candidates)
        .collect::<Vec<_>>();
    select_best_candidates(
        context,
        candidates,
        attempted,
        parse_rejected,
        semantic_rejected,
    )
}

fn target_char_size(code: &str) -> usize {
    code.encode_utf16().count()
}

fn render_variant_candidate(
    variant: &Variant,
    toggles: &PassToggles,
    zero_cost_newlines: bool,
    order: usize,
) -> Result<Candidate, String> {
    let (candidate_ast, candidate_root) = if pass_enabled(toggles, "scope-renaming") {
        let renamed = scope_rename_fast(&variant.ast, variant.root);
        (renamed.ast, renamed.root)
    } else {
        (variant.ast.clone(), variant.root)
    };
    let compact = Printer::new(&candidate_ast, false).output(candidate_root);
    if parse_source(&compact).is_err() {
        return Err("satisficing checkpoint failed reparse validation".into());
    }
    let size = target_char_size(&compact);
    let (code, newline_count, origins) =
        final_output(&candidate_ast, candidate_root, zero_cost_newlines);
    if target_char_size(&code) != size {
        return Err("zero-cost newline printer changed size".into());
    }
    Ok(Candidate {
        origins,
        structural: variant.name.clone(),
        layout: "none".into(),
        code,
        size,
        order,
        passes: variant.passes.clone(),
        aliases: Vec::new(),
        zero_cost_newlines: newline_count,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_satisficing_result(
    property_reads_hardcoded: usize,
    candidates: &[Candidate],
    attempted: usize,
    parse_rejected: usize,
    semantic_rejected: usize,
    structural_total: usize,
    structural_names: &[String],
    target_size: usize,
    target_met: bool,
    stopped_early: bool,
    checkpoints: usize,
    stage: &str,
) -> Result<CompileCodeResult, String> {
    let context = SearchContext {
        lexical: false,
        lexical_target: None,
        property_reads_hardcoded,
        core_variants: 1,
        structural_variants: structural_total,
        structural_names: structural_names.to_vec(),
        completed_batches: Vec::new(),
    };
    let mut result = select_best_candidates(
        context,
        candidates.to_vec(),
        attempted,
        parse_rejected,
        semantic_rejected,
    )?;
    result.stats.target_size = Some(target_size);
    result.stats.target_met = target_met;
    result.stats.stopped_early = stopped_early;
    result.stats.checkpoints = checkpoints;
    result.stats.stage = Some(stage.to_string());
    Ok(result)
}

fn target_is_met(candidates: &[Candidate], target_size: usize) -> bool {
    candidates
        .iter()
        .any(|candidate| target_char_size(&candidate.code) <= target_size)
}

/// Run deterministic target search. Source/base checkpoints may finish early;
/// all later work uses the same core and candidates as target-free search.
/// The optional checkpoint budget returns the best valid code seen so far.
/// No wall-clock measurements affect scheduling or candidate selection.
pub fn try_satisficing(
    source: &str,
    options: &CompileOptions,
    max_checkpoints: Option<usize>,
) -> Result<SatisficingAttempt, String> {
    options.validate_pass_toggles()?;
    let target_size = options
        .target_size
        .ok_or("try_satisficing requires target_size")?;
    let resolved_toggles = resolve_pass_toggles(&options.pass_toggles, options.numeric_mode);
    let (parsed_ast, parsed_root) = parse_for_search(source, options)?;
    storm_lua_analysis::environment_checks::validate(
        &parsed_ast,
        parsed_root,
        options.environment,
        &options.host_bindings,
    )?;
    if storm_lua_analysis::environment_checks::lexical_reason(
        &parsed_ast,
        options.environment,
        &options.host_bindings,
    )
    .is_some()
    {
        let (context, jobs) = prepare_lexical(source, parsed_ast, parsed_root, options)?;
        let batches = jobs
            .into_iter()
            .map(evaluate_candidate)
            .collect::<Result<Vec<_>, _>>()?;
        let result = select_best(context, batches)?;
        return Ok(SatisficingAttempt {
            status: if result.stats.target_met {
                SatisficingStatus::Satisfied
            } else {
                SatisficingStatus::Stopped
            },
            result,
            fallback: None,
        });
    }
    let (base_ast, base_root, property_reads_hardcoded) =
        passes::property_reads::transform_property_reads(
            &parsed_ast,
            parsed_root,
            options.property.as_ref(),
        );
    let initial_passes = if property_reads_hardcoded > 0 {
        vec![PassRecord {
            name: "property hardcoding".into(),
            saved: None,
            detail: Some(format!("{{\"replaced\":{property_reads_hardcoded}}}")),
            elapsed_ms: None,
        }]
    } else {
        Vec::new()
    };

    let mut explored = Vec::<Candidate>::new();
    let mut attempted = 0usize;
    let mut parse_rejected = 0usize;
    let mut semantic_rejected = 0usize;
    let mut checkpoints = 0usize;
    let mut structural_names = Vec::<String>::new();
    let mut structural_total = 0usize;

    macro_rules! checkpoint {
        ($stage:expr) => {{
            checkpoints += 1;
            let met = target_is_met(&explored, target_size);
            if met {
                let result = build_satisficing_result(
                    property_reads_hardcoded,
                    &explored,
                    attempted,
                    parse_rejected,
                    semantic_rejected,
                    structural_total,
                    &structural_names,
                    target_size,
                    true,
                    true,
                    checkpoints,
                    $stage,
                )?;
                return Ok(SatisficingAttempt {
                    status: SatisficingStatus::Satisfied,
                    result,
                    fallback: None,
                });
            }
            if max_checkpoints.is_some_and(|limit| checkpoints >= limit) {
                let result = build_satisficing_result(
                    property_reads_hardcoded,
                    &explored,
                    attempted,
                    parse_rejected,
                    semantic_rejected,
                    structural_total,
                    &structural_names,
                    target_size,
                    false,
                    true,
                    checkpoints,
                    $stage,
                )?;
                return Ok(SatisficingAttempt {
                    status: SatisficingStatus::Stopped,
                    result,
                    fallback: None,
                });
            }
        }};
    }

    // The source itself is the first anytime candidate when no property read had to
    // be hardcoded. If it already satisfies the target, OBJ-2 should perform no
    // optimization at all. Property hardcode mode instead emits the transformed
    // AST first so the requested option is still honored.
    if property_reads_hardcoded == 0 {
        explored.push(Candidate {
            origins: options
                .origin_source
                .as_ref()
                .map(|name| GeneratedOrigins::identity(name, source)),
            structural: "target:source".into(),
            layout: "source".into(),
            code: source.to_string(),
            size: target_char_size(source),
            order: 0,
            passes: Vec::new(),
            aliases: Vec::new(),
            zero_cost_newlines: 0,
        });
    } else {
        let property_variant = Variant {
            name: "target:property".into(),
            ast: base_ast.clone(),
            root: base_root,
            passes: initial_passes.clone(),
        };
        let mut unrenamed_toggles = resolved_toggles.clone();
        unrenamed_toggles.insert("scope-renaming", false);
        explored.push(render_variant_candidate(
            &property_variant,
            &unrenamed_toggles,
            options.zero_cost_newlines,
            0,
        )?);
    }
    attempted += 1;
    checkpoint!("source");

    // Next cheapest candidate: property transform + scope rename only.
    let base_variant = Variant {
        name: "target:base".into(),
        ast: base_ast.clone(),
        root: base_root,
        passes: initial_passes.clone(),
    };
    explored.push(render_variant_candidate(
        &base_variant,
        &resolved_toggles,
        options.zero_cost_newlines,
        1,
    )?);
    attempted += 1;
    checkpoint!("base");

    // Prepare the canonical core and deduplicated structural jobs exactly once.
    // Do not optimize separate cheap/balanced arenas and then restart from base:
    // unmet large drawing inputs otherwise pay for the expensive chains again.
    let (mut context, jobs) =
        prepare_search_from_base(options, base_ast, base_root, property_reads_hardcoded, true)?;
    structural_total = context.structural_variants;
    let mut ranked = jobs
        .into_iter()
        .map(|job| {
            let direct = render_variant_candidate(
                &job.variant,
                &resolved_toggles,
                options.zero_cost_newlines,
                job.order_base,
            )?;
            Ok((direct, job))
        })
        .collect::<Result<Vec<_>, String>>()?;
    // Rank direct candidates by their actual size, retaining canonical order on
    // ties. The job itself keeps its canonical order for full-search selection.
    ranked.sort_by(|(left, _), (right, _)| {
        left.size
            .cmp(&right.size)
            .then(left.order.cmp(&right.order))
    });
    let mut ranked = ranked.into_iter();
    for _ in 0..SATISFICING_MAX_CANDIDATE_JOBS {
        let Some((direct, job)) = ranked.next() else {
            break;
        };
        structural_names.push(job.variant.name.clone());
        explored.push(direct);
        attempted += 1;
        checkpoint!("structural");

        let batch = evaluate_candidate(job)?;
        attempted += batch.attempted;
        parse_rejected += batch.parse_rejected;
        semantic_rejected += batch.semantic_rejected;
        explored.extend(batch.candidates.iter().cloned());
        context.completed_batches.push(batch);
        if ranked.len() == 0 {
            // Finishing the last canonical job is completion, not an early
            // stop, even when it is also the first point reaching the target.
            checkpoints += 1;
            break;
        }
        checkpoint!("candidate");
    }
    let remaining_jobs = ranked.map(|(_, job)| job).collect::<Vec<_>>();
    if remaining_jobs.is_empty() {
        let result = finalize_satisficing_fallback(
            select_best(context, Vec::new())?,
            target_size,
            checkpoints,
        );
        return Ok(SatisficingAttempt {
            status: if result.stats.target_met {
                SatisficingStatus::Satisfied
            } else {
                SatisficingStatus::Stopped
            },
            result,
            fallback: None,
        });
    }
    let result = build_satisficing_result(
        property_reads_hardcoded,
        &explored,
        attempted,
        parse_rejected,
        semantic_rejected,
        structural_total,
        &structural_names,
        target_size,
        false,
        false,
        checkpoints,
        "canonical-prefix-complete",
    )?;
    Ok(SatisficingAttempt {
        status: SatisficingStatus::NeedsFullSearch,
        result,
        fallback: Some((context, remaining_jobs)),
    })
}

pub fn take_satisficing_fallback(
    attempt: &mut SatisficingAttempt,
) -> Option<(SearchContext, Vec<CandidateJob>)> {
    attempt.fallback.take()
}

/// Annotate a canonical full-search fallback as the terminal OBJ-2 result.
pub fn finalize_satisficing_fallback(
    mut result: CompileCodeResult,
    target_size: usize,
    prior_checkpoints: usize,
) -> CompileCodeResult {
    result.stats.target_size = Some(target_size);
    result.stats.target_met = target_char_size(&result.code) <= target_size;
    result.stats.stopped_early = false;
    result.stats.checkpoints += prior_checkpoints;
    if result.stats.winner_structural != "lexical" {
        result.stats.stage = Some("full-search".into());
    }
    result
}

pub fn encode_candidate_job(job: &CandidateJob) -> Result<Vec<u8>, String> {
    bincode::serialize(job).map_err(|error| format!("failed to encode candidate job: {error}"))
}

pub fn decode_and_evaluate_candidate_job(bytes: &[u8]) -> Result<CandidateBatch, String> {
    let job: CandidateJob = bincode::deserialize(bytes)
        .map_err(|error| format!("failed to decode candidate job: {error}"))?;
    evaluate_candidate(job)
}

pub fn encode_search_context(context: &SearchContext) -> Result<Vec<u8>, String> {
    bincode::serialize(context).map_err(|error| format!("failed to encode search context: {error}"))
}

pub fn decode_search_context(bytes: &[u8]) -> Result<SearchContext, String> {
    bincode::deserialize(bytes).map_err(|error| format!("failed to decode search context: {error}"))
}

fn compile_code_obj1(source: &str, options: &CompileOptions) -> Result<CompileCodeResult, String> {
    let (context, jobs) = prepare_search(source, options)?;
    let batches = jobs
        .into_iter()
        .map(evaluate_candidate)
        .collect::<Result<Vec<_>, _>>()?;
    select_best(context, batches)
}

/// Compile without an external semantic verifier. `target_size` activates the
/// OBJ-2 checkpoints within canonical search; absence of a target preserves OBJ-1.
pub fn compile_code(source: &str, options: &CompileOptions) -> Result<CompileCodeResult, String> {
    let Some(target_size) = options.target_size else {
        return compile_code_obj1(source, options);
    };
    let mut attempt = try_satisficing(source, options, None)?;
    match attempt.status {
        SatisficingStatus::Satisfied | SatisficingStatus::Stopped => Ok(attempt.result),
        SatisficingStatus::NeedsFullSearch => {
            let prior_checkpoints = attempt.result.stats.checkpoints;
            let (context, jobs) = take_satisficing_fallback(&mut attempt)
                .ok_or("satisficing fallback preparation missing")?;
            let batches = jobs
                .into_iter()
                .map(evaluate_candidate)
                .collect::<Result<Vec<_>, _>>()?;
            let result = select_best(context, batches)?;
            Ok(finalize_satisficing_fallback(
                result,
                target_size,
                prior_checkpoints,
            ))
        }
    }
}

/// Native/Node candidate verification hook. Verification remains sequential so
/// callback invocation order stays compatible with the TS oracle.
pub fn compile_code_with_verifier(
    source: &str,
    options: &CompileOptions,
    mut verify_candidate: Option<&mut dyn FnMut(&str) -> bool>,
) -> Result<CompileCodeResult, String> {
    // FnMut verifiers may keep state: preserve every historical invocation,
    // including identical candidates. Only the no-verifier path deduplicates.
    let (context, jobs) = prepare_search_with_dedup(source, options, verify_candidate.is_none())?;
    let mut batches = Vec::with_capacity(jobs.len());
    for job in jobs {
        batches.push(evaluate_candidate_with_verifier(
            job,
            &mut verify_candidate,
        )?);
    }
    let result = select_best(context, batches)?;
    Ok(if let Some(target_size) = options.target_size {
        finalize_satisficing_fallback(result, target_size, 0)
    } else {
        result
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod phase7_serialization_tests {
    use super::*;

    #[test]
    fn candidate_size_uses_the_public_utf16_billing_unit() {
        assert_eq!(target_char_size("aあ😀"), 4);
        assert_ne!(target_char_size("aあ😀"), "aあ😀".len());
    }

    #[test]
    fn satisficing_checkpoint_budget_returns_best_so_far() {
        let source = "function onTick()output.setNumber(1,1+2)end";
        let options = CompileOptions {
            target_size: Some(0),
            ..CompileOptions::default()
        };
        let attempt = try_satisficing(source, &options, Some(1)).expect("target attempt");
        assert_eq!(attempt.status, SatisficingStatus::Stopped);
        assert!(!attempt.result.stats.target_met);
        assert!(attempt.result.stats.stopped_early);
        assert_eq!(attempt.result.stats.checkpoints, 1);
        assert_eq!(attempt.result.stats.stage.as_deref(), Some("source"));
        assert_eq!(attempt.result.stats.candidates, 1);
        assert_eq!(attempt.result.code, source);
        parse_source(&attempt.result.code).expect("best-so-far output reparses");
    }

    #[test]
    fn satisficing_returns_source_when_input_already_meets_target() {
        let source = "function onTick()end\n";
        let options = CompileOptions {
            target_size: Some(source.encode_utf16().count()),
            ..CompileOptions::default()
        };
        let result = compile_code(source, &options).expect("already satisfied");
        assert_eq!(result.code, source);
        assert!(result.stats.target_met);
        assert!(result.stats.stopped_early);
        assert_eq!(result.stats.stage.as_deref(), Some("source"));
        assert_eq!(result.stats.checkpoints, 1);
    }

    #[test]
    fn satisficing_reaches_small_target_without_full_search() {
        let source = "function onTick()output.setNumber(1,1+2)end";
        let options = CompileOptions {
            target_size: Some(41),
            ..CompileOptions::default()
        };
        let result = compile_code(source, &options).expect("target compile");
        assert_eq!(result.code, "function onTick()output.setNumber(1,3)end");
        assert!(result.stats.target_met);
        assert!(result.stats.stopped_early);
        assert_ne!(result.stats.stage.as_deref(), Some("full-search"));
    }

    #[test]
    fn satisficing_full_fallback_reports_unmet_target() {
        let source = "function onTick()output.setNumber(1,1+2)end";
        let options = CompileOptions {
            target_size: Some(0),
            ..CompileOptions::default()
        };
        let result = compile_code(source, &options).expect("full fallback");
        assert!(!result.stats.target_met);
        assert!(!result.stats.stopped_early);
        assert_eq!(result.stats.stage.as_deref(), Some("full-search"));
        assert!(result.stats.checkpoints >= 1);
        assert_eq!(result.code, "function onTick()output.setNumber(1,3)end");
    }

    const RESUME_SOURCE: &str = "local total=0
        function onTick()
          local x=input.getNumber(1) local y=input.getNumber(2)
          total=total+x+y output.setNumber(1,total+x+x+y)
        end
        function onDraw()
          local w=screen.getWidth()
          screen.drawLine(0,0,w,w) screen.drawText(1,1,total)
        end";

    #[test]
    fn missed_target_resumes_canonical_jobs_without_repeating_evaluated_prefix() {
        let options = CompileOptions {
            target_size: Some(0),
            ..Default::default()
        };
        let (canonical_context, canonical_jobs) = prepare_search(RESUME_SOURCE, &options).unwrap();
        assert!(
            canonical_jobs.len() > SATISFICING_MAX_CANDIDATE_JOBS,
            "fixture must actually exercise a remaining Worker batch"
        );
        let expected = select_best(
            canonical_context,
            canonical_jobs
                .clone()
                .into_iter()
                .map(|job| evaluate_candidate(job).unwrap())
                .collect(),
        )
        .unwrap();
        let mut attempt = try_satisficing(RESUME_SOURCE, &options, None).unwrap();
        assert_eq!(attempt.status, SatisficingStatus::NeedsFullSearch);
        let (context, remaining) = take_satisficing_fallback(&mut attempt).unwrap();
        assert_eq!(
            context.completed_batches.len(),
            SATISFICING_MAX_CANDIDATE_JOBS
        );
        assert_eq!(
            context.completed_batches.len() + remaining.len(),
            canonical_jobs.len()
        );
        let completed_orders = context
            .completed_batches
            .iter()
            .flat_map(|batch| batch.candidates.iter().map(|candidate| candidate.order))
            .collect::<std::collections::HashSet<_>>();
        let mut remaining_batches = Vec::new();
        for job in remaining.into_iter().rev() {
            let batch =
                decode_and_evaluate_candidate_job(&encode_candidate_job(&job).unwrap()).unwrap();
            assert!(batch
                .candidates
                .iter()
                .all(|c| !completed_orders.contains(&c.order)));
            remaining_batches.push(batch);
        }
        // Completed candidates survive context serialization to another worker.
        let decoded = decode_search_context(&encode_search_context(&context).unwrap()).unwrap();
        assert_eq!(
            decoded.completed_batches.len(),
            context.completed_batches.len()
        );
        let actual = select_best(decoded, remaining_batches).unwrap();
        assert_eq!(actual.code, expected.code);
        assert_eq!(actual.stats, expected.stats);
        assert_eq!(actual.api_aliases, expected.api_aliases);
    }

    #[test]
    fn target_miss_preserves_full_search_for_modes_and_property_specialization() {
        use crate::config::{PropertyConfig, PropertyMode};
        for numeric_mode in [NumericMode::Exact, NumericMode::Tolerant] {
            for mode in [CompileMode::Safe, CompileMode::Smallest] {
                for search_mode in [SearchMode::Fast, SearchMode::Exhaustive] {
                    let options = CompileOptions {
                        numeric_mode,
                        mode,
                        search_mode,
                        zero_cost_newlines: false,
                        property: Some(PropertyConfig {
                            mode: PropertyMode::Hardcode,
                            numbers: Some(std::collections::BTreeMap::from([("gain".into(), 1.5)])),
                            bools: None,
                            texts: None,
                        }),
                        ..Default::default()
                    };
                    let source = format!("gain=property.getNumber('gain')\n{RESUME_SOURCE}");
                    let baseline = compile_code(
                        &source,
                        &CompileOptions {
                            search_mode: SearchMode::Exhaustive,
                            ..options.clone()
                        },
                    )
                    .unwrap();
                    let target = compile_code(
                        &source,
                        &CompileOptions {
                            target_size: Some(0),
                            ..options
                        },
                    )
                    .unwrap();
                    assert_eq!(
                        target.code, baseline.code,
                        "{numeric_mode:?}/{mode:?}/{search_mode:?}"
                    );
                    assert_eq!(target.stats.candidate_sizes, baseline.stats.candidate_sizes);
                    assert_eq!(target.stats.attempted, baseline.stats.attempted);
                    assert_eq!(
                        target.property_reads_hardcoded,
                        baseline.property_reads_hardcoded
                    );
                    assert!(!target.stats.target_met);
                    assert!(!target.stats.stopped_early);
                }
            }
        }
    }

    #[test]
    fn unmet_fast_target_retains_the_exhaustive_candidate_set() {
        let maximum = CompileOptions::default();
        let (_, canonical) = prepare_search(RESUME_SOURCE, &maximum).unwrap();
        assert!(canonical.len() > 2);
        let fast = CompileOptions {
            search_mode: SearchMode::Fast,
            search_beam_width: 1,
            ..maximum.clone()
        };
        assert_eq!(prepare_search(RESUME_SOURCE, &fast).unwrap().1.len(), 1);
        let expected = compile_code(RESUME_SOURCE, &maximum).unwrap();
        for beam in [1, 4, 16] {
            for limit in [0, target_char_size(&expected.code) - 1] {
                let options = CompileOptions {
                    target_size: Some(limit),
                    search_beam_width: beam,
                    ..fast.clone()
                };
                let actual = compile_code(RESUME_SOURCE, &options).unwrap();
                assert_eq!(actual.code, expected.code);
                assert_eq!(actual.stats.candidate_sizes, expected.stats.candidate_sizes);
                assert_eq!(actual.stats.attempted, expected.stats.attempted);
                assert!(!actual.stats.target_met);
                assert!(!actual.stats.stopped_early);
            }
        }
    }

    #[test]
    fn completed_single_job_target_miss_does_not_export_work_again() {
        let source = "function onDraw()screen.drawRectF(1,2,3,4)end";
        let options = CompileOptions {
            target_size: Some(0),
            ..Default::default()
        };
        let mut attempt = try_satisficing(source, &options, None).unwrap();
        assert_eq!(attempt.status, SatisficingStatus::Stopped);
        assert!(take_satisficing_fallback(&mut attempt).is_none());
        let baseline = compile_code(source, &CompileOptions::default()).unwrap();
        assert_eq!(attempt.result.code, baseline.code);
        assert_eq!(attempt.result.stats.attempted, baseline.stats.attempted);
        assert_eq!(attempt.result.stats.stage.as_deref(), Some("full-search"));
        assert!(!attempt.result.stats.stopped_early);
    }

    #[test]
    fn target_source_checkpoint_reports_utf16_and_honors_property_transform() {
        let source = "-- 日本語😀\nfunction onTick()end";
        let options = CompileOptions {
            target_size: Some(source.encode_utf16().count()),
            ..Default::default()
        };
        let result = compile_code(source, &options).unwrap();
        assert_eq!(
            result.stats.candidate_sizes[0].size,
            source.encode_utf16().count()
        );
        assert_eq!(result.code, source);
        assert!(result.stats.target_met);
    }

    #[test]
    fn target_reached_by_the_last_canonical_job_is_not_an_early_stop() {
        let body = (0..64)
            .map(|i| format!("screen.drawRectF({},{},3,4)", i % 32, i / 32))
            .collect::<Vec<_>>()
            .join("\n");
        let source = format!("function onDraw()\n{body}\nend");
        let options = CompileOptions::default();
        let expected = compile_code(&source, &options).unwrap();
        let target = target_char_size(&expected.code);
        let (_, jobs) = prepare_search(&source, &options).unwrap();
        assert_eq!(jobs.len(), 1);
        let toggles = resolve_pass_toggles(&options.pass_toggles, options.numeric_mode);
        let direct = render_variant_candidate(&jobs[0].variant, &toggles, true, 0).unwrap();
        assert!(
            target_char_size(&direct.code) > target,
            "fixture must need the final candidate chain"
        );
        let actual = compile_code(
            &source,
            &CompileOptions {
                target_size: Some(target),
                ..options
            },
        )
        .unwrap();
        assert_eq!(actual.code, expected.code);
        assert_eq!(actual.stats.candidate_sizes, expected.stats.candidate_sizes);
        assert!(actual.stats.target_met);
        assert!(!actual.stats.stopped_early);
        assert_eq!(actual.stats.stage.as_deref(), Some("full-search"));
    }

    #[test]
    fn candidate_job_binary_roundtrip_preserves_result() {
        let source = "function onTick()local a=input.getNumber(1)output.setNumber(1,a+1)end";
        let options = CompileOptions::default();
        let (context, jobs) = prepare_search(source, &options).expect("prepare");
        assert!(!jobs.is_empty());
        let direct = evaluate_candidate(jobs[0].clone()).expect("direct");
        let bytes = encode_candidate_job(&jobs[0]).expect("encode job");
        let roundtrip = decode_and_evaluate_candidate_job(&bytes).expect("roundtrip");
        assert_eq!(direct.candidates.len(), roundtrip.candidates.len());
        assert_eq!(direct.attempted, roundtrip.attempted);
        assert_eq!(direct.parse_rejected, roundtrip.parse_rejected);
        for (left, right) in direct.candidates.iter().zip(roundtrip.candidates.iter()) {
            assert_eq!(left.code, right.code);
            assert_eq!(left.size, right.size);
            assert_eq!(left.order, right.order);
            assert_eq!(left.structural, right.structural);
            assert_eq!(left.layout, right.layout);
        }
        let context_bytes = encode_search_context(&context).expect("encode context");
        let decoded = decode_search_context(&context_bytes).expect("decode context");
        assert_eq!(decoded.structural_names, context.structural_names);
    }
}
