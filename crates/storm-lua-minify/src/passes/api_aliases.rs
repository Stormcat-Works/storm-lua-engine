//! API alias optimization (`passes/scope-and-api.ts::optimizeApiAliases`).
//!
//! Frequently used fixed API roots and members are assigned to short globals.
//! The analytical selection is refined by the same four-round remove/add
//! search as the TypeScript oracle. Every candidate is measured as a complete
//! scope-renamed program, but only the winner is renamed. Cost-only candidates carry
//! no provenance; a tracked input transfers origins onto its selected candidate
//! instead of rebuilding the whole tree a second time.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use crate::pass::PassResult;
use crate::scope_rename::{scope_rename_fast_owned, ScopeRenameResult};
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::size::measure_size;

struct Candidate {
    ast: Ast,
    root: NodeId,
    len: usize,
}

fn increment(entries: &mut Vec<(String, usize)>, key: String) {
    if let Some((_, count)) = entries.iter_mut().find(|(name, _)| *name == key) {
        *count += 1;
    } else {
        entries.push((key, 1));
    }
}

fn count_for(entries: &[(String, usize)], key: &str) -> usize {
    entries
        .iter()
        .find_map(|(name, count)| (name == key).then_some(*count))
        .unwrap_or(0)
}

fn is_write(resolution: &Resolution, node: NodeId) -> bool {
    resolution
        .node_write
        .get(node as usize)
        .copied()
        .unwrap_or(false)
}

fn scan(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    parent: Option<(NodeId, &'static str)>,
    members: &mut Vec<(String, usize)>,
    root_uses: &mut Vec<(String, usize)>,
) {
    let builtin = (!is_write(resolution, node))
        .then(|| analyzer.resolve_builtin_reference(node))
        .flatten();
    if let Some(builtin) = builtin {
        if builtin.contains('.') {
            increment(members, builtin.clone());
            let root = builtin.split('.').next().unwrap_or_default().to_string();
            increment(root_uses, root);
        } else {
            let member_object = parent.is_some_and(|(parent_id, key)| {
                if key != "obj" {
                    return false;
                }
                matches!(ast.node(parent_id), Node::Index(_, member, _) if matches!(ast.node(*member), Node::Str(_)))
            });
            if !member_object {
                increment(root_uses, builtin);
            }
        }
    }

    storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |key, child| {
        scan(
            ast,
            resolution,
            analyzer,
            child,
            Some((node, key)),
            members,
            root_uses,
        );
    });
}

fn clone_rewriting(
    target: &mut Ast,
    source: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    aliases: &HashMap<String, String>,
    node: NodeId,
) -> NodeId {
    if !is_write(resolution, node) {
        if let Some(builtin) = analyzer.resolve_builtin_reference(node) {
            if let Some(alias) = aliases.get(&builtin) {
                let replacement = target.name(alias);
                target.nodes.derive_from(
                    replacement,
                    &source.nodes,
                    node,
                    "api-alias-optimization",
                );
                return replacement;
            }
        }
    }
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(source.node(node), &mut |child| {
        clone_rewriting(target, source, resolution, analyzer, aliases, child)
    });
    let cloned = target.push(mapped);
    // A clone, not a replacement: replaced subtrees shift later child IDs, so
    // the value-equality fast path in `derive_from` would miss almost every
    // node after the first replacement. Replaced nodes above keep `derive_from`
    // so they still record their rewrite reason.
    target
        .nodes
        .derive_from_clone(cloned, &source.nodes, node, "api-alias-optimization");
    cloned
}

fn alias_assignment(
    ast: &mut Ast,
    keys: &[String],
    aliases: &HashMap<String, String>,
    use_root_aliases: bool,
    localize: bool,
) -> NodeId {
    let first_synthetic = ast.nodes.len();
    let mut expressions = Vec::with_capacity(keys.len());
    for key in keys {
        let parts = key.split('.').collect::<Vec<_>>();
        if parts.len() == 1 {
            expressions.push(ast.name(key));
        } else {
            let root = if use_root_aliases {
                aliases
                    .get(parts[0])
                    .map(String::as_str)
                    .unwrap_or(parts[0])
            } else {
                parts[0]
            };
            let object = ast.name(root);
            let member = ast.str(&format!("\"{}\"", parts[1]));
            expressions.push(ast.index(object, member, true));
        }
    }
    // Target-size compilation spends its character budget on runtime speed:
    // a chunk-local alias reads through a register instead of the `_ENV`
    // table, while a global alias pays a table lookup on every use. The
    // local form costs the `local` keyword, so it is only ever produced by
    // the localized entry point, never by the size-minimizing one.
    let assignment = if localize {
        #[expect(
            clippy::expect_used,
            reason = "keys is the ordered selection from the already constructed aliases map, which is immutable during emission"
        )]
        let names = keys
            .iter()
            .map(|key| aliases.get(key).expect("alias exists").clone())
            .collect::<Vec<_>>();
        ast.local(names, expressions)
    } else {
        #[expect(
            clippy::expect_used,
            reason = "keys is the ordered selection from the already constructed aliases map, which is immutable during emission"
        )]
        let targets = keys
            .iter()
            .map(|key| ast.name(aliases.get(key).expect("alias exists")))
            .collect::<Vec<_>>();
        ast.assign(targets, expressions)
    };
    if ast.nodes.tracks_origins() {
        for id in first_synthetic..ast.nodes.len() {
            ast.nodes
                .mark_synthetic(id as NodeId, "api-alias-definition");
        }
    }
    assignment
}

