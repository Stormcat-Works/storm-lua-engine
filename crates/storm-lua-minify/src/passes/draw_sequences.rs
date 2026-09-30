//! Outline repeated literal draw motifs without moving or deleting draw calls.
//!
//! Only global-callee literal statements inside a root onDraw are considered.
//! This deliberately avoids capture analysis: the private helper is inserted
//! just before onDraw and its body has no free lexical locals. Every invocation
//! performs the same ordered side effects as the original statement sequence.
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::pass::PassResult;
use crate::scope_rename::scope_rename_fast;
use storm_lua_analysis::resolver::{resolve, BindingKind};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::size::{measure_size, measure_stmt};

#[derive(Clone, Copy)]
struct Site {
    block: NodeId,
    start: usize,
}
struct Motif {
    sites: Vec<Site>,
    length: usize,
    gain: usize,
    key: Vec<u32>,
}

fn on_draw(ast: &Ast, root: NodeId) -> Option<(usize, NodeId)> {
    let Node::Block(stmts) = ast.node(root) else {
        return None;
    };
    let res = resolve(ast, root);
    stmts.iter().enumerate().find_map(|(index, &s)| {
        let Node::Funcstat(target, function) = ast.node(s) else {
            return None;
        };
        let b = res.node_bid[*target as usize]?;
        let binding = res.binding(b);
        if binding.kind != BindingKind::Global
            || ast.strings.get(binding.name) != "onDraw"
            || res.binding_write_counts[b as usize] != 1
        {
            return None;
        }
        let Node::Function(_, false, body) = ast.node(*function) else {
            return None;
        };
        Some((index, *body))
    })
}

fn best_motif(ast: &Ast, root: NodeId, body: NodeId) -> Option<Motif> {
    let res = resolve(ast, root);
    let calls = super::draw_records::global_literal_draw_calls(ast, &res, root);
    if calls.len() < 4 {
        return None;
    }
    let mut blocks = Vec::new();
    storm_lua_syntax::ast_utils::walk(ast, body, &mut |n| {
        if matches!(ast.node(n), Node::Block(_)) {
            blocks.push(n)
        }
    });
    let mut intern = HashMap::<String, u32>::new();
    let mut occurrences = HashMap::<Vec<u32>, Vec<Site>>::new();
    for block in blocks {
        let Node::Block(stmts) = ast.node(block) else {
            unreachable!()
        };
        let mut tokens = Vec::new();
        for &s in stmts {
            tokens.push(calls.get(&s).map(|key| {
                let next = intern.len() as u32;
                *intern.entry(key.clone()).or_insert(next)
            }));
        }
        for start in 0..tokens.len() {
            let mut key = Vec::new();
            for token in tokens.iter().skip(start).take(48) {
                let Some(token) = token else { break };
                key.push(*token);
                if key.len() >= 2 {
                    occurrences
                        .entry(key.clone())
                        .or_default()
                        .push(Site { block, start });
                }
            }
        }
    }
    let mut best = None::<Motif>;
    for (key, mut sites) in occurrences {
        if sites.len() < 2 {
            continue;
        }
        sites.sort_by_key(|s| (s.block, s.start));
        let mut disjoint = Vec::<Site>::new();
        for site in sites {
            if disjoint
                .last()
                .is_none_or(|old| old.block != site.block || old.start + key.len() <= site.start)
            {
                disjoint.push(site);
            }
        }
        if disjoint.len() < 2 {
            continue;
        }
        let first = disjoint[0];
        let Node::Block(stmts) = ast.node(first.block) else {
            unreachable!()
        };
        let length = key.len();
        let bytes = stmts[first.start..first.start + length]
            .iter()
            .map(|&s| measure_stmt(ast, s))
            .sum::<usize>();
        // `local function f() ... end` plus each `f()` and a separator. The
        // authoritative gate below measures the full renamed candidate.
        let gain = ((disjoint.len() - 1) * bytes).saturating_sub(24 + disjoint.len() * 3);
        if gain == 0 {
            continue;
        }
        let candidate = Motif {
            sites: disjoint,
            length,
            gain,
            key,
        };
        let better = best.as_ref().is_none_or(|old| {
            candidate.gain > old.gain
                || candidate.gain == old.gain
                    && (candidate.length > old.length
                        || candidate.length == old.length && candidate.key < old.key)
        });
        if better {
            best = Some(candidate);
        }
    }
    best
}

