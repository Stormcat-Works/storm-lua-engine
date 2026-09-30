//! Ordered/periodic screen call loop synthesis (`passes/screen-loops.ts`).

use crate::pass::PassResult;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId, TableField};
use storm_lua_syntax::numeric::{num_val, short_num};
use storm_lua_syntax::size::{measure_expr, measure_stmt};

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn same_expr(ast: &Ast, res: &Resolution, a: NodeId, b: NodeId) -> bool {
    super::immutable_values::expression_key(ast, res, a)
        == super::immutable_values::expression_key(ast, res, b)
}

fn screen_call(ast: &Ast, analyzer: &EffectAnalyzer<'_>, statement: NodeId) -> Option<NodeId> {
    let Node::Callstat(call) = ast.node(statement) else {
        return None;
    };
    let Node::Call(function, _, _) = ast.node(*call) else {
        return None;
    };
    let builtin = analyzer.resolve_builtin_reference(*function)?;
    (builtin.starts_with("screen.")
        && builtin != "screen.getWidth"
        && builtin != "screen.getHeight")
        .then_some(*call)
}

fn add_term(ast: &mut Ast, base: Option<NodeId>, coefficient: f64, term: NodeId) -> Option<NodeId> {
    if coefficient == 0.0 {
        return base;
    }
    let magnitude = coefficient.abs();
    let scaled = if magnitude == 1.0 {
        term
    } else {
        let number = ast.push(Node::Num(short_num(magnitude).into()));
        ast.push(Node::Bin("*".to_string(), term, number))
    };
    match base {
        None if coefficient > 0.0 => Some(scaled),
        None => Some(ast.push(Node::Un("-".to_string(), scaled))),
        Some(base) => Some(ast.push(Node::Bin(
            if coefficient > 0.0 { "+" } else { "-" }.to_string(),
            base,
            scaled,
        ))),
    }
}

fn affine_expression(
    source: &Ast,
    target: &mut Ast,
    values: &[NodeId],
    index: NodeId,
) -> Option<NodeId> {
    if values.len() < 2
        || !values
            .iter()
            .all(|node| matches!(source.node(*node), Node::Num(_)))
    {
        return None;
    }
    let numbers = values
        .iter()
        .map(|node| match source.node(*node) {
            Node::Num(value) => num_val(value),
            _ => unreachable!(),
        })
        .collect::<Vec<_>>();
    let start = numbers[0];
    let step = numbers[1] - start;
    if !numbers
        .iter()
        .enumerate()
        .all(|(at, value)| *value == start + step * at as f64)
    {
        return None;
    }
    if step == 0.0 {
        return Some(values[0]);
    }
    let mut term = if step == 1.0 {
        index
    } else {
        let number = target.push(Node::Num(short_num(step).into()));
        target.push(Node::Bin("*".to_string(), index, number))
    };
    if start != 0.0 {
        let number = target.push(Node::Num(short_num(start.abs()).into()));
        term = target.push(Node::Bin(
            if start > 0.0 { "+" } else { "-" }.to_string(),
            term,
            number,
        ));
    }
    Some(term)
}

// For bounded integer samples, the two kinds of adjacent difference uniquely
// determine a periodic affine sequence. Reject an impossible fit in O(n),
// rather than trying O(n^2) pairs and rescanning the whole sequence for each.
// The first nondegenerate pair in the legacy search has determinant +1/-1;
// these integer coefficients are therefore exactly the same as that pair.
fn integer_periodic_possible(numbers: &[f64], period: usize, phase: usize) -> bool {
    let wrap = period - phase;
    let step = if wrap == 1 {
        numbers[2] - numbers[1]
    } else {
        numbers[1] - numbers[0]
    };
    let jump = numbers[wrap] - numbers[wrap - 1] - step;
    numbers.iter().enumerate().all(|(at, &value)| {
        value == numbers[0] + step * at as f64 + jump * ((at + phase) / period) as f64
    })
}

