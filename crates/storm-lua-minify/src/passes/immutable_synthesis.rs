//! Immutable carrier synthesis (`passes/immutable-synthesis.ts`).
//!
//! Repeated stable unary/binary expressions inside functions that depend only
//! on single-write top-level numeric globals can be evaluated once into a
//! synthetic global. Candidate prefixes are measured after scope renaming and
//! only the smallest candidate is committed in each of up to eight rounds.
//!
//! The carrier evaluates the expression unconditionally at the declaration
//! point, so a candidate must not be able to trap under the premise that
//! every read resolves to a single-write numeric binding: an index read
//! whose base is not a table constructor indexes a number and always
//! errors, and any operation the literal folder refuses to evaluate on
//! the premise values raises its runtime error instead, even though
//! every original occurrence sat behind a condition or inside a
//! function that might never run (candidates AC and AF, w9 cycles
//! 68-69).

use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use crate::scope_rename::measure_renamed_size;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::print::Printer;

#[derive(Clone)]
struct Group {
    expression: NodeId,
    occurrences: Vec<NodeId>,
    declaration: usize,
    estimate: isize,
}

/// Literal values of the numeric-dependency bindings under the
/// carrier premise, keyed by binding.
type PremiseValues = HashMap<BindingId, super::literal_folding::Literal>;

fn unparen(ast: &Ast, mut node: NodeId) -> NodeId {
    while let Node::Paren(inner) = ast.node(node) {
        node = *inner;
    }
    node
}

/// Evaluate an expression under the carrier premise, reusing the
/// literal folder's Lua 5.3 operator semantics: literals evaluate to
/// themselves, a read evaluates to its binding's initializer literal,
/// and an index into a table constructor evaluates to the matching
/// field's value (nil for an absent key). `None` when any part is not
/// premise-evaluable (a non-constructor index, a call, a function, a
/// table value, or an operation the folder refuses on the values).
fn eval_premise(
    ast: &Ast,
    res: &Resolution,
    values: &PremiseValues,
    node: NodeId,
) -> Option<super::literal_folding::Literal> {
    use super::literal_folding::Literal;
    match ast.node(node) {
        Node::Paren(inner) => eval_premise(ast, res, values, *inner),
        Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil => {
            super::literal_folding::literal_value(ast, node)
        }
        Node::Name(_) => {
            let bid = res.node_bid.get(node as usize).copied().flatten()?;
            values.get(&bid).cloned()
        }
        Node::Un(op, operand) => {
            let a = eval_premise(ast, res, values, *operand)?;
            super::literal_folding::eval_unary(op.as_str(), &a)
        }
        Node::Bin(op, left, right) => {
            let a = eval_premise(ast, res, values, *left)?;
            let b = eval_premise(ast, res, values, *right)?;
            super::literal_folding::eval_binary(op.as_str(), &a, &b)
        }
        Node::Index(base, key, _) => {
            let Node::Table(fields) = ast.node(unparen(ast, *base)) else {
                return None;
            };
            let key = eval_premise(ast, res, values, *key)?;
            let mut found = None;
            let mut position = 0i64;
            for field in fields {
                match field {
                    storm_lua_syntax::ast::TableField::Arr(value) => {
                        position += 1;
                        if matches!(&key, Literal::Int(i) if *i == position) {
                            found = Some(*value);
                        }
                    }
                    storm_lua_syntax::ast::TableField::Name(name, value) => {
                        if let Literal::Str(s) = &key {
                            if ast.strings.get(*name).as_bytes() == s.as_slice() {
                                found = Some(*value);
                            }
                        }
                    }
                    storm_lua_syntax::ast::TableField::KVar(k, value) => {
                        if let Some(fk) = eval_premise(ast, res, values, *k) {
                            let equal = match (&fk, &key) {
                                (Literal::Int(a), Literal::Int(b)) => a == b,
                                (Literal::Str(a), Literal::Str(b)) => a == b,
                                _ => false,
                            };
                            if equal {
                                found = Some(*value);
                            }
                        }
                    }
                }
            }
            match found {
                Some(value) => eval_premise(ast, res, values, value),
                None => Some(Literal::Nil),
            }
        }
        _ => None,
    }
}

