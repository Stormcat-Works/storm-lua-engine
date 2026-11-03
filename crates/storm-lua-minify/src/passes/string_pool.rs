//! Pool repeated string literals without changing their value.
//!
//! The original spelling is emitted once into a fresh, never-written
//! chunk-local binding. All uses keep exactly the same Lua string value:
//! a literal and a local holding that literal are indistinguishable to
//! every reader (equality, table keys, concatenation, calls), and the
//! pooled binding is never assigned after its initializer, so no use can
//! observe a different value. This is a final-stage transformation, so the
//! measured size of each trial (including call-sugar parentheses and
//! separator costs, which the printer accounts for) decides acceptance:
//! a portfolio is adopted only when the whole-AST printed size decreases.
//!
//! Dot-access keys are not pooled: in winner code they are identifiers,
//! not value spellings, and pooling them would rewrite call shapes
//! (for example `screen.drawText` into `screen[name]`) for a different,
//! unmeasured effect. Bracket keys keep their value meaning and stay in.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use crate::pass::PassResult;
use crate::scope_rename::{measure_renamed_size_unmemoized, scope_rename_fast};
use storm_lua_analysis::resolver::{resolve, BindingKind};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::size::{measure_expr, measure_size};

struct Group {
    token: Arc<str>,
    nodes: Vec<NodeId>,
    estimate: usize,
}

fn pool_repeated_strings(ast: &mut Ast, root: NodeId, rename: bool) -> PassResult {
    let unchanged = || PassResult {
        root,
        saved: Some(0),
        details: None,
    };
    if !matches!(ast.node(root), Node::Block(_)) {
        return unchanged();
    }
    // Dot-access keys are identifiers in the printed code, not pooled
    // value spellings; rewriting them would change call shapes.
    let mut dot_keys = HashSet::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |node| {
        if let Node::Index(_, key, dot) = ast.node(node) {
            if *dot {
                dot_keys.insert(*key);
            }
        }
    });
    let mut literals = BTreeMap::<Arc<str>, Vec<NodeId>>::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |node| {
        if let Node::Str(token) = ast.node(node) {
            // Exclude the shortest spellings before allocating candidate
            // vectors. Measurement below uses the actual printer.
            if token.len() >= 3 && !dot_keys.contains(&node) {
                literals.entry(token.clone()).or_default().push(node);
            }
        }
    });
    let mut groups = literals
        .into_iter()
        .filter_map(|(token, nodes)| {
            if nodes.len() < 2 {
                return None;
            }
            let size = measure_expr(ast, nodes[0]);
            let estimate = size.saturating_sub(1) * nodes.len();
            let estimate = estimate.saturating_sub(size + 2);
            (estimate > 0).then_some(Group {
                token,
                nodes,
                estimate,
            })
        })
        .collect::<Vec<_>>();
    // No closed-key-space gate: pooling replaces value occurrences with a
    // local holding the identical string value. It renames no keys or
    // fields, and the pooled binding is never written after its
    // initializer, so dynamic keys and string-producing code cannot
    // observe a difference. (The same gate guards the numeric pool,
    // whose design this pass follows; corpus diagnostics showed it
    // blocked pooling on 589 files with profitable groups.)
    if groups.is_empty() {
        return unchanged();
    }
    groups.sort_by(|a, b| {
        b.estimate
            .cmp(&a.estimate)
            .then_with(|| a.token.cmp(&b.token))
    });
    let res = resolve(ast, root);
    let mut inherited = vec![0usize; res.scopes.len()];
    for scope in &res.scopes {
        inherited[scope.id as usize] = scope.parent.map_or(0, |p| inherited[p as usize])
            + scope
                .bindings
                .iter()
                .filter(|&&b| res.binding(b).kind != BindingKind::Global)
                .count();
    }
    // Pessimistic total local declarations in the chunk (excluding nested
    // function frames) bounds Lua's 200-register limit even across do/loops.
    fn chunk_locals(ast: &Ast, node: NodeId) -> usize {
        if matches!(ast.node(node), Node::Function(..)) {
            return 0;
        }
        let own = match ast.node(node) {
            Node::Local(names, _) | Node::Forin(names, ..) => names.len(),
            Node::Fornum(..) | Node::Localfunc(..) => 1,
            _ => 0,
        };
        let mut children = 0;
        storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
            children += chunk_locals(ast, child)
        });
        own + children
    }
    let budget = 64
        .min(180usize.saturating_sub(chunk_locals(ast, root)))
        .min(245usize.saturating_sub(inherited.into_iter().max().unwrap_or(0)));
    groups.truncate(budget);
    if groups.is_empty() {
        return unchanged();
    }
    // Bounded batch search, not one whole-AST rename per literal. The shorter
    // prefixes protect against alias-pressure making marginal pooling costly.
    // The 24 anchor keeps previously optimal portfolios reachable when more
    // than 24 groups are profitable; the register-limit bounds above still apply.
    let mut widths = vec![groups.len(), groups.len() / 2, 24.min(groups.len()), 1];
    widths.retain(|&n| n > 0);
    widths.sort_unstable();
    widths.dedup();
    let before = measure_size(ast, root);
    let mut best = None;
    let mut best_size = before;
    let mut best_count = 0;
    for width in widths {
        let mut trial = ast.clone();
        let mut taken = ast
            .strings
            .all_strings()
            .into_iter()
            .collect::<HashSet<_>>();
        let mut serial = 0;
        let mut symbols = Vec::new();
        let mut values = Vec::new();
        for group in groups.iter().take(width) {
            let name = loop {
                let name = format!("__stormmin_string_{serial}");
                serial += 1;
                if taken.insert(name.clone()) {
                    break name;
                }
            };
            let symbol = trial.strings.intern(&name);
            for &node in &group.nodes {
                trial
                    .nodes
                    .rewrite(node, Node::Name(symbol), "string-literal-pool-use");
            }
            symbols.push(symbol);
            let value = trial.push(Node::Str(group.token.clone()));
            if ast.nodes.tracks_origins() && group.nodes.iter().all(|&n| ast.nodes.has_origin(n)) {
                trial.nodes.derive_from(
                    value,
                    &ast.nodes,
                    group.nodes[0],
                    "string-literal-pool-value",
                );
                for &original in &group.nodes[1..] {
                    trial.nodes.relate_from(
                        value,
                        &ast.nodes,
                        original,
                        "string-literal-pool-value",
                    );
                }
            }
            values.push(value);
        }
        let declaration = trial.push(Node::Local(symbols, values));
        trial
            .nodes
            .mark_synthetic(declaration, "string-literal-pool-storage");
        let Node::Block(mut stmts) = trial.node(root).clone() else {
            unreachable!()
        };
        stmts.insert(0, declaration);
        trial
            .nodes
            .rewrite(root, Node::Block(stmts), "string-literal-pool-insertion");
        let size = if rename {
            measure_renamed_size_unmemoized(&trial, root)
        } else {
            measure_size(&trial, root)
        };
        if size < best_size {
            best = Some(trial);
            best_size = size;
            best_count = width;
        }
    }
    if let Some(trial) = best {
        *ast = if rename {
            scope_rename_fast(&trial, root).ast
        } else {
            trial
        };
        PassResult {
            root,
            saved: Some((before - best_size) as u64),
            details: Some(vec![format!("strings={best_count}")]),
        }
    } else {
        unchanged()
    }
}

