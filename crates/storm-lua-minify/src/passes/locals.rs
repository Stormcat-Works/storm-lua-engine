//! Local binding cleanup passes (`passes/locals.ts`).

use crate::pass::PassResult;
use storm_lua_analysis::effects::{count_binding_uses, EffectAnalyzer};
use storm_lua_analysis::resolver::{resolve, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, TableField};
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
                    && effects.is_droppable(expressions[index]);
                if removable {
                    *changed = true;
                    None
                } else {
                    Some(index)
                }
            })
            .collect::<Vec<_>>();
        if keep.is_empty() {
            if target.nodes.tracks_origins() {
                target.nodes.explain_removal(
                    statement,
                    storm_lua_syntax::explanation::OptimizationReason::decision(
                        "dead-local-elimination",
                        "all-declared-bindings-unread-unwritten-and-initializers-movable",
                        [
                            ("removedBindings", names.len().to_string()),
                            ("reads", "0".into()),
                            ("writes", "0".into()),
                            ("initializerCheck", "EffectAnalyzer::is_droppable".into()),
                        ],
                    ),
                );
            }
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
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

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
    fn keeps_dead_local_whose_initializer_may_throw_on_dynamic_argument() {
        // Candidate O (w9 cycle 66): math.min is a TOTAL_API member,
        // but with a dynamically typed argument (an undefined global,
        // nil at run time) the call throws, so the declaration is not
        // droppable even though the local is never read.
        let out = output("local x = math.min(g, 2)", true);
        assert!(out.contains("local x="), "{out}");
    }

    #[test]
    fn keeps_dead_local_whose_constructor_key_may_throw() {
        // Q03: the constructor's own store throws for a nil key (and
        // may throw for a dynamically typed key), so the declaration
        // is not droppable even though the local is never read.
        let out = output("local t = {[nil]=1}", true);
        assert!(out.contains("local t="), "{out}");
        let out = output("local t = {[k]=1}", true);
        assert!(out.contains("local t="), "{out}");
        // A literal-key constructor cannot throw and stays droppable.
        let out = output("local t = {a=1}", true);
        assert!(!out.contains("local t="), "{out}");
    }

    #[test]
    fn keeps_dead_local_whose_initializer_is_a_recursive_call() {
        // Candidate W (w9 cycle 67): the summary fixpoint starts from
        // optimistic initial values, so a self-recursive function could
        // "prove" its own totality and its recursive call was dropped
        // here as droppable. The call may not return (unbounded
        // recursion ends in a stack overflow), so it stays.
        let out = output(
            "local function f() local v = f() output.setNumber(1, 1) end local x = f()",
            true,
        );
        assert!(out.contains("local v=f()"), "{out}");
        // Mutual recursion is downgraded the same way.
        let out = output(
            "local function f() local v = g() end local function g() local w = f() end local x = f()",
            true,
        );
        assert!(out.contains("local v=g()"), "{out}");
        // An acyclic call chain keeps its proven totality and is still
        // eliminated.
        let out = output(
            "local function g() return 1 end local function h() local v = g() return 2 end local x = h()",
            true,
        );
        assert!(!out.contains("local v="), "{out}");
        assert!(!out.contains("local x="), "{out}");
    }

    #[test]
    fn still_removes_dead_local_with_statically_total_call_initializer() {
        assert_eq!(output("local x = math.min(1, 2)", true), "");
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

fn remove_statement_from_blocks_with_code(
    ast: &mut Ast,
    statement: NodeId,
    code: &std::sync::Arc<str>,
) {
    ast.nodes
        .retain_block_statements_with_code(|id| id != statement, code);
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

/// Node ids of `self` occurrences inside a method body. At run
/// time they denote the method's implicit receiver parameter, not
/// the enclosing `self` binding the resolver attached them to (the
/// AST binds no implicit self parameter). Value-substitution passes
/// must never rewrite them (candidate S1, sibling of the phantom
/// implicit-self binding fixed for scalarization / flattening /
/// renaming in R277).
fn phantom_self_nodes(ast: &Ast, root: NodeId) -> std::collections::HashSet<NodeId> {
    fn collect(
        ast: &Ast,
        node: NodeId,
        method_depth: usize,
        out: &mut std::collections::HashSet<NodeId>,
    ) {
        if method_depth > 0 {
            if let Node::Name(symbol) = ast.node(node) {
                if ast.strings.get(*symbol) == "self" {
                    out.insert(node);
                }
            }
        }
        if let Node::Funcstat(target, function) = ast.node(node) {
            collect(ast, *target, method_depth, out);
            let inner = if matches!(ast.node(*target), Node::Methodname(..)) {
                method_depth + 1
            } else {
                method_depth
            };
            collect(ast, *function, inner, out);
            return;
        }
        storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
            collect(ast, child, method_depth, out);
        });
    }
    let mut out = std::collections::HashSet::new();
    collect(ast, root, 0, &mut out);
    out
}

fn replace_binding_with_literal(
    ast: &mut Ast,
    source: &Ast,
    res: &Resolution,
    bid: u32,
    literal: NodeId,
    phantom_self: &std::collections::HashSet<NodeId>,
) {
    let replacement = ast.node(literal).clone();
    let ids = (0..ast.nodes.len() as NodeId).collect::<Vec<_>>();
    for id in ids {
        if matches!(ast.node(id), Node::Name(_))
            && !phantom_self.contains(&id)
            && res.node_bid.get(id as usize).copied().flatten() == Some(bid)
        {
            ast.nodes[id as usize] = replacement.clone();
            ast.nodes
                .derive_from(id, &source.nodes, literal, "tiny-literal-copy");
            ast.nodes.relate_from_role(
                id,
                &source.nodes,
                id,
                "tiny-literal-use",
                storm_lua_syntax::explanation::RelationRole::UseSite,
            );
        }
    }
}

/// Binding ids that appear as the root name of a `function a.b()` /
/// `function a:b()` definition target anywhere below `root`.
///
/// A definition target must stay rooted at a name: substituting a literal
/// for its root name produces a target the printer renders unparenthesized
/// (`function 0.run()`), which no longer parses. Value-substitution passes
/// must therefore never replace these bindings' names (candidate K).
fn funcstat_target_root_bids(
    ast: &Ast,
    res: &Resolution,
    root: NodeId,
) -> std::collections::HashSet<u32> {
    let mut out = std::collections::HashSet::new();
    let mut nodes = Vec::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |id| nodes.push(id));
    for id in nodes {
        let Node::Funcstat(target, _) = ast.node(id) else {
            continue;
        };
        let mut node = *target;
        loop {
            match ast.node(node) {
                Node::Index(object, _, _) => node = *object,
                Node::Methodname(object, _) => node = *object,
                _ => break,
            }
        }
        if matches!(ast.node(node), Node::Name(_)) {
            if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
                out.insert(bid);
            }
        }
    }
    out
}

