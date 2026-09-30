//! Program-wide pair dictionaries amortize one decoder across small draw runs.
//! Each candidate is a representation of scalar argument tuples, not knowledge
//! of rectangle rasterization. Calls never cross statement/control boundaries.
use super::*;

struct Run {
    block: NodeId,
    start: usize,
    stop: usize,
    calls: Vec<Call>,
}
struct Candidate {
    shape: Shape,
    payloads: Vec<(usize, Payload)>,
    gain: usize,
}

pub(super) fn fixed_payload(codec: &Codec, columns: &[Vec<i64>], count: usize) -> Option<String> {
    let (biases, radices, width, prefixes) = match codec {
        Codec::PackedBytes {
            biases,
            radices,
            width,
        } => (biases, radices, *width, None),
        Codec::PrefixBytes {
            biases,
            radices,
            width,
            prefixes,
        } => (biases, radices, *width, Some(*prefixes)),
        _ => return None,
    };
    let capacity = 92i64.pow(width as u32);
    let cutoff = prefixes.map(|n| n as i64 * (capacity / 92));
    let mut text = String::new();
    for row in 0..count {
        let mut value = 0i64;
        for ((column, &bias), &radix) in columns.iter().zip(biases).zip(radices) {
            let digit = column[row].checked_sub(bias)?;
            if !(0..radix).contains(&digit) {
                return None;
            }
            value = value.checked_mul(radix)?.checked_add(digit)?;
        }
        let (encoded, digits) = if cutoff.is_some_and(|cutoff| value >= cutoff) {
            (value - cutoff? + prefixes? as i64 * capacity, width + 1)
        } else {
            (value, width)
        };
        let mut divisor = 92i64.pow(digits as u32 - 1);
        for _ in 0..digits {
            text.push((encoded / divisor % 92 + 35) as u8 as char);
            divisor /= 92;
        }
    }
    Some(text)
}

fn candidates(runs: &[Run], indices: &[usize]) -> Vec<Candidate> {
    let arity = runs[indices[0]].calls[0].args.len();
    let rows: Vec<_> = indices.iter().flat_map(|&i| &runs[i].calls).collect();
    if !(2..=6).contains(&arity) || !(24..=8192).contains(&rows.len()) {
        return Vec::new();
    }
    let Some(integers) = rows
        .iter()
        .map(|c| c.args.iter().map(Atom::integer).collect::<Option<Vec<_>>>())
        .collect::<Option<Vec<_>>>()
    else {
        return Vec::new();
    };
    if integers
        .iter()
        .flatten()
        .any(|n| !(-1_000_000_000..=1_000_000_000).contains(n))
    {
        return Vec::new();
    }
    let mut out = Vec::new();
    for first in 0..arity {
        for second in first + 1..arity {
            let mut frequencies = BTreeMap::new();
            for r in &integers {
                *frequencies.entry((r[first], r[second])).or_insert(0usize) += 1;
            }
            if !(2..=64).contains(&frequencies.len()) || frequencies.len() * 3 > rows.len() {
                continue;
            }
            let mut tuples: Vec<_> = frequencies.into_iter().collect();
            // Common pairs occupy the low part of the mixed-radix integer,
            // allowing the prefix representation to use shorter records.
            tuples.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let lookup: BTreeMap<_, _> = tuples
                .iter()
                .enumerate()
                .map(|(i, (t, _))| (*t, i as i64 + 1))
                .collect();
            let mut arguments = Vec::new();
            let mut next = 1;
            for arg in 0..arity {
                if arg == first || arg == second {
                    let values = tuples
                        .iter()
                        .map(|((a, b), _)| {
                            Atom::Num((if arg == first { *a } else { *b }).to_string().into())
                        })
                        .collect();
                    arguments.push(Arg::Lookup(0, values));
                } else {
                    arguments.push(Arg::Column(next));
                    next += 1;
                }
            }
            let project = |calls: &[Call]| -> Option<Vec<Vec<i64>>> {
                let mut columns = vec![Vec::new(); arity - 1];
                for call in calls {
                    let a = call.args[first].integer()?;
                    let b = call.args[second].integer()?;
                    columns[0].push(*lookup.get(&(a, b))?);
                    let mut col = 1;
                    for arg in 0..arity {
                        if arg != first && arg != second {
                            columns[col].push(call.args[arg].integer()?);
                            col += 1;
                        }
                    }
                }
                Some(columns)
            };
            let mut columns = vec![Vec::new(); arity - 1];
            for &run in indices {
                #[expect(
                    clippy::expect_used,
                    reason = "The candidate group was built from integer calls compatible with this shared dictionary projection"
                )]
                let values = project(&runs[run].calls).expect("validated integer calls");
                for (column, values) in columns.iter_mut().zip(values) {
                    column.extend(values)
                }
            }
            let Some((codec @ Codec::PackedBytes { .. }, _)) =
                packed_payload_with_limit(&columns, rows.len(), 8)
            else {
                continue;
            };
            let mut codecs = vec![codec.clone()];
            if let Codec::PackedBytes {
                biases,
                radices,
                width,
            } = &codec
            {
                if *width >= 2 {
                    let width = width - 1;
                    let cardinality: i64 = radices.iter().product();
                    let small = 92i64.pow(width as u32 - 1);
                    let large = small * 92;
                    let prefixes = ((92 * large - cardinality) / (large - small)).min(91);
                    if prefixes > 0 {
                        codecs.push(Codec::PrefixBytes {
                            biases: biases.clone(),
                            radices: radices.clone(),
                            width,
                            prefixes: prefixes as usize,
                        });
                    }
                }
            }
            for codec in codecs {
                let shape = Shape {
                    targets: vec![0],
                    args: vec![arguments.clone()],
                    columns: arity - 1,
                    codec,
                    repeats: 1,
                    translations: Vec::new(),
                };
                let mut payloads = Vec::new();
                for &run in indices {
                    let r = &runs[run];
                    #[expect(
                        clippy::expect_used,
                        reason = "The group selection and dictionary construction validated every call in these immutable runs"
                    )]
                    let columns = project(&r.calls).expect("validated integer calls");
                    #[expect(
                        clippy::expect_used,
                        reason = "The fixed codec bounds were selected from these same validated integer columns"
                    )]
                    let payload = Payload::Bytes(
                        fixed_payload(&shape.codec, &columns, r.calls.len())
                            .expect("bounded dictionary data"),
                    );
                    payloads.push((run, payload));
                }
                out.push(Candidate {
                    shape,
                    payloads,
                    gain: 0,
                });
            }
        }
    }
    out
}

