//! Literal expression/control-flow folding (`passes/literal-folding.ts`).
//!
//! This is intentionally a reusable Phase 4c primitive: literal user-function
//! folding and equivalent trailing-argument omission both depend on the exact
//! same folding semantics.

use crate::pass::PassResult;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::numeric::{
    decode_lua_string, integer_literal_value, normalize_num_literal, num_val, quote_lua, short_num,
};
use storm_lua_syntax::size::measure_expr;

use super::immutable_values::expression_key;

#[derive(Clone, Debug)]
enum Literal {
    Int(i64),
    Num(f64),
    Str(String),
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

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn literal_value(ast: &Ast, node: NodeId) -> Option<Literal> {
    match ast.node(node) {
        Node::Num(v) => integer_literal_value(v)
            .map(Literal::Int)
            .or_else(|| Some(Literal::Num(num_val(v)))),
        Node::Str(v) => Some(Literal::Str(decode_lua_string(v))),
        Node::Bool(v) => Some(Literal::Bool(*v)),
        Node::Nil => Some(Literal::Nil),
        _ => None,
    }
}

fn literal_node(ast: &mut Ast, value: Literal) -> Option<NodeId> {
    match value {
        Literal::Nil => Some(ast.push(Node::Nil)),
        Literal::Bool(v) => Some(ast.push(Node::Bool(v))),
        Literal::Int(v) => Some(ast.push(Node::Num(v.to_string().into()))),
        Literal::Num(v) if !v.is_nan() => Some(ast.push(Node::Num(short_num(v).into()))),
        // JS `shortNum(NaN)` is not a valid Lua literal. The TS implementation
        // constructs it but the parser/printer path does not use such folds in
        // the accepted corpus; keep the fold disabled here rather than emit an
        // invalid numeric token.
        Literal::Num(_) => None,
        Literal::Str(v) => Some(ast.push(Node::Str(quote_lua(&v).into()))),
    }
}

fn aggressive_literal_node(
    ast: &mut Ast,
    value: Literal,
    aggressive: bool,
    tolerance: NumericTolerance,
) -> Option<NodeId> {
    let Literal::Num(number) = value else {
        return literal_node(ast, value);
    };
    if !aggressive || !number.is_finite() || number.fract() == 0.0 {
        return literal_node(ast, Literal::Num(number));
    }
    let mut best = short_num(number);
    for digits in 1..=15usize {
        // Number(value.toPrecision(digits)): scientific formatting followed by
        // f64 parsing yields the same rounded numeric value independent of the
        // textual exponent/fixed representation selected by JS.
        let text = format!("{:.*e}", digits - 1, number);
        let Ok(approximate) = text.parse::<f64>() else {
            continue;
        };
        let error = (approximate - number).abs();
        let limit = tolerance.abs.max(number.abs() * tolerance.rel);
        if error > limit {
            continue;
        }
        let candidate = short_num(approximate);
        if candidate.len() < best.len() {
            best = candidate;
        }
    }
    Some(ast.push(Node::Num(best.into())))
}

fn lua_truth(value: &Literal) -> bool {
    !matches!(value, Literal::Nil | Literal::Bool(false))
}

fn js_number_string(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v == f64::INFINITY {
        "Infinity".into()
    } else if v == f64::NEG_INFINITY {
        "-Infinity".into()
    } else if v == 0.0 {
        "0".into()
    } else {
        short_num(v)
    }
}

fn literal_to_js_string(v: &Literal) -> String {
    match v {
        Literal::Int(v) => v.to_string(),
        Literal::Num(v) => js_number_string(*v),
        Literal::Str(v) => v.clone(),
        Literal::Bool(v) => if *v { "true" } else { "false" }.into(),
        Literal::Nil => "nil".into(),
    }
}

fn js_to_number(v: &Literal) -> f64 {
    match v {
        Literal::Int(v) => *v as f64,
        Literal::Num(v) => *v,
        Literal::Bool(v) => {
            if *v {
                1.0
            } else {
                0.0
            }
        }
        Literal::Nil => 0.0,
        Literal::Str(v) => v.trim().parse::<f64>().unwrap_or(f64::NAN),
    }
}

fn js_to_uint32(v: f64) -> u32 {
    if !v.is_finite() || v == 0.0 {
        return 0;
    }
    let truncated = v.trunc();
    let modulo = truncated.rem_euclid(4294967296.0);
    modulo as u32
}

fn js_to_int32(v: f64) -> i32 {
    js_to_uint32(v) as i32
}

fn eval_unary(op: &str, a: &Literal) -> Option<Literal> {
    match op {
        "not" => Some(Literal::Bool(!lua_truth(a))),
        "-" => match a {
            Literal::Int(v) => v.checked_neg().map(Literal::Int),
            _ => Some(Literal::Num(-js_to_number(a))),
        },
        "~" => match a {
            Literal::Int(v) => Some(Literal::Int(!v)),
            _ => Some(Literal::Int(!js_to_int32(js_to_number(a)) as i64)),
        },
        "#" => match a {
            Literal::Str(v) => Some(Literal::Int(v.len() as i64)),
            _ => None,
        },
        _ => None,
    }
}

fn strict_equal(a: &Literal, b: &Literal) -> bool {
    match (a, b) {
        (Literal::Nil, Literal::Nil) => true,
        (Literal::Bool(a), Literal::Bool(b)) => a == b,
        (Literal::Str(a), Literal::Str(b)) => a == b,
        (Literal::Int(a), Literal::Int(b)) => a == b,
        (Literal::Int(a), Literal::Num(b)) | (Literal::Num(b), Literal::Int(a)) => {
            (*a as f64) == *b
        }
        (Literal::Num(a), Literal::Num(b)) => a == b,
        _ => false,
    }
}

fn relational(op: &str, a: &Literal, b: &Literal) -> bool {
    if let (Literal::Str(a), Literal::Str(b)) = (a, b) {
        return match op {
            "<" => a < b,
            ">" => a > b,
            "<=" => a <= b,
            ">=" => a >= b,
            _ => false,
        };
    }
    let a = js_to_number(a);
    let b = js_to_number(b);
    match op {
        "<" => a < b,
        ">" => a > b,
        "<=" => a <= b,
        ">=" => a >= b,
        _ => false,
    }
}

fn eval_binary(op: &str, a: &Literal, b: &Literal) -> Option<Literal> {
    let an = || js_to_number(a);
    let bn = || js_to_number(b);
    match op {
        // Unlike JavaScript, Lua does not coerce strings for arithmetic and
        // `+` is never concatenation.  Refuse to fold such expressions; the
        // runtime error/number coercion boundary must remain observable.
        "+" => match (a, b) {
            (Literal::Int(x), Literal::Int(y)) => x.checked_add(*y).map(Literal::Int),
            (Literal::Str(_), _) | (_, Literal::Str(_)) => None,
            _ => Some(Literal::Num(an() + bn())),
        },
        "-" => match (a, b) {
            (Literal::Int(x), Literal::Int(y)) => x.checked_sub(*y).map(Literal::Int),
            _ => Some(Literal::Num(an() - bn())),
        },
        "*" => match (a, b) {
            (Literal::Int(x), Literal::Int(y)) => x.checked_mul(*y).map(Literal::Int),
            _ => Some(Literal::Num(an() * bn())),
        },
        "/" => {
            let x = an();
            let y = bn();
            (y != 0.0).then_some(Literal::Num(x / y))
        }
        "//" => {
            if let (Literal::Int(x), Literal::Int(y)) = (a, b) {
                if *y != 0 {
                    return Some(Literal::Int(x.div_euclid(*y)));
                }
            }
            (bn() != 0.0).then_some(Literal::Num((an() / bn()).floor()))
        }
        "%" => {
            if let (Literal::Int(x), Literal::Int(y)) = (a, b) {
                if *y != 0 {
                    return Some(Literal::Int(x.rem_euclid(*y)));
                }
                return None;
            }
            let x = an();
            let y = bn();
            (y != 0.0).then_some(Literal::Num(((x % y) + y) % y))
        }
        "^" => Some(Literal::Num(an().powf(bn()))),
        ".." => Some(Literal::Str(format!(
            "{}{}",
            literal_to_js_string(a),
            literal_to_js_string(b)
        ))),
        "==" => Some(Literal::Bool(strict_equal(a, b))),
        "~=" => Some(Literal::Bool(!strict_equal(a, b))),
        "<" | ">" | "<=" | ">=" => Some(Literal::Bool(relational(op, a, b))),
        "&" => match (a, b) {
            (Literal::Int(x), Literal::Int(y)) => Some(Literal::Int(x & y)),
            _ => None,
        },
        "|" => match (a, b) {
            (Literal::Int(x), Literal::Int(y)) => Some(Literal::Int(x | y)),
            _ => None,
        },
        "~" => match (a, b) {
            (Literal::Int(x), Literal::Int(y)) => Some(Literal::Int(x ^ y)),
            _ => None,
        },
        "<<" => match (a, b) {
            (Literal::Int(x), Literal::Int(y)) => {
                Some(Literal::Int(x.wrapping_shl((*y as u64 & 63) as u32)))
            }
            _ => None,
        },
        ">>" => match (a, b) {
            (Literal::Int(x), Literal::Int(y)) => {
                // Lua's right shift is logical (zero-filled), unlike Rust's
                // signed `>>` and JavaScript's 32-bit arithmetic shift.
                Some(Literal::Int(((*x as u64) >> (*y as u64 & 63)) as i64))
            }
            _ => None,
        },
        "and" => Some(if lua_truth(a) { b.clone() } else { a.clone() }),
        "or" => Some(if lua_truth(a) { a.clone() } else { b.clone() }),
        _ => None,
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
    let once = ast.push(Node::Un("not".into(), condition));
    Some(ast.push(Node::Un("not".into(), once)))
}

fn positive_term(ast: &mut Ast, node: NodeId) -> Option<NodeId> {
    let value = without_parens(ast, node);
    match ast.node(value).clone() {
        Node::Num(v) if num_val(&v) < 0.0 => {
            Some(ast.push(Node::Num(short_num(-num_val(&v)).into())))
        }
        Node::Un(op, expression) if op == "-" => Some(expression),
        Node::Bin(op, left, right) if op == "/" => {
            let numerator = positive_term(ast, left)?;
            Some(ast.push(Node::Bin("/".into(), numerator, right)))
        }
        Node::Bin(op, left, right) if op == "*" => {
            let l = without_parens(ast, left);
            let r = without_parens(ast, right);
            if let Node::Num(v) = ast.node(l).clone() {
                if num_val(&v) < 0.0 {
                    let n = ast.push(Node::Num(short_num(-num_val(&v)).into()));
                    return Some(ast.push(Node::Bin("*".into(), n, right)));
                }
            }
            if let Node::Num(v) = ast.node(r).clone() {
                if num_val(&v) < 0.0 {
                    let n = ast.push(Node::Num(short_num(-num_val(&v)).into()));
                    return Some(ast.push(Node::Bin("*".into(), left, n)));
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
    if let Node::Num(n) = ast.node(value).clone() {
        let numerator = num_val(&n);
        if numerator.fract() == 0.0 && numerator.abs() <= i64::MAX as f64 {
            let factor = gcd(numerator as i64, denominator as i64) as f64;
            let rn = numerator / factor;
            let rd = denominator / factor;
            if rd == 1.0 {
                return Some(ast.push(Node::Num(short_num(rn).into())));
            }
            let l = ast.push(Node::Num(short_num(rn).into()));
            let r = ast.push(Node::Num(short_num(rd).into()));
            return Some(ast.push(Node::Bin("/".into(), l, r)));
        }
    }
    Some(ast.push(Node::Bin("/".into(), value, divisor)))
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
    if bit_op != "&" {
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
    let modulo = ast.push(Node::Bin("%".into(), value, two_mask));
    let boundary = ast.push(Node::Num(
        short_num(if op == "~=" { mask - 1.0 } else { mask }).into(),
    ));
    Some(ast.push(Node::Bin(
        if op == "~=" { ">" } else { "<" }.into(),
        modulo,
        boundary,
    )))
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
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    aggressive: bool,
    tolerance: NumericTolerance,
    fold_nonportable_math: bool,
) {
    let origin = target.nodes.capture_origin(node);
    fold_node_inner(
        source,
        target,
        resolution,
        analyzer,
        node,
        aggressive,
        tolerance,
        fold_nonportable_math,
    );
    target
        .nodes
        .finish_rewrite(node, origin, "constant-folding");
}

#[allow(clippy::too_many_arguments)]
fn fold_node_inner(
    source: &Ast,
    target: &mut Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    aggressive: bool,
    tolerance: NumericTolerance,
    fold_nonportable_math: bool,
) {
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        fold_node(
            source,
            target,
            resolution,
            analyzer,
            child,
            aggressive,
            tolerance,
            fold_nonportable_math,
        );
        child
    });
    target.nodes[node as usize] = mapped;

    if let Node::Num(v) = target.node(node).clone() {
        // Keep the lexical float spelling in the AST.  The ordinary printer
        // may still shorten it, while context-sensitive uses such as
        // `math.type(1.0)` and `-0.0` can preserve Lua's subtype/sign.
        let unsafe_integer =
            integer_literal_value(&v).is_some_and(|value| value.unsigned_abs() > (1u64 << 53));
        if !(v.contains('.') || v.contains('e') || v.contains('E')) && !unsafe_integer {
            target.nodes[node as usize] = Node::Num(normalize_num_literal(&v).into());
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
                let decoded = decode_lua_string(&format);
                if decoded.starts_with("%0") && decoded.ends_with('d') {
                    let width_text = &decoded[2..decoded.len() - 1];
                    if !width_text.is_empty()
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
        if let Some(value) = literal_value(target, expression) {
            if let Some(value) = eval_unary(&op, &value) {
                if let Some(candidate) =
                    aggressive_literal_node(target, value, aggressive, tolerance)
                {
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
                    if let Some(candidate) =
                        aggressive_literal_node(target, value, aggressive, tolerance)
                    {
                        if measure_expr(target, candidate) <= measure_expr(target, node) {
                            target.nodes[node as usize] = target.node(candidate).clone();
                            return;
                        }
                    }
                }
            }
        }
        if op == "and" {
            if let Some(a) = &a {
                target.nodes[node as usize] =
                    target.node(if lua_truth(a) { right } else { left }).clone();
                return;
            }
        }
        if op == "or" {
            if let Some(a) = &a {
                target.nodes[node as usize] =
                    target.node(if lua_truth(a) { left } else { right }).clone();
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
        if matches!(op.as_str(), "+" | "-") && is_zero(target, right) {
            target.nodes[node as usize] = target.node(left).clone();
            return;
        }
        if op == "+" && is_zero(target, left) {
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
        if op == "*" && is_one(target, right) {
            target.nodes[node as usize] = target.node(left).clone();
            return;
        }
        if op == "*" && is_one(target, left) {
            target.nodes[node as usize] = target.node(right).clone();
            return;
        }
        if op == "/" && is_one(target, right) {
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
            && original_child_movable(analyzer, source, target, orig_left, left)
        {
            target.nodes[node as usize] = target.node(right).clone();
            return;
        }
        if aggressive
            && op == "*"
            && is_zero(target, left)
            && original_child_movable(analyzer, source, target, orig_right, right)
        {
            target.nodes[node as usize] = target.node(left).clone();
            return;
        }
        if aggressive
            && op == "-"
            && semantic_same(target, resolution, left, right)
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
                            && original_child_movable(analyzer, source, target, orig_left, left)
                        {
                            let one = target.push(Node::Num("1".into()));
                            let sub = target.push(Node::Bin("-".into(), one, term));
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
                    let values: Vec<f64> = args
                        .iter()
                        .map(|a| js_to_number(&literal_value(target, *a).unwrap()))
                        .collect();
                    let value = match builtin.as_str() {
                        "math.min" if !values.is_empty() => {
                            Some(values.iter().copied().fold(f64::INFINITY, f64::min))
                        }
                        "math.max" if !values.is_empty() => {
                            Some(values.iter().copied().fold(f64::NEG_INFINITY, f64::max))
                        }
                        "math.abs" if values.len() == 1 => Some(values[0].abs()),
                        "math.floor" if values.len() == 1 => Some(values[0].floor()),
                        "math.sqrt" if values.len() == 1 => Some(values[0].sqrt()),
                        "math.sin" if values.len() == 1 => Some(values[0].sin()),
                        "math.cos" if values.len() == 1 => Some(values[0].cos()),
                        "math.tan" if values.len() == 1 => Some(values[0].tan()),
                        "math.atan" if values.len() == 1 => Some(values[0].atan()),
                        "math.atan" if values.len() == 2 => Some(values[0].atan2(values[1])),
                        _ => None,
                    };
                    if let Some(value) = value {
                        if let Some(candidate) = aggressive_literal_node(
                            target,
                            Literal::Num(value),
                            aggressive,
                            tolerance,
                        ) {
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
    let resolution = resolve(ast, root);
    let analyzer = EffectAnalyzer::new(ast, &resolution, root, true);
    let mut target = clone_ast(ast);
    fold_node(
        ast,
        &mut target,
        &resolution,
        &analyzer,
        root,
        aggressive,
        tolerance,
        fold_nonportable_math,
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

fn simplify_node(source: &Ast, target: &mut Ast, node: NodeId) {
    let origin = target.nodes.capture_origin(node);
    simplify_node_inner(source, target, node);
    target
        .nodes
        .finish_rewrite(node, origin, "control-flow-simplification");
}

fn simplify_node_inner(source: &Ast, target: &mut Ast, node: NodeId) {
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        simplify_node(source, target, child);
        child
    });
    target.nodes[node as usize] = mapped;

    if let Node::If(arms, else_block) = target.node(node).clone() {
        let mut kept = Vec::new();
        let mut unresolved = false;
        for arm in arms {
            match literal_value(target, arm.cond) {
                Some(value) if !lua_truth(&value) => {
                    // A known-false arm is unreachable only after all previous
                    // conditions have also been resolved.
                }
                Some(_) if !unresolved => {
                    target.nodes[node as usize] = target.node(arm.body).clone();
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
                target.nodes[node as usize] = target.node(else_block).clone();
            } else {
                target.nodes[node as usize] = Node::Block(Vec::new());
            }
            return;
        }
        target.nodes[node as usize] = Node::If(kept, else_block);
    }

    if let Node::Block(statements) = target.node(node).clone() {
        let mut output = Vec::new();
        for statement in statements {
            match target.node(statement).clone() {
                Node::Block(children) => output.extend(children),
                Node::Do(body) if matches!(target.node(body), Node::Block(ss) if ss.is_empty()) => {
                }
                _ => output.push(statement),
            }
        }
        target.nodes[node as usize] = Node::Block(output);
    }
}

pub fn simplify_control_flow(ast: &Ast, root: NodeId, _aggressive: bool) -> (Ast, NodeId) {
    let mut target = clone_ast(ast);
    simplify_node(ast, &mut target, root);
    (target, root)
}

fn fold_exact_safe_node(ast: &mut Ast, node: NodeId) {
    let origin = ast.nodes.capture_origin(node);
    fold_exact_safe_node_inner(ast, node);
    ast.nodes
        .finish_rewrite(node, origin, "exact-control-flow-folding");
}

fn fold_exact_safe_node_inner(ast: &mut Ast, node: NodeId) {
    let original = ast.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        fold_exact_safe_node(ast, child);
        child
    });
    ast.nodes[node as usize] = mapped;

    match ast.node(node).clone() {
        Node::Un(op, expression) if op == "not" => {
            if let Some(value) = literal_value(ast, expression) {
                ast.nodes[node as usize] = Node::Bool(!lua_truth(&value));
            }
        }
        Node::Bin(op, left, right) if op == "and" || op == "or" => {
            if op == "or" {
                if let Some(candidate) = boolean_coercion(ast, node) {
                    if measure_expr(ast, candidate) < measure_expr(ast, node) {
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
                ast.nodes[node as usize] = ast.node(selected).clone();
            }
        }
        Node::If(arms, else_block) => {
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
                ast.nodes[node as usize] = Node::Do(body);
            } else if kept.is_empty() {
                ast.nodes[node as usize] = if let Some(body) = replacement_else {
                    Node::Do(body)
                } else {
                    Node::Block(Vec::new())
                };
            } else {
                ast.nodes[node as usize] = Node::If(kept, replacement_else);
            }
        }
        Node::Block(statements) => {
            let output = statements
                .into_iter()
                .filter(|statement| {
                    !matches!(ast.node(*statement), Node::Block(children) if children.is_empty())
                })
                .collect();
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
    fold_exact_safe_node(ast, root);
    let after = storm_lua_syntax::size::measure_size(ast, root);
    PassResult {
        root,
        saved: Some(before.saturating_sub(after) as u64),
        details: None,
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
    use storm_lua_syntax::parser::parse_source;
    use storm_lua_syntax::print::Printer;

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
}
