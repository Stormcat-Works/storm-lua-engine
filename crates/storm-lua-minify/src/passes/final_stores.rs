//! Final overwritten assignment elimination (`passes/final-stores.ts`).
//!
//! This is a conservative backwards liveness pass. Non-fixed globals are live
//! at every callback exit, straight-line assignments can be dropped when their
//! values are overwritten before a read, and `if`/`do` recurse. Loops and jumps
//! remain conservative effect barriers because they require a CFG fixed point.
//! A `return` resets liveness to the function-exit set: a store before a
//! return inside a branch must not be judged against the fall-through
//! liveness, where a later overwrite on the fall-through path may kill it,
//! because the return path exits the function while the stored value is
//! still live for later callbacks and invocations.

use std::collections::HashSet;

use crate::pass::PassResult;
use storm_lua_analysis::effects::{empty_effects, EffectAnalyzer, Effects};
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
fn captured_across_functions(source: &Ast, res: &Resolution) -> LiveSet {
    let owner = owning_function_scopes(source, res);
    let mut captured = LiveSet::new(res.bindings.len());
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

/// Live-binding set for the backwards scan, as a dense bitset over
/// binding ids.
///
/// The scan consumes this set only through membership tests, removal,
/// insertion, union and clone; it never iterates the elements (the only
/// iteration over a set's contents is as the source of a union, whose
/// result depends on content alone), and it never reads the cardinality.
/// The contents are therefore a pure function of the program, so the
/// representation -- and any iteration order -- is unobservable in the
/// pass output (audit recorded in
/// `docs/verification/compile-cpu-final-stores-liveset-20261101.md`).
/// A bitset makes clone a word copy and every update a single word
/// operation, where the previous `HashSet<u32>` paid a hash per element
/// per operation (cycle-89 instrumentation: 646,696 clones copying
/// 40,939,083 elements in one full std-nomap corpus run).
#[derive(Clone, Default)]
struct LiveSet {
    words: Vec<u64>,
}

impl LiveSet {
    fn new(num_bindings: usize) -> Self {
        Self {
            words: vec![0; num_bindings.div_ceil(64)],
        }
    }

    /// An empty set with the same universe size as `self`.
    fn zeros_like(&self) -> Self {
        Self {
            words: vec![0; self.words.len()],
        }
    }

    fn contains(&self, bid: u32) -> bool {
        self.words
            .get((bid >> 6) as usize)
            .is_some_and(|word| word & (1u64 << (bid & 63)) != 0)
    }

    fn insert(&mut self, bid: u32) {
        let word = (bid >> 6) as usize;
        if word >= self.words.len() {
            self.words.resize(word + 1, 0);
        }
        self.words[word] |= 1u64 << (bid & 63);
    }

    fn remove(&mut self, bid: u32) {
        if let Some(word) = self.words.get_mut((bid >> 6) as usize) {
            *word &= !(1u64 << (bid & 63));
        }
    }

    fn extend(&mut self, bids: impl IntoIterator<Item = u32>) {
        for bid in bids {
            self.insert(bid);
        }
    }

    fn union_from(&mut self, other: &LiveSet) {
        if other.words.len() > self.words.len() {
            self.words.resize(other.words.len(), 0);
        }
        for (word, other_word) in self.words.iter_mut().zip(&other.words) {
            *word |= *other_word;
        }
    }
}

/// Backwards transfer for a statement in a position where any binding
/// write in its effects can only arrive through a called function's
/// summary (a call statement has no direct assignment, and neither does a
/// return expression).  Summary writes are may-writes: whether they happen
/// is decided by the callee's own control flow at run time, so a value
/// stored before the call may still be the value observed after it.
/// Killing such bindings — as a straight-line assignment's definite write
/// may be killed — drops stores that are observable whenever the callee
/// does not overwrite them.  This mirrors the loop handling below, which
/// likewise re-adds a loop body's writes.  Note the summary of a callee
/// that only uses pure APIs carries `calls == false`, so the presence of
/// a call cannot be detected from the effects alone; the statement's
/// position is what guarantees the writes are call-mediated.
fn transfer_effects_may_write(live: &mut LiveSet, effects: &Effects) {
    live.extend(effects.reads.iter().copied());
    live.extend(effects.writes.iter().copied());
}

fn transfer_effects(live: &mut LiveSet, effects: &Effects) {
    for bid in &effects.writes {
        live.remove(*bid);
    }
    live.extend(effects.reads.iter().copied());
}

fn droppable(
    ast: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    expression: NodeId,
    aggressive: bool,
) -> bool {
    let effect = analyzer.effects_for_expr(expression);
    !effect.calls
        && !effect.ordered
        && effect.writes.is_empty()
        && (!effect.may_throw
            || (aggressive
                && super::global_stores::total_numeric_expression(ast, analyzer, expression)))
}

fn transfer_kept_assignment(
    ast: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    live: &mut LiveSet,
    targets: &[NodeId],
    expressions: &[NodeId],
) {
    let mut reads = Vec::new();
    for expression in expressions {
        let effect = analyzer.effects_for_expr(*expression);
        reads.extend(effect.reads);
    }
    reads.sort_unstable();
    reads.dedup();
    // Only the assignment's own targets are definite writes.  Writes that
    // arrive through a call in a right-hand expression are may-writes (the
    // callee may not perform them), so they must not kill a binding whose
    // earlier store stays observable on the paths where the call does not
    // overwrite it.
    let mut target_writes = Vec::new();
    for target in targets {
        if let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() {
            target_writes.push(bid);
        }
    }
    target_writes.sort_unstable();
    target_writes.dedup();
    for bid in target_writes {
        live.remove(bid);
    }
    live.extend(reads);
    let _ = ast;
}

struct CleanResult {
    block: NodeId,
    live_in: LiveSet,
    removed: usize,
}

#[allow(clippy::too_many_arguments)]
fn clean_block(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    block: NodeId,
    live_out: &LiveSet,
    exit_live: &LiveSet,
    aggressive: bool,
) -> CleanResult {
    let Node::Block(statements) = source.node(block).clone() else {
        unreachable!("clean_block requires a block")
    };
    let mut rhs_pool: Vec<Effects> = Vec::new();
    let mut effect = empty_effects();
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
                while rhs_pool.len() < expressions.len() {
                    rhs_pool.push(empty_effects());
                }
                for (index, expression) in expressions.iter().enumerate() {
                    analyzer.effects_for_expr_into(*expression, &mut rhs_pool[index]);
                }
                let rhs_effects = &rhs_pool[..expressions.len()];
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
                    let dead = target_bid.is_some_and(|bid| !live.contains(bid))
                        && droppable(source, analyzer, expression, aggressive);
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
                    target.nodes.rewrite(
                        statement,
                        Node::Assign(kept_targets.clone(), kept_expressions.clone()),
                        "final-dead-store-elimination",
                    );
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
                    let result = clean_block(
                        target, source, res, analyzer, arm.body, &live, exit_live, aggressive,
                    );
                    removed += result.removed;
                    branch_inputs.push(result.live_in);
                    rewritten_arms.push(IfArm {
                        cond: arm.cond,
                        body: result.block,
                    });
                }
                let rewritten_else = if let Some(else_id) = else_block {
                    let result = clean_block(
                        target, source, res, analyzer, else_id, &live, exit_live, aggressive,
                    );
                    removed += result.removed;
                    branch_inputs.push(result.live_in);
                    Some(result.block)
                } else {
                    branch_inputs.push(live.clone());
                    None
                };
                let mut merged = live.zeros_like();
                for branch in &branch_inputs {
                    merged.union_from(branch);
                }
                for arm in &arms {
                    analyzer.effects_for_expr_into(arm.cond, &mut effect);
                    merged.extend(effect.reads.iter().copied());
                }
                live = merged;
                target.nodes.rewrite(
                    statement,
                    Node::If(rewritten_arms, rewritten_else),
                    "final-dead-store-elimination",
                );
                output.push(statement);
            }
            Node::Do(body) => {
                let result = clean_block(
                    target, source, res, analyzer, body, &live, exit_live, aggressive,
                );
                live = result.live_in;
                removed += result.removed;
                target.nodes.rewrite(
                    statement,
                    Node::Do(result.block),
                    "final-dead-store-elimination",
                );
                output.push(statement);
            }
            Node::Return(..) => {
                // A return exits the function, so liveness at this point is
                // the function-exit set, not the fall-through liveness of
                // the enclosing block. Without this reset, a store before a
                // return inside a branch is judged dead when an overwrite on
                // the fall-through path kills the binding there, even though
                // the returned path leaves the stored value live for later
                // callbacks and invocations.
                live = exit_live.clone();
                analyzer.effects_for_statement_into(statement, &mut effect);
                transfer_effects_may_write(&mut live, &effect);
                output.push(statement);
            }
            Node::While(..) | Node::Repeat(..) | Node::Fornum(..) | Node::Forin(..) => {
                // A loop may execute zero times.  A write seen in its body is
                // therefore a may-write, so the pre-loop value is live at the
                // loop entry even when it is overwritten on every iteration.
                analyzer.effects_for_statement_into(statement, &mut effect);
                transfer_effects(&mut live, &effect);
                live.extend(effect.writes.iter().copied());
                output.push(statement);
            }
            _ => {
                analyzer.effects_for_statement_into(statement, &mut effect);
                transfer_effects_may_write(&mut live, &effect);
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
    persistent_globals: &LiveSet,
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
    if super::has_unstructured_flow(ast, root) {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }

    let before = measure_size(ast, root);
    let original_nodes = ast.nodes.clone();
    let mut source = storm_lua_syntax::ast_utils::inherit_ast(ast);
    source.nodes = original_nodes.clone();
    let res = resolve(&source, root);
    let analyzer = EffectAnalyzer::new(&source, &res, root, aggressive);
    let mut persistent_globals = LiveSet::new(res.bindings.len());
    for (bid, binding) in res.bindings.iter().enumerate() {
        if matches!(binding.kind, BindingKind::Global) && !binding.fixed {
            persistent_globals.insert(bid as u32);
        }
    }
    persistent_globals.union_from(&captured_across_functions(&source, &res));
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
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

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
    fn keeps_dead_global_store_whose_rhs_may_throw() {
        // Candidate M (w9 cycle 66): aggressive (Smallest) mode dropped
        // a dead store whenever its right-hand side merely could throw.
        // Evaluating `t.x` is observable when `t` is not a table, so the
        // store must stay even though its value is overwritten unread.
        let out = output("function onTick() g = t.x g = 1 end");
        assert!(out.contains("g=t.x"), "{out}");
    }

    #[test]
    fn still_drops_dead_store_with_total_numeric_rhs_in_aggressive() {
        // The aggressive license survives only for a right-hand side
        // proven total: literals combined with total numeric operators.
        assert_eq!(
            output("function onTick() g = 1 + 2 g = 1 end"),
            "function onTick()g=1 end"
        );
    }

    #[test]
    fn preserves_persistent_global_at_callback_exit() {
        let out = output("function onTick() state=1 end");
        assert!(out.contains("state=1"), "{out}");
    }

    #[test]
    fn keeps_global_store_before_return_inside_branch() {
        // The store is overwritten on the fall-through path (state='CAP')
        // but the branch returns early, so at function exit the global
        // still holds the stored value for later callbacks and invocations.
        let out = output(
            "state='WAIT' count=0 function onTick() count=count+1              if count==1 then if count~=2 then state='GAP' return end              state='CAP' end output.setNumber(1,count) end              function onDraw() screen.drawText(1,10,state) end",
        );
        assert!(out.contains("GAP"), "{out}");
    }

    #[test]
    fn keeps_global_store_before_return_inside_do_block() {
        let out = output(
            "state='WAIT' function onTick() if true then do state='GAP' return end end              state='CAP' end function onDraw() screen.drawText(1,10,state) end",
        );
        assert!(out.contains("GAP"), "{out}");
    }

    #[test]
    fn still_removes_dead_local_store_before_return() {
        let out = output("function f() local x x=1 x=2 return 0 end");
        assert!(!out.contains("x=2"), "{out}");
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
    #[test]
    fn keeps_global_store_before_call_that_may_write_same_global() {
        // The callee writes `g` only on one branch, so the conditional
        // store in onTick stays observable on every path where the callee
        // does not overwrite it (and on later ticks via onDraw).  Treating
        // the call's may-write as a definite kill dropped the store.
        let out = output(
            "g=\"\" function setter() if input.getBool(1) then g=\"x\" end end \
             function onTick() if g==\"\" then g=\"init\" end setter() end \
             function onDraw() screen.drawText(1, 1, g) end",
        );
        assert!(out.contains("g=\"init\""), "{out}");
    }

    #[test]
    fn keeps_global_store_with_pure_call_rhs_before_may_writing_call() {
        // Same may-write kill, with the kept store's value produced by a
        // pure call and the overwriting write two call levels deep.
        let out = output(
            "fovL=\"\" fov=0 function getFovLabel(f) return string.format(\"%1.3f\", f) end \
             function onTick() if fovL==\"\" then fovL=getFovLabel(fov) end processTouchscreen() output.setNumber(1, fov) end \
             function processTouchscreen() if input.getBool(1) then decreaseFov() end end \
             function decreaseFov() fovL=getFovLabel(fov) end \
             function onDraw() screen.drawText(1, 1, fovL) end",
        );
        assert!(out.contains("fovL=getFovLabel(fov)"), "{out}");
    }

    #[test]
    fn keeps_global_store_before_kept_assignment_whose_rhs_call_may_write() {
        // The may-write arrives through the right-hand side of a kept
        // assignment rather than a call statement; it still must not kill
        // the earlier store.
        let out = output(
            "g=\"\" function probe() if input.getBool(1) then g=\"x\" end return 1 end \
             function onTick() g=\"init\" local v v=probe() output.setNumber(1, v) end \
             function onDraw() screen.drawText(1, 1, g) end",
        );
        assert!(out.contains("g=\"init\""), "{out}");
    }

    #[test]
    fn live_set_matches_set_semantics() {
        let mut set = LiveSet::new(130);
        assert!(!set.contains(0));
        // Word boundaries and the last partial word.
        for bid in [0u32, 1, 63, 64, 65, 129] {
            set.insert(bid);
        }
        for bid in [0u32, 1, 63, 64, 65, 129] {
            assert!(set.contains(bid), "{bid}");
        }
        for bid in [2u32, 62, 66, 128] {
            assert!(!set.contains(bid), "{bid}");
        }
        set.remove(64);
        assert!(!set.contains(64));
        set.remove(64);
        // Removal of an absent binding is a no-op.
        assert!(set.contains(65));

        let mut other = LiveSet::new(70);
        other.insert(65);
        other.insert(69);
        other.insert(200);
        set.union_from(&other);
        assert!(set.contains(69));
        // union_from grows to the source universe when needed.
        assert!(set.contains(200));

        let empty = set.zeros_like();
        assert!(!empty.contains(0));
        assert!(!empty.contains(200));

        // Clones are independent copies.
        let mut cloned = set.clone();
        cloned.remove(0);
        assert!(set.contains(0));
        assert!(!cloned.contains(0));

        // extend inserts every element.
        let mut extended = LiveSet::new(4);
        extended.extend([3u32, 4, 5]);
        assert!(extended.contains(3) && extended.contains(4) && extended.contains(5));
        assert!(!extended.contains(2));
    }
}
