//! Inline exact regular drawing runs. A bounded suffix dynamic program chooses
//! among unchanged statements and fully predicted loops, before byte packing.
//! No payload, callee caching, floating-point fit or extra function is needed.
use super::*;

const MAX_PREFIX: usize = 32;

#[derive(Clone)]
struct Loop {
    stop: usize,
    count: usize,
    shape: Shape,
    targets: Vec<NodeId>,
}

/// One (slot, argument) column of a regular-draw run, built once for the
/// maximal trial count and shared across the DP's trial counts (which are
/// nested prefixes of that count). See `predicted` for the equivalence
/// argument.
struct PrecomputedColumn<'a> {
    /// Atom of the column's first row; the value of a constant prefix.
    first: &'a Atom,
    /// First row whose atom differs from the first row's; None when the
    /// whole maximal column is constant.
    first_diff: Option<usize>,
    /// Integer values up to (excluding) the first non-integer atom.
    integers: Vec<i64>,
    /// First row whose atom has no integer value; None when all rows do.
    first_non_integer: Option<usize>,
}

struct Precomputed<'a> {
    targets: Vec<NodeId>,
    columns: Vec<Vec<PrecomputedColumn<'a>>>,
    /// Smallest count above which predicted() must return None: the
    /// minimum, over columns that are both non-constant and
    /// non-integer somewhere, of max(first_diff, first_non_integer).
    /// None when no column can fail this way.
    fail_above: Option<usize>,
}

/// Build the shared columns for one (at, period) run at its maximal trial
/// count. Column extraction is the part of `predicted` that is identical
/// across trial counts up to prefixing, so it is done once here instead of
/// once per trial count. Integer conversion is not repeated per window
/// either: `ints` carries every call argument's integer value for the
/// whole run segment, parsed once by the caller (`Atom::integer` is a
/// pure, total function of the atom, so the memoized values are exactly
/// what per-window conversion produced).
fn precompute_columns<'a>(
    calls: &'a [Call],
    ints: &[Vec<Option<i64>>],
    period: usize,
    max_count: usize,
) -> Precomputed<'a> {
    let mut targets = Vec::new();
    let mut columns = Vec::new();
    for slot in 0..period {
        let call = &calls[slot];
        targets.push(call.callee);
        let mut slot_columns = Vec::new();
        for (arg, first) in call.args.iter().enumerate() {
            let mut first_diff = None;
            let mut integers = Vec::new();
            let mut first_non_integer = None;
            for r in 0..max_count {
                let idx = r * period + slot;
                if first_diff.is_none() && calls[idx].args[arg] != *first {
                    first_diff = Some(r);
                }
                if first_non_integer.is_none() {
                    match ints[idx][arg] {
                        Some(v) => integers.push(v),
                        None => first_non_integer = Some(r),
                    }
                }
            }
            slot_columns.push(PrecomputedColumn {
                first,
                first_diff,
                integers,
                first_non_integer,
            });
        }
        columns.push(slot_columns);
    }
    let mut fail_above = None;
    for slot_columns in &columns {
        for col in slot_columns {
            if let (Some(d), Some(b)) = (col.first_diff, col.first_non_integer) {
                let t = d.max(b);
                fail_above = Some(fail_above.map_or(t, |m: usize| m.min(t)));
            }
        }
    }
    Precomputed {
        targets,
        columns,
        fail_above,
    }
}