pub fn outline_draw_sequences(ast: &mut Ast, root: NodeId, rename: bool) -> PassResult {
    let unchanged = || PassResult {
        root,
        saved: Some(0),
        details: None,
    };
    // Most logic-only programs have no onDraw. Avoid resolver/key-space work
    // in that common case; goto can cross a newly inserted local declaration.
    let Node::Block(stmts) = ast.node(root) else {
        return unchanged();
    };
    let has_draw=stmts.iter().any(|&n|matches!(ast.node(n),Node::Funcstat(t,_) if matches!(ast.node(*t),Node::Name(s) if ast.strings.get(*s)=="onDraw")));
    if !has_draw {
        return unchanged();
    }
    let mut jumps = false;
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |n| {
        jumps |= matches!(ast.node(n), Node::Goto(_) | Node::Label(_))
    });
    if jumps || !super::closed_fields::has_closed_key_space(ast, root) {
        return unchanged();
    }
    let Some((_, body)) = on_draw(ast, root) else {
        return unchanged();
    };
    let before = measure_size(ast, root);
    let mut target = ast.clone();
    let mut taken = ast
        .strings
        .all_strings()
        .into_iter()
        .collect::<HashSet<_>>();
    let mut serial = 0;
    let mut count = 0;
    let mut replaced = 0;
    for _ in 0..8 {
        let Some(motif) = best_motif(&target, root, body) else {
            break;
        };
        let res = resolve(&target, root);
        let mut inherited = vec![0usize; res.scopes.len()];
        for s in &res.scopes {
            inherited[s.id as usize] = s.parent.map_or(0, |p| inherited[p as usize])
                + s.bindings
                    .iter()
                    .filter(|&&b| res.binding(b).kind != BindingKind::Global)
                    .count();
        }
        if inherited.into_iter().max().unwrap_or(0) >= 180 {
            break;
        }
        let symbol = loop {
            let s = format!("__stormmin_draw_motif_{serial}");
            serial += 1;
            if taken.insert(s.clone()) {
                break target.strings.intern(&s);
            }
        };
        let first = motif.sites[0];
        let Node::Block(stmts) = target.node(first.block) else {
            unreachable!()
        };
        let motif_body = stmts[first.start..first.start + motif.length].to_vec();
        if target.nodes.tracks_origins() {
            for site in &motif.sites[1..] {
                let Node::Block(original) = target.node(site.block) else {
                    unreachable!()
                };
                let counterparts = original[site.start..site.start + motif.length].to_vec();
                for (&moved, &other) in motif_body.iter().zip(&counterparts) {
                    target
                        .nodes
                        .relate_within(moved, other, "draw-sequence-shared-command");
                }
            }
        }
        let block = target.push(Node::Block(motif_body));
        let function = target.push(Node::Function(Vec::new(), false, block));
        let definition = target.push(Node::Localfunc(symbol, function));
        for node in [block, function, definition] {
            target
                .nodes
                .mark_synthetic(node, "draw-sequence-helper-definition");
        }
        let mut by_block = BTreeMap::<NodeId, Vec<usize>>::new();
        for site in &motif.sites {
            by_block.entry(site.block).or_default().push(site.start);
        }
        for (block, starts) in by_block {
            let Node::Block(stmts) = target.node(block).clone() else {
                unreachable!()
            };
            let mut out = Vec::new();
            let mut cursor = 0;
            for start in starts {
                out.extend_from_slice(&stmts[cursor..start]);
                let f = target.push(Node::Name(symbol));
                let call = target.push(Node::Call(f, Vec::new(), None));
                let statement = target.push(Node::Callstat(call));
                target
                    .nodes
                    .mark_synthetic(f, "draw-sequence-helper-reference");
                for node in [call, statement] {
                    let origin = target.nodes.capture_origin(stmts[start]);
                    target
                        .nodes
                        .finish_rewrite(node, origin, "draw-sequence-invocation");
                    for &original in &stmts[start + 1..start + motif.length] {
                        target
                            .nodes
                            .relate_within(node, original, "draw-sequence-invocation");
                    }
                }
                out.push(statement);
                cursor = start + motif.length;
            }
            out.extend_from_slice(&stmts[cursor..]);
            target
                .nodes
                .rewrite(block, Node::Block(out), "draw-sequence-outlining");
        }
        let Some((at, _)) = on_draw(&target, root) else {
            unreachable!()
        };
        let Node::Block(mut stmts) = target.node(root).clone() else {
            unreachable!()
        };
        stmts.insert(at, definition);
        target
            .nodes
            .rewrite(root, Node::Block(stmts), "draw-sequence-helper-insertion");
        count += 1;
        replaced += motif.sites.len();
    }
    if count == 0 {
        return unchanged();
    }
    if rename {
        target = scope_rename_fast(&target, root).ast;
    }
    let after = measure_size(&target, root);
    if after >= before {
        return unchanged();
    }
    *ast = target;
    PassResult {
        root,
        saved: Some((before - after) as u64),
        details: Some(vec![format!("motifs={count};uses={replaced}")]),
    }
}