/// Whether the expression contains a subexpression that certainly
/// traps once evaluated under the carrier premise: an index read
/// whose base is not a table constructor indexes a number, the
/// length of a non-string/non-table operand measures a number, a
/// concatenation with a table/function node operand or a boolean /
/// nil premise value (Lua 5.3 concatenation accepts only strings
/// and numbers), an arithmetic, bitwise, or comparison operation
/// whose operands the folder refuses to evaluate on the premise
/// values raises instead
/// of producing a value (integer division or remainder by a divisor
/// that is zero only after derivation -- `b - b`, `b % b`, a read of
/// a zero-valued binding --, arithmetic on a non-numeric string, a
/// bitwise operation on a non-integer float), and a `//` or `%`
/// divisor that is not premise-evaluable at all cannot be proven
/// nonzero. The carrier evaluates the expression unconditionally,
/// while the original occurrences may sit behind conditions or
/// inside functions that never run (candidates AC and AF, w9 cycles
/// 68-69).
fn contains_certain_trap(
    ast: &Ast,
    res: &Resolution,
    values: &PremiseValues,
    node: NodeId,
) -> bool {
    use super::literal_folding::Literal;
    match ast.node(node) {
        Node::Function(..) => false,
        Node::Index(base, key, _) => {
            !matches!(ast.node(unparen(ast, *base)), Node::Table(_))
                || contains_certain_trap(ast, res, values, *base)
                || contains_certain_trap(ast, res, values, *key)
        }
        Node::Un(op, operand) => {
            let op = op.as_str();
            if op == "#" {
                if !matches!(
                    ast.node(unparen(ast, *operand)),
                    Node::Table(_) | Node::Str(_)
                ) {
                    return true;
                }
            } else {
                if matches!(
                    ast.node(unparen(ast, *operand)),
                    Node::Table(_) | Node::Function(..)
                ) {
                    return true;
                }
                if let Some(a) = eval_premise(ast, res, values, *operand) {
                    if super::literal_folding::eval_unary(op, &a).is_none() {
                        return true;
                    }
                }
            }
            contains_certain_trap(ast, res, values, *operand)
        }
        Node::Bin(op, left, right) => {
            let op = op.as_str();
            if op == "//" || op == "%" {
                match eval_premise(ast, res, values, *right) {
                    // The divisor is not premise-evaluable, so it
                    // cannot be proven nonzero.
                    None => return true,
                    // An integer zero divisor traps unless the
                    // dividend is provably a float (float division
                    // by zero yields inf/NaN, not an error).
                    Some(Literal::Int(0))
                        if !matches!(
                            eval_premise(ast, res, values, *left),
                            Some(Literal::Num(_))
                        ) =>
                    {
                        return true;
                    }
                    Some(_) => {}
                }
            } else if op == ".." {
                // Concatenation accepts only strings and numbers in
                // Lua 5.3: a table or function node operand is a
                // fresh value no `__concat` metamethod can be
                // attached to during this evaluation, and a boolean
                // or nil premise value has no `__concat` either, so
                // either side trapping makes the whole expression
                // trap (candidate S2). `==` / `~=` never trap and
                // stay excluded.
                let operand_traps = |operand: NodeId| {
                    matches!(
                        ast.node(unparen(ast, operand)),
                        Node::Table(_) | Node::Function(..)
                    ) || matches!(
                        eval_premise(ast, res, values, operand),
                        Some(Literal::Bool(_)) | Some(Literal::Nil)
                    )
                };
                if operand_traps(*left) || operand_traps(*right) {
                    return true;
                }
            } else if matches!(
                op,
                "+" | "-"
                    | "*"
                    | "/"
                    | "^"
                    | "&"
                    | "|"
                    | "~"
                    | "<<"
                    | ">>"
                    | "<"
                    | ">"
                    | "<="
                    | ">="
            ) {
                if matches!(
                    ast.node(unparen(ast, *left)),
                    Node::Table(_) | Node::Function(..)
                ) || matches!(
                    ast.node(unparen(ast, *right)),
                    Node::Table(_) | Node::Function(..)
                ) {
                    return true;
                }
                if let (Some(a), Some(b)) = (
                    eval_premise(ast, res, values, *left),
                    eval_premise(ast, res, values, *right),
                ) {
                    if super::literal_folding::eval_binary(op, &a, &b).is_none() {
                        return true;
                    }
                }
            }
            contains_certain_trap(ast, res, values, *left)
                || contains_certain_trap(ast, res, values, *right)
        }
        Node::Table(fields) => fields.iter().any(|field| match field {
            storm_lua_syntax::ast::TableField::Arr(value) => {
                contains_certain_trap(ast, res, values, *value)
            }
            storm_lua_syntax::ast::TableField::Name(_, value) => {
                contains_certain_trap(ast, res, values, *value)
            }
            storm_lua_syntax::ast::TableField::KVar(key, value) => {
                contains_certain_trap(ast, res, values, *key)
                    || contains_certain_trap(ast, res, values, *value)
            }
        }),
        _ => {
            let mut found = false;
            storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
                if contains_certain_trap(ast, res, values, child) {
                    found = true;
                }
            });
            found
        }
    }
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn clone_subtree(target: &mut Ast, source: &Ast, node: NodeId) -> NodeId {
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_subtree(target, source, child)
    });
    let copied = target.push(mapped);
    target
        .nodes
        .derive_from_clone(copied, &source.nodes, node, "immutable-carrier-copy");
    copied
}

