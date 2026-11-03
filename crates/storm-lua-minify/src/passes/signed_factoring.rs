//! Signed expression factoring (`signed-expressions.ts`).
//!
//! Reuses a pure base expression across positive/negative forms. Candidate
//! groups are collected in deterministic block/insertion order, then the top
//! estimated groups are measured after scope renaming. Existing bindings may be
//! reused as carriers only when every read in the range belongs to the matched
//! expression and the old value is not observed after the range.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use crate::scope_rename::{measure_renamed_size, measure_renamed_size_unmemoized};
use storm_lua_analysis::effects::{empty_effects, EffectAnalyzer, Effects};
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId, TableField};
use storm_lua_syntax::size::measure_size;

use super::available_expressions::{
    contains_mutable_index_read, statement_has_field_touching_call, statement_may_store_fields,
};
use super::immutable_values::expression_key;

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

/// Clone a subtree from `source` into `target` with fresh node ids.
fn clone_subtree(target: &mut Ast, source: &Ast, node: NodeId) -> NodeId {
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_subtree(target, source, child)
    });
    let copied = target.push(mapped);
    target.nodes.derive_from_clone(
        copied,
        &source.nodes,
        node,
        "signed-carrier-definition-copy",
    );
    copied
}

/// `NodeArena::rewrite` on `target`, recording the overwritten value in
/// `undo` when the caller keeps an undo log (recycled trial arenas).
/// The arena itself skips no-op rewrites, so those are not logged.
fn rewrite_slot(
    target: &mut Ast,
    undo: Option<&mut Vec<(NodeId, Node)>>,
    node: NodeId,
    value: Node,
    transformation: &str,
) {
    if let Some(undo) = undo {
        if target.node(node) != &value {
            undo.push((node, target.node(node).clone()));
        }
    }
    target.nodes.rewrite(node, value, transformation);
}

fn is_atom(node: &Node) -> bool {
    matches!(
        node,
        Node::Name(_) | Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
    )
}

fn is_control(node: &Node) -> bool {
    matches!(
        node,
        Node::If(..)
            | Node::While(..)
            | Node::Repeat(..)
            | Node::Fornum(..)
            | Node::Forin(..)
            | Node::Do(..)
            | Node::Goto(_)
            | Node::Label(_)
            | Node::Break
            | Node::Return(_)
    )
}

fn safe_signed(ast: &Ast, analyzer: &EffectAnalyzer<'_>, node: NodeId) -> bool {
    // TS EXPRESSIONS minus atoms/call/table/function. Vararg remains permitted
    // by the source pass and is filtered by its effects when unsafe.
    if !matches!(
        ast.node(node),
        Node::Paren(_) | Node::Index(..) | Node::Un(..) | Node::Bin(..) | Node::Vararg
    ) {
        return false;
    }
    let effect = analyzer.effects_for_expr(node);
    !effect.calls && !effect.ordered && effect.writes.is_empty() && effect.stable
}

fn preserves_binding_values(ast: &Ast, analyzer: &EffectAnalyzer<'_>, statement: NodeId) -> bool {
    let Node::Callstat(expression) = ast.node(statement) else {
        return false;
    };
    let Node::Call(function, _, _) = ast.node(*expression) else {
        return false;
    };
    let Some(builtin) = analyzer.resolve_builtin_reference(*function) else {
        return false;
    };
    builtin.starts_with("output.")
        || (builtin.starts_with("screen.")
            && builtin != "screen.getWidth"
            && builtin != "screen.getHeight")
}

fn barrier(
    ast: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    statement: NodeId,
    reads: &[BindingId],
    effect: &mut Effects,
) -> bool {
    if is_control(ast.node(statement)) {
        return true;
    }
    analyzer.effects_for_statement_into(statement, effect);
    if reads.iter().any(|bid| effect.writes.contains(bid)) {
        return true;
    }
    !preserves_binding_values(ast, analyzer, statement)
        && (effect.calls || effect.ordered || effect.may_throw)
}

/// Build the unsigned base into `scratch`, returning its sign and NodeId.
/// New Bin nodes retain original child IDs, so BindingId-aware keys remain valid
/// against the original Resolution.
fn signed_form(scratch: &mut Ast, source: &Ast, node: NodeId) -> (i8, NodeId) {
    match source.node(node) {
        Node::Un(op, expression) if op == "-" => {
            let (sign, base) = signed_form(scratch, source, *expression);
            (-sign, base)
        }
        Node::Bin(op, left, right) if op == "*" || op == "/" => {
            let (left_sign, left_base) = signed_form(scratch, source, *left);
            let (right_sign, right_base) = signed_form(scratch, source, *right);
            if left_sign < 0 || right_sign < 0 {
                let base = scratch.push(Node::Bin(op.clone(), left_base, right_base));
                scratch
                    .nodes
                    .derive_from(base, &source.nodes, node, "signed-base-normalization");
                (if left_sign == right_sign { 1 } else { -1 }, base)
            } else {
                (1, node)
            }
        }
        _ => (1, node),
    }
}

fn signed_key(
    scratch: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
) -> (i8, NodeId, String) {
    let (sign, base) = signed_form(scratch, source, node);
    let key = expression_key(scratch, res, base);
    (sign, base, key)
}

fn ordered_reads(ast: &Ast, res: &Resolution, node: NodeId) -> Vec<BindingId> {
    let mut out = Vec::new();
    storm_lua_syntax::ast_utils::walk(ast, node, &mut |id| {
        if matches!(ast.node(id), Node::Name(_)) {
            if let Some(bid) = res.node_bid.get(id as usize).copied().flatten() {
                if !out.contains(&bid) {
                    out.push(bid);
                }
            }
        }
    });
    out
}

#[allow(clippy::too_many_arguments)]
fn inspect_signed_expression(
    ast: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    scratch: &mut Ast,
    node: NodeId,
    statement: usize,
    found: &mut Vec<(String, NodeId, Vec<usize>, Vec<BindingId>)>,
    positions: &mut HashMap<String, usize>,
) {
    if matches!(ast.node(node), Node::Block(_) | Node::Function(..)) {
        return;
    }
    if safe_signed(ast, analyzer, node) {
        let (_, base, key) = signed_key(scratch, ast, res, node);
        if !is_atom(scratch.node(base)) && key.len() >= 3 {
            let index = if let Some(index) = positions.get(&key).copied() {
                index
            } else {
                let index = found.len();
                positions.insert(key.clone(), index);
                let reads = ordered_reads(scratch, res, base);
                found.push((key.clone(), base, Vec::new(), reads));
                index
            };
            found[index].2.push(statement);
        }
    }
    if let Node::Bin(op, left, _) = ast.node(node) {
        if op == "and" || op == "or" {
            inspect_signed_expression(
                ast, res, analyzer, scratch, *left, statement, found, positions,
            );
            return;
        }
    }
    match ast.node(node) {
        Node::Assign(_, expressions) => {
            for expression in expressions {
                inspect_signed_expression(
                    ast,
                    res,
                    analyzer,
                    scratch,
                    *expression,
                    statement,
                    found,
                    positions,
                );
            }
        }
        Node::Funcstat(_, _) => {
            // A function statement's target is a write position, exactly
            // like an assignment target: occurrences inside it are not
            // carrier reads. Its function child is a `Node::Function`,
            // which the entry check above already refuses to enter.
        }
        Node::Call(_, arguments, _) => {
            // TS skips call.fn.
            for argument in arguments {
                inspect_signed_expression(
                    ast, res, analyzer, scratch, *argument, statement, found, positions,
                );
            }
        }
        Node::Table(fields) => {
            for field in fields {
                match field {
                    TableField::Arr(value) | TableField::Name(_, value) => {
                        inspect_signed_expression(
                            ast, res, analyzer, scratch, *value, statement, found, positions,
                        )
                    }
                    TableField::KVar(key, value) => {
                        inspect_signed_expression(
                            ast, res, analyzer, scratch, *key, statement, found, positions,
                        );
                        inspect_signed_expression(
                            ast, res, analyzer, scratch, *value, statement, found, positions,
                        );
                    }
                }
            }
        }
        _ => storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
            inspect_signed_expression(
                ast, res, analyzer, scratch, child, statement, found, positions,
            )
        }),
    }
}

