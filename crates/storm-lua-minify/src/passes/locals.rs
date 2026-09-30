//! Local binding cleanup passes (`passes/locals.ts`).

use crate::pass::PassResult;
use storm_lua_analysis::effects::{count_binding_uses, EffectAnalyzer};
use storm_lua_analysis::resolver::{resolve, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::size::measure_size;

#[allow(clippy::too_many_arguments)]
fn rewrite_dead_node(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    effects: &EffectAnalyzer<'_>,
    reads: &std::collections::HashMap<u32, u32>,
    writes: &std::collections::HashMap<u32, u32>,
    id: NodeId,
    changed: &mut bool,
) -> Option<NodeId> {
    if matches!(source.node(id), Node::Block(_)) {
        return Some(rewrite_dead_block(
            target, source, res, effects, reads, writes, id, changed,
        ));
    }
    let node = source.node(id).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&node, &mut |child| {
        rewrite_dead_node(target, source, res, effects, reads, writes, child, changed)
            .unwrap_or(child)
    });
    target.nodes.rewrite(id, mapped, "local-scope-cleanup");
    Some(id)
}

#[allow(clippy::too_many_arguments)]
fn rewrite_dead_block(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    effects: &EffectAnalyzer<'_>,
    reads: &std::collections::HashMap<u32, u32>,
    writes: &std::collections::HashMap<u32, u32>,
    block: NodeId,
    changed: &mut bool,
) -> NodeId {
    let Node::Block(statements) = source.node(block).clone() else {
        unreachable!()
    };
    let mut output = Vec::with_capacity(statements.len());
    for statement in statements {
        #[expect(
            clippy::expect_used,
            reason = "This loop visits non-block statements; deletion of local declarations is performed only after the child rewrite"
        )]
        let statement = rewrite_dead_node(
            target, source, res, effects, reads, writes, statement, changed,
        )
        .expect("non-block statements are retained");
        let Node::Local(names, expressions) = target.node(statement).clone() else {
            output.push(statement);
            continue;
        };
        let bids = res
            .node_bids
            .get(statement as usize)
            .cloned()
            .unwrap_or_default();
        if names.len() != expressions.len() || bids.len() != names.len() {
            output.push(statement);
            continue;
        }
        let keep = bids
            .iter()
            .enumerate()
            .filter_map(|(index, bid)| {
                let removable = reads.get(bid).copied().unwrap_or(0) == 0
                    && writes.get(bid).copied().unwrap_or(0) == 0
                    && effects.is_movable(expressions[index]);
                if removable {
                    *changed = true;
                    None
                } else {
                    Some(index)
                }
            })
            .collect::<Vec<_>>();
        if keep.is_empty() {
            continue;
        }
        if keep.len() != names.len() {
            target.nodes.rewrite(
                statement,
                Node::Local(
                    keep.iter().map(|index| names[*index]).collect(),
                    keep.iter().map(|index| expressions[*index]).collect(),
                ),
                "dead-local-elimination",
            );
            for (new_slot, &old_slot) in keep.iter().enumerate() {
                target.nodes.copy_name_from(
                    statement,
                    storm_lua_syntax::NameSite::Binding(new_slot as u32),
                    &source.nodes,
                    statement,
                    storm_lua_syntax::NameSite::Binding(old_slot as u32),
                );
            }
        }
        output.push(statement);
    }
    target
        .nodes
        .rewrite(block, Node::Block(output), "local-scope-cleanup");
    block
}

pub fn eliminate_dead_locals(ast: &mut Ast, root: NodeId) -> PassResult {
    let original = measure_size(ast, root);
    for _ in 0..20 {
        let mut source = storm_lua_syntax::ast_utils::inherit_ast(ast);
        source.nodes = ast.nodes.clone();
        let res = resolve(&source, root);
        let effects = EffectAnalyzer::new(&source, &res, root, true);
        let (reads, writes) = count_binding_uses(&source, &res, root);
        let mut changed = false;
        rewrite_dead_block(
            ast,
            &source,
            &res,
            &effects,
            &reads,
            &writes,
            root,
            &mut changed,
        );
        if !changed {
            break;
        }
    }
    PassResult {
        root,
        saved: Some(original.saturating_sub(measure_size(ast, root)) as u64),
        details: None,
    }
}

