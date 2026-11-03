//! Phase 5b: whole-program structural/candidate search parity with TS `compileSource`.
//! Public packaging/report metadata belongs to Phase 6; this module owns the
//! candidate graph, beam pruning, reparse validation, and deterministic winner.

use crate::config::{
    pass_enabled, resolve_pass_toggles, CompileMode, CompileOptions, NumericMode, NumericTolerance,
    PassRecord, SearchMode,
};
use crate::orchestration::core_optimize;
use crate::pass_ids::{pass_index, PassToggles, OPTIMIZATION_PASS_IDS};
use crate::passes;
use crate::scope_rename::scope_rename_fast;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use storm_lua_syntax::ast::{Ast, NodeId};
use storm_lua_syntax::parser::parse_source;
use storm_lua_syntax::print::{PrintedSource, Printer};
use storm_lua_syntax::provenance::{
    DeferredGeneratedOrigins, GeneratedOrigins, LexicalCopiedOrigins,
};
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
    origins: Option<Arc<DeferredGeneratedOrigins>>,
    lexical_origins: Option<Arc<LexicalCopiedOrigins>>,
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
    #[doc(hidden)]
    pub lexical_origins: Option<Arc<LexicalCopiedOrigins>>,
    pub code: String,
    pub stats: SearchStats,
    pub passes: Vec<PassRecord>,
    pub api_aliases: Vec<String>,
    pub property_reads_hardcoded: usize,
    pub zero_cost_newlines: u32,
}

impl CompileCodeResult {
    fn materialize_lexical_origins(mut self) -> Self {
        if self.origins.is_none() {
            if let Some(lexical) = self.lexical_origins.take() {
                let lexical = match Arc::try_unwrap(lexical) {
                    Ok(value) => value,
                    Err(shared) => shared.as_ref().clone(),
                };
                self.origins = Some(lexical.into_generated());
            }
        } else {
            self.lexical_origins = None;
        }
        self
    }
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
        crate::scope_rename::measure_renamed_size(ast, root)
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
        let Some(index) = pass_index(id) else {
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
    lexical_origins: Option<LexicalCopiedOrigins>,
    lexical_source: Option<String>,
    variant: Variant,
    aggressive: bool,
    toggles: PassMask,
    numeric_tolerance: Option<NumericTolerance>,
    exact_safe_constant_folding: bool,
    zero_cost_newlines: bool,
    order_base: usize,
    /// True only for target-size compilation. Target-free compilation must
    /// never see the speed layouts, so its winner is unchanged.
    target_speed: bool,
    /// Target character budget for target-size compilation, if any. Used
    /// only for winner-preserving layout pruning in
    /// evaluate_candidate_with_verifier; target-free jobs carry None.
    target_size: Option<usize>,
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
    // These options select lexical-only compilation independently of the AST.
    // Keep the normal parse/validation but skip unused source-range collection.
    if options.environment == storm_lua_spec::environment::EnvironmentProfile::Extended
        || !options.host_bindings.is_empty()
    {
        return parse_source(source).map_err(|error| error.to_string());
    }
    if let Some(name) = &options.origin_source {
        storm_lua_syntax::parser::parse_source_with_conditional_origins(name, source, |ast| {
            storm_lua_analysis::environment_checks::lexical_reason(
                ast,
                options.environment,
                &options.host_bindings,
            )
            .is_none()
        })
    } else {
        parse_source(source).map_err(|error| error.to_string())
    }
}

enum FinalPrint {
    Plain {
        code: String,
        newline_count: u32,
    },
    Tracked {
        printed: PrintedSource,
        newline_count: u32,
    },
}

impl FinalPrint {
    fn code(&self) -> &str {
        match self {
            Self::Plain { code, .. } => code,
            Self::Tracked { printed, .. } => &printed.code,
        }
    }