fn periodic_affine_expression(
    source: &Ast,
    target: &mut Ast,
    values: &[NodeId],
    index_symbol: SymbolId,
) -> Option<NodeId> {
    if values.len() < 4
        || !values
            .iter()
            .all(|node| matches!(source.node(*node), Node::Num(_)))
    {
        return None;
    }
    let numbers = values
        .iter()
        .map(|node| match source.node(*node) {
            Node::Num(value) => num_val(value),
            _ => unreachable!(),
        })
        .collect::<Vec<_>>();
    // Products remain below 2^53 throughout this finite domain, so the
    // rejection proof never relies on approximate floating-point fitting.
    let bounded_integers = numbers.len() <= 4096
        && numbers
            .iter()
            .all(|v| v.is_finite() && v.abs() <= 1_000_000_000. && v.fract() == 0.);
    let mut shortest: Option<(NodeId, usize)> = None;
    for period in 2..=8usize.min(values.len() - 1) {
        for phase in 0..period {
            if bounded_integers && !integer_periodic_possible(&numbers, period, phase) {
                continue;
            }
            let quotient = (0..numbers.len())
                .map(|at| ((at + phase) / period) as f64)
                .collect::<Vec<_>>();
            let q0 = quotient[0];
            'left: for left in 1..numbers.len() - 1 {
                for right in left + 1..numbers.len() {
                    let ql = quotient[left] - q0;
                    let qr = quotient[right] - q0;
                    let determinant = left as f64 * qr - right as f64 * ql;
                    if determinant == 0.0 {
                        continue;
                    }
                    let yl = numbers[left] - numbers[0];
                    let yr = numbers[right] - numbers[0];
                    let step = (yl * qr - yr * ql) / determinant;
                    let jump = (left as f64 * yr - right as f64 * yl) / determinant;
                    let start = numbers[0] - jump * q0;
                    if !start.is_finite()
                        || !step.is_finite()
                        || !jump.is_finite()
                        || !numbers.iter().enumerate().all(|(at, value)| {
                            *value == start + step * at as f64 + jump * quotient[at]
                        })
                    {
                        continue;
                    }
                    let index = target.push(Node::Name(index_symbol));
                    target.nodes.mark_synthetic(index, "screen-loop-index");
                    let mut expression = add_term(target, None, step, index);
                    let shifted_index = target.push(Node::Name(index_symbol));
                    let shifted = if phase == 0 {
                        shifted_index
                    } else {
                        let phase_node = target.push(Node::Num(short_num(phase as f64).into()));
                        target.push(Node::Bin("+".to_string(), shifted_index, phase_node))
                    };
                    let period_node = target.push(Node::Num(short_num(period as f64).into()));
                    let divided = target.push(Node::Bin("//".to_string(), shifted, period_node));
                    expression = add_term(target, expression, jump, divided);
                    if start != 0.0 {
                        let start_node = target.push(Node::Num(short_num(start.abs()).into()));
                        expression = add_term(
                            target,
                            expression,
                            if start > 0.0 { 1.0 } else { -1.0 },
                            start_node,
                        );
                    }
                    if let Some(expression) = expression {
                        let size = measure_expr(target, expression);
                        if shortest.as_ref().is_none_or(|(_, best)| size < *best) {
                            shortest = Some((expression, size));
                        }
                    }
                    break 'left;
                }
            }
        }
    }
    shortest.map(|(node, _)| node)
}

struct Template {
    template: NodeId,
    leaves: Vec<NodeId>,
}

fn fallback_template(
    source: &Ast,
    target: &mut Ast,
    replacement: NodeId,
    values: &[NodeId],
) -> Template {
    derive_samples(
        target,
        replacement,
        source,
        values,
        "screen-value-table-lookup",
    );
    Template {
        template: replacement,
        leaves: values.to_vec(),
    }
}

