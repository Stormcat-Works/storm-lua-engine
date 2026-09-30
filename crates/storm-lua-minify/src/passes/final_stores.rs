//! Final overwritten assignment elimination (`passes/final-stores.ts`).
//!
//! This is a conservative backwards liveness pass. Non-fixed globals are live
//! at every callback exit, straight-line assignments can be dropped when their
//! values are overwritten before a read, and `if`/`do` recurse. Loops and jumps
//! remain conservative effect barriers because they require a CFG fixed point.

use std::collections::HashSet;

use crate::pass::PassResult;
use storm_lua_analysis::effects::{EffectAnalyzer, Effects};
use storm_lua_analysis::resolver::{resolve, BindingKind, Resolution, ScopeId};
use storm_lua_syntax::ast::{Ast, IfArm, Node, NodeId};
use storm_lua_syntax::size::measure_size;

/// 各スコープを「所有する関数」の own scope id へ写す(root chunk 直下は `None`)。
/// スコープは親が必ず先に生成されている(id が単調増加)ため、単純な前方 DP で求まる。
fn owning_function_scopes(source: &Ast, res: &Resolution) -> Vec<Option<ScopeId>> {
    let function_scopes: HashSet<ScopeId> = (0..source.nodes.len() as NodeId)
        .filter(|&id| matches!(source.node(id), Node::Function(..)))
        .filter_map(|id| res.node_scope_id[id as usize])
        .collect();
    let mut owner = vec![None; res.scopes.len()];
    for scope in &res.scopes {
        let sid = scope.id as usize;
        owner[sid] = if function_scopes.contains(&scope.id) {
            Some(scope.id)
        } else {
            scope.parent.and_then(|p| owner[p as usize])
        };
    }
    owner
}

/// 関数境界を跨いで参照される binding (root-scope の共有 local、ネストしたクロージャが
/// 捕捉する外側 local など) を集める。これらは「宣言した関数」単体の後方解析だけでは
/// dead 判定できない — 別の関数呼び出しから読まれ得るため、`persistent_globals` と同様
/// 常に生存扱いにする(D-... 参照: onTick/onDraw 等のコールバックは別タイミングで呼ばれ、
/// トップレベル local はそれらの間で共有されるアップバリューになる)。
fn captured_across_functions(source: &Ast, res: &Resolution) -> HashSet<u32> {
    let owner = owning_function_scopes(source, res);
    let mut captured = HashSet::new();
    for scope in &res.scopes {
        for &bid in &scope.refs {
            let binding = &res.bindings[bid as usize];
            if owner[scope.id as usize] != owner[binding.scope as usize] {
                captured.insert(bid);
            }
        }
    }
    captured
}

fn union_into(target: &mut HashSet<u32>, values: impl IntoIterator<Item = u32>) {
    target.extend(values);
}

fn transfer_effects(live: &mut HashSet<u32>, effects: &Effects) {
    for bid in &effects.writes {
        live.remove(bid);
    }
    union_into(live, effects.reads.iter().copied());
}

fn droppable(analyzer: &EffectAnalyzer<'_>, expression: NodeId, aggressive: bool) -> bool {
    let effect = analyzer.effects_for_expr(expression);
    !effect.calls
        && !effect.ordered
        && effect.writes.is_empty()
        && (!effect.may_throw || aggressive)
}

fn transfer_kept_assignment(
    ast: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    live: &mut HashSet<u32>,
    targets: &[NodeId],
    expressions: &[NodeId],
) {
    let mut writes = Vec::new();
    let mut reads = Vec::new();
    for (target, expression) in targets.iter().zip(expressions) {
        if let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() {
            writes.push(bid);
        }
        let effect = analyzer.effects_for_expr(*expression);
        writes.extend(effect.writes);
        reads.extend(effect.reads);
    }
    writes.sort_unstable();
    writes.dedup();
    reads.sort_unstable();
    reads.dedup();
    for bid in writes {
        live.remove(&bid);
    }
    union_into(live, reads);
    let _ = ast;
}

struct CleanResult {
    block: NodeId,
    live_in: HashSet<u32>,
    removed: usize,
}

