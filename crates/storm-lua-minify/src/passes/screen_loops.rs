//! Ordered/periodic screen call loop synthesis (`passes/screen-loops.ts`).

use std::collections::HashMap;

use crate::pass::PassResult;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId, TableField};
use storm_lua_syntax::numeric::{integer_literal_value, num_val, short_num};
use storm_lua_syntax::size::measure_expr;
#[cfg(test)]
use storm_lua_syntax::size::measure_stmt;

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

// Shared numeric extraction for the affine and periodic fits: both require
// an all-Num column with no integer literal above 2^53, so a column that
// fails extraction fails both fits. build_candidate used to extract the same
// column twice per argument attempt (once per fit, each re-parsing every
// literal); extracting once and sharing it removes that duplication.
fn numeric_column(source: &Ast, values: &[NodeId]) -> Option<Vec<f64>> {
    // Single pass: the old version scanned the column three times (all-Num
    // check, integer-range check, then num_val, which parses the integer
    // literal a second time). Parse each literal once: num_val returns the
    // integer value as f64 whenever integer_literal_value is Some, so
    // pushing `int as f64` in that case and num_val otherwise yields
    // exactly the old vector, with the same None conditions (any non-Num
    // node, or any integer literal with abs > 2^53).
    let mut numbers = Vec::with_capacity(values.len());
    for node in values {
        let Node::Num(value) = source.node(*node) else {
            return None;
        };
        if let Some(int) = integer_literal_value(value) {
            // Integer equality must be established before conversion to f64.
            if int.unsigned_abs() > (1u64 << 53) {
                return None;
            }
            numbers.push(int as f64);
        } else {
            numbers.push(num_val(value));
        }
    }
    Some(numbers)
}

fn affine_expression(
    source: &Ast,
    target: &mut Ast,
    values: &[NodeId],
    index: NodeId,
) -> Option<NodeId> {
    if values.len() < 2 {
        return None;
    }
    let numbers = numeric_column(source, values)?;
    affine_numbers(&numbers, target, index, values[0])
}