    fn finish(self, ast: &Ast) -> (String, u32, Option<DeferredGeneratedOrigins>) {
        match self {
            Self::Plain {
                code,
                newline_count,
            } => (code, newline_count, None),
            Self::Tracked {
                printed,
                newline_count,
            } => {
                let code = printed.code.clone();
                let origins = DeferredGeneratedOrigins::from_print(ast, printed);
                (code, newline_count, origins)
            }
        }
    }
}

fn print_final(ast: &Ast, root: NodeId, zero: bool) -> FinalPrint {
    let mut printer = Printer::new(ast, zero);
    if ast.nodes.tracks_origins() {
        let printed = printer.output_with_positions(root);
        FinalPrint::Tracked {
            printed,
            newline_count: printer.newline_count(),
        }
    } else {
        let code = printer.output(root);
        FinalPrint::Plain {
            code,
            newline_count: printer.newline_count(),
        }
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
    // Local-only renaming applies only to the _ENV lexical reason: the
    // other lexical reasons (host bindings, Extended environment) exist
    // precisely to preserve tokens, so they keep the unrenamed output.
    // Globals stay fixed (they can be observed through _ENV); only
    // locals are renamed, at the token level, with a re-lex verification
    // inside the renamed emitter and a fallback to the unrenamed output.
    let rename_plan = if options.environment
        != storm_lua_spec::environment::EnvironmentProfile::Extended
        && options.host_bindings.is_empty()
        && ast.strings.contains("_ENV")
    {
        crate::lexical_rename::lexical_rename_plan(source).filter(|plan| !plan.is_empty())
    } else {
        None
    };
    let (code, lexical_origins) = if let Some(name) = &options.origin_source {
        let (code, origins) = match &rename_plan {
            Some(plan) => storm_lua_syntax::print::lexical_minify_renamed_with_compact_origins(
                name, source, plan,
            )
            .map_err(|error| error.to_string())?,
            None => storm_lua_syntax::print::lexical_minify_with_compact_origins(name, source)
                .map_err(|error| error.to_string())?,
        };
        (code, Some(origins))
    } else {
        let code = match &rename_plan {
            Some(plan) => storm_lua_syntax::print::lexical_minify_renamed(source, plan)
                .map_err(|error| error.to_string())?,
            None => storm_lua_syntax::print::lexical_minify(source)
                .map_err(|error| error.to_string())?,
        };
        (code, None)
    };
    // `parse_for_search` already accepted the original program. The lexical
    // emitter then re-lexes its output and requires the exact same token kind,
    // spelling and source line sequence before returning it. Lua parsing is a
    // function of that token stream; building and immediately discarding a
    // second AST here cannot discover a new syntax failure. Avoid that complete
    // parser/arena pass on the conservative route.
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
            target_speed: false,
            target_size: None,
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
    prepare_search_from_parsed(source, options, deduplicate, parsed_ast, parsed_root)
}

fn prepare_search_from_parsed(
    source: &str,
    options: &CompileOptions,
    deduplicate: bool,
    parsed_ast: Ast,
    parsed_root: NodeId,
) -> Result<(SearchContext, Vec<CandidateJob>), String> {
    storm_lua_analysis::environment_checks::validate(
        &parsed_ast,
        parsed_root,
        options.environment,
        &options.host_bindings,
    )?;
    // Load legality is judged on the source program, before any pass can
    // remove or move the constructs it depends on (candidates S10/S14).
    // The verdict mirrors the project VM's load path (dialect rewrite +
    // PUC parse), so programs the VM accepts pass unchanged.
    storm_lua_vm::load_legality::check_load_legality(source.as_bytes())?;
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
    // Extra layouts exist only in target-size mode: the localized
    // alias layout and, on top of it, the LICM layout, each with a full
    // S1 variant, a first-group-only partial S1 variant, an MR
    // variant, an SMC variant, a first-group-only partial SMC
    // variant, composite S1+SMC / MR+SMC variants (SMC on top of
    // the S1 / MR layout, offered only when both passes fire), and
    // partial composite variants (full S1 / MR with first-group-only
    // SMC, offered only when the full composite cached more than
    // one SMC group).
    // Order bases
    // must stay disjoint across jobs even when a job produces no LICM
    // layout.
    let layouts_per_job = if job_toggles.enabled("api-alias-optimization") {
        if options.target_size.is_some() {
            20
        } else {
            2
        }
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
            target_speed: options.target_size.is_some(),
            target_size: options.target_size,
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
    mut job: CandidateJob,
    verify_candidate: &mut Option<&mut dyn FnMut(&str) -> bool>,
) -> Result<CandidateBatch, String> {
    if let Some(code) = job.lexical_source.take() {
        if let Some(verify) = verify_candidate.as_deref_mut() {
            if !verify(&code) {
                return Ok(CandidateBatch {
                    candidates: Vec::new(),
                    attempted: 1,
                    parse_rejected: 0,
                    semantic_rejected: 1,
                });
            }
        }
        // This job is consumed, not shared. Retain the already completed
        // lexical code and mapping vectors rather than cloning both payloads.
        let size = target_char_size(&code);
        return Ok(CandidateBatch {
            candidates: vec![Candidate {
                origins: None,
                lexical_origins: job.lexical_origins.map(Arc::new),
                structural: "lexical".into(),
                layout: "tokens".into(),
                code,
                size,
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
        target_speed,
        target_size,
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
    if toggles.enabled("redundant-output-store-elimination") {
        let r = passes::output_stores::eliminate_redundant_output_stores(&mut ast, root);
        root = r.root;
    }
    if toggles.enabled("negated-comparison-inversion") {
        let r = passes::negated_comparison::invert_negated_comparisons(&mut ast, root);
        root = r.root;
    }
    if toggles.enabled("straightline-arithmetic-cse") {
        let r = passes::arithmetic_cse::cse_straightline_arithmetic(&mut ast, root);
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
    if toggles.enabled("pure-call-carrier-synthesis") {
        let r = passes::available_expressions::synthesize_pure_call_carriers(&mut ast, root);
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
    // Target-size only: the joint-api layout and the localized layout
    // run the alias pass over the same phase tree (the localized base is
    // the phase tree unless relaxed carrier synthesis replaces it
    // below), so the pass's first analysis — origin-less clone,
    // resolution, effect analysis, builtin-use scan — is built once
    // here and shared by both calls. The analysis is a deterministic
    // pure function of the source tree and root, and a deep clone
    // carries identical node/symbol tables, so the shared values equal
    // what each call would compute on its own; no selection or output
    // changes (see passes::api_aliases::ApiAliasAnalysis).
    let alias_shared_source = if target_speed && toggles.enabled("api-alias-optimization") {
        Some(storm_lua_syntax::ast_utils::clone_without_origins(
            &phase_ast,
        ))
    } else {
        None
    };
    let alias_shared_resolution = alias_shared_source
        .as_ref()
        .map(|source| storm_lua_analysis::resolver::resolve(source, phase_root));
    let alias_analysis = match (&alias_shared_source, &alias_shared_resolution) {
        (Some(source), Some(resolution)) => Some(passes::api_aliases::ApiAliasAnalysis::new(
            source, resolution, phase_root,
        )),
        _ => None,
    };
    if toggles.enabled("scope-renaming") {
        let renamed = scope_rename_fast(&phase_ast, phase_root);
        layouts.push(("none", renamed.ast, renamed.root, Vec::new()));
    } else {
        layouts.push(("none", phase_ast.clone(), phase_root, Vec::new()));
    }
    if toggles.enabled("api-alias-optimization") {
        let mut alias_ast = phase_ast.clone();
        let alias = match &alias_analysis {
            Some(analysis) => passes::api_aliases::optimize_api_aliases_with_analysis(
                &mut alias_ast,
                phase_root,
                analysis,
            ),
            None => passes::api_aliases::optimize_api_aliases(&mut alias_ast, phase_root),
        };
        let aliases = alias.details.clone().unwrap_or_default();
        layouts.push(("joint-api", alias_ast, alias.root, aliases));
    }
    // Target-free only: barrier-relaxed carrier synthesis as *additional*
    // layout candidates, not as a phase replacement. Cycles 42/44 showed
    // that relaxing the strict synthesis barrier in place perturbs
    // downstream alias selection and moves the size-minimal winner
    // (+14 characters on two corpus files, -6 on one). The scratch clone
    // is adopted only when synthesis fires, and target-free selection is
    // plain size minimization (see select_best_candidates), so the new
    // candidates can only win when they are smaller; the previous layouts
    // stay available and the winner size never regresses. Ties keep the
    // earlier layout (the new layouts are pushed last), so same-size
    // speed wins are deliberately left for a later decision. In
    // target-size mode this block is skipped: the target layouts already
    // run the relaxed-barrier synthesis on their scratch clones.
    if !target_speed && toggles.enabled("pure-call-carrier-synthesis") {
        let mut relaxed_ast = phase_ast.clone();
        let relaxed = passes::available_expressions::synthesize_pure_call_carriers_barrier_relaxed(
            &mut relaxed_ast,
            phase_root,
        );
        let relaxed_count = relaxed
            .details
            .as_ref()
            .and_then(|details| details.first())
            .and_then(|detail| detail.strip_prefix("synthesized="))
            .and_then(|count| count.parse::<usize>().ok())
            .unwrap_or(0);
        if relaxed_count > 0 {
            if toggles.enabled("scope-renaming") {
                let renamed = scope_rename_fast(&relaxed_ast, relaxed.root);
                layouts.push((
                    "none-barrier-relaxed",
                    renamed.ast,
                    renamed.root,
                    Vec::new(),
                ));
            } else {
                layouts.push((
                    "none-barrier-relaxed",
                    relaxed_ast.clone(),
                    relaxed.root,
                    Vec::new(),
                ));
            }
            if toggles.enabled("api-alias-optimization") {
                let mut alias_ast = relaxed_ast.clone();
                let alias = passes::api_aliases::optimize_api_aliases(&mut alias_ast, relaxed.root);
                let aliases = alias.details.clone().unwrap_or_default();
                layouts.push(("joint-api-barrier-relaxed", alias_ast, alias.root, aliases));
            }
        }
    }
    // Target-size only: the same alias selection, rerun under local costs,
    // with definitions emitted as chunk locals. This layout is longer than
    // the global one; it can only win through target-size winner selection
    // (see select_best_candidates), never through size minimization.
    // Before aliasing, relaxed carrier synthesis runs on a scratch clone:
    // groups the strict gate rejected (positive character delta) are
    // synthesized here, because within the target budget speed outranks
    // size. Barrier-stable reuse and relaxed-barrier synthesis run on
    // the same scratch clone, so barrier-split screen-size / literal-math
    // duplicates also collapse here without touching the target-free
    // winner. The scratch clone is adopted only when either step fires,
    // so inputs without an opportunity keep exactly the previous layout.
    if target_speed && toggles.enabled("api-alias-optimization") {
        let mut alias_ast = phase_ast.clone();
        let mut alias_root = phase_root;
        let mut alias_base_shared = alias_analysis.is_some();
        if toggles.enabled("pure-call-carrier-synthesis") {
            let mut relaxed_ast = phase_ast.clone();
            let mut reused_count = 0;
            if toggles.enabled("available-expression-reuse") {
                let reuse =
                    passes::available_expressions::reuse_available_expressions_barrier_relaxed(
                        &mut relaxed_ast,
                        phase_root,
                    );
                reused_count = reuse
                    .details
                    .as_ref()
                    .and_then(|details| details.first())
                    .and_then(|detail| detail.strip_prefix("reused="))
                    .and_then(|count| count.parse::<usize>().ok())
                    .unwrap_or(0);
            }
            let relaxed =
                passes::available_expressions::synthesize_pure_call_carriers_relaxed_barrier(
                    &mut relaxed_ast,
                    phase_root,
                );
            let relaxed_count = relaxed
                .details
                .as_ref()
                .and_then(|details| details.first())
                .and_then(|detail| detail.strip_prefix("synthesized="))
                .and_then(|count| count.parse::<usize>().ok())
                .unwrap_or(0);
            if reused_count > 0 || relaxed_count > 0 {
                alias_ast = relaxed_ast;
                alias_root = relaxed.root;
                alias_base_shared = false;
            }
        }
        let alias = match &alias_analysis {
            Some(analysis) if alias_base_shared => {
                passes::api_aliases::optimize_api_aliases_localized_with_analysis(
                    &mut alias_ast,
                    alias_root,
                    analysis,
                )
            }
            _ => passes::api_aliases::optimize_api_aliases_localized(&mut alias_ast, alias_root),
        };
        let aliases = alias.details.clone().unwrap_or_default();
        // Shared first analyses for the speed passes on this base
        // (S1 / MR / SMC each used to rebuild the same resolve +
        // EffectAnalyzer + mutated set; see passes::speed_analysis).
        // The analyses are pure and side-effect free, and every pass
        // still applies its own non-Block early return, so building
        // them here changes no pass result.
        let speed_resolution = storm_lua_analysis::resolver::resolve(&alias_ast, alias.root);
        let speed_effects = storm_lua_analysis::effects::EffectAnalyzer::new(
            &alias_ast,
            &speed_resolution,
            alias.root,
            true,
        );
        let speed_mutated = storm_lua_analysis::effects::mutated_builtin_roots(
            &alias_ast,
            &speed_resolution,
            alias.root,
        );
        let speed_analysis = passes::speed_analysis::SpeedAnalysis {
            resolution: &speed_resolution,
            effects: &speed_effects,
            mutated: &speed_mutated,
        };
        // Target-size only: single member-call caching on top of the
        // localized layout (see passes::single_member_cache). The
        // scratch clone is adopted only when caching fires, so inputs
        // without an S1 opportunity keep exactly the previous layout.
        let mut s1_ast = alias_ast.clone();
        let s1 = passes::single_member_cache::cache_single_member_calls_post_alias_with_analysis(
            &mut s1_ast,
            alias.root,
            &speed_analysis,
        );
        let s1_cached = s1
            .details
            .as_ref()
            .and_then(|details| details.first())
            .and_then(|detail| detail.strip_prefix("cached="))
            .and_then(|count| count.parse::<usize>().ok())
            .unwrap_or(0);
        let s1_aliases = aliases.clone();
        let s1_partial_aliases = s1_aliases.clone();
        layouts.push(("joint-api-local", alias_ast.clone(), alias.root, aliases));
        if s1_cached > 0 {
            let mut s1_smc_ast = s1_ast.clone();
            let s1_smc = passes::straightline_member_reads::cache_straightline_member_reads(
                &mut s1_smc_ast,
                alias.root,
            );
            let s1_smc_cached = s1_smc
                .details
                .as_ref()
                .and_then(|details| details.first())
                .and_then(|detail| detail.strip_prefix("cached="))
                .and_then(|count| count.parse::<usize>().ok())
                .unwrap_or(0);
            if s1_smc_cached > 0 {
                layouts.push((
                    "joint-api-local-s1-smc",
                    s1_smc_ast,
                    alias.root,
                    s1_aliases.clone(),
                ));
            }
            // Partial composite: S1 in full, SMC first-group-only.
            // Offered only when the full composite cached more than
            // one SMC group, so it differs from the full composite.
            // It ranks directly below the full composite, so it can
            // only win when the full composite exceeds the target
            // while this layout still fits.
            if s1_smc_cached > 1 {
                let mut s1_smc_partial_ast = s1_ast.clone();
                let s1_smc_partial =
                    passes::straightline_member_reads::cache_straightline_member_reads_partial(
                        &mut s1_smc_partial_ast,
                        alias.root,
                    );
                let s1_smc_partial_cached = s1_smc_partial
                    .details
                    .as_ref()
                    .and_then(|details| details.first())
                    .and_then(|detail| detail.strip_prefix("cached="))
                    .and_then(|count| count.parse::<usize>().ok())
                    .unwrap_or(0);
                if s1_smc_partial_cached > 0 {
                    layouts.push((
                        "joint-api-local-s1-smc-partial",
                        s1_smc_partial_ast,
                        alias.root,
                        s1_aliases.clone(),
                    ));
                }
            }
            layouts.push(("joint-api-local-s1", s1_ast, alias.root, s1_aliases));
        }
        // Partial S1: only when the full run cached more than one
        // group, so this layout differs from the full one. It ranks
        // below the full S1 layout but above the base layout, so it
        // can only win when the full layout exceeds the target while
        // the single-group layout still fits.
        if s1_cached > 1 {
            let mut s1_partial_ast = alias_ast.clone();
            let s1_partial = passes::single_member_cache::cache_single_member_calls_post_alias_partial_with_analysis(
                    &mut s1_partial_ast,
                    alias.root,
                    &speed_analysis,
                );
            let s1_partial_cached = s1_partial
                .details
                .as_ref()
                .and_then(|details| details.first())
                .and_then(|detail| detail.strip_prefix("cached="))
                .and_then(|count| count.parse::<usize>().ok())
                .unwrap_or(0);
            if s1_partial_cached > 0 {
                layouts.push((
                    "joint-api-local-s1-partial",
                    s1_partial_ast,
                    alias.root,
                    s1_partial_aliases,
                ));
            }
        }
        // Target-size only: loop member-read caching on top of the
        // localized layout (see passes::loop_member_reads). Adopted
        // only when caching fires, as with S1.
        let mut mr_ast = alias_ast.clone();
        let mr = passes::loop_member_reads::cache_loop_member_reads_with_analysis(
            &mut mr_ast,
            alias.root,
            &speed_analysis,
        );
        let mr_cached = mr
            .details
            .as_ref()
            .and_then(|details| details.first())
            .and_then(|detail| detail.strip_prefix("cached="))
            .and_then(|count| count.parse::<usize>().ok())
            .unwrap_or(0);
        if mr_cached > 0 {
            let mut mr_smc_ast = mr_ast.clone();
            let mr_smc = passes::straightline_member_reads::cache_straightline_member_reads(
                &mut mr_smc_ast,
                alias.root,
            );
            let mr_smc_cached = mr_smc
                .details
                .as_ref()
                .and_then(|details| details.first())
                .and_then(|detail| detail.strip_prefix("cached="))
                .and_then(|count| count.parse::<usize>().ok())
                .unwrap_or(0);
            if mr_smc_cached > 0 {
                layouts.push((
                    "joint-api-local-mr-smc",
                    mr_smc_ast,
                    alias.root,
                    alias.details.clone().unwrap_or_default(),
                ));
            }
            // Partial composite: MR in full, SMC first-group-only.
            // Same gating and ranking convention as the S1 partial
            // composite above.
            if mr_smc_cached > 1 {
                let mut mr_smc_partial_ast = mr_ast.clone();
                let mr_smc_partial =
                    passes::straightline_member_reads::cache_straightline_member_reads_partial(
                        &mut mr_smc_partial_ast,
                        alias.root,
                    );
                let mr_smc_partial_cached = mr_smc_partial
                    .details
                    .as_ref()
                    .and_then(|details| details.first())
                    .and_then(|detail| detail.strip_prefix("cached="))
                    .and_then(|count| count.parse::<usize>().ok())
                    .unwrap_or(0);
                if mr_smc_partial_cached > 0 {
                    layouts.push((
                        "joint-api-local-mr-smc-partial",
                        mr_smc_partial_ast,
                        alias.root,
                        alias.details.clone().unwrap_or_default(),
                    ));
                }
            }
            layouts.push((
                "joint-api-local-mr",
                mr_ast,
                alias.root,
                alias.details.clone().unwrap_or_default(),
            ));
        }
        // Target-size only: straight-line member-read caching on top of
        // the localized layout (see passes::straightline_member_reads).
        // Adopted only when caching fires, as with S1/MR.
        let mut smc_ast = alias_ast.clone();
        let smc =
            passes::straightline_member_reads::cache_straightline_member_reads_with_resolution(
                &mut smc_ast,
                alias.root,
                speed_analysis.resolution,
            );
        let smc_cached = smc
            .details
            .as_ref()
            .and_then(|details| details.first())
            .and_then(|detail| detail.strip_prefix("cached="))
            .and_then(|count| count.parse::<usize>().ok())
            .unwrap_or(0);
        if smc_cached > 0 {
            layouts.push((
                "joint-api-local-smc",
                smc_ast,
                alias.root,
                alias.details.clone().unwrap_or_default(),
            ));
        }
        // Partial SMC: only when the full run cached more than one
        // group, so this layout differs from the full one. It ranks
        // below the full SMC layout but above the base layout, so it
        // can only win when the full layout exceeds the target while
        // the single-group layout still fits.
        if smc_cached > 1 {
            let mut smc_partial_ast = alias_ast.clone();
            let smc_partial = passes::straightline_member_reads::cache_straightline_member_reads_partial_with_resolution(
                    &mut smc_partial_ast,
                    alias.root,
                    speed_analysis.resolution,
                );
            let smc_partial_cached = smc_partial
                .details
                .as_ref()
                .and_then(|details| details.first())
                .and_then(|detail| detail.strip_prefix("cached="))
                .and_then(|count| count.parse::<usize>().ok())
                .unwrap_or(0);
            if smc_partial_cached > 0 {
                layouts.push((
                    "joint-api-local-smc-partial",
                    smc_partial_ast,
                    alias.root,
                    alias.details.clone().unwrap_or_default(),
                ));
            }
        }
    }
    // Target-size only: loop-invariant pure host reads and total
    // arithmetic hoisted out of loops, then localized aliases on top. The LICM pass runs
    // first so it sees the original host names; the alias pass then
    // shortens the remaining references, including hoist definitions.
    // The layout is only added when at least one call was hoisted, so
    // inputs without a LICM opportunity keep the previous candidate set.
    if target_speed && toggles.enabled("api-alias-optimization") {
        let mut licm_ast = phase_ast.clone();
        let licm =
            passes::loop_invariant::hoist_loop_invariant_host_reads(&mut licm_ast, phase_root);
        let hoisted = licm
            .details
            .as_ref()
            .and_then(|details| details.first())
            .and_then(|detail| detail.strip_prefix("hoisted="))
            .and_then(|count| count.parse::<usize>().ok())
            .unwrap_or(0);
        if hoisted > 0 {
            // Relaxed carrier synthesis on top of LICM, for the same
            // reason as the localized layout: LICM runs first so
            // loop-invariant reads are hoisted before any remaining
            // straight-line duplicate is given a carrier, and the
            // scratch clone is adopted only when a step fires.
            // Barrier-stable reuse and relaxed-barrier synthesis run
            // here too, matching the localized layout.
            let mut licm_root = licm.root;
            if toggles.enabled("pure-call-carrier-synthesis") {
                let mut relaxed_ast = licm_ast.clone();
                let mut reused_count = 0;
                if toggles.enabled("available-expression-reuse") {
                    let reuse =
                        passes::available_expressions::reuse_available_expressions_barrier_relaxed(
                            &mut relaxed_ast,
                            licm.root,
                        );
                    reused_count = reuse
                        .details
                        .as_ref()
                        .and_then(|details| details.first())
                        .and_then(|detail| detail.strip_prefix("reused="))
                        .and_then(|count| count.parse::<usize>().ok())
                        .unwrap_or(0);
                }
                let relaxed =
                    passes::available_expressions::synthesize_pure_call_carriers_relaxed_barrier(
                        &mut relaxed_ast,
                        licm.root,
                    );
                let relaxed_count = relaxed
                    .details
                    .as_ref()
                    .and_then(|details| details.first())
                    .and_then(|detail| detail.strip_prefix("synthesized="))
                    .and_then(|count| count.parse::<usize>().ok())
                    .unwrap_or(0);
                if reused_count > 0 || relaxed_count > 0 {
                    licm_ast = relaxed_ast;
                    licm_root = relaxed.root;
                }
            }
            let alias =
                passes::api_aliases::optimize_api_aliases_localized(&mut licm_ast, licm_root);
            let aliases = alias.details.clone().unwrap_or_default();
            // Shared first analyses for the speed passes on the
            // LICM base, as on the localized base above (see
            // passes::speed_analysis).
            let licm_speed_resolution =
                storm_lua_analysis::resolver::resolve(&licm_ast, alias.root);
            let licm_speed_effects = storm_lua_analysis::effects::EffectAnalyzer::new(
                &licm_ast,
                &licm_speed_resolution,
                alias.root,
                true,
            );
            let licm_speed_mutated = storm_lua_analysis::effects::mutated_builtin_roots(
                &licm_ast,
                &licm_speed_resolution,
                alias.root,
            );
            let licm_speed_analysis = passes::speed_analysis::SpeedAnalysis {
                resolution: &licm_speed_resolution,
                effects: &licm_speed_effects,
                mutated: &licm_speed_mutated,
            };
            // Target-size only: single member-call caching on top of
            // the LICM layout, for member calls LICM did not hoist
            // (LICM hoists whole calls; S1 caches callees). Adopted
            // only when caching fires, as above.
            let mut s1_ast = licm_ast.clone();
            let s1 =
                passes::single_member_cache::cache_single_member_calls_post_alias_with_analysis(
                    &mut s1_ast,
                    alias.root,
                    &licm_speed_analysis,
                );
            let s1_cached = s1
                .details
                .as_ref()
                .and_then(|details| details.first())
                .and_then(|detail| detail.strip_prefix("cached="))
                .and_then(|count| count.parse::<usize>().ok())
                .unwrap_or(0);
            let s1_aliases = aliases.clone();
            let s1_partial_aliases = s1_aliases.clone();
            layouts.push((
                "joint-api-local-licm",
                licm_ast.clone(),
                alias.root,
                aliases,
            ));
            if s1_cached > 0 {
                let mut s1_smc_ast = s1_ast.clone();
                let s1_smc = passes::straightline_member_reads::cache_straightline_member_reads(
                    &mut s1_smc_ast,
                    alias.root,
                );
                let s1_smc_cached = s1_smc
                    .details
                    .as_ref()
                    .and_then(|details| details.first())
                    .and_then(|detail| detail.strip_prefix("cached="))
                    .and_then(|count| count.parse::<usize>().ok())
                    .unwrap_or(0);
                if s1_smc_cached > 0 {
                    layouts.push((
                        "joint-api-local-licm-s1-smc",
                        s1_smc_ast,
                        alias.root,
                        s1_aliases.clone(),
                    ));
                }
                // Partial composite: S1 in full, SMC
                // first-group-only (same convention as the
                // non-LICM layout above).
                if s1_smc_cached > 1 {
                    let mut s1_smc_partial_ast = s1_ast.clone();
                    let s1_smc_partial =
                        passes::straightline_member_reads::cache_straightline_member_reads_partial(
                            &mut s1_smc_partial_ast,
                            alias.root,
                        );
                    let s1_smc_partial_cached = s1_smc_partial
                        .details
                        .as_ref()
                        .and_then(|details| details.first())
                        .and_then(|detail| detail.strip_prefix("cached="))
                        .and_then(|count| count.parse::<usize>().ok())
                        .unwrap_or(0);
                    if s1_smc_partial_cached > 0 {
                        layouts.push((
                            "joint-api-local-licm-s1-smc-partial",
                            s1_smc_partial_ast,
                            alias.root,
                            s1_aliases.clone(),
                        ));
                    }
                }
                layouts.push(("joint-api-local-licm-s1", s1_ast, alias.root, s1_aliases));
            }
            if s1_cached > 1 {
                let mut s1_partial_ast = licm_ast.clone();
                let s1_partial = passes::single_member_cache::cache_single_member_calls_post_alias_partial_with_analysis(
                        &mut s1_partial_ast,
                        alias.root,
                        &licm_speed_analysis,
                    );
                let s1_partial_cached = s1_partial
                    .details
                    .as_ref()
                    .and_then(|details| details.first())
                    .and_then(|detail| detail.strip_prefix("cached="))
                    .and_then(|count| count.parse::<usize>().ok())
                    .unwrap_or(0);
                if s1_partial_cached > 0 {
                    layouts.push((
                        "joint-api-local-licm-s1-partial",
                        s1_partial_ast,
                        alias.root,
                        s1_partial_aliases,
                    ));
                }
            }
            // Target-size only: loop member-read caching on top of the
            // LICM layout (see passes::loop_member_reads). Adopted only
            // when caching fires, as with S1.
            let mut mr_ast = licm_ast.clone();
            let mr = passes::loop_member_reads::cache_loop_member_reads_with_analysis(
                &mut mr_ast,
                alias.root,
                &licm_speed_analysis,
            );
            let mr_cached = mr
                .details
                .as_ref()
                .and_then(|details| details.first())
                .and_then(|detail| detail.strip_prefix("cached="))
                .and_then(|count| count.parse::<usize>().ok())
                .unwrap_or(0);
            if mr_cached > 0 {
                let mut mr_smc_ast = mr_ast.clone();
                let mr_smc = passes::straightline_member_reads::cache_straightline_member_reads(
                    &mut mr_smc_ast,
                    alias.root,
                );
                let mr_smc_cached = mr_smc
                    .details
                    .as_ref()
                    .and_then(|details| details.first())
                    .and_then(|detail| detail.strip_prefix("cached="))
                    .and_then(|count| count.parse::<usize>().ok())
                    .unwrap_or(0);
                if mr_smc_cached > 0 {
                    layouts.push((
                        "joint-api-local-licm-mr-smc",
                        mr_smc_ast,
                        alias.root,
                        alias.details.clone().unwrap_or_default(),
                    ));
                }
                // Partial composite: MR in full, SMC
                // first-group-only (same convention as the
                // non-LICM layout above).
                if mr_smc_cached > 1 {
                    let mut mr_smc_partial_ast = mr_ast.clone();
                    let mr_smc_partial =
                        passes::straightline_member_reads::cache_straightline_member_reads_partial(
                            &mut mr_smc_partial_ast,
                            alias.root,
                        );
                    let mr_smc_partial_cached = mr_smc_partial
                        .details
                        .as_ref()
                        .and_then(|details| details.first())
                        .and_then(|detail| detail.strip_prefix("cached="))
                        .and_then(|count| count.parse::<usize>().ok())
                        .unwrap_or(0);
                    if mr_smc_partial_cached > 0 {
                        layouts.push((
                            "joint-api-local-licm-mr-smc-partial",
                            mr_smc_partial_ast,
                            alias.root,
                            alias.details.clone().unwrap_or_default(),
                        ));
                    }
                }
                layouts.push((
                    "joint-api-local-licm-mr",
                    mr_ast,
                    alias.root,
                    alias.details.clone().unwrap_or_default(),
                ));
            }
            // Target-size only: straight-line member-read caching on top
            // of the LICM layout (see passes::straightline_member_reads).
            // Adopted only when caching fires, as with S1/MR.
            let mut smc_ast = licm_ast.clone();
            let smc =
                passes::straightline_member_reads::cache_straightline_member_reads_with_resolution(
                    &mut smc_ast,
                    alias.root,
                    licm_speed_analysis.resolution,
                );
            let smc_cached = smc
                .details
                .as_ref()
                .and_then(|details| details.first())
                .and_then(|detail| detail.strip_prefix("cached="))
                .and_then(|count| count.parse::<usize>().ok())
                .unwrap_or(0);
            if smc_cached > 0 {
                layouts.push((
                    "joint-api-local-licm-smc",
                    smc_ast,
                    alias.root,
                    alias.details.clone().unwrap_or_default(),
                ));
            }
            // Partial SMC: only when the full run cached more than one
            // group, so this layout differs from the full one. It ranks
            // below the full SMC layout but above the base layout, so it
            // can only win when the full layout exceeds the target while
            // the single-group layout still fits.
            if smc_cached > 1 {
                let mut smc_partial_ast = licm_ast.clone();
                let smc_partial = passes::straightline_member_reads::cache_straightline_member_reads_partial_with_resolution(
                        &mut smc_partial_ast,
                        alias.root,
                        licm_speed_analysis.resolution,
                    );
                let smc_partial_cached = smc_partial
                    .details
                    .as_ref()
                    .and_then(|details| details.first())
                    .and_then(|detail| detail.strip_prefix("cached="))
                    .and_then(|count| count.parse::<usize>().ok())
                    .unwrap_or(0);
                if smc_partial_cached > 0 {
                    layouts.push((
                        "joint-api-local-licm-smc-partial",
                        smc_partial_ast,
                        alias.root,
                        alias.details.clone().unwrap_or_default(),
                    ));
                }
            }
        }
    }

    // Best speed rank among accepted fitting candidates in this job, for
    // winner-preserving target-size pruning (see the loop head).
    let mut best_fitting_rank: Option<usize> = None;
    for (layout_index, (layout, mut candidate_ast, mut candidate_root, aliases)) in
        layouts.into_iter().enumerate()
    {
        // Winner-preserving target-size pruning (option B full search):
        // selection sorts fitting candidates by (fits, speed rank, size,
        // order). Once this job holds an accepted fitting candidate of
        // rank r, a later layout with a worse rank can never win: it
        // loses on rank if it fits, and on fits if it does not. The
        // beating candidate stays in this batch, so it also survives
        // global selection. Nothing is pruned before a fitting candidate
        // exists, so the nothing-fits fallback (plain size minimization)
        // is unaffected. Evaluated layouts keep their layout-index order.
        if target_size.is_some() {
            if let Some(best) = best_fitting_rank {
                if target_speed_rank(layout) > best {
                    continue;
                }
            }
        }
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
        if toggles.enabled("pure-call-carrier-synthesis") {
            let r = passes::available_expressions::synthesize_pure_call_carriers(
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
                    // A localized alias layout must not be polished back into
                    // globals: that would silently discard the speed variant
                    // that target-size selection is about to prefer.
                    if target_speed
                        && id == "root-local-globalization"
                        && (layout == "joint-api-local"
                            || layout == "joint-api-local-licm"
                            || layout == "joint-api-local-s1"
                            || layout == "joint-api-local-licm-s1"
                            || layout == "joint-api-local-s1-partial"
                            || layout == "joint-api-local-licm-s1-partial"
                            || layout == "joint-api-local-mr"
                            || layout == "joint-api-local-licm-mr"
                            || layout == "joint-api-local-smc"
                            || layout == "joint-api-local-licm-smc"
                            || layout == "joint-api-local-smc-partial"
                            || layout == "joint-api-local-licm-smc-partial"
                            || layout == "joint-api-local-s1-smc"
                            || layout == "joint-api-local-licm-s1-smc"
                            || layout == "joint-api-local-mr-smc"
                            || layout == "joint-api-local-licm-mr-smc"
                            || layout == "joint-api-local-s1-smc-partial"
                            || layout == "joint-api-local-licm-s1-smc-partial"
                            || layout == "joint-api-local-mr-smc-partial"
                            || layout == "joint-api-local-licm-mr-smc-partial")
                    {
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
            if aggressive && toggles.enabled("exact-string-literal-pooling") {
                let r = passes::string_pool::pool_string_literals(
                    &mut candidate_ast,
                    candidate_root,
                    toggles.enabled("scope-renaming"),
                );
                candidate_root = r.root;
                if r.saved.unwrap_or(0) > 0 {
                    candidate_passes.push(PassRecord {
                        name: "exact string literal pooling".into(),
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
        let output = print_final(&candidate_ast, candidate_root, zero_cost_newlines);
        if parse_source(output.code()).is_err() {
            parse_rejected += 1;
            continue;
        }
        if let Some(verify) = verify_candidate.as_deref_mut() {
            // The external verifier contract uses compact Lua even when the
            // selected output requests zero-cost newlines.
            let compact = zero_cost_newlines
                .then(|| Printer::new(&candidate_ast, false).output(candidate_root));
            if !verify(compact.as_deref().unwrap_or_else(|| output.code())) {
                semantic_rejected += 1;
                continue;
            }
        }
        let size = target_char_size(output.code());
        if size != final_size {
            return Err("zero-cost newline printer changed size".into());
        }
        let (code, newline_count, origins) = output.finish(&candidate_ast);
        if let Some(target) = target_size {
            if size <= target {
                let rank = target_speed_rank(layout);
                if best_fitting_rank.is_none_or(|best| rank < best) {
                    best_fitting_rank = Some(rank);
                }
            }
        }
        candidates.push(Candidate {
            origins: origins.map(Arc::new),
            lexical_origins: None,
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

/// Target-size speed rank of a layout, shared by winner selection and by
/// winner-preserving layout pruning in evaluate_candidate_with_verifier.
/// Lower is faster; layouts outside the speed family rank last.
fn target_speed_rank(layout: &str) -> usize {
    match layout {
        "joint-api-local-licm-s1-smc" => 0,
        "joint-api-local-licm-mr-smc" => 1,
        "joint-api-local-licm-s1-smc-partial" => 2,
        "joint-api-local-licm-mr-smc-partial" => 3,
        "joint-api-local-licm-s1" => 4,
        "joint-api-local-licm-s1-partial" => 5,
        "joint-api-local-licm-mr" => 6,
        "joint-api-local-licm-smc" => 7,
        "joint-api-local-licm-smc-partial" => 8,
        "joint-api-local-licm" => 9,
        "joint-api-local-s1-smc" => 10,
        "joint-api-local-mr-smc" => 11,
        "joint-api-local-s1-smc-partial" => 12,
        "joint-api-local-mr-smc-partial" => 13,
        "joint-api-local-s1" => 14,
        "joint-api-local-s1-partial" => 15,
        "joint-api-local-mr" => 16,
        "joint-api-local-smc" => 17,
        "joint-api-local-smc-partial" => 18,
        "joint-api-local" => 19,
        "joint-api" => 20,
        _ => 21,
    }
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
    // Target-size compilation (OBJ-2) prioritizes generated-code runtime
    // speed within the target, not minimal size: among candidates that fit
    // the target, the LICM layout beats the localized alias layout, which
    // beats the global alias layout, which beats every other layout; the
    // S1 layouts sit above their non-S1 counterparts (LICM+S1 above
    // LICM, local+S1 above local), since the S1 layout is its base
    // layout plus per-iteration table-lookup savings; a partial S1
    // layout sits between its full S1 layout and its base layout, so
    // it can only win when the full layout exceeds the target; the MR
    // layouts sit above their base layouts but below the S1 layouts; the
    // SMC layouts sit above their base layouts but below the MR layouts
    // (an MR saving scales with trip count, an SMC saving does not); a
    // partial SMC layout sits between its full SMC layout and its base
    // layout, so it can only win when the full layout exceeds the
    // target; ties fall back to size, then order. Composite S1+SMC /
    // MR+SMC layouts sit above both of their components; a partial
    // composite (full S1/MR with first-group-only SMC) sits directly
    // below its full composite, so it can only win when the full
    // composite exceeds the target.
    // When nothing fits the target, selection is plain size minimization,
    // exactly as in target-free compilation. This ranking is only reachable
    // with a target: target-free contexts carry lexical_target None (the
    // lexical path has a single candidate, so its ranking is unchanged).
    let speed_rank = target_speed_rank;
    // The sort key is (over-target, speed rank, size, order): fitting
    // candidates always sort before over-target ones, so a speed variant
    // that exceeds the target can never beat a fitting candidate. When
    // nothing fits, the rank is ignored and plain size minimization applies.
    if let Some(target) = context.lexical_target {
        candidates.sort_by(|a, b| {
            let key = |candidate: &Candidate| {
                let over = candidate.size > target;
                (
                    over,
                    if over {
                        0
                    } else {
                        speed_rank(&candidate.layout)
                    },
                    candidate.size,
                    candidate.order,
                )
            };
            key(a).cmp(&key(b))
        });
    } else {
        candidates.sort_by(|a, b| a.size.cmp(&b.size).then(a.order.cmp(&b.order)));
    }
    let candidate_sizes = candidates
        .iter()
        .map(|candidate| CandidateSize {
            structural: candidate.structural.clone(),
            layout: candidate.layout.clone(),
            size: candidate.size,
        })
        .collect::<Vec<_>>();
    let best = candidates.remove(0);
    let origins = best.origins.map(|value| match Arc::try_unwrap(value) {
        Ok(value) => value.into_owned(),
        Err(shared) => shared.get().clone(),
    });
    let lexical_origins = best.lexical_origins;
    if let Some(origins) = &origins {
        origins.validate_for_code(&best.code)?;
    }
    if let Some(origins) = &lexical_origins {
        origins.validate_for_code(&best.code)?;
    }
    let lexical_target_met = context
        .lexical_target
        .is_some_and(|target| best.size <= target);
    Ok(CompileCodeResult {
        origins,
        lexical_origins,
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

fn select_best_compact(
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

pub fn select_best(
    context: SearchContext,
    batches: Vec<CandidateBatch>,
) -> Result<CompileCodeResult, String> {
    select_best_compact(context, batches).map(CompileCodeResult::materialize_lexical_origins)
}

fn target_char_size(code: &str) -> usize {
    storm_lua_syntax::source_position::utf16_len(code)
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
    let output = print_final(&candidate_ast, candidate_root, zero_cost_newlines);
    if parse_source(output.code()).is_err() {
        return Err("satisficing checkpoint failed reparse validation".into());
    }
    let size = target_char_size(output.code());
    let (code, newline_count, origins) = output.finish(&candidate_ast);
    Ok(Candidate {
        origins: origins.map(Arc::new),
        lexical_origins: None,
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
        lexical_target: Some(target_size),
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

/// Run deterministic target search. Checkpoints record progress and honor
/// the optional checkpoint budget, but a fitting candidate no longer ends
/// the search: under the 2026-10-30 satisficing decision (option B) the
/// search continues to the speed layouts and the winner is the fastest
/// fitting candidate. All later work uses the same core and candidates as
/// target-free search. No wall-clock measurements affect scheduling or
/// candidate selection.
fn try_satisficing_compact(
    source: &str,
    options: &CompileOptions,
    max_checkpoints: Option<usize>,
) -> Result<SatisficingAttempt, String> {
    options.validate_pass_toggles()?;
    // `target_size` presence is validated here; the value itself is read
    // again by `try_satisficing_from_parsed`.
    let _target_size = options
        .target_size
        .ok_or("try_satisficing requires target_size")?;
    // A source-fit result is no longer guaranteed to be the identity copy
    // (option B: the fastest fitting optimized candidate wins instead), so
    // always parse for search: the winning optimized candidate needs its
    // tracked origins. Hardcoded properties retain full tracking as before.
    let (parsed_ast, parsed_root) = parse_for_search(source, options)?;
    try_satisficing_from_parsed(source, options, max_checkpoints, parsed_ast, parsed_root)
}
pub fn try_satisficing(
    source: &str,
    options: &CompileOptions,
    max_checkpoints: Option<usize>,
) -> Result<SatisficingAttempt, String> {
    let mut attempt = try_satisficing_compact(source, options, max_checkpoints)?;
    attempt.result = attempt.result.materialize_lexical_origins();
    Ok(attempt)
}

fn try_satisficing_from_parsed(
    source: &str,
    options: &CompileOptions,
    max_checkpoints: Option<usize>,
    parsed_ast: Ast,
    parsed_root: NodeId,
) -> Result<SatisficingAttempt, String> {
    let target_size = options
        .target_size
        .ok_or("try_satisficing requires target_size")?;
    let resolved_toggles = resolve_pass_toggles(&options.pass_toggles, options.numeric_mode);
    storm_lua_analysis::environment_checks::validate(
        &parsed_ast,
        parsed_root,
        options.environment,
        &options.host_bindings,
    )?;
    // Same source-legality gate as prepare_search_from_parsed (S10/S14).
    storm_lua_vm::load_legality::check_load_legality(source.as_bytes())?;
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
        let result = select_best_compact(context, batches)?;
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

    // Checkpoints no longer return on a fitting candidate (option B):
    // they only count progress and honor the caller's checkpoint budget.
    // The final winner is selected after the full search completes.
    macro_rules! checkpoint {
        ($stage:expr) => {{
            checkpoints += 1;
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
    // be hardcoded. Fitting the target no longer ends the search: the source
    // remains the fallback winner only when no optimized candidate fits.
    // Property hardcode mode instead emits the transformed
    // AST first so the requested option is still honored.
    if property_reads_hardcoded == 0 {
        explored.push(Candidate {
            origins: options.origin_source.as_ref().map(|name| {
                Arc::new(DeferredGeneratedOrigins::ready(GeneratedOrigins::identity(
                    name, source,
                )))
            }),
            lexical_origins: None,
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
        context.lexical_target = Some(target_size);
        let result = finalize_satisficing_fallback(
            select_best_compact(context, Vec::new())?,
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
    select_best_compact(context, batches)
}

/// Compile without an external semantic verifier. `target_size` activates
/// OBJ-2 target search within canonical search (checkpoints record progress;
/// a fitting candidate does not stop the search); absence of a target
/// preserves OBJ-1.
#[doc(hidden)]
pub fn compile_code_compact(
    source: &str,
    options: &CompileOptions,
) -> Result<CompileCodeResult, String> {
    let Some(target_size) = options.target_size else {
        return compile_code_obj1(source, options);
    };
    finish_target_attempt(try_satisficing_compact(source, options, None)?, target_size)
}

pub fn compile_code(source: &str, options: &CompileOptions) -> Result<CompileCodeResult, String> {
    compile_code_compact(source, options).map(CompileCodeResult::materialize_lexical_origins)
}
fn finish_target_attempt(
    mut attempt: SatisficingAttempt,
    target_size: usize,
) -> Result<CompileCodeResult, String> {
    match attempt.status {
        SatisficingStatus::Satisfied | SatisficingStatus::Stopped => Ok(attempt.result),
        SatisficingStatus::NeedsFullSearch => {
            let prior_checkpoints = attempt.result.stats.checkpoints;
            let (mut context, jobs) = take_satisficing_fallback(&mut attempt)
                .ok_or("satisficing fallback preparation missing")?;
            context.lexical_target = Some(target_size);
            let batches = jobs
                .into_iter()
                .map(evaluate_candidate)
                .collect::<Result<Vec<_>, _>>()?;
            let result = select_best_compact(context, batches)?;
            Ok(finalize_satisficing_fallback(
                result,
                target_size,
                prior_checkpoints,
            ))
        }
    }
}

/// Compile while allowing the host to inspect the original parsed source once.
/// Inspection is read-only and happens before any transformation; its positions
/// are diagnostic points on this arena. The compiler retains the same validation,
/// candidate order and final result as `compile_code`. If parsing fails, the
/// callback is not invoked. No state is cached between compilations.
#[doc(hidden)]
pub fn compile_code_with_source_inspection_compact(
    source: &str,
    options: &CompileOptions,
    inspect: impl FnOnce(&Ast, NodeId, &storm_lua_syntax::parser::NodePositions),
) -> Result<CompileCodeResult, String> {
    options.validate_pass_toggles()?;
    let lexical_options = options.environment
        == storm_lua_spec::environment::EnvironmentProfile::Extended
        || !options.host_bindings.is_empty();
    let (ast, root, positions) = if let Some(name) = options
        .origin_source
        .as_deref()
        .filter(|_| !lexical_options)
    {
        storm_lua_syntax::parser::parse_source_with_origin_positions(name, source, |ast| {
            storm_lua_analysis::environment_checks::lexical_reason(
                ast,
                options.environment,
                &options.host_bindings,
            )
            .is_none()
        })?
    } else {
        storm_lua_syntax::parser::parse_source_with_diagnostic_positions(source)
            .map_err(|e| e.to_string())?
    };
    inspect(&ast, root, &positions);
    // The origins own their ranges. Do not retain the original diagnostic/name
    // indexes throughout every candidate search after the host has inspected them.
    drop(positions);
    if let Some(target) = options.target_size {
        finish_target_attempt(
            try_satisficing_from_parsed(source, options, None, ast, root)?,
            target,
        )
    } else {
        let (context, jobs) = prepare_search_from_parsed(source, options, true, ast, root)?;
        let batches = jobs
            .into_iter()
            .map(evaluate_candidate)
            .collect::<Result<Vec<_>, _>>()?;
        select_best_compact(context, batches)
    }
}

pub fn compile_code_with_source_inspection(
    source: &str,
    options: &CompileOptions,
    inspect: impl FnOnce(&Ast, NodeId, &storm_lua_syntax::parser::NodePositions),
) -> Result<CompileCodeResult, String> {
    compile_code_with_source_inspection_compact(source, options, inspect)
        .map(CompileCodeResult::materialize_lexical_origins)
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
    let result = select_best_compact(context, batches)?;
    let result = if let Some(target_size) = options.target_size {
        finalize_satisficing_fallback(result, target_size, 0)
    } else {
        result
    };
    Ok(result.materialize_lexical_origins())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod phase7_serialization_tests {
    use super::*;

    const TARGET_PROBE_SOURCE: &str = "function onTick()output.setNumber(1,1)output.setNumber(2,2)output.setNumber(3,3)output.setNumber(4,4)output.setBool(1,true)output.setBool(2,false)output.setBool(3,true)output.setBool(4,false)end";

    /// Target-size compilation evaluates one extra layout (the localized
    /// alias layout) per api-alias job. Tests that compare a target-mode
    /// result against the target-free candidate set must therefore strip
    /// the localized entries and add the extra attempts back.
    fn without_local_layouts(sizes: &[CandidateSize]) -> Vec<CandidateSize> {
        sizes
            .iter()
            .filter(|size| size.layout != "joint-api-local")
            .cloned()
            .collect()
    }

    fn alias_job_count(source: &str, options: &CompileOptions) -> usize {
        prepare_search(source, options)
            .unwrap()
            .1
            .iter()
            .filter(|job| job.toggles.enabled("api-alias-optimization"))
            .count()
    }

    #[test]
    fn target_size_prefers_localized_aliases_within_budget() {
        let free = compile_code(TARGET_PROBE_SOURCE, &CompileOptions::default()).unwrap();
        let free_size = target_char_size(&free.code);
        assert_eq!(free.stats.winner_layout, "joint-api");
        // A target that fits the localized layout selects it, even though
        // it is longer than the global-alias winner.
        let mut options = CompileOptions::default();
        options.target_size = Some(free_size + 12);
        let localized = compile_code(TARGET_PROBE_SOURCE, &options).unwrap();
        assert_eq!(localized.stats.winner_layout, "joint-api-local");
        assert!(target_char_size(&localized.code) <= free_size + 12);
        // Zero-cost newlines put a newline after the `local` keyword.
        assert!(localized.code.starts_with("local"));
        assert!(localized.stats.target_met);
        // A target that fits only the global layout must not select the
        // localized layout, even though it ranks first on speed: the
        // target is a hard budget cap.
        let mut options = CompileOptions::default();
        options.target_size = Some(free_size);
        let capped = compile_code(TARGET_PROBE_SOURCE, &options).unwrap();
        assert_eq!(capped.stats.winner_layout, "joint-api");
        assert!(target_char_size(&capped.code) <= free_size);
        assert!(capped.stats.target_met);
        // Target-free compilation is unchanged: the localized layout is
        // never generated, so the winner stays the global alias layout.
        assert!(!free.code.starts_with("local"));
    }

    /// Relaxed carrier synthesis inside the target-size localized
    /// layout: three straight-line `screen.getHeight()` calls are
    /// rejected by the strict gate (their aliased form is only three
    /// characters, so the carrier definition costs more characters
    /// than it saves), but within a target budget speed outranks
    /// size, so the localized layout synthesizes one carrier and
    /// calls it once instead of three times. The capped case checks
    /// that the relaxed layout, at one character over the free
    /// winner, never beats a fitting candidate.
    #[test]
    fn target_size_relaxed_carrier_layout_within_budget() {
        let source = "function onTick()output.setNumber(1,screen.getHeight()+screen.getHeight()+screen.getHeight())end";
        let free = compile_code(source, &CompileOptions::default()).unwrap();
        assert_eq!(free.stats.winner_layout, "joint-api");
        let free_size = target_char_size(&free.code);
        let mut options = CompileOptions::default();
        options.target_size = Some(free_size + 5);
        let relaxed = compile_code(source, &options).unwrap();
        assert_eq!(relaxed.stats.winner_layout, "joint-api-local");
        assert!(relaxed.stats.target_met);
        assert!(target_char_size(&relaxed.code) <= free_size + 5);
        assert_eq!(relaxed.code.matches("getHeight()").count(), 1);
        parse_source(&relaxed.code).expect("relaxed carrier winner reparses");
        let mut options = CompileOptions::default();
        options.target_size = Some(free_size);
        let capped = compile_code(source, &options).unwrap();
        assert_eq!(capped.stats.winner_layout, "joint-api");
        assert!(capped.stats.target_met);
        assert!(target_char_size(&capped.code) <= free_size);
    }

    /// Target-size barrier relaxation (reuse): two straight-line
    /// `screen.getWidth()` calls separated by an unknown call keep two
    /// calls in the target-free winner (the alias definition is the only
    /// `screen.getWidth` text), but the target-size localized layout
    /// reuses the carrier across the barrier and wins with a single
    /// direct call inside the budget.
    #[test]
    fn target_size_barrier_relaxed_reuse_wins_within_budget() {
        let source = "function onDraw()local w=screen.getWidth()foo()local h=screen.getWidth()screen.drawRect(0,0,w,h)end";
        let free = compile_code(source, &CompileOptions::default()).unwrap();
        assert_eq!(free.stats.winner_layout, "joint-api");
        assert_eq!(free.code.matches("screen.getWidth()").count(), 0);
        let free_size = target_char_size(&free.code);
        let mut options = CompileOptions::default();
        options.target_size = Some(free_size + 2);
        let relaxed = compile_code(source, &options).unwrap();
        assert_eq!(relaxed.stats.winner_layout, "joint-api-local");
        assert!(relaxed.stats.target_met);
        assert!(target_char_size(&relaxed.code) <= free_size + 2);
        assert_eq!(relaxed.code.matches("screen.getWidth()").count(), 1);
        parse_source(&relaxed.code).expect("barrier-relaxed winner reparses");
    }

    /// Target-size barrier relaxation (synthesis): two straight-line
    /// `math.fmod` calls separated by a stateful call keep two calls in
    /// the target-free winner, but the target-size localized layout
    /// synthesizes one barrier-stable carrier and wins with a single
    /// call inside the budget.
    #[test]
    fn target_size_barrier_relaxed_synth_wins_within_budget() {
        let source = "function onTick()x=math.fmod(5.5,2)a=math.random(10)y=math.fmod(5.5,2)output.setNumber(1,x+y)end";
        let free = compile_code(source, &CompileOptions::default()).unwrap();
        assert_eq!(free.stats.winner_layout, "joint-api");
        assert_eq!(free.code.matches("math.fmod(5.5,2)").count(), 0);
        let free_size = target_char_size(&free.code);
        let mut options = CompileOptions::default();
        options.target_size = Some(free_size + 3);
        let relaxed = compile_code(source, &options).unwrap();
        assert_eq!(relaxed.stats.winner_layout, "joint-api-local");
        assert!(relaxed.stats.target_met);
        assert!(target_char_size(&relaxed.code) <= free_size + 3);
        assert_eq!(relaxed.code.matches("math.fmod(5.5,2)").count(), 1);
        parse_source(&relaxed.code).expect("barrier-relaxed winner reparses");
    }

    /// Target-free barrier-relaxed synthesis as an additional candidate
    /// (cycle 45): three straight-line `math.fmod` calls separated by
    /// unknown calls keep three calls in the target-free winner without
    /// the new layouts (joint-api, 97 characters), but the
    /// barrier-relaxed candidate collapses them into one carrier and
    /// wins at 91 characters. The previous layouts stay available, so a
    /// candidate like this can only win when it is smaller.
    #[test]
    fn target_free_barrier_relaxed_synth_candidate_wins() {
        let source = "function onTick()x=math.fmod(5.5,2)foo()y=math.fmod(5.5,2)foo()z=math.fmod(5.5,2)output.setNumber(1,x+y+z)end";
        let free = compile_code(source, &CompileOptions::default()).unwrap();
        assert_eq!(free.stats.winner_layout, "none-barrier-relaxed");
        assert_eq!(target_char_size(&free.code), 91);
        assert_eq!(free.code.matches("math.fmod(5.5,2)").count(), 1);
        parse_source(&free.code).expect("barrier-relaxed winner reparses");
    }

    /// Full-search winner selection for the LICM+S1 layout: with a
    /// generous target every candidate fits, so the combined layout
    /// (speed rank 0: LICM hoists the host reads and S1 caches the
    /// single math.min callee) beats the LICM-only layout. The
    /// capped case below checks that an over-target LICM layout never
    /// beats a fitting candidate.
    #[test]
    fn target_size_prefers_licm_layout_within_budget() {
        let source = "local s=0 function onTick() local t=0 for i=1,200 do t=t+input.getNumber(1)*2+input.getNumber(2)*3+math.min(i,50)*0.5 end s=t output.setNumber(1,t) output.setBool(1,t>100) end";
        let mut options = CompileOptions::default();
        options.target_size = Some(1000);
        let (mut context, jobs) = prepare_search(source, &options).unwrap();
        let batches = jobs
            .into_iter()
            .map(evaluate_candidate)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        context.lexical_target = Some(1000);
        let result = select_best_compact(context, batches).unwrap();
        assert_eq!(result.stats.winner_layout, "joint-api-local-licm-s1");
        assert!(result.stats.target_met);
        parse_source(&result.code).expect("licm winner reparses");
    }

    #[test]
    fn target_size_cap_excludes_licm_layout_over_budget() {
        let source = "local s=0 function onTick() local t=0 for i=1,200 do t=t+input.getNumber(1)*2+input.getNumber(2)*3+math.min(i,50)*0.5 end s=t output.setNumber(1,t) output.setBool(1,t>100) end";
        // The localized alias layout for this source is 146 characters;
        // every LICM layout is longer, so at this target the localized
        // layout must win and the LICM layout must not, even though it
        // ranks first on speed among fitting candidates.
        let mut options = CompileOptions::default();
        options.target_size = Some(146);
        let (mut context, jobs) = prepare_search(source, &options).unwrap();
        let batches = jobs
            .into_iter()
            .map(evaluate_candidate)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        context.lexical_target = Some(146);
        let result = select_best_compact(context, batches).unwrap();
        assert_eq!(result.stats.winner_layout, "joint-api-local");
        assert!(target_char_size(&result.code) <= 146);
        assert!(result.stats.target_met);
    }

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
    fn satisficing_source_fit_continues_to_full_search() {
        let source = "function onTick()end\n";
        let options = CompileOptions {
            target_size: Some(source.encode_utf16().count()),
            ..CompileOptions::default()
        };
        let result = compile_code(source, &options).expect("already satisfied");
        // Option B: the fitting source does not stop the search; the
        // winner is the printed form and the search runs to completion.
        assert_eq!(result.code, "function\nonTick()end");
        assert!(result.stats.target_met);
        assert!(!result.stats.stopped_early);
        assert_eq!(result.stats.stage.as_deref(), Some("full-search"));
        assert!(result.stats.checkpoints >= 2);
    }

    #[test]
    fn satisficing_continues_past_fitting_checkpoint_to_full_search() {
        let source = "function onTick()output.setNumber(1,1+2)end";
        let options = CompileOptions {
            target_size: Some(41),
            ..CompileOptions::default()
        };
        let result = compile_code(source, &options).expect("target compile");
        // Option B: the fitting structural candidate does not stop the
        // search, but it still wins here because no speed layout fits 41.
        assert_eq!(result.code, "function\nonTick()output.setNumber(1,3)end");
        assert!(result.stats.target_met);
        assert!(!result.stats.stopped_early);
        assert_eq!(result.stats.stage.as_deref(), Some("full-search"));
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
        assert_eq!(result.code, "function\nonTick()output.setNumber(1,3)end");
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
                    let alias_jobs = alias_job_count(
                        &source,
                        &CompileOptions {
                            target_size: Some(0),
                            ..options.clone()
                        },
                    );
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
                    assert_eq!(
                        without_local_layouts(&target.stats.candidate_sizes),
                        baseline.stats.candidate_sizes
                    );
                    assert_eq!(
                        target.stats.attempted,
                        baseline.stats.attempted + alias_jobs
                    );
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
                let alias_jobs = alias_job_count(RESUME_SOURCE, &options);
                let actual = compile_code(RESUME_SOURCE, &options).unwrap();
                assert_eq!(actual.code, expected.code);
                assert_eq!(
                    without_local_layouts(&actual.stats.candidate_sizes),
                    expected.stats.candidate_sizes
                );
                assert_eq!(
                    actual.stats.attempted,
                    expected.stats.attempted + alias_jobs
                );
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
        let alias_jobs = alias_job_count(source, &options);
        assert_eq!(
            attempt.result.stats.attempted,
            baseline.stats.attempted + alias_jobs
        );
        assert_eq!(attempt.result.stats.stage.as_deref(), Some("full-search"));
        assert!(!attempt.result.stats.stopped_early);
    }

    #[test]
    fn target_search_winner_reports_utf16_size() {
        let source = "-- 日本語😀\nfunction onTick()end";
        let options = CompileOptions {
            target_size: Some(source.encode_utf16().count()),
            ..CompileOptions::default()
        };
        let result = compile_code(source, &options).unwrap();
        // Option B: the comment is stripped by the winning candidate, so
        // the reported size is the winner's UTF-16 length, not the source's.
        assert_eq!(result.code, "function\nonTick()end");
        assert_eq!(
            result.stats.candidate_sizes[0].size,
            result.code.encode_utf16().count()
        );
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
        assert_eq!(
            without_local_layouts(&actual.stats.candidate_sizes),
            expected.stats.candidate_sizes
        );
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod identity_origin_tests {
    use super::*;
    use crate::config::{PropertyConfig, PropertyMode};

    #[test]
    fn source_sized_target_continues_to_fastest_fitting_winner() {
        // Option B: a fitting source is no longer returned unexamined.
        // The winner must fit, its origins must validate for the winner
        // code, and it is the identity copy only when no optimized
        // candidate fits (the fallback). The folded first input pins that
        // optimization actually happens instead of an identity return.
        for source in [
            "function onTick()output.setNumber(1,2*3)end",
            "-- 😀雪\r\nfunction onTick()\r\n output.setNumber(1,2)\r\nend\r\n",
            "-- comment-only source",
            "",
        ] {
            for zero in [false, true] {
                let config = CompileOptions {
                    target_size: Some(8192),
                    origin_source: Some("input.lua".into()),
                    zero_cost_newlines: zero,
                    ..Default::default()
                };
                let result = try_satisficing(source, &config, None).unwrap().result;
                assert!(result.stats.target_met);
                assert!(!result.stats.stopped_early);
                if !zero && source == "function onTick()output.setNumber(1,2*3)end" {
                    assert_eq!(result.code, "function onTick()output.setNumber(1,6)end");
                }
                let origins = result.origins.unwrap();
                origins.validate_for_code(&result.code).unwrap();
                if result.stats.winner_structural == "target:source" {
                    assert_eq!(result.code, source);
                    assert_eq!(origins, GeneratedOrigins::identity("input.lua", source));
                }
            }
        }
        let config = CompileOptions {
            target_size: Some(8192),
            origin_source: Some("input.lua".into()),
            ..Default::default()
        };
        assert!(try_satisficing("local =", &config, None).is_err());
        assert!(
            try_satisficing("function onTick()pcall(function()end)end", &config, None).is_err()
        );
    }

    #[test]
    fn property_and_lexical_paths_are_not_mistaken_for_identity_copies() {
        let source = "function onTick()output.setNumber(1,property.getNumber('value'))end";
        let mut config = CompileOptions {
            target_size: Some(8192),
            origin_source: Some("input.lua".into()),
            property: Some(PropertyConfig {
                mode: PropertyMode::Hardcode,
                numbers: Some(std::collections::BTreeMap::from([("value".into(), 7.0)])),
                bools: None,
                texts: None,
            }),
            ..Default::default()
        };
        let result = try_satisficing(source, &config, None).unwrap().result;
        assert_ne!(result.code, source);
        assert!(!result.code.contains("getNumber"));
        let origins = result.origins.unwrap();
        origins.validate_for_code(&result.code).unwrap();
        assert_eq!(origins.unknown_bytes(), 0);
        config.property.as_mut().unwrap().mode = PropertyMode::Runtime;
        let result = try_satisficing(source, &config, None).unwrap().result;
        // Option B: the fitting source no longer wins by default; the
        // winner must still fit and its origins must validate for it.
        assert!(result.stats.target_met);
        assert!(!result.stats.stopped_early);
        let origins = result.origins.unwrap();
        origins.validate_for_code(&result.code).unwrap();
        if result.stats.winner_structural == "target:source" {
            assert_eq!(result.code, source);
            assert_eq!(origins, GeneratedOrigins::identity("input.lua", source));
        }
        config.property = None;
        let source = "local _ENV=_ENV -- not identity\nfunction onTick()output.setNumber(1,2)end";
        let result = try_satisficing(source, &config, None).unwrap().result;
        assert_eq!(result.stats.winner_structural, "lexical");
        let (expected_code, expected_origins) =
            storm_lua_syntax::print::lexical_minify_with_origins("input.lua", source).unwrap();
        assert_eq!(result.code, expected_code);
        assert_eq!(result.origins.unwrap(), expected_origins);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod shared_single_source_tests {
    use super::*;
    use std::cell::Cell;
    fn without_timings(mut value: CompileCodeResult) -> CompileCodeResult {
        // Compare all result fields except elapsed times of distinct executions.
        for pass in &mut value.passes {
            pass.elapsed_ms = None;
        }
        value
    }
    #[test]
    fn inspected_compilation_matches_the_original_entry_for_each_registered_pass_input() {
        for id in crate::pass_ids::OPTIMIZATION_PASS_IDS {
            let text = crate::provenance_pass_matrix::fixture(id);
            for target_size in [None, Some(0), Some(8192)] {
                for origin_source in [None, Some("controller.lua".into())] {
                    let options = CompileOptions {
                        target_size,
                        origin_source,
                        ..Default::default()
                    };
                    let expected = compile_code(&text, &options).unwrap();
                    let calls = Cell::new(0);
                    let actual = compile_code_with_source_inspection(
                        &text,
                        &options,
                        |ast, root, positions| {
                            calls.set(calls.get() + 1);
                            assert!(positions.get(root).is_some());
                            let (plain, plain_root) = parse_source(&text).unwrap();
                            assert_eq!(plain_root, root);
                            assert!(&plain == ast);
                        },
                    )
                    .unwrap();
                    assert_eq!(calls.get(), 1);
                    assert_eq!(
                        without_timings(actual),
                        without_timings(expected),
                        "pass={id},target={target_size:?}"
                    );
                }
            }
        }
    }
    #[test]
    fn inspected_error_and_conservative_paths_preserve_results_and_original_points() {
        for source in [
            "",
            "local =",
            "function onTick()pcall(function()end)end",
            "local _ENV=_ENV\r\nfunction onTick()output.setNumber(1,2)end",
            "-- 雪😀\r\nfunction onDraw()screen.drawText(1,1,'雪')end",
        ] {
            for target_size in [None, Some(8192)] {
                let options = CompileOptions {
                    target_size,
                    origin_source: Some("source.lua".into()),
                    ..Default::default()
                };
                let expected = compile_code(source, &options);
                let actual = compile_code_with_source_inspection(
                    source,
                    &options,
                    |ast, root, positions| {
                        let (plain, plain_root, plain_points) =
                            storm_lua_syntax::parser::parse_source_with_positions(source).unwrap();
                        assert_eq!(root, plain_root);
                        assert!(&plain == ast);
                        for node in 0..ast.nodes.len() {
                            assert_eq!(positions.get(node as u32), plain_points.get(node as u32));
                        }
                    },
                );
                match (actual, expected) {
                    (Ok(a), Ok(b)) => assert_eq!(without_timings(a), without_timings(b)),
                    (Err(a), Err(b)) => assert_eq!(a, b),
                    pair => panic!("result mismatch: {pair:?}"),
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod owned_lexical_candidate_tests {
    use super::*;
    #[test]
    fn lexical_job_transfers_its_exact_buffers_and_preserves_verifier_decisions() {
        let source = "local _ENV=_ENV -- 雪😀\r\nfunction onTick()output.setNumber(1,2)end\r\n";
        for mapped in [false, true] {
            let config = CompileOptions {
                origin_source: mapped.then(|| "lexical.lua".into()),
                ..Default::default()
            };
            let (_, mut jobs) = prepare_search(source, &config).unwrap();
            assert_eq!(jobs.len(), 1);
            let job = jobs.pop().unwrap();
            let code_pointer = job.lexical_source.as_ref().unwrap().as_ptr();
            let map_pointer = job.lexical_origins.as_ref().map(|o| o.tokens.as_ptr());
            let expected_code = job.lexical_source.as_ref().unwrap().clone();
            let expected_origins = job.lexical_origins.clone();
            let mut calls = Vec::new();
            let mut verify = |text: &str| {
                calls.push(text.to_string());
                true
            };
            let batch = evaluate_candidate_with_verifier(job, &mut Some(&mut verify)).unwrap();
            assert_eq!(calls, vec![expected_code.clone()]);
            assert_eq!(batch.attempted, 1);
            assert_eq!(batch.parse_rejected, 0);
            assert_eq!(batch.semantic_rejected, 0);
            let candidate = &batch.candidates[0];
            assert_eq!(candidate.code, expected_code);
            assert_eq!(
                candidate.code.as_ptr(),
                code_pointer,
                "consuming a job must not duplicate its completed source"
            );
            assert_eq!(
                candidate
                    .lexical_origins
                    .as_ref()
                    .map(|o| o.tokens.as_ptr()),
                map_pointer,
                "completed compact token vectors must move unchanged"
            );
            assert_eq!(
                candidate.lexical_origins.as_deref(),
                expected_origins.as_ref()
            );
            assert!(candidate.origins.is_none());
            let (_, mut jobs) = prepare_search(source, &config).unwrap();
            let mut rejected = 0;
            let mut reject = |text: &str| {
                assert_eq!(text, expected_code);
                rejected += 1;
                false
            };
            let batch =
                evaluate_candidate_with_verifier(jobs.pop().unwrap(), &mut Some(&mut reject))
                    .unwrap();
            assert_eq!(rejected, 1);
            assert!(batch.candidates.is_empty());
            assert_eq!(batch.semantic_rejected, 1);
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod load_legality_gate_tests {
    use super::*;

    // 候補S10 の真の核: ラベル後にバリア局所変数を使う形は VM も拒否する。
    const USED_BARRIER: &str = "local a = 1 goto done local b = 2 ::done:: return a + b";
    // 第210ラウンドの方言規則: ラベル後に参照されないバリアは VM が受理する。
    const UNUSED_BARRIER: &str = "local a = 1 goto done local b = 2 ::done:: return a";
    // 候補S14: 死んだ関数の `...` 誤用（洗浄前は VM が拒否する）。
    const DEADFN_VARARG: &str = "local function f() return ... end return 1";
    // 候補S14 機構B: ループ内で一度だけ呼ばれる関数の break。
    const FUSION_BREAK: &str =
        "local i = 0 while i < 1 do i = i + 1 local function f() break end f() end return i";
    // 候補S14 機構Bの vararg 版: 呼ばれた非可変長関数の `return ...`。
    const FUSION_VARARG: &str = "local function f(a) return ... end return f(1)";

    fn locals_201() -> String {
        let mut s = String::from("local function f() local ");
        s.push_str(
            &(1..=201)
                .map(|i| format!("v{i}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
        s.push_str(" = 1 return v201 end return f()");
        s
    }

    #[test]
    fn gate_rejects_vm_illegal_sources() {
        for src in [USED_BARRIER, DEADFN_VARARG, FUSION_BREAK, FUSION_VARARG] {
            assert!(
                compile_code(src, &CompileOptions::default()).is_err(),
                "expected rejection: {src}"
            );
        }
        let s = locals_201();
        assert!(
            compile_code(&s, &CompileOptions::default()).is_err(),
            "expected rejection: 201 locals"
        );
    }

    #[test]
    fn gate_accepts_vm_legal_sources() {
        for src in [
            UNUSED_BARRIER,
            "local a = 1 return a",
            "local function f() goto l local x = 5 ::l:: end return f()",
        ] {
            assert!(
                compile_code(src, &CompileOptions::default()).is_ok(),
                "expected acceptance: {src}"
            );
        }
    }

    #[test]
    fn gate_applies_on_target_path() {
        let mut options = CompileOptions::default();
        options.target_size = Some(64);
        assert!(
            compile_code(USED_BARRIER, &options).is_err(),
            "target path must reject too"
        );
        assert!(
            compile_code(UNUSED_BARRIER, &options).is_ok(),
            "target path must accept dialect-legal sources"
        );
    }
}
