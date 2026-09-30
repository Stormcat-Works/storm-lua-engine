//! Bounded, exact Rice codes for skewed integer drawing columns.
//! The stream stores the original row count; tail padding cannot replay calls.
//! ASCII six-bit digits need no Unicode or binary-source assumptions. Predictors
//! and unary lengths are bounded before encoding; no floating arithmetic occurs.
use super::*;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct Column {
    pub bias: i64,
    pub delta: bool,
    pub low_bits: u8,
}

const MAX_UNARY: i64 = 128;

pub(super) fn payload(columns: &[Vec<i64>], count: usize) -> Option<(Codec, String)> {
    if !(32..=8192).contains(&count) || columns.is_empty() {
        return None;
    }
    let mut specs = Vec::new();
    let mut encoded = Vec::new();
    for column in columns {
        if column.len() != count
            || column
                .iter()
                .any(|v| !(-1_000_000_000..=1_000_000_000).contains(v))
        {
            return None;
        }
        let min = *column.iter().min()?;
        let direct = column.iter().map(|&v| v - min).collect::<Vec<_>>();
        let deltas = std::iter::once(0)
            .chain(column.windows(2).map(|p| {
                let d = p[1] - p[0];
                if d >= 0 {
                    d * 2
                } else {
                    -d * 2 - 1
                }
            }))
            .collect::<Vec<_>>();
        let mut best = None;
        let mut best_cost = usize::MAX;
        for (delta, values) in [(false, direct), (true, deltas)] {
            for k in 0..=24u8 {
                if values.iter().any(|v| v >> k > MAX_UNARY) {
                    continue;
                }
                let bits: usize = values
                    .iter()
                    .map(|v| (v >> k) as usize + 1 + k as usize)
                    .sum();
                // Prefer a simple bias unless delta coding repays its update and
                // seed overhead. Actual whole-program cost still decides later.
                let cost = bits + if delta { 120 } else { 0 };
                if cost < best_cost {
                    best_cost = cost;
                    best = Some((
                        Column {
                            bias: if delta { column[0] } else { min },
                            delta,
                            low_bits: k,
                        },
                        values.clone(),
                    ));
                }
            }
        }
        let (spec, values) = best?;
        specs.push(spec);
        encoded.push(values);
    }
    let mut bytes = String::new();
    let (mut digit, mut used) = (0u8, 0u8);
    let mut bit = |value: bool| {
        digit |= u8::from(value) << used;
        used += 1;
        if used == 6 {
            bytes.push((digit + 35) as char);
            digit = 0;
            used = 0;
        }
    };
    for row in 0..count {
        for (column, spec) in encoded.iter().zip(&specs) {
            let value = column[row];
            for _ in 0..(value >> spec.low_bits) {
                bit(true);
            }
            bit(false);
            for i in (0..spec.low_bits).rev() {
                bit(value >> i & 1 != 0);
            }
        }
    }
    if used != 0 {
        bytes.push((digit + 35) as char);
    }
    Some((
        Codec::Rice {
            columns: specs,
            count,
        },
        bytes,
    ))
}

fn bin(ast: &mut Ast, op: &str, a: NodeId, b: NodeId) -> NodeId {
    ast.push(Node::Bin(op.into(), a, b))
}
fn increment(ast: &mut Ast, symbol: SymbolId) -> NodeId {
    let lhs = name(ast, symbol);
    let old = name(ast, symbol);
    let value = offset(ast, old, 1);
    ast.push(Node::Assign(vec![lhs], vec![value]))
}
fn read_bit(ast: &mut Ast, data: SymbolId, cursor: SymbolId) -> NodeId {
    let d = name(ast, data);
    let at = name(ast, cursor);
    let six = num(ast, 6);
    let byte_index = bin(ast, "//", at, six);
    let byte_index = offset(ast, byte_index, 1);
    let byte = ast.push(Node::Call(d, vec![byte_index], Some("byte".into())));
    let digit = offset(ast, byte, -35);
    let at = name(ast, cursor);
    let six = num(ast, 6);
    let shift = bin(ast, "%", at, six);
    let shifted = bin(ast, ">>", digit, shift);
    let one = num(ast, 1);
    bin(ast, "&", shifted, one)
}

