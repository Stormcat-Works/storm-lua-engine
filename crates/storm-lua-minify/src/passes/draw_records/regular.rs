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

fn predicted(calls: &[Call], period: usize, count: usize) -> Option<(Shape, Vec<NodeId>)> {
    let mut targets = Vec::new();
    let mut args = Vec::new();
    for slot in 0..period {
        let call = &calls[slot];
        targets.push(call.callee);
        let mut arguments = Vec::new();
        for arg in 0..call.args.len() {
            let values = (0..count)
                .map(|r| &calls[r * period + slot].args[arg])
                .collect::<Vec<_>>();
            if values.iter().all(|v| *v == values[0]) {
                arguments.push(Arg::Constant(values[0].clone()));
            } else {
                let integers = values
                    .iter()
                    .map(|v| v.integer())
                    .collect::<Option<Vec<_>>>()?;
                arguments.push(Arg::Series(predictors::fit(&integers)?));
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
        targets,
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

pub(super) fn synthesize(ast: &mut Ast, root: NodeId, rename: bool) -> usize {
    // pack_impl already applies these guards, but this precursor runs first.
    // Budget conservatively for the numeric loop's control registers.
    let mut jumps = false;
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |n| {
        jumps |= matches!(ast.node(n), Node::Goto(_) | Node::Label(_))
    });
    if jumps || !super::super::closed_fields::has_closed_key_space(ast, root) {
        return 0;
    }
    let source = ast.clone();
    let res = resolve(&source, root);
    let mut inherited = vec![0; res.scopes.len()];
    for scope in &res.scopes {
        inherited[scope.id as usize] = scope.parent.map_or(0, |p| inherited[p as usize])
            + scope
                .bindings
                .iter()
                .filter(|&&b| res.binding(b).kind != BindingKind::Global)
                .count();
    }
    if inherited.into_iter().max().unwrap_or(0) >= 175 {
        return 0;
    }
    let classifier = Classifier::new(&source, &res, root);
    let mut blocks = Vec::new();
    storm_lua_syntax::ast_utils::walk(&source, root, &mut |n| {
        if matches!(source.node(n), Node::Block(_)) {
            blocks.push(n);
        }
    });
    let mut taken = source
        .strings
        .all_strings()
        .into_iter()
        .collect::<HashSet<_>>();
    let mut serial = 0;
    let mut loops = 0;
    for block in blocks {
        let Node::Block(stmts) = source.node(block) else {
            unreachable!()
        };
        let mut output = Vec::new();
        let mut cursor = 0;
        while cursor < stmts.len() {
            let mut calls = Vec::new();
            for &s in &stmts[cursor..] {
                if let Some(c) = classifier.call(s) {
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
            let n = calls.len();
            let mut costs = vec![0usize; n + 1];
            let mut choices = vec![None; n];
            // Classifier-normalized callee and argument-count signatures must
            // match at every row. No color or statement boundary is crossed.
            for at in (0..n).rev() {
                costs[at] = measure_stmt(&source, calls[at].statement) + costs[at + 1];
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
                    for count in counts {
                        let Some((shape, targets)) = predicted(slice, period, count) else {
                            continue;
                        };
                        let plan = Loop {
                            stop: at + count * period,
                            count,
                            shape,
                            targets,
                        };
                        let size = cost(&source, &plan) + costs[plan.stop];
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
                    output.push(emit(&source, ast, plan, index, &calls[at..plan.stop]));
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
        if measure_size(ast, root) >= measure_size(&source, root) {
            *ast = source;
            return 0;
        }
    }
    loops
}
