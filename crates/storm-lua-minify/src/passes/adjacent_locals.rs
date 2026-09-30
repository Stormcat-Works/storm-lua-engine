//! Combine adjacent independent local declarations at the final size stage.
//! Every initializer retains its evaluation position and single-value adjustment.
//! Binding-aware dependency checks include captures inside functions and tables.
use std::collections::HashSet;

use crate::pass::PassResult;
use crate::scope_rename::scope_rename_fast;
use storm_lua_analysis::resolver::resolve;
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::size::measure_size;

pub fn pack_adjacent_locals(ast: &mut Ast, root: NodeId, rename: bool) -> PassResult {
    let unchanged = || PassResult {
        root,
        saved: Some(0),
        details: None,
    };
    let mut blocks = Vec::new();
    let mut unsafe_scope = false;
    let mut local_count = 0;
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |n| match ast.node(n) {
        Node::Block(statements) => {
            if statements.windows(2).any(|p| {
                matches!(ast.node(p[0]), Node::Local(..))
                    && matches!(ast.node(p[1]), Node::Local(..))
            }) {
                blocks.push(n)
            }
        }
        Node::Goto(_) | Node::Label(_) => unsafe_scope = true,
        Node::Local(names, _) => {
            local_count += names.len();
            unsafe_scope |= names.iter().any(|&s| ast.strings.get(s) == "_ENV");
        }
        _ => {}
    });
    // Conservative whole-program bound leaves temporary-register headroom even
    // when many locals are live. No new declarations or helper functions appear.
    if blocks.is_empty()
        || unsafe_scope
        || local_count > 150
        || !super::closed_fields::has_closed_key_space(ast, root)
    {
        return unchanged();
    }
    let res = resolve(ast, root);
    let mut target = ast.clone();
    let mut merged = 0;
    for block in blocks {
        let Node::Block(statements) = ast.node(block) else {
            unreachable!()
        };
        let mut output = Vec::with_capacity(statements.len());
        let mut pending: Option<NodeId> = None;
        let mut declared = HashSet::new();
        for &stmt in statements {
            let eligible = match ast.node(stmt) {
                Node::Local(names, values) => {
                    !names.is_empty()
                        && names.len() == values.len()
                        && names.len() <= 16
                        && names.iter().collect::<HashSet<_>>().len() == names.len()
                }
                _ => false,
            };
            if !eligible {
                pending = None;
                declared.clear();
                output.push(stmt);
                continue;
            }
            let Node::Local(names, values) = ast.node(stmt) else {
                unreachable!()
            };
            let depends = values.iter().any(|&v| {
                let mut found = false;
                storm_lua_syntax::ast_utils::walk(ast, v, &mut |n| {
                    found |= res.node_bid[n as usize].is_some_and(|bid| declared.contains(&bid));
                });
                found
            });
            if let Some(previous) = pending {
                let Node::Local(old_names, old_values) = target.node(previous) else {
                    unreachable!()
                };
                if !depends
                    && old_names.len() + names.len() <= 16
                    && names.iter().all(|n| !old_names.contains(n))
                {
                    let first_new_slot = old_names.len();
                    let mut all_names = old_names.clone();
                    let mut all_values = old_values.clone();
                    all_names.extend_from_slice(names);
                    all_values.extend_from_slice(values);
                    target.nodes.rewrite(
                        previous,
                        Node::Local(all_names, all_values),
                        "adjacent-local-packing",
                    );
                    target
                        .nodes
                        .relate_from(previous, &ast.nodes, stmt, "adjacent-local-packing");
                    for offset in 0..names.len() {
                        target.nodes.copy_name_from(
                            previous,
                            storm_lua_syntax::NameSite::Binding((first_new_slot + offset) as u32),
                            &ast.nodes,
                            stmt,
                            storm_lua_syntax::NameSite::Binding(offset as u32),
                        );
                    }
                    declared.extend(res.node_bids[stmt as usize].iter().copied());
                    merged += 1;
                    continue;
                }
            }
            pending = Some(stmt);
            declared.clear();
            declared.extend(res.node_bids[stmt as usize].iter().copied());
            output.push(stmt);
        }
        target
            .nodes
            .rewrite(block, Node::Block(output), "adjacent-local-packing");
    }
    if merged == 0 {
        return unchanged();
    }
    if rename {
        target = scope_rename_fast(&target, root).ast
    }
    let before = measure_size(ast, root);
    let after = measure_size(&target, root);
    if after >= before {
        return unchanged();
    }
    *ast = target;
    PassResult {
        root,
        saved: Some((before - after) as u64),
        details: Some(vec![format!("merged={merged}")]),
    }
}