pub fn inline_tiny_literal_bindings(ast: &mut Ast, root: NodeId) -> PassResult {
    let original = measure_size(ast, root);
    for _ in 0..64 {
        let source = clone_ast(ast);
        let res = resolve(&source, root);
        let (reads, writes) = count_binding_uses(&source, &res, root);
        let funcstat_roots = funcstat_target_root_bids(&source, &res, root);
        let phantom_self = phantom_self_nodes(&source, root);
        let before = measure_size(&source, root);
        // Candidate enumeration is unchanged (same walk,
        // same order, same filters); only trial evaluation changes.
        let mut candidates: Vec<(u32, NodeId)> = Vec::new();
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
                if funcstat_roots.contains(bid)
                    || writes.get(bid).copied().unwrap_or(0) != 0
                    || !matches!(
                        source.node(literal),
                        Node::Num(_) | Node::Bool(_) | Node::Nil | Node::Str(_)
                    )
                    || storm_lua_syntax::size::measure_expr(&source, literal) > 2
                    || reads.get(bid).copied().unwrap_or(0) == 0
                {
                    continue;
                }
                candidates.push((*bid, literal));
            }
        }
        if candidates.is_empty() {
            break;
        }
        // One recycled trial arena for the whole round. A
        // trial is applied in place, measured, then every slot it wrote
        // is restored from the pristine round source (node values only;
        // the trial arena is never committed, so its provenance history
        // is irrelevant -- the winner is replayed onto `ast` below).
        let mut work = clone_ast(&source);
        let mut best: Option<(usize, u32, NodeId)> = None;
        for (bid, literal) in candidates {
            let touched = tiny_trial_touched_slots(&source, &res, root, bid, &phantom_self);
            replace_binding_with_literal(&mut work, &source, &res, bid, literal, &phantom_self);
            if !remove_local_binding(&mut work, &source, &res, bid) {
                restore_tiny_trial(&mut work, &source, &touched);
                continue;
            }
            let after = measure_size(&work, root);
            restore_tiny_trial(&mut work, &source, &touched);
            let gain = before.saturating_sub(after);
            if gain > 0 && best.as_ref().is_none_or(|(current, _, _)| gain > *current) {
                best = Some((gain, bid, literal));
            }
        }
        let Some((_, bid, literal)) = best else {
            break;
        };
        // Replay the winning trial onto the live arena.
        // The edit functions are deterministic in (arena values, source,
        // res), and `ast` holds exactly the round source values, so the
        // committed arena -- nodes and provenance alike -- equals the
        // fresh-clone trial the unmodified pass would have committed.
        replace_binding_with_literal(ast, &source, &res, bid, literal, &phantom_self);
        remove_local_binding(ast, &source, &res, bid);
    }
    PassResult {
        root,
        saved: Some(original.saturating_sub(measure_size(ast, root)) as u64),
        details: None,
    }
}

