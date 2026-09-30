//! Split-sign recomposition elimination (`passes/algebraic-sequences.ts`).
//!
//! Two related rewrites are implemented:
//! - `p=math.max(x,0); x=math.min(x,0); x=p+x` is removed when the old `p`
//!   value cannot be observed later, because the sequence recomposes `x`.
//! - `math.max(x,0)^2*k-math.min(x,0)^2*k` becomes `x*math.abs(x)*k`
//!   when the replacement is shorter.

use crate::pass::PassResult;
use storm_lua_analysis::effects::{ast_same, EffectAnalyzer};
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::numeric::num_val;
use storm_lua_syntax::size::{measure_expr, measure_size};

#[derive(Clone, Copy)]
struct SignedPart {
    target: BindingId,
    value: NodeId,
}

#[derive(Clone, Copy)]
struct SquaredPart {
    value: NodeId,
    factor: Option<NodeId>,
}

fn zero(ast: &Ast, node: NodeId) -> bool {
    matches!(ast.node(node), Node::Num(raw) if num_val(raw) == 0.0)
}

fn node_bid(res: &Resolution, node: NodeId) -> Option<BindingId> {
    res.node_bid.get(node as usize).copied().flatten()
}

fn statement_target_and_expression(
    ast: &Ast,
    res: &Resolution,
    statement: NodeId,
) -> Option<(BindingId, NodeId)> {
    match ast.node(statement) {
        Node::Assign(vs, es)
            if vs.len() == 1 && es.len() == 1 && matches!(ast.node(vs[0]), Node::Name(_)) =>
        {
            Some((node_bid(res, vs[0])?, es[0]))
        }
        Node::Local(names, es) if names.len() == 1 && es.len() == 1 => {
            let bids = res.node_bids.get(statement as usize)?;
            (bids.len() == 1).then_some((bids[0], es[0]))
        }
        _ => None,
    }
}

fn call_operation(
    ast: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    operation: &str,
) -> Option<NodeId> {
    let Node::Call(function, arguments, _) = ast.node(node) else {
        return None;
    };
    if arguments.len() != 2
        || analyzer.resolve_builtin_reference(*function).as_deref()
            != Some(&format!("math.{operation}"))
    {
        return None;
    }
    if zero(ast, arguments[0]) {
        Some(arguments[1])
    } else if zero(ast, arguments[1]) {
        Some(arguments[0])
    } else {
        None
    }
}

fn signed_part(
    ast: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    statement: NodeId,
    operation: &str,
) -> Option<SignedPart> {
    let (target, expression) = statement_target_and_expression(ast, res, statement)?;
    let value = call_operation(ast, analyzer, expression, operation)?;
    Some(SignedPart { target, value })
}

fn definitely_writes(ast: &Ast, res: &Resolution, node: NodeId, bid: BindingId) -> bool {
    match ast.node(node) {
        Node::Assign(targets, _) => targets.iter().any(|target| {
            matches!(ast.node(*target), Node::Name(_)) && node_bid(res, *target) == Some(bid)
        }),
        Node::Block(statements) => statements
            .iter()
            .any(|statement| definitely_writes(ast, res, *statement, bid)),
        Node::If(arms, Some(else_block)) => {
            arms.iter()
                .all(|arm| definitely_writes(ast, res, arm.body, bid))
                && definitely_writes(ast, res, *else_block, bid)
        }
        _ => false,
    }
}

fn old_value_is_read(ast: &Ast, res: &Resolution, statements: &[NodeId], bid: BindingId) -> bool {
    for statement in statements {
        let mut read = false;
        storm_lua_syntax::ast_utils::walk(ast, *statement, &mut |node| {
            if !read
                && matches!(ast.node(node), Node::Name(_))
                && node_bid(res, node) == Some(bid)
                && !res.node_write.get(node as usize).copied().unwrap_or(false)
            {
                read = true;
            }
        });
        if read {
            return true;
        }
        if definitely_writes(ast, res, *statement, bid) {
            return false;
        }
    }
    false
}

fn transform_blocks_node(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    eliminated: &mut usize,
) -> NodeId {
    if matches!(source.node(node), Node::Block(_)) {
        return transform_block(target, source, res, analyzer, node, eliminated);
    }
    let original = source.node(node).clone();
    let (mapped, changed) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        transform_blocks_node(target, source, res, analyzer, child, eliminated)
    });
    if changed {
        target
            .nodes
            .rewrite(node, mapped, "split-sign-recomposition-elimination");
    }
    node
}