#[derive(Clone)]
struct Group {
    block: NodeId,
    key: String,
    expression: NodeId,
    statements: Vec<usize>,
    reads: Vec<BindingId>,
    estimate: isize,
    order: usize,
}

fn nested_blocks(ast: &Ast, node: NodeId, out: &mut Vec<NodeId>) {
    match ast.node(node) {
        Node::Block(_) => out.push(node),
        Node::Function(_, _, body) => out.push(*body),
        _ => storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
            nested_blocks(ast, child, out)
        }),
    }
}

/// Whether the factored expression may throw when evaluated,
/// mirroring the effect analyzer's rules structurally (the group
/// expression lives in the scratch arena): an index read always may,
/// arithmetic and length/bitwise unary operators may, a constructor
/// store with a non-literal key may, and a comparison or boolean
/// operator only through its operands.
fn expression_may_throw(ast: &Ast, node: NodeId) -> bool {
    match ast.node(node) {
        Node::Index(..) => true,
        Node::Function(..) => false,
        Node::Paren(inner) => expression_may_throw(ast, *inner),
        Node::Un(op, inner) => {
            matches!(op.as_str(), "-" | "#" | "~") || expression_may_throw(ast, *inner)
        }
        Node::Bin(op, left, right) => {
            !matches!(op.as_str(), "and" | "or" | "==" | "~=")
                || expression_may_throw(ast, *left)
                || expression_may_throw(ast, *right)
        }
        Node::Table(fields) => fields.iter().any(|field| match field {
            TableField::Arr(value) | TableField::Name(_, value) => {
                expression_may_throw(ast, *value)
            }
            TableField::KVar(key, value) => {
                !matches!(ast.node(*key), Node::Num(_) | Node::Str(_) | Node::Bool(_))
                    || expression_may_throw(ast, *key)
                    || expression_may_throw(ast, *value)
            }
        }),
        _ => {
            let mut found = false;
            storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
                found |= expression_may_throw(ast, child);
            });
            found
        }
    }
}