fn measured(ast: &Ast, root: NodeId) -> usize {
    measure_renamed_size(ast, root)
}

#[allow(clippy::too_many_arguments)]
fn inspect(
    ast: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    resolution: &Resolution,
    numeric_initializers: &HashMap<BindingId, usize>,
    premise_values: &PremiseValues,
    node: NodeId,
    inside_function: bool,
    groups: &mut Vec<Group>,
    group_indexes: &mut HashMap<String, usize>,
) {
    if inside_function && matches!(ast.node(node), Node::Bin(..) | Node::Un(..)) {
        let key = super::immutable_values::expression_key(ast, resolution, node);
        let expression_size = Printer::new(ast, false).expr_public(node).len();
        if expression_size >= 3 {
            let effect = analyzer.effects_for_expr(node);
            if !effect.calls
                && !effect.ordered
                && effect.writes.is_empty()
                && effect.stable
                && !contains_certain_trap(ast, resolution, premise_values, node)
                && !effect.reads.is_empty()
                && effect
                    .reads
                    .iter()
                    .all(|bid| numeric_initializers.contains_key(bid))
            {
                let declaration = effect
                    .reads
                    .iter()
                    .filter_map(|bid| numeric_initializers.get(bid))
                    .copied()
                    .max()
                    .unwrap_or(0);
                let index = if let Some(index) = group_indexes.get(&key).copied() {
                    index
                } else {
                    let index = groups.len();
                    group_indexes.insert(key.clone(), index);
                    groups.push(Group {
                        expression: node,
                        occurrences: Vec::new(),
                        declaration,
                        estimate: 0,
                    });
                    index
                };
                let group = &mut groups[index];
                group.occurrences.push(node);
                group.estimate = group.occurrences.len() as isize * (expression_size as isize - 1)
                    - expression_size as isize
                    - 2;
            }
        }
    }
    if let Node::Function(_, _, body) = ast.node(node) {
        inspect(
            ast,
            analyzer,
            resolution,
            numeric_initializers,
            premise_values,
            *body,
            true,
            groups,
            group_indexes,
        );
        return;
    }
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        inspect(
            ast,
            analyzer,
            resolution,
            numeric_initializers,
            premise_values,
            child,
            inside_function,
            groups,
            group_indexes,
        );
    });
}