fn translated_template(
    source: &Ast,
    target: &mut Ast,
    res: &Resolution,
    values: &[NodeId],
    replacement: NodeId,
    affine_symbol: Option<SymbolId>,
    periodic: bool,
) -> Option<Template> {
    if values.is_empty() {
        return None;
    }
    if values
        .iter()
        .all(|value| same_expr(source, res, *value, values[0]))
    {
        return Some(Template {
            template: values[0],
            leaves: Vec::new(),
        });
    }
    if let Some(symbol) = affine_symbol {
        let index = target.push(Node::Name(symbol));
        target.nodes.mark_synthetic(index, "screen-loop-index");
        if let Some(progression) = affine(source, target, values, index).or_else(|| {
            periodic
                .then(|| periodic_affine(source, target, values, symbol))
                .flatten()
        }) {
            return Some(Template {
                template: progression,
                leaves: Vec::new(),
            });
        }
    }
    let kind = std::mem::discriminant(source.node(values[0]));
    if !values
        .iter()
        .all(|value| std::mem::discriminant(source.node(*value)) == kind)
    {
        return Some(fallback_template(source, target, replacement, values));
    }

    let first = source.node(values[0]).clone();
    // Arrays in the TS object representation (call args/table fields/function params)
    // force a whole-expression value table fallback as soon as the expressions differ.
    match first {
        Node::Call(..) | Node::Table(..) | Node::Function(..) => {
            return Some(fallback_template(source, target, replacement, values));
        }
        _ => {}
    }

    let mut changing: Option<Vec<NodeId>> = None;
    let mut merge_child = |parts: Vec<NodeId>| -> Option<NodeId> {
        let nested = translated_template(
            source,
            target,
            res,
            &parts,
            replacement,
            affine_symbol,
            periodic,
        )?;
        if !nested.leaves.is_empty() {
            if changing.is_some() {
                return None;
            }
            changing = Some(nested.leaves);
        }
        Some(nested.template)
    };

    let template = match first {
        Node::Paren(_) => {
            let parts = values
                .iter()
                .map(|value| match source.node(*value) {
                    Node::Paren(inner) => *inner,
                    _ => unreachable!(),
                })
                .collect();
            let inner = match merge_child(parts) {
                Some(inner) => inner,
                None => return Some(fallback_template(source, target, replacement, values)),
            };
            target.push(Node::Paren(inner))
        }
        Node::Un(operator, _) => {
            if !values
                .iter()
                .all(|value| matches!(source.node(*value), Node::Un(op, _) if *op == operator))
            {
                return Some(fallback_template(source, target, replacement, values));
            }
            let parts = values
                .iter()
                .map(|value| match source.node(*value) {
                    Node::Un(_, inner) => *inner,
                    _ => unreachable!(),
                })
                .collect();
            let inner = match merge_child(parts) {
                Some(inner) => inner,
                None => return Some(fallback_template(source, target, replacement, values)),
            };
            target.push(Node::Un(operator, inner))
        }
        Node::Bin(operator, _, _) => {
            if !values
                .iter()
                .all(|value| matches!(source.node(*value), Node::Bin(op, _, _) if *op == operator))
            {
                return Some(fallback_template(source, target, replacement, values));
            }
            let lefts = values
                .iter()
                .map(|value| match source.node(*value) {
                    Node::Bin(_, left, _) => *left,
                    _ => unreachable!(),
                })
                .collect();
            let left = match merge_child(lefts) {
                Some(value) => value,
                None => return Some(fallback_template(source, target, replacement, values)),
            };
            let rights = values
                .iter()
                .map(|value| match source.node(*value) {
                    Node::Bin(_, _, right) => *right,
                    _ => unreachable!(),
                })
                .collect();
            let right = match merge_child(rights) {
                Some(value) => value,
                None => return Some(fallback_template(source, target, replacement, values)),
            };
            target.push(Node::Bin(operator, left, right))
        }
        Node::Index(_, _, dot) => {
            if !values.iter().all(|value| matches!(source.node(*value), Node::Index(_, _, other_dot) if *other_dot == dot)) {
                return Some(fallback_template(source, target, replacement, values));
            }
            let objects = values
                .iter()
                .map(|value| match source.node(*value) {
                    Node::Index(object, _, _) => *object,
                    _ => unreachable!(),
                })
                .collect();
            let object = match merge_child(objects) {
                Some(value) => value,
                None => return Some(fallback_template(source, target, replacement, values)),
            };
            let keys = values
                .iter()
                .map(|value| match source.node(*value) {
                    Node::Index(_, key, _) => *key,
                    _ => unreachable!(),
                })
                .collect();
            let key = match merge_child(keys) {
                Some(value) => value,
                None => return Some(fallback_template(source, target, replacement, values)),
            };
            target.push(Node::Index(object, key, dot))
        }
        Node::Methodname(_, name) => {
            if !values.iter().all(
                |value| matches!(source.node(*value), Node::Methodname(_, other) if *other == name),
            ) {
                return Some(fallback_template(source, target, replacement, values));
            }
            let objects = values
                .iter()
                .map(|value| match source.node(*value) {
                    Node::Methodname(object, _) => *object,
                    _ => unreachable!(),
                })
                .collect();
            let object = match merge_child(objects) {
                Some(value) => value,
                None => return Some(fallback_template(source, target, replacement, values)),
            };
            target.push(Node::Methodname(object, name))
        }
        // If the differing expressions are scalar/atom-like, the scalar value itself changes.
        Node::Name(_) | Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil | Node::Vararg => {
            return Some(fallback_template(source, target, replacement, values));
        }
        _ => return Some(fallback_template(source, target, replacement, values)),
    };
    derive_samples(target, template, source, values, "screen-argument-template");
    Some(Template {
        template,
        leaves: changing.unwrap_or_default(),
    })
}