fn apply_alias_set(
    source: &Ast,
    root: NodeId,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    selected: &[String],
    localize: bool,
) -> (Ast, NodeId) {
    let mut target = storm_lua_syntax::ast_utils::inherit_ast(source);
    let mut alias_names = HashMap::new();
    for (index, key) in selected.iter().enumerate() {
        let symbol = target.strings.fresh(&format!("__api{index}"));
        alias_names.insert(key.clone(), target.strings.get(symbol).to_string());
    }
    let body_root = clone_rewriting(
        &mut target,
        source,
        resolution,
        analyzer,
        &alias_names,
        root,
    );
    if selected.is_empty() {
        return (target, body_root);
    }
    let Node::Block(body_statements) = target.node(body_root).clone() else {
        return (target, body_root);
    };

    let root_aliases = selected
        .iter()
        .filter(|key| !key.contains('.'))
        .cloned()
        .collect::<Vec<_>>();
    let member_aliases = selected
        .iter()
        .filter(|key| key.contains('.'))
        .cloned()
        .collect::<Vec<_>>();
    let mut definitions = Vec::with_capacity(2);
    if !root_aliases.is_empty() {
        definitions.push(alias_assignment(
            &mut target,
            &root_aliases,
            &alias_names,
            false,
            localize,
        ));
    }
    if !member_aliases.is_empty() {
        definitions.push(alias_assignment(
            &mut target,
            &member_aliases,
            &alias_names,
            true,
            localize,
        ));
    }
    definitions.extend(body_statements);
    let new_root = target.block(definitions);
    target
        .nodes
        .derive_from(new_root, &source.nodes, root, "api-alias-optimization");
    (target, new_root)
}

fn make_candidate(
    source: &Ast,
    root: NodeId,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    selected: &[String],
    localize: bool,
    chunk_locals: usize,
) -> Candidate {
    let (ast, root) = apply_alias_set(source, root, resolution, analyzer, selected, localize);
    // Trial comparison needs the exact renamed size, not a renamed AST. Keep
    // the selected unrenamed tree and materialize its winning name plan once.
    // A localized candidate whose alias locals would push the chunk past
    // the active-local limit is infeasible (the VM refuses to load the
    // result), so it measures as infinitely long and can never win. The
    // empty set adds no locals and is always feasible -- even for a
    // source already at or over the limit, where the pass must not add
    // to the problem but otherwise behaves as before -- so selection
    // still terminates on a loadable-or-original candidate.
    let len = if localize
        && !selected.is_empty()
        && chunk_locals + selected.len() > super::single_member_cache::MAX_LOCALS
    {
        usize::MAX
    } else {
        crate::scope_rename::measure_renamed_size(&ast, root)
    };
    Candidate { ast, root, len }
}

/// Measure one candidate set for the selection search in
/// [`run_with_analysis`], memoizing the measured length per exact
/// ordered set for the duration of the run.
///
/// The length is a deterministic pure function of
/// `(source, root, selected, localize, chunk_locals)`: candidate
/// construction and renamed-size measurement only read those inputs.
/// The search's acceptance decisions depend only on lengths, and the
/// incumbent length never grows, so a set rejected once can never be
/// accepted later; rebuilding its tree for a later comparison would
/// reproduce the same length and the same rejection. The returned
/// candidate is `Some` exactly when this call constructed the tree.
/// When a memoized set is nevertheless selected (the final empty-set
/// check, or a tie against the initial set), the caller reconstructs
/// it with [`make_candidate`], which yields the identical tree.
///
/// A localized set that the chunk local budget already proves
/// infeasible measures as infinitely long by definition (see
/// `make_candidate`), so it is memoized as such without construction.
/// It could only ever be selected while every alternative is also
/// infeasible, which the always-feasible empty set rules out.
///
/// The empty set is measured without construction as well: applying
/// an empty selection is a pure structural clone (with an empty alias
/// map `clone_rewriting` replaces nothing, no definitions are
/// emitted, and the interner is inherited unchanged), so the empty
/// candidate's renamed size is the source's own renamed size. A
/// caller that selects the empty set reconstructs the identical tree
/// with `make_candidate`, the same path already used for memoized
/// sets. The identity is pinned by the
/// `empty_candidate_length_equals_source_renamed_size` test.
#[allow(clippy::too_many_arguments)]
fn measure_candidate(
    cache: &mut HashMap<Vec<String>, usize>,
    source: &Ast,
    root: NodeId,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    selected: &[String],
    localize: bool,
    chunk_locals: usize,
) -> (usize, Option<Candidate>) {
    if let Some(&len) = cache.get(selected) {
        return (len, None);
    }
    if selected.is_empty() {
        let len = crate::scope_rename::measure_renamed_size(source, root);
        cache.insert(selected.to_vec(), len);
        return (len, None);
    }
    if localize
        && !selected.is_empty()
        && chunk_locals + selected.len() > super::single_member_cache::MAX_LOCALS
    {
        cache.insert(selected.to_vec(), usize::MAX);
        return (usize::MAX, None);
    }
    let candidate = make_candidate(
        source,
        root,
        resolution,
        analyzer,
        selected,
        localize,
        chunk_locals,
    );
    cache.insert(selected.to_vec(), candidate.len);
    (candidate.len, Some(candidate))
}

fn collect_subtree_nodes(target: &Ast, node: NodeId, out: &mut Vec<NodeId>) {
    out.push(node);
    let mut children = Vec::new();
    storm_lua_syntax::ast_utils::for_each_child(target, node, &mut |child| {
        children.push(child);
    });
    for child in children {
        collect_subtree_nodes(target, child, out);
    }
}

fn mark_subtree_synthetic(target: &mut Ast, node: NodeId) {
    target.nodes.mark_synthetic(node, "api-alias-definition");
    let mut children = Vec::new();
    storm_lua_syntax::ast_utils::for_each_child(target, node, &mut |child| {
        children.push(child);
    });
    for child in children {
        mark_subtree_synthetic(target, child);
    }
}