fn candidate_for_group(
    source: &Ast,
    root: NodeId,
    group: &Group,
    count: usize,
    carrier: &str,
) -> Ast {
    let mut candidate = clone_ast(source);
    let expression = clone_subtree(&mut candidate, source, group.expression);
    let carrier_symbol = candidate.strings.intern(carrier);
    // One shared code string per rule for the whole candidate: the
    // recorded origin content is identical, only the per-occurrence
    // fresh `Arc<str>` allocations disappear (rewrite allocates its
    // code even on origin-free trial arenas, so the trials benefit
    // too; derive/relate allocate only when tracking, i.e. on replay).
    let use_code: std::sync::Arc<str> = std::sync::Arc::from("immutable-carrier-use");
    let definition_code: std::sync::Arc<str> = std::sync::Arc::from("immutable-carrier-definition");
    for occurrence in group.occurrences.iter().take(count) {
        // Replace the value without recording a rewrite origin: the
        // derive below unconditionally overwrites this slot from the
        // source occurrence (or clears it on the unknown-input fallback),
        // so the rewrite origin would be discarded before any read.
        // The recorded origin is therefore identical, only the
        // redundant rewrite work disappears.
        candidate.nodes[*occurrence as usize] = Node::Name(carrier_symbol);
        super::origins::derive_with_code(
            &mut candidate,
            *occurrence,
            source,
            &[*occurrence],
            &use_code,
        );
        candidate.nodes.relate_from_role_code(
            *occurrence,
            &source.nodes,
            group.expression,
            &definition_code,
            storm_lua_syntax::explanation::RelationRole::Definition,
        );
    }
    let target = candidate.name(carrier);
    candidate
        .nodes
        .mark_synthetic(target, "immutable-carrier-storage");
    let assignment = candidate.assign(vec![target], vec![expression]);
    candidate
        .nodes
        .mark_synthetic(assignment, "immutable-carrier-storage");
    if let Node::Block(statements) = candidate.node(root).clone() {
        let mut statements = statements;
        let insert_at = (group.declaration + 1).min(statements.len());
        statements.insert(insert_at, assignment);
        let insertion_code: std::sync::Arc<str> =
            std::sync::Arc::from("immutable-carrier-insertion");
        candidate
            .nodes
            .rewrite_with_code(root, Node::Block(statements), &insertion_code);
    }
    candidate
}

fn clone_subtree_in_place(ast: &mut Ast, node: NodeId) -> NodeId {
    let original = ast.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_subtree_in_place(ast, child)
    });
    let copied = ast.push(mapped);
    ast.nodes
        .derive_from_clone_self(copied, node, "immutable-carrier-copy");
    copied
}