fn normalize_cost(
    target: &Ast,
    calls: &[NodeId],
    assignment: Option<NodeId>,
    loop_node: NodeId,
    table_symbol: SymbolId,
    index_symbol: SymbolId,
) -> (usize, usize) {
    let mut cost = clone_ast(target);
    let f_symbol = cost.strings.intern("f");
    let a_symbol = cost.strings.intern("a");
    let b_symbol = cost.strings.intern("b");

    let mut old = 0usize;
    for call in calls {
        let Node::Call(_, args, method) = cost.node(*call).clone() else {
            continue;
        };
        let f = cost.push(Node::Name(f_symbol));
        let c = cost.push(Node::Call(f, args, method));
        let statement = cost.push(Node::Callstat(c));
        old += measure_stmt(&cost, statement);
    }
    // Synthetic names are unique per accepted run, so global replacement is safe in this clone.
    for node in &mut cost.nodes {
        match node {
            Node::Name(symbol) if *symbol == table_symbol => *symbol = a_symbol,
            Node::Name(symbol) if *symbol == index_symbol => *symbol = b_symbol,
            Node::Fornum(symbol, _, _, _, _) if *symbol == index_symbol => *symbol = b_symbol,
            _ => {}
        }
    }
    if let Some(assignment) = assignment {
        if let Node::Assign(targets, _) = cost.node(assignment).clone() {
            for target_node in targets {
                if let Node::Name(_) = cost.node(target_node) {
                    cost.nodes[target_node as usize] = Node::Name(a_symbol);
                }
            }
        }
    }
    if let Node::Fornum(_, _, _, _, body) = cost.node(loop_node).clone() {
        let Node::Block(statements) = cost.node(body).clone() else {
            unreachable!()
        };
        if let Some(statement) = statements.first() {
            if let Node::Callstat(call) = cost.node(*statement).clone() {
                if let Node::Call(_, args, method) = cost.node(call).clone() {
                    let f = cost.push(Node::Name(f_symbol));
                    cost.nodes[call as usize] = Node::Call(f, args, method);
                }
            }
        }
    }
    let new = assignment
        .map(|node| measure_stmt(&cost, node))
        .unwrap_or(0)
        + measure_stmt(&cost, loop_node);
    (old, new)
}

struct BuiltRun {
    statements: Vec<NodeId>,
    assignment: Option<NodeId>,
    loop_node: NodeId,
    table_symbol: SymbolId,
    index_symbol: SymbolId,
}