/// A candidate node is an alias replacement exactly when it is a Name
/// while its source counterpart is a different construct or a Name with
/// another symbol: clones preserve kind and symbol, and alias symbols are
/// fresh names absent from the source. This avoids resolving every builtin
/// reference (which allocates its name) a second time for the walk.
fn is_alias_replacement(target: &Ast, node: NodeId, source: &Ast, original: NodeId) -> bool {
    let Node::Name(symbol) = target.node(node) else {
        return false;
    };
    match source.node(original) {
        Node::Name(original_symbol) => symbol != original_symbol,
        _ => true,
    }
}

/// Fill origin slots on a winning cost-only candidate so it matches the
/// replay `apply_alias_set` would have produced from the tracked input:
/// replaced builtins record their rewrite reason, cloned nodes share their
/// source slot, and nothing else is visited. The candidate's values and
/// node numbering already equal the replay's, so only slots are written.
/// Iterative with reused child buffers: a recursive walk would allocate
/// two child vectors per node, which costs more than the replay it replaces.
/// Reusable walk state for origin transfer: one stack plus one child
/// buffer per side, shared across every root pair of a single transfer.
struct TransferBuffers {
    stack: Vec<(NodeId, NodeId)>,
    target_children: Vec<NodeId>,
    source_children: Vec<NodeId>,
}

fn transfer_alias_origins(
    target: &mut Ast,
    source: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    selected: &[String],
    root: NodeId,
    original_root: NodeId,
) {
    let mut buffers = TransferBuffers {
        stack: vec![(root, original_root)],
        target_children: Vec::new(),
        source_children: Vec::new(),
    };
    let code: Arc<str> = Arc::from("api-alias-optimization");
    transfer_alias_origins_stack(
        target,
        source,
        resolution,
        analyzer,
        selected,
        &mut buffers,
        &code,
    );
}

/// Stack-driven walk with caller-owned buffers, so several root pairs
/// (for example every body statement) can share one set of buffers.
fn transfer_alias_origins_stack(
    target: &mut Ast,
    source: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    selected: &[String],
    buffers: &mut TransferBuffers,
    code: &Arc<str>,
) {
    let TransferBuffers {
        stack,
        target_children,
        source_children,
    } = buffers;
    while let Some((node, original)) = stack.pop() {
        let replaced = is_alias_replacement(target, node, source, original);
        debug_assert_eq!(
            replaced,
            !is_write(resolution, original)
                && analyzer
                    .resolve_builtin_reference(original)
                    .is_some_and(|builtin| contains(selected, &builtin)),
            "api-alias origin transfer replacement mismatch"
        );
        if replaced {
            // One shared code string for every replaced node: `derive_from`
            // would allocate a fresh code `Arc<str>` per node, while the
            // recorded origin is identical either way.
            target
                .nodes
                .derive_from_with_code(node, &source.nodes, original, code);
            continue;
        }
        target
            .nodes
            .derive_from_clone(node, &source.nodes, original, "api-alias-optimization");
        target_children.clear();
        storm_lua_syntax::ast_utils::for_each_child(target, node, &mut |child| {
            target_children.push(child);
        });
        source_children.clear();
        storm_lua_syntax::ast_utils::for_each_child(source, original, &mut |child| {
            source_children.push(child);
        });
        debug_assert_eq!(
            target_children.len(),
            source_children.len(),
            "api-alias origin transfer child mismatch"
        );
        for (child, original_child) in target_children.iter().zip(source_children.iter()).rev() {
            stack.push((*child, *original_child));
        }
    }
}

fn profitable(key: &str, count: usize) -> bool {
    count * key.len().saturating_sub(1) > key.len() + 3
}

fn contains(selected: &[String], key: &str) -> bool {
    selected.iter().any(|current| current == key)
}

pub fn optimize_api_aliases(ast: &mut Ast, root: NodeId) -> PassResult {
    optimize_api_aliases_with_localization(ast, root, false)
}

/// Target-size variant of [`optimize_api_aliases`]: alias definitions are
/// emitted as chunk-local declarations instead of global assignments.
/// Selection is rerun under local costs, so the chosen set may shrink
/// relative to the global variant; that is the correct local-cost optimum.
/// Only target-size compilation may call this: target-free compilation must
/// keep minimizing size, and the local form is longer.
pub fn optimize_api_aliases_localized(ast: &mut Ast, root: NodeId) -> PassResult {
    optimize_api_aliases_with_localization(ast, root, true)
}

/// The first analysis of the alias passes: the origin-less source clone's
/// resolution, effect analysis, and builtin-use counts. Every value here
/// is a deterministic pure function of `(source, root)` — resolution and
/// effect analysis only read the tree, and the scan only counts — so one
/// analysis serves any number of pass runs over the same source tree.
/// Target-size compilation runs the global and localized variants over
/// the same phase tree; sharing this analysis between them (via
/// [`optimize_api_aliases_with_analysis`] and
/// [`optimize_api_aliases_localized_with_analysis`]) removes one full
/// build per layout pair without changing any selection or output.
///
/// The analyzer borrows the caller's `source`/`resolution` (kept as
/// separate owned values next to this struct), never this struct's own
/// fields, so the struct is not self-referential.
pub struct ApiAliasAnalysis<'a> {
    source: &'a Ast,
    resolution: &'a Resolution,
    analyzer: EffectAnalyzer<'a>,
    members: Vec<(String, usize)>,
    root_uses: Vec<(String, usize)>,
}