/// Replay a winning candidate directly onto the tracked arena instead of
/// building a fresh clone of the round source and committing it wholesale.
/// A fresh clone re-shares every origin page with the round source, so
/// every round pays a page copy for each page its occurrences touch; the
/// live arena's pages are detached at most once per pass (and not at all
/// when the pass owns them uniquely, as in a direct pass run). The
/// committed code and origins are identical to `candidate_for_group`:
/// derive inputs and the definition contributor are snapshotted before
/// any mutation, because the value write clears each occurrence slot and
/// the expression node is itself the first occurrence. Trial candidates
/// keep using `candidate_for_group` unchanged.
fn replay_group_on(ast: &mut Ast, root: NodeId, group: &Group, count: usize, carrier: &str) {
    let mut snapshots = Vec::with_capacity(count);
    for occurrence in group.occurrences.iter().take(count) {
        let known_flag = super::origins::known(ast, *occurrence);
        let previous = ast.node(*occurrence).clone();
        let borrowed = ast.nodes.capture_origin_ref(*occurrence);
        snapshots.push((known_flag, previous, borrowed));
    }
    let definition = ast.nodes.capture_origin_ref(group.expression);
    let expression = clone_subtree_in_place(ast, group.expression);
    let carrier_symbol = ast.strings.intern(carrier);
    // One shared code string per rule for the whole replay: the recorded
    // origin content is identical, only the per-occurrence fresh
    // `Arc<str>` allocations disappear.
    let use_code: std::sync::Arc<str> = std::sync::Arc::from("immutable-carrier-use");
    let definition_code: std::sync::Arc<str> = std::sync::Arc::from("immutable-carrier-definition");
    for (occurrence, (known_flag, previous, borrowed)) in
        group.occurrences.iter().take(count).zip(snapshots.iter())
    {
        // Replace the value without recording a rewrite origin: the
        // derive below unconditionally overwrites this slot from the
        // snapshotted occurrence (or clears it on the unknown-input
        // fallback, which the value write already did), so the rewrite
        // origin would be discarded before any read. The recorded origin
        // is therefore identical, only the redundant rewrite work
        // disappears.
        ast.nodes[*occurrence as usize] = Node::Name(carrier_symbol);
        if *known_flag {
            if let Some(borrowed) = borrowed {
                ast.nodes.derive_from_borrowed_with_code(
                    *occurrence,
                    previous,
                    borrowed,
                    &use_code,
                );
            }
        }
        ast.nodes.relate_from_borrowed_role_code(
            *occurrence,
            definition.as_ref(),
            &definition_code,
            storm_lua_syntax::explanation::RelationRole::Definition,
        );
    }
    let target = ast.name(carrier);
    ast.nodes
        .mark_synthetic(target, "immutable-carrier-storage");
    let assignment = ast.assign(vec![target], vec![expression]);
    ast.nodes
        .mark_synthetic(assignment, "immutable-carrier-storage");
    if let Node::Block(statements) = ast.node(root).clone() {
        let mut statements = statements;
        let insert_at = (group.declaration + 1).min(statements.len());
        statements.insert(insert_at, assignment);
        let insertion_code: std::sync::Arc<str> =
            std::sync::Arc::from("immutable-carrier-insertion");
        ast.nodes
            .rewrite_with_code(root, Node::Block(statements), &insertion_code);
    }
}

pub fn synthesize_immutable_carriers_with_options(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    max_rounds: usize,
) -> PassResult {
    if !aggressive || !matches!(ast.node(root), Node::Block(_)) {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let original_size = measured(ast, root);
    let mut synthesized = 0usize;
    let mut considered = 0usize;

    for _ in 0..max_rounds {
        let res = resolve(ast, root);
        let analyzer = EffectAnalyzer::new(ast, &res, root, true);
        let mut writes = vec![0u32; res.bindings.len()];
        let mut used_names = HashSet::new();
        storm_lua_syntax::ast_utils::walk(ast, root, &mut |node| {
            if let Node::Name(symbol) = ast.node(node) {
                used_names.insert(ast.strings.get(*symbol).to_string());
                if res.node_write.get(node as usize).copied().unwrap_or(false) {
                    if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
                        writes[bid as usize] += 1;
                    }
                }
            }
        });
        let mut numeric_initializers = HashMap::new();
        let mut premise_values = PremiseValues::new();
        let Node::Block(statements) = ast.node(root) else {
            break;
        };
        for (statement_index, statement) in statements.iter().enumerate() {
            let Node::Assign(targets, expressions) = ast.node(*statement) else {
                continue;
            };
            for index in 0..targets.len() {
                let Some(target) = targets.get(index) else {
                    continue;
                };
                let Some(expression) = expressions.get(index) else {
                    continue;
                };
                if !matches!(ast.node(*target), Node::Name(_))
                    || !matches!(ast.node(*expression), Node::Num(_))
                {
                    continue;
                }
                let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() else {
                    continue;
                };
                if writes.get(bid as usize).copied().unwrap_or(0) == 1 {
                    numeric_initializers.insert(bid, statement_index);
                    if let Some(value) = super::literal_folding::literal_value(ast, *expression) {
                        premise_values.insert(bid, value);
                    }
                }
            }
        }

        let mut groups = Vec::new();
        let mut group_indexes = HashMap::new();
        inspect(
            ast,
            &analyzer,
            &res,
            &numeric_initializers,
            &premise_values,
            root,
            false,
            &mut groups,
            &mut group_indexes,
        );
        groups.retain(|group| group.occurrences.len() >= 2);
        groups.sort_by_key(|group| std::cmp::Reverse(group.estimate));
        groups.truncate(32);

        let baseline = measured(ast, root);
        // Trials run on an origin-free clone of the round source: candidate
        // construction writes origins for every occurrence rewrite, and the
        // counts loop evaluates thousands of candidates that are all but one
        // discarded. Sizes are origin-independent, so the winner is selected
        // on trials and replayed once on the tracked source below.
        let trial = storm_lua_syntax::ast_utils::clone_without_origins(ast);
        let mut best: Option<(Group, usize, String, usize)> = None;
        for group in groups {
            for count in 2..=group.occurrences.len() {
                considered += 1;
                let mut serial = 0usize;
                let mut carrier = format!("__stormmin_immutable_{serial}");
                while used_names.contains(&carrier) {
                    serial += 1;
                    carrier = format!("__stormmin_immutable_{serial}");
                }
                let candidate = candidate_for_group(&trial, root, &group, count, &carrier);
                let size = measured(&candidate, root);
                if size < baseline && best.as_ref().is_none_or(|entry| size < entry.3) {
                    best = Some((group.clone(), count, carrier, size));
                }
            }
        }
        let Some((group, count, carrier, _)) = best else {
            break;
        };
        // Replay the winning candidate in place on the tracked arena, so
        // the committed AST carries exactly the origins the direct
        // construction produced, without re-sharing (and re-detaching)
        // every origin page for another round.
        replay_group_on(ast, root, &group, count, &carrier);
        synthesized += 1;
    }

    PassResult {
        root,
        saved: Some(original_size.saturating_sub(measured(ast, root)) as u64),
        details: if synthesized == 0 && considered == 0 {
            None
        } else {
            Some(vec![
                format!("synthesized={synthesized}"),
                format!("considered={considered}"),
            ])
        },
    }
}

