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
use crate::scope_rename::scope_rename_fast;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId, TableField};
use storm_lua_syntax::size::measure_size;

use super::immutable_values::expression_key;

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
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
) -> bool {
    if is_control(ast.node(statement)) {
        return true;
    }
    let effect = analyzer.effects_for_statement(statement);
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
        for statement in occurrence_statements {
            if let Some(previous) = part.last().copied() {
                let mut split = false;
                for current in statements
                    .iter()
                    .take(statement)
                    .skip(previous + 1)
                    .copied()
                {
                    if barrier(ast, analyzer, current, &reads) {
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
    let mut count = 0usize;
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        count += count_matched_bid(ast, res, analyzer, scratch, child, bid, group_key)
    });
    count
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
    for statement in statements.iter().skip(start).copied() {
        match ast.node(statement) {
            Node::Assign(targets, expressions) => {
                if expressions
                    .iter()
                    .any(|expression| analyzer.effects_for_expr(*expression).reads.contains(&bid))
                {
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
                if arms
                    .iter()
                    .any(|arm| analyzer.effects_for_expr(arm.cond).reads.contains(&bid))
                {
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
                if analyzer
                    .effects_for_statement(statement)
                    .reads
                    .contains(&bid)
                {
                    return ScanState::Unsafe;
                }
                return ScanState::Overwritten;
            }
            _ => {
                if analyzer
                    .effects_for_statement(statement)
                    .reads
                    .contains(&bid)
                {
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
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        rewrite_signed(
            target, source, res, analyzer, scratch, child, group_key, carrier, definition,
        )
    });
    target
        .nodes
        .rewrite(node, mapped, "signed-expression-factoring");
    node
}

fn measured(ast: &Ast, root: NodeId) -> usize {
    let renamed = scope_rename_fast(ast, root);
    measure_size(&renamed.ast, renamed.root)
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

        let mut best: Option<(Ast, usize)> = None;
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
                let mut trial = clone_ast(&scratch);
                let carrier = trial.strings.intern(&carrier_name);
                let mut key_scratch = clone_ast(&scratch);
                let Node::Block(original_statements) = source.node(group.block).clone() else {
                    continue;
                };
                let mut rewritten = original_statements.clone();
                for index in first..=last {
                    rewritten[index] = rewrite_signed(
                        &mut trial,
                        &source,
                        &res,
                        &analyzer,
                        &mut key_scratch,
                        original_statements[index],
                        &group.key,
                        carrier,
                        group.expression,
                    );
                }
                let definition = if local {
                    trial.push(Node::Local(vec![carrier], vec![group.expression]))
                } else {
                    let target = trial.push(Node::Name(carrier));
                    trial.nodes.mark_synthetic(target, "signed-carrier-storage");
                    trial.push(Node::Assign(vec![target], vec![group.expression]))
                };
                trial
                    .nodes
                    .mark_synthetic(definition, "signed-carrier-definition");
                rewritten.insert(first, definition);
                trial.nodes.rewrite(
                    group.block,
                    Node::Block(rewritten),
                    "signed-expression-factoring",
                );
                let size = measured(&trial, root);
                if size < baseline && best.as_ref().is_none_or(|(_, best_size)| size < *best_size) {
                    best = Some((trial, size));
                }
            }
        }
        let Some((winner, _)) = best else {
            break;
        };
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
            "function onTick()local a=input.getNumber(1)local b=input.getNumber(2)a=a*b*123 output.setNumber(1,a)output.setNumber(2,-(a))output.setNumber(3,-(a))end"
        );
    }

    #[test]
    fn groups_with_multiple_signs() {
        let source = "function onTick()local x=input.getNumber(1)local y=input.getNumber(2)local z=input.getNumber(3)output.setNumber(1,x*y*z)output.setNumber(2,-(x*y*z))output.setNumber(3,x*y*z)output.setNumber(4,-(x*y*z))end";
        assert_eq!(
            output(source, true),
            "function onTick()local x=input.getNumber(1)local y=input.getNumber(2)local z=input.getNumber(3)x=x*y*z output.setNumber(1,x)output.setNumber(2,-(x))output.setNumber(3,x)output.setNumber(4,-(x))end"
        );
    }

    #[test]
    fn skips_safe_signed() {
        let source = "function onTick()local a=input.getNumber(1)output.setNumber(1,a)output.setNumber(2,-a)end";
        assert_eq!(output(source, true), source);
    }

    #[test]
    fn does_not_group_without_profit() {
        let source = "function onTick()local a=input.getNumber(1)local b=input.getNumber(2)output.setNumber(1,a*b)output.setNumber(2,-(a*b))end";
        assert_eq!(output(source, true), source);
    }

    #[test]
    fn does_not_reuse_persistent_global_as_carrier() {
        let source = "state={index=0,best={rate=1,error=2}}function onTick()state.index=state.index+1 if state.best then output.setNumber(1,state.best.rate)output.setNumber(2,state.best.error)end end";
        let out = output(source, true);
        assert!(!out.contains("state=state.best"), "{out}");
    }

    #[test]
    fn needs_aggressive_mode() {
        let source = "function onTick()local a=input.getNumber(1)local b=input.getNumber(2)output.setNumber(1,a*b*123)output.setNumber(2,-(a*b*123))output.setNumber(3,-(a*b*123))end";
        assert_eq!(output(source, false), source);
    }
}
