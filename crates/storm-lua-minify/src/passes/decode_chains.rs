//! Quotient-chain fusion (`passes/decode-chains.ts`).
//!
//! Consecutive or separated assignments of the form `x=x//a; x=x//b` can be
//! replaced with `x=x//(a*b)` when no intervening statement references the
//! binding and the product remains a JavaScript safe integer. The TypeScript
//! pass applies at most one rewrite per block in each round and repeats for up
//! to 16 rounds; nested blocks are processed before their parent block.

use crate::pass::PassResult;
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::numeric::{num_val, short_num};
use storm_lua_syntax::print::Printer;
use storm_lua_syntax::size::measure_size;

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

#[derive(Clone, Copy)]
struct QuotientAssignment {
    target: NodeId,
    target_bid: BindingId,
    lhs: NodeId,
    divisor: f64,
    divisor_node: NodeId,
}

fn quotient_assignment(
    ast: &Ast,
    res: &Resolution,
    statement: NodeId,
) -> Option<QuotientAssignment> {
    let Node::Assign(vs, es) = ast.node(statement) else {
        return None;
    };
    if vs.len() != 1 || es.len() != 1 || !matches!(ast.node(vs[0]), Node::Name(_)) {
        return None;
    }
    let Node::Bin(op, lhs, rhs) = ast.node(es[0]) else {
        return None;
    };
    if op != "//" || !matches!(ast.node(*lhs), Node::Name(_)) {
        return None;
    }
    let target_bid = res.node_bid.get(vs[0] as usize).copied().flatten()?;
    if res.node_bid.get(*lhs as usize).copied().flatten()? != target_bid {
        return None;
    }
    let Node::Num(raw) = ast.node(*rhs) else {
        return None;
    };
    let divisor = num_val(raw);
    if !divisor.is_finite()
        || divisor.fract() != 0.0
        || divisor <= 0.0
        || divisor > MAX_SAFE_INTEGER
    {
        return None;
    }
    Some(QuotientAssignment {
        target: vs[0],
        target_bid,
        lhs: *lhs,
        divisor,
        divisor_node: *rhs,
    })
}

fn references_binding(ast: &Ast, res: &Resolution, statement: NodeId, bid: BindingId) -> bool {
    let mut found = false;
    storm_lua_syntax::ast_utils::walk(ast, statement, &mut |id| {
        if !found
            && matches!(ast.node(id), Node::Name(_))
            && res.node_bid.get(id as usize).copied().flatten() == Some(bid)
        {
            found = true;
        }
    });
    found
}

fn contains_call(ast: &Ast, statement: NodeId) -> bool {
    let mut found = false;
    storm_lua_syntax::ast_utils::walk(ast, statement, &mut |id| {
        if matches!(ast.node(id), Node::Call(..)) {
            found = true;
        }
    });
    found
}

fn measure_stat(ast: &Ast, statement: NodeId) -> usize {
    Printer::new(ast, false).stat_public(statement).len()
}

fn make_quotient_assignment(
    ast: &mut Ast,
    target: NodeId,
    lhs: NodeId,
    divisor: f64,
    statements: [NodeId; 2],
    divisor_sources: &[NodeId],
) -> NodeId {
    let original = ast.nodes.capture_origin(statements[0]);
    // A computed product must not masquerade as fully traced when one of its
    // factors lost attribution in an earlier, unannotated transformation.
    let literal = divisor_sources
        .iter()
        .all(|&id| ast.nodes.origin(id).is_some())
        .then(|| ast.nodes.capture_origin(divisor_sources[0]))
        .flatten();
    let divisor_node = ast.num(short_num(divisor));
    ast.nodes
        .finish_rewrite(divisor_node, literal, "quotient-chain-divisor");
    for &source in &divisor_sources[1..] {
        ast.nodes
            .relate_within(divisor_node, source, "quotient-chain-divisor");
    }
    let expression = ast.bin("//", lhs, divisor_node);
    ast.nodes
        .finish_rewrite(expression, original.clone(), "quotient-chain-fusion");
    ast.nodes
        .relate_within(expression, statements[1], "quotient-chain-fusion");
    let assignment = ast.assign(vec![target], vec![expression]);
    ast.nodes
        .finish_rewrite(assignment, original, "quotient-chain-fusion");
    ast.nodes
        .relate_within(assignment, statements[1], "quotient-chain-fusion");
    assignment
}

/// Rewrites all nested blocks, then the block itself. This mirrors TS
/// `mapBlocks(block, transformBlock)` followed by the current-block scan.
fn transform_node(
    ast: &mut Ast,
    res: &Resolution,
    id: NodeId,
    fused: &mut usize,
    any_applied: &mut bool,
) -> NodeId {
    if matches!(ast.node(id), Node::Block(_)) {
        return transform_block(ast, res, id, fused, any_applied);
    }
    let node = ast.node(id).clone();
    let (mapped, changed) = storm_lua_syntax::ast_utils::map_children(&node, &mut |child| {
        transform_node(ast, res, child, fused, any_applied)
    });
    if changed {
        ast.nodes.rewrite(id, mapped, "quotient-chain-fusion");
    }
    id
}

