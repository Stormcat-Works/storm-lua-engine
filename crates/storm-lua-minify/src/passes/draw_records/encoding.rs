//! Emit exact replay decoders for table, ASCII, mixed-radix and delta records.
//! All arithmetic construction is shared by cost estimation and actual output.
use super::*;

fn column_value(
    ast: &mut Ast,
    codec: &Codec,
    col: usize,
    vars: &[SymbolId],
    packed: SymbolId,
) -> NodeId {
    match codec {
        Codec::PackedBytes {
            biases, radices, ..
        } => {
            let mut v = name(ast, packed);
            let stride: i64 = radices[col + 1..].iter().product();
            if stride != 1 {
                let divisor = num(ast, stride);
                v = ast.push(Node::Bin("//".into(), v, divisor));
            }
            if col > 0 {
                let radix = num(ast, radices[col]);
                v = ast.push(Node::Bin("%".into(), v, radix));
            }
            offset(ast, v, biases[col])
        }
        Codec::Bytes(biases) => {
            let v = name(ast, vars[col]);
            offset(ast, v, biases[col] - 35)
        }
        _ => name(ast, vars[col]),
    }
}

pub(super) fn emit_helper(ast: &mut Ast, shape: &Shape, symbol: SymbolId, serial: usize) -> NodeId {
    emit_helper_recording(ast, shape, symbol, serial, &mut None)
}

