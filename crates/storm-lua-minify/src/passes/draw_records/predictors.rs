//! Exact finite integer-column predictors. No floating-point fitting: every
//! proposed expression is checked against every original value before use.
use super::{num, offset};
use storm_lua_syntax::ast::{Ast, Node, NodeId};

/// Memoized Series sizes. See Series::size for the equivalence
/// argument; the cache is cleared when full, which only costs
/// recomputation.
static SIZE_CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<Series, usize>>> =
    std::sync::OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Series {
    Linear {
        start: i64,
        step: i64,
        divisor: usize,
    },
    Cycle {
        bias: i64,
        scale: i64,
        phase: i64,
        step: i64,
        modulus: i64,
        divisor: usize,
    },
}

fn bin(ast: &mut Ast, op: &str, value: NodeId, rhs: i64) -> NodeId {
    let rhs = num(ast, rhs);
    ast.push(Node::Bin(op.into(), value, rhs))
}
pub(super) fn affine(ast: &mut Ast, value: NodeId, scale: i64, bias: i64) -> NodeId {
    let value = match scale {
        1 => value,
        -1 => ast.push(Node::Un("-".into(), value)),
        _ => bin(ast, "*", value, scale),
    };
    offset(ast, value, bias)
}
impl Series {
    pub(super) fn emit(&self, ast: &mut Ast, row: NodeId) -> NodeId {
        match *self {
            Self::Linear {
                start,
                step,
                divisor,
            } => {
                let q = if divisor == 1 {
                    row
                } else {
                    bin(ast, "//", row, divisor as i64)
                };
                affine(ast, q, step, start)
            }
            Self::Cycle {
                bias,
                scale,
                phase,
                step,
                modulus,
                divisor,
            } => {
                let q = if divisor == 1 {
                    row
                } else {
                    bin(ast, "//", row, divisor as i64)
                };
                let q = affine(ast, q, step, phase);
                let q = bin(ast, "%", q, modulus);
                affine(ast, q, scale, bias)
            }
        }
    }
    fn value(&self, row: usize) -> Option<i64> {
        match *self {
            Self::Linear {
                start,
                step,
                divisor,
            } => i64::try_from(row / divisor)
                .ok()?
                .checked_mul(step)?
                .checked_add(start),
            Self::Cycle {
                bias,
                scale,
                phase,
                step,
                modulus,
                divisor,
            } => i64::try_from(row / divisor)
                .ok()?
                .checked_mul(step)?
                .checked_add(phase)?
                .rem_euclid(modulus)
                .checked_mul(scale)?
                .checked_add(bias),
        }
    }
    fn size(&self) -> usize {
        let cache =
            SIZE_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
        if let Some(&size) = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(self)
        {
            return size;
        }
        let size = self.size_uncached();
        let mut cache = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cache.len() >= 65536 {
            cache.clear();
        }
        cache.insert(self.clone(), size);
        size
    }

    fn size_uncached(&self) -> usize {
        let mut ast = Ast::new();
        let i = ast.strings.intern("i");
        let row = ast.push(Node::Name(i));
        let value = self.emit(&mut ast, row);
        storm_lua_syntax::size::measure_expr(&ast, value)
    }
}
fn bounded(values: &[i64]) -> bool {
    values
        .iter()
        .all(|&v| (-1_000_000_000..=1_000_000_000).contains(&v))
}
fn gcd(mut a: i64, mut b: i64) -> i64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

pub(super) fn fit(values: &[i64]) -> Option<Series> {
    if values.len() < 4 || !bounded(values) {
        return None;
    }
    let first_change = values.iter().position(|v| *v != values[0])?;
    let divisors = [1, first_change];
    let divisor_count = if first_change > 1 && first_change <= 64 {
        2
    } else {
        1
    };
    let mut best = None;
    let mut best_size = usize::MAX;
    for &divisor in &divisors[..divisor_count] {
        if !values.chunks(divisor).all(|c| c.iter().all(|v| *v == c[0])) {
            continue;
        }
        let steps = values.iter().step_by(divisor).copied();
        let mut pairs = steps.clone();
        let first = pairs.next()?;
        let Some(second) = pairs.next() else { continue };
        let initial_delta = second - first;
        let mut other_delta = None;
        let mut previous = second;
        // Both a linear series and an affine modular series have at most two
        // adjacent differences. This is a necessary condition, not a heuristic:
        // a third difference cannot match any candidate the old fit considered.
        if pairs.any(|value| {
            let delta = value - previous;
            previous = value;
            if delta == initial_delta {
                return false;
            }
            match other_delta {
                None => {
                    other_delta = Some(delta);
                    false
                }
                Some(other) => delta != other,
            }
        }) {
            continue;
        }
        let mut consider = |candidate: Series| {
            if values
                .iter()
                .enumerate()
                .all(|(r, &v)| candidate.value(r) == Some(v))
            {
                let size = candidate.size();
                if size < best_size {
                    best_size = size;
                    best = Some(candidate);
                }
            }
        };
        consider(Series::Linear {
            start: first,
            step: initial_delta,
            divisor,
        });
        let min = steps.clone().min()?;
        let max = steps.clone().max()?;
        let scale = steps.fold(0, |g, v| gcd(g, v - min));
        if scale != 0 {
            let modulus = (max - min) / scale + 1;
            let phase = (first - min) / scale;
            let step = (initial_delta / scale).rem_euclid(modulus);
            for step in [step, step - modulus] {
                consider(Series::Cycle {
                    bias: min,
                    scale,
                    phase,
                    step,
                    modulus,
                    divisor,
                });
            }
        }
    }
    best
}