fn transform_block(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    block: NodeId,
    eliminated: &mut usize,
) -> NodeId {
    let Node::Block(statements) = source.node(block).clone() else {
        unreachable!("transform_block requires a block")
    };
    let nested = statements
        .into_iter()
        .map(|statement| {
            transform_blocks_node(target, source, res, analyzer, statement, eliminated)
        })
        .collect::<Vec<_>>();
    let mut output = Vec::with_capacity(nested.len());
    let mut index = 0usize;
    while index < nested.len() {
        if index + 2 < nested.len() {
            let first = nested[index];
            let second = nested[index + 1];
            let third = nested[index + 2];
            let positive = signed_part(target, res, analyzer, first, "max");
            let negative = signed_part(target, res, analyzer, second, "min");
            if let (Some(positive), Some(negative)) = (positive, negative) {
                let negative_value_is_target = matches!(target.node(negative.value), Node::Name(_))
                    && node_bid(res, negative.value) == Some(negative.target);
                let recomposes = match target.node(third) {
                    Node::Assign(targets, expressions)
                        if targets.len() == 1
                            && expressions.len() == 1
                            && matches!(target.node(targets[0]), Node::Name(_))
                            && node_bid(res, targets[0]) == Some(negative.target) =>
                    {
                        match target.node(expressions[0]) {
                            Node::Bin(op, left, right) if op == "+" => {
                                let term_bid = |term: NodeId| {
                                    matches!(target.node(term), Node::Name(_))
                                        .then(|| node_bid(res, term))
                                        .flatten()
                                };
                                let left_bid = term_bid(*left);
                                let right_bid = term_bid(*right);
                                [left_bid, right_bid].contains(&Some(positive.target))
                                    && [left_bid, right_bid].contains(&Some(negative.target))
                            }
                            _ => false,
                        }
                    }
                    _ => false,
                };
                if positive.target != negative.target
                    && negative_value_is_target
                    && ast_same(target, positive.value, negative.value)
                    && recomposes
                    && !old_value_is_read(target, res, &nested[index + 3..], positive.target)
                {
                    *eliminated += 1;
                    index += 3;
                    continue;
                }
            }
        }
        output.push(nested[index]);
        index += 1;
    }
    target.nodes.rewrite(
        block,
        Node::Block(output),
        "split-sign-recomposition-elimination",
    );
    block
}

fn squared_part(
    ast: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    operation: &str,
) -> Option<SquaredPart> {
    let (power, factor) = match ast.node(node) {
        Node::Bin(op, left, right) if op == "*" => {
            if matches!(ast.node(*left), Node::Bin(power_op, _, _) if power_op == "^") {
                (*left, Some(*right))
            } else if matches!(ast.node(*right), Node::Bin(power_op, _, _) if power_op == "^") {
                (*right, Some(*left))
            } else {
                return None;
            }
        }
        Node::Bin(op, _, _) if op == "^" => (node, None),
        _ => return None,
    };
    let Node::Bin(_, base, exponent) = ast.node(power) else {
        return None;
    };
    if !matches!(ast.node(*exponent), Node::Num(raw) if num_val(raw) == 2.0) {
        return None;
    }
    let value = call_operation(ast, analyzer, *base, operation)?;
    Some(SquaredPart { value, factor })
}

fn expression_size(ast: &Ast, node: NodeId) -> usize {
    measure_expr(ast, node)
}