/// Trial-count view of `precompute_columns`: decide, for one trial count,
/// exactly what the unshared code decided for that count's prefix.
/// - A prefix is constant exactly when its rows agree with atoms[0], which
///   is first_diff >= count (or no differing row at all).
/// - A non-constant prefix converts to integers exactly when no row before
///   count lacks an integer value, i.e. first_non_integer >= count (or none).
/// - fit is evaluated per trial count on the prefix integers, unchanged:
///   a fit result is not determined by any other trial count's result.
fn predicted(pre: &Precomputed<'_>, period: usize, count: usize) -> Option<(Shape, Vec<NodeId>)> {
    // A column that is both non-constant and non-integer within the
    // prefix makes the walk return None, wherever it sits: the walk's
    // result is None as soon as any column fails, and later columns
    // or fits cannot change that. Skip the whole column walk then.
    if pre.fail_above.is_some_and(|t| count > t) {
        return None;
    }
    let mut args = Vec::new();
    for slot in 0..period {
        let mut arguments = Vec::new();
        for col in &pre.columns[slot] {
            if col.first_diff.is_none_or(|d| d >= count) {
                arguments.push(Arg::Constant(col.first.clone()));
            } else {
                if col.first_non_integer.is_some_and(|b| b < count) {
                    return None;
                }
                arguments.push(Arg::Series(predictors::fit(&col.integers[..count])?));
            }
        }
        args.push(arguments);
    }
    Some((
        Shape {
            targets: (0..period).collect(),
            args,
            columns: 0,
            codec: Codec::Counter,
            repeats: 1,
            translations: Vec::new(),
        },
        pre.targets.clone(),
    ))
}

fn emit(source: &Ast, target: &mut Ast, plan: &Loop, index: SymbolId, calls: &[Call]) -> NodeId {
    let mut body = Vec::new();
    for (slot, (&callee, arguments)) in plan.targets.iter().zip(&plan.shape.args).enumerate() {
        let callee = copy_callee(source, target, callee);
        let args = arguments
            .iter()
            .enumerate()
            .map(|(column, arg)| {
                let first = target.nodes.len();
                let result = match arg {
                    Arg::Constant(a) => a.emit(target),
                    Arg::Series(series) => {
                        let row = name(target, index);
                        series.emit(target, row)
                    }
                    _ => unreachable!("inline loops have no stored columns"),
                };
                if target.nodes.tracks_origins() {
                    let inputs = calls
                        .iter()
                        .skip(slot)
                        .step_by(plan.shape.targets.len())
                        .map(|c| c.origin_arguments[column])
                        .collect::<Vec<_>>();
                    for id in first..target.nodes.len() {
                        provenance::derive(
                            target,
                            id as NodeId,
                            source,
                            &inputs,
                            "regular-draw-argument",
                        );
                    }
                }
                result
            })
            .collect();
        let call = target.push(Node::Call(callee, args, None));
        let stmt = target.push(Node::Callstat(call));
        if target.nodes.tracks_origins() {
            let inputs = calls
                .iter()
                .skip(slot)
                .step_by(plan.shape.targets.len())
                .map(|c| c.origin_call)
                .collect::<Vec<_>>();
            provenance::derive(target, call, source, &inputs, "regular-draw-replay");
            provenance::derive(target, stmt, source, &inputs, "regular-draw-replay");
        }
        body.push(stmt);
    }
    let body = target.push(Node::Block(body));
    let start = num(target, 0);
    let end = num(target, plan.count as i64 - 1);
    {
        let loop_ = target.push(Node::Fornum(index, start, end, None, body));
        for id in [body, start, end, loop_] {
            target.nodes.mark_synthetic(id, "regular-draw-loop-control");
        }
        loop_
    }
}

fn cost(source: &Ast, plan: &Loop) -> usize {
    // A one-character fresh index estimates its eventual renamed form. User
    // callee spelling is retained in the cost; this is not a semantic key.
    let mut ast = Ast::new();
    let f = ast.strings.intern("f");
    let callees = plan.targets.iter().map(|_| name(&mut ast, f)).collect();
    let mock = Loop {
        targets: callees,
        ..plan.clone()
    };
    let i = ast.strings.intern("i");
    let input = ast.clone();
    let loop_ = emit(&input, &mut ast, &mock, i, &[]);
    measure_stmt(&ast, loop_)
        + plan
            .targets
            .iter()
            .map(|&n| measure_expr(source, n) - 1)
            .sum::<usize>()
}