/// Every arena slot a tiny-literal trial for `bid` can
/// write, computed on the pristine round source: the read sites
/// `replace_binding_with_literal` rewrites (same predicate it uses),
/// the declaration statement `remove_local_binding` rewrites, and the
/// block(s) whose statement list contains the declaration (rewritten
/// by `retain_block_statements` when the declaration is removed
/// outright). Provenance-only writes (derive / relate / copy_name)
/// need no entry: the trial arena is discarded.
fn tiny_trial_touched_slots(
    source: &Ast,
    res: &Resolution,
    root: NodeId,
    bid: u32,
    phantom_self: &std::collections::HashSet<NodeId>,
) -> Vec<NodeId> {
    let mut touched: Vec<NodeId> = Vec::new();
    for id in 0..source.nodes.len() as NodeId {
        if matches!(source.node(id), Node::Name(_))
            && !phantom_self.contains(&id)
            && res.node_bid.get(id as usize).copied().flatten() == Some(bid)
        {
            touched.push(id);
        }
    }
    if let Some(statement) = res.binding(bid).decl_node {
        touched.push(statement);
        let mut all = Vec::new();
        storm_lua_syntax::ast_utils::walk(source, root, &mut |id| all.push(id));
        for id in all {
            if let Node::Block(statements) = source.node(id) {
                if statements.contains(&statement) {
                    touched.push(id);
                }
            }
        }
    }
    touched
}