fn transform_squares_node(
    target: &mut Ast,
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    aggressive: bool,
    abs_alias: Option<&str>,
    squared: &mut usize,
) -> NodeId {
    let original = source.node(node).clone();
    let (mapped, changed) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        transform_squares_node(
            target, source, analyzer, child, aggressive, abs_alias, squared,
        )
    });
    if changed {
        target
            .nodes
            .rewrite(node, mapped, "split-sign-recomposition-elimination");
    }
    if !aggressive {
        return node;
    }
    let Node::Bin(op, left, right) = target.node(node).clone() else {
        return node;
    };
    if op != "-" {
        return node;
    }
    let Some(positive) = squared_part(target, analyzer, left, "max") else {
        return node;
    };
    let Some(negative) = squared_part(target, analyzer, right, "min") else {
        return node;
    };
    if !ast_same(target, positive.value, negative.value)
        || positive.factor.is_some() != negative.factor.is_some()
        || positive
            .factor
            .zip(negative.factor)
            .is_some_and(|(a, b)| !ast_same(target, a, b))
    {
        return node;
    }

    let abs_function = if let Some(alias) = abs_alias {
        target.name(alias)
    } else {
        let math = target.name("math");
        let key = target.str("\"abs\"".to_string());
        target.index(math, key, true)
    };
    let abs_call = target.call(abs_function, vec![positive.value], None);
    let mut replacement = target.bin("*", positive.value, abs_call);
    if let Some(factor) = positive.factor {
        replacement = target.bin("*", replacement, factor);
    }
    if expression_size(target, replacement) >= expression_size(target, node) {
        return node;
    }
    *squared += 1;
    replacement
}

pub fn eliminate_split_sign_recomposition(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
) -> PassResult {
    let original_size = measure_size(ast, root);
    let mut source = storm_lua_syntax::ast_utils::inherit_ast(ast);
    source.nodes = ast.nodes.clone();
    let resolution = resolve(&source, root);
    let analyzer = EffectAnalyzer::new(&source, &resolution, root, true);

    let mut abs_alias: Option<String> = None;
    storm_lua_syntax::ast_utils::walk(&source, root, &mut |node| {
        if let Node::Name(symbol) = source.node(node) {
            if analyzer.resolve_builtin_reference(node).as_deref() == Some("math.abs") {
                let name = source.strings.get(*symbol);
                if abs_alias
                    .as_ref()
                    .is_none_or(|best| name.len() < best.len())
                {
                    abs_alias = Some(name.to_string());
                }
            }
        }
    });

    let mut after_blocks = storm_lua_syntax::ast_utils::inherit_ast(&source);
    after_blocks.nodes = source.nodes.clone();
    let mut eliminated = 0usize;
    transform_blocks_node(
        &mut after_blocks,
        &source,
        &resolution,
        &analyzer,
        root,
        &mut eliminated,
    );

    let mut candidate = storm_lua_syntax::ast_utils::inherit_ast(&after_blocks);
    candidate.nodes = after_blocks.nodes.clone();
    let mut squared = 0usize;
    let candidate_root = transform_squares_node(
        &mut candidate,
        &after_blocks,
        &analyzer,
        root,
        aggressive,
        abs_alias.as_deref(),
        &mut squared,
    );
    ast.nodes = candidate.nodes;
    ast.strings = candidate.strings;
    PassResult {
        root: candidate_root,
        saved: Some(original_size.saturating_sub(measure_size(ast, candidate_root)) as u64),
        details: Some(vec![
            format!("eliminated={eliminated}"),
            format!("squared={squared}"),
        ]),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use storm_lua_syntax::parser::parse_source;
    use storm_lua_syntax::print::Printer;

    fn output(source: &str, aggressive: bool) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = eliminate_split_sign_recomposition(&mut ast, root, aggressive);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn removes_split_and_recomposition_sequence() {
        let source = "function onTick()local x=input.getNumber(1)local p=math.max(x,0)x=math.min(x,0)x=p+x output.setNumber(1,x)end";
        assert_eq!(
            output(source, true),
            "function onTick()local x=input.getNumber(1)output.setNumber(1,x)end"
        );
    }

    #[test]
    fn keeps_sequence_when_positive_part_is_read_later() {
        let source =
            "x=input.getNumber(1)p=math.max(x,0)x=math.min(x,0)x=p+x output.setNumber(1,p)";
        assert!(output(source, true).contains("p=math.max"));
    }

    #[test]
    fn rewrites_squared_split_parts_through_abs() {
        let source = "x=input.getNumber(1)y=math.max(x,0)^2*3-math.min(x,0)^2*3";
        assert_eq!(
            output(source, true),
            "x=input.getNumber(1)y=x*math.abs(x)*3"
        );
    }

    #[test]
    fn squared_rewrite_requires_aggressive_mode() {
        let source = "x=input.getNumber(1)y=math.max(x,0)^2*3-math.min(x,0)^2*3";
        assert!(output(source, false).contains("math.max"));
    }
}