pub(super) fn emit_helper_recording(
    ast: &mut Ast,
    shape: &Shape,
    symbol: SymbolId,
    serial: usize,
    trace: &mut Option<provenance::Trace>,
) -> NodeId {
    if matches!(shape.codec, Codec::GroupedBytes { .. }) {
        return super::grouped::emit_helper(ast, shape, symbol, serial, trace);
    }
    if matches!(shape.codec, Codec::PrefixBytes { .. }) {
        return super::prefix::emit_helper(ast, shape, symbol, serial, trace);
    }
    let targets = 1 + shape.targets.iter().copied().max().unwrap_or(0);
    let functions = (0..targets)
        .map(|n| ast.strings.intern(&format!("__draw_f_{serial}_{n}")))
        .collect::<Vec<_>>();
    let data = ast.strings.intern(&format!("__draw_d_{serial}"));
    let index = ast.strings.intern(&format!("__draw_i_{serial}"));
    let packed = ast.strings.intern(&format!("__draw_p_{serial}"));
    let tile_index = ast.strings.intern(&format!("__draw_t_{serial}"));
    let vars = (0..shape.columns)
        .map(|n| ast.strings.intern(&format!("__draw_v_{serial}_{n}")))
        .collect::<Vec<_>>();
    let zero_counter = shape.codec == Codec::Counter
        && shape
            .args
            .iter()
            .flatten()
            .any(|a| matches!(a, Arg::Series(_)));
    let mut prefix = Vec::new();
    let mut dictionaries = BTreeMap::new();
    for arg in shape.args.iter().flatten() {
        if let Arg::Lookup(_, entries) = arg {
            if dictionaries.contains_key(entries) {
                continue;
            }
            let symbol = ast
                .strings
                .intern(&format!("__draw_lookup_{serial}_{}", dictionaries.len()));
            let fields = entries
                .iter()
                .map(|a| {
                    let start = ast.nodes.len();
                    let n = a.emit(ast);
                    if let Some(t) = trace.as_mut() {
                        t.lookup(start..ast.nodes.len(), shape, entries, Some(a));
                    }
                    TableField::Arr(n)
                })
                .collect();
            let table = ast.push(Node::Table(fields));
            let mut value = table;
            let mut byte_bias = None;
            if let Some(numbers) = entries
                .iter()
                .map(Atom::integer)
                .collect::<Option<Vec<_>>>()
            {
                #[expect(
                    clippy::expect_used,
                    reason = "The codec emitter only receives nonempty dictionaries from matched drawing runs"
                )]
                let low = *numbers.iter().min().expect("nonempty dictionary");
                #[expect(
                    clippy::expect_used,
                    reason = "The codec emitter only receives nonempty dictionaries from matched drawing runs"
                )]
                let high = *numbers.iter().max().expect("nonempty dictionary");
                if low >= -1_000_000_000 && high <= 1_000_000_000 && high - low <= 91 {
                    let text: String = numbers
                        .iter()
                        .map(|n| (n - low + 35) as u8 as char)
                        .collect();
                    let string = ast.push(Node::Str(quote_lua(&text).into()));
                    if let Some(t) = trace.as_mut() {
                        t.lookup(string as usize..string as usize + 1, shape, entries, None);
                    }
                    let d = name(ast, symbol);
                    let at = num(ast, 1);
                    let lookup = ast.push(Node::Index(d, at, false));
                    let d = name(ast, symbol);
                    let at = num(ast, 1);
                    let byte = ast.push(Node::Call(d, vec![at], Some("byte".into())));
                    let decoded = offset(ast, byte, low - 35);
                    let uses = shape
                        .args
                        .iter()
                        .flatten()
                        .filter(|a| matches!(a, Arg::Lookup(_, other) if other == entries))
                        .count();
                    if measure_expr(ast, string) + uses * measure_expr(ast, decoded)
                        < measure_expr(ast, table) + uses * measure_expr(ast, lookup)
                    {
                        value = string;
                        byte_bias = Some(low - 35);
                    }
                }
            }
            prefix.push(ast.push(Node::Local(vec![symbol], vec![value])));
            dictionaries.insert(entries.clone(), (symbol, byte_bias));
        }
    }
    let mut accumulators = Vec::new();
    let codec = if let Codec::Delta { firsts, inner } = &shape.codec {
        accumulators = (0..shape.columns)
            .map(|n| ast.strings.intern(&format!("__draw_a_{serial}_{n}")))
            .collect::<Vec<_>>();
        let initial = firsts
            .iter()
            .enumerate()
            .map(|(column, &v)| {
                let node = num(ast, v);
                if let Some(trace) = trace.as_mut() {
                    trace.column(node as usize..node as usize + 1, shape, column, true);
                }
                node
            })
            .collect();
        prefix.push(ast.push(Node::Local(accumulators.clone(), initial)));
        inner.as_ref()
    } else {
        &shape.codec
    };
    let mut body = Vec::new();
    let mut decoded = Vec::new();
    match codec {
        Codec::Rice { columns, .. } => {
            let (setup, decode) =
                super::rice::decoder(ast, columns, data, &vars, serial, shape, trace);
            prefix.extend(setup);
            body.extend(decode);
        }
        Codec::Counter => {}
        Codec::Delta { .. } | Codec::GroupedBytes { .. } | Codec::PrefixBytes { .. } => {
            unreachable!("separately emitted codec")
        }
        Codec::Table => {
            for col in 0..shape.columns {
                let d = name(ast, data);
                let i = name(ast, index);
                let at = offset(ast, i, col as i64);
                decoded.push(ast.push(Node::Index(d, at, false)));
            }
            body.push(ast.push(Node::Local(vars.clone(), decoded)));
        }
        Codec::PackedBytes { width, .. } => {
            let d = name(ast, data);
            let i = name(ast, index);
            let end_i = name(ast, index);
            let end = offset(ast, end_i, *width as i64 - 1);
            let args = if *width == 1 { vec![i] } else { vec![i, end] };
            let bytes = ast.push(Node::Call(d, args, Some("byte".into())));
            // Wide rows can use more encoded bytes than logical columns.
            let raw = (0..*width)
                .map(|n| ast.strings.intern(&format!("__draw_b_{serial}_{n}")))
                .collect::<Vec<_>>();
            body.push(ast.push(Node::Local(raw.clone(), vec![bytes])));
            let first = name(ast, raw[0]);
            let mut value = offset(ast, first, -35);
            for &symbol in &raw[1..] {
                let radix = num(ast, 92);
                value = ast.push(Node::Bin("*".into(), value, radix));
                let next = name(ast, symbol);
                let digit = offset(ast, next, -35);
                value = ast.push(Node::Bin("+".into(), value, digit));
            }
            body.push(ast.push(Node::Local(vec![packed], vec![value])));
        }
        Codec::Bytes(_) => {
            let d = name(ast, data);
            let i = name(ast, index);
            let end_i = name(ast, index);
            let end = offset(ast, end_i, shape.columns as i64 - 1);
            let args = if shape.columns == 1 {
                vec![i]
            } else {
                vec![i, end]
            };
            let bytes = ast.push(Node::Call(d, args, Some("byte".into())));
            body.push(ast.push(Node::Local(vars.clone(), vec![bytes])));
        }
    }
    if !accumulators.is_empty() {
        let mut lhs = Vec::new();
        let mut rhs = Vec::new();
        for (col, &symbol) in accumulators.iter().enumerate() {
            lhs.push(name(ast, symbol));
            let old = name(ast, symbol);
            let delta = column_value(ast, codec, col, &vars, packed);
            rhs.push(ast.push(Node::Bin("+".into(), old, delta)));
        }
        body.push(ast.push(Node::Assign(lhs, rhs)));
    }
    let value_vars = if accumulators.is_empty() {
        &vars
    } else {
        &accumulators
    };
    for (slot, (which, arguments)) in shape.targets.iter().zip(&shape.args).enumerate() {
        let mut values = Vec::new();
        for (column, arg) in arguments.iter().enumerate() {
            let first = ast.nodes.len();
            let value = match arg {
                Arg::Constant(a) => a.emit(ast),
                Arg::Column(col) => column_value(ast, &shape.codec, *col, value_vars, packed),
                Arg::Lookup(col, entries) => {
                    let (symbol, byte_bias) = dictionaries[entries];
                    let table = name(ast, symbol);
                    let index = column_value(ast, &shape.codec, *col, value_vars, packed);
                    if let Some(bias) = byte_bias {
                        let byte = ast.push(Node::Call(table, vec![index], Some("byte".into())));
                        offset(ast, byte, bias)
                    } else {
                        ast.push(Node::Index(table, index, false))
                    }
                }
                Arg::Affine(col, scale, bias) => {
                    let v = column_value(ast, &shape.codec, *col, value_vars, packed);
                    predictors::affine(ast, v, *scale, *bias)
                }
                Arg::Series(series) => {
                    let i = name(ast, index);
                    let width = shape.codec.record_width(shape.columns);
                    let mut row = if zero_counter || width > 1 {
                        i
                    } else {
                        offset(ast, i, -1)
                    };
                    // i = row * width + 1, so floor(i/width) == row
                    // whenever width > 1; no subtract-one is required.
                    if width > 1 {
                        let n = num(ast, width as i64);
                        row = ast.push(Node::Bin("//".into(), row, n));
                    }
                    series.emit(ast, row)
                }
            };
            let value = if shape.repeats > 1 && shape.translations[slot][column] != 0 {
                let tile = name(ast, tile_index);
                let shift = predictors::affine(ast, tile, shape.translations[slot][column], 0);
                ast.push(Node::Bin("+".into(), value, shift))
            } else {
                value
            };
            if let Some(t) = trace.as_mut() {
                t.argument(first..ast.nodes.len(), slot, column);
            }
            values.push(value);
        }
        let f = name(ast, functions[*which]);
        let call = ast.push(Node::Call(f, values, None));
        let statement = ast.push(Node::Callstat(call));
        if let Some(t) = trace.as_mut() {
            t.replay(f, call, statement, slot);
        }
        body.push(statement);
    }
    let body = ast.push(Node::Block(body));
    let first = num(ast, if zero_counter { 0 } else { 1 });
    let d = name(ast, data);
    let last = if let Codec::Rice { count, .. } = &shape.codec {
        num(ast, *count as i64)
    } else if shape.codec == Codec::Counter {
        if zero_counter {
            offset(ast, d, -1)
        } else {
            d
        }
    } else {
        ast.push(Node::Un("#".into(), d))
    };
    let width = shape.codec.record_width(shape.columns);
    let step = (width > 1).then(|| num(ast, width as i64));
    let for_ = ast.push(Node::Fornum(index, first, last, step, body));
    prefix.push(for_);
    let mut block = ast.push(Node::Block(prefix));
    if shape.repeats > 1 {
        let first = num(ast, 0);
        let last = num(ast, shape.repeats as i64 - 1);
        let outer = ast.push(Node::Fornum(tile_index, first, last, None, block));
        block = ast.push(Node::Block(vec![outer]));
    }
    let mut params = functions;
    params.push(data);
    let f = ast.push(Node::Function(params, false, block));
    ast.push(Node::Localfunc(symbol, f))
}
pub(super) fn helper_cost(shape: &Shape) -> usize {
    let mut ast = Ast::new();
    let symbol = ast.strings.intern("helper");
    let declaration = emit_helper(&mut ast, shape, symbol, 0);
    let root = ast.push(Node::Block(vec![declaration]));
    let renamed = scope_rename_fast(&ast, root);
    measure_size(&renamed.ast, renamed.root)
}