fn clean_block(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    block: NodeId,
    live_out: &HashSet<u32>,
    aggressive: bool,
) -> CleanResult {
    let Node::Block(statements) = source.node(block).clone() else {
        unreachable!("clean_block requires a block")
    };
    let mut live = live_out.clone();
    let mut output = Vec::with_capacity(statements.len());
    let mut removed = 0usize;

    for statement in statements.into_iter().rev() {
        if let Node::Assign(targets, expressions) = source.node(statement) {
            if targets.len() == expressions.len()
                && targets
                    .iter()
                    .all(|target_id| matches!(source.node(*target_id), Node::Name(_)))
            {
                let rhs_effects = expressions
                    .iter()
                    .map(|expression| analyzer.effects_for_expr(*expression))
                    .collect::<Vec<_>>();
                let mut keep = Vec::new();
                for index in 0..targets.len() {
                    let target_id = targets[index];
                    let expression = expressions[index];
                    let target_bid = res.node_bid.get(target_id as usize).copied().flatten();
                    let self_assignment = matches!(source.node(expression), Node::Name(_))
                        && target_bid.is_some()
                        && res.node_bid.get(expression as usize).copied().flatten() == target_bid
                        // Parallel RHS expressions are all evaluated before
                        // writes.  `x,y=x,g()` cannot drop `x=x` when g writes
                        // x, because the retained y assignment would then
                        // expose g's value instead of the saved old x.
                        && !rhs_effects.iter().enumerate().any(|(rhs_index, effect)| {
                            rhs_index != index
                                && target_bid.is_some_and(|bid| effect.writes.contains(&bid))
                        });
                    let dead = target_bid.is_some_and(|bid| !live.contains(&bid))
                        && droppable(analyzer, expression, aggressive);
                    if self_assignment || dead {
                        removed += 1;
                    } else {
                        keep.push(index);
                    }
                }
                if !keep.is_empty() {
                    let kept_targets = keep.iter().map(|index| targets[*index]).collect::<Vec<_>>();
                    let kept_expressions = keep
                        .iter()
                        .map(|index| expressions[*index])
                        .collect::<Vec<_>>();
                    target.nodes[statement as usize] =
                        Node::Assign(kept_targets.clone(), kept_expressions.clone());
                    transfer_kept_assignment(
                        source,
                        res,
                        analyzer,
                        &mut live,
                        &kept_targets,
                        &kept_expressions,
                    );
                    output.push(statement);
                }
                continue;
            }
        }

        match source.node(statement).clone() {
            Node::If(arms, else_block) => {
                let mut branch_inputs = Vec::new();
                let mut rewritten_arms = Vec::with_capacity(arms.len());
                for arm in &arms {
                    let result =
                        clean_block(target, source, res, analyzer, arm.body, &live, aggressive);
                    removed += result.removed;
                    branch_inputs.push(result.live_in);
                    rewritten_arms.push(IfArm {
                        cond: arm.cond,
                        body: result.block,
                    });
                }
                let rewritten_else = if let Some(else_id) = else_block {
                    let result =
                        clean_block(target, source, res, analyzer, else_id, &live, aggressive);
                    removed += result.removed;
                    branch_inputs.push(result.live_in);
                    Some(result.block)
                } else {
                    branch_inputs.push(live.clone());
                    None
                };
                let mut merged = HashSet::new();
                for branch in branch_inputs {
                    union_into(&mut merged, branch);
                }
                for arm in &arms {
                    union_into(&mut merged, analyzer.effects_for_expr(arm.cond).reads);
                }
                live = merged;
                target.nodes[statement as usize] = Node::If(rewritten_arms, rewritten_else);
                output.push(statement);
            }
            Node::Do(body) => {
                let result = clean_block(target, source, res, analyzer, body, &live, aggressive);
                live = result.live_in;
                removed += result.removed;
                target.nodes[statement as usize] = Node::Do(result.block);
                output.push(statement);
            }
            Node::While(..) | Node::Repeat(..) | Node::Fornum(..) | Node::Forin(..) => {
                // A loop may execute zero times.  A write seen in its body is
                // therefore a may-write, so the pre-loop value is live at the
                // loop entry even when it is overwritten on every iteration.
                let effect = analyzer.effects_for_statement(statement);
                transfer_effects(&mut live, &effect);
                live.extend(effect.writes.iter().copied());
                output.push(statement);
            }
            _ => {
                transfer_effects(&mut live, &analyzer.effects_for_statement(statement));
                output.push(statement);
            }
        }
    }
    output.reverse();
    target
        .nodes
        .rewrite(block, Node::Block(output), "final-dead-store-elimination");
    CleanResult {
        block,
        live_in: live,
        removed,
    }
}