pub(super) fn synthesize(ast: &mut Ast, root: NodeId, rename: bool) -> usize {
    let mut jumps = false;
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |n| {
        jumps |= matches!(ast.node(n), Node::Goto(_) | Node::Label(_))
    });
    if jumps
        || chunk_locals(ast, root) > 145
        || !super::super::closed_fields::has_closed_key_space(ast, root)
    {
        return 0;
    }
    let source = ast.clone();
    let res = resolve(&source, root);
    // Match the record planner's conservative upvalue/temporary headroom.
    let mut inherited = vec![0usize; res.scopes.len()];
    for scope in &res.scopes {
        inherited[scope.id as usize] = scope.parent.map_or(0, |p| inherited[p as usize])
            + scope
                .bindings
                .iter()
                .filter(|&&b| res.binding(b).kind != BindingKind::Global)
                .count();
    }
    if inherited.into_iter().max().unwrap_or(0) > 200 {
        return 0;
    }
    let classifier = Classifier::new(&source, &res, root);
    let mut blocks = Vec::new();
    storm_lua_syntax::ast_utils::walk(&source, root, &mut |n| {
        if matches!(source.node(n), Node::Block(_)) {
            blocks.push(n)
        }
    });
    let mut runs = Vec::new();
    let mut groups = BTreeMap::<(String, usize), Vec<usize>>::new();
    for block in blocks {
        let Node::Block(stmts) = source.node(block) else {
            unreachable!()
        };
        let mut at = 0;
        while at < stmts.len() {
            let Some(first) = classifier.call(stmts[at]) else {
                at += 1;
                continue;
            };
            let key = (first.key.clone(), first.args.len());
            let mut calls = vec![first];
            while at + calls.len() < stmts.len() {
                let Some(next) = classifier.call(stmts[at + calls.len()]) else {
                    break;
                };
                if next.key != key.0 || next.args.len() != key.1 {
                    break;
                }
                calls.push(next);
            }
            let stop = at + calls.len();
            if calls.len() >= 4 {
                groups.entry(key).or_default().push(runs.len());
                runs.push(Run {
                    block,
                    start: at,
                    stop,
                    calls,
                });
            }
            at = stop;
        }
    }
    let mut selected = Vec::new();
    for indices in groups.values() {
        let mut best = None::<Candidate>;
        for mut c in candidates(&runs, indices) {
            let mut total = 0;
            c.payloads.retain(|(i, payload)| {
                let run = &runs[*i];
                let old = run
                    .calls
                    .iter()
                    .map(|c| measure_stmt(&source, c.statement))
                    .sum::<usize>();
                let new = 4 + measure_expr(&source, run.calls[0].callee) + payload.size();
                if old > new {
                    total += old - new;
                    true
                } else {
                    false
                }
            });
            c.gain = total.saturating_sub(helper_cost(&c.shape) + 1);
            if c.gain > 0 && best.as_ref().is_none_or(|b| c.gain > b.gain) {
                best = Some(c)
            }
        }
        if let Some(c) = best {
            selected.push(c)
        }
    }
    selected.sort_by_key(|b| std::cmp::Reverse(b.gain));
    selected.truncate(4);
    if selected.is_empty() {
        return 0;
    }
    let mut target = source.clone();
    let mut taken = source
        .strings
        .all_strings()
        .into_iter()
        .collect::<HashSet<_>>();
    let mut serial = 0;
    let mut definitions = Vec::new();
    let mut edits = BTreeMap::<NodeId, BTreeMap<usize, (usize, NodeId)>>::new();
    for c in &selected {
        let symbol = fresh(&mut target, &mut taken, "shared_draw", &mut serial);
        let first = target.nodes.len();
        let mut trace = source.nodes.tracks_origins().then(|| {
            let calls = c
                .payloads
                .iter()
                .flat_map(|(i, _)| runs[*i].calls.iter())
                .collect::<Vec<_>>();
            provenance::Trace::new(&calls, c.shape.args.len())
        });
        let definition =
            encoding::emit_helper_recording(&mut target, &c.shape, symbol, serial, &mut trace);
        if let Some(t) = trace {
            t.finish(&mut target, &source, first, definition);
        }
        definitions.push(definition);
        for (i, payload) in &c.payloads {
            let run = &runs[*i];
            let f = name(&mut target, symbol);
            let callee = copy_callee(&source, &mut target, run.calls[0].callee);
            let data = payload.emit(&mut target);
            let call = target.push(Node::Call(f, vec![callee, data], None));
            let stmt = target.push(Node::Callstat(call));
            if source.nodes.tracks_origins() {
                let inputs = run
                    .calls
                    .iter()
                    .flat_map(|c| c.origin_arguments.iter().copied())
                    .collect::<Vec<_>>();
                provenance::derive(
                    &mut target,
                    data,
                    &source,
                    &inputs,
                    "draw-record-encoded-payload",
                );
                target
                    .nodes
                    .mark_synthetic(f, "draw-record-helper-reference");
                let commands = run.calls.iter().map(|c| c.origin_call).collect::<Vec<_>>();
                provenance::derive(
                    &mut target,
                    call,
                    &source,
                    &commands,
                    "draw-record-batch-call",
                );
                provenance::derive(
                    &mut target,
                    stmt,
                    &source,
                    &commands,
                    "draw-record-batch-call",
                );
            }
            edits
                .entry(run.block)
                .or_default()
                .insert(run.start, (run.stop, stmt));
        }
    }
    for (block, changes) in edits {
        let Node::Block(old) = source.node(block) else {
            unreachable!()
        };
        let mut out = Vec::new();
        let mut at = 0;
        for (start, (stop, stmt)) in changes {
            out.extend_from_slice(&old[at..start]);
            out.push(stmt);
            at = stop;
        }
        out.extend_from_slice(&old[at..]);
        target
            .nodes
            .rewrite(block, Node::Block(out), "shared-draw-record-insertion");
    }
    let Node::Block(stmts) = target.node(root).clone() else {
        return 0;
    };
    definitions.extend(stmts);
    target.nodes.rewrite(
        root,
        Node::Block(definitions),
        "shared-draw-record-insertion",
    );
    remove_unused_forwarders(&mut target, root);
    if rename {
        target = scope_rename_fast(&target, root).ast
    }
    if measure_size(&target, root) >= measure_size(&source, root) {
        return 0;
    }
    *ast = target;
    selected.len()
}