impl<'a> ApiAliasAnalysis<'a> {
    /// Build the analysis for `root` in the origin-less `source` tree
    /// (see [`storm_lua_syntax::ast_utils::clone_without_origins`]).
    pub fn new(source: &'a Ast, resolution: &'a Resolution, root: NodeId) -> Self {
        let analyzer = EffectAnalyzer::new(source, resolution, root, true);
        let mut members = Vec::new();
        let mut root_uses = Vec::new();
        scan(
            source,
            resolution,
            &analyzer,
            root,
            None,
            &mut members,
            &mut root_uses,
        );
        Self {
            source,
            resolution,
            analyzer,
            members,
            root_uses,
        }
    }
}

/// [`optimize_api_aliases`] with a caller-built shared analysis. The
/// analysis must have been built from the origin-less clone of the same
/// tree `ast` holds (identical node/symbol tables); the result is then
/// identical to the self-contained entry point by construction.
pub fn optimize_api_aliases_with_analysis(
    ast: &mut Ast,
    root: NodeId,
    analysis: &ApiAliasAnalysis<'_>,
) -> PassResult {
    run_with_analysis(ast, root, false, analysis)
}

/// [`optimize_api_aliases_localized`] with a caller-built shared
/// analysis; see [`optimize_api_aliases_with_analysis`].
pub fn optimize_api_aliases_localized_with_analysis(
    ast: &mut Ast,
    root: NodeId,
    analysis: &ApiAliasAnalysis<'_>,
) -> PassResult {
    run_with_analysis(ast, root, true, analysis)
}

fn optimize_api_aliases_with_localization(
    ast: &mut Ast,
    root: NodeId,
    localize: bool,
) -> PassResult {
    // Candidate selection depends only on Lua syntax and binding/effect analysis.
    // Keep its NodeId/SymbolId space but do not construct source explanations for
    // candidates which are immediately discarded by the size comparison.
    let source = storm_lua_syntax::ast_utils::clone_without_origins(ast);
    let resolution = resolve(&source, root);
    let analysis = ApiAliasAnalysis::new(&source, &resolution, root);
    run_with_analysis(ast, root, localize, &analysis)
}