/// Restore trial-written slots from the pristine source.
fn restore_tiny_trial(work: &mut Ast, source: &Ast, touched: &[NodeId]) {
    for id in touched {
        work.nodes[*id as usize] = source.node(*id).clone();
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

/// Whether evaluating `node` up to (but not including) the sole read
/// of `bid` can throw.  `node` must contain that read.  Subtrees that
/// evaluation reaches strictly before the read contribute their full
/// may_throw bit; subtrees reached only afterwards contribute nothing;
/// a function body is never entered (creating the closure evaluates
/// nothing inside it).  Children are visited in Lua evaluation order.
fn prefix_may_throw(
    ast: &Ast,
    res: &Resolution,
    effects: &EffectAnalyzer<'_>,
    node: NodeId,
    bid: u32,
) -> bool {
    let contains = |id: NodeId| storm_lua_analysis::effects::contains_binding(ast, res, id, bid);
    if matches!(ast.node(node), Node::Name(_))
        && res.node_bid.get(node as usize).copied().flatten() == Some(bid)
    {
        return false;
    }
    // A sequence of subexpressions evaluated in order: accumulate the
    // may_throw bits of the items before the one holding the read.
    let seq = |items: &[NodeId]| -> bool {
        let mut acc = false;
        for &item in items {
            if contains(item) {
                return acc || prefix_may_throw(ast, res, effects, item, bid);
            }
            acc |= effects.effects_for_expr(item).may_throw;
        }
        acc
    };
    match ast.node(node) {
        Node::Function(..) => false,
        Node::Paren(inner) | Node::Un(_, inner) => prefix_may_throw(ast, res, effects, *inner, bid),
        Node::Methodname(obj, _) => prefix_may_throw(ast, res, effects, *obj, bid),
        Node::Bin(_, l, r) => {
            if contains(*l) {
                prefix_may_throw(ast, res, effects, *l, bid)
            } else {
                effects.effects_for_expr(*l).may_throw
                    || prefix_may_throw(ast, res, effects, *r, bid)
            }
        }
        Node::Index(obj, key, _) => seq(&[*obj, *key]),
        Node::Call(function, args, _) => {
            let mut items = Vec::with_capacity(args.len() + 1);
            items.push(*function);
            items.extend(args.iter().copied());
            seq(&items)
        }
        Node::Table(fields) => {
            let mut items = Vec::new();
            for field in fields {
                match field {
                    storm_lua_syntax::TableField::Arr(value) => items.push(*value),
                    storm_lua_syntax::TableField::Name(_, value) => items.push(*value),
                    storm_lua_syntax::TableField::KVar(key, value) => {
                        items.push(*key);
                        items.push(*value);
                    }
                }
            }
            seq(&items)
        }
        Node::Local(_, expressions) | Node::Return(expressions) => seq(expressions),
        Node::Assign(targets, expressions) => {
            // A read inside a target's base/key expression is evaluated
            // interleaved with the right-hand side in ways this walk
            // does not model; stay conservative there.
            if targets.iter().any(|target| contains(*target)) {
                return effects.effects_for_statement(node).may_throw;
            }
            targets
                .iter()
                .any(|target| effects.effects_for_expr(*target).may_throw)
                || seq(expressions)
        }
        _ => {
            // Any other shape (a statement kind or an expression form
            // without a modeled evaluation order): the whole node's
            // may_throw bit, the safe direction.
            if matches!(ast.node(node), Node::Block(_)) {
                false
            } else if is_statement_node(ast.node(node)) {
                effects.effects_for_statement(node).may_throw
            } else {
                effects.effects_for_expr(node).may_throw
            }
        }
    }
}

/// Whether `node` is a statement (as opposed to an expression), for the
/// conservative fallback in [`prefix_may_throw`].
fn is_statement_node(node: &Node) -> bool {
    matches!(
        node,
        Node::Block(_)
            | Node::Local(..)
            | Node::Localfunc(..)
            | Node::Assign(..)
            | Node::Callstat(..)
            | Node::Return(..)
            | Node::If(..)
            | Node::While(..)
            | Node::Repeat(..)
            | Node::Fornum(..)
            | Node::Forin(..)
            | Node::Do(..)
            | Node::Break
            | Node::Goto(..)
            | Node::Label(..)
            | Node::Funcstat(..)
    )
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
    let mut scratch = storm_lua_analysis::effects::empty_effects();
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
            //
            // The same ordering argument applies to errors: aggressive
            // sinking may move a may-throw initializer into the target,
            // but only when no evaluation inside the target that
            // precedes the read can throw.  If such an evaluation
            // throws, that error surfaces instead and the initializer's
            // own error is lost (candidate V, w9 cycle 67).  Throws at
            // or after the read position are unaffected: the sunk
            // initializer still evaluates first there.
            effects.effects_for_statement_into(target, &mut scratch);
            let target_effect = &scratch;
            if target_effect.calls || target_effect.ordered {
                continue;
            }
            let target_prefix_may_throw = prefix_may_throw(ast, res, effects, target, *bid);
            effects.effects_for_expr_into(expression, &mut scratch);
            let effect = &scratch;
            if effect.ordered
                || effect.calls
                || !effect.writes.is_empty()
                || (effect.may_throw && (!aggressive || target_prefix_may_throw))
                || !effect.stable
            {
                continue;
            }
            // A component of a multi-value declaration is evaluated in
            // source order together with its siblings. Sinking it into
            // the target moves it after every sibling that stays in the
            // declaration, so when the candidate itself can throw, each
            // sibling must be unobservable (no throw, call, write, or
            // ordered effect): a sibling that originally evaluated after
            // the candidate -- and never ran when the candidate threw --
            // would otherwise run first and surface its own error or
            // effects instead (candidate AB, w9 cycle 68).
            if effect.may_throw && expressions.len() > 1 {
                let mut sibling_scratch = storm_lua_analysis::effects::empty_effects();
                let siblings_quiet = expressions.iter().enumerate().all(|(other, sibling)| {
                    if other == index {
                        return true;
                    }
                    effects.effects_for_expr_into(*sibling, &mut sibling_scratch);
                    !sibling_scratch.may_throw
                        && !sibling_scratch.calls
                        && !sibling_scratch.ordered
                        && sibling_scratch.writes.is_empty()
                });
                if !siblings_quiet {
                    continue;
                }
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

/// A direct multi-value slot of the parent: the last argument of a call, the
/// last positional field of a constructor, the last value of a return list,
/// the last initializer of a local/assign, or the last iterator expression of
/// a generic for. Any deeper nesting (operands, indexes, non-last slots,
/// parentheses) already adjusts to one value, so only the direct slot
/// matters, and each level recomputes it for its own children. This mirrors
/// the expanding-slot guard in single-use global forwarding: a sunk local
/// held exactly one adjusted value, so a multi-value-capable definition
/// (a call or `...`) substituted into an expanding slot must be
/// parenthesized, or it would yield more values than the original
/// (e.g. `local x = pair(); local c, d = x` becoming `local c, d = pair()`
/// with `d` taking the second value instead of nil).
fn parenthesize_if_multi_value(
    target: &mut Ast,
    source: &Ast,
    id: NodeId,
    copied: NodeId,
    expanding: bool,
) -> NodeId {
    if expanding && matches!(target.node(copied), Node::Call(..) | Node::Vararg) {
        let paren = target.push(Node::Paren(copied));
        // A synthetic paren has no origin of its own; derive it from the
        // replaced read so the provenance audit covers "(" and ")" (the
        // same capture-then-finish pattern single-use global forwarding
        // uses for its parenthesized replacements).
        target
            .nodes
            .derive_from(paren, &source.nodes, id, "single-use-local-substitution");
        paren
    } else {
        copied
    }
}

fn map_expanding_children(node: &Node, f: &mut dyn FnMut(NodeId, bool) -> NodeId) -> Node {
    match node {
        Node::Call(function, arguments, method) => {
            let last = arguments.len().saturating_sub(1);
            Node::Call(
                f(*function, false),
                arguments
                    .iter()
                    .enumerate()
                    .map(|(index, argument)| f(*argument, index == last))
                    .collect(),
                *method,
            )
        }
        Node::Table(fields) => {
            let last = fields.len().saturating_sub(1);
            Node::Table(
                fields
                    .iter()
                    .enumerate()
                    .map(|(index, field)| match field {
                        TableField::Arr(value) => TableField::Arr(f(*value, index == last)),
                        TableField::Name(key, value) => TableField::Name(*key, f(*value, false)),
                        TableField::KVar(key, value) => {
                            TableField::KVar(f(*key, false), f(*value, false))
                        }
                    })
                    .collect(),
            )
        }
        Node::Return(values) => {
            let last = values.len().saturating_sub(1);
            Node::Return(
                values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| f(*value, index == last))
                    .collect(),
            )
        }
        Node::Local(names, expressions) => {
            let last = expressions.len().saturating_sub(1);
            Node::Local(
                names.clone(),
                expressions
                    .iter()
                    .enumerate()
                    .map(|(index, value)| f(*value, index == last))
                    .collect(),
            )
        }
        Node::Assign(targets, expressions) => {
            let last = expressions.len().saturating_sub(1);
            Node::Assign(
                targets.iter().map(|target| f(*target, false)).collect(),
                expressions
                    .iter()
                    .enumerate()
                    .map(|(index, value)| f(*value, index == last))
                    .collect(),
            )
        }
        Node::Forin(bindings, expressions, body) => {
            let last = expressions.len().saturating_sub(1);
            Node::Forin(
                bindings.clone(),
                expressions
                    .iter()
                    .enumerate()
                    .map(|(index, value)| f(*value, index == last))
                    .collect(),
                f(*body, false),
            )
        }
        other => {
            let (mapped, _) =
                storm_lua_syntax::ast_utils::map_children(other, &mut |child| f(child, false));
            mapped
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn clone_expanded(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    replacements: &std::collections::HashMap<u32, NodeId>,
    id: NodeId,
    seen: &std::collections::HashSet<u32>,
    expanding: bool,
    read_code: &std::sync::Arc<str>,
) -> NodeId {
    if matches!(source.node(id), Node::Name(_)) {
        if let Some(bid) = res.node_bid.get(id as usize).copied().flatten() {
            if let Some(replacement) = replacements.get(&bid) {
                if !seen.contains(&bid) {
                    let mut next = seen.clone();
                    next.insert(bid);
                    let copied = clone_expanded(
                        target,
                        source,
                        res,
                        replacements,
                        *replacement,
                        &next,
                        expanding,
                        read_code,
                    );
                    let copied = parenthesize_if_multi_value(target, source, id, copied, expanding);
                    target.nodes.relate_from_role_code(
                        copied,
                        &source.nodes,
                        id,
                        read_code,
                        storm_lua_syntax::explanation::RelationRole::UseSite,
                    );
                    return copied;
                }
            }
        }
    }
    let node = source.node(id).clone();
    let mapped = map_expanding_children(&node, &mut |child, child_expanding| {
        clone_expanded(
            target,
            source,
            res,
            replacements,
            child,
            seen,
            child_expanding,
            read_code,
        )
    });
    let copied = target.push(mapped);
    // A clone, not a replacement: replaced subtrees shift later child IDs, so
    // the value-equality fast path in `derive_from` would miss almost every
    // node after the first replacement. Replaced nodes above keep
    // `relate_from_role` so they still record their use-site relation.
    target.nodes.derive_from_clone(
        copied,
        &source.nodes,
        id,
        "single-use-local-expression-copy",
    );
    copied
}

#[allow(clippy::too_many_arguments)]
fn expand_tree(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    replacements: &std::collections::HashMap<u32, NodeId>,
    id: NodeId,
    expanding: bool,
    substitution_code: &std::sync::Arc<str>,
    read_code: &std::sync::Arc<str>,
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
                    expanding,
                    read_code,
                );
                let copied = parenthesize_if_multi_value(target, source, id, copied, expanding);
                target.nodes.relate_from_role_code(
                    copied,
                    &source.nodes,
                    id,
                    read_code,
                    storm_lua_syntax::explanation::RelationRole::UseSite,
                );
                return copied;
            }
        }
    }
    let node = source.node(id).clone();
    let mapped = map_expanding_children(&node, &mut |child, child_expanding| {
        expand_tree(
            target,
            source,
            res,
            replacements,
            child,
            child_expanding,
            substitution_code,
            read_code,
        )
    });
    target
        .nodes
        .rewrite_with_code(id, mapped, substitution_code);
    id
}

fn strip_removed_locals(
    ast: &mut Ast,
    source: &Ast,
    res: &Resolution,
    remove: &std::collections::HashSet<u32>,
    removal_code: &std::sync::Arc<str>,
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
            remove_statement_from_blocks_with_code(ast, statement, removal_code);
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
    // One shared code string per rule for the whole pass: the recorded
    // origins are identical by string content; only the per-node fresh
    // `Arc<str>` allocations are removed (same pattern as the
    // root-local-globalization reason sharing).
    let substitution_code: std::sync::Arc<str> =
        std::sync::Arc::from("single-use-local-substitution");
    let read_code: std::sync::Arc<str> = std::sync::Arc::from("single-use-local-read");
    // Shared with every strip call in this pass (same pattern as the two
    // codes above): the removal reason string is identical by content.
    let removal_code: std::sync::Arc<str> =
        std::sync::Arc::from("unused-local-declaration-removal");
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
        expand_tree(
            &mut trial,
            &source,
            &res,
            &replacements,
            root,
            false,
            &substitution_code,
            &read_code,
        );
        strip_removed_locals(&mut trial, &source, &res, &remove, &removal_code);
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
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

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
    fn tiny_literal_keeps_funcstat_target_root() {
        // Candidate K: the base of a definition target must stay a name;
        // substituting the literal made every candidate unparseable.
        let out = output("local a=0 function a.b() return 1 end", true);
        assert!(out.contains("function a.b()"), "{out}");
        let out = output("local abc=\"\" function abc.b() return 1 end", true);
        assert!(out.contains("function abc.b()"), "{out}");
        let out = output("local a=0 function a:b() return 1 end", true);
        assert!(out.contains("function a:b()"), "{out}");
        // An unrelated binding used only in reads is still inlined.
        let out = output("local a=0 local b={} function b.c() return a end", true);
        assert!(out.contains("return 0"), "{out}");
        assert!(out.contains("function b.c()"), "{out}");
    }

    #[test]
    fn tiny_literal_never_substitutes_phantom_self() {
        // Candidate S1 (w6 cycle 65; sibling of candidate AA / R277):
        // a `self` read inside a method body denotes the implicit
        // receiver, not the enclosing tiny-literal `self` binding the
        // resolver attaches it to. Substituting the literal made the
        // method compare the literal against its own table.
        let out = output(
            "local self=42 local t={} function t:m() return self==t end return self",
            true,
        );
        assert!(out.contains("self==t"), "{out}");
        assert!(!out.contains("42==t"), "{out}");
        // The genuine chunk-level read is still inlined.
        assert!(out.ends_with("return 42"), "{out}");
        // A closure nested inside the method captures the receiver
        // the same way.
        let out = output(
            "local self=42 local t={} function t:m() local f=function() return self end return f() end",
            true,
        );
        assert!(out.contains("return self"), "{out}");
        assert!(!out.contains("return 42"), "{out}");
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

    #[test]
    fn does_not_sink_may_throw_initializer_into_throwing_target() {
        // Candidate V (w9 cycle 67): the string.format initializer
        // throws (the %d conversion has no argument).  Sinking it into
        // the second operand of the target expression lets the first
        // operand (`a * i`, a boolean times a number) throw a different
        // error first, so the format error was lost.  The declaration
        // must stay in place.
        let out = output(
            "for i = 1, 5 do local a, b = true, string.format(\"%s-%d\", (not false)) local c = a * i + b * i end",
            false,
        );
        assert!(out.contains("local b=string.format"), "{out}");
        // A may-throw initializer still sinks into a target that cannot
        // throw: its error then surfaces at the read, unchanged.
        let out = output(
            "local b = string.format(\"%s-%d\", (not false)) local c = b",
            false,
        );
        assert!(!out.contains("local b="), "{out}");
        assert!(out.contains("string.format"), "{out}");
    }

    #[test]
    fn does_not_sink_may_throw_component_past_throwing_sibling() {
        // Candidate AB (w9 cycle 68): in a multi-value declaration the
        // components evaluate in source order.  Sinking the first
        // component (may-throw: `#T` on a nil global) into its use moved
        // it after the second component (`T .. "x"`, also may-throw),
        // so the concatenation error surfaced instead of the length
        // error.  The declaration must stay in place.
        let out = output(
            "local a, b, c = math.max(2, #T), (T .. \"x\"), math.max(3, 4) local d = a * c + c",
            false,
        );
        assert!(out.contains("local a,b,c=math.max(2,#T)"), "{out}");
        // A may-throw component still sinks when every sibling is
        // unobservable.
        let out = output("local a, b = math.max(2, #T), 4 local d = a + b", false);
        assert!(!out.contains("local a="), "{out}");
    }

    #[test]
    fn sinks_call_into_expanding_local_parenthesized() {
        // A sunk local held exactly one adjusted value; in a multi-name
        // local list the bare call would expand to all its values.
        let out = output(
            "local function pair() return 7,8,9 end local x=pair() local c,d=x",
            false,
        );
        assert!(out.contains("local c,d=(pair())"), "{out}");
        assert!(!out.contains("local x="), "{out}");
    }

    #[test]
    fn sinks_call_into_non_expanding_position_bare() {
        // A non-last slot already adjusts to one value, so no parens
        // are needed there.
        let out = output(
            "local function pair() return 7,8,9 end local x=pair() local y=x+1",
            false,
        );
        assert!(out.contains("pair()+1"), "{out}");
        assert!(!out.contains("(pair())"), "{out}");
    }

    #[test]
    fn sinks_call_into_expanding_return_parenthesized() {
        let out = output(
            "local function pair() return 7,8,9 end local x=pair() return x",
            false,
        );
        assert!(out.contains("return(pair())"), "{out}");
    }

    #[test]
    fn sinks_vararg_into_expanding_local_parenthesized() {
        let out = output(
            "local function f(...) local x=... local c,d=x return c end",
            false,
        );
        assert!(out.contains("local c,d=(...)"), "{out}");
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
