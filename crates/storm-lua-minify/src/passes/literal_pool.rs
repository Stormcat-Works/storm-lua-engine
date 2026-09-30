//! Pool repeated long numeric literals without folding or approximating them.
//!
//! The original token is emitted once into a fresh, never-written chunk-local
//! binding. All uses keep exactly the same Lua numeric value and subtype,
//! including bitwise/index/loop routes. This is a final-stage transformation so
//! ordinary tiny-literal inlining cannot undo it or round the initializer.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use crate::pass::PassResult;
use crate::scope_rename::scope_rename_fast;
use storm_lua_analysis::resolver::{resolve, BindingKind};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::size::{measure_expr, measure_size};

mod expressions;

struct Group {
    token: Arc<str>,
    nodes: Vec<NodeId>,
    estimate: usize,
}

fn pool_repeated_literals(ast: &mut Ast, root: NodeId, rename: bool) -> PassResult {
    let unchanged = || PassResult {
        root,
        saved: Some(0),
        details: None,
    };
    if !matches!(ast.node(root), Node::Block(_)) {
        return unchanged();
    }
    let mut literals = BTreeMap::<Arc<str>, Vec<NodeId>>::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |node| {
        if let Node::Num(token) = ast.node(node) {
            // Exclude the overwhelmingly common tiny literals before allocating
            // candidate vectors. Measurement below uses the actual printer.
            if token.len() >= 3 {
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
    if groups.is_empty() || !super::closed_fields::has_closed_key_space(ast, root) {
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
    let budget = 24
        .min(180usize.saturating_sub(chunk_locals(ast, root)))
        .min(245usize.saturating_sub(inherited.into_iter().max().unwrap_or(0)));
    groups.truncate(budget);
    if groups.is_empty() {
        return unchanged();
    }
    // Bounded batch search, not one whole-AST rename per literal. The shorter
    // prefixes protect against alias-pressure making marginal pooling costly.
    let mut widths = vec![groups.len(), groups.len() / 2, 1];
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
                let name = format!("__stormmin_literal_{serial}");
                serial += 1;
                if taken.insert(name.clone()) {
                    break name;
                }
            };
            let symbol = trial.strings.intern(&name);
            for &node in &group.nodes {
                trial
                    .nodes
                    .rewrite(node, Node::Name(symbol), "numeric-literal-pool-use");
            }
            symbols.push(symbol);
            let value = trial.push(Node::Num(group.token.clone()));
            if ast.nodes.tracks_origins()
                && group.nodes.iter().all(|&n| ast.nodes.origin(n).is_some())
            {
                trial.nodes.derive_from(
                    value,
                    &ast.nodes,
                    group.nodes[0],
                    "numeric-literal-pool-value",
                );
                for &original in &group.nodes[1..] {
                    trial.nodes.relate_from(
                        value,
                        &ast.nodes,
                        original,
                        "numeric-literal-pool-value",
                    );
                }
            }
            values.push(value);
        }
        let declaration = trial.push(Node::Local(symbols, values));
        trial
            .nodes
            .mark_synthetic(declaration, "numeric-literal-pool-storage");
        let Node::Block(mut stmts) = trial.node(root).clone() else {
            unreachable!()
        };
        stmts.insert(0, declaration);
        trial
            .nodes
            .rewrite(root, Node::Block(stmts), "numeric-literal-pool-insertion");
        if rename {
            trial = scope_rename_fast(&trial, root).ast;
        }
        let size = measure_size(&trial, root);
        if size < best_size {
            best = Some(trial);
            best_size = size;
            best_count = width;
        }
    }
    if let Some(trial) = best {
        *ast = trial;
        PassResult {
            root,
            saved: Some((before - best_size) as u64),
            details: Some(vec![format!("literals={best_count}")]),
        }
    } else {
        unchanged()
    }
}

/// Retain the old pooling result and compare two exact-expression portfolios.
/// Synthesizing before pooling avoids making an alias for a now-tiny fraction;
/// synthesizing after pooling retains useful sharing of long exact constants.
pub fn pool_numeric_literals(ast: &mut Ast, root: NodeId, rename: bool) -> PassResult {
    let source = ast.clone();
    let before = measure_size(ast, root);
    let mut result = pool_repeated_literals(ast, root, rename);
    let mut best_size = measure_size(ast, root);
    let mut best = ast.clone();
    for mut trial in [best.clone(), source] {
        let synthesized = expressions::synthesize(&mut trial, root);
        if synthesized == 0 {
            continue;
        }
        let pooled = pool_repeated_literals(&mut trial, root, rename);
        let size = measure_size(&trial, root);
        if size < best_size {
            best_size = size;
            best = trial;
            result.details = pooled.details;
            result
                .details
                .get_or_insert_with(Vec::new)
                .push(format!("exact_expressions={synthesized}"));
        }
    }
    *ast = best;
    result.saved = Some(before.saturating_sub(best_size) as u64);
    result
}
