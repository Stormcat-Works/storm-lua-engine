//! Literal expression/control-flow folding (`passes/literal-folding.ts`).
//!
//! This is intentionally a reusable Phase 4c primitive: literal user-function
//! folding and equivalent trailing-argument omission both depend on the exact
//! same folding semantics.

use crate::pass::PassResult;
use std::collections::HashSet;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, Resolution};

use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::numeric::{
    decode_lua_string, integer_literal_value, num_val, quote_lua, short_num,
};
use storm_lua_syntax::size::measure_expr;

use super::immutable_values::expression_key;

#[derive(Clone, Debug)]
pub(crate) enum Literal {
    Int(i64),
    Num(f64),
    Str(Vec<u8>),
    Bool(bool),
    Nil,
}

#[derive(Clone, Copy)]
pub struct NumericTolerance {
    pub abs: f64,
    pub rel: f64,
}

impl Default for NumericTolerance {
    fn default() -> Self {
        Self {
            abs: 1e-12,
            rel: 1e-12,
        }
    }
}

/// Call nodes that appear as statements: in the AST a call statement is a
/// `Call` wrapped in `Callstat`. Lua allows only calls as expression
/// statements, so folding such a call into a literal (or any non-call
/// expression) would print a bare expression statement that does not
/// reparse. Folding a statement call is therefore undone unless the
/// folded result is itself a call.
fn statement_call_nodes(source: &Ast, root: NodeId) -> HashSet<NodeId> {
    fn walk(source: &Ast, node: NodeId, out: &mut HashSet<NodeId>) {
        if let Node::Callstat(inner) = source.node(node) {
            out.insert(*inner);
        }
        let mut children = Vec::new();
        storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| {
            children.push(child)
        });
        for child in children {
            walk(source, child, out);
        }
    }
    let mut out = HashSet::new();
    walk(source, root, &mut out);
    out
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

/// Shared rewrite codes for the per-node derivations below. A fold derives
/// every node it visits, so allocating a fresh code string per changed node
/// amplified tracking-only pressure; the recorded code is compared by
/// string content, so sharing it changes nothing.
fn fold_code() -> std::sync::Arc<str> {
    static CODE: std::sync::OnceLock<std::sync::Arc<str>> = std::sync::OnceLock::new();
    std::sync::Arc::clone(CODE.get_or_init(|| std::sync::Arc::from("constant-folding")))
}

fn simplify_code() -> std::sync::Arc<str> {
    static CODE: std::sync::OnceLock<std::sync::Arc<str>> = std::sync::OnceLock::new();
    std::sync::Arc::clone(CODE.get_or_init(|| std::sync::Arc::from("control-flow-simplification")))
}

/// Evidence facts that are identical for every node of one fold. The
/// aggressive flag and the tolerance bit patterns do not vary between
/// folded nodes, so formatting them once per fold instead of once per
/// evidence reason removes three string allocations per folded node. The
/// recorded facts are identical; only the allocations are shared.
/// Key for a fold evidence fact value. The key captures exactly what
/// `literal_fact` and the size facts encode, so equal keys always format
/// to equal strings and the recorded reason is unchanged: an integer by
/// value, a float by bits, a string by byte length, a boolean by value,
/// nil by kind, and an expression size by count.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum SharedFactKey {
    Literal(u8, u64),
    Size(usize),
}

pub(crate) struct FoldSharedFacts {
    pub(crate) aggressive: std::sync::Arc<str>,
    pub(crate) abs_bits: std::sync::Arc<str>,
    pub(crate) rel_bits: std::sync::Arc<str>,
    facts: std::cell::RefCell<std::collections::HashMap<SharedFactKey, std::sync::Arc<str>>>,
}

impl FoldSharedFacts {
    pub(crate) fn new(aggressive: bool, tolerance: NumericTolerance) -> Self {
        Self {
            aggressive: std::sync::Arc::from(if aggressive { "true" } else { "false" }),
            abs_bits: std::sync::Arc::from(format!("{:016x}", tolerance.abs.to_bits())),
            rel_bits: std::sync::Arc::from(format!("{:016x}", tolerance.rel.to_bits())),
            facts: std::cell::RefCell::new(std::collections::HashMap::new()),
        }
    }

    /// Return the shared fact text for a literal, formatting it only on
    /// first sight within this fold. Repeated literals (the common case in
    /// dense inputs) then pay no per-node formatting allocation, while the
    /// recorded fact string is identical to `literal_fact`.
    pub(crate) fn literal_fact_shared(&self, value: &Literal) -> std::sync::Arc<str> {
        let key = match value {
            Literal::Int(v) => SharedFactKey::Literal(0, *v as u64),
            Literal::Num(v) => SharedFactKey::Literal(1, v.to_bits()),
            Literal::Str(v) => SharedFactKey::Literal(2, v.len() as u64),
            Literal::Bool(v) => SharedFactKey::Literal(3, *v as u64),
            Literal::Nil => SharedFactKey::Literal(4, 0),
        };
        self.cached_fact(key, || literal_fact(value))
    }

    /// Return the shared fact text for an expression size, formatting it
    /// only on first sight within this fold.
    pub(crate) fn size_fact_shared(&self, size: usize) -> std::sync::Arc<str> {
        self.cached_fact(SharedFactKey::Size(size), || size.to_string())
    }

    fn cached_fact(
        &self,
        key: SharedFactKey,
        format: impl FnOnce() -> String,
    ) -> std::sync::Arc<str> {
        let mut facts = self.facts.borrow_mut();
        if let Some(shared) = facts.get(&key) {
            return std::sync::Arc::clone(shared);
        }
        let shared: std::sync::Arc<str> = std::sync::Arc::from(format());
        facts.insert(key, std::sync::Arc::clone(&shared));
        shared
    }
}

/// Shared "nonliteral" emitted-fact text, built once per process instead
/// of once per folded node that emits a nonliteral candidate.
pub(crate) fn nonliteral_fact() -> std::sync::Arc<str> {
    static NONLITERAL: std::sync::OnceLock<std::sync::Arc<str>> = std::sync::OnceLock::new();
    std::sync::Arc::clone(NONLITERAL.get_or_init(|| std::sync::Arc::from("nonliteral")))
}

pub(crate) fn literal_value(ast: &Ast, node: NodeId) -> Option<Literal> {
    match ast.node(node) {
        Node::Num(v) => integer_literal_value(v)
            .map(Literal::Int)
            .or_else(|| Some(Literal::Num(num_val(v)))),
        Node::Str(v) => storm_lua_syntax::source_tools::string_bytes(v)
            .ok()
            .map(Literal::Str),
        Node::Bool(v) => Some(Literal::Bool(*v)),
        Node::Nil => Some(Literal::Nil),
        _ => None,
    }
}

fn literal_fact(value: &Literal) -> String {
    match value {
        Literal::Int(v) => format!("i64:{v}"),
        Literal::Num(v) => format!("f64-bits:{:016x}", v.to_bits()),
        Literal::Str(v) => format!("string-byte-length:{}", v.len()),
        Literal::Bool(v) => format!("boolean:{v}"),
        Literal::Nil => "nil".into(),
    }
}

fn literal_node(ast: &mut Ast, value: Literal) -> Option<NodeId> {
    match value {
        Literal::Nil => Some(ast.push(Node::Nil)),
        Literal::Bool(v) => Some(ast.push(Node::Bool(v))),
        Literal::Int(v) => Some(ast.push(Node::Num(if v == i64::MIN {
            "0x8000000000000000".into()
        } else {
            v.to_string().into()
        }))),
        Literal::Num(v) if v.is_finite() => {
            let mut text = short_num(v);
            if integer_literal_value(&text).is_some() {
                text.push_str(".0");
            }
            Some(ast.push(Node::Num(text.into())))
        }
        // JS `shortNum(NaN)` is not a valid Lua literal. The TS implementation
        // constructs it but the parser/printer path does not use such folds in
        // the accepted corpus; keep the fold disabled here rather than emit an
        // invalid numeric token.
        Literal::Num(_) => None,
        Literal::Str(v) => Some(ast.push(Node::Str(quote_lua(&v).into()))),
    }
}

pub(crate) fn aggressive_literal_node(
    ast: &mut Ast,
    value: Literal,
    aggressive: bool,
    tolerance: NumericTolerance,
) -> Option<NodeId> {
    let _ = (aggressive, tolerance);
    // This constructor used to spell a non-integral fold result with a
    // shorter within-tolerance approximation in aggressive mode. That
    // changed the value the folded literal denotes, with no analysis
    // of where the literal flows: inside a comparison, a discretizing
    // call (math.ceil), a table index, or any other exact route, the
    // approximation is an observable behavior change (candidates P/Q,
    // w9 cycle 66). Fold results are now always spelled exactly;
    // within-tolerance shortening is the numeric-literals pass's job,
    // which performs the exact-route analysis over the whole tree.
    literal_node(ast, value)
}

fn lua_truth(value: &Literal) -> bool {
    !matches!(value, Literal::Nil | Literal::Bool(false))
}

fn number(v: &Literal) -> Option<f64> {
    match v {
        Literal::Int(v) => Some(*v as f64),
        Literal::Num(v) => Some(*v),
        // Preserve Lua's coercion/error path for other types.
        _ => None,
    }
}

fn integer(v: &Literal) -> Option<i64> {
    match v {
        Literal::Int(v) => Some(*v),
        Literal::Num(v)
            if v.fract() == 0.0 && *v >= -9223372036854775808.0 && *v < 9223372036854775808.0 =>
        {
            Some(*v as i64)
        }
        _ => None,
    }
}

pub(crate) fn eval_unary(op: &str, a: &Literal) -> Option<Literal> {
    match op {
        "not" => Some(Literal::Bool(!lua_truth(a))),
        "-" => Some(match a {
            Literal::Int(v) => Literal::Int(v.wrapping_neg()),
            _ => Literal::Num(-number(a)?),
        }),
        "~" => Some(Literal::Int(!integer(a)?)),
        "#" => match a {
            Literal::Str(v) => Some(Literal::Int(v.len() as i64)),
            _ => None,
        },
        _ => None,
    }
}

fn int_float_cmp(integer: i64, float: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    if float.is_nan() {
        return None;
    }
    if float >= 9223372036854775808.0 {
        return Some(Ordering::Less);
    }
    if float < -9223372036854775808.0 {
        return Some(Ordering::Greater);
    }
    let truncated = float as i64;
    Some(match integer.cmp(&truncated) {
        Ordering::Equal if float.fract() > 0.0 => Ordering::Less,
        Ordering::Equal if float.fract() < 0.0 => Ordering::Greater,
        order => order,
    })
}