fn transform_block(
    ast: &mut Ast,
    res: &Resolution,
    block_id: NodeId,
    fused: &mut usize,
    any_applied: &mut bool,
) -> NodeId {
    let Node::Block(original_statements) = ast.node(block_id).clone() else {
        unreachable!("transform_block requires a block")
    };
    let mut statements = original_statements
        .into_iter()
        .map(|statement| transform_node(ast, res, statement, fused, any_applied))
        .collect::<Vec<_>>();
    ast.nodes.rewrite(
        block_id,
        Node::Block(statements.clone()),
        "quotient-chain-fusion",
    );

    for first in 0..statements.len() {
        if let Some(quotient) = quotient_assignment(ast, res, statements[first]) {
            for second in (first + 1)..statements.len() {
                if let Some(next) = quotient_assignment(ast, res, statements[second]) {
                    if next.target_bid == quotient.target_bid {
                        let product = quotient.divisor * next.divisor;
                        if !product.is_finite()
                            || product.fract() != 0.0
                            || product > MAX_SAFE_INTEGER
                        {
                            break;
                        }
                        let replacement = make_quotient_assignment(
                            ast,
                            quotient.target,
                            quotient.lhs,
                            product,
                            [statements[first], statements[second]],
                            &[quotient.divisor_node, next.divisor_node],
                        );
                        let old_length = measure_stat(ast, statements[first])
                            + measure_stat(ast, statements[second]);
                        let new_length = measure_stat(ast, replacement);
                        if new_length < old_length {
                            statements[first] = replacement;
                            statements.remove(second);
                            ast.nodes.rewrite(
                                block_id,
                                Node::Block(statements),
                                "quotient-chain-fusion",
                            );
                            *fused += 1;
                            *any_applied = true;
                            return block_id;
                        }
                        break;
                    }
                }
                // A call may observe the binding indirectly through a global
                // or closure; name-only scanning cannot see that dependency.
                // Stop conservatively at every call between quotient updates.
                if contains_call(ast, statements[second]) {
                    break;
                }
                if references_binding(ast, res, statements[second], quotient.target_bid) {
                    break;
                }
            }
        }

        if first + 1 >= statements.len() {
            continue;
        }
        let assignment_id = statements[first];
        let Some(next) = quotient_assignment(ast, res, statements[first + 1]) else {
            continue;
        };
        let Node::Assign(vs, es) = ast.node(assignment_id).clone() else {
            continue;
        };
        if vs.len() != 1 || es.len() != 1 || !matches!(ast.node(vs[0]), Node::Name(_)) {
            continue;
        }
        if res.node_bid.get(vs[0] as usize).copied().flatten() != Some(next.target_bid) {
            continue;
        }
        let replacement = make_quotient_assignment(
            ast,
            vs[0],
            es[0],
            next.divisor,
            [assignment_id, statements[first + 1]],
            &[next.divisor_node],
        );
        let old_length =
            measure_stat(ast, assignment_id) + measure_stat(ast, statements[first + 1]);
        let new_length = measure_stat(ast, replacement);
        if new_length >= old_length {
            continue;
        }
        statements.splice(first..=first + 1, [replacement]);
        ast.nodes
            .rewrite(block_id, Node::Block(statements), "quotient-chain-fusion");
        *fused += 1;
        *any_applied = true;
        return block_id;
    }

    ast.nodes
        .rewrite(block_id, Node::Block(statements), "quotient-chain-fusion");
    block_id
}

pub fn fuse_quotient_chains_with_rounds(
    ast: &mut Ast,
    root: NodeId,
    max_rounds: usize,
) -> PassResult {
    let original = measure_size(ast, root);
    let mut fused = 0usize;
    for _ in 0..max_rounds {
        let resolution = resolve(ast, root);
        let mut applied = false;
        transform_node(ast, &resolution, root, &mut fused, &mut applied);
        if !applied {
            break;
        }
    }
    PassResult {
        root,
        saved: Some(original.saturating_sub(measure_size(ast, root)) as u64),
        details: if fused == 0 {
            Some(Vec::new())
        } else {
            Some(vec![format!("fused={fused}")])
        },
    }
}

pub fn fuse_quotient_chains(ast: &mut Ast, root: NodeId) -> PassResult {
    fuse_quotient_chains_with_rounds(ast, root, 16)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use storm_lua_syntax::parser::parse_source;

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = fuse_quotient_chains(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn fuses_same_binding_quotients() {
        assert_eq!(output("x=x//10\nx=x//10"), "x=x//100");
    }

    #[test]
    fn folds_assignment_followed_by_quotient() {
        assert_eq!(output("x=a+b\nx=x//10"), "x=(a+b)//10");
    }

    #[test]
    fn stops_at_intervening_binding_reference() {
        let out = output("x=x//10\ny=x\nx=x//10");
        assert_eq!(out.matches("x=x//10").count(), 2, "{out}");
    }

    #[test]
    fn does_not_multiply_an_unsafe_product() {
        let out = output("x=x//9007199254740991\nx=x//2");
        assert_eq!(out, "x=x//9007199254740991//2");
        assert!(!out.contains("18014398509481982"));
    }
}