#[allow(clippy::too_many_arguments)]
fn rewrite_functions(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    persistent_globals: &HashSet<u32>,
    id: NodeId,
    aggressive: bool,
    removed: &mut usize,
) -> NodeId {
    if let Node::Function(params, vararg, body) = source.node(id).clone() {
        let result = clean_block(
            target,
            source,
            res,
            analyzer,
            body,
            persistent_globals,
            aggressive,
        );
        *removed += result.removed;
        target.nodes.rewrite(
            id,
            Node::Function(params, vararg, result.block),
            "final-dead-store-elimination",
        );
        return id;
    }
    let node = source.node(id).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&node, &mut |child| {
        rewrite_functions(
            target,
            source,
            res,
            analyzer,
            persistent_globals,
            child,
            aggressive,
            removed,
        )
    });
    target
        .nodes
        .rewrite(id, mapped, "final-dead-store-elimination");
    id
}

#[allow(clippy::too_many_arguments)]
pub fn eliminate_overwritten_assignments_with_options(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
) -> PassResult {
    let before = measure_size(ast, root);
    let original_nodes = ast.nodes.clone();
    let mut source = storm_lua_syntax::ast_utils::inherit_ast(ast);
    source.nodes = original_nodes.clone();
    let res = resolve(&source, root);
    let analyzer = EffectAnalyzer::new(&source, &res, root, aggressive);
    let mut persistent_globals = res
        .bindings
        .iter()
        .enumerate()
        .filter_map(|(bid, binding)| {
            (matches!(binding.kind, BindingKind::Global) && !binding.fixed).then_some(bid as u32)
        })
        .collect::<HashSet<_>>();
    persistent_globals.extend(captured_across_functions(&source, &res));
    let mut removed = 0usize;
    let out = rewrite_functions(
        ast,
        &source,
        &res,
        &analyzer,
        &persistent_globals,
        root,
        aggressive,
        &mut removed,
    );
    let after = measure_size(ast, out);
    if after <= before {
        PassResult {
            root: out,
            saved: Some(before.saturating_sub(after) as u64),
            details: if removed == 0 {
                None
            } else {
                Some(vec![format!("removed={removed}")])
            },
        }
    } else {
        ast.nodes = original_nodes;
        PassResult {
            root,
            saved: Some(0),
            details: None,
        }
    }
}

pub fn eliminate_overwritten_assignments(ast: &mut Ast, root: NodeId) -> PassResult {
    eliminate_overwritten_assignments_with_options(ast, root, true)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use storm_lua_syntax::parser::parse_source;
    use storm_lua_syntax::print::Printer;

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = eliminate_overwritten_assignments(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn removes_overwritten_local_assignment_inside_function() {
        assert_eq!(
            output("function f() local x x=1 x=2 return x end"),
            "function f()local x x=2 return x end"
        );
    }

    #[test]
    fn preserves_persistent_global_at_callback_exit() {
        let out = output("function onTick() state=1 end");
        assert!(out.contains("state=1"), "{out}");
    }

    #[test]
    fn removes_self_assignment() {
        let out = output("function f() local x=1 x=x return x end");
        assert!(!out.contains("x=x"), "{out}");
    }

    #[test]
    fn keeps_effectful_dead_value() {
        let out = output("function f() local x x=g() x=2 return x end");
        assert!(out.contains("x=g()"), "{out}");
    }

    #[test]
    fn keeps_write_to_root_local_shared_across_callbacks() {
        // onTick/onDraw はランタイムが別タイミングで呼ぶコールバックであり、root-scope
        // の local `p` はその間で共有されるアップバリューになる。onTick 単体の後方解析
        // では読まれないように見えても、onDraw から読まれるため dead ではない。
        let out = output(
            "local p function onTick() p=input.getNumber(2) end \
             function onDraw() screen.drawText(0,0,p) end",
        );
        assert!(
            out.contains("function onTick()p=input.getNumber(2)end"),
            "{out}"
        );
    }

    #[test]
    fn keeps_write_to_local_captured_by_nested_closure() {
        // x は F 内で宣言されるが、ネストしたクロージャ g が捕捉して後から読む。
        // Node::Function は effects 上不透明(empty_effects)なので、g の生成文だけでは
        // x への読みとして数えられない — captured_across_functions で保護する。
        let out = output("function f() local x local g=function() return x end x=1 return g end");
        assert!(out.contains("x=1"), "{out}");
    }
}
