// Local-only renaming for lexical winners.
//
// The lexical (token-preserving) compilation route keeps original local
// names, because a global can be observed through `_ENV`. Renaming only
// locals is safe: a local binding is never a key of the environment
// table, so `_ENV` observations (including dynamic `_ENV[...]` indexing
// and whole-table replacement by `_ENV = ...`) cannot see a local.
// Globals keep their original spelling, field/method names, labels,
// string literals and comments are never touched, and a local originally
// named `_ENV` keeps its spelling. `_ENV` is never assigned as a new
// name. The plan is the globals-fixed variant of the scope-rename plan,
// whose corpus gain on the five lexical winners was censused at
// ~178,785 characters.
//
// The rename is applied at the token level (see
// `lexical_minify_renamed` in storm-lua-syntax): the AST is not
// rewritten, so token preservation (line positions, comments) is kept.
// Any anomaly in span collection aborts the whole plan (returns `None`)
// and the caller falls back to the unrenamed lexical output.

use storm_lua_analysis::resolver::for_each_child;
use storm_lua_syntax::ast::Node;
use storm_lua_syntax::parser::{parse_source_with_positions, NameSite};

/// Sorted, non-overlapping `(source_start, source_end, new_name)`
/// replacements for local identifier occurrences, or `None` if the plan
/// cannot be established safely (parse failure, missing span, or a span
/// that does not contain the expected original spelling).
pub fn lexical_rename_plan(source: &str) -> Option<Vec<(usize, usize, String)>> {
    let (ast, root, positions) = parse_source_with_positions(source).ok()?;
    let (resolution, names) = crate::scope_rename::local_only_rename_plan(&ast, root);
    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    let mut stack = vec![root];
    let mut visited = vec![false; ast.nodes.len()];
    while let Some(id) = stack.pop() {
        if std::mem::replace(&mut visited[id as usize], true) {
            continue;
        }
        // Collect (site, symbol, bid) triples for this node, mirroring
        // the rename application in scope_rename.rs.
        let mut occurrences: Vec<(NameSite, storm_lua_syntax::ast::SymbolId, u32)> = Vec::new();
        match ast.node(id) {
            Node::Name(symbol) => {
                if let Some(bid) = resolution.node_bid.get(id as usize).copied().flatten() {
                    occurrences.push((NameSite::Reference, *symbol, bid));
                }
            }
            Node::Localfunc(symbol, _) | Node::Fornum(symbol, ..) => {
                if let Some(bid) = resolution.node_bid.get(id as usize).copied().flatten() {
                    occurrences.push((NameSite::Binding(0), *symbol, bid));
                }
            }
            Node::Local(symbols, _) | Node::Forin(symbols, ..) => {
                if let Some(bids) = resolution.node_bids.get(id as usize) {
                    for (index, (symbol, bid)) in symbols.iter().zip(bids.iter()).enumerate() {
                        occurrences.push((NameSite::Binding(index as u32), *symbol, *bid));
                    }
                }
            }
            Node::Function(symbols, ..) => {
                if let Some(bids) = resolution.node_bids.get(id as usize) {
                    for (index, (symbol, bid)) in symbols.iter().zip(bids.iter()).enumerate() {
                        occurrences.push((NameSite::Parameter(index as u32), *symbol, *bid));
                    }
                }
            }
            _ => {}
        }
        for (site, symbol, bid) in occurrences {
            let Some(new_name) = names.get(bid as usize).and_then(|n| n.as_deref()) else {
                continue;
            };
            let original = ast.strings.get(symbol);
            if new_name == original {
                continue;
            }
            // A renamed occurrence without a source span would break
            // consistency (some occurrences renamed, this one not), so
            // abort the whole plan instead of renaming partially.
            let (start, end) = positions.name_span(id, site)?;
            if source.get(start..end) != Some(original) {
                return None;
            }
            replacements.push((start, end, new_name.to_owned()));
        }
        for_each_child(&ast, id, &mut |child| stack.push(child));
    }
    replacements.sort_unstable_by_key(|(start, _, _)| *start);
    for pair in replacements.windows(2) {
        if pair[0].1 > pair[1].0 {
            return None;
        }
    }
    Some(replacements)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn local_only_plan_renames_locals_but_not_globals() {
        let source = "local long_name=1\n_ENV.x=long_name\nreturn long_name";
        let plan = lexical_rename_plan(source).unwrap();
        // Declaration + two uses, all renamed to the first free short name.
        assert_eq!(plan.len(), 3);
        assert!(plan.iter().all(|(_, _, name)| name == "a"));
        for (start, end, _) in &plan {
            assert_eq!(&source[*start..*end], "long_name");
        }
        // The _ENV token itself is not in the plan (it is a global).
        assert!(!plan.iter().any(|(s, e, _)| &source[*s..*e] == "_ENV"));
    }

    #[test]
    fn local_named_env_keeps_its_spelling() {
        let source = "local _ENV={}\nreturn _ENV";
        let plan = lexical_rename_plan(source).unwrap();
        assert!(plan.is_empty());
    }

    #[test]
    fn string_and_comment_mentions_are_not_renamed() {
        let source = "local long_name=1 -- long_name\n_ENV.x='long_name'\nreturn long_name";
        let plan = lexical_rename_plan(source).unwrap();
        // Only the declaration and the final use are local occurrences;
        // the comment and the string literal are not identifier tokens.
        assert_eq!(plan.len(), 2);
        for (start, end, _) in &plan {
            assert_eq!(&source[*start..*end], "long_name");
        }
    }

    #[test]
    fn renamed_lexical_output_keeps_env_and_shortens_locals() {
        let source = "local long_name=1\n_ENV.x=long_name\nreturn long_name";
        let plan = lexical_rename_plan(source).unwrap();
        let renamed = storm_lua_syntax::print::lexical_minify_renamed(source, &plan).unwrap();
        assert_eq!(renamed, "local a=1\n_ENV.x=a\nreturn a");
        // The unrenamed path keeps the original local name.
        let plain = storm_lua_syntax::print::lexical_minify(source).unwrap();
        assert_eq!(
            plain,
            "local long_name=1\n_ENV.x=long_name\nreturn long_name"
        );
    }

    #[test]
    fn renamed_lexical_origins_validate_and_keep_original_names() {
        let source = "local long_name=1\n_ENV.x=long_name\nreturn long_name";
        let plan = lexical_rename_plan(source).unwrap();
        let (code, origins) = storm_lua_syntax::print::lexical_minify_renamed_with_compact_origins(
            "test.lua", source, &plan,
        )
        .unwrap();
        assert_eq!(code, "local a=1\n_ENV.x=a\nreturn a");
        origins.validate_for_code(&code).unwrap();
        let generated = origins.into_generated();
        generated.validate_for_code(&code).unwrap();
        // Map names are the original spellings, not the short names.
        assert!(generated
            .origins
            .iter()
            .any(|o| o.name.as_deref() == Some("long_name")));
        assert!(!generated
            .origins
            .iter()
            .any(|o| o.name.as_deref() == Some("a")));
    }
}