/// T05: whether evaluating `node` up to (but not including) its
/// first signed occurrence of `key`, in Lua evaluation order, may
/// produce an observable effect (a call or an ordered host effect).
/// Returns `None` when `node` holds no occurrence. The walk mirrors
/// the occurrence scan's reach (no function or block bodies, no
/// assignment or function statement targets, no right operand of
/// `and`/`or`), and a preceding sibling subtree contributes its
/// observable effect bits.
#[allow(clippy::too_many_arguments)]
fn occurrence_prefix_dirty(
    ast: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    scratch: &mut Ast,
    node: NodeId,
    key: &str,
) -> Option<bool> {
    fn effect_dirty(analyzer: &EffectAnalyzer<'_>, node: NodeId) -> bool {
        // Observable effects only: a call, or an ordered host effect.
        // A preceding sibling that merely may throw (an arithmetic
        // operand, an index read) does not lose an observable event
        // when the factored read moves in front of it -- when both
        // throw, no event precedes either error; the defect class
        // (T05) is the observable event the original emits first.
        let effect = analyzer.effects_for_expr(node);
        effect.calls || effect.ordered
    }
    #[allow(clippy::too_many_arguments)]
    fn seq(
        ast: &Ast,
        res: &Resolution,
        analyzer: &EffectAnalyzer<'_>,
        scratch: &mut Ast,
        items: &[NodeId],
        key: &str,
    ) -> Option<bool> {
        let mut acc = false;
        for &item in items {
            if let Some(inner) = occurrence_prefix_dirty(ast, res, analyzer, scratch, item, key) {
                return Some(acc || inner);
            }
            acc |= effect_dirty(analyzer, item);
        }
        None
    }
    if matches!(ast.node(node), Node::Block(_) | Node::Function(..)) {
        return None;
    }
    // The occurrence itself: its own evaluation is exactly what the
    // carrier definition performs, so the prefix inside it is clean.
    if !is_atom(ast.node(node)) && safe_signed(ast, analyzer, node) {
        let (_, _, node_key) = signed_key(scratch, ast, res, node);
        if node_key == key {
            return Some(false);
        }
    }
    match ast.node(node) {
        Node::Paren(inner) | Node::Un(_, inner) => {
            occurrence_prefix_dirty(ast, res, analyzer, scratch, *inner, key)
        }
        Node::Methodname(base, _) => {
            occurrence_prefix_dirty(ast, res, analyzer, scratch, *base, key)
        }
        Node::Bin(op, left, right) => {
            let (left, right) = (*left, *right);
            if matches!(op.as_str(), "and" | "or") {
                // The occurrence scan never collects the right
                // operand of a boolean operator.
                occurrence_prefix_dirty(ast, res, analyzer, scratch, left, key)
            } else if let Some(inner) =
                occurrence_prefix_dirty(ast, res, analyzer, scratch, left, key)
            {
                Some(inner)
            } else {
                occurrence_prefix_dirty(ast, res, analyzer, scratch, right, key)
                    .map(|inner| effect_dirty(analyzer, left) || inner)
            }
        }
        Node::Index(base, index_key, _) => {
            let items = [*base, *index_key];
            seq(ast, res, analyzer, scratch, &items, key)
        }
        Node::Call(function, arguments, _) => {
            let (function, arguments) = (*function, arguments);
            if let Some(inner) = occurrence_prefix_dirty(ast, res, analyzer, scratch, function, key)
            {
                return Some(inner);
            }
            let mut acc = effect_dirty(analyzer, function);
            for argument in arguments {
                if let Some(inner) =
                    occurrence_prefix_dirty(ast, res, analyzer, scratch, *argument, key)
                {
                    return Some(acc || inner);
                }
                acc |= effect_dirty(analyzer, *argument);
            }
            None
        }
        Node::Table(fields) => {
            let mut items = Vec::new();
            for field in fields {
                match field {
                    TableField::Arr(value) | TableField::Name(_, value) => items.push(*value),
                    TableField::KVar(field_key, value) => {
                        items.push(*field_key);
                        items.push(*value);
                    }
                }
            }
            seq(ast, res, analyzer, scratch, &items, key)
        }
        Node::Local(_, expressions) | Node::Return(expressions) => {
            seq(ast, res, analyzer, scratch, expressions, key)
        }
        Node::Callstat(expression) => {
            occurrence_prefix_dirty(ast, res, analyzer, scratch, *expression, key)
        }
        Node::Assign(targets, expressions) => {
            // Target bases and keys evaluate interleaved with the
            // values; count any of them as prefix, conservatively.
            let target_dirty = targets.iter().any(|target| effect_dirty(analyzer, *target));
            seq(ast, res, analyzer, scratch, expressions, key).map(|inner| target_dirty || inner)
        }
        Node::Funcstat(..) => None,
        _ => {
            let mut children = Vec::new();
            storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
                children.push(child)
            });
            seq(ast, res, analyzer, scratch, &children, key)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn inspect_block(
    ast: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    scratch: &mut Ast,
    block: NodeId,
    groups: &mut Vec<Group>,
    order: &mut usize,
) {
    let Node::Block(statements) = ast.node(block) else {
        return;
    };
    let mut found = Vec::<(String, NodeId, Vec<usize>, Vec<BindingId>)>::new();
    let mut positions = HashMap::<String, usize>::new();
    for (index, statement) in statements.iter().copied().enumerate() {
        if is_control(ast.node(statement)) {
            continue;
        }
        inspect_signed_expression(
            ast,
            res,
            analyzer,
            scratch,
            statement,
            index,
            &mut found,
            &mut positions,
        );
    }
    for (key, expression, occurrence_statements, reads) in found {
        // When the factored expression reads a table field, its value
        // can change under ANY field store, not just a store through
        // the binding it reads: `u = t` aliases the same table, so a
        // store `u.value = ...` rewrites what `t.value` reads (S04).
        // Mirror the available-expression reuse model (Q05): such a
        // group splits at any statement that may store fields.
        let reads_fields = contains_mutable_index_read(scratch, analyzer, expression);
        if occurrence_statements.len() < 3 {
            continue;
        }
        let mut part = Vec::<usize>::new();
        let flush = |part: &mut Vec<usize>, groups: &mut Vec<Group>, order: &mut usize| {
            if part.len() >= 3 {
                let estimate =
                    part.len() as isize * (key.len() as isize - 1) - key.len() as isize - 2;
                if estimate > 1 {
                    groups.push(Group {
                        block,
                        key: key.clone(),
                        expression,
                        statements: part.clone(),
                        reads: reads.clone(),
                        estimate,
                        order: *order,
                    });
                    *order += 1;
                }
            }
            part.clear();
        };
        let base_may_throw = expression_may_throw(scratch, expression);
        let mut effect = empty_effects();
        for statement in occurrence_statements {
            // Statement-granularity exclusions (T04/T05): the split
            // model below reasons about whole statements, so an
            // occurrence whose own statement cannot be treated
            // atomically is left unfactored and also splits the run.
            // (T04) When the factored expression reads a field, an
            // occurrence statement holding a field-touching call --
            // in its own argument list, say -- has no barrier
            // position inside the statement: the read and the
            // call's store are ordered within it, and grouping
            // would serve the read the carrier's pre-store value.
            // (A direct field store in the statement needs no such
            // exclusion: the store lands after the statement's
            // values evaluate, and the split checks below already
            // police it.)
            // (T05) When the factored expression may throw, hoisting
            // it into the carrier definition in front of the
            // statement must not cross an observable effect that the
            // statement's own evaluation performs before reaching
            // the occurrence.
            if (reads_fields
                && statement_has_field_touching_call(ast, res, analyzer, statements[statement]))
                || (base_may_throw
                    && occurrence_prefix_dirty(
                        ast,
                        res,
                        analyzer,
                        scratch,
                        statements[statement],
                        &key,
                    ) == Some(true))
            {
                flush(&mut part, groups, order);
                continue;
            }
            if let Some(previous) = part.last().copied() {
                analyzer.effects_for_statement_into(statements[previous], &mut effect);
                let previous_effect = &effect;
                let mut split = reads.iter().any(|bid| previous_effect.writes.contains(bid))
                    || statement_writes_group_member(ast, res, statements[previous], &reads)
                    || (reads_fields
                        && statement_may_store_fields(ast, res, analyzer, statements[previous]))
                    || (!preserves_binding_values(ast, analyzer, statements[previous])
                        && (previous_effect.calls || previous_effect.ordered));
                for current in statements
                    .iter()
                    .take(statement)
                    .skip(previous + 1)
                    .copied()
                {
                    if barrier(ast, analyzer, current, &reads, &mut effect)
                        || (reads_fields && statement_may_store_fields(ast, res, analyzer, current))
                    {
                        split = true;
                        break;
                    }
                }
                if split {
                    flush(&mut part, groups, order);
                }
            }
            part.push(statement);
        }
        flush(&mut part, groups, order);
    }
    for statement in statements {
        let mut nested = Vec::new();
        nested_blocks(ast, *statement, &mut nested);
        for child in nested {
            if child != block {
                inspect_block(ast, res, analyzer, scratch, child, groups, order);
            }
        }
    }
}

fn count_bid(ast: &Ast, res: &Resolution, node: NodeId, bid: BindingId) -> usize {
    let mut count = 0usize;
    storm_lua_syntax::ast_utils::walk(ast, node, &mut |id| {
        if matches!(ast.node(id), Node::Name(_))
            && res.node_bid.get(id as usize).copied().flatten() == Some(bid)
        {
            count += 1;
        }
    });
    count
}

fn count_matched_bid(
    ast: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    scratch: &mut Ast,
    node: NodeId,
    bid: BindingId,
    group_key: &str,
) -> usize {
    if !is_atom(ast.node(node)) && safe_signed(ast, analyzer, node) {
        let (_, _, key) = signed_key(scratch, ast, res, node);
        if key == group_key {
            return count_bid(ast, res, node, bid);
        }
    }
    // Assignment targets are write positions: an occurrence of the factored
    // expression there is not a carrier read (the rewrite keeps targets
    // untouched), so it must not count as matched. Counting it would let
    // carrier reuse overwrite a table binding whose member targets still
    // read the table itself.
    if let Node::Assign(_, expressions) = ast.node(node) {
        return expressions
            .iter()
            .map(|expression| {
                count_matched_bid(ast, res, analyzer, scratch, *expression, bid, group_key)
            })
            .sum();
    }
    // Same for a function statement: its target is a write position and
    // must not count as matched; only the function child is inspected.
    if let Node::Funcstat(_, function) = ast.node(node) {
        return count_matched_bid(ast, res, analyzer, scratch, *function, bid, group_key);
    }
    let mut count = 0usize;
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        count += count_matched_bid(ast, res, analyzer, scratch, child, bid, group_key)
    });
    count
}