fn build_candidate(
    source: &Ast,
    target: &mut Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    calls: &[NodeId],
    serial: usize,
    periodic: bool,
) -> Option<BuiltRun> {
    if calls.len() < 4 {
        return None;
    }
    let first_args = match source.node(calls[0]) {
        Node::Call(_, args, _) => args,
        _ => return None,
    };
    if calls.iter().any(|call| match source.node(*call) {
        Node::Call(_, args, _) => args.len() != first_args.len(),
        _ => true,
    }) {
        return None;
    }
    for call in calls {
        let Node::Call(_, args, _) = source.node(*call) else {
            return None;
        };
        for argument in args {
            let effect = analyzer.effects_for_expr(*argument);
            if effect.calls || effect.ordered || !effect.writes.is_empty() || !effect.stable {
                return None;
            }
        }
    }
    let index_symbol = target
        .strings
        .intern(&format!("__stormmin_screen_index_{serial}"));
    let table_symbol = target
        .strings
        .intern(&format!("__stormmin_screen_values_{serial}"));
    let mut table_values: Option<Vec<NodeId>> = None;
    let mut varying = 0usize;
    let mut args = Vec::new();
    for argument_index in 0..first_args.len() {
        let values = calls
            .iter()
            .map(|call| match source.node(*call) {
                Node::Call(_, args, _) => args[argument_index],
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        let index = target.push(Node::Name(index_symbol));
        target.nodes.mark_synthetic(index, "screen-loop-index");
        if let Some(progression) = affine(source, target, &values, index).or_else(|| {
            periodic
                .then(|| periodic_affine(source, target, &values, index_symbol))
                .flatten()
        }) {
            if !values
                .iter()
                .all(|value| same_expr(source, res, *value, values[0]))
            {
                varying += 1;
            }
            args.push(progression);
            continue;
        }
        let table_name = target.push(Node::Name(table_symbol));
        let index_name = target.push(Node::Name(index_symbol));
        let one = target.push(Node::Num("1".to_string().into()));
        let plus_one = target.push(Node::Bin("+".to_string(), index_name, one));
        let table_index = target.push(Node::Index(table_name, plus_one, false));
        for node in [table_name, index_name, one, plus_one] {
            target
                .nodes
                .mark_synthetic(node, "screen-value-table-address");
        }
        let template = translated_template(
            source,
            target,
            res,
            &values,
            table_index,
            Some(index_symbol),
            periodic,
        )?;
        if !template.leaves.is_empty() {
            if table_values.is_some() {
                return None;
            }
            table_values = Some(template.leaves);
        }
        varying += 1;
        args.push(template.template);
    }
    if varying == 0 {
        return None;
    }
    if let Some(values) = &table_values {
        if !values.iter().all(|value| analyzer.is_movable(*value)) {
            return None;
        }
    }
    let assignment = if let Some(values) = table_values {
        let target_name = target.push(Node::Name(table_symbol));
        let fields = values.into_iter().map(TableField::Arr).collect();
        let table = target.push(Node::Table(fields));
        let assignment = target.push(Node::Assign(vec![target_name], vec![table]));
        // Only the storage shell is inserted. Original table elements retain their own origins.
        for node in [target_name, table, assignment] {
            target
                .nodes
                .mark_synthetic(node, "screen-value-table-storage");
        }
        Some(assignment)
    } else {
        None
    };
    let Node::Call(first_fn, _, method) = source.node(calls[0]).clone() else {
        return None;
    };
    let body_call = target.push(Node::Call(first_fn, args, method));
    let body_statement = target.push(Node::Callstat(body_call));
    let body = target.push(Node::Block(vec![body_statement]));
    let start = target.push(Node::Num("0".to_string().into()));
    let end = target.push(Node::Num(short_num((calls.len() - 1) as f64).into()));
    let loop_node = target.push(Node::Fornum(index_symbol, start, end, None, body));
    derive_samples(
        target,
        body_call,
        source,
        calls,
        "screen-loop-replayed-call",
    );
    derive_samples(
        target,
        body_statement,
        source,
        calls,
        "screen-loop-replayed-call",
    );
    for node in [body, start, end, loop_node] {
        target.nodes.mark_synthetic(node, "screen-loop-control");
    }
    let (old_cost, new_cost) = normalize_cost(
        target,
        calls,
        assignment,
        loop_node,
        table_symbol,
        index_symbol,
    );
    if new_cost >= old_cost {
        return None;
    }
    let mut statements = Vec::new();
    if let Some(assignment) = assignment {
        statements.push(assignment);
    }
    statements.push(loop_node);
    Some(BuiltRun {
        statements,
        assignment,
        loop_node,
        table_symbol,
        index_symbol,
    })
}

#[allow(clippy::too_many_arguments)]
fn transform_node(
    source: &Ast,
    target: &mut Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    periodic: bool,
    serial: &mut usize,
    synthesized: &mut usize,
    considered: &mut usize,
) {
    // Bottom-up: process nested blocks before this block, matching transformNode/transformBlock.
    let mut children = Vec::new();
    storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| children.push(child));
    for child in children {
        transform_node(
            source,
            target,
            res,
            analyzer,
            child,
            periodic,
            serial,
            synthesized,
            considered,
        );
    }
    let Node::Block(statements) = source.node(node).clone() else {
        return;
    };
    let mut output = Vec::new();
    let mut index = 0usize;
    while index < statements.len() {
        let Some(first) = screen_call(source, analyzer, statements[index]) else {
            output.push(statements[index]);
            index += 1;
            continue;
        };
        let first_builtin = match source.node(first) {
            Node::Call(function, _, _) => analyzer.resolve_builtin_reference(*function),
            _ => None,
        };
        let mut calls = Vec::new();
        let mut end = index;
        while end < statements.len() {
            let Some(call) = screen_call(source, analyzer, statements[end]) else {
                break;
            };
            let builtin = match source.node(call) {
                Node::Call(function, _, _) => analyzer.resolve_builtin_reference(*function),
                _ => None,
            };
            if builtin != first_builtin {
                break;
            }
            calls.push(call);
            end += 1;
        }
        if calls.len() >= 4 {
            *considered += 1;
        }
        let mut cursor = index;
        while cursor < end {
            let mut accepted: Option<(BuiltRun, usize)> = None;
            let mut stop = end;
            while stop >= cursor + 4 {
                let slice = &calls[cursor - index..stop - index];
                if let Some(built) =
                    build_candidate(source, target, res, analyzer, slice, *serial, periodic)
                {
                    accepted = Some((built, stop));
                    break;
                }
                stop -= 1;
            }
            if let Some((built, candidate_end)) = accepted {
                let _ = (
                    built.assignment,
                    built.loop_node,
                    built.table_symbol,
                    built.index_symbol,
                );
                output.extend(built.statements);
                *serial += 1;
                *synthesized += 1;
                cursor = candidate_end;
            } else {
                output.push(statements[cursor]);
                cursor += 1;
            }
        }
        index = end;
    }
    target
        .nodes
        .rewrite(node, Node::Block(output), "screen-loop-block-rewrite");
}

pub fn synthesize_screen_loops(ast: &mut Ast, root: NodeId, periodic: bool) -> PassResult {
    let source = clone_ast(ast);
    let res = resolve(&source, root);
    let analyzer = EffectAnalyzer::new(&source, &res, root, true);
    let mut target = clone_ast(&source);
    let mut serial = 0usize;
    let mut synthesized = 0usize;
    let mut considered = 0usize;
    transform_node(
        &source,
        &mut target,
        &res,
        &analyzer,
        root,
        periodic,
        &mut serial,
        &mut synthesized,
        &mut considered,
    );
    *ast = target;
    PassResult {
        root,
        saved: None,
        details: Some(vec![format!(
            "synthesized={synthesized};considered={considered}"
        )]),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use storm_lua_syntax::parser::parse_source;
    use storm_lua_syntax::print::Printer;

    #[test]
    fn ordered_screen_calls_become_loop() {
        let source = "function onDraw()screen.drawText(0,0,\"abcdefghijk\")screen.drawText(0,6,\"abcdefghijk\")screen.drawText(0,12,\"abcdefghijk\")screen.drawText(0,18,\"abcdefghijk\")screen.drawText(0,24,\"abcdefghijk\")screen.drawText(0,30,\"abcdefghijk\")end";
        let (mut ast, root) = parse_source(source).unwrap();
        synthesize_screen_loops(&mut ast, root, false);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("for "), "{out}");
    }

    #[test]
    fn periodic_coordinates_can_be_synthesized() {
        let source = "function onDraw()screen.drawText(0,0,input.getNumber(1))screen.drawText(0,6,input.getNumber(2))screen.drawText(0,18,input.getNumber(3))screen.drawText(0,24,input.getNumber(4))screen.drawText(0,30,input.getNumber(5))screen.drawText(0,42,input.getNumber(6))screen.drawText(0,48,input.getNumber(7))screen.drawText(0,54,input.getNumber(8))end";
        let (mut ast, root) = parse_source(source).unwrap();
        synthesize_screen_loops(&mut ast, root, true);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("for "), "{out}");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod integer_periodic_tests {
    use super::*;

    fn brute_possible(numbers: &[f64], period: usize, phase: usize) -> bool {
        let q = (0..numbers.len())
            .map(|i| ((i + phase) / period) as f64)
            .collect::<Vec<_>>();
        for left in 1..numbers.len() - 1 {
            for right in left + 1..numbers.len() {
                let determinant = left as f64 * q[right] - right as f64 * q[left];
                if determinant == 0. {
                    continue;
                }
                let yl = numbers[left] - numbers[0];
                let yr = numbers[right] - numbers[0];
                let step = (yl * q[right] - yr * q[left]) / determinant;
                let jump = (left as f64 * yr - right as f64 * yl) / determinant;
                if numbers
                    .iter()
                    .enumerate()
                    .all(|(i, &v)| v == numbers[0] + step * i as f64 + jump * q[i])
                {
                    return true;
                }
            }
        }
        false
    }
    #[test]
    fn integer_rejection_matches_the_legacy_pair_search() {
        let mut seed = 0x1234abcu32;
        for n in 4..=24 {
            for _ in 0..24 {
                let samples = (0..n)
                    .map(|_| {
                        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                        ((seed >> 24) as i64 - 128) as f64
                    })
                    .collect::<Vec<_>>();
                for period in 2..=8usize.min(n - 1) {
                    for phase in 0..period {
                        assert_eq!(
                            integer_periodic_possible(&samples, period, phase),
                            brute_possible(&samples, period, phase)
                        );
                        let valid = (0..n)
                            .map(|i| -99. + i as f64 * 7. - ((i + phase) / period) as f64 * 31.)
                            .collect::<Vec<_>>();
                        assert!(integer_periodic_possible(&valid, period, phase));
                        assert!(brute_possible(&valid, period, phase));
                    }
                }
            }
        }
    }
    #[test]
    fn long_integer_sequences_keep_phase_edges_and_reject_one_outlier() {
        for phase in 0..8 {
            let mut samples = (0..4096)
                .map(|i| 1_000_000_000. - i as f64 * 123. - ((i + phase) / 8) as f64 * 77.)
                .collect::<Vec<_>>();
            assert!(integer_periodic_possible(&samples, 8, phase));
            samples[4095] += 1.;
            assert!(!integer_periodic_possible(&samples, 8, phase));
        }
    }
}

fn derive_samples(target: &mut Ast, node: NodeId, source: &Ast, values: &[NodeId], reason: &str) {
    if !target.nodes.tracks_origins() {
        return;
    }
    if values.is_empty() || values.iter().any(|&id| source.nodes.origin(id).is_none()) {
        let syntax = target.node(node).clone();
        target.nodes[node as usize] = syntax;
        return;
    }
    target
        .nodes
        .derive_from(node, &source.nodes, values[0], reason);
    for &original in &values[1..] {
        target
            .nodes
            .relate_from(node, &source.nodes, original, reason);
    }
}

fn affine(source: &Ast, target: &mut Ast, values: &[NodeId], index: NodeId) -> Option<NodeId> {
    let first = target.nodes.len();
    let result = affine_expression(source, target, values, index)?;
    // A unit progression can reduce to the index alone. That read is the
    // representation of this original column, not only loop-control syntax.
    if result == index {
        derive_samples(target, result, source, values, "screen-affine-arguments");
    }
    if target.nodes.tracks_origins() {
        for node in first..target.nodes.len() {
            derive_samples(
                target,
                node as NodeId,
                source,
                values,
                "screen-affine-arguments",
            );
        }
    }
    Some(result)
}

fn periodic_affine(
    source: &Ast,
    target: &mut Ast,
    values: &[NodeId],
    symbol: SymbolId,
) -> Option<NodeId> {
    let first = target.nodes.len();
    let result = periodic_affine_expression(source, target, values, symbol)?;
    if target.nodes.tracks_origins() {
        for node in first..target.nodes.len() {
            if matches!(target.node(node as NodeId), Node::Name(_)) {
                target
                    .nodes
                    .mark_synthetic(node as NodeId, "screen-loop-index");
            } else {
                derive_samples(
                    target,
                    node as NodeId,
                    source,
                    values,
                    "screen-periodic-arguments",
                );
            }
        }
    }
    Some(result)
}