pub fn synthesize_immutable_carriers(ast: &mut Ast, root: NodeId) -> PassResult {
    synthesize_immutable_carriers_with_options(ast, root, true, 8)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::{parse_source, Printer};

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = synthesize_immutable_carriers(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn synthesizes_repeated_cross_function_expression() {
        let source = "a=123456 b=789012 function f0()return a+b end function f1()return a+b end function f2()return a+b end function f3()return a+b end";
        let out = output(source);
        assert!(out.contains("__stormmin_immutable_0=a+b"), "{out}");
        assert_eq!(out.matches("a+b").count(), 1, "{out}");
    }

    #[test]
    fn tracked_trial_winner_matches_untracked_output() {
        // Winner selection runs on origin-free trials and is replayed once on
        // the tracked source; the committed code must equal the untracked
        // run, and the audit printer in `output` verifies the replay kept
        // complete origins (unknown bytes 0).
        let source = "a=123456 b=789012 c=345678 function f0()return a+b end function f1()return a+b end function f2()return a+b end function f3()return a+b end function g0()return a+c end function g1()return a+c end";
        let tracked = output(source);
        let (mut ast, root) = storm_lua_syntax::parser::parse_source(source).expect("parse");
        let result = synthesize_immutable_carriers(&mut ast, root);
        let untracked = storm_lua_syntax::Printer::new(&ast, false).output(result.root);
        assert_eq!(tracked, untracked);
    }

    #[test]
    fn does_not_synthesize_trapping_member_read() {
        // Candidate AC (w9 cycle 68): `(v9.x or 0)` depends only on
        // the numeric global v9, but indexing a number always traps;
        // the source occurrences sit inside functions that are never
        // called, while the synthesized carrier would evaluate the
        // read unconditionally at the top of the chunk.
        let source = "v9=-0.5 function f() return (v9.x or 0) < 42 end function g(p) local t = {(v9.x or 0), f} return t end";
        assert!(
            !output(source).contains("__stormmin_immutable"),
            "{}",
            output(source)
        );
    }

    #[test]
    fn does_not_synthesize_trapping_concatenation() {
        // Candidate S2 (w6 cycle 65): concatenation with a boolean or
        // nil operand always traps in Lua 5.3, but the trap audit's
        // operator lists did not include `..`, so the carrier
        // evaluated `(v9 .. true)` unconditionally at the top of the
        // chunk while the source occurrences sat in never-called
        // functions (masked in the default pipeline only by pass
        // ordering).
        let source = "v9=2.5 function f() return (v9 .. true) + (v9 .. true) end function g() return (v9 .. true) * 2 end";
        assert!(
            !output(source).contains("__stormmin_immutable"),
            "{}",
            output(source)
        );
        let source = "v9=2.5 function f() return (v9 .. nil) + (v9 .. nil) end function g() return (v9 .. nil) * 2 end";
        assert!(
            !output(source).contains("__stormmin_immutable"),
            "{}",
            output(source)
        );
    }

    #[test]
    fn does_not_synthesize_derived_zero_divisor() {
        // Candidate AF (w9 cycle 69): the divisor `v9 - v9` is zero
        // only after derivation, so the literal-zero check from
        // cycle 68 did not see it; the synthesized carrier divided
        // by zero unconditionally while the source occurrences sat
        // in never-called functions.
        let source = "v9=5 function f() return (100 // (v9 - v9)) < 42 end function g(p) local t = {(100 // (v9 - v9)), f} return t end";
        assert!(
            !output(source).contains("__stormmin_immutable"),
            "{}",
            output(source)
        );
    }

    #[test]
    fn does_not_synthesize_zero_valued_read_divisor() {
        // Candidate AF: the divisor is a bare read of a numeric
        // binding whose single write initializes it to zero.
        let source = "v9=0 function f() return (100 // v9) < 42 end function g(p) local t = {(100 // v9), f} return t end";
        assert!(
            !output(source).contains("__stormmin_immutable"),
            "{}",
            output(source)
        );
    }

    #[test]
    fn does_not_synthesize_non_numeric_string_arithmetic() {
        // Candidate AF: `v9 + "x"` traps under the premise; the
        // guard previously only inspected index, length, and
        // division shapes.
        let source = "v9=5 function f() return (v9 + \"x\") < 42 end function g(p) local t = {(v9 + \"x\"), f} return t end";
        assert!(
            !output(source).contains("__stormmin_immutable"),
            "{}",
            output(source)
        );
    }

    #[test]
    fn does_not_synthesize_bitwise_on_derived_float() {
        // Candidate AF: `v9 / 2` is a non-integer float, so the
        // bitwise operation traps under the premise.
        let source = "v9=5 function f() return ((v9 / 2) & 3) < 42 end function g(p) local t = {((v9 / 2) & 3), f} return t end";
        assert!(
            !output(source).contains("__stormmin_immutable"),
            "{}",
            output(source)
        );
    }

    #[test]
    fn synthesizes_nonzero_divisor() {
        let source = "v9=5 function f() return 100 // v9 end function g() return 100 // v9 end function h() return 100 // v9 end function i() return 100 // v9 end";
        assert!(
            output(source).contains("__stormmin_immutable"),
            "{}",
            output(source)
        );
    }

    #[test]
    fn synthesizes_float_zero_divisor() {
        // A float zero divisor yields inf/NaN rather than an error,
        // so a derived float zero does not block synthesis.
        let source = "v9=0.5 function f() return 100 // (v9 - v9) end function g() return 100 // (v9 - v9) end function h() return 100 // (v9 - v9) end function i() return 100 // (v9 - v9) end";
        assert!(
            output(source).contains("__stormmin_immutable"),
            "{}",
            output(source)
        );
    }

    #[test]
    fn requires_single_write_numeric_dependencies() {
        let source = "a=1 a=2 function f()return a+3 end function g()return a+3 end function h()return a+3 end";
        assert!(!output(source).contains("__stormmin_immutable"));
    }

    #[test]
    fn ignores_top_level_occurrences() {
        let source = "a=1 b=2 x=a+b function f()return a+b end";
        assert!(!output(source).contains("__stormmin_immutable"));
    }
}