/// Emit a helper-private reader and row decoder. Prefix is executed on every
/// helper invocation, including each translated tile, so no state leaks frames.
#[allow(clippy::too_many_arguments)]
pub(super) fn decoder(
    ast: &mut Ast,
    columns: &[Column],
    data: SymbolId,
    vars: &[SymbolId],
    serial: usize,
    shape: &Shape,
    trace: &mut Option<provenance::Trace>,
) -> (Vec<NodeId>, Vec<NodeId>) {
    let cursor = ast.strings.intern(&format!("__draw_bit_{serial}"));
    let read = ast.strings.intern(&format!("__draw_read_{serial}"));
    let k = ast.strings.intern(&format!("__draw_k_{serial}"));
    let value = ast.strings.intern(&format!("__draw_unary_{serial}"));
    let index = ast.strings.intern(&format!("__draw_low_{serial}"));
    let raw = ast.strings.intern(&format!("__draw_raw_{serial}"));
    let zero = num(ast, 0);
    let mut prefix = vec![ast.push(Node::Local(vec![cursor], vec![zero]))];
    let zero = num(ast, 0);
    let mut reader = vec![ast.push(Node::Local(vec![value], vec![zero]))];
    let bit = read_bit(ast, data, cursor);
    let zero = num(ast, 0);
    let condition = bin(ast, ">", bit, zero);
    let add_value = increment(ast, value);
    let add_cursor = increment(ast, cursor);
    let body = ast.push(Node::Block(vec![add_value, add_cursor]));
    reader.push(ast.push(Node::While(condition, body)));
    reader.push(increment(ast, cursor));
    let old = name(ast, value);
    let two = num(ast, 2);
    let shifted = bin(ast, "*", old, two);
    let bit = read_bit(ast, data, cursor);
    let result = bin(ast, "+", shifted, bit);
    let lhs = name(ast, value);
    let store = ast.push(Node::Assign(vec![lhs], vec![result]));
    let next = increment(ast, cursor);
    let body = ast.push(Node::Block(vec![store, next]));
    let first = num(ast, 1);
    let last = name(ast, k);
    reader.push(ast.push(Node::Fornum(index, first, last, None, body)));
    let result = name(ast, value);
    reader.push(ast.push(Node::Return(vec![result])));
    let body = ast.push(Node::Block(reader));
    let function = ast.push(Node::Function(vec![k], false, body));
    prefix.push(ast.push(Node::Localfunc(read, function)));
    let seeds = columns
        .iter()
        .enumerate()
        .map(|(column, s)| {
            let node = num(ast, if s.delta { s.bias } else { 0 });
            if s.delta {
                if let Some(trace) = trace.as_mut() {
                    trace.column(node as usize..node as usize + 1, shape, column, true);
                }
            }
            node
        })
        .collect();
    prefix.push(ast.push(Node::Local(vars.to_vec(), seeds)));
    let mut body = Vec::new();
    for (i, spec) in columns.iter().enumerate() {
        let f = name(ast, read);
        let k = num(ast, spec.low_bits as i64);
        let decoded = ast.push(Node::Call(f, vec![k], None));
        let result = if spec.delta {
            body.push(ast.push(Node::Local(vec![raw], vec![decoded])));
            let v = name(ast, raw);
            let one = num(ast, 1);
            let half = bin(ast, ">>", v, one);
            let v = name(ast, raw);
            let one = num(ast, 1);
            let sign = bin(ast, "&", v, one);
            let sign = ast.push(Node::Un("-".into(), sign));
            let delta = bin(ast, "~", half, sign);
            let previous = name(ast, vars[i]);
            bin(ast, "+", previous, delta)
        } else {
            let first = ast.nodes.len();
            let result = offset(ast, decoded, spec.bias);
            if let Some(trace) = trace.as_mut() {
                trace.column(first..ast.nodes.len(), shape, i, false);
            }
            result
        };
        let lhs = name(ast, vars[i]);
        body.push(ast.push(Node::Assign(vec![lhs], vec![result])));
    }
    (prefix, body)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    #[test]
    fn bounded_stream_roundtrips_columns_and_padding() {
        for count in [32, 33, 67, 128, 257] {
            let values = vec![
                (0..count).map(|i| i as i64 / 4 - 50).collect(),
                (0..count)
                    .map(|i| if i % 13 == 0 { 31 } else { 1 })
                    .collect(),
                (0..count).map(|i| ((i * 17) % 32) as i64).collect(),
            ];
            let (
                Codec::Rice {
                    columns,
                    count: actual,
                },
                bytes,
            ) = payload(&values, count).unwrap()
            else {
                panic!()
            };
            assert_eq!(actual, count);
            let mut bit_index = 0;
            let mut bit = || {
                let b = ((bytes.as_bytes()[bit_index / 6] - 35) >> (bit_index % 6)) & 1;
                bit_index += 1;
                b as i64
            };
            let mut last = columns.iter().map(|c| c.bias).collect::<Vec<_>>();
            for (row, _) in values[0].iter().enumerate() {
                for (col, spec) in columns.iter().enumerate() {
                    let mut v = 0;
                    while bit() != 0 {
                        v += 1;
                        assert!(v <= MAX_UNARY);
                    }
                    for _ in 0..spec.low_bits {
                        v = v * 2 + bit();
                    }
                    let result = if spec.delta {
                        last[col] + ((v >> 1) ^ -(v & 1))
                    } else {
                        v + spec.bias
                    };
                    assert_eq!(result, values[col][row]);
                    last[col] = result;
                }
            }
            assert!(bytes.len() * 6 - bit_index < 6);
        }
        assert!(payload(&[vec![i64::MIN; 32]], 32).is_none());
        assert!(payload(&[vec![1; 31]], 31).is_none());
    }
}