/// Fit target = scale * source + bias using exact checked integer arithmetic.
pub(super) fn relation(source: &[i64], target: &[i64]) -> Option<(i64, i64)> {
    if source.len() != target.len() || source.len() < 4 || !bounded(source) || !bounded(target) {
        return None;
    }
    let other = source.iter().position(|v| *v != source[0])?;
    let dx = source[other] - source[0];
    let dy = target[other] - target[0];
    if dy % dx != 0 {
        return None;
    }
    let scale = dy / dx;
    let bias = target[0].checked_sub(scale.checked_mul(source[0])?)?;
    if !(-1_000_000_000..=1_000_000_000).contains(&scale)
        || !(-1_000_000_000..=1_000_000_000).contains(&bias)
    {
        return None;
    }
    source
        .iter()
        .zip(target)
        .all(|(&x, &y)| x.checked_mul(scale).and_then(|v| v.checked_add(bias)) == Some(y))
        .then_some((scale, bias))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    #[test]
    fn finite_integer_predictors_cover_linear_grid_and_modular_series() {
        for n in [4, 15, 64, 257] {
            for divisor in [1, 2, 3, 8, 32] {
                for step in [-17, -1, 1, 13] {
                    let values = (0..n)
                        .map(|i| (i / divisor) as i64 * step - 23)
                        .collect::<Vec<_>>();
                    if values[0] == *values.last().unwrap() {
                        continue;
                    }
                    let model = fit(&values).expect("linear/grid series");
                    assert!(values
                        .iter()
                        .enumerate()
                        .all(|(i, &v)| model.value(i) == Some(v)));
                }
            }
        }
        for modulus in [2, 3, 7, 31, 92] {
            for step in [1, 5, -1] {
                let values = (0..256)
                    .map(|i| (i * step + 3i64).rem_euclid(modulus) * 7 - 100)
                    .collect::<Vec<_>>();
                if values.iter().all(|v| *v == values[0]) {
                    continue;
                }
                let model = fit(&values).expect("modular series");
                assert!(values
                    .iter()
                    .enumerate()
                    .all(|(i, &v)| model.value(i) == Some(v)));
            }
        }
    }
    #[test]
    fn predictors_reject_outliers_and_unsafe_integer_bounds() {
        assert!(fit(&[1, 2, 3, 4, 5, 6, 55]).is_none());
        assert!(fit(&[i64::MIN, 0, 1, i64::MAX]).is_none());
        assert_eq!(relation(&[3, 1, 8, 5], &[16, 20, 6, 12]), Some((-2, 22)));
        assert!(relation(&[3, 1, 8, 5], &[16, 20, 6, 13]).is_none());
        assert!(relation(&[i64::MAX, 1, 2, 3], &[1, 2, 3, 4]).is_none());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod fitting_parity {
    use super::*;
    fn reference_fit(values: &[i64]) -> Option<Series> {
        if values.len() < 4 || !bounded(values) {
            return None;
        }
        let mut best = None;
        let mut best_size = usize::MAX;
        // The first change determines a possible repeated-value block width.
        // Including 1 also admits progressions/cycles with no repeated values.
        let first_change = values.iter().position(|v| *v != values[0])?;
        let divisors = if first_change > 1 && first_change <= 64 {
            vec![1, first_change]
        } else {
            vec![1]
        };
        for divisor in divisors {
            if !values.chunks(divisor).all(|c| c.iter().all(|v| *v == c[0])) {
                continue;
            }
            let steps = values.iter().step_by(divisor).copied().collect::<Vec<_>>();
            if steps.len() < 2 {
                continue;
            }
            let linear = Series::Linear {
                start: steps[0],
                step: steps[1] - steps[0],
                divisor,
            };
            let mut candidates = vec![linear];
            let min = *steps.iter().min()?;
            let max = *steps.iter().max()?;
            let scale = steps.iter().fold(0, |g, v| gcd(g, v - min));
            if scale != 0 {
                let modulus = (max - min) / scale + 1;
                let phase = (steps[0] - min) / scale;
                let step = ((steps[1] - steps[0]) / scale).rem_euclid(modulus);
                // Both congruent signs are exact over this bounded finite input.
                for step in [step, step - modulus] {
                    candidates.push(Series::Cycle {
                        bias: min,
                        scale,
                        phase,
                        step,
                        modulus,
                        divisor,
                    });
                }
            }
            for candidate in candidates {
                if values
                    .iter()
                    .enumerate()
                    .all(|(r, &v)| candidate.value(r) == Some(v))
                {
                    let size = candidate.size();
                    if size < best_size {
                        best_size = size;
                        best = Some(candidate);
                    }
                }
            }
        }
        best
    }

    #[test]
    fn pruned_fitter_matches_reference_on_all_small_sequences() {
        for length in 4..=7 {
            for mut bits in 0..4usize.pow(length) {
                let values = (0..length)
                    .map(|_| {
                        let v = (bits % 4) as i64 - 2;
                        bits /= 4;
                        v
                    })
                    .collect::<Vec<_>>();
                assert_eq!(fit(&values), reference_fit(&values), "{values:?}");
            }
        }
    }
    #[test]
    fn pruned_fitter_preserves_ties_block_divisors_and_boundaries() {
        let mut seed = 0x74657374u64;
        for n in [4, 7, 32, 65, 128, 257] {
            for divisor in [1, 2, 3, 32, 64, 65] {
                for step in [-17i64, -1, 1, 19] {
                    for modulus in [2, 7, 31, 97] {
                        let mut values = (0..n)
                            .map(|i| {
                                ((i / divisor) as i64 * step + 13).rem_euclid(modulus) * 5
                                    - 1_000_000_000
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(fit(&values), reference_fit(&values));
                        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                        values[(seed as usize) % n] += 1;
                        assert_eq!(fit(&values), reference_fit(&values));
                    }
                }
            }
        }
    }
}