/// Pool repeated string literals into fresh chunk-local bindings, keeping
/// only portfolios whose measured printed size decreases.
pub fn pool_string_literals(ast: &mut Ast, root: NodeId, rename: bool) -> PassResult {
    pool_repeated_strings(ast, root, rename)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn run(source: &str) -> (String, u64) {
        let (mut ast, root) = parse_source(source).expect("parse");
        // rename=true mirrors the pipeline: the pooled binding is renamed
        // to a short name, which is what makes pooling profitable.
        let result = pool_string_literals(&mut ast, root, true);
        let code = Printer::new(&ast, false).output(result.root);
        (code, result.saved.unwrap_or(0))
    }

    #[test]
    fn pools_a_repeated_long_string() {
        let (code, saved) = run(
            "function onTick() screen.drawText(0,0,\"repeated label\") screen.drawText(0,6,\"repeated label\") screen.drawText(0,12,\"repeated label\") end",
        );
        assert!(saved > 0, "code: {code}");
        assert_eq!(
            code.matches("\"repeated label\"").count(),
            1,
            "code: {code}"
        );
        assert!(
            !code.contains("__stormmin_string_0"),
            "pooled binding must be renamed short: {code}"
        );
        assert!(
            code.contains("screen.drawText"),
            "dot-access keys must stay untouched: {code}"
        );
    }

    #[test]
    fn pools_despite_dynamic_keys_and_string_production() {
        // Dynamic key access (t[k]) plus string production (concatenation)
        // used to trip the closed-key-space gate; pooling only replaces
        // value occurrences with a local holding the identical value, so
        // the gate must not block it.
        let (code, saved) = run(
            "function onTick(t, k) local s = t[k] .. \"!\" screen.drawText(0,0,\"repeated label here\") screen.drawText(0,6,\"repeated label here\") screen.drawText(0,12,\"repeated label here\") return s end",
        );
        assert!(saved > 0, "code: {code}");
        assert_eq!(
            code.matches("\"repeated label here\"").count(),
            1,
            "code: {code}"
        );
    }

    #[test]
    fn leaves_a_single_use_alone() {
        let (code, saved) = run("function onTick() screen.drawText(0,0,\"only once label\") end");
        assert_eq!(saved, 0, "code: {code}");
        assert_eq!(
            code.matches("\"only once label\"").count(),
            1,
            "code: {code}"
        );
    }

    #[test]
    fn pools_more_than_24_groups() {
        // 30 distinct profitable groups: with the old 24-group budget only
        // 24 could be pooled, leaving 6 spellings repeated.
        let mut src = String::from("function onTick() ");
        let mut spellings = Vec::new();
        for i in 0..30 {
            let s = format!("long label number {i:02}");
            spellings.push(s.clone());
            for _ in 0..3 {
                src.push_str(&format!("screen.drawText(0, 0, \"{s}\") "));
            }
        }
        src.push_str("end");
        let (code, saved) = run(&src);
        assert!(saved > 0, "code: {code}");
        for s in spellings {
            let quoted = format!("\"{s}\"");
            assert_eq!(
                code.matches(&quoted).count(),
                1,
                "spelling {s} must appear exactly once: {code}"
            );
        }
    }

    #[test]
    fn leaves_short_strings_alone() {
        let (code, saved) =
            run("function onTick() screen.drawText(0,0,\"ab\") screen.drawText(0,6,\"ab\") end");
        assert_eq!(saved, 0, "code: {code}");
        assert_eq!(code.matches("\"ab\"").count(), 2, "code: {code}");
    }
}