/// Whether `statement` writes through a member target (`root.member = ...`
/// or `root[key] = ...`) whose root binding is read by the factored
/// expression. Such a write changes the factored expression's value but is
/// not a binding write, so the binding-based split check misses it when the
/// writing statement is itself an occurrence statement (the barrier check
/// only runs on statements strictly between occurrences, where it does
/// catch the intervening form through `may_throw`).
fn statement_writes_group_member(
    ast: &Ast,
    res: &Resolution,
    statement: NodeId,
    reads: &[BindingId],
) -> bool {
    let targets: &[NodeId] = match ast.node(statement) {
        Node::Assign(targets, _) => targets,
        Node::Funcstat(target, _) => std::slice::from_ref(target),
        _ => return false,
    };
    targets.iter().any(|target| {
        let mut current = *target;
        loop {
            match ast.node(current) {
                Node::Index(base, ..) => current = *base,
                Node::Methodname(base, _) => current = *base,
                Node::Paren(inner) => current = *inner,
                Node::Name(_) => {
                    return res
                        .node_bid
                        .get(current as usize)
                        .copied()
                        .flatten()
                        .is_some_and(|bid| reads.contains(&bid));
                }
                _ => return false,
            }
        }
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScanState {
    Live,
    Overwritten,
    Unsafe,
}

fn scan_old_value(
    ast: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    block: NodeId,
    start: usize,
    bid: BindingId,
) -> ScanState {
    let Node::Block(statements) = ast.node(block) else {
        return ScanState::Live;
    };
    let mut effect = empty_effects();
    for statement in statements.iter().skip(start).copied() {
        match ast.node(statement) {
            Node::Assign(targets, expressions) => {
                if expressions.iter().any(|expression| {
                    analyzer.effects_for_expr_into(*expression, &mut effect);
                    effect.reads.contains(&bid)
                }) {
                    return ScanState::Unsafe;
                }
                if targets.iter().any(|target| {
                    matches!(ast.node(*target), Node::Name(_))
                        && res.node_bid.get(*target as usize).copied().flatten() == Some(bid)
                }) {
                    return ScanState::Overwritten;
                }
            }
            Node::If(arms, else_block) => {
                if arms.iter().any(|arm| {
                    analyzer.effects_for_expr_into(arm.cond, &mut effect);
                    effect.reads.contains(&bid)
                }) {
                    return ScanState::Unsafe;
                }
                let mut states = arms
                    .iter()
                    .map(|arm| scan_old_value(ast, res, analyzer, arm.body, 0, bid))
                    .collect::<Vec<_>>();
                states.push(else_block.map_or(ScanState::Live, |body| {
                    scan_old_value(ast, res, analyzer, body, 0, bid)
                }));
                if states.contains(&ScanState::Unsafe) {
                    return ScanState::Unsafe;
                }
                if states.iter().all(|state| *state == ScanState::Overwritten) {
                    return ScanState::Overwritten;
                }
            }
            Node::Do(body) => {
                let state = scan_old_value(ast, res, analyzer, *body, 0, bid);
                if state != ScanState::Live {
                    return state;
                }
            }
            Node::Return(_) | Node::Break => {
                analyzer.effects_for_statement_into(statement, &mut effect);
                if effect.reads.contains(&bid) {
                    return ScanState::Unsafe;
                }
                return ScanState::Overwritten;
            }
            _ => {
                analyzer.effects_for_statement_into(statement, &mut effect);
                if effect.reads.contains(&bid) {
                    return ScanState::Unsafe;
                }
            }
        }
    }
    ScanState::Live
}

#[allow(clippy::too_many_arguments)]
fn rewrite_signed(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    scratch: &mut Ast,
    node: NodeId,
    group_key: &str,
    carrier: SymbolId,
    definition: NodeId,
    mut undo: Option<&mut Vec<(NodeId, Node)>>,
) -> NodeId {
    if !is_atom(source.node(node)) && safe_signed(source, analyzer, node) {
        let (sign, _, key) = signed_key(scratch, source, res, node);
        if key == group_key {
            let name = target.push(Node::Name(carrier));
            target
                .nodes
                .derive_from(name, &source.nodes, node, "signed-carrier-read");
            target
                .nodes
                .relate_within(name, definition, "signed-carrier-definition-use");
            return if sign > 0 {
                name
            } else {
                let negated = target.push(Node::Un("-".into(), name));
                target
                    .nodes
                    .derive_from(negated, &source.nodes, node, "signed-carrier-negation");
                negated
            };
        }
    }
    // Assignment targets are write positions, not reads: replacing a
    // factored member expression there (`t.x = t.x + 1` becoming
    // `carrier = carrier + 1`) redirects the write to the carrier and
    // leaves the real table unchanged. Mirror the occurrence scan, which
    // only inspects assignment values: rewrite the values and keep the
    // targets exactly as they are.
    if let Node::Assign(targets, expressions) = source.node(node) {
        let targets = targets.clone();
        let mut rewritten = Vec::with_capacity(expressions.len());
        for expression in expressions {
            rewritten.push(rewrite_signed(
                target,
                source,
                res,
                analyzer,
                scratch,
                *expression,
                group_key,
                carrier,
                definition,
                undo.as_deref_mut(),
            ));
        }
        rewrite_slot(
            target,
            undo,
            node,
            Node::Assign(targets, rewritten),
            "signed-expression-factoring",
        );
        return node;
    }
    // A function statement's target is a write position for the same
    // reason: rewriting a factored member chain there would redefine
    // the carrier instead of the intended table member. Keep the target
    // exactly as it is and rewrite only inside the function child.
    if let Node::Funcstat(statement_target, function) = source.node(node) {
        let kept_target = *statement_target;
        let rewritten_function = rewrite_signed(
            target,
            source,
            res,
            analyzer,
            scratch,
            *function,
            group_key,
            carrier,
            definition,
            undo.as_deref_mut(),
        );
        rewrite_slot(
            target,
            undo,
            node,
            Node::Funcstat(kept_target, rewritten_function),
            "signed-expression-factoring",
        );
        return node;
    }
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        rewrite_signed(
            target,
            source,
            res,
            analyzer,
            scratch,
            child,
            group_key,
            carrier,
            definition,
            undo.as_deref_mut(),
        )
    });
    rewrite_slot(target, undo, node, mapped, "signed-expression-factoring");
    node
}

/// Build one signed-factoring candidate application into `trial`, using
/// `key_scratch` for signed-form key computation. Both arenas must start
/// out holding `scratch`'s arena content (a fresh clone, or a recycled
/// arena restored to that state). When `undo` is given, every slot the
/// build overwrites in `trial` is recorded so the caller can restore the
/// arena for reuse; `key_scratch` is only appended to, so truncating it
/// back to its starting length restores it.
#[allow(clippy::too_many_arguments)]
fn build_signed_candidate(
    trial: &mut Ast,
    key_scratch: &mut Ast,
    source: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    scratch: &Ast,
    group: &Group,
    carrier_name: &str,
    local: bool,
    mut undo: Option<&mut Vec<(NodeId, Node)>>,
) {
    let carrier = trial.strings.intern(carrier_name);
    let Some(&first) = group.statements.first() else {
        return;
    };
    let Some(&last) = group.statements.last() else {
        return;
    };
    let Node::Block(original_statements) = source.node(group.block).clone() else {
        return;
    };
    let mut rewritten = original_statements.clone();
    for index in first..=last {
        rewritten[index] = rewrite_signed(
            trial,
            source,
            res,
            analyzer,
            key_scratch,
            original_statements[index],
            &group.key,
            carrier,
            group.expression,
            undo.as_deref_mut(),
        );
    }
    // The carrier definition must own a fresh copy of the factored
    // expression: `group.expression` is the representative occurrence's
    // node, which stays in place (its home statement is not necessarily
    // inside the rewritten range), so referencing it from the new
    // definition would give that subtree two parents. Any later id-based
    // rewrite would then rewrite both positions at once — for example,
    // available-expression reuse turning `y = h/3` into `y = y` when the
    // representative occurrence is that assignment's right-hand side and
    // the carrier definition is its other parent. The copy is taken from
    // `scratch` (the arena `trial` was cloned from): the representative
    // node lives in that id space and may be a normalization product that
    // does not exist in `source` at all.
    let definition_expression = clone_subtree(trial, scratch, group.expression);
    let definition = if local {
        trial.push(Node::Local(vec![carrier], vec![definition_expression]))
    } else {
        let target = trial.push(Node::Name(carrier));
        trial.nodes.mark_synthetic(target, "signed-carrier-storage");
        trial.push(Node::Assign(vec![target], vec![definition_expression]))
    };
    trial
        .nodes
        .mark_synthetic(definition, "signed-carrier-definition");
    rewritten.insert(first, definition);
    rewrite_slot(
        trial,
        undo,
        group.block,
        Node::Block(rewritten),
        "signed-expression-factoring",
    );
}

/// Build one signed-factoring candidate application on a tracked clone,
/// for committing as the round's winner. Size trials use
/// `measure_signed_candidate` on origin-less recycled arenas instead;
/// both run the exact same build (`build_signed_candidate`), so the
/// committed winner yields the exact same nodes and origins as the
/// previous trial-then-commit path. Size measurement is origin-independent
/// (`measure_renamed_size` only resolves and measures), so winner
/// selection is unchanged.
#[allow(clippy::too_many_arguments)]
fn apply_signed_candidate(
    source: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    scratch: &Ast,
    group: &Group,
    carrier_name: &str,
    local: bool,
) -> Ast {
    let mut trial = clone_ast(scratch);
    let mut key_scratch = clone_ast(scratch);
    build_signed_candidate(
        &mut trial,
        &mut key_scratch,
        source,
        res,
        analyzer,
        scratch,
        group,
        carrier_name,
        local,
        None,
    );
    trial
}

/// Measure one signed-factoring candidate on recycled origin-less arenas.
///
/// The trial is built on an arena without provenance, so every origin
/// call inside the build (`derive_from`, `relate_within`, `rewrite`
/// recording, `mark_synthetic`) is a no-op and losing trials pay no
/// tracking cost. Such a trial must never be committed: it carries no
/// provenance, so the caller replays the winning candidate with
/// `apply_signed_candidate`, running the exact same build on a tracked
/// clone and therefore yielding the exact same nodes and origins.
///
/// Recycling: `trial` and `key_scratch` start out as clones of `scratch`
/// (the round's first candidate) and are restored after each measurement
/// — appended nodes are truncated away and every slot the build
/// overwrote is restored from the undo log — so the next candidate
/// starts from exactly the state a fresh clone would have, without
/// paying the clone's copy-on-write detach of the whole values buffer
/// per candidate. The restore stays detach-free only because the trial
/// is measured through the unmemoized route (`measured_trial`): nothing
/// retains an `Arc` clone of the recycled buffers, so they stay
/// uniquely owned from the first candidate's build onward. As a guard,
/// the recycled arenas are re-cloned from scratch whenever `scratch`
/// itself has moved since they were taken (same length, arena
/// generation and symbol count required); the candidate loop only reads
/// `scratch`, so this is not expected to trigger. The one state that is
/// not restored is the trial interner, which only grows: interning a
/// carrier name leaves earlier
/// candidates' carrier symbols behind as unreferenced entries. The
/// interner is append-only, so the ids of pre-existing strings are
/// unchanged, and measurement only resolves the symbols the tree
/// actually references, so measured sizes are identical to a
/// fresh-clone build.
#[allow(clippy::too_many_arguments)]
fn measure_signed_candidate(
    source: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    scratch: &Ast,
    group: &Group,
    carrier_name: &str,
    local: bool,
    root: NodeId,
    trial: &mut Option<Ast>,
    key_scratch: &mut Option<Ast>,
    undo: &mut Vec<(NodeId, Node)>,
    recycle_stamp: &mut Option<(usize, u64, usize)>,
) -> usize {
    let stamp = (
        scratch.nodes.len(),
        scratch.nodes.generation(),
        scratch.strings.symbol_count(),
    );
    if *recycle_stamp != Some(stamp) {
        *trial = None;
        *key_scratch = None;
        undo.clear();
        *recycle_stamp = Some(stamp);
    }
    debug_assert!(undo.is_empty());
    let base_len = stamp.0;
    let trial =
        trial.get_or_insert_with(|| storm_lua_syntax::ast_utils::clone_without_origins(scratch));
    let key_scratch = key_scratch
        .get_or_insert_with(|| storm_lua_syntax::ast_utils::clone_without_origins(scratch));
    debug_assert_eq!(trial.nodes.len(), base_len);
    debug_assert_eq!(key_scratch.nodes.len(), base_len);
    build_signed_candidate(
        trial,
        key_scratch,
        source,
        res,
        analyzer,
        scratch,
        group,
        carrier_name,
        local,
        Some(&mut *undo),
    );
    let size = measured_trial(trial, root);
    for (node, old) in undo.drain(..).rev() {
        trial.nodes[node as usize] = old;
    }
    trial.nodes.truncate(base_len);
    key_scratch.nodes.truncate(base_len);
    size
}

fn measured(ast: &Ast, root: NodeId) -> usize {
    measure_renamed_size(ast, root)
}

/// Measure a recycled trial arena. Trials go through the unmemoized
/// route: the renamed-size memo retains an `Arc` clone of every buffer
/// it measures, so after a memoized measurement the recycled trial
/// buffer is shared, and the restore's first slot write would detach
/// the whole buffer at its built length — the exact cost recycling
/// exists to avoid (cycle 97 measured recycling with the memo as a net
/// detach increase for this reason). The measured value is identical
/// either way; see `measure_renamed_size_unmemoized`.
fn measured_trial(ast: &Ast, root: NodeId) -> usize {
    measure_renamed_size_unmemoized(ast, root)
}

/// Conservative active-local maximum of the enclosing function for
/// every block in the chunk, keyed by block node. The chunk itself is
/// the outermost function (zero parameters); method definitions add
/// their implicit `self` parameter.
fn block_local_budgets(source: &Ast, root: NodeId) -> HashMap<NodeId, usize> {
    fn walk(
        source: &Ast,
        node: NodeId,
        fn_body: NodeId,
        fn_params: usize,
        memo: &mut HashMap<NodeId, usize>,
        budgets: &mut HashMap<NodeId, usize>,
    ) {
        if matches!(source.node(node), Node::Block(_)) {
            let max = *memo.entry(fn_body).or_insert_with(|| {
                super::single_member_cache::max_active_locals(source, fn_params, fn_body)
            });
            budgets.insert(node, max);
        }
        match source.node(node).clone() {
            Node::Funcstat(target, function) => {
                walk(source, target, fn_body, fn_params, memo, budgets);
                if let Node::Function(params, _, body) = source.node(function).clone() {
                    let extra = usize::from(matches!(source.node(target), Node::Methodname(..)));
                    walk(source, body, body, params.len() + extra, memo, budgets);
                } else {
                    walk(source, function, fn_body, fn_params, memo, budgets);
                }
            }
            Node::Function(params, _, body) => {
                walk(source, body, body, params.len(), memo, budgets);
            }
            _ => {
                let mut children = Vec::new();
                storm_lua_syntax::ast_utils::for_each_child(source, node, &mut |child| {
                    children.push(child);
                });
                for child in children {
                    walk(source, child, fn_body, fn_params, memo, budgets);
                }
            }
        }
    }
    let mut memo = HashMap::new();
    let mut budgets = HashMap::new();
    walk(source, root, root, 0, &mut memo, &mut budgets);
    budgets
}

pub fn factor_signed_expressions(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    max_rounds: usize,
) -> PassResult {
    if !aggressive {
        return PassResult {
            root,
            saved: None,
            details: None,
        };
    }
    let original = measure_size(ast, root);
    let mut factored = 0usize;
    for _ in 0..max_rounds {
        let source = clone_ast(ast);
        let res = resolve(&source, root);
        let analyzer = EffectAnalyzer::new(&source, &res, root, true);
        let budgets = block_local_budgets(&source, root);
        let baseline = measured(&source, root);
        let mut used_names = HashSet::<String>::new();
        storm_lua_syntax::ast_utils::walk(&source, root, &mut |id| {
            if let Node::Name(symbol) = source.node(id) {
                used_names.insert(source.strings.get(*symbol).to_string());
            }
        });
        let mut scratch = clone_ast(&source);
        let mut groups = Vec::<Group>::new();
        let mut order = 0usize;
        inspect_block(
            &source,
            &res,
            &analyzer,
            &mut scratch,
            root,
            &mut groups,
            &mut order,
        );
        // Stable sort by estimate descending. Explicit order retains TS stable-sort tie behavior.
        groups.sort_by_key(|group| (Reverse(group.estimate), group.order));
        groups.truncate(16);

        let mut best: Option<(Group, String, bool, usize)> = None;
        let mut trial_recycle: Option<Ast> = None;
        let mut key_recycle: Option<Ast> = None;
        let mut trial_undo: Vec<(NodeId, Node)> = Vec::new();
        let mut trial_stamp: Option<(usize, u64, usize)> = None;
        for group in groups {
            let mut serial = 0usize;
            let mut synthetic_name = format!("__stormmin_signed_{serial}");
            while used_names.contains(&synthetic_name) {
                serial += 1;
                synthetic_name = format!("__stormmin_signed_{serial}");
            }
            let first = group.statements[0];
            #[expect(
                clippy::unwrap_used,
                reason = "A candidate group has already been filtered to multiple matching statements"
            )]
            let last = *group.statements.last().unwrap();
            let Node::Block(statements) = source.node(group.block) else {
                continue;
            };

            let mut reusable = Vec::<BindingId>::new();
            for bid in group.reads.iter().copied() {
                let binding = &res.bindings[bid as usize];
                // Globals persist across callback invocations; overwriting one
                // as a temporary carrier changes the next onTick even when the
                // value is not read again in this lexical block.  Restrict
                // carrier reuse to lexical locals.
                if binding.fixed || binding.function_node.is_some() || binding.scope == 0 {
                    continue;
                }
                // Reuse candidates are exactly the bindings the matched
                // expression reads, so the definition expression reads the
                // carrier itself. That is only sound when the defining block
                // executes once with a fresh carrier. When the carrier is
                // declared outside the group block, the definition overwrites
                // a binding whose old value may still be observed after the
                // group block: `scan_old_value` only scans the group block
                // itself, never the ancestor blocks after the statement that
                // contains it, so a read after a nested once-only block (a
                // Do/If body at top level, where no loop or function makes
                // the block "repeated") would see the factored value instead
                // of the old value. The same argument covers the repeated
                // case (a loop body or function body re-executes with the
                // carrier still holding the previous execution value, for
                // example an outer for variable overwritten inside an inner
                // loop body, turning the next definition into an index by a
                // table and faulting). Require the declaration to be inside
                // the group block in all cases and fall back to the synthetic
                // local carrier otherwise; same-block reuse (the profitable
                // case) is unaffected.
                let decl_inside = binding.decl_node.is_some_and(|decl| {
                    let mut found = false;
                    storm_lua_syntax::ast_utils::walk(&source, group.block, &mut |id| {
                        if id == decl {
                            found = true;
                        }
                    });
                    found
                });
                if !decl_inside {
                    continue;
                }
                let mut total = 0usize;
                let mut matched = 0usize;
                let mut count_scratch = clone_ast(&scratch);
                for statement in statements.iter().take(last + 1).skip(first).copied() {
                    total += count_bid(&source, &res, statement, bid);
                    matched += count_matched_bid(
                        &source,
                        &res,
                        &analyzer,
                        &mut count_scratch,
                        statement,
                        bid,
                        &group.key,
                    );
                }
                if total == matched
                    && total > 0
                    && scan_old_value(&source, &res, &analyzer, group.block, last + 1, bid)
                        != ScanState::Unsafe
                {
                    reusable.push(bid);
                }
            }

            let mut carriers = Vec::<(String, bool)>::new();
            carriers.push((synthetic_name, true));
            for bid in reusable {
                carriers.push((
                    source
                        .strings
                        .get(res.bindings[bid as usize].name)
                        .to_string(),
                    false,
                ));
            }

            for (carrier_name, local) in carriers {
                // Local budget: the synthetic carrier is a new local
                // in the enclosing function. Skip it when the function
                // is already at the VM's 200 simultaneous-local limit
                // (a reused existing local adds nothing). Carriers
                // applied in earlier rounds are ordinary locals in
                // this round's input, so the shared whole-function
                // maximum accounts for them.
                if local
                    && budgets.get(&group.block).copied().unwrap_or(0) + 1
                        > super::single_member_cache::MAX_LOCALS
                {
                    continue;
                }
                let size = measure_signed_candidate(
                    &source,
                    &res,
                    &analyzer,
                    &scratch,
                    &group,
                    &carrier_name,
                    local,
                    root,
                    &mut trial_recycle,
                    &mut key_recycle,
                    &mut trial_undo,
                    &mut trial_stamp,
                );
                if size < baseline
                    && best
                        .as_ref()
                        .is_none_or(|(_, _, _, best_size)| size < *best_size)
                {
                    best = Some((group.clone(), carrier_name, local, size));
                }
            }
        }
        let Some((group, carrier_name, local, _)) = best else {
            break;
        };
        // Replay the winning candidate on a tracked clone: the origin-less
        // trial above must never be committed (see `apply_signed_candidate`).
        let winner = apply_signed_candidate(
            &source,
            &res,
            &analyzer,
            &scratch,
            &group,
            &carrier_name,
            local,
        );
        *ast = winner;
        factored += 1;
    }
    PassResult {
        root,
        saved: Some(original.saturating_sub(measure_size(ast, root)) as u64),
        details: if factored == 0 {
            None
        } else {
            Some(vec![format!("factored={factored}")])
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn output(source: &str, aggressive: bool) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = factor_signed_expressions(&mut ast, root, aggressive, 10);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn groups_complex_expressions() {
        let source = "function onTick()local a=input.getNumber(1)local b=input.getNumber(2)output.setNumber(1,a*b*123)output.setNumber(2,-(a*b*123))output.setNumber(3,-(a*b*123))end";
        assert_eq!(
            output(source, true),
            "function onTick()local a,b=input.getNumber(1),input.getNumber(2)a=a*b*123 output.setNumber(1,a)output.setNumber(2,-(a))output.setNumber(3,-(a))end"
        );
    }

    #[test]
    fn groups_with_multiple_signs() {
        let source = "function onTick()local x=input.getNumber(1)local y=input.getNumber(2)local z=input.getNumber(3)output.setNumber(1,x*y*z)output.setNumber(2,-(x*y*z))output.setNumber(3,x*y*z)output.setNumber(4,-(x*y*z))end";
        assert_eq!(
            output(source, true),
            "function onTick()local x,y,z=input.getNumber(1),input.getNumber(2),input.getNumber(3)x=x*y*z output.setNumber(1,x)output.setNumber(2,-(x))output.setNumber(3,x)output.setNumber(4,-(x))end"
        );
    }

    #[test]
    fn skips_safe_signed() {
        let source = "function onTick()local a=input.getNumber(1)output.setNumber(1,a)output.setNumber(2,-a)end";
        assert_eq!(output(source, true), source);
    }

    #[test]
    fn wins_one_group_and_discards_losing_trials() {
        let source = "function onTick()local a=input.getNumber(1)local b=input.getNumber(2)local c=input.getNumber(3)local d=input.getNumber(4)output.setNumber(1,a*b*123)output.setNumber(2,-(a*b*123))output.setNumber(3,-(a*b*123))output.setNumber(4,c*d)output.setNumber(5,-(c*d))end";
        assert_eq!(output(source, true), "function onTick()local a,b,c,d=input.getNumber(1),input.getNumber(2),input.getNumber(3),input.getNumber(4)a=a*b*123 output.setNumber(1,a)output.setNumber(2,-(a))output.setNumber(3,-(a))output.setNumber(4,c*d)output.setNumber(5,-(c*d))end");
    }

    #[test]
    fn does_not_group_without_profit() {
        let source = "function onTick()local a=input.getNumber(1)local b=input.getNumber(2)output.setNumber(1,a*b)output.setNumber(2,-(a*b))end";
        assert_eq!(
            output(source, true),
            "function onTick()local a,b=input.getNumber(1),input.getNumber(2)output.setNumber(1,a*b)output.setNumber(2,-(a*b))end"
        );
    }

    #[test]
    fn does_not_reuse_persistent_global_as_carrier() {
        let source = "state={index=0,best={rate=1,error=2}}function onTick()state.index=state.index+1 if state.best then output.setNumber(1,state.best.rate)output.setNumber(2,state.best.error)end end";
        let out = output(source, true);
        assert!(!out.contains("state=state.best"), "{out}");
    }

    #[test]
    fn does_not_factor_field_read_across_aliased_field_store() {
        // S04: the factored expression `t.value` reads a field, and
        // each occurrence statement stores through the alias `u`
        // (`local u = t`), rewriting the very field the expression
        // reads. Grouping them would reuse the stale first value for
        // every later read. The invalidation must not compare binding
        // ids (u and t are distinct bindings of one table): mirroring
        // the Q05 model, a field-reading group splits at any statement
        // that may store fields.
        let source = "local t={value=7} local u=t u.value=t.value-1 u.value=t.value-2 u.value=t.value-3 return t.value";
        let out = output(source, true);
        assert!(!out.contains("__stormmin_signed"), "{out}");
        assert!(out.contains("u.value=t.value-3"), "{out}");
    }

    #[test]
    fn does_not_factor_field_read_across_higher_order_call_in_occurrence() {
        // T04: the mutating call sits inside the occurrence statement
        // itself, in the same argument list as the read, reached
        // through a function parameter. At statement granularity
        // there is no barrier position inside the statement, so the
        // occurrence must be excluded from the group entirely (and
        // the remaining singletons cannot form one).
        let source = "local function run(t,f) output.setNumber(1,t.longname) output.setNumber(f(),t.longname) output.setNumber(3,t.longname) end local t={longname=7} run(t,function() t.longname=8 return 2 end)";
        let out = output(source, true);
        assert!(!out.contains("__stormmin_signed"), "{out}");
        assert!(out.contains("output.setNumber(f(),t.longname)"), "{out}");
    }

    #[test]
    fn does_not_hoist_throwing_read_across_observable_call() {
        // T05: the factored read `(nil).x` always throws. In the
        // original, the first statement emits its inner event before
        // the read throws; factoring would evaluate the read in the
        // carrier definition in front of the statement and the event
        // would never happen. The statement's prefix up to the
        // occurrence holds an observable call, so it cannot join a
        // group, and the rest cannot reach three.
        let source = "output.setNumber(output.setNumber(9,9),(nil).x) output.setNumber(2,(nil).x) output.setNumber(3,(nil).x)";
        let out = output(source, true);
        assert!(!out.contains("__stormmin_signed"), "{out}");
        assert!(
            out.contains("output.setNumber(output.setNumber(9,9),(nil).x)"),
            "{out}"
        );
    }

    #[test]
    fn still_factors_field_reads_in_api_call_arguments() {
        // The T05 prefix rule must not gut the common shape: the
        // callee position of a proven builtin contributes no throw,
        // so field reads in plain argument positions keep factoring.
        let source = "local t={longname=7} output.setNumber(1,t.longname) output.setNumber(2,t.longname) output.setNumber(3,t.longname)";
        let out = output(source, true);
        assert!(out.contains("__stormmin_signed"), "{out}");
    }

    #[test]
    fn does_not_factor_field_read_across_field_storing_call() {
        // The barrier side of the same invalidation: a call between
        // occurrences whose body stores through the alias rewrites the
        // read field. (The effect barrier also splits here today; this
        // pins the behavior so the field-store model, not an accident
        // of effect summaries, is what guarantees it.)
        let source = "local t={value=7} local u=t local function s() u.value=0 end local a=t.value-1 s() local b=t.value-2 local c=t.value-3 return a+b+c";
        let out = output(source, true);
        assert!(!out.contains("__stormmin_signed"), "{out}");
    }

    #[test]
    fn does_not_factor_field_read_across_intervening_field_store() {
        // The same invalidation for a store statement sitting strictly
        // between two occurrences, again through an alias of the read
        // table. (Also split by the effect barrier today; pinned as a
        // guarantee of the field-store model.)
        let source = "local t={value=7} local u=t local a=t.value-1 u.value=0 local b=t.value-2 local c=t.value-3 return a+b+c";
        let out = output(source, true);
        assert!(!out.contains("__stormmin_signed"), "{out}");
    }

    #[test]
    fn needs_aggressive_mode() {
        let source = "function onTick()local a=input.getNumber(1)local b=input.getNumber(2)output.setNumber(1,a*b*123)output.setNumber(2,-(a*b*123))output.setNumber(3,-(a*b*123))end";
        assert_eq!(
            output(source, false),
            "function onTick()local a,b=input.getNumber(1),input.getNumber(2)output.setNumber(1,a*b*123)output.setNumber(2,-(a*b*123))output.setNumber(3,-(a*b*123))end"
        );
    }
    #[test]
    fn does_not_reuse_outer_for_variable_as_carrier_in_loop_body() {
        let source = "function onDraw()local rows={{1,2,3,4,5,6,7,8}}for i=1,#rows do for j=5,#rows[i],4 do output.setNumber(1,rows[i][j])output.setNumber(2,rows[i][j+1])output.setNumber(3,rows[i][j+2])output.setNumber(4,rows[i][j+3])end end end";
        assert_eq!(output(source, true), source);
    }

    #[test]
    fn does_not_reuse_outer_carrier_in_nested_do_block() {
        // Carrier declared in the outer Do, group in the inner Do, old value
        // read after the inner Do: reusing `c` would overwrite it with the
        // factored value. The synthetic local must be used instead.
        let source = "do local c=input.getNumber(1)+1 do output.setNumber(1,c*123)output.setNumber(2,c*123)output.setNumber(3,c*123)output.setNumber(4,c*123)end output.setNumber(5,c)end";
        let out = output(source, true);
        assert!(!out.contains("c=c*123"), "{out}");
        assert!(out.contains("__stormmin_signed_0"), "{out}");
        assert!(out.contains("output.setNumber(5,c)"), "{out}");
    }

    #[test]
    fn does_not_reuse_outer_carrier_in_nested_if_arm() {
        let source = "do local c=input.getNumber(1)+1 if input.getNumber(2)>0 then output.setNumber(1,c*123)output.setNumber(2,c*123)output.setNumber(3,c*123)output.setNumber(4,c*123)end output.setNumber(5,c)end";
        let out = output(source, true);
        assert!(!out.contains("c=c*123"), "{out}");
        assert!(out.contains("__stormmin_signed_0"), "{out}");
        assert!(out.contains("output.setNumber(5,c)"), "{out}");
    }

    // The representative occurrence of a factored expression can be the
    // right-hand side of an assignment that sits before the rewritten
    // statement range (`y = h/4` below). The carrier definition must own a
    // fresh copy of the expression: referencing the occurrence's node from
    // the new definition statement would give that subtree two parents, and
    // any later id-based rewrite would rewrite both positions at once.
    const CARRIER_SHARING_SOURCE: &str = "function onDraw()\n\tw = screen.getWidth()\n\th = screen.getHeight()\n\ty = h/4\n\tscreen.drawRectF(0, 0, w, h/4)\n\tscreen.drawTriangleF(w/5, h/9, 3*w/5, h/11, 3*w/4, h/4)\n\tscreen.drawTriangleF(9*w/10, 0, 10*w/11, 0, 3*w/4, h/4)\n\tscreen.drawTriangleF(w/4, h-0, 2*w/4, h-0, 3*w/4, h-h/4)\n\tscreen.drawTriangleF(w/5, h-h/9, 3*w/5, h-h/11, 3*w/4, h-h/4)\n\tscreen.drawTriangleF(9*w/10, h-0, 10*w/11, h-0, 3*w/4, h-h/4)\n\tscreen.drawTriangleF(w/5, h/9, wm, h/11, 3*w/4, h/4)\n\tscreen.drawTriangleF(0, h/4, w/5, 2*h/6, 2*w/6, hm)\nend";

    #[test]
    fn carrier_definition_does_not_share_representative_node() {
        let (mut ast, root) = parse_source(CARRIER_SHARING_SOURCE).expect("parse");
        let result = factor_signed_expressions(&mut ast, root, true, 10);
        assert!(
            result
                .details
                .as_ref()
                .is_some_and(|d| d.iter().any(|d| d == "factored=2")),
            "expected both expression groups to factor: {:?}",
            result.details
        );
        // Tree invariant: every node has at most one parent.
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![result.root];
        while let Some(id) = stack.pop() {
            storm_lua_analysis::resolver::for_each_child(&ast, id, &mut |child| {
                assert!(
                    seen.insert(child),
                    "node {child} has two parents after factoring"
                );
                stack.push(child);
            });
        }
        // The pre-existing assignment keeps its own right-hand side.
        let out = Printer::new(&ast, false).output(result.root);
        assert!(out.contains("y=h/4"), "{out}");
    }

    #[test]
    fn factor_then_reuse_keeps_representative_assignment() {
        // Regression composition: with the representative node shared,
        // available-expression reuse rewrote the carrier definition's
        // right-hand side to the reaching carrier and, through the shared
        // node, also turned `y = h/4` into `y = y`, losing the definition.
        let (mut ast, root) = parse_source(CARRIER_SHARING_SOURCE).expect("parse");
        let factored = factor_signed_expressions(&mut ast, root, true, 10);
        let reused = crate::passes::available_expressions::reuse_available_expressions(
            &mut ast,
            factored.root,
        );
        let out = Printer::new(&ast, false).output(reused.root);
        assert!(!out.contains("y=y"), "{out}");
        assert!(out.contains("y=h/4"), "{out}");
    }
    #[test]
    fn synthetic_carrier_skips_function_at_the_local_limit() {
        // 198 parameters + the a/b locals reach the limit; the
        // synthetic carrier would be the 201st local. (The existing
        // locals stay live after the group, so neither can serve as
        // the reuse carrier.)
        let params = (1..=198)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "function onTick({params})local a=input.getNumber(1)local b=input.getNumber(2)output.setNumber(1,a*b*123)output.setNumber(2,-(a*b*123))output.setNumber(3,-(a*b*123))output.setNumber(9,a+b)end"
        );
        assert!(!output(&source, true).contains("__stormmin_signed"));
    }

    #[test]
    fn synthetic_carrier_fires_below_the_local_limit() {
        let params = (1..=197)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "function onTick({params})local a=input.getNumber(1)local b=input.getNumber(2)output.setNumber(1,a*b*123)output.setNumber(2,-(a*b*123))output.setNumber(3,-(a*b*123))output.setNumber(9,a+b)end"
        );
        assert!(output(&source, true).contains("__stormmin_signed"));
    }

    // A factored member expression can occur in statements that also write
    // it (`t.longname = t.longname + 1`). The write must keep targeting the
    // real table: replacing the target with the carrier turns the write
    // into a carrier update and the table never changes (Codex run-2
    // finding 11). With a write in every occurrence statement, every group
    // part splits at the write, so nothing is factored at all.
    #[test]
    fn does_not_factor_across_member_writes_in_occurrence_statements() {
        let source = "local t,c={longname=3},0 t.longname=t.longname+1 c=c+t.longname t.longname=t.longname+1 c=c+t.longname t.longname=t.longname+1 c=c+t.longname return c,t";
        assert_eq!(output(source, true), source);
    }

    // A member write as the last statement of a group range is sound to
    // keep: its right-hand side still reads the pre-write value through
    // the carrier, but the target itself must stay the real member.
    #[test]
    fn keeps_member_target_when_write_ends_a_group() {
        let source = "local t={longname=3}local a=t.longname local b=t.longname local c=t.longname t.longname=t.longname+1 return a+b+c,t.longname";
        assert_eq!(
            output(source, true),
            "local t={longname=3}local __stormmin_signed_0=t.longname local a,b,c=__stormmin_signed_0,__stormmin_signed_0,__stormmin_signed_0 t.longname=__stormmin_signed_0+1 return a+b+c,t.longname"
        );
    }

    // A member write as the first statement of a range: the write stays
    // intact and the reads after it factor through a fresh carrier that
    // is defined after the write.
    #[test]
    fn factors_reads_after_a_leading_member_write() {
        let source = "local t={longname=3}t.longname=t.longname+1 local a=t.longname local b=t.longname local c=t.longname return a+b+c,t.longname";
        assert_eq!(
            output(source, true),
            "local t={longname=3}t.longname=t.longname+1 local __stormmin_signed_0=t.longname local a,b,c=__stormmin_signed_0,__stormmin_signed_0,__stormmin_signed_0 return a+b+c,t.longname"
        );
    }

    // A function statement whose target is the factored member chain is
    // a write position, exactly like an assignment target (Codex run-3
    // finding 4): the occurrence scan must not count it, the split check
    // must treat it as a member write, and the rewrite must keep the
    // target verbatim or the table member is never (re)defined.
    #[test]
    fn keeps_funcstat_member_target_verbatim() {
        let source = "local t={xxxxx=1}local a=t.xxxxx local b=t.xxxxx local c=t.xxxxx function t.xxxxx()return 1 end return a+b+c,type(t.xxxxx)";
        let out = output(source, true);
        assert!(out.contains("function t.xxxxx()"), "{out}");
        assert!(!out.contains("function __stormmin_signed"), "{out}");
    }

    // A function-statement member write inside a group range splits the
    // group: reads after it see the function the statement installed,
    // not the value the earlier reads factored.
    #[test]
    fn funcstat_member_write_splits_group() {
        let source = "local t={longname=3}local a=t.longname local b=t.longname function t.longname()return 1 end local c=t.longname return a+b,type(t.longname)";
        let out = output(source, true);
        assert!(out.contains("function t.longname()"), "{out}");
        assert!(!out.contains("local c=__stormmin_signed"), "{out}");
    }

    // Carrier reuse must not overwrite the table binding when a statement
    // in the range writes through one of its members: the member target
    // still reads the table, so the target occurrence is not a matched
    // carrier read and reuse is rejected.
    #[test]
    fn does_not_reuse_table_binding_across_a_member_write() {
        let source = "local t={longname=3}local a=t.longname local b=t.longname local c=t.longname t.longname=t.longname+1 return a+b+c";
        let out = output(source, true);
        assert!(!out.contains("t=t.longname"), "{out}");
        assert!(out.contains("t.longname=__stormmin_signed_0+1"), "{out}");
    }
}
