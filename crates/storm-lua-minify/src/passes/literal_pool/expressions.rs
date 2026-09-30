//! Final-stage representations of floating literals, never approximate fits.
//!
//! Every numerator/denominator is exactly representable as an f64. A rational
//! candidate is admitted only when IEEE division returns the original bits.
//! Dyadic candidates additionally use an exactly representable power of two;
//! overflow, subnormal powers, integer literals and signed zero are excluded.
//! No host API, user binding or metamethod participates in these expressions.
use std::collections::BTreeMap;
use std::sync::Arc;

use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::numeric::{integer_literal_value, num_val, shortest_exact_hex_float_literal};
use storm_lua_syntax::size::{measure_expr, measure_size};

const MAX_EXACT_INTEGER: u64 = (1 << 53) - 1;

#[derive(Clone, Debug)]
enum ExactReplacement {
    Ratio(Ratio),
    HexFloat(String),
}

impl ExactReplacement {
    fn emit(&self, ast: &mut Ast) -> NodeId {
        match self {
            Self::Ratio(ratio) => ratio.emit(ast),
            Self::HexFloat(token) => ast.push(Node::Num(token.clone().into())),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Denominator {
    Integer(u64),
    PowerOfTwo(u32),
}
#[derive(Clone, Copy, Debug)]
struct Ratio {
    numerator: u64,
    denominator: Denominator,
    negative: bool,
}
impl Ratio {
    fn value(self) -> f64 {
        let divisor = match self.denominator {
            Denominator::Integer(n) => n as f64,
            Denominator::PowerOfTwo(n) => f64::from_bits((u64::from(n) + 1023) << 52),
        };
        let value = self.numerator as f64 / divisor;
        if self.negative {
            -value
        } else {
            value
        }
    }
    fn emit(self, ast: &mut Ast) -> NodeId {
        let numerator = ast.push(Node::Num(self.numerator.to_string().into()));
        let denominator = match self.denominator {
            Denominator::Integer(n) => ast.push(Node::Num(n.to_string().into())),
            Denominator::PowerOfTwo(n) => {
                let two = ast.push(Node::Num("2".into()));
                let exponent = ast.push(Node::Num(n.to_string().into()));
                ast.push(Node::Bin("^".into(), two, exponent))
            }
        };
        let quotient = ast.push(Node::Bin("/".into(), numerator, denominator));
        if self.negative {
            ast.push(Node::Un("-".into(), quotient))
        } else {
            quotient
        }
    }
}

fn candidates(token: &str) -> Vec<Ratio> {
    if integer_literal_value(token).is_some() {
        return Vec::new();
    }
    let value = num_val(token);
    if !value.is_normal() {
        // In particular, do not erase -0.0 or manufacture infinity/NaN.
        return Vec::new();
    }
    let magnitude = value.abs();
    let negative = value.is_sign_negative();
    let mut out = Vec::new();
    let mut admit = |ratio: Ratio| {
        if ratio.numerator != 0 && ratio.value().to_bits() == value.to_bits() {
            out.push(ratio);
        }
    };

    // Strip powers of two from the exact binary significand. This is especially
    // useful for expanded f32 constants, without relying on their provenance.
    let bits = magnitude.to_bits();
    let mut significand = (bits & ((1 << 52) - 1)) | (1 << 52);
    let zeros = significand.trailing_zeros();
    significand >>= zeros;
    let exponent = ((bits >> 52) & 2047) as i32 - 1023 - 52 + zeros as i32;
    if (-1023..=-1).contains(&exponent) {
        admit(Ratio {
            numerator: significand,
            denominator: Denominator::PowerOfTwo((-exponent) as u32),
            negative,
        });
    }

    // Bounded continued fractions find 1/3, 1/60, etc. They propose only;
    // equality of the resulting division bits is the acceptance condition.
    let (mut p0, mut p1, mut q0, mut q1) = (0u64, 1u64, 1u64, 0u64);
    let mut remainder = magnitude;
    for _ in 0..20 {
        let whole = remainder.floor();
        if !whole.is_finite() || whole > MAX_EXACT_INTEGER as f64 {
            break;
        }
        let whole = whole as u64;
        let Some(p) = whole.checked_mul(p1).and_then(|v| v.checked_add(p0)) else {
            break;
        };
        let Some(q) = whole.checked_mul(q1).and_then(|v| v.checked_add(q0)) else {
            break;
        };
        if p > MAX_EXACT_INTEGER || q > MAX_EXACT_INTEGER || q == 0 {
            break;
        }
        admit(Ratio {
            numerator: p,
            denominator: Denominator::Integer(q),
            negative,
        });
        (p0, p1, q0, q1) = (p1, p, q1, q);
        let fraction = remainder - whole as f64;
        if fraction == 0.0 {
            break;
        }
        remainder = 1.0 / fraction;
    }
    out
}

/// Replaces only literal leaves and retains the whole input unless its measured
/// size decreases. The printer accounts for parenthesization in the actual use.
pub(super) fn synthesize(ast: &mut Ast, root: NodeId) -> usize {
    let mut literals = BTreeMap::<Arc<str>, Vec<NodeId>>::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |node| {
        if let Node::Num(token) = ast.node(node) {
            if token.len() >= 7 {
                literals.entry(token.clone()).or_default().push(node);
            }
        }
    });
    let mut replacements = Vec::new();
    let mut scratch = Ast::new();
    for (token, nodes) in literals {
        let mut best_size = measure_expr(ast, nodes[0]);
        let mut best: Option<ExactReplacement> = None;
        if let Some(hex) = shortest_exact_hex_float_literal(&token) {
            let expression = scratch.push(Node::Num(hex.clone().into()));
            let size = measure_expr(&scratch, expression);
            if size < best_size {
                best_size = size;
                best = Some(ExactReplacement::HexFloat(hex));
            }
        }
        for ratio in candidates(&token) {
            let expression = ratio.emit(&mut scratch);
            let size = measure_expr(&scratch, expression);
            if size < best_size {
                best_size = size;
                best = Some(ExactReplacement::Ratio(ratio));
            }
        }
        if let Some(replacement) = best {
            replacements.push((nodes, replacement));
        }
    }
    if replacements.is_empty() {
        return 0;
    }
    let before = measure_size(ast, root);
    let mut trial = ast.clone();
    let mut count = 0;
    for (nodes, replacement) in replacements {
        for node in nodes {
            // Separate expression nodes preserve resolver/source-map ownership.
            let first = trial.nodes.len();
            let emitted = replacement.emit(&mut trial);
            // This emitter creates only the arithmetic representation of this
            // one literal; it does not contain any borrowed source children.
            if ast.nodes.tracks_origins() {
                for new_node in first..trial.nodes.len() {
                    trial.nodes.derive_from(
                        new_node as NodeId,
                        &ast.nodes,
                        node,
                        "exact-literal-expression",
                    );
                }
            }
            trial.nodes[node as usize] = trial.node(emitted).clone();
            trial
                .nodes
                .derive_from(node, &ast.nodes, node, "exact-literal-expression");
            count += 1;
        }
    }
    if measure_size(&trial, root) < before {
        *ast = trial;
        count
    } else {
        0
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn hexadecimal_float_candidates_preserve_bits_and_subtype_marker() {
        assert_eq!(
            shortest_exact_hex_float_literal(".10000000149011612").as_deref(),
            Some("0x.199999a")
        );
        assert_eq!(
            shortest_exact_hex_float_literal("-.6000000238418579").as_deref(),
            Some("-0x.99999a")
        );
        assert_eq!(
            shortest_exact_hex_float_literal("65536.0").as_deref(),
            Some("0x1p16")
        );
        for token in [
            ".10000000149011612",
            "-.6000000238418579",
            "2.7999999474559445e-6",
            ".3333333333333333",
            "1.5",
            "65536.0",
        ] {
            if let Some(candidate) = shortest_exact_hex_float_literal(token) {
                let unsigned = candidate.strip_prefix('-').unwrap_or(&candidate);
                assert!(
                    unsigned.contains('p') || unsigned.contains('.'),
                    "{token}: {candidate}"
                );
                let parsed = num_val(unsigned);
                let parsed = if candidate.starts_with('-') {
                    -parsed
                } else {
                    parsed
                };
                assert_eq!(parsed.to_bits(), num_val(token).to_bits());
            }
        }
        assert!(shortest_exact_hex_float_literal("9223372036854775807").is_none());
        assert!(shortest_exact_hex_float_literal("-0.0").is_none());
    }

    #[test]
    fn candidates_preserve_bits_and_do_not_treat_integers_as_floats() {
        for token in [
            "9223372036854775807",
            "0xffffffffffffffff",
            "-9007199254740993",
            "-0.0",
            "0.0",
            "1e309",
            "5e-324",
        ] {
            assert!(candidates(token).is_empty(), "{token}");
        }
        for token in [
            ".3333333333333333",
            ".016666666666666666",
            ".10000000149011612",
            "-.6000000238418579",
            "2.7999999474559445e-6",
            "2.2250738585072014e-308",
            "1.7976931348623157e308",
        ] {
            for ratio in candidates(token) {
                assert_eq!(
                    ratio.value().to_bits(),
                    num_val(token).to_bits(),
                    "{token}: {ratio:?}"
                );
            }
        }
    }
}