/// Build the affine term for a column already known to fit: the term
/// depends only on the first two numbers (start/step), so callers that
/// proved fit another way (the cursor's mismatch position) can build it
/// without rescanning the column. The pushed nodes are exactly the ones
/// affine_numbers pushes on success.
fn affine_term(numbers: &[f64], target: &mut Ast, index: NodeId, first: NodeId) -> NodeId {
    let start = numbers[0];
    let step = numbers[1] - start;
    if step == 0.0 {
        return first;
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
    term
}

fn affine_numbers(
    numbers: &[f64],
    target: &mut Ast,
    index: NodeId,
    first: NodeId,
) -> Option<NodeId> {
    if numbers.len() < 2 {
        return None;
    }
    let start = numbers[0];
    let step = numbers[1] - start;
    if !numbers
        .iter()
        .enumerate()
        .all(|(at, value)| *value == start + step * at as f64)
    {
        return None;
    }
    Some(affine_term(numbers, target, index, first))
}

// For bounded integer samples, the two kinds of adjacent difference uniquely
// determine a periodic affine sequence. Reject an impossible fit in O(n),
// rather than trying O(n^2) pairs and rescanning the whole sequence for each.
// The first nondegenerate pair in the legacy search has determinant +1/-1;
// these integer coefficients are therefore exactly the same as that pair.
/// First index where the (period, phase) precheck fails on these numbers,
/// or None when the whole column passes it. The scan is identical to
/// integer_periodic_possible's, only the failure position is reported:
/// a prefix passes iff it ends at or before that position, because the
/// step/jump derivation uses only indices below wrap and the boundary
/// sequence depends only on (wrap, period), never on the column length.
fn integer_periodic_first_failure(numbers: &[f64], period: usize, phase: usize) -> Option<usize> {
    let wrap = period - phase;
    let step = if wrap == 1 {
        numbers[2] - numbers[1]
    } else {
        numbers[1] - numbers[0]
    };
    let jump = numbers[wrap] - numbers[wrap - 1] - step;
    // Exactly equivalent to the original formula check, for the bounded-integer
    // domain this precheck serves (all values are integers within 2^53, so
    // every step/jump/difference is exact in f64): the formula's quotient
    // increments exactly at at = wrap, wrap + period, wrap + 2 * period, ...,
    // so the formula holds iff every adjacent difference equals step, except
    // at those boundaries where it equals step + jump. Checking differences
    // avoids one integer division and two f64 multiplications per element,
    // which dominated long screen-call runs that never synthesize a loop
    // (every (period, phase) pair rescanned each attempted slice).
    let mut next_boundary = wrap;
    for at in 1..numbers.len() {
        let expected = if at == next_boundary {
            next_boundary += period;
            step + jump
        } else {
            step
        };
        if numbers[at] - numbers[at - 1] != expected {
            return Some(at);
        }
    }
    None
}

fn integer_periodic_possible(numbers: &[f64], period: usize, phase: usize) -> bool {
    integer_periodic_first_failure(numbers, period, phase).is_none()
}

fn periodic_affine_expression(
    source: &Ast,
    target: &mut Ast,
    values: &[NodeId],
    index_symbol: SymbolId,
    plan: &mut TrialOrigins,
) -> Option<NodeId> {
    if values.len() < 4 {
        return None;
    }
    let numbers = numeric_column(source, values)?;
    periodic_affine_numbers(&numbers, target, index_symbol, None, plan)
}

fn periodic_affine_numbers(
    numbers: &[f64],
    target: &mut Ast,
    index_symbol: SymbolId,
    pre: Option<&PeriodicPre>,
    plan: &mut TrialOrigins,
) -> Option<NodeId> {
    if numbers.len() < 4 {
        return None;
    }
    // Products remain below 2^53 throughout this finite domain, so the
    // rejection proof never relies on approximate floating-point fitting.
    // A shared table means the cursor's longest column was bounded, so
    // every prefix is bounded too; otherwise check this slice directly.
    let bounded_integers = pre.is_some()
        || (numbers.len() <= 4096
            && numbers
                .iter()
                .all(|v| v.is_finite() && v.abs() <= 1_000_000_000. && v.fract() == 0.));
    let mut shortest: Option<(NodeId, usize)> = None;
    for period in 2..=8usize.min(numbers.len() - 1) {
        for phase in 0..period {
            match pre.and_then(|pre| pre.get(period, phase)) {
                // Shared precheck: the cursor's longest column already
                // decided this pair; a prefix passes iff it ends at or
                // before the first failure position.
                Some(failure) => {
                    if failure.is_some_and(|at| numbers.len() > at) {
                        continue;
                    }
                }
                // No shared table (or pair outside the precomputed
                // range): the per-slice precheck as before.
                None => {
                    if bounded_integers && !integer_periodic_possible(numbers, period, phase) {
                        continue;
                    }
                }
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
                    plan.synthetic(target, index, "screen-loop-index");
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
    plan: &mut TrialOrigins,
) -> Template {
    plan.derive(
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

#[allow(clippy::too_many_arguments)]
fn translated_template(
    source: &Ast,
    target: &mut Ast,
    res: &Resolution,
    values: &[NodeId],
    replacement: NodeId,
    affine_symbol: Option<SymbolId>,
    periodic: bool,
    affine_done: bool,
    plan: &mut TrialOrigins,
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
        plan.synthetic(target, index, "screen-loop-index");
        // When the caller already tried affine/periodic for these exact
        // values and failed (build_candidate), retrying here would return
        // None again: both checks are pure in (values, symbol). Skipping
        // the retry only avoids rescanning the slice; the Name push above
        // is kept so the target arena is unchanged.
        if !affine_done {
            if let Some(progression) =
                affine(source, target, values, index, None, plan).or_else(|| {
                    periodic
                        .then(|| periodic_affine(source, target, values, symbol, None, None, plan))
                        .flatten()
                })
            {
                return Some(Template {
                    template: progression,
                    leaves: Vec::new(),
                });
            }
        }
    }
    let kind = std::mem::discriminant(source.node(values[0]));
    if !values
        .iter()
        .all(|value| std::mem::discriminant(source.node(*value)) == kind)
    {
        return Some(fallback_template(source, target, replacement, values, plan));
    }

    let first = source.node(values[0]).clone();
    // Arrays in the TS object representation (call args/table fields/function params)
    // force a whole-expression value table fallback as soon as the expressions differ.
    match first {
        Node::Call(..) | Node::Table(..) | Node::Function(..) => {
            return Some(fallback_template(source, target, replacement, values, plan));
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
            false,
            plan,
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
                None => return Some(fallback_template(source, target, replacement, values, plan)),
            };
            target.push(Node::Paren(inner))
        }
        Node::Un(operator, _) => {
            if !values
                .iter()
                .all(|value| matches!(source.node(*value), Node::Un(op, _) if *op == operator))
            {
                return Some(fallback_template(source, target, replacement, values, plan));
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
                None => return Some(fallback_template(source, target, replacement, values, plan)),
            };
            target.push(Node::Un(operator, inner))
        }
        Node::Bin(operator, _, _) => {
            if !values
                .iter()
                .all(|value| matches!(source.node(*value), Node::Bin(op, _, _) if *op == operator))
            {
                return Some(fallback_template(source, target, replacement, values, plan));
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
                None => return Some(fallback_template(source, target, replacement, values, plan)),
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
                None => return Some(fallback_template(source, target, replacement, values, plan)),
            };
            target.push(Node::Bin(operator, left, right))
        }
        Node::Index(_, _, dot) => {
            if !values.iter().all(|value| matches!(source.node(*value), Node::Index(_, _, other_dot) if *other_dot == dot)) {
                return Some(fallback_template(source, target, replacement, values, plan));
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
                None => return Some(fallback_template(source, target, replacement, values, plan)),
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
                None => return Some(fallback_template(source, target, replacement, values, plan)),
            };
            target.push(Node::Index(object, key, dot))
        }
        Node::Methodname(_, name) => {
            if !values.iter().all(
                |value| matches!(source.node(*value), Node::Methodname(_, other) if *other == name),
            ) {
                return Some(fallback_template(source, target, replacement, values, plan));
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
                None => return Some(fallback_template(source, target, replacement, values, plan)),
            };
            target.push(Node::Methodname(object, name))
        }
        // If the differing expressions are scalar/atom-like, the scalar value itself changes.
        Node::Name(_) | Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil | Node::Vararg => {
            return Some(fallback_template(source, target, replacement, values, plan));
        }
        _ => return Some(fallback_template(source, target, replacement, values, plan)),
    };
    plan.derive(target, template, source, values, "screen-argument-template");
    Some(Template {
        template,
        leaves: changing.unwrap_or_default(),
    })
}

// The cost model changes only callee/name spellings. Borrow the existing
// tree instead of cloning every node and origin just to measure two small roots.
struct LoopCostNames<'a> {
    ast: &'a Ast,
    callee: Option<NodeId>,
    symbols: Option<(SymbolId, SymbolId)>,
    assignment: Option<NodeId>,
}
impl storm_lua_syntax::size::SizeNameResolver for LoopCostNames<'_> {
    fn replacement(&self, node: NodeId, site: storm_lua_syntax::NameSite) -> Option<&str> {
        let (table, index) = self.symbols?;
        match (self.ast.node(node),site) {
            (Node::Name(symbol),storm_lua_syntax::NameSite::Reference)=>{
                if self.assignment.is_some_and(|assignment|matches!(self.ast.node(assignment),Node::Assign(targets,_) if targets.contains(&node))) || *symbol==table {Some("a")}
                else if *symbol==index {Some("b")} else {None}
            }
            (Node::Fornum(symbol,..),storm_lua_syntax::NameSite::Binding(0)) if *symbol==index=>Some("b"),
            _=>None,
        }
    }
    fn call_target(&self, call: NodeId) -> Option<&str> {
        (self.callee == Some(call)).then_some("f")
    }
}
fn normalize_cost(
    target: &Ast,
    calls: &[NodeId],
    assignment: Option<NodeId>,
    loop_node: NodeId,
    table_symbol: SymbolId,
    index_symbol: SymbolId,
) -> (usize, usize) {
    use storm_lua_syntax::size::{measure_expr_with_names, measure_stmt_with_names};
    let old = calls
        .iter()
        .filter(|&&call| matches!(target.node(call), Node::Call(..)))
        .map(|&call| {
            measure_expr_with_names(
                target,
                call,
                &LoopCostNames {
                    ast: target,
                    callee: Some(call),
                    symbols: None,
                    assignment: None,
                },
            )
        })
        .sum();
    let callee = match target.node(loop_node) {
        Node::Fornum(_, _, _, _, body) => {
            let Node::Block(statements) = target.node(*body) else {
                unreachable!()
            };
            statements
                .first()
                .and_then(|&statement| match target.node(statement) {
                    Node::Callstat(call) if matches!(target.node(*call), Node::Call(..)) => {
                        Some(*call)
                    }
                    _ => None,
                })
        }
        _ => None,
    };
    let names = LoopCostNames {
        ast: target,
        callee,
        symbols: Some((table_symbol, index_symbol)),
        assignment,
    };
    let new = assignment
        .map(|node| measure_stmt_with_names(target, node, &names))
        .unwrap_or(0)
        + measure_stmt_with_names(target, loop_node, &names);
    (old, new)
}

#[cfg(test)]
fn reference_normalize_cost(
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

// Next unused variant of a synthetic-name sequence, reproducing fresh()
// results without rescanning from the first variant on every call.
// `next` is a lower bound only: it advances past names confirmed used, and
// used names are never removed, so the smallest unused variant at or after
// `next` is exactly the one fresh() would return. Callers reset `next` to 0
// when the preferred base changes (here: when the run serial changes).
fn next_screen_symbol(target: &mut Ast, preferred: &str, next: &mut usize) -> SymbolId {
    loop {
        let candidate = if *next == 0 {
            preferred.to_string()
        } else {
            format!("{preferred}_{next}")
        };
        *next += 1;
        if !target.strings.contains(&candidate) {
            return target.strings.intern(&candidate);
        }
    }
}

/// One argument column of a screen-call run, pre-extracted for the run's
/// longest slice starting at one cursor.
///
/// Slices attempted from the same cursor are prefixes of that longest
/// slice, and `numeric_column` is a pure function of the column's nodes:
/// a prefix's numbers are the longest column's numbers truncated, unless
/// the prefix reaches the first non-numeric or out-of-range element.
/// Caching the longest column therefore answers every shorter slice
/// without re-parsing its literals. An `Unusable` entry (the longest
/// slice's calls disagree on this argument's arity) falls back to the
/// per-slice extraction, exactly as before.
/// First-failure positions of the periodic precheck for one cursor
/// column's longest slice, shared by every slice from that cursor.
/// fail[period][phase] is the first index where the longest column
/// breaks that pair's precheck, or None when it passes. Only pairs with
/// period <= max_period were computed; a slice never queries a larger
/// period (its period bound is tighter than its cursor's), so an
/// uncomputed pair is never consulted -- get() reports it as unknown
/// and the caller falls back to the per-slice precheck.
struct PeriodicPre {
    fail: [[Option<usize>; 8]; 9],
    max_period: usize,
}

impl PeriodicPre {
    /// Some(failure) when the pair was precomputed (failure None means
    /// the longest column passes it), None when unknown.
    fn get(&self, period: usize, phase: usize) -> Option<Option<usize>> {
        if period > self.max_period {
            return None;
        }
        Some(self.fail[period][phase])
    }
}

enum CursorColumn {
    Ready {
        numbers: Vec<f64>,
        bad: Option<usize>,
        // First index (>= 2) where the longest column breaks the affine
        // fit fixed by its first two elements, or None when the whole
        // stored column fits. Affine fit is prefix-monotone: a prefix
        // fits iff it ends at or before this position, so every slice
        // from this cursor can decide fit in O(1) without rescanning.
        affine_bad: Option<usize>,
        // Precomputed periodic-precheck failures for this column (None
        // when the longest column is not bounded, or when this cursor
        // is not a periodic attempt: then the per-slice precheck runs).
        periodic_pre: Option<Box<PeriodicPre>>,
    },
    Unusable,
}

fn cursor_columns(source: &Ast, longest: &[NodeId], periodic: bool) -> Vec<CursorColumn> {
    let Some(first) = longest.first() else {
        return Vec::new();
    };
    let Node::Call(_, first_args, _) = source.node(*first) else {
        return Vec::new();
    };
    (0..first_args.len())
        .map(|argument_index| {
            let mut numbers = Vec::with_capacity(longest.len());
            let mut affine_bad: Option<usize> = None;
            let mut bad: Option<usize> = None;
            for (pos, call) in longest.iter().enumerate() {
                let Node::Call(_, args, _) = source.node(*call) else {
                    return CursorColumn::Unusable;
                };
                let Some(argument) = args.get(argument_index) else {
                    return CursorColumn::Unusable;
                };
                let Node::Num(value) = source.node(*argument) else {
                    bad = Some(pos);
                    break;
                };
                let pushed = if let Some(int) = integer_literal_value(value) {
                    if int.unsigned_abs() > (1u64 << 53) {
                        bad = Some(pos);
                        break;
                    }
                    int as f64
                } else {
                    num_val(value)
                };
                // The element being pushed sits at index numbers.len();
                // start/step are fixed by the first two elements, so the
                // first mismatch found here is the mismatch position for
                // every prefix reaching it.
                if affine_bad.is_none() && numbers.len() >= 2 {
                    let start = numbers[0];
                    let step = numbers[1] - numbers[0];
                    if pushed != start + step * numbers.len() as f64 {
                        affine_bad = Some(numbers.len());
                    }
                }
                numbers.push(pushed);
            }
            // The periodic table is computed over the stored prefix (up
            // to bad, when present): every slice reaching the periodic
            // attempt uses exactly this prefix's numbers, and a bounded
            // longest column implies every prefix is bounded too.
            let periodic_pre = if periodic
                && numbers.len() <= 4096
                && numbers
                    .iter()
                    .all(|v| v.is_finite() && v.abs() <= 1_000_000_000. && v.fract() == 0.)
            {
                let max_period = 8usize.min(numbers.len().saturating_sub(1));
                let mut fail = [[None; 8]; 9];
                for (period, row) in fail.iter_mut().enumerate().skip(2) {
                    if period > max_period {
                        break;
                    }
                    for (phase, slot) in row.iter_mut().enumerate().take(period) {
                        *slot = integer_periodic_first_failure(&numbers, period, phase);
                    }
                }
                Some(Box::new(PeriodicPre { fail, max_period }))
            } else {
                None
            };
            CursorColumn::Ready {
                numbers,
                bad,
                affine_bad,
                periodic_pre,
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn build_candidate(
    source: &Ast,
    target: &mut Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    calls: &[NodeId],
    symbols: (SymbolId, SymbolId),
    periodic: bool,
    columns: &[CursorColumn],
    plan: &mut TrialOrigins,
) -> Option<BuiltRun> {
    if calls.len() < 4 {
        return None;
    }
    // The caller (transform_node) guarantees the arity and effect
    // predicates for this slice via its prefix-sum precheck below. Both
    // predicates are pure in (analyzer, call); rescanning them here for
    // every attempted slice used to dominate long runs that never
    // synthesize a loop (O(n) slices per cursor, each rescanning O(n)
    // calls through effects_for_expr).
    let first_args = match source.node(calls[0]) {
        Node::Call(_, args, _) => args,
        _ => return None,
    };
    // The caller supplies these via next_screen_symbol, which reproduces
    // the fresh() results for this attempt without rescanning from the
    // first variant on every attempted slice (see transform_node).
    let (index_symbol, table_symbol) = symbols;
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
        plan.synthetic(target, index, "screen-loop-index");
        // Extract this argument column's numbers once and share them
        // between the affine and periodic attempts (see numeric_column).
        // A non-numeric column skips both attempts: both fits would
        // return None for it.
        //
        // Reuse the cursor's pre-extracted column when available: slices
        // from one cursor are prefixes of the longest slice, so this
        // yields exactly numeric_column(source, &values) without
        // re-parsing literals for each of the O(n) slices (see
        // CursorColumn). Otherwise fall back to the direct extraction.
        // Borrow the cursor's pre-extracted numbers when available:
        // slices from one cursor are prefixes of the longest slice, so
        // borrowing avoids copying the column for every attempted slice.
        // Otherwise fall back to the direct extraction (owned).
        let owned_numbers: Option<Vec<f64>>;
        let (numbers, affine_fit, periodic_pre) = match columns.get(argument_index) {
            Some(CursorColumn::Ready {
                numbers,
                bad,
                affine_bad,
                periodic_pre,
            }) => match bad {
                Some(pos) if values.len() > *pos => (None, None, None),
                _ => {
                    let slice = &numbers[..values.len()];
                    let fit = slice.len() >= 2
                        && affine_bad.is_none_or(|mismatch| slice.len() <= mismatch);
                    (Some(slice), Some(fit), periodic_pre.as_deref())
                }
            },
            _ => {
                owned_numbers = numeric_column(source, &values);
                (owned_numbers.as_deref(), None, None)
            }
        };
        let progression = numbers.and_then(|numbers| {
            match affine_fit {
                // Fit already proven by the cursor's mismatch position:
                // build the term directly without rescanning the column.
                Some(true) => {
                    let first = target.nodes.len();
                    let term = affine_term(numbers, target, index, values[0]);
                    Some(finish_affine(
                        target, source, &values, index, first, term, plan,
                    ))
                }
                // Proven non-fit: skip straight to the periodic attempt.
                Some(false) => periodic
                    .then(|| {
                        periodic_affine(
                            source,
                            target,
                            &values,
                            index_symbol,
                            Some(numbers),
                            periodic_pre,
                            plan,
                        )
                    })
                    .flatten(),
                // No cursor column: the old scan-and-build path.
                None => affine(source, target, &values, index, Some(numbers), plan).or_else(|| {
                    periodic
                        .then(|| {
                            periodic_affine(
                                source,
                                target,
                                &values,
                                index_symbol,
                                Some(numbers),
                                None,
                                plan,
                            )
                        })
                        .flatten()
                }),
            }
        });
        if let Some(progression) = progression {
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
            plan.synthetic(target, node, "screen-value-table-address");
        }
        // Second-table early exit (winner-invariant): translated_template
        // with affine_done=true can only return empty leaves when every
        // value is the same expression. For a scalar-first column that is
        // not constant, the template is a value-table fallback with
        // non-empty leaves, so a second such column makes build_candidate
        // return None here. Skipping the template build is observationally
        // identical: the caller rolls the target arena back on failure, and
        // the one index Name push below reproduces translated_template's
        // own push so the arena state matches even without a rollback.
        // Non-scalar columns are left to translated_template, whose
        // structural merge can still yield empty leaves via affine subparts.
        if table_values.is_some() {
            let scalar_table_conflict = match &numbers {
                Some(nums) => !nums.iter().all(|n| *n == nums[0]),
                None => {
                    let first_scalar = matches!(
                        source.node(values[0]),
                        Node::Name(_)
                            | Node::Num(_)
                            | Node::Str(_)
                            | Node::Bool(_)
                            | Node::Nil
                            | Node::Vararg
                    );
                    if !first_scalar {
                        false
                    } else {
                        let key0 = super::immutable_values::expression_key(source, res, values[0]);
                        !values.iter().all(|value| {
                            super::immutable_values::expression_key(source, res, *value) == key0
                        })
                    }
                }
            };
            if scalar_table_conflict {
                let index = target.push(Node::Name(index_symbol));
                plan.synthetic(target, index, "screen-loop-index");
                return None;
            }
        }
        let template = translated_template(
            source,
            target,
            res,
            &values,
            table_index,
            Some(index_symbol),
            periodic,
            // build_candidate already tried affine and periodic for these
            // values above and failed, so translated_template's retry
            // would return None; see the affine_done note there.
            true,
            plan,
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
            plan.synthetic(target, node, "screen-value-table-storage");
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
    plan.derive(
        target,
        body_call,
        source,
        calls,
        "screen-loop-replayed-call",
    );
    plan.derive(
        target,
        body_statement,
        source,
        calls,
        "screen-loop-replayed-call",
    );
    for node in [body, start, end, loop_node] {
        plan.synthetic(target, node, "screen-loop-control");
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
/// Conservative active-local maximum of the enclosing function for
/// every block in the chunk, keyed by block node. The chunk itself is
/// the outermost function (zero parameters); method definitions add
/// their implicit `self` parameter. The synthesized numeric-for loop
/// needs four control locals (variable + limit/step/state) at the
/// sequence's block, so synthesis is skipped when the enclosing
/// function is already at the VM's simultaneous-local limit.
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

/// Locals a synthesized numeric-for loop adds at its block: the loop
/// variable plus the three hidden control values.
const FORNUM_LOCALS: usize = 4;

#[allow(clippy::too_many_arguments)]
fn transform_node(
    source: &Ast,
    target: &mut Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    periodic: bool,
    budgets: &HashMap<NodeId, usize>,
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
            budgets,
            serial,
            synthesized,
            considered,
        );
    }
    let Node::Block(statements) = source.node(node).clone() else {
        return;
    };
    // Synthetic-name variant cursors for this block (see next_screen_symbol).
    // They are lower bounds only, so a stale cursor (e.g. after a child
    // block consumed variants for the same serial) still yields the same
    // result; only a serial change requires resetting them.
    let mut screen_serial: Option<usize> = None;
    let mut index_next = 0usize;
    let mut table_next = 0usize;
    let mut output = Vec::new();
    // Reused across attempts: clear() keeps the buffers, so recording
    // a failed trial costs no allocation once warmed up.
    let mut plan = TrialOrigins::new();
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
        // One effects pass per call in this run, shared by the precheck
        // of all attempted slices below.
        let mut arg_count_sum = 0usize;
        let mut bad_sum = 0usize;
        let mut arg_count_prefix = Vec::with_capacity(calls.len() + 1);
        let mut bad_prefix = Vec::with_capacity(calls.len() + 1);
        arg_count_prefix.push(0);
        bad_prefix.push(0);
        for &call in &calls {
            match source.node(call) {
                Node::Call(_, args, _) => {
                    let bad = args.iter().any(|&argument| {
                        let effect = analyzer.effects_for_expr(argument);
                        effect.calls
                            || effect.ordered
                            || !effect.writes.is_empty()
                            || !effect.stable
                    });
                    arg_count_sum += args.len();
                    bad_sum += usize::from(bad);
                }
                _ => {
                    bad_sum += 1;
                }
            }
            arg_count_prefix.push(arg_count_sum);
            bad_prefix.push(bad_sum);
        }
        let mut cursor = index;
        let over_budget = budgets.get(&node).copied().unwrap_or(0) + FORNUM_LOCALS
            > super::single_member_cache::MAX_LOCALS;
        if over_budget {
            output.extend_from_slice(&statements[index..end]);
        }
        while cursor < end && !over_budget {
            // Pre-extract this cursor's argument columns once for its
            // longest slice; every slice attempted from this cursor is a
            // prefix of it (see CursorColumn), so build_candidate reuses
            // these numbers instead of re-parsing literals for each slice.
            // Short tails (fewer than 4 calls) attempt no slice, so they
            // pay no pre-extraction.
            let columns = if end - cursor >= 4 {
                cursor_columns(source, &calls[cursor - index..], periodic)
            } else {
                Vec::new()
            };
            let mut accepted: Option<(BuiltRun, usize)> = None;
            let mut stop = end;
            while stop >= cursor + 4 {
                let slice = &calls[cursor - index..stop - index];
                // O(1) arity/effect precheck over the prefix sums above.
                // A failing slice is exactly what build_candidate used to
                // reject by rescanning it. The bad check runs first, so a
                // slice reaching the arity check holds only genuine Call
                // nodes and its prefix differences are real argument counts
                // (a non-Call entry is always marked bad).
                let start = cursor - index;
                let stop_rel = stop - index;
                if bad_prefix[stop_rel] - bad_prefix[start] > 0 {
                    stop -= 1;
                    continue;
                }
                let first_count = arg_count_prefix[start + 1] - arg_count_prefix[start];
                if arg_count_prefix[stop_rel] - arg_count_prefix[start]
                    != first_count * (stop_rel - start)
                {
                    stop -= 1;
                    continue;
                }
                // Consume exactly one symbol pair per attempt reaching
                // build_candidate, in attempt order, exactly as the old
                // per-attempt fresh() calls did (see next_screen_symbol).
                // Snapshot the arena length and the interner before this
                // attempt interns its symbol pair, so a failed trial can
                // roll both back: a failed trial otherwise leaves its pushed
                // nodes dead in the target arena and interns 2 fresh symbols,
                // so the arena grows ~16 nodes and the symbol table 2 entries
                // per attempt, and downstream passes pay copy-on-write copies
                // of both. Surviving nodes never reference rolled-back
                // symbols (their nodes are truncated on failure, and the
                // symbol cursors only move forward).
                let checkpoint = target.nodes.len();
                let strings_checkpoint = target.strings.clone();
                if screen_serial != Some(*serial) {
                    screen_serial = Some(*serial);
                    index_next = 0;
                    table_next = 0;
                }
                let index_symbol = next_screen_symbol(
                    target,
                    &format!("__stormmin_screen_index_{}", *serial),
                    &mut index_next,
                );
                let table_symbol = next_screen_symbol(
                    target,
                    &format!("__stormmin_screen_values_{}", *serial),
                    &mut table_next,
                );
                plan.clear();
                if let Some(built) = build_candidate(
                    source,
                    target,
                    res,
                    analyzer,
                    slice,
                    (index_symbol, table_symbol),
                    periodic,
                    &columns,
                    &mut plan,
                ) {
                    plan.apply(target, source);
                    accepted = Some((built, stop));
                    break;
                }
                // Roll back the failed trial's dead nodes and its interned
                // symbols (see the checkpoint above).
                target.nodes.truncate(checkpoint);
                target.strings = strings_checkpoint;
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
    let budgets = block_local_budgets(&source, root);
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
        &budgets,
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
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

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
mod budget_tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn run(source: &str) -> String {
        let (mut ast, root) = parse_source(source).unwrap();
        synthesize_screen_loops(&mut ast, root, false);
        Printer::new(&ast, false).output(root)
    }

    #[test]
    fn skips_synthesis_when_function_is_at_the_local_limit() {
        // 197 parameters + the synthesized loop's four control locals
        // would exceed the VM's 200 simultaneous-local limit.
        let params = (1..=197)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let calls = (0..6)
            .map(|i| format!("screen.drawText(0,{},\"abcdefghijk\")", i * 6))
            .collect::<String>();
        let source = format!("local function big({params}){calls}return p1 end");
        assert!(!run(&source).contains("for "), "{source}");
    }

    #[test]
    fn synthesizes_below_the_local_limit() {
        let params = (1..=196)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let calls = (0..6)
            .map(|i| format!("screen.drawText(0,{},\"abcdefghijk\")", i * 6))
            .collect::<String>();
        let source = format!("local function big({params}){calls}return p1 end");
        assert!(run(&source).contains("for "));
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

/// Origin work deferred from a screen-loop trial until the trial is accepted.
///
/// build_candidate pushes trial nodes into the shared target arena and
/// derives their origins as it goes. On a failed trial those nodes are
/// truncated, but the derivation allocations are already spent: on draw.lua
/// they account for nearly all of the pass tracked cost even though no
/// loop is synthesized. Recording the derivation calls here and applying
/// them only for the accepted trial keeps the accepted origins identical
/// (same calls, same order, same source) while failed trials cost nothing
/// tracked.
struct TrialOrigins {
    ops: Vec<TrialOriginOp>,
    /// Source values referenced by `Derive` ops, appended per op so a
    /// single buffer serves the whole trial instead of one allocation
    /// per recorded call.
    values: Vec<NodeId>,
}

enum TrialOriginOp {
    Derive {
        node: NodeId,
        start: usize,
        len: usize,
        reason: &'static str,
    },
    Synthetic {
        node: NodeId,
        transformation: &'static str,
    },
    Reset {
        node: NodeId,
    },
}

impl TrialOrigins {
    fn new() -> Self {
        Self {
            ops: Vec::new(),
            values: Vec::new(),
        }
    }

    /// Drop the recorded calls but keep the buffers for the next attempt.
    fn clear(&mut self) {
        self.ops.clear();
        self.values.clear();
    }

    /// Record the equivalent of derive_samples for a trial node.
    fn derive(
        &mut self,
        target: &Ast,
        node: NodeId,
        source: &Ast,
        values: &[NodeId],
        reason: &'static str,
    ) {
        if !target.nodes.tracks_origins() {
            return;
        }
        if values.is_empty() || values.iter().any(|&id| !source.nodes.has_origin(id)) {
            self.ops.push(TrialOriginOp::Reset { node });
        } else {
            let start = self.values.len();
            self.values.extend_from_slice(values);
            self.ops.push(TrialOriginOp::Derive {
                node,
                start,
                len: values.len(),
                reason,
            });
        }
    }

    /// Record the equivalent of mark_synthetic for a trial node.
    fn synthetic(&mut self, target: &Ast, node: NodeId, transformation: &'static str) {
        if target.nodes.tracks_origins() {
            self.ops.push(TrialOriginOp::Synthetic {
                node,
                transformation,
            });
        }
    }

    /// Apply the recorded calls, in order, for an accepted trial.
    fn apply(&self, target: &mut Ast, source: &Ast) {
        if !target.nodes.tracks_origins() {
            return;
        }
        for op in &self.ops {
            match op {
                TrialOriginOp::Derive {
                    node,
                    start,
                    len,
                    reason,
                } => {
                    let values = &self.values[*start..*start + *len];
                    target
                        .nodes
                        .derive_from(*node, &source.nodes, values[0], reason);
                    target
                        .nodes
                        .relate_many_from(*node, &source.nodes, &values[1..], reason);
                }
                TrialOriginOp::Synthetic {
                    node,
                    transformation,
                } => {
                    target.nodes.mark_synthetic(*node, transformation);
                }
                TrialOriginOp::Reset { node } => {
                    let syntax = target.node(*node).clone();
                    target.nodes[*node as usize] = syntax;
                }
            }
        }
    }
}

fn affine(
    source: &Ast,
    target: &mut Ast,
    values: &[NodeId],
    index: NodeId,
    numbers: Option<&[f64]>,
    plan: &mut TrialOrigins,
) -> Option<NodeId> {
    let first = target.nodes.len();
    let result = match numbers {
        Some(numbers) => affine_numbers(numbers, target, index, values[0]),
        None => affine_expression(source, target, values, index),
    }?;
    Some(finish_affine(
        target, source, values, index, first, result, plan,
    ))
}

/// The derive tail shared by affine() and by build_candidate's
/// mismatch-proven fast path, so both push exactly the same origins.
fn finish_affine(
    target: &mut Ast,
    source: &Ast,
    values: &[NodeId],
    index: NodeId,
    first: usize,
    result: NodeId,
    plan: &mut TrialOrigins,
) -> NodeId {
    // A unit progression can reduce to the index alone. That read is the
    // representation of this original column, not only loop-control syntax.
    if result == index {
        plan.derive(target, result, source, values, "screen-affine-arguments");
    }
    if target.nodes.tracks_origins() {
        for node in first..target.nodes.len() {
            plan.derive(
                target,
                node as NodeId,
                source,
                values,
                "screen-affine-arguments",
            );
        }
    }
    result
}

fn periodic_affine(
    source: &Ast,
    target: &mut Ast,
    values: &[NodeId],
    symbol: SymbolId,
    numbers: Option<&[f64]>,
    pre: Option<&PeriodicPre>,
    plan: &mut TrialOrigins,
) -> Option<NodeId> {
    let first = target.nodes.len();
    let result = match numbers {
        Some(numbers) => periodic_affine_numbers(numbers, target, symbol, pre, plan),
        None => periodic_affine_expression(source, target, values, symbol, plan),
    }?;
    if target.nodes.tracks_origins() {
        for node in first..target.nodes.len() {
            if matches!(target.node(node as NodeId), Node::Name(_)) {
                plan.synthetic(target, node as NodeId, "screen-loop-index");
            } else {
                plan.derive(
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod normalized_cost_tests {
    use super::*;
    #[test]
    fn borrowed_loop_cost_matches_the_full_clone_normalizer() {
        for source in [
            "screen.drawLine(x+1,0,x+1,10)screen.drawLine(x+2,0,x+2,10)",
            "f('雪😀')f({key=1})f(true,false,nil)",
            "object:method(veryLongParameter)other.field(nested(x),{key=2})",
            "screen.drawText(__index,0,__table[__index+1])screen.drawText(0,1,'x')",
        ] {
            for assign in [false, true] {
                let (mut ast, root) =
                    storm_lua_syntax::parse_source_with_origins("loop-cost.lua", source).unwrap();
                let Node::Block(original) = ast.node(root).clone() else {
                    panic!()
                };
                let calls = original
                    .iter()
                    .map(|&id| match ast.node(id) {
                        Node::Callstat(call) => *call,
                        _ => panic!(),
                    })
                    .collect::<Vec<_>>();
                let table = ast.strings.fresh("__table");
                let index = ast.strings.fresh("__index");
                let target = ast.push(Node::Name(table));
                let value = ast.push(Node::Table(vec![TableField::Arr(calls[0])]));
                let assignment = assign.then(|| ast.push(Node::Assign(vec![target], vec![value])));
                let f = ast.push(Node::Name(table));
                let i = ast.push(Node::Name(index));
                let method_sym = ast.strings.intern("method");
                let new_call = ast.push(Node::Call(f, vec![i], Some(method_sym)));
                let callstat = ast.push(Node::Callstat(new_call));
                let body = ast.push(Node::Block(vec![callstat]));
                let start = ast.push(Node::Num("0".into()));
                let end = ast.push(Node::Num("7".into()));
                let loop_ = ast.push(Node::Fornum(index, start, end, None, body));
                let before = serde_json::to_vec(&ast).unwrap();
                assert_eq!(
                    normalize_cost(&ast, &calls, assignment, loop_, table, index),
                    reference_normalize_cost(&ast, &calls, assignment, loop_, table, index),
                    "{source}, assignment={assign}"
                );
                assert_eq!(serde_json::to_vec(&ast).unwrap(), before);
            }
        }
    }
}