fn value_cmp(a: &Literal, b: &Literal) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Literal::Str(a), Literal::Str(b)) => Some(a.cmp(b)),
        (Literal::Int(a), Literal::Int(b)) => Some(a.cmp(b)),
        (Literal::Int(a), Literal::Num(b)) => int_float_cmp(*a, *b),
        (Literal::Num(a), Literal::Int(b)) => {
            int_float_cmp(*b, *a).map(std::cmp::Ordering::reverse)
        }
        (Literal::Num(a), Literal::Num(b)) => a.partial_cmp(b),
        _ => None,
    }
}

fn strict_equal(a: &Literal, b: &Literal) -> bool {
    match (a, b) {
        (Literal::Nil, Literal::Nil) => true,
        (Literal::Bool(a), Literal::Bool(b)) => a == b,
        _ => value_cmp(a, b) == Some(std::cmp::Ordering::Equal),
    }
}

fn relational(op: &str, a: &Literal, b: &Literal) -> Option<bool> {
    use std::cmp::Ordering::{Equal, Greater, Less};
    if !matches!((a, b), (Literal::Str(_), Literal::Str(_)))
        && (number(a).is_none() || number(b).is_none())
    {
        return None;
    }
    let order = value_cmp(a, b);
    Some(match op {
        "<" => order == Some(Less),
        ">" => order == Some(Greater),
        "<=" => matches!(order, Some(Less | Equal)),
        ">=" => matches!(order, Some(Greater | Equal)),
        _ => return None,
    })
}

fn shift(value: i64, count: i64, left: bool) -> i64 {
    if count.unsigned_abs() >= 64 {
        return 0;
    }
    if left != (count < 0) {
        value.wrapping_shl(count.unsigned_abs() as u32)
    } else {
        ((value as u64) >> count.unsigned_abs()) as i64
    }
}

pub(crate) fn eval_binary(op: &str, a: &Literal, b: &Literal) -> Option<Literal> {
    match op {
        "+" | "-" | "*" => Some(match (a, b) {
            (Literal::Int(x), Literal::Int(y)) => Literal::Int(match op {
                "+" => x.wrapping_add(*y),
                "-" => x.wrapping_sub(*y),
                _ => x.wrapping_mul(*y),
            }),
            _ => Literal::Num(match op {
                "+" => number(a)? + number(b)?,
                "-" => number(a)? - number(b)?,
                _ => number(a)? * number(b)?,
            }),
        }),
        "/" => Some(Literal::Num(number(a)? / number(b)?)),
        "//" | "%" => {
            if let (Literal::Int(x), Literal::Int(y)) = (a, b) {
                if *y == 0 {
                    return None;
                }
                // Lua explicitly wraps MIN/-1, and floor division follows the divisor's sign.
                let mut quotient = x.wrapping_div(*y);
                let mut remainder = x.wrapping_rem(*y);
                if remainder != 0 && (remainder < 0) != (*y < 0) {
                    quotient = quotient.wrapping_sub(1);
                    remainder = remainder.wrapping_add(*y);
                }
                return Some(Literal::Int(if op == "//" { quotient } else { remainder }));
            }
            let (x, y) = (number(a)?, number(b)?);
            let mut remainder = x % y;
            if remainder != 0.0 && (remainder < 0.0) != (y < 0.0) {
                remainder += y;
            }
            Some(Literal::Num(if op == "//" {
                (x / y).floor()
            } else {
                remainder
            }))
        }
        "^" => Some(Literal::Num(number(a)?.powf(number(b)?))),
        ".." => {
            // Float tostring depends on Lua's formatting/locale contract. Keep
            // that runtime conversion, but fold exact byte strings and integers.
            fn bytes(value: &Literal) -> Option<Vec<u8>> {
                match value {
                    Literal::Str(v) => Some(v.clone()),
                    Literal::Int(v) => Some(v.to_string().into_bytes()),
                    _ => None,
                }
            }
            let mut result = bytes(a)?;
            result.extend(bytes(b)?);
            Some(Literal::Str(result))
        }
        "==" => Some(Literal::Bool(strict_equal(a, b))),
        "~=" => Some(Literal::Bool(!strict_equal(a, b))),
        "<" | ">" | "<=" | ">=" => Some(Literal::Bool(relational(op, a, b)?)),
        "&" => Some(Literal::Int(integer(a)? & integer(b)?)),
        "|" => Some(Literal::Int(integer(a)? | integer(b)?)),
        "~" => Some(Literal::Int(integer(a)? ^ integer(b)?)),
        "<<" | ">>" => Some(Literal::Int(shift(integer(a)?, integer(b)?, op == "<<"))),
        "and" => Some(if lua_truth(a) { b.clone() } else { a.clone() }),
        "or" => Some(if lua_truth(a) { a.clone() } else { b.clone() }),
        _ => None,
    }
}

fn eval_math(builtin: &str, values: &[Literal]) -> Option<Literal> {
    match builtin {
        "math.min" | "math.max" => {
            let mut best = values.first()?.clone();
            for value in &values[1..] {
                if relational(if builtin == "math.min" { "<" } else { ">" }, value, &best)? {
                    best = value.clone();
                }
            }
            Some(best)
        }
        "math.abs" if values.len() == 1 => Some(match &values[0] {
            Literal::Int(v) => Literal::Int(v.wrapping_abs()),
            value => Literal::Num(number(value)?.abs()),
        }),
        "math.floor" if values.len() == 1 => {
            if let Literal::Int(_) = values[0] {
                return Some(values[0].clone());
            }
            let value = Literal::Num(number(&values[0])?.floor());
            Some(integer(&value).map(Literal::Int).unwrap_or(value))
        }
        _ => {
            let x = number(values.first()?)?;
            let result = match builtin {
                "math.sqrt" if values.len() == 1 => x.sqrt(),
                "math.sin" if values.len() == 1 => x.sin(),
                "math.cos" if values.len() == 1 => x.cos(),
                "math.tan" if values.len() == 1 => x.tan(),
                "math.atan" if values.len() == 1 => x.atan(),
                "math.atan" if values.len() == 2 => x.atan2(number(&values[1])?),
                _ => return None,
            };
            Some(Literal::Num(result))
        }
    }
}

fn without_parens(ast: &Ast, mut node: NodeId) -> NodeId {
    while let Node::Paren(inner) = ast.node(node) {
        node = *inner;
    }
    node
}

fn is_zero(ast: &Ast, node: NodeId) -> bool {
    matches!(ast.node(node), Node::Num(v) if num_val(v) == 0.0)
}

fn is_one(ast: &Ast, node: NodeId) -> bool {
    matches!(ast.node(node), Node::Num(v) if num_val(v) == 1.0)
}

/// The aggressive identities `x * 0 -> 0`, `0 * x -> 0` and `x - x -> 0`
/// are only valid when `x` is a finite number: for infinite or NaN `x`
/// the product / difference is NaN, and for a non-numeric `x` the
/// operation must raise a runtime error instead of yielding 0. A name or
/// a general expression carries no such proof, so only numeric literals
/// with a finite value qualify here (literal-literal cases are normally
/// already handled by generic evaluation above; this guard keeps the
/// identities from firing where evaluation was refused, e.g. because
/// the result would be NaN or infinite).
fn finite_number_literal(ast: &Ast, node: NodeId) -> bool {
    match literal_value(ast, node) {
        Some(Literal::Int(_)) => true,
        Some(Literal::Num(v)) => v.is_finite(),
        _ => false,
    }
}

fn semantic_same(ast: &Ast, resolution: &Resolution, a: NodeId, b: NodeId) -> bool {
    expression_key(ast, resolution, a) == expression_key(ast, resolution, b)
}

/// Both forms evaluate the condition once, adjust it to one value, and return
/// a boolean (including for nil, zero, strings, tables, and functions). Lua
/// truthiness and `not` cannot invoke metamethods. The literal branches have no
/// effects, so neither evaluation order nor exception behavior changes.
/// Do not generalize this to `x and false or true` or non-boolean branches.
fn boolean_coercion(ast: &mut Ast, node: NodeId) -> Option<NodeId> {
    let Node::Bin(op, left, right) = ast.node(node) else {
        return None;
    };
    if op != "or" || !matches!(ast.node(without_parens(ast, *right)), Node::Bool(false)) {
        return None;
    }
    let left = without_parens(ast, *left);
    let Node::Bin(and_op, condition, yes) = ast.node(left) else {
        return None;
    };
    if and_op != "and" || !matches!(ast.node(without_parens(ast, *yes)), Node::Bool(true)) {
        return None;
    }
    let condition = *condition;
    let origin = ast.nodes.capture_origin(node);
    let once = ast.push(Node::Un("not".into(), condition));
    ast.nodes
        .finish_rewrite(once, origin.clone(), "boolean-coercion");
    let twice = ast.push(Node::Un("not".into(), once));
    ast.nodes.finish_rewrite(twice, origin, "boolean-coercion");
    Some(twice)
}

// These operands, not an enclosing statement, determine the computed value.
// A missing input origin therefore remains missing on the result.
fn computed_origin(ast: &mut Ast, result: NodeId, inputs: &[NodeId], reason: &str) {
    super::origins::within(ast, result, inputs, reason);
}