fn flatten_node(target: &mut Ast, source: &Ast, id: NodeId) -> NodeId {
    if matches!(source.node(id), Node::Block(_)) {
        return flatten_block(target, source, id);
    }
    let node = source.node(id).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&node, &mut |child| {
        flatten_node(target, source, child)
    });
    target.nodes.rewrite(id, mapped, "local-scope-cleanup");
    id
}

fn flatten_block(target: &mut Ast, source: &Ast, block: NodeId) -> NodeId {
    let Node::Block(statements) = source.node(block).clone() else {
        unreachable!()
    };
    let statements = statements
        .into_iter()
        .map(|statement| flatten_node(target, source, statement))
        .collect::<Vec<_>>();
    let mut output = Vec::with_capacity(statements.len());

    for (index, statement) in statements.iter().copied().enumerate() {
        let Node::Do(body) = target.node(statement) else {
            output.push(statement);
            continue;
        };
        let Node::Block(inner) = target.node(*body).clone() else {
            output.push(statement);
            continue;
        };

        // Removing this wrapper only widens the lexical scope of statements
        // directly owned by its body.  Nested if/loop/function/do blocks keep
        // their own scope, so locals and labels inside those nested blocks do
        // not make the outer wrapper observable.
        let mut unsafe_scope = inner.iter().copied().any(|id| {
            matches!(
                target.node(id),
                Node::Local(..) | Node::Localfunc(..) | Node::Label(..) | Node::Goto(..)
            )
        });

        let is_terminal = index + 1 == statements.len();
        if !is_terminal {
            // Lua requires a return statement to be the final statement of its
            // block.  Splicing a direct return before a following sibling would
            // therefore produce invalid Lua.  `break` is kept conservative too
            // even though `do` itself is not a loop boundary.
            unsafe_scope |= inner
                .iter()
                .copied()
                .any(|id| matches!(target.node(id), Node::Return(..) | Node::Break));
        }

        if !unsafe_scope {
            // Preserve the old label/goto conservatism: if the enclosing block
            // already participates in named control flow, do not widen any
            // lexical block there even when this particular body has no label.
            for sibling in statements.iter().copied().filter(|id| *id != statement) {
                storm_lua_syntax::ast_utils::walk(target, sibling, &mut |id| {
                    unsafe_scope |= matches!(target.node(id), Node::Label(..) | Node::Goto(..));
                });
                if unsafe_scope {
                    break;
                }
            }
        }

        if unsafe_scope {
            output.push(statement);
        } else {
            output.extend(inner);
        }
    }

    target
        .nodes
        .rewrite(block, Node::Block(output), "local-scope-cleanup");
    block
}

