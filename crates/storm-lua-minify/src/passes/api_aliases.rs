//! API alias optimization (`passes/scope-and-api.ts::optimizeApiAliases`).
//!
//! Frequently used fixed API roots and members are assigned to short globals.
//! The analytical selection is refined by the same four-round remove/add
//! search as the TypeScript oracle. Every candidate is measured as a complete
//! scope-renamed program, but only the winner is renamed. Cost-only candidates carry
//! no provenance; a tracked input materializes only its selected candidate.

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::pass::PassResult;
use crate::scope_rename::{scope_rename_fast, ScopeRenameResult};
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
    target
        .nodes
        .derive_from(cloned, &source.nodes, node, "api-alias-optimization");
    cloned
}

fn alias_assignment(
    ast: &mut Ast,
    keys: &[String],
    aliases: &HashMap<String, String>,
    use_root_aliases: bool,
) -> NodeId {
    let first_synthetic = ast.nodes.len();
    #[expect(
        clippy::expect_used,
        reason = "keys is the ordered selection from the already constructed aliases map, which is immutable during emission"
    )]
    let targets = keys
        .iter()
        .map(|key| ast.name(aliases.get(key).expect("alias exists")))
        .collect::<Vec<_>>();
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
            let member = ast.str(format!("\"{}\"", parts[1]));
            expressions.push(ast.index(object, member, true));
        }
    }
    let assignment = ast.assign(targets, expressions);
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
) -> (Ast, NodeId) {
    let mut alias_names = HashMap::new();
    for (index, key) in selected.iter().enumerate() {
        alias_names.insert(key.clone(), format!("__api{index}"));
    }

    let mut target = storm_lua_syntax::ast_utils::inherit_ast(source);
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
        ));
    }
    if !member_aliases.is_empty() {
        definitions.push(alias_assignment(
            &mut target,
            &member_aliases,
            &alias_names,
            true,
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
) -> Candidate {
    let (ast, root) = apply_alias_set(source, root, resolution, analyzer, selected);
    // Trial comparison needs the exact renamed size, not a renamed AST. Keep
    // the selected unrenamed tree and materialize its winning name plan once.
    let len = crate::scope_rename::measure_renamed_size(&ast, root);
    Candidate { ast, root, len }
}

fn profitable(key: &str, count: usize) -> bool {
    count * key.len().saturating_sub(1) > key.len() + 3
}

fn contains(selected: &[String], key: &str) -> bool {
    selected.iter().any(|current| current == key)
}

pub fn optimize_api_aliases(ast: &mut Ast, root: NodeId) -> PassResult {
    // Candidate selection depends only on Lua syntax and binding/effect analysis.
    // Keep its NodeId/SymbolId space but do not construct source explanations for
    // candidates which are immediately discarded by the size comparison.
    let source = storm_lua_syntax::ast_utils::clone_without_origins(ast);
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

    let mut best = make_candidate(&source, root, &resolution, &analyzer, &selected);
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
            let candidate = make_candidate(&source, root, &resolution, &analyzer, &trial);
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
            let candidate = make_candidate(&source, root, &resolution, &analyzer, &trial);
            if candidate.len < best.len {
                selected.push(key);
                best = candidate;
                changed = true;
            }
        }
    }

    let none = make_candidate(&source, root, &resolution, &analyzer, &[]);
    let none_len = none.len;
    if none.len < best.len {
        selected.clear();
        best = none;
    }

    if ast.nodes.tracks_origins() {
        // Selection used the same immutable syntax/IDs. Apply only the selected
        // aliases to the tracked input; losing alternatives never need origins.
        let (tracked, tracked_root) = apply_alias_set(ast, root, &resolution, &analyzer, &selected);
        best.ast = tracked;
        best.root = tracked_root;
    }
    let ScopeRenameResult {
        ast: renamed,
        root: renamed_root,
        ..
    } = scope_rename_fast(&best.ast, best.root);
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
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod trial_provenance_tests {
    use super::*;
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
        let (aliased, aliased_root) = apply_alias_set(source, root, resolution, analyzer, selected);
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