fn positive_term(ast: &mut Ast, node: NodeId) -> Option<NodeId> {
    let value = without_parens(ast, node);
    match ast.node(value).clone() {
        Node::Num(v) if num_val(&v) < 0.0 => {
            let result = ast.push(Node::Num(short_num(-num_val(&v)).into()));
            computed_origin(ast, result, &[value], "positive-term-literal");
            Some(result)
        }
        Node::Un(op, expression) if op == "-" => Some(expression),
        Node::Bin(op, left, right) if op == "/" => {
            let numerator = positive_term(ast, left)?;
            let result = ast.push(Node::Bin("/".into(), numerator, right));
            computed_origin(ast, result, &[value], "positive-term-division");
            Some(result)
        }
        Node::Bin(op, left, right) if op == "*" => {
            for (literal, is_left) in [
                (without_parens(ast, left), true),
                (without_parens(ast, right), false),
            ] {
                if let Node::Num(v) = ast.node(literal).clone() {
                    if num_val(&v) < 0.0 {
                        let n = ast.push(Node::Num(short_num(-num_val(&v)).into()));
                        computed_origin(ast, n, &[literal], "positive-term-literal");
                        let result = ast.push(Node::Bin(
                            "*".into(),
                            if is_left { n } else { left },
                            if is_left { right } else { n },
                        ));
                        computed_origin(ast, result, &[value], "positive-term-product");
                        return Some(result);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn gcd(mut a: i64, mut b: i64) -> i64 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        (a, b) = (b, a % b);
    }
    if a == 0 {
        1
    } else {
        a
    }
}

fn divide_by_literal(ast: &mut Ast, value: NodeId, divisor: NodeId) -> Option<NodeId> {
    let Node::Num(d) = ast.node(divisor).clone() else {
        return None;
    };
    let denominator = num_val(&d);
    if denominator.fract() != 0.0 || denominator == 0.0 || denominator.abs() > i64::MAX as f64 {
        return None;
    }
    // Rewriting `value * (1 / d)` as `value / d` is only value-identical
    // when the reciprocal 1/d is exact, i.e. when d is a power of two:
    // otherwise the multiplication by the rounded reciprocal and the
    // division can differ by 1 ulp (and that difference is observable,
    // e.g. through an equality comparison against the division form).
    if denominator.abs().log2().fract() != 0.0 {
        return None;
    }
    if let Node::Num(n) = ast.node(value).clone() {
        let numerator = num_val(&n);
        if numerator.fract() == 0.0 && numerator.abs() <= i64::MAX as f64 {
            let factor = gcd(numerator as i64, denominator as i64) as f64;
            let rn = numerator / factor;
            let rd = denominator / factor;
            if rd == 1.0 {
                let result = ast.push(Node::Num(short_num(rn).into()));
                computed_origin(ast, result, &[value, divisor], "reduced-literal-division");
                return Some(result);
            }
            let l = ast.push(Node::Num(short_num(rn).into()));
            let r = ast.push(Node::Num(short_num(rd).into()));
            for result in [l, r] {
                computed_origin(ast, result, &[value, divisor], "reduced-literal-division");
            }
            let result = ast.push(Node::Bin("/".into(), l, r));
            computed_origin(ast, result, &[value, divisor], "reduced-literal-division");
            return Some(result);
        }
    }
    let result = ast.push(Node::Bin("/".into(), value, divisor));
    computed_origin(ast, result, &[value, divisor], "literal-division");
    Some(result)
}

/// The modulo form of a bit test only agrees with `value & mask` when
/// `value` has an integer representation: a fractional float, an
/// infinite or NaN value, or an integral float outside the integer
/// range makes the `&` itself raise a runtime error, while the modulo
/// form would silently evaluate. Only literal integers, integral
/// float literals inside the integer range, and bitwise expressions
/// (whose result is an integer whenever they evaluate at all) carry
/// that proof; a name or a general expression does not.
fn provably_integer_valued(ast: &Ast, node: NodeId) -> bool {
    let node = without_parens(ast, node);
    match literal_value(ast, node) {
        Some(Literal::Int(_)) => true,
        Some(Literal::Num(v)) => {
            v.is_finite()
                && v.fract() == 0.0
                && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&v)
        }
        _ => match ast.node(node) {
            Node::Bin(op, ..) => matches!(op.as_str(), "&" | "|" | "~" | "<<" | ">>"),
            Node::Un(op, _) => op.as_str() == "~",
            _ => false,
        },
    }
}

fn modulo_bit_test(ast: &mut Ast, node: NodeId) -> Option<NodeId> {
    let Node::Bin(op, left, right) = ast.node(node).clone() else {
        return None;
    };
    if !matches!(op.as_str(), "==" | "~=") || !is_zero(ast, right) {
        return None;
    }
    let bit = without_parens(ast, left);
    let Node::Bin(bit_op, value, mask_node) = ast.node(bit).clone() else {
        return None;
    };
    if bit_op != "&" || !provably_integer_valued(ast, value) {
        return None;
    }
    let Node::Num(mask_text) = ast.node(mask_node).clone() else {
        return None;
    };
    let mask = num_val(&mask_text);
    if !mask.is_finite()
        || mask.fract() != 0.0
        || mask <= 0.0
        || mask > 9_007_199_254_740_991.0
        || mask.log2().fract() != 0.0
    {
        return None;
    }
    let two_mask = ast.push(Node::Num(short_num(mask * 2.0).into()));
    computed_origin(ast, two_mask, &[mask_node], "bit-test-modulus");
    let modulo = ast.push(Node::Bin("%".into(), value, two_mask));
    computed_origin(ast, modulo, &[bit], "bit-test-modulo");
    let boundary = ast.push(Node::Num(
        short_num(if op == "~=" { mask - 1.0 } else { mask }).into(),
    ));
    computed_origin(ast, boundary, &[mask_node, right], "bit-test-boundary");
    let result = ast.push(Node::Bin(
        if op == "~=" { ">" } else { "<" }.into(),
        modulo,
        boundary,
    ));
    computed_origin(ast, result, &[node], "bit-test-comparison");
    Some(result)
}

fn original_child_movable(
    analyzer: &EffectAnalyzer<'_>,
    source: &Ast,
    target: &Ast,
    original: NodeId,
    transformed: NodeId,
) -> bool {
    if matches!(
        target.node(transformed),
        Node::Nil | Node::Bool(_) | Node::Num(_) | Node::Str(_)
    ) {
        true
    } else if original < source.nodes.len() as NodeId {
        analyzer.is_movable(original)
    } else {
        false
    }
}

#[allow(clippy::too_many_arguments)]
fn fold_node(
    source: &Ast,
    target: &mut Ast,
    resolution: &Resolution,
    statement_calls: &HashSet<NodeId>,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    aggressive: bool,
    tolerance: NumericTolerance,
    fold_nonportable_math: bool,
    shared: &FoldSharedFacts,
) {
    let mut evidence = None;
    fold_node_inner(
        source,
        target,
        resolution,
        statement_calls,
        analyzer,
        node,
        aggressive,
        tolerance,
        fold_nonportable_math,
        shared,
        &mut evidence,
    );
    // A statement call folded into a non-call would print as a bare
    // expression statement and fail to reparse, whatever inner step
    // produced it (builtin math folding, string.format folding, or any
    // other). Restore the original call; its already-folded arguments
    // stay folded, so only the invalid fold itself is undone.
    if statement_calls.contains(&node) && !matches!(target.node(node), Node::Call(..)) {
        target.nodes[node as usize] = source.node(node).clone();
    }
    // Derive from the immutable source instead of capturing before the
    // traversal and finishing after it. The inner traversal already rewrites
    // this node, restoring the shared origin slot when the value is unchanged,
    // so unchanged nodes take the shared-slot fast path without
    // cloning the node value. Changed nodes fall through to the same
    // capture + finish_rewrite path with the same "constant-folding" reason.
    // This is only valid because `source` is never mutated during the fold;
    // the single-arena exact fold below keeps its capture. The code string
    // is shared across all nodes of the fold.
    target
        .nodes
        .derive_from_with_code(node, &source.nodes, node, &fold_code());
    // A collapsed value no longer emits its operands. Do not replace missing
    // operand attribution with the old enclosing expression's known anchor.
    if target.nodes.tracks_origins()
        && source.node(node) != target.node(node)
        && matches!(
            target.node(node),
            Node::Name(_) | Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
        )
        && !super::origins::known(source, node)
    {
        let value = target.node(node).clone();
        target.nodes[node as usize] = value;
    }
    if let Some(reason) = evidence {
        target.nodes.explain(node, reason);
    }
}

#[allow(clippy::too_many_arguments)]
fn fold_node_inner(
    source: &Ast,
    target: &mut Ast,
    resolution: &Resolution,
    statement_calls: &HashSet<NodeId>,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    aggressive: bool,
    tolerance: NumericTolerance,
    fold_nonportable_math: bool,
    shared: &FoldSharedFacts,
    evidence: &mut Option<storm_lua_syntax::explanation::OptimizationReason>,
) {
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        fold_node(
            source,
            target,
            resolution,
            statement_calls,
            analyzer,
            child,
            aggressive,
            tolerance,
            fold_nonportable_math,
            shared,
        );
        child
    });
    target
        .nodes
        .rewrite(node, mapped, "literal-folding-traversal");

    if let Node::Num(v) = target.node(node).clone() {
        // render_num borrows the input for already-canonical literals; writing
        // the same text back would needlessly clear this node's origin slot
        // (and allocate a duplicate string) before finish_rewrite restores it.
        let rendered = storm_lua_syntax::print::render_num(&v);
        if *rendered != *v {
            target.nodes[node as usize] = Node::Num(rendered.into_owned().into());
        }
    }

    if let Node::Paren(inner) = target.node(node).clone() {
        if matches!(
            target.node(inner),
            Node::Name(_)
                | Node::Num(_)
                | Node::Str(_)
                | Node::Bool(_)
                | Node::Nil
                | Node::Index(..)
                // A parenthesized call has one result in Lua; dropping it can
                // expose all results when used as an assignment/argument.
                | Node::Table(_)
        ) {
            target.nodes[node as usize] = target.node(inner).clone();
            return;
        }
    }

    // string.format("%0Nd", integer)
    if let Node::Call(function, args, _) = target.node(node).clone() {
        let original_function = match original {
            Node::Call(f, _, _) => f,
            _ => function,
        };
        if analyzer
            .resolve_builtin_reference(original_function)
            .as_deref()
            == Some("string.format")
            && args.len() == 2
        {
            if let (Node::Str(format), Node::Num(value)) =
                (target.node(args[0]).clone(), target.node(args[1]).clone())
            {
                let Some(decoded) = decode_lua_string(&format) else {
                    return;
                };
                if decoded.starts_with("%0") && decoded.ends_with('d') {
                    let width_text = &decoded[2..decoded.len() - 1];
                    if !width_text.is_empty()
                        // Lua 5.3 permits at most two width digits.
                        && width_text.len() <= 2
                        && !width_text.starts_with('0')
                        && width_text.chars().all(|c| c.is_ascii_digit())
                    {
                        if let Ok(width) = width_text.parse::<usize>() {
                            let number = num_val(&value);
                            if number.fract() == 0.0 && number.abs() <= 9_007_199_254_740_991.0 {
                                let integer = number as i64;
                                let sign = if integer < 0 { "-" } else { "" };
                                let abs = integer.unsigned_abs().to_string();
                                let zeros = width.saturating_sub(sign.len() + abs.len());
                                if sign.len() + abs.len() + zeros + 2 >= measure_expr(target, node)
                                {
                                    return;
                                }
                                let formatted = format!("{sign}{}{abs}", "0".repeat(zeros));
                                let candidate =
                                    target.push(Node::Str(quote_lua(&formatted).into()));
                                if measure_expr(target, candidate) < measure_expr(target, node) {
                                    target.nodes[node as usize] = target.node(candidate).clone();
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if let Node::Un(op, expression) = target.node(node).clone() {
        if let Some(operand_value) = literal_value(target, expression) {
            let operand_fact = target
                .nodes
                .tracks_origins()
                .then(|| shared.literal_fact_shared(&operand_value));
            if let Some(evaluated_value) = eval_unary(&op, &operand_value) {
                let evaluated_fact = target
                    .nodes
                    .tracks_origins()
                    .then(|| shared.literal_fact_shared(&evaluated_value));
                if let Some(candidate) =
                    aggressive_literal_node(target, evaluated_value, aggressive, tolerance)
                {
                    if let (Some(operand_fact), Some(evaluated_fact)) =
                        (operand_fact, evaluated_fact)
                    {
                        *evidence = Some(
                            storm_lua_syntax::explanation::OptimizationReason::decision_shared(
                                "constant-folding",
                                "literal-unary-evaluation",
                                [
                                    (
                                        "operator",
                                        storm_lua_syntax::explanation::DecisionFactValue::Borrowed(
                                            op.as_str(),
                                        ),
                                    ),
                                    (
                                        "operand",
                                        storm_lua_syntax::explanation::DecisionFactValue::Shared(
                                            operand_fact,
                                        ),
                                    ),
                                    (
                                        "evaluated",
                                        storm_lua_syntax::explanation::DecisionFactValue::Shared(
                                            evaluated_fact,
                                        ),
                                    ),
                                    (
                                        "emitted",
                                        storm_lua_syntax::explanation::DecisionFactValue::Shared(
                                            literal_value(target, candidate)
                                                .map(|value| shared.literal_fact_shared(&value))
                                                .unwrap_or_else(nonliteral_fact),
                                        ),
                                    ),
                                    (
                                        "aggressive",
                                        storm_lua_syntax::explanation::DecisionFactValue::Shared(
                                            std::sync::Arc::clone(&shared.aggressive),
                                        ),
                                    ),
                                    (
                                        "absToleranceBits",
                                        storm_lua_syntax::explanation::DecisionFactValue::Shared(
                                            std::sync::Arc::clone(&shared.abs_bits),
                                        ),
                                    ),
                                    (
                                        "relToleranceBits",
                                        storm_lua_syntax::explanation::DecisionFactValue::Shared(
                                            std::sync::Arc::clone(&shared.rel_bits),
                                        ),
                                    ),
                                ],
                            ),
                        );
                    }
                    target.nodes[node as usize] = target.node(candidate).clone();
                    return;
                }
            }
        }
        if op == "-" {
            if let Node::Un(inner, value) = target.node(expression).clone() {
                if inner == "-" {
                    target.nodes[node as usize] = target.node(value).clone();
                    return;
                }
            }
        }
        // `not not x` is a boolean conversion, not an identity operation.
        // Literal operands were already folded above; dynamic values must
        // retain both operators.
    }

    if let Node::Bin(op, left, right) = target.node(node).clone() {
        if let Some(candidate) = boolean_coercion(target, node) {
            if measure_expr(target, candidate) < measure_expr(target, node) {
                target.nodes[node as usize] = target.node(candidate).clone();
                return;
            }
        }
        let a = literal_value(target, left);
        let b = literal_value(target, right);
        if let (Some(a), Some(b)) = (&a, &b) {
            if fold_nonportable_math || op != "^" {
                if let Some(value) = eval_binary(&op, a, b) {
                    let evaluated_fact = target
                        .nodes
                        .tracks_origins()
                        .then(|| shared.literal_fact_shared(&value));
                    if let Some(candidate) =
                        aggressive_literal_node(target, value, aggressive, tolerance)
                    {
                        let before_size = measure_expr(target, node);
                        let after_size = measure_expr(target, candidate);
                        if after_size <= before_size {
                            if let Some(evaluated_fact) = evaluated_fact {
                                *evidence = Some(
                                    storm_lua_syntax::explanation::OptimizationReason::decision_shared(
                                        "constant-folding",
                                        "literal-operands-evaluated-and-size-nonincreasing",
                                        [
                                            (
                                                "operator",
                                                storm_lua_syntax::explanation::DecisionFactValue::Borrowed(
                                                    op.as_str(),
                                                ),
                                            ),
                                            (
                                                "left",
                                                storm_lua_syntax::explanation::DecisionFactValue::Shared(
                                                    shared.literal_fact_shared(a),
                                                ),
                                            ),
                                            (
                                                "right",
                                                storm_lua_syntax::explanation::DecisionFactValue::Shared(
                                                    shared.literal_fact_shared(b),
                                                ),
                                            ),
                                            (
                                                "evaluated",
                                                storm_lua_syntax::explanation::DecisionFactValue::Shared(
                                                    evaluated_fact,
                                                ),
                                            ),
                                            (
                                                "emitted",
                                                storm_lua_syntax::explanation::DecisionFactValue::Shared(
                                                    literal_value(target, candidate)
                                                        .map(|emitted| {
                                                            shared.literal_fact_shared(&emitted)
                                                        })
                                                        .unwrap_or_else(nonliteral_fact),
                                                ),
                                            ),
                                            (
                                                "expressionSizeBefore",
                                                storm_lua_syntax::explanation::DecisionFactValue::Shared(
                                                    shared.size_fact_shared(before_size),
                                                ),
                                            ),
                                            (
                                                "expressionSizeAfter",
                                                storm_lua_syntax::explanation::DecisionFactValue::Shared(
                                                    shared.size_fact_shared(after_size),
                                                ),
                                            ),
                                            (
                                                "aggressive",
                                                storm_lua_syntax::explanation::DecisionFactValue::Shared(
                                                    std::sync::Arc::clone(&shared.aggressive),
                                                ),
                                            ),
                                            (
                                                "absToleranceBits",
                                                storm_lua_syntax::explanation::DecisionFactValue::Shared(
                                                    std::sync::Arc::clone(&shared.abs_bits),
                                                ),
                                            ),
                                            (
                                                "relToleranceBits",
                                                storm_lua_syntax::explanation::DecisionFactValue::Shared(
                                                    std::sync::Arc::clone(&shared.rel_bits),
                                                ),
                                            ),
                                        ],
                                    ),
                                );
                            }
                            target.nodes[node as usize] = target.node(candidate).clone();
                            return;
                        }
                    }
                }
            }
        }
        if op == "and" {
            if let Some(a) = &a {
                let selected = if lua_truth(a) { right } else { left };
                // An `and`/`or` expression yields exactly one value in Lua,
                // but a bare call or `...` substituted in its place would
                // expand in an expanding slot (e.g. `local a, b = true and
                // pair()` becoming `local a, b = pair()`). Parenthesize a
                // multi-value-capable selected operand to preserve the
                // single-value adjustment; the parentheses pass keeps parens
                // around calls for exactly this reason.
                if matches!(target.node(selected), Node::Call(..) | Node::Vararg) {
                    let paren = target.push(Node::Paren(selected));
                    target.nodes[node as usize] = target.node(paren).clone();
                } else {
                    target.nodes[node as usize] = target.node(selected).clone();
                }
                return;
            }
        }
        if op == "or" {
            if let Some(a) = &a {
                let selected = if lua_truth(a) { left } else { right };
                // Same single-value adjustment as the `and` case above.
                if matches!(target.node(selected), Node::Call(..) | Node::Vararg) {
                    let paren = target.push(Node::Paren(selected));
                    target.nodes[node as usize] = target.node(paren).clone();
                } else {
                    target.nodes[node as usize] = target.node(selected).clone();
                }
                return;
            }
        }
        if aggressive {
            if let Some(candidate) = modulo_bit_test(target, node) {
                if measure_expr(target, candidate) < measure_expr(target, node) {
                    target.nodes[node as usize] = target.node(candidate).clone();
                    return;
                }
            }
        }
        if aggressive
            && matches!(op.as_str(), "+" | "-")
            && matches!(a, Some(Literal::Int(_) | Literal::Num(_)))
            && is_zero(target, right)
        {
            target.nodes[node as usize] = target.node(left).clone();
            return;
        }
        if aggressive
            && op == "+"
            && matches!(b, Some(Literal::Int(_) | Literal::Num(_)))
            && is_zero(target, left)
        {
            target.nodes[node as usize] = target.node(right).clone();
            return;
        }
        if aggressive && op == "-" && is_zero(target, left) {
            let r = without_parens(target, right);
            if let Node::Un(inner, value) = target.node(r).clone() {
                if inner == "-" {
                    target.nodes[node as usize] = target.node(value).clone();
                    return;
                }
            }
            target.nodes[node as usize] = Node::Un("-".into(), right);
            return;
        }
        if aggressive
            && op == "*"
            && matches!(a, Some(Literal::Int(_) | Literal::Num(_)))
            && is_one(target, right)
        {
            target.nodes[node as usize] = target.node(left).clone();
            return;
        }
        if aggressive
            && op == "*"
            && matches!(b, Some(Literal::Int(_) | Literal::Num(_)))
            && is_one(target, left)
        {
            target.nodes[node as usize] = target.node(right).clone();
            return;
        }
        if aggressive
            && op == "/"
            && matches!(a, Some(Literal::Int(_) | Literal::Num(_)))
            && is_one(target, right)
        {
            target.nodes[node as usize] = target.node(left).clone();
            return;
        }
        if aggressive && op == "*" {
            let l = without_parens(target, left);
            let r = without_parens(target, right);
            for (constant, value) in [(l, right), (r, left)] {
                if let Node::Num(text) = target.node(constant).clone() {
                    let multiplier = num_val(&text);
                    let divisor = 1.0 / multiplier;
                    if divisor.fract() == 0.0
                        && divisor > 1.0
                        && divisor <= 9_007_199_254_740_991.0
                        && divisor.log2().fract() == 0.0
                    {
                        let d = target.push(Node::Num(short_num(divisor).into()));
                        computed_origin(target, d, &[constant], "reciprocal-power-of-two");
                        let candidate = target.push(Node::Bin("/".into(), value, d));
                        if measure_expr(target, candidate) < measure_expr(target, node) {
                            target.nodes[node as usize] = target.node(candidate).clone();
                            return;
                        }
                    }
                }
            }
            for (fraction, value, original_value) in [
                (
                    l,
                    right,
                    match &original {
                        Node::Bin(_, _, r) => *r,
                        _ => right,
                    },
                ),
                (
                    r,
                    left,
                    match &original {
                        Node::Bin(_, l, _) => *l,
                        _ => left,
                    },
                ),
            ] {
                if let Node::Bin(frac_op, numerator, divisor) = target.node(fraction).clone() {
                    if frac_op == "/"
                        && is_one(target, numerator)
                        && original_child_movable(analyzer, source, target, original_value, value)
                    {
                        if let Some(candidate) = divide_by_literal(target, value, divisor) {
                            if measure_expr(target, candidate) < measure_expr(target, node) {
                                target.nodes[node as usize] = target.node(candidate).clone();
                                return;
                            }
                        }
                    }
                }
            }
        }
        let (orig_left, orig_right) = match &original {
            Node::Bin(_, l, r) => (*l, *r),
            _ => (left, right),
        };
        if aggressive
            && op == "*"
            && is_zero(target, right)
            && finite_number_literal(target, left)
            && original_child_movable(analyzer, source, target, orig_left, left)
        {
            target.nodes[node as usize] = target.node(right).clone();
            return;
        }
        if aggressive
            && op == "*"
            && is_zero(target, left)
            && finite_number_literal(target, right)
            && original_child_movable(analyzer, source, target, orig_right, right)
        {
            target.nodes[node as usize] = target.node(left).clone();
            return;
        }
        if aggressive
            && op == "-"
            && semantic_same(target, resolution, left, right)
            && finite_number_literal(target, left)
            && original_child_movable(analyzer, source, target, orig_left, left)
        {
            target.nodes[node as usize] = Node::Num("0".into());
            return;
        }
        if aggressive && op == "+" {
            if let Node::Bin(mul_op, neg, term) = target.node(right).clone() {
                if mul_op == "*" {
                    let neg = without_parens(target, neg);
                    if let Node::Un(neg_op, inner) = target.node(neg).clone() {
                        if neg_op == "-"
                            && semantic_same(target, resolution, left, inner)
                            && finite_number_literal(target, left)
                            && original_child_movable(analyzer, source, target, orig_left, left)
                        {
                            let one = target.push(Node::Num("1".into()));
                            computed_origin(target, one, &[node], "factored-unit-coefficient");
                            let sub = target.push(Node::Bin("-".into(), one, term));
                            computed_origin(target, sub, &[right], "factored-coefficient");
                            let candidate = target.push(Node::Bin("*".into(), left, sub));
                            if measure_expr(target, candidate) < measure_expr(target, node) {
                                target.nodes[node as usize] = target.node(candidate).clone();
                                return;
                            }
                        }
                    }
                }
            }
        }
        if aggressive && op == "or" {
            let l = without_parens(target, left);
            let r = without_parens(target, right);
            if let Node::Bin(and_op, condition, yes) = target.node(l).clone() {
                if and_op == "and" {
                    let c = without_parens(target, condition);
                    if let Node::Un(not_op, inner) = target.node(c).clone() {
                        if not_op == "not"
                            && semantic_same(target, resolution, inner, r)
                            && original_child_movable(analyzer, source, target, orig_right, right)
                        {
                            if let Some(truth) = literal_value(target, yes) {
                                if lua_truth(&truth) {
                                    let candidate = target.push(Node::Bin("or".into(), right, yes));
                                    if measure_expr(target, candidate) < measure_expr(target, node)
                                    {
                                        target.nodes[node as usize] =
                                            target.node(candidate).clone();
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if let Node::Un(not_op, inner) = target.node(l).clone() {
                if not_op == "not" {
                    if let Node::Bin(and_op, condition, yes) = target.node(r).clone() {
                        if and_op == "and"
                            && semantic_same(target, resolution, inner, condition)
                            && original_child_movable(analyzer, source, target, orig_left, left)
                        {
                            let candidate = target.push(Node::Bin("or".into(), left, yes));
                            if measure_expr(target, candidate) < measure_expr(target, node) {
                                target.nodes[node as usize] = target.node(candidate).clone();
                                return;
                            }
                        }
                    }
                }
            }
            if let (Node::Bin(aop, ac, av), Node::Bin(bop, bc, bv)) =
                (target.node(l).clone(), target.node(r).clone())
            {
                if aop == "and"
                    && bop == "and"
                    && semantic_same(target, resolution, ac, bc)
                    && original_child_movable(analyzer, source, target, orig_left, left)
                {
                    let alternatives = target.push(Node::Bin("or".into(), av, bv));
                    computed_origin(
                        target,
                        alternatives,
                        &[l, r],
                        "shared-condition-alternatives",
                    );
                    let candidate = target.push(Node::Bin("and".into(), ac, alternatives));
                    if measure_expr(target, candidate) < measure_expr(target, node) {
                        target.nodes[node as usize] = target.node(candidate).clone();
                        return;
                    }
                }
            }
        }
        if op == "+" {
            if let Some(positive) = positive_term(target, right) {
                target.nodes[node as usize] = Node::Bin("-".into(), left, positive);
                return;
            }
        }
    }

    if let Node::Call(function, args, _) = target.node(node).clone() {
        let original_function = match original {
            Node::Call(f, _, _) => f,
            _ => function,
        };
        if args.iter().all(|a| literal_value(target, *a).is_some()) {
            if let Some(builtin) = analyzer.resolve_builtin_reference(original_function) {
                let nonportable_math = matches!(
                    builtin.as_str(),
                    "math.sqrt" | "math.sin" | "math.cos" | "math.tan" | "math.atan"
                );
                if builtin.starts_with("math.") && (fold_nonportable_math || !nonportable_math) {
                    #[expect(
                        clippy::unwrap_used,
                        reason = "The preceding all-arguments literal check applies to the same immutable target arena"
                    )]
                    let values: Vec<Literal> = args
                        .iter()
                        .map(|a| literal_value(target, *a).unwrap())
                        .collect();
                    let value = eval_math(&builtin, &values);
                    if let Some(value) = value {
                        if let Some(candidate) =
                            aggressive_literal_node(target, value, aggressive, tolerance)
                        {
                            if measure_expr(target, candidate) <= measure_expr(target, node) {
                                target.nodes[node as usize] = target.node(candidate).clone();
                            }
                        }
                    }
                }
            }
        }
    }
}

pub fn fold_expressions_with_options(
    ast: &Ast,
    root: NodeId,
    aggressive: bool,
    tolerance: NumericTolerance,
    fold_nonportable_math: bool,
) -> (Ast, NodeId) {
    let shared = FoldSharedFacts::new(aggressive, tolerance);
    fold_expressions_with_shared(
        ast,
        root,
        aggressive,
        tolerance,
        fold_nonportable_math,
        &shared,
    )
}

/// Like `fold_expressions_with_options`, but reuses a caller-owned
/// `FoldSharedFacts` across calls. The shared facts are pure functions of
/// (aggressive, tolerance, literal value, expression size), so reusing them
/// across folds with identical parameters records identical fact strings
/// and only shares the allocations.
pub(crate) fn fold_expressions_with_shared(
    ast: &Ast,
    root: NodeId,
    aggressive: bool,
    tolerance: NumericTolerance,
    fold_nonportable_math: bool,
    shared: &FoldSharedFacts,
) -> (Ast, NodeId) {
    let resolution = resolve(ast, root);
    let analyzer = EffectAnalyzer::new(ast, &resolution, root, true);
    let statement_calls = statement_call_nodes(ast, root);
    let mut target = clone_ast(ast);
    fold_node(
        ast,
        &mut target,
        &resolution,
        &statement_calls,
        &analyzer,
        root,
        aggressive,
        tolerance,
        fold_nonportable_math,
        shared,
    );
    (target, root)
}

pub fn fold_expressions(
    ast: &Ast,
    root: NodeId,
    aggressive: bool,
    tolerance: NumericTolerance,
) -> (Ast, NodeId) {
    fold_expressions_with_options(ast, root, aggressive, tolerance, true)
}

fn conditional_evidence(
    ast: &Ast,
    node: NodeId,
) -> Option<storm_lua_syntax::explanation::OptimizationReason> {
    if !ast.nodes.tracks_origins() {
        return None;
    }
    let Node::If(arms, otherwise) = ast.node(node) else {
        return None;
    };
    let values = arms
        .iter()
        .map(|arm| {
            literal_value(ast, arm.cond)
                .map(|v| lua_truth(&v).to_string())
                .unwrap_or_else(|| "unknown".into())
        })
        .collect::<Vec<_>>();
    Some(storm_lua_syntax::explanation::OptimizationReason::decision(
        "constant-control-flow",
        "lua-truthiness-of-known-conditions",
        [
            ("armTruthiness", values.join(",")),
            ("hasElse", otherwise.is_some().to_string()),
        ],
    ))
}

/// Allocation-free snapshot of a `conditional_evidence` reason, taken at the
/// same point in the traversal (after the children rewrite, before arm
/// pruning). Each arm contributes two bits: 0 = unknown, 1 = known-true,
/// 2 = known-false, so up to 32 arms fit in one word. The reason itself is
/// only built from the snapshot when the node turns out to have changed;
/// unchanged `If` nodes previously paid the full construction cost (arm
/// truthiness strings, the joined value, the facts box) for a reason that
/// was then discarded. Arms beyond 32 fall back to the eager path.
struct DeferredConditionalEvidence {
    arms: u64,
    count: u32,
    has_else: bool,
}

fn conditional_evidence_snapshot(ast: &Ast, node: NodeId) -> Option<DeferredConditionalEvidence> {
    if !ast.nodes.tracks_origins() {
        return None;
    }
    let Node::If(arms, otherwise) = ast.node(node) else {
        return None;
    };
    if arms.len() > 32 {
        return None;
    }
    let mut packed = 0u64;
    for (i, arm) in arms.iter().enumerate() {
        let bits = match literal_value(ast, arm.cond).map(|v| lua_truth(&v)) {
            Some(true) => 1u64,
            Some(false) => 2u64,
            None => 0u64,
        };
        packed |= bits << (2 * i);
    }
    Some(DeferredConditionalEvidence {
        arms: packed,
        count: arms.len() as u32,
        has_else: otherwise.is_some(),
    })
}

fn conditional_evidence_from_snapshot(
    snapshot: &DeferredConditionalEvidence,
) -> storm_lua_syntax::explanation::OptimizationReason {
    let values = (0..snapshot.count)
        .map(|i| match (snapshot.arms >> (2 * i)) & 3 {
            1 => "true",
            2 => "false",
            _ => "unknown",
        })
        .collect::<Vec<_>>();
    storm_lua_syntax::explanation::OptimizationReason::decision(
        "constant-control-flow",
        "lua-truthiness-of-known-conditions",
        [
            ("armTruthiness", values.join(",")),
            ("hasElse", snapshot.has_else.to_string()),
        ],
    )
}
fn simplify_node(source: &Ast, target: &mut Ast, node: NodeId) {
    let mut evidence = None;
    let mut deferred = None;
    simplify_node_inner(source, target, node, &mut evidence, &mut deferred);
    // Same split-arena argument as `fold_node`: the inner traversal already
    // rewrites this node, so deriving from the immutable source avoids
    // cloning the node value for unchanged nodes while changed nodes keep
    // the same capture + finish_rewrite path and reason. The code string
    // is shared across all nodes of the simplification.
    target
        .nodes
        .derive_from_with_code(node, &source.nodes, node, &simplify_code());
    // A deferred reason is built only for nodes that actually changed, so
    // unchanged `If` nodes pay no evidence construction at all. The
    // snapshot was taken before arm pruning, exactly where the eager
    // reason used to be built, so a changed node records the same reason.
    // The change check must come first: building the reason before it
    // would pay the full construction cost for unchanged nodes as well.
    if source.node(node) != target.node(node) {
        let mut reason = evidence;
        if reason.is_none() {
            reason = deferred.map(|snapshot| conditional_evidence_from_snapshot(&snapshot));
        }
        if let Some(reason) = reason {
            if matches!(target.node(node),Node::Block(nodes) if nodes.is_empty()) {
                target.nodes.explain_removal(node, reason);
            } else {
                target.nodes.explain(node, reason);
            }
        }
    }
}

/// Node to substitute for a constant-selected `if`/`else` body. The parent
/// block flattens bare `Block` statements into itself, so a body whose top
/// level contains `return` must keep a `do` wrapper: spliced into the
/// parent, the `return` would no longer be the block's last statement and
/// the emitted code would not parse. A body whose top level declares a
/// local must also keep the wrapper: splicing extends the declaration's
/// scope over the rest of the enclosing block, and later passes re-resolve
/// the flattened tree, so a following read of a same-named outer local
/// would bind to the spliced declaration instead. A body whose top level
/// defines a label must also keep the wrapper: spliced into the parent,
/// the label would share the parent block's label namespace, duplicating
/// a same-named parent label (which does not parse) and capturing parent
/// `goto`s that bound to the parent's own label.
fn selected_body(target: &Ast, body: NodeId) -> Node {
    let needs_wrap = matches!(
        target.node(body),
        Node::Block(statements) if statements.iter().any(|statement| {
            matches!(
                target.node(*statement),
                Node::Return(..) | Node::Local(..) | Node::Localfunc(..) | Node::Label(..)
            )
        })
    );
    if needs_wrap {
        Node::Do(body)
    } else {
        target.node(body).clone()
    }
}

fn simplify_node_inner(
    source: &Ast,
    target: &mut Ast,
    node: NodeId,
    evidence: &mut Option<storm_lua_syntax::explanation::OptimizationReason>,
    deferred: &mut Option<DeferredConditionalEvidence>,
) {
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        simplify_node(source, target, child);
        child
    });
    target
        .nodes
        .rewrite(node, mapped, "literal-folding-traversal");

    if let Node::If(arms, else_block) = target.node(node).clone() {
        // Snapshot the pre-pruning evidence instead of building the reason:
        // it is only materialized when this node turns out to have changed.
        // Overflowing arm counts (or untracked input) fall back to the
        // eager path, which returns None for untracked input.
        if let Some(snapshot) = conditional_evidence_snapshot(target, node) {
            *deferred = Some(snapshot);
        } else {
            *evidence = conditional_evidence(target, node);
        }
        let arm_count = arms.len();
        let mut kept = Vec::new();
        let mut unresolved = false;
        for arm in arms {
            match literal_value(target, arm.cond) {
                Some(value) if !lua_truth(&value) => {
                    // A known-false arm is unreachable only after all previous
                    // conditions have also been resolved.
                }
                Some(_) if !unresolved => {
                    target.nodes[node as usize] = selected_body(target, arm.body);
                    return;
                }
                Some(_) => {
                    // The first unresolved arm may fall through to this true
                    // arm.  Keep it, and discard all arms after it.
                    kept.push(arm);
                    break;
                }
                None => {
                    unresolved = true;
                    kept.push(arm);
                }
            }
        }
        if kept.is_empty() {
            if let Some(else_block) = else_block {
                target.nodes[node as usize] = selected_body(target, else_block);
            } else {
                target.nodes[node as usize] = Node::Block(Vec::new());
            }
            return;
        }
        // Arms are pushed unchanged and in order, so an unchanged arm count
        // means nothing was dropped: skip the write so the origin slot stays shared.
        if kept.len() != arm_count {
            target.nodes[node as usize] = Node::If(kept, else_block);
        }
    }

    if let Node::Block(statements) = target.node(node).clone() {
        let mut output = Vec::new();
        let last_index = statements.len().saturating_sub(1);
        for (index, &statement) in statements.iter().enumerate() {
            match target.node(statement).clone() {
                Node::Block(children) => {
                    // Splicing a block whose top level contains `return`
                    // into a non-final position would leave statements
                    // after a `return`, which does not parse. Keep such a
                    // block wrapped in `do` unless it is the last statement.
                    // A block whose top level declares a local must stay
                    // wrapped at any position: splicing extends the
                    // declaration's scope to the end of this block (and,
                    // for a `repeat` body, into the `until` condition,
                    // which may legally read body locals), so a following
                    // read of a same-named outer local would re-resolve
                    // to the spliced declaration. A block whose top level
                    // defines a label must stay wrapped at any position:
                    // spliced into this block, the label would duplicate
                    // a same-named label here (which does not parse) and
                    // capture `goto`s that bound to this block's own
                    // label. A `goto` alone needs no wrapper: with labels
                    // and locals kept out of the splice, its resolution
                    // environment is unchanged.
                    let has_top_level_return = children
                        .iter()
                        .any(|child| matches!(target.node(*child), Node::Return(..)));
                    let has_top_level_local = children.iter().any(|child| {
                        matches!(target.node(*child), Node::Local(..) | Node::Localfunc(..))
                    });
                    let has_top_level_label = children
                        .iter()
                        .any(|child| matches!(target.node(*child), Node::Label(..)));
                    if has_top_level_local
                        || has_top_level_label
                        || (has_top_level_return && index != last_index)
                    {
                        let wrapped = target.push(Node::Do(statement));
                        output.push(wrapped);
                    } else {
                        output.extend(children);
                    }
                }
                Node::Do(body) if matches!(target.node(body), Node::Block(ss) if ss.is_empty()) => {
                }
                _ => output.push(statement),
            }
        }
        // Without a flattened child block or a dropped empty do-block the
        // output is the input list; skip the write so the origin slot stays shared.
        if output != statements {
            target.nodes[node as usize] = Node::Block(output);
        }
    }
}

pub fn simplify_control_flow(ast: &Ast, root: NodeId, _aggressive: bool) -> (Ast, NodeId) {
    let mut target = clone_ast(ast);
    simplify_node(ast, &mut target, root);
    (target, root)
}

fn fold_exact_safe_node(ast: &mut Ast, node: NodeId, changed: &mut bool) {
    let origin = ast.nodes.capture_origin(node);
    let mut evidence = None;
    fold_exact_safe_node_inner(ast, node, &mut evidence, changed);
    ast.nodes
        .finish_rewrite(node, origin, "exact-control-flow-folding");
    if let Some(reason) = evidence {
        if !matches!(ast.node(node), Node::If(..)) {
            if matches!(ast.node(node),Node::Block(nodes) if nodes.is_empty()) {
                ast.nodes.explain_removal(node, reason);
            } else {
                ast.nodes.explain(node, reason);
            }
        }
    }
}

fn fold_exact_safe_node_inner(
    ast: &mut Ast,
    node: NodeId,
    evidence: &mut Option<storm_lua_syntax::explanation::OptimizationReason>,
    changed: &mut bool,
) {
    let original = ast.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        fold_exact_safe_node(ast, child, changed);
        child
    });
    ast.nodes.rewrite(node, mapped, "literal-folding-traversal");

    match ast.node(node).clone() {
        Node::Un(op, expression) if op == "not" => {
            if let Some(value) = literal_value(ast, expression) {
                *changed = true;
                ast.nodes[node as usize] = Node::Bool(!lua_truth(&value));
            }
        }
        Node::Bin(op, left, right) if op == "and" || op == "or" => {
            if op == "or" {
                if let Some(candidate) = boolean_coercion(ast, node) {
                    if measure_expr(ast, candidate) < measure_expr(ast, node) {
                        *changed = true;
                        ast.nodes[node as usize] = ast.node(candidate).clone();
                        return;
                    }
                }
            }
            if let Some(value) = literal_value(ast, left) {
                let selected = if op == "and" {
                    if lua_truth(&value) {
                        right
                    } else {
                        left
                    }
                } else if lua_truth(&value) {
                    left
                } else {
                    right
                };
                // Same single-value adjustment as the fold above: a bare
                // call or `...` selected out of an `and`/`or` would expand
                // in an expanding slot, so parenthesize it.
                *changed = true;
                if matches!(ast.node(selected), Node::Call(..) | Node::Vararg) {
                    let paren = ast.push(Node::Paren(selected));
                    ast.nodes[node as usize] = ast.node(paren).clone();
                } else {
                    ast.nodes[node as usize] = ast.node(selected).clone();
                }
            }
        }
        Node::If(arms, else_block) => {
            *evidence = conditional_evidence(ast, node);
            let original_arm_count = arms.len();
            let mut kept = Vec::new();
            let mut replacement_else = else_block;
            let mut unresolved = false;
            let mut selected_body = None;

            for arm in arms {
                match literal_value(ast, arm.cond) {
                    Some(value) if !lua_truth(&value) => {}
                    Some(_) if !unresolved => {
                        selected_body = Some(arm.body);
                        break;
                    }
                    Some(_) => {
                        replacement_else = Some(arm.body);
                        break;
                    }
                    None => {
                        unresolved = true;
                        kept.push(arm);
                    }
                }
            }

            if let Some(body) = selected_body {
                // `if`/`else` bodies are lexical scopes. Keep that scope with a
                // `do` block instead of flattening the body into its parent.
                *changed = true;
                ast.nodes[node as usize] = Node::Do(body);
            } else if kept.is_empty() {
                *changed = true;
                ast.nodes[node as usize] = if let Some(body) = replacement_else {
                    Node::Do(body)
                } else {
                    Node::Block(Vec::new())
                };
            } else {
                if kept.len() != original_arm_count || replacement_else != else_block {
                    *changed = true;
                }
                ast.nodes[node as usize] = Node::If(kept, replacement_else);
            }
        }
        Node::Block(statements) => {
            let statement_count = statements.len();
            let output: Vec<NodeId> = statements
                .into_iter()
                .filter(|statement| {
                    !matches!(ast.node(*statement), Node::Block(children) if children.is_empty())
                })
                .collect();
            if output.len() != statement_count {
                *changed = true;
            }
            ast.nodes[node as usize] = Node::Block(output);
        }
        _ => {}
    }
}

/// Exact-mode subset of constant folding. This only evaluates Lua truthiness
/// for literal `not`/`and`/`or`, shortens `x and true or false` to `not not x`,
/// and prunes literal `if` arms. It never performs numeric arithmetic, libm
/// calls, approximation, or subtype-changing rewrites.
/// Branch bodies keep their lexical scope via `do ... end` when the surrounding
/// `if` disappears.
pub fn fold_exact_safe_control_flow(ast: &mut Ast, root: NodeId) -> PassResult {
    let before = storm_lua_syntax::size::measure_size(ast, root);
    let mut changed = false;
    fold_exact_safe_node(ast, root, &mut changed);
    let after = storm_lua_syntax::size::measure_size(ast, root);
    PassResult {
        root,
        saved: Some(before.saturating_sub(after) as u64),
        // Report whether the subset actually rewrote the tree. Size alone
        // cannot carry that signal: a rewrite can be size-neutral, and a
        // transformation of a statement the printer drops entirely (a
        // dead `if`) measures as no saving even though the fold ran.
        details: changed.then(|| vec!["exact-safe control-flow rewrite".to_string()]),
    }
}

pub fn constant_fold(ast: &mut Ast, root: NodeId, aggressive: bool) -> PassResult {
    let (folded, folded_root) =
        fold_expressions(ast, root, aggressive, NumericTolerance::default());
    let (simplified, simplified_root) = simplify_control_flow(&folded, folded_root, aggressive);
    *ast = simplified;
    PassResult {
        root: simplified_root,
        saved: None,
        details: None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn run(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        constant_fold(&mut ast, root, true);
        Printer::new(&ast, false).output(root)
    }

    #[test]
    fn folds_literals_and_truthiness() {
        let out = run("a=1+2 b=not 0 c=false or 7 if true then d=4*1 else d=9 end");
        assert!(out.contains("a=3"), "{out}");
        assert!(out.contains("b=1>2"), "{out}");
        assert!(out.contains("c=7"), "{out}");
        assert!(out.contains("d=4"), "{out}");
    }

    #[test]
    fn constant_selected_body_with_return_stays_parseable() {
        // A constant-selected `if`/`else` body whose top level returns must
        // not be spliced into the enclosing block: the statements after it
        // would land behind a `return`, which does not parse.
        let out = run("function f(a) if 2 > 0 then return a end return 99 end");
        assert!(parse_source(&out).is_ok(), "{out}");
        let out = run("function f(a) if 1 > 2 then a = 1 else return a end return 99 end");
        assert!(parse_source(&out).is_ok(), "{out}");
        // A return nested inside a further statement is not top level and
        // keeps the flat splice.
        let out = run("function f(a) if 2 > 0 then while a do return a end end return 99 end");
        assert!(parse_source(&out).is_ok(), "{out}");
    }

    #[test]
    fn constant_selected_body_with_local_stays_scoped() {
        // Regression (w6 cycle 54): a constant-selected body that declares
        // a local must keep its block boundary. Splicing it into the
        // enclosing block extends the declaration's scope, and a later
        // same-named read re-resolves to the spliced declaration.
        // These tests drive `simplify_control_flow`, the entry the search
        // pipeline uses (the `run` helper's `constant_fold` entry wraps
        // bodies through a different path and cannot see this defect).
        let scoped = |source: &str| -> String {
            let (ast, root) = parse_source(source).expect("parse");
            let (folded, folded_root) =
                fold_expressions(&ast, root, true, NumericTolerance { abs: 0.0, rel: 0.0 });
            let (out_ast, out_root) = simplify_control_flow(&folded, folded_root, true);
            Printer::new(&out_ast, false).output(out_root)
        };
        let out = scoped("local x = 1 if 2 > 0 then local x = 5 end y = x");
        assert!(out.contains("do"), "{out}");
        let out = scoped("local x = 1 if 1 > 2 then x = 2 else local x = 5 end y = x");
        assert!(out.contains("do"), "{out}");
        let out = scoped(
            "local function f() if 2 > 0 then local function g() return 5 end end return g end",
        );
        assert!(out.contains("do"), "{out}");
        // A body without returns or local declarations still flattens.
        let out = scoped("if true then g = 4 end");
        assert!(!out.contains("do"), "{out}");
        assert!(out.contains("g=4"), "{out}");
    }

    #[test]
    fn constant_selected_body_with_label_stays_scoped() {
        // Regression (w6 cycle 60, candidate G): a constant-selected body
        // whose top level defines a label must keep its block boundary.
        // Splicing the label into the enclosing block duplicates a
        // same-named enclosing label (which does not parse) and captures
        // enclosing `goto`s that bound to the enclosing block's own label.
        // Like the local-declaration regression, these tests drive
        // `simplify_control_flow`, the entry the search pipeline uses.
        let scoped = |source: &str| -> String {
            let (ast, root) = parse_source(source).expect("parse");
            let (folded, folded_root) =
                fold_expressions(&ast, root, true, NumericTolerance { abs: 0.0, rel: 0.0 });
            let (out_ast, out_root) = simplify_control_flow(&folded, folded_root, true);
            Printer::new(&out_ast, false).output(out_root)
        };
        // Duplicate label against the enclosing block.
        let out = scoped("::m:: local n = 0 if 2 > 0 then ::m:: n = 1 end output.setNumber(1, n)");
        assert!(out.contains("do"), "{out}");
        assert!(parse_source(&out).is_ok(), "{out}");
        // Two selected bodies in the same block must not duplicate each
        // other's labels either (the flattening site sees both splices).
        let out = scoped(
            "local n = 0 if 2 > 0 then ::m:: n = 1 end if 3 > 0 then ::m:: n = 2 end output.setNumber(1, n)",
        );
        assert!(out.contains("do"), "{out}");
        assert!(parse_source(&out).is_ok(), "{out}");
        // Else-arm selection goes through the same condition.
        let out = scoped(
            "::m:: local n = 0 if 1 > 2 then n = 9 else ::m:: n = 1 end output.setNumber(1, n)",
        );
        assert!(out.contains("do"), "{out}");
        assert!(parse_source(&out).is_ok(), "{out}");
        // Rebinding form: a `goto` in the enclosing block (here an
        // unresolved `if` body) bound to the enclosing block's own label
        // must not be captured by the spliced label.
        let out = scoped(
            "local n = 0 if input.getBool(2) then goto l if 2 > 0 then ::l:: n = n + 100 end end ::l:: n = n + 1 output.setNumber(1, n)",
        );
        assert!(out.contains("do"), "{out}");
        assert!(parse_source(&out).is_ok(), "{out}");
        // A body whose top level only jumps (it defines no label and
        // declares no local) still flattens: with labels and locals kept
        // out of the splice, a `goto` keeps the same resolution
        // environment.
        let out = scoped("local n = 0 ::top:: n = n + 1 if 2 > 0 then goto top end");
        assert!(!out.contains("do"), "{out}");
        assert!(out.contains("goto top"), "{out}");
    }

    #[test]
    fn constant_selected_body_without_return_still_flattens() {
        let out = run("function f() if true then g = 4 end return g end");
        assert!(out.contains("g=4"), "{out}");
        assert!(!out.contains("do"), "{out}");
    }

    #[test]
    fn statement_call_to_builtin_is_not_folded_to_bare_literal() {
        // math.abs(-5) used as a statement must stay a call: folding it to
        // a bare `5` statement does not reparse, so the whole optimization
        // used to fail with no surviving candidate.
        let out = run("function onTick() math.abs(-5) end");
        assert!(out.contains("math.abs(-5)"), "{out}");
        assert!(parse_source(&out).is_ok(), "{out}");
    }

    #[test]
    fn expression_call_to_builtin_still_folds() {
        let out = run("function onTick() local x = math.abs(-5) output.setNumber(1, x) end");
        assert!(out.contains("x=5"), "{out}");
    }

    #[test]
    fn tracked_folding_keeps_code_and_generated_origins() {
        // The derive_from change in `fold_node`/`simplify_node` must not alter
        // the folded code or the generated origins: folded and unfolded nodes
        // alike keep the attribution the capture + finish_rewrite path gave.
        let source =
            "local a=1+2*3 b=(4-1)*x c=not false if true then d=2+2 else d=9 end e=10/4 f=x+0";
        let (mut plain, plain_root) = parse_source(source).expect("parse");
        let plain_result = constant_fold(&mut plain, plain_root, true);
        let plain_code = Printer::new(&plain, false).output(plain_result.root);
        let (mut tracked, tracked_root) =
            storm_lua_syntax::parse_source_with_origins("fold.lua", source).unwrap();
        let tracked_result = constant_fold(&mut tracked, tracked_root, true);
        let output = storm_lua_syntax::Printer::new(&tracked, false)
            .output_with_positions(tracked_result.root);
        assert_eq!(output.code, plain_code, "{plain_code} vs {}", output.code);
        let origins =
            storm_lua_syntax::provenance::GeneratedOrigins::from_print(&tracked, &output).unwrap();
        origins.validate_for_code(&output.code).unwrap();
        assert_eq!(origins.unknown_bytes(), 0);
    }

    #[test]
    fn deferred_conditional_evidence_matches_eager() {
        // The simplify deferral must record exactly the eager reason for a
        // changed node: same code, basis, and fact values, including the
        // pre-pruning arm truthiness of multi-arm and unknown conditions.
        let source = "if true then a=1 else a=2 end if false then b=1 end if x then c=1 elseif true then c=2 else c=3 end if y then d=1 end if 0 then e=1 elseif nil then e=2 end";
        let (ast, _root) =
            storm_lua_syntax::parse_source_with_origins("defer.lua", source).unwrap();
        assert!(ast.nodes.tracks_origins());
        let mut if_count = 0;
        for i in 0..ast.nodes.len() {
            let id = i as NodeId;
            if !matches!(ast.node(id), Node::If(..)) {
                continue;
            }
            if_count += 1;
            let eager = conditional_evidence(&ast, id).expect("eager reason");
            let snapshot = conditional_evidence_snapshot(&ast, id).expect("snapshot");
            let rebuilt = conditional_evidence_from_snapshot(&snapshot);
            assert_eq!(eager, rebuilt, "if node {id}");
        }
        assert!(if_count >= 5, "found {if_count} if nodes");
    }

    #[test]
    fn exact_safe_folding_prunes_literal_control_flow_without_numeric_arithmetic() {
        let (mut ast, root) = parse_source(
            "local x=9 if true then local x=1 a=x else a=2 end b=false or x c=not nil d=1+2",
        )
        .expect("parse");
        fold_exact_safe_control_flow(&mut ast, root);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("do local x=1 a=x end"), "{out}");
        assert!(out.contains("b=x"), "{out}");
        assert!(out.contains("c=1>0"), "{out}");
        assert!(out.contains("d=1+2"), "{out}");
    }

    #[test]
    fn boolean_short_circuit_coercion_uses_double_not() {
        let out = run("a=f() and true or false");
        assert_eq!(out, "a=not not f()");

        let (mut ast, root) = parse_source("a=f() and true or false").expect("parse");
        fold_exact_safe_control_flow(&mut ast, root);
        let out = Printer::new(&ast, false).output(root);
        assert_eq!(out, "a=not not f()");
    }

    #[test]
    fn exact_safe_folding_turns_literal_true_elseif_into_else() {
        let (mut ast, root) =
            parse_source("if x then a=1 elseif true then a=2 else a=3 end").expect("parse");
        fold_exact_safe_control_flow(&mut ast, root);
        let out = Printer::new(&ast, false).output(root);
        assert_eq!(out, "if x then a=1 else a=2 end");
    }

    #[test]
    fn does_not_drop_effectful_zero_product() {
        let out = run("function f()output.setNumber(1,1)return 2 end a=f()*0");
        assert!(out.contains("f()*0"), "{out}");
    }

    #[test]
    fn zero_product_keeps_unproven_operand() {
        // `x * 0` / `0 * x` are 0 only for finite numeric x: for x = inf or
        // NaN the product is NaN, and for a non-numeric x the multiplication
        // must raise a runtime error. A name carries no finiteness proof, so
        // the aggressive identity must not replace the product with 0.
        let out = run("a=x*0");
        assert!(out.contains("x*0"), "{out}");
        let out = run("a=0*x");
        assert!(out.contains("0*x"), "{out}");
        // A non-finite literal (1e999 evaluates to +inf) is not a proof
        // either: 1e999 * 0 is NaN, not 0.
        let out = run("a=1e999*0");
        assert!(!out.contains("a=0"), "{out}");
        assert!(out.contains("*0"), "{out}");
    }

    #[test]
    fn self_subtraction_keeps_unproven_operand() {
        // `x - x` is 0 only for finite numeric x: inf - inf and NaN - NaN
        // are NaN, and a non-numeric x must raise a runtime error.
        let out = run("a=x-x");
        assert!(out.contains("x-x"), "{out}");
    }

    #[test]
    fn bit_test_modulo_form_keeps_unproven_operand() {
        // `(x & mask) == 0` rewritten to a modulo comparison drops the
        // integer-representation requirement of `&`: a fractional,
        // infinite, NaN or out-of-integer-range x must raise a runtime
        // error instead of evaluating the modulo form. A name carries
        // no proof of integer-valuedness, so the rewrite must not fire.
        let out = run("a=(x&1)==0");
        assert!(out.contains("x&1"), "{out}");
        let out = run("a=(x&2)~=0");
        assert!(out.contains("x&2"), "{out}");
    }

    #[test]
    fn bit_test_modulo_form_still_fires_for_proven_integers() {
        // An integral literal operand keeps the rewrite available.
        let out = run("a=(6&2)==0");
        assert!(out.contains("a=1>2"), "{out}");
        // A bitwise expression result is an integer whenever it
        // evaluates at all, so the nested form may still be rewritten.
        let out = run("a=((x&3)&1)==0");
        assert!(out.contains("%"), "{out}");
    }

    #[test]
    fn factored_coefficient_keeps_unproven_operand() {
        // `x + (-x * t) -> x * (1 - t)` changes the result for infinite
        // x (NaN versus -inf) and reassociates the rounding for finite
        // non-literal x. A name carries no finiteness proof.
        let out = run("local somebase=g() function f() return somebase+-somebase*2 end");
        assert!(!out.contains("somebase*-1"), "{out}");
        assert!(out.contains("somebase+-somebase*2"), "{out}");
    }

    #[test]
    fn reciprocal_multiplication_keeps_inexact_divisors() {
        // `x * (1 / d) -> x / d` is value-identical only when 1/d is
        // exact (d a power of two); 1/3 and 1/7 are rounded, and the
        // multiplication can differ from the division by 1 ulp.
        let out = run("a=x*(1/3)");
        assert!(!out.contains("x/3"), "{out}");
        let out = run("a=x*(1/7)");
        assert!(!out.contains("x/7"), "{out}");
        let out = run("a=x*(1/4)");
        assert!(out.contains("x/4"), "{out}");
    }

    #[test]
    fn zero_product_still_folds_finite_literals() {
        let out = run("a=3*0");
        assert!(out.contains("a=0"), "{out}");
        assert!(!out.contains("3*0"), "{out}");
    }

    #[test]
    fn portable_safe_mode_keeps_libm_dependent_folds_at_runtime() {
        fn safe(source: &str) -> String {
            let (ast, root) = parse_source(source).expect("parse");
            let (folded, folded_root) = fold_expressions_with_options(
                &ast,
                root,
                false,
                NumericTolerance::default(),
                false,
            );
            let (simplified, simplified_root) = simplify_control_flow(&folded, folded_root, false);
            Printer::new(&simplified, false).output(simplified_root)
        }

        assert_eq!(safe("x=6.3*10^(-5)"), "x=6.3*10^-5");
        assert_eq!(safe("x=math.sin(1)"), "x=math.sin(1)");
        assert_eq!(safe("x=math.floor(1.9)"), "x=1");
    }

    #[test]
    fn shared_facts_match_literal_fact_and_sizes() {
        let shared = FoldSharedFacts::new(false, NumericTolerance::default());
        for value in [
            Literal::Int(-7),
            Literal::Int(42),
            Literal::Num(1.5),
            Literal::Str(b"abc".to_vec()),
            Literal::Bool(true),
            Literal::Nil,
        ] {
            assert_eq!(
                shared.literal_fact_shared(&value).as_ref(),
                literal_fact(&value)
            );
        }
        // A repeated literal reuses the same shared text.
        let first = shared.literal_fact_shared(&Literal::Int(42));
        let second = shared.literal_fact_shared(&Literal::Int(42));
        assert!(std::sync::Arc::ptr_eq(&first, &second));
        assert_eq!(shared.size_fact_shared(13).as_ref(), "13");
        assert_eq!(shared.size_fact_shared(0).as_ref(), "0");
    }

    #[test]
    fn shared_facts_reuse_matches_fresh_fold() {
        // Reusing one FoldSharedFacts across folds must record exactly the
        // same code and generated origins as a fresh table per fold, even
        // when the shared table was already populated by another fold.
        let source = "a=1+2*3 b=(4-1)*5 c=10/4 d=not false";
        let shared = FoldSharedFacts::new(false, NumericTolerance::default());
        let (seed, seed_root) =
            storm_lua_syntax::parse_source_with_origins("seed.lua", "x=1+2*3").unwrap();
        let _ = fold_expressions_with_shared(
            &seed,
            seed_root,
            false,
            NumericTolerance::default(),
            true,
            &shared,
        );
        let (fresh_ast, fresh_root) =
            storm_lua_syntax::parse_source_with_origins("fold.lua", source).unwrap();
        let (fresh_folded, _) =
            fold_expressions(&fresh_ast, fresh_root, false, NumericTolerance::default());
        let fresh_output =
            storm_lua_syntax::Printer::new(&fresh_folded, false).output_with_positions(fresh_root);
        let fresh_origins = storm_lua_syntax::provenance::GeneratedOrigins::from_print(
            &fresh_folded,
            &fresh_output,
        )
        .unwrap();
        fresh_origins.validate_for_code(&fresh_output.code).unwrap();
        let (shared_ast, shared_root) =
            storm_lua_syntax::parse_source_with_origins("fold.lua", source).unwrap();
        let (shared_folded, _) = fold_expressions_with_shared(
            &shared_ast,
            shared_root,
            false,
            NumericTolerance::default(),
            true,
            &shared,
        );
        let shared_output = storm_lua_syntax::Printer::new(&shared_folded, false)
            .output_with_positions(shared_root);
        let shared_origins = storm_lua_syntax::provenance::GeneratedOrigins::from_print(
            &shared_folded,
            &shared_output,
        )
        .unwrap();
        shared_origins
            .validate_for_code(&shared_output.code)
            .unwrap();
        assert_eq!(fresh_output.code, shared_output.code);
        assert_eq!(fresh_origins, shared_origins);
        assert_eq!(fresh_origins.unknown_bytes(), 0);
    }

    #[test]
    fn and_or_fold_parenthesizes_multivalue_call() {
        // An `and`/`or` expression yields exactly one value in Lua, so a
        // call (or `...`) selected out of one must stay parenthesized:
        // `local a, b = true and pair()` is (7, nil), not (7, 8).
        for source in [
            "local a,b=true and f()",
            "local a,b=false or f()",
            "local a,b=nil or f()",
        ] {
            let out = run(source);
            assert!(out.contains("(f())"), "{source} -> {out}");
        }
        let out = run("function f(...) local a,b=true and ... end");
        assert!(out.contains("(...)"), "{out}");
        // Scalar selections still fold without parentheses.
        let out = run("a=true and 7 b=false or 9 c=nil or 3");
        assert!(out.contains("a=7"), "{out}");
        assert!(out.contains("b=9"), "{out}");
        assert!(out.contains("c=3"), "{out}");
    }

    #[test]
    fn fold_expressions_parenthesizes_multivalue_call() {
        // Same guard on the fold-only path (without the simplify pass).
        let (ast, root) = parse_source("local a,b=true and f()").expect("parse");
        let (folded, folded_root) = fold_expressions(&ast, root, true, NumericTolerance::default());
        let out = Printer::new(&folded, false).output(folded_root);
        assert!(out.contains("(f())"), "{out}");
    }

    #[test]
    fn exact_safe_fold_parenthesizes_multivalue_call() {
        // Same guard on the exact-safe (simplify) path on its own.
        for source in ["local a,b=true and f()", "local a,b=false or f()"] {
            let (mut ast, root) = parse_source(source).expect("parse");
            fold_exact_safe_control_flow(&mut ast, root);
            let out = Printer::new(&ast, false).output(root);
            assert!(out.contains("(f())"), "{source} -> {out}");
        }
    }
}