pub fn flatten_terminal_do_blocks(ast: &mut Ast, root: NodeId) -> PassResult {
    let original = measure_size(ast, root);
    let mut source = storm_lua_syntax::ast_utils::inherit_ast(ast);
    source.nodes = ast.nodes.clone();
    let out = flatten_block(ast, &source, root);
    PassResult {
        root: out,
        saved: Some(original.saturating_sub(measure_size(ast, out)) as u64),
        details: None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use storm_lua_syntax::parser::parse_source;
    use storm_lua_syntax::print::Printer;

    fn output(source: &str, dead: bool) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = if dead {
            eliminate_dead_locals(&mut ast, root)
        } else {
            flatten_terminal_do_blocks(&mut ast, root)
        };
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn removes_unused_movable_locals() {
        assert_eq!(
            output("local x=1 local y=f() return y", true),
            "local y=f()return y"
        );
    }

    #[test]
    fn preserves_unused_effectful_initializer() {
        assert!(output("local x=f()", true).contains("local x=f()"));
    }

    #[test]
    fn removes_dead_local_chains_across_rounds() {
        assert_eq!(output("local a=1 local b=a", true), "");
    }

    #[test]
    fn flattens_terminal_and_scope_independent_nonterminal_do_blocks() {
        assert_eq!(output("x=1 do y=2 end", false), "x=1 y=2");
        assert_eq!(output("do y=2 end x=1", false), "y=2 x=1");
    }

    #[test]
    fn keeps_nonterminal_do_when_direct_scope_or_control_flow_is_observable() {
        assert!(output("do local y=2 end x=1", false).starts_with("do local y=2 end"));
        assert!(output("function f()do return 1 end x=2 end", false).contains("do return 1 end"));
        assert!(output("::outer::do goto outer end x=1", false).contains("do goto outer end"));
    }

    #[test]
    fn flattens_nonterminal_do_when_only_nested_scopes_are_observable() {
        assert_eq!(
            output("do if a then local y=2 end end x=1", false),
            "if a then local y=2 end x=1"
        );
        assert_eq!(
            output("function f()do if a then return 1 end end x=2 end", false),
            "function f()if a then return 1 end x=2 end"
        );
    }
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn remove_statement_from_blocks(ast: &mut Ast, statement: NodeId) {
    ast.nodes
        .retain_block_statements(|id| id != statement, "unused-local-declaration-removal");
}

fn remove_local_binding(ast: &mut Ast, source: &Ast, res: &Resolution, bid: u32) -> bool {
    let Some(statement) = res.binding(bid).decl_node else {
        return false;
    };
    let bids = res
        .node_bids
        .get(statement as usize)
        .cloned()
        .unwrap_or_default();
    let Some(index) = bids.iter().position(|candidate| *candidate == bid) else {
        return false;
    };
    let Node::Local(names, expressions) = ast.node(statement).clone() else {
        return false;
    };
    let mut names = names;
    let mut expressions = expressions;
    names.remove(index);
    if index < expressions.len() {
        expressions.remove(index);
    }
    if names.is_empty() {
        remove_statement_from_blocks(ast, statement);
    } else {
        ast.nodes.rewrite(
            statement,
            Node::Local(names, expressions),
            "tiny-literal-binding-removal",
        );
        for new_index in 0..bids.len() - 1 {
            let old_index = if new_index < index {
                new_index
            } else {
                new_index + 1
            };
            ast.nodes.copy_name_from(
                statement,
                storm_lua_syntax::NameSite::Binding(new_index as u32),
                &source.nodes,
                statement,
                storm_lua_syntax::NameSite::Binding(old_index as u32),
            );
        }
    }
    true
}

fn replace_binding_with_literal(
    ast: &mut Ast,
    source: &Ast,
    res: &Resolution,
    bid: u32,
    literal: NodeId,
) {
    let replacement = ast.node(literal).clone();
    let ids = (0..ast.nodes.len() as NodeId).collect::<Vec<_>>();
    for id in ids {
        if matches!(ast.node(id), Node::Name(_))
            && res.node_bid.get(id as usize).copied().flatten() == Some(bid)
        {
            ast.nodes[id as usize] = replacement.clone();
            ast.nodes
                .derive_from(id, &source.nodes, literal, "tiny-literal-copy");
            ast.nodes
                .relate_from(id, &source.nodes, id, "tiny-literal-use");
        }
    }
}

pub fn inline_tiny_literal_bindings(ast: &mut Ast, root: NodeId) -> PassResult {
    let original = measure_size(ast, root);
    for _ in 0..64 {
        let source = clone_ast(ast);
        let res = resolve(&source, root);
        let (reads, writes) = count_binding_uses(&source, &res, root);
        let before = measure_size(&source, root);
        let mut best: Option<(usize, storm_lua_syntax::node_arena::NodeArena)> = None;
        let mut nodes = Vec::new();
        storm_lua_syntax::ast_utils::walk(&source, root, &mut |id| nodes.push(id));
        for statement in nodes {
            let Node::Local(names, expressions) = source.node(statement) else {
                continue;
            };
            let bids = res
                .node_bids
                .get(statement as usize)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            if names.len() != expressions.len() || bids.len() != names.len() {
                continue;
            }
            for (index, bid) in bids.iter().enumerate() {
                let literal = expressions[index];
                if writes.get(bid).copied().unwrap_or(0) != 0
                    || !matches!(
                        source.node(literal),
                        Node::Num(_) | Node::Bool(_) | Node::Nil | Node::Str(_)
                    )
                    || storm_lua_syntax::size::measure_expr(&source, literal) > 2
                    || reads.get(bid).copied().unwrap_or(0) == 0
                {
                    continue;
                }
                let mut trial = clone_ast(&source);
                replace_binding_with_literal(&mut trial, &source, &res, *bid, literal);
                if !remove_local_binding(&mut trial, &source, &res, *bid) {
                    continue;
                }
                let after = measure_size(&trial, root);
                let gain = before.saturating_sub(after);
                if gain > 0 && best.as_ref().is_none_or(|(current, _)| gain > *current) {
                    best = Some((gain, trial.nodes));
                }
            }
        }
        let Some((_, nodes)) = best else {
            break;
        };
        ast.nodes = nodes;
    }
    PassResult {
        root,
        saved: Some(original.saturating_sub(measure_size(ast, root)) as u64),
        details: None,
    }
}

fn is_sink_target_excluded(node: &Node) -> bool {
    matches!(
        node,
        Node::If(..)
            | Node::While(..)
            | Node::Repeat(..)
            | Node::Fornum(..)
            | Node::Forin(..)
            | Node::Funcstat(..)
            | Node::Localfunc(..)
            | Node::Do(..)
            | Node::Callstat(..)
    )
}

/// Collect only the nearest nested block for each branch.  The old
/// implementation walked every statement's complete descendant tree and then
/// recursively walked each block it found, revisiting deep blocks once per
/// ancestor.  Keeping the frontier here lets `collect_single_use_replacements`
/// recurse exactly once per block (linear in the AST depth).
fn nested_block_frontier(ast: &Ast, node: NodeId, output: &mut Vec<NodeId>) {
    let mut children = Vec::new();
    storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
        children.push(child)
    });
    for child in children {
        if matches!(ast.node(child), Node::Block(_)) {
            output.push(child);
        } else {
            nested_block_frontier(ast, child, output);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_single_use_replacements(
    ast: &Ast,
    res: &Resolution,
    effects: &EffectAnalyzer<'_>,
    reads: &std::collections::HashMap<u32, u32>,
    writes: &std::collections::HashMap<u32, u32>,
    block: NodeId,
    aggressive: bool,
    replacements: &mut std::collections::HashMap<u32, NodeId>,
    remove: &mut std::collections::HashSet<u32>,
) {
    let Node::Block(statements) = ast.node(block) else {
        return;
    };
    for window in statements.windows(2) {
        let statement = window[0];
        let target = window[1];
        let Node::Local(names, expressions) = ast.node(statement) else {
            continue;
        };
        let bids = res
            .node_bids
            .get(statement as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if names.len() != expressions.len()
            || bids.len() != names.len()
            || is_sink_target_excluded(ast.node(target))
        {
            continue;
        }
        for (index, bid) in bids.iter().enumerate() {
            let expression = expressions[index];
            if reads.get(bid).copied().unwrap_or(0) != 1
                || writes.get(bid).copied().unwrap_or(0) != 0
                || !storm_lua_analysis::effects::contains_binding(ast, res, target, *bid)
            {
                continue;
            }
            // The replacement is evaluated where the sole read occurs.  A call
            // (or another ordered expression) in the target can observe a
            // different value when it runs before that read, e.g.
            // `output.setNumber(g(), local_value)`.  The local snapshot must
            // remain before the call in that case.
            let target_effect = effects.effects_for_statement(target);
            if target_effect.calls || target_effect.ordered {
                continue;
            }
            let effect = effects.effects_for_expr(expression);
            if effect.ordered
                || effect.calls
                || !effect.writes.is_empty()
                || (!aggressive && effect.may_throw)
                || !effect.stable
            {
                continue;
            }
            // Literal values and a proven builtin reference do not sample
            // changing runtime state. Preserve those existing safe reductions
            // while keeping calls and mutable expression snapshots outside.
            let capture_invariant = matches!(
                ast.node(expression),
                Node::Nil | Node::Bool(_) | Node::Num(_) | Node::Str(_)
            ) || effects.resolve_builtin_reference(expression).is_some();
            let mut occurrences = 0usize;
            let mut captured_read = false;
            storm_lua_syntax::ast_utils::walk(ast, target, &mut |id| {
                // A closure body runs later, possibly more than once. Moving an
                // initializer into it changes the snapshot lifetime, even when
                // the value is stable within one callback. This also covers
                // function literals nested inside returned tables.
                if let Node::Function(_, _, body) = ast.node(id) {
                    captured_read |= !capture_invariant
                        && storm_lua_analysis::effects::contains_binding(ast, res, *body, *bid);
                }
                if matches!(ast.node(id), Node::Name(_))
                    && res.node_bid.get(id as usize).copied().flatten() == Some(*bid)
                {
                    occurrences += 1;
                }
            });
            if occurrences == 1 && (!captured_read || capture_invariant) {
                replacements.insert(*bid, expression);
                remove.insert(*bid);
            }
        }
    }
    let mut nested = Vec::new();
    for statement in statements {
        nested_block_frontier(ast, *statement, &mut nested);
    }
    for nested_block in nested {
        collect_single_use_replacements(
            ast,
            res,
            effects,
            reads,
            writes,
            nested_block,
            aggressive,
            replacements,
            remove,
        );
    }
}

fn clone_expanded(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    replacements: &std::collections::HashMap<u32, NodeId>,
    id: NodeId,
    seen: &std::collections::HashSet<u32>,
) -> NodeId {
    if matches!(source.node(id), Node::Name(_)) {
        if let Some(bid) = res.node_bid.get(id as usize).copied().flatten() {
            if let Some(replacement) = replacements.get(&bid) {
                if !seen.contains(&bid) {
                    let mut next = seen.clone();
                    next.insert(bid);
                    let copied =
                        clone_expanded(target, source, res, replacements, *replacement, &next);
                    target
                        .nodes
                        .relate_from(copied, &source.nodes, id, "single-use-local-read");
                    return copied;
                }
            }
        }
    }
    let node = source.node(id).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&node, &mut |child| {
        clone_expanded(target, source, res, replacements, child, seen)
    });
    let copied = target.push(mapped);
    target.nodes.derive_from(
        copied,
        &source.nodes,
        id,
        "single-use-local-expression-copy",
    );
    copied
}

fn expand_tree(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    replacements: &std::collections::HashMap<u32, NodeId>,
    id: NodeId,
) -> NodeId {
    if matches!(source.node(id), Node::Name(_)) {
        if let Some(bid) = res.node_bid.get(id as usize).copied().flatten() {
            if let Some(replacement) = replacements.get(&bid) {
                let copied = clone_expanded(
                    target,
                    source,
                    res,
                    replacements,
                    *replacement,
                    &std::collections::HashSet::from([bid]),
                );
                target
                    .nodes
                    .relate_from(copied, &source.nodes, id, "single-use-local-read");
                return copied;
            }
        }
    }
    let node = source.node(id).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&node, &mut |child| {
        expand_tree(target, source, res, replacements, child)
    });
    target
        .nodes
        .rewrite(id, mapped, "single-use-local-substitution");
    id
}

fn strip_removed_locals(
    ast: &mut Ast,
    source: &Ast,
    res: &Resolution,
    remove: &std::collections::HashSet<u32>,
) {
    let ids = (0..source.nodes.len() as NodeId).collect::<Vec<_>>();
    for statement in ids {
        let Node::Local(names, expressions) = ast.node(statement).clone() else {
            continue;
        };
        let bids = res
            .node_bids
            .get(statement as usize)
            .cloned()
            .unwrap_or_default();
        if bids.len() != names.len() {
            continue;
        }
        let keep = bids
            .iter()
            .enumerate()
            .filter_map(|(index, bid)| (!remove.contains(bid)).then_some(index))
            .collect::<Vec<_>>();
        if keep.len() == names.len() {
            continue;
        }
        if keep.is_empty() {
            remove_statement_from_blocks(ast, statement);
        } else {
            ast.nodes.rewrite(
                statement,
                Node::Local(
                    keep.iter().map(|index| names[*index]).collect(),
                    keep.iter()
                        .filter_map(|index| expressions.get(*index).copied())
                        .collect(),
                ),
                "single-use-local-binding-removal",
            );
            for (new_index, &old_index) in keep.iter().enumerate() {
                ast.nodes.copy_name_from(
                    statement,
                    storm_lua_syntax::NameSite::Binding(new_index as u32),
                    &source.nodes,
                    statement,
                    storm_lua_syntax::NameSite::Binding(old_index as u32),
                );
            }
        }
    }
}

pub fn inline_single_use_locals_with_options(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    max_rounds: usize,
) -> PassResult {
    let original = measure_size(ast, root);
    for _ in 0..max_rounds {
        let source = clone_ast(ast);
        let res = resolve(&source, root);
        let effects = EffectAnalyzer::new(&source, &res, root, true);
        let (reads, writes) = count_binding_uses(&source, &res, root);
        let mut replacements = std::collections::HashMap::new();
        let mut remove = std::collections::HashSet::new();
        collect_single_use_replacements(
            &source,
            &res,
            &effects,
            &reads,
            &writes,
            root,
            aggressive,
            &mut replacements,
            &mut remove,
        );
        if replacements.is_empty() {
            break;
        }
        let before = measure_size(&source, root);
        let mut trial = clone_ast(&source);
        expand_tree(&mut trial, &source, &res, &replacements, root);
        strip_removed_locals(&mut trial, &source, &res, &remove);
        let after = measure_size(&trial, root);
        if after >= before {
            break;
        }
        ast.nodes = trial.nodes;
    }
    PassResult {
        root,
        saved: Some(original.saturating_sub(measure_size(ast, root)) as u64),
        details: None,
    }
}

pub fn inline_single_use_locals(ast: &mut Ast, root: NodeId) -> PassResult {
    inline_single_use_locals_with_options(ast, root, true, 8)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;
    use storm_lua_syntax::parser::parse_source;
    use storm_lua_syntax::print::Printer;

    fn output(source: &str, tiny: bool) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = if tiny {
            inline_tiny_literal_bindings(&mut ast, root)
        } else {
            inline_single_use_locals(&mut ast, root)
        };
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn inlines_profitable_tiny_literal() {
        assert_eq!(output("local x=1 return x", true), "return 1");
    }

    #[test]
    fn tiny_literal_respects_shadowed_bindings() {
        let out = output("local x=1 do local x=2 return x end return x", true);
        assert!(out.contains("return 2"), "{out}");
        assert!(out.ends_with("return 1"), "{out}");
    }

    #[test]
    fn sinks_adjacent_single_use_local() {
        assert_eq!(output("local x=a+b y=x*2", false), "y=(a+b)*2");
    }

    #[test]
    fn does_not_sink_across_control_flow() {
        let out = output("local x=a+b if c then y=x end", false);
        assert!(out.starts_with("local x="), "{out}");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod local_origin_tests {
    use super::*;
    use storm_lua_syntax::provenance::{GeneratedOrigins, OriginKind};
    use storm_lua_syntax::{parse_source_with_origins, NameSite, Printer};

    fn printed(ast: &Ast, root: NodeId) -> (String, GeneratedOrigins) {
        let output = Printer::new(ast, false).output_with_positions(root);
        let origins = GeneratedOrigins::from_print(ast, &output).unwrap();
        origins.validate_for_code(&output.code).unwrap();
        (output.code, origins)
    }

    #[test]
    fn sinking_keeps_definition_use_and_unrelated_call_origins() {
        let source = "local sum=left+right result=sum*2 output.setNumber(1,result)";
        let (mut ast, root) = parse_source_with_origins("controller.lua", source).unwrap();
        let result = inline_single_use_locals(&mut ast, root);
        let (code, origins) = printed(&ast, result.root);
        assert_eq!(code, "result=(left+right)*2 output.setNumber(1,result)");
        assert_eq!(origins.unknown_bytes(), 0);
        let moved = origins
            .origins
            .iter()
            .find(|origin| {
                origin
                    .primary
                    .is_some_and(|s| &source[s.start..s.end] == "left+right")
                    && origin
                        .related
                        .iter()
                        .any(|s| &source[s.start..s.end] == "sum")
            })
            .unwrap();
        assert_eq!(moved.kind, OriginKind::Derived);
        assert!(origins
            .origins
            .iter()
            .any(|o| o.name.as_deref() == Some("setNumber")));
        assert!(!origins
            .origins
            .iter()
            .any(|o| o.kind == OriginKind::Synthetic));
    }

    #[test]
    fn sinking_remaps_surviving_slots_after_removing_first_binding() {
        let source =
            "local discard,retained=left+right,8 result=discard*2 output.setNumber(1,retained)";
        let (mut ast, root) = parse_source_with_origins("controller.lua", source).unwrap();
        let result = inline_single_use_locals(&mut ast, root);
        let (code, origins) = printed(&ast, result.root);
        assert!(code.starts_with("local retained=8"), "{code}");
        assert!(!code.contains("discard"));
        let declaration = ast
            .nodes
            .iter()
            .position(|n| matches!(n, Node::Local(names,_) if names.len()==1))
            .unwrap() as NodeId;
        let retained = ast
            .nodes
            .name_origin(declaration, NameSite::Binding(0))
            .unwrap();
        assert_eq!(retained.name.as_deref(), Some("retained"));
        assert_eq!(
            retained.primary.unwrap().start,
            source.find("retained").unwrap()
        );
        assert_eq!(origins.unknown_bytes(), 0);
    }

    #[test]
    fn chain_substitution_keeps_each_definition_and_use() {
        let source = "local first=left+right local second=first*2 result=second+3";
        let (mut ast, root) = parse_source_with_origins("controller.lua", source).unwrap();
        let result = inline_single_use_locals(&mut ast, root);
        let (code, origins) = printed(&ast, result.root);
        assert_eq!(code, "result=(left+right)*2+3");
        assert_eq!(origins.unknown_bytes(), 0);
        for name in ["first", "second"] {
            assert!(
                origins
                    .origins
                    .iter()
                    .any(|o| o.related.iter().any(|s| &source[s.start..s.end] == name)),
                "missing use {name}"
            );
        }
    }

    #[test]
    fn unknown_expression_child_is_not_recovered_from_its_known_parent() {
        let source = "local sum=left+right result=sum*2";
        let (mut ast, root) = parse_source_with_origins("controller.lua", source).unwrap();
        let left = ast
            .nodes
            .iter()
            .position(|n| matches!(n, Node::Name(s) if ast.strings.get(*s)=="left"))
            .unwrap();
        // Simulate an unannotated earlier pass, even if it happens to write the same syntax.
        let value = ast.nodes[left].clone();
        ast.nodes[left] = value;
        let result = inline_single_use_locals(&mut ast, root);
        let (code, origins) = printed(&ast, result.root);
        let start = code.find("left").unwrap();
        assert!(origins
            .mappings
            .iter()
            .any(|m| m.start <= start && m.end >= start + 4 && m.origin.is_none()));
        assert_eq!(origins.unknown_bytes(), 4);
        assert!(origins
            .origins
            .iter()
            .any(|o| o.name.as_deref() == Some("right")));
    }

    #[test]
    fn literal_substitution_distinguishes_shadowed_definitions() {
        let source =
            "local value=1 do local value=2 output.setNumber(1,value)end output.setNumber(2,value)";
        let (mut ast, root) = parse_source_with_origins("controller.lua", source).unwrap();
        let result = inline_tiny_literal_bindings(&mut ast, root);
        let (code, origins) = printed(&ast, result.root);
        assert!(!code.contains("value"));
        assert_eq!(origins.unknown_bytes(), 0);
        let copies = origins
            .origins
            .iter()
            .filter(|o| o.transformation.as_deref() == Some("tiny-literal-use"))
            .collect::<Vec<_>>();
        assert_eq!(copies.len(), 2);
        for literal in ["1", "2"] {
            let definition = source.find(&format!("={literal}")).unwrap() + 1;
            assert!(copies.iter().any(|o| o.primary.unwrap().start == definition
                && o.related.iter().any(|s| &source[s.start..s.end] == "value")));
        }
    }
}
