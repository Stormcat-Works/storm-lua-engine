//! Quotient/remainder fusion (`passes/misc.ts::fuseQuotientRemainder`).
//!
//! Rewrites adjacent statements of the form
//! `r=x%d; x=(x-r)/d` into `r=x%d; x=x//d`. Nested blocks are processed
//! before their parent block, matching TypeScript `mapBlocks` order. The
//! transformed tree is accepted only when its compact byte size decreases.

use crate::pass::PassResult;
use storm_lua_analysis::effects::ast_same;
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::size::measure_size;

fn unparen(ast: &Ast, mut id: NodeId) -> NodeId {
    while let Node::Paren(inner) = ast.node(id) {
        id = *inner;
    }
    id
}

fn node_bid(res: &Resolution, id: NodeId) -> Option<BindingId> {
    res.node_bid.get(id as usize).copied().flatten()
}

fn first_assignment(ast: &Ast, res: &Resolution, statement: NodeId) -> Option<(BindingId, NodeId)> {
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

fn matching_pair(
    ast: &Ast,
    res: &Resolution,
    first: NodeId,
    second: NodeId,
) -> Option<(NodeId, NodeId, NodeId)> {
    let (remainder_bid, first_expression) = first_assignment(ast, res, first)?;
    let Node::Assign(second_targets, second_expressions) = ast.node(second) else {
        return None;
    };
    if second_targets.len() != 1
        || second_expressions.len() != 1
        || !matches!(ast.node(second_targets[0]), Node::Name(_))
    {
        return None;
    }
    let source_bid = node_bid(res, second_targets[0])?;
    if source_bid == remainder_bid {
        return None;
    }

    let modulo = unparen(ast, first_expression);
    let quotient = unparen(ast, second_expressions[0]);
    let Node::Bin(modulo_op, modulo_lhs, modulo_rhs) = ast.node(modulo) else {
        return None;
    };
    if modulo_op != "%"
        || !matches!(ast.node(*modulo_lhs), Node::Name(_))
        || node_bid(res, *modulo_lhs) != Some(source_bid)
        || !matches!(ast.node(*modulo_rhs), Node::Num(_))
    {
        return None;
    }

    let Node::Bin(quotient_op, quotient_lhs, quotient_rhs) = ast.node(quotient) else {
        return None;
    };
    if quotient_op != "/" || !ast_same(ast, *modulo_rhs, *quotient_rhs) {
        return None;
    }
    let subtraction = unparen(ast, *quotient_lhs);
    let Node::Bin(subtraction_op, subtraction_lhs, subtraction_rhs) = ast.node(subtraction) else {
        return None;
    };
    if subtraction_op != "-"
        || !matches!(ast.node(*subtraction_lhs), Node::Name(_))
        || node_bid(res, *subtraction_lhs) != Some(source_bid)
        || !matches!(ast.node(*subtraction_rhs), Node::Name(_))
        || node_bid(res, *subtraction_rhs) != Some(remainder_bid)
    {
        return None;
    }

    Some((second_targets[0], *modulo_lhs, *modulo_rhs))
}

fn transform_node(ast: &mut Ast, res: &Resolution, id: NodeId, fused: &mut usize) -> NodeId {
    if matches!(ast.node(id), Node::Block(_)) {
        return transform_block(ast, res, id, fused);
    }
    let node = ast.node(id).clone();
    let (mapped, changed) = storm_lua_syntax::ast_utils::map_children(&node, &mut |child| {
        transform_node(ast, res, child, fused)
    });
    if changed {
        ast.nodes.rewrite(id, mapped, "quotient-remainder-fusion");
    }
    id
}

fn transform_block(ast: &mut Ast, res: &Resolution, block_id: NodeId, fused: &mut usize) -> NodeId {
    let Node::Block(statements) = ast.node(block_id).clone() else {
        unreachable!("transform_block requires a block")
    };
    let nested = statements
        .into_iter()
        .map(|statement| transform_node(ast, res, statement, fused))
        .collect::<Vec<_>>();
    let mut output = Vec::with_capacity(nested.len());
    let mut index = 0usize;
    while index < nested.len() {
        let first = nested[index];
        if let Some(&second) = nested.get(index + 1) {
            if let Some((target, source, divisor)) = matching_pair(ast, res, first, second) {
                let floor_division = ast.bin("//", source, divisor);
                let Node::Assign(_, expressions) = ast.node(second) else {
                    unreachable!()
                };
                let original_expression = expressions[0];
                super::origins::within(
                    ast,
                    floor_division,
                    &[original_expression, source, divisor],
                    "quotient-remainder-fusion",
                );
                ast.nodes
                    .relate_within(floor_division, first, "quotient-remainder-fusion");
                let replacement = ast.assign(vec![target], vec![floor_division]);
                super::origins::within(ast, replacement, &[second], "quotient-remainder-fusion");
                output.push(first);
                output.push(replacement);
                *fused += 1;
                index += 2;
                continue;
            }
        }
        output.push(first);
        index += 1;
    }
    ast.nodes
        .rewrite(block_id, Node::Block(output), "quotient-remainder-fusion");
    block_id
}

pub fn fuse_quotient_remainder_with_options(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
) -> PassResult {
    if !aggressive {
        return PassResult {
            root,
            saved: Some(0),
            details: Some(Vec::new()),
        };
    }
    let original_size = measure_size(ast, root);
    let mut candidate = storm_lua_syntax::ast_utils::inherit_ast(ast);
    candidate.nodes = ast.nodes.clone();
    let resolution = resolve(&candidate, root);
    let mut fused = 0usize;
    transform_node(&mut candidate, &resolution, root, &mut fused);
    let candidate_size = measure_size(&candidate, root);
    let accepted = candidate_size < original_size;
    if accepted {
        ast.nodes = candidate.nodes;
        ast.strings = candidate.strings;
    }
    PassResult {
        root,
        saved: Some(if accepted {
            (original_size - candidate_size) as u64
        } else {
            0
        }),
        details: if fused == 0 {
            Some(Vec::new())
        } else {
            Some(vec![format!("fused={fused}")])
        },
    }
}

pub fn fuse_quotient_remainder(ast: &mut Ast, root: NodeId) -> PassResult {
    fuse_quotient_remainder_with_options(ast, root, true)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = fuse_quotient_remainder(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn fuses_adjacent_quotient_and_remainder() {
        assert_eq!(output("r=x%10\nx=(x-r)/10"), "r=x%10 x=x//10");
    }

    #[test]
    fn supports_local_remainder_binding() {
        assert_eq!(output("local r=x%16\nx=(x-r)/16"), "local r=x%16 x=x//16");
    }

    #[test]
    fn requires_same_literal_divisor() {
        assert_eq!(output("r=x%10\nx=(x-r)/8"), "r=x%10 x=(x-r)/8");
    }

    #[test]
    fn requires_adjacent_statements() {
        assert_eq!(output("r=x%10\ny=1\nx=(x-r)/10"), "r=x%10 y=1 x=(x-r)/10");
    }

    #[test]
    fn safe_mode_does_not_fuse_quotient_remainder() {
        let (mut ast, root) = parse_source("r=x%10\nx=(x-r)/10").expect("parse");
        let result = fuse_quotient_remainder_with_options(&mut ast, root, false);
        assert_eq!(
            Printer::new(&ast, false).output(result.root),
            "r=x%10 x=(x-r)/10"
        );
    }
}