fn run_with_analysis(
    ast: &mut Ast,
    root: NodeId,
    localize: bool,
    analysis: &ApiAliasAnalysis<'_>,
) -> PassResult {
    let source = analysis.source;
    let resolution = analysis.resolution;
    let analyzer = &analysis.analyzer;
    let members = &analysis.members;
    let root_uses = &analysis.root_uses;

    let mut selected = Vec::new();
    for (key, count) in members {
        if profitable(key, *count) {
            selected.push(key.clone());
        }
    }
    for (key, count) in root_uses {
        if profitable(key, *count) && !contains(&selected, key) {
            selected.push(key.clone());
        }
    }

    // Cycle 95 (local-budget class, api-alias form): the localized
    // variant declares one chunk local per selected key at the root,
    // live for the whole chunk, so a candidate set is loadable only
    // when the chunk's existing active-local maximum plus the alias
    // count stays within the limit. The accounting mirrors the
    // member-cache passes (shared `max_active_locals` whole-function
    // maximum plus the inserted count). The global variant declares
    // no locals and needs no budget.
    let chunk_locals = if localize {
        super::single_member_cache::max_active_locals(source, 0, root)
    } else {
        0
    };

    let mut lens: HashMap<Vec<String>, usize> = HashMap::new();
    let (mut best_len, mut best) = measure_candidate(
        &mut lens,
        source,
        root,
        resolution,
        analyzer,
        &selected,
        localize,
        chunk_locals,
    );
    let mut changed = true;
    for _ in 0..4 {
        if !changed {
            break;
        }
        changed = false;
        for key in selected.clone() {
            let trial = selected
                .iter()
                .filter(|current| **current != key)
                .cloned()
                .collect::<Vec<_>>();
            let (len, candidate) = measure_candidate(
                &mut lens,
                source,
                root,
                resolution,
                analyzer,
                &trial,
                localize,
                chunk_locals,
            );
            if len <= best_len {
                best = Some(candidate.unwrap_or_else(|| {
                    make_candidate(
                        source,
                        root,
                        resolution,
                        analyzer,
                        &trial,
                        localize,
                        chunk_locals,
                    )
                }));
                best_len = len;
                selected = trial;
                changed = true;
            }
        }

        let mut excluded = members
            .iter()
            .map(|(key, _)| key.clone())
            .chain(root_uses.iter().map(|(key, _)| key.clone()))
            .filter(|key| !contains(&selected, key))
            .collect::<Vec<_>>();
        excluded.sort_by(|left, right| {
            let left_count = count_for(members, left).max(count_for(root_uses, left));
            let right_count = count_for(members, right).max(count_for(root_uses, right));
            right_count.cmp(&left_count).then(Ordering::Equal)
        });
        excluded.truncate(8);
        for key in excluded {
            let mut trial = selected.clone();
            trial.push(key.clone());
            let (len, candidate) = measure_candidate(
                &mut lens,
                source,
                root,
                resolution,
                analyzer,
                &trial,
                localize,
                chunk_locals,
            );
            if len < best_len {
                best = Some(candidate.unwrap_or_else(|| {
                    make_candidate(
                        source,
                        root,
                        resolution,
                        analyzer,
                        &trial,
                        localize,
                        chunk_locals,
                    )
                }));
                best_len = len;
                selected.push(key);
                changed = true;
            }
        }
    }

    let (none_len, none) = measure_candidate(
        &mut lens,
        source,
        root,
        resolution,
        analyzer,
        &[],
        localize,
        chunk_locals,
    );
    if none_len < best_len {
        selected.clear();
        best = none;
        best_len = none_len;
    }
    // `best` is `None` only for a budget-infeasible incumbent measured
    // without construction; the empty set is always feasible and
    // strictly shorter than an infeasible length, so it has already
    // replaced any such incumbent above. Reconstruct defensively
    // rather than rely on that argument at the use site.
    let mut best = match best {
        Some(candidate) => candidate,
        None => make_candidate(
            source,
            root,
            resolution,
            analyzer,
            &selected,
            localize,
            chunk_locals,
        ),
    };
    debug_assert_eq!(best.len, best_len);

    if ast.nodes.tracks_origins() {
        // Selection used the same immutable syntax/IDs. Transfer origins onto
        // the selected candidate instead of rebuilding it from the tracked
        // input; losing alternatives never need origins. The candidate's
        // values and node numbering already match a fresh replay, so only
        // its origin slots are filled here.
        let mut winner = best.ast;
        winner.nodes.attach_origins_like(&ast.nodes);
        if !selected.is_empty() && matches!(ast.node(root), Node::Block(_)) {
            let root_aliases = selected.iter().filter(|key| !key.contains('.')).count();
            let member_aliases = selected.iter().filter(|key| key.contains('.')).count();
            let definitions = usize::from(root_aliases > 0) + usize::from(member_aliases > 0);
            let mut children = Vec::new();
            storm_lua_syntax::ast_utils::for_each_child(&winner, best.root, &mut |child| {
                children.push(child);
            });
            let mut source_children = Vec::new();
            storm_lua_syntax::ast_utils::for_each_child(ast, root, &mut |child| {
                source_children.push(child);
            });
            debug_assert_eq!(
                children.len(),
                definitions + source_children.len(),
                "api-alias origin transfer root mismatch"
            );
            for child in children.iter().take(definitions) {
                mark_subtree_synthetic(&mut winner, *child);
            }
            // The cloned body block itself is orphaned by the new root
            // (which inlines its statements), but the replay still
            // attributed it. It is the last body node pushed, directly
            // before the first definition node.
            let mut definition_nodes = Vec::new();
            for child in children.iter().take(definitions) {
                collect_subtree_nodes(&winner, *child, &mut definition_nodes);
            }
            if let Some(first) = definition_nodes.iter().min() {
                let body_block = first - 1;
                debug_assert!(matches!(winner.node(body_block), Node::Block(_)));
                winner.nodes.derive_from_clone(
                    body_block,
                    &ast.nodes,
                    root,
                    "api-alias-optimization",
                );
            }
            let mut buffers = TransferBuffers {
                stack: Vec::new(),
                target_children: Vec::new(),
                source_children: Vec::new(),
            };
            for (child, original) in children.into_iter().skip(definitions).zip(source_children) {
                buffers.stack.push((child, original));
            }
            // One shared code string for the transfer and the root
            // derivation, as in `transfer_alias_origins`.
            let alias_code: Arc<str> = Arc::from("api-alias-optimization");
            transfer_alias_origins_stack(
                &mut winner,
                ast,
                resolution,
                analyzer,
                &selected,
                &mut buffers,
                &alias_code,
            );
            // Last, as in the replay: the new root's detachment scan reads
            // the already populated body and definition slots.
            winner
                .nodes
                .derive_from_with_code(best.root, &ast.nodes, root, &alias_code);
        } else {
            transfer_alias_origins(
                &mut winner,
                ast,
                resolution,
                analyzer,
                &selected,
                best.root,
                root,
            );
        }
        best.ast = winner;
    }
    let ScopeRenameResult {
        ast: renamed,
        root: renamed_root,
        ..
    } = scope_rename_fast_owned(best.ast, best.root);
    debug_assert_eq!(
        measure_size(&renamed, renamed_root),
        best.len,
        "cost-only rename changed selected size"
    );
    *ast = renamed;

    PassResult {
        root: renamed_root,
        saved: Some(none_len.saturating_sub(best.len) as u64),
        details: Some(selected),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn output(source: &str) -> (String, Vec<String>) {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = optimize_api_aliases(&mut ast, root);
        (
            Printer::new(&ast, false).output(result.root),
            result.details.unwrap_or_default(),
        )
    }

    /// Chunk shape at the active-local boundary (w6 cycle 51,
    /// v16-b8a/b8b): `n` chunk locals reading `input.getNumber`,
    /// a sum local, and a numeric-for (four slots in the shared
    /// accounting), so the chunk maximum is `n + 5`.
    fn at_limit_source(n: usize) -> String {
        let mut src = String::from("A = screen\n");
        for i in 1..=n {
            src.push_str(&format!("local c{i} = input.getNumber(1)\n"));
        }
        src.push_str("local s = 0\nfor i = 1, 2 do A.drawRectF(1, 2, 3, 4) s = s + i end\nR = s");
        for i in 1..=n {
            src.push_str(&format!(" + c{i}"));
        }
        src.push_str("\nfunction onTick() output.setNumber(1, R) end\n");
        src
    }

    fn localized(source: &str) -> (String, Vec<String>) {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = optimize_api_aliases_localized(&mut ast, root);
        (
            Printer::new(&ast, false).output(result.root),
            result.details.unwrap_or_default(),
        )
    }

    #[test]
    fn method_self_member_reads_are_not_aliased() {
        // `self` inside a method body is the implicit parameter, not the
        // global of the same name. Aliasing `self.n` hoisted the member
        // read to the chunk head, where `self` is nil, breaking every
        // method that read it (candidate X, w9 cycle 67). Both reads must
        // stay inside their method bodies.
        let source = "local t = {}\nfunction t:m() local v = (self.n or 0) return v end\nfunction t:k() local w = (self.n or 0) return w end\noutput.setNumber(1, t:m())\n";
        let (out, _details) = output(source);
        assert_eq!(out.matches("self.n").count(), 2, "{out}");
    }

    #[test]
    fn localized_aliases_skip_when_chunk_at_local_limit() {
        // Chunk maximum 200: any alias local would make the
        // generated code unloadable, so the localized variant
        // must select nothing (w6 cycle 51, v16-b8b form).
        let source = at_limit_source(195);
        let (_code, aliases) = localized(&source);
        assert!(aliases.is_empty(), "{aliases:?}");
        // The global variant declares no locals and still fires
        // on the same source: the budget is localized-only.
        let (_code, aliases) = output(&source);
        assert!(!aliases.is_empty(), "{aliases:?}");
    }

    #[test]
    fn empty_candidate_length_equals_source_renamed_size() {
        // Pin the identity the empty-set shortcut in measure_candidate
        // relies on: an empty apply_alias_set is a pure structural
        // clone, so the constructed candidate measures exactly the
        // source's renamed size, in both variants. If the clone path
        // ever changes (a rewrite that fires with an empty alias map,
        // a definition emitted for the empty set, an interner change),
        // this fails instead of silently skewing selection.
        let sources = [
            "function onDraw()screen.drawText(1,1,\"a\")screen.drawText(2,2,\"b\")screen.drawText(3,3,\"c\")screen.drawText(4,4,\"d\")end",
            "local x = input.getNumber(1) output.setNumber(1, x)",
            "function onTick() end",
            "A = screen for i = 1, 3 do A.drawRectF(i, 2, 3, 4) end",
        ];
        for source in sources {
            let (ast, root) = parse_source(source).expect("parse");
            let originless = storm_lua_syntax::ast_utils::clone_without_origins(&ast);
            let resolution = resolve(&originless, root);
            let analysis = ApiAliasAnalysis::new(&originless, &resolution, root);
            let direct = crate::scope_rename::measure_renamed_size(&originless, root);
            for localize in [false, true] {
                let candidate = make_candidate(
                    &originless,
                    root,
                    &resolution,
                    &analysis.analyzer,
                    &[],
                    localize,
                    0,
                );
                assert_eq!(candidate.len, direct, "{source} localize={localize}");
            }
        }
    }

    #[test]
    fn empty_selection_reconstructs_a_plain_renamed_tree() {
        // Nothing here is profitable to alias (a single short member
        // use), so the initial selection is already empty and the
        // winner arrives through the reconstruction path. The result
        // must be the plain renamed tree: no alias definitions, no
        // saved characters.
        let source = "function onTick() output.setNumber(1, 2) end";
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = optimize_api_aliases(&mut ast, root);
        assert_eq!(result.details, Some(Vec::new()));
        assert_eq!(result.saved, Some(0));
        let code = Printer::new(&ast, false).output(result.root);
        assert!(code.contains("output.setNumber"), "{code}");
        assert!(!code.contains("__api"), "{code}");
    }

    #[test]
    fn localized_aliases_fire_with_exactly_one_slot() {
        // Chunk maximum 199: exactly one alias local fits.
        let source = at_limit_source(194);
        let (_code, aliases) = localized(&source);
        assert_eq!(aliases.len(), 1, "{aliases:?}");
    }

    #[test]
    fn localized_aliases_fire_well_below_limit() {
        // Positive control: far from the limit the profitable
        // member alias is selected as before.
        let source = at_limit_source(150);
        let (_code, aliases) = localized(&source);
        assert!(
            aliases.iter().any(|key| key == "input.getNumber"),
            "{aliases:?}"
        );
    }

    #[test]
    fn localized_aliases_do_not_add_when_source_over_limit() {
        // A source already over the limit (maximum 201) is not
        // this pass's to fix, but the pass must not add locals
        // (and must not distort selection): it selects nothing.
        let source = at_limit_source(196);
        let (_code, aliases) = localized(&source);
        assert!(aliases.is_empty(), "{aliases:?}");
    }

    #[test]
    fn aliases_frequently_used_api_member() {
        let source = "function onDraw()screen.drawText(1,1,\"a\")screen.drawText(2,2,\"b\")screen.drawText(3,3,\"c\")screen.drawText(4,4,\"d\")end";
        let (code, aliases) = output(source);
        assert!(
            aliases.iter().any(|key| key == "screen.drawText"),
            "{aliases:?} {code}"
        );
        assert_eq!(code.matches("screen.drawText").count(), 1, "{code}");
    }

    #[test]
    fn localized_aliases_emit_chunk_locals() {
        let source = "function onDraw()screen.drawText(1,1,\"a\")screen.drawText(2,2,\"b\")screen.drawText(3,3,\"c\")screen.drawText(4,4,\"d\")screen.drawText(5,5,\"e\")screen.drawText(6,6,\"f\")end";
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = optimize_api_aliases_localized(&mut ast, root);
        let aliases = result.details.unwrap_or_default();
        assert!(!aliases.is_empty(), "expected aliases to be selected");
        let code = Printer::new(&ast, false).output(result.root);
        // The alias definitions are chunk-local declarations, not globals.
        assert!(
            code.starts_with("local "),
            "localized output must start with a local alias definition: {code}"
        );
        assert!(
            !code.starts_with("local function"),
            "first statement must be the alias definition, not a function: {code}"
        );
        let (reparsed, _) = parse_source(&code).expect("localized output reparses");
        let _ = reparsed;
    }

    #[test]
    fn shadowed_api_root_is_not_aliased() {
        let source = "function onTick()local screen={drawText=function()end}screen.drawText()screen.drawText()screen.drawText()end";
        let (code, aliases) = output(source);
        assert!(aliases.is_empty(), "{aliases:?} {code}");
    }

    #[test]
    fn aliases_root_when_that_layout_is_shorter() {
        let source = "function onTick()output.setNumber(1,1)output.setNumber(2,2)output.setNumber(3,3)output.setBool(1,true)output.setBool(2,false)output.setBool(3,true)end";
        let (code, aliases) = output(source);
        assert!(
            aliases.iter().any(|key| key == "output"),
            "{aliases:?} {code}"
        );
    }

    /// The shared-analysis entry points must reproduce the self-contained
    /// ones exactly: same printed code, same selected aliases, same saved
    /// count, for both the global and the localized variant. The analysis
    /// is built from a deep clone of the input tree (as search does when
    /// it shares one analysis between the two layout calls), which also
    /// pins the clone-equality the sharing relies on.
    #[test]
    fn shared_analysis_entries_match_self_contained() {
        let sources = [
            "function onDraw()screen.drawText(1,1,\"a\")screen.drawText(2,2,\"b\")screen.drawText(3,3,\"c\")screen.drawText(4,4,\"d\")screen.drawText(5,5,\"e\")screen.drawText(6,6,\"f\")end",
            "function onTick()output.setNumber(1,1)output.setNumber(2,2)output.setNumber(3,3)output.setBool(1,true)output.setBool(2,false)output.setBool(3,true)input.getNumber(1)input.getNumber(2)input.getNumber(3)input.getNumber(4)end",
            "function onTick()local screen={drawText=function()end}screen.drawText()screen.drawText()screen.drawText()end",
            "function onTick()local x=1 end",
            "function onDraw()for i=1,4 do screen.drawRectF(i,i,2,2)screen.drawLine(i,i,i+1,i+1)property.setNumber(\"n\",i)end end",
        ];
        for source in sources {
            for localize in [false, true] {
                let (mut base_ast, root) = parse_source(source).expect("parse");
                let expected = if localize {
                    optimize_api_aliases_localized(&mut base_ast, root)
                } else {
                    optimize_api_aliases(&mut base_ast, root)
                };
                let expected_code = Printer::new(&base_ast, false).output(expected.root);

                let (shared_ast, shared_root) = parse_source(source).expect("parse");
                let analysis_source =
                    storm_lua_syntax::ast_utils::clone_without_origins(&shared_ast);
                let analysis_resolution =
                    storm_lua_analysis::resolver::resolve(&analysis_source, shared_root);
                let analysis =
                    ApiAliasAnalysis::new(&analysis_source, &analysis_resolution, shared_root);
                // The pass runs on a deep clone of the analyzed tree, the
                // exact shape of the search-side sharing.
                let mut run_ast = shared_ast.clone();
                let actual = if localize {
                    optimize_api_aliases_localized_with_analysis(
                        &mut run_ast,
                        shared_root,
                        &analysis,
                    )
                } else {
                    optimize_api_aliases_with_analysis(&mut run_ast, shared_root, &analysis)
                };
                let actual_code = Printer::new(&run_ast, false).output(actual.root);
                assert_eq!(
                    actual_code, expected_code,
                    "code differs ({localize}): {source}"
                );
                assert_eq!(
                    actual.details, expected.details,
                    "aliases differ ({localize})"
                );
                assert_eq!(actual.saved, expected.saved, "saved differs ({localize})");
                let _ = shared_ast;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod trial_provenance_tests {
    use super::*;
    use crate::scope_rename::scope_rename_fast;
    use storm_lua_syntax::parser::parse_source_with_origins;
    use storm_lua_syntax::provenance::GeneratedOrigins;
    use storm_lua_syntax::Printer;

    // Frozen pre-change search: constructs full tracked candidates at every
    // comparison. Keep this independent of the new selection/replay branch.
    fn reference_candidate(
        source: &Ast,
        root: NodeId,
        resolution: &Resolution,
        analyzer: &EffectAnalyzer<'_>,
        selected: &[String],
    ) -> Candidate {
        let (aliased, aliased_root) =
            apply_alias_set(source, root, resolution, analyzer, selected, false);
        let ScopeRenameResult {
            ast,
            root: renamed_root,
            ..
        } = scope_rename_fast(&aliased, aliased_root);
        let len = measure_size(&ast, renamed_root);
        Candidate {
            ast,
            root: renamed_root,
            len,
        }
    }

    fn reference_aliases(ast: &mut Ast, root: NodeId) -> PassResult {
        let source = {
            let mut cloned = storm_lua_syntax::ast_utils::inherit_ast(ast);
            cloned.nodes = ast.nodes.clone();
            cloned
        };
        let resolution = resolve(&source, root);
        let analyzer = EffectAnalyzer::new(&source, &resolution, root, true);
        let mut members = Vec::new();
        let mut root_uses = Vec::new();
        scan(
            &source,
            &resolution,
            &analyzer,
            root,
            None,
            &mut members,
            &mut root_uses,
        );

        let mut selected = Vec::new();
        for (key, count) in &members {
            if profitable(key, *count) {
                selected.push(key.clone());
            }
        }
        for (key, count) in &root_uses {
            if profitable(key, *count) && !contains(&selected, key) {
                selected.push(key.clone());
            }
        }

        let mut best = reference_candidate(&source, root, &resolution, &analyzer, &selected);
        let mut changed = true;
        for _ in 0..4 {
            if !changed {
                break;
            }
            changed = false;
            for key in selected.clone() {
                let trial = selected
                    .iter()
                    .filter(|current| **current != key)
                    .cloned()
                    .collect::<Vec<_>>();
                let candidate = reference_candidate(&source, root, &resolution, &analyzer, &trial);
                if candidate.len <= best.len {
                    selected = trial;
                    best = candidate;
                    changed = true;
                }
            }

            let mut excluded = members
                .iter()
                .map(|(key, _)| key.clone())
                .chain(root_uses.iter().map(|(key, _)| key.clone()))
                .filter(|key| !contains(&selected, key))
                .collect::<Vec<_>>();
            excluded.sort_by(|left, right| {
                let left_count = count_for(&members, left).max(count_for(&root_uses, left));
                let right_count = count_for(&members, right).max(count_for(&root_uses, right));
                right_count.cmp(&left_count).then(Ordering::Equal)
            });
            excluded.truncate(8);
            for key in excluded {
                let mut trial = selected.clone();
                trial.push(key.clone());
                let candidate = reference_candidate(&source, root, &resolution, &analyzer, &trial);
                if candidate.len < best.len {
                    selected.push(key);
                    best = candidate;
                    changed = true;
                }
            }
        }

        let none = reference_candidate(&source, root, &resolution, &analyzer, &[]);
        let none_len = none.len;
        if none.len < best.len {
            selected.clear();
            best = none;
        }

        *ast = best.ast;
        PassResult {
            root: best.root,
            saved: Some(none_len.saturating_sub(best.len) as u64),
            details: Some(selected),
        }
    }

    fn compare(input: &Ast, root: NodeId) {
        let original = serde_json::to_vec(input).unwrap();
        let mut expected = input.clone();
        let mut actual = input.clone();
        let old = reference_aliases(&mut expected, root);
        let new = optimize_api_aliases(&mut actual, root);
        assert_eq!(new.root, old.root, "node ordering changed");
        assert_eq!(new.saved, old.saved, "candidate cost changed");
        assert_eq!(new.details, old.details, "ordered alias selection changed");
        assert_eq!(
            serde_json::to_value(&actual).unwrap(),
            serde_json::to_value(&expected).unwrap(),
            "AST/origin/reason snapshots differ"
        );
        assert_eq!(
            serde_json::to_vec(input).unwrap(),
            original,
            "input snapshot mutated"
        );
        for newlines in [false, true] {
            let a = Printer::new(&actual, newlines).output_with_positions(new.root);
            let b = Printer::new(&expected, newlines).output_with_positions(old.root);
            assert_eq!(a.code, b.code);
            let after = GeneratedOrigins::from_print(&actual, &a);
            let before = GeneratedOrigins::from_print(&expected, &b);
            assert_eq!(after, before, "complete generated provenance differs");
            if let Some(origins) = after {
                origins.validate_for_code(&a.code).unwrap();
            }
        }
    }

    #[test]
    fn tracked_and_plain_candidates_match_the_prior_search_on_all_registered_pass_inputs() {
        for id in crate::pass_ids::OPTIMIZATION_PASS_IDS {
            let source = crate::provenance_pass_matrix::fixture(id);
            let (ast, root) = parse_source_with_origins("pass.lua", &source).unwrap();
            compare(&ast, root);
            let mut transformed = ast.clone();
            let transformed_root = crate::provenance_pass_matrix::apply(id, &mut transformed, root);
            compare(&transformed, transformed_root);
            compare(
                &storm_lua_syntax::ast_utils::clone_without_origins(&transformed),
                transformed_root,
            );
            let restored: Ast =
                serde_json::from_slice(&serde_json::to_vec(&transformed).unwrap()).unwrap();
            compare(&restored, transformed_root);
        }
    }

    #[test]
    fn alias_replay_preserves_shadowing_ties_unicode_and_incomplete_origins() {
        let cases=[
            "",
            "function onTick()local long=input.getNumber(1) output.setNumber(1,long) output.setNumber(2,long)end",
            "local __api0=7 function onTick()local a=__api0 output.setNumber(1,a)output.setNumber(2,a)output.setBool(1,true)output.setBool(2,false)end",
            "function onTick()local output={setNumber=function()end} output.setNumber(1,1)output.setNumber(2,2)end",
            "local set=output.setNumber function onTick()set(1,1)set(2,2)set(3,3)end",
            "function onTick()local x={input.getNumber(1),input.getNumber(2)}for i,v in ipairs(x)do output.setNumber(i,v)end end",
            "-- 雪😀\r\nfunction onDraw() screen.drawText(1,2,'雪') screen.drawText(2,3,'😀') screen.drawText(3,4,'雪')end\r\n",
            "function onTick()output['setNumber'](1,1) output.setNumber(2,2) output.setNumber(3,3)end",
            "function onTick() local f=function(output)return output.setNumber(1,2)end output.setNumber(1,f({setNumber=function()return 3 end}))end",
            "function onTick() output.setNumber=function()end output.setNumber(1,2)end",
        ];
        for source in cases {
            let (ast, root) = parse_source_with_origins("controller.lua", source).unwrap();
            compare(&ast, root);
            let mut incomplete = ast.clone();
            // Deliberate raw writes invalidate attribution while retaining syntax.
            for node in &mut incomplete.nodes {
                let _ = node;
            }
            compare(&incomplete, root);
            let first = optimize_api_aliases(&mut incomplete, root);
            compare(&incomplete, first.root);
        }
    }

    #[test]
    fn map_output_is_retained_for_empty_alias_sets_and_repeated_member_candidates() {
        for repetitions in [1, 2, 3, 8, 20] {
            let body=(0..repetitions).map(|n|format!("output.setNumber({n},input.getNumber({n})) output.setBool({n},input.getBool({n})) ")).collect::<String>();
            let (ast, root) =
                parse_source_with_origins("candidate.lua", &format!("function onTick(){body}end"))
                    .unwrap();
            compare(&ast, root);
            let untracked = storm_lua_syntax::ast_utils::clone_without_origins(&ast);
            compare(&untracked, root);
            let mut result = ast.clone();
            let output = optimize_api_aliases(&mut result, root);
            let printed = Printer::new(&result, false).output_with_positions(output.root);
            assert_eq!(
                GeneratedOrigins::from_print(&result, &printed)
                    .unwrap()
                    .unknown_bytes(),
                0
            );
        }
    }
}