pub(super) fn synthesize(ast: &mut Ast, root: NodeId, rename: bool, input: &SharedInput) -> usize {
    // pack_impl already applies these guards, but this precursor runs first.
    // Budget conservatively for the numeric loop's control registers.
    if input.facts().jumps || !input.closed_key_space() {
        return 0;
    }
    if input.max_inherited() >= 175 {
        return 0;
    }
    let source = input.source();
    let mut taken = source
        .strings
        .all_strings()
        .into_iter()
        .collect::<HashSet<_>>();
    let mut serial = 0;
    let mut loops = 0;
    for block in input.blocks().iter().copied() {
        let Node::Block(stmts) = source.node(block) else {
            unreachable!()
        };
        let mut output = Vec::new();
        let mut cursor = 0;
        while cursor < stmts.len() {
            let mut calls = Vec::new();
            for &s in &stmts[cursor..] {
                if let Some(c) = input.call(s) {
                    calls.push(c);
                } else {
                    break;
                }
            }
            if calls.len() < 4 {
                let length = calls.len().max(1);
                output.extend_from_slice(&stmts[cursor..cursor + length]);
                cursor += length;
                continue;
            }
            // Integer values of every argument in this run segment,
            // parsed once: the DP's per-window column extraction reads
            // them via precompute_columns instead of re-parsing the same
            // atoms for every (at, period) window they appear in.
            let ints: Vec<Vec<Option<i64>>> = calls
                .iter()
                .map(|c| c.args.iter().map(|a| a.integer()).collect())
                .collect();
            let n = calls.len();
            let mut costs = vec![0usize; n + 1];
            let mut choices = vec![None; n];
            // Classifier-normalized callee and argument-count signatures must
            // match at every row. No color or statement boundary is crossed.
            for at in (0..n).rev() {
                costs[at] = measure_stmt(source, calls[at].statement) + costs[at + 1];
                for period in 1..=MAX_PERIOD.min((n - at) / 4) {
                    let slice = &calls[at..];
                    let mut count = 1;
                    // Bounded work at interior starts; a whole-cluster model
                    // is also considered at its beginning for very long runs.
                    let cap = if at == 0 {
                        slice.len() / period
                    } else {
                        MAX_PREFIX
                    };
                    while count < cap
                        && (count + 1) * period <= slice.len()
                        && (0..period).all(|p| {
                            slice[p].key == slice[count * period + p].key
                                && slice[p].args.len() == slice[count * period + p].args.len()
                        })
                    {
                        count += 1;
                    }
                    if count < 4 {
                        continue;
                    }
                    let mut counts = (4..=count.min(MAX_PREFIX)).collect::<Vec<_>>();
                    if count > MAX_PREFIX {
                        counts.push(count);
                    }
                    let columns = precompute_columns(slice, &ints[at..], period, count);
                    for count in counts {
                        let Some((shape, targets)) = predicted(&columns, period, count) else {
                            continue;
                        };
                        let plan = Loop {
                            stop: at + count * period,
                            count,
                            shape,
                            targets,
                        };
                        let size = cost(source, &plan) + costs[plan.stop];
                        if size < costs[at] {
                            costs[at] = size;
                            choices[at] = Some(plan);
                        }
                    }
                }
            }
            let mut at = 0;
            while at < n {
                if let Some(plan) = &choices[at] {
                    let index = fresh(ast, &mut taken, "draw_row", &mut serial);
                    output.push(emit(source, ast, plan, index, &calls[at..plan.stop]));
                    loops += 1;
                    at = plan.stop;
                } else {
                    output.push(calls[at].statement);
                    at += 1;
                }
            }
            cursor += n;
        }
        ast.nodes
            .rewrite(block, Node::Block(output), "regular-draw-run-replacement");
    }
    if loops > 0 {
        remove_unused_forwarders(ast, root);
        if rename {
            *ast = scope_rename_fast(ast, root).ast;
        }
        if measure_size(ast, root) >= measure_size(source, root) {
            *ast = source.clone();
            return 0;
        }
    }
    loops
}
