//! Negated comparison inversion.
//!
//! `not (a == b)` is exactly `a ~= b`, and `not (a ~= b)` is exactly
//! `a == b`: in Lua, `~=` is defined as the negation of `==`, including
//! when the `__eq` metamethod runs (its boolean result is negated) and
//! when either side is NaN (`NaN == NaN` is false, `NaN ~= NaN` is
//! true). The rewrite therefore holds for every operand type, with the
//! same evaluation count and order (both forms evaluate each operand
//! exactly once, left to right), and it needs no type proof.
//!
//! Ordering comparisons are deliberately NOT inverted: `not (a < b)` is
//! not `a >= b` when NaN is involved (`NaN < x` and `NaN >= x` are both
//! false), so inverting `<`, `>`, `<=`, or `>=` would change meaning.
//!
//! A parenthesized operand is stripped together with the `not`: a
//! comparison always yields exactly one value, so the single-value
//! adjustment that parentheses impose is a no-op for it. The printer
//! re-adds parentheses wherever the surrounding precedence requires
//! them, so the rewritten form is never shorter by less than the
//! removed `not` itself: `not(a==b)` (inner + 5) becomes `a~=b`
//! (inner + 0) in a flat context, and keeps a positive saving in
//! nested contexts because the printer parenthesizes the inverted
//! comparison exactly where it parenthesized the original one.
//!
//! This pass is intentionally separate from literal folding: it fires on
//! non-literal operands, which literal folding leaves alone, and it is
//! a pure local rewrite with no resolver, scope, or effect analysis.

use crate::pass::PassResult;
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::ast_utils::map_children;
use storm_lua_syntax::size::measure_size;

/// Invert `not (a == b)` / `not (a ~= b)` wherever it appears.
/// Returns the (unchanged) root and the measured character saving.
pub fn invert_negated_comparisons(ast: &mut Ast, root: NodeId) -> PassResult {
    let before = measure_size(ast, root);
    let mut inverted = 0u64;
    let new_root = rewrite(ast, root, &mut inverted);
    let after = measure_size(ast, new_root);
    PassResult {
        root: new_root,
        saved: Some(before.saturating_sub(after) as u64),
        details: Some(vec![format!("inverted={inverted}")]),
    }
}

fn rewrite(ast: &mut Ast, id: NodeId, inverted: &mut u64) -> NodeId {
    let value = rewrite_inner(ast, id, inverted);
    ast.nodes.rewrite(id, value, "negated-comparison-inversion");
    id
}

fn strip_parens(ast: &Ast, mut id: NodeId) -> NodeId {
    loop {
        match ast.node(id) {
            Node::Paren(inner) => id = *inner,
            _ => return id,
        }
    }
}

fn rewrite_inner(ast: &mut Ast, id: NodeId, inverted: &mut u64) -> Node {
    let node = ast.nodes[id as usize].clone();
    match node {
        Node::Un(op, inner) if op == "not" => {
            // Rewrite the operand subtree first so nested inversions
            // are handled, then look through any parentheses around it.
            let rewritten_inner = rewrite(ast, inner, inverted);
            let target = strip_parens(ast, rewritten_inner);
            if let Node::Bin(cmp, left, right) = ast.node(target).clone() {
                if cmp == "==" || cmp == "~=" {
                    let flipped = if cmp == "==" { "~=" } else { "==" };
                    *inverted += 1;
                    return Node::Bin(flipped.into(), left, right);
                }
            }
            Node::Un(op, rewritten_inner)
        }
        other => {
            let (new_node, _) = map_children(&other, &mut |c| rewrite(ast, c, inverted));
            new_node
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn run(source: &str) -> (String, usize) {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = invert_negated_comparisons(&mut ast, root);
        let code = Printer::new(&ast, false).output(result.root);
        let inverted = result
            .details
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.strip_prefix("inverted="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        (code, inverted)
    }

    #[test]
    fn inverts_equality_negation() {
        let (code, inverted) = run("local x = not (a == b) return x");
        assert_eq!(inverted, 1, "code: {code}");
        assert!(code.contains("a~=b"), "code: {code}");
        assert!(!code.contains("not("), "code: {code}");
    }

    #[test]
    fn inverts_inequality_negation() {
        let (code, inverted) = run("local x = not (a ~= b) return x");
        assert_eq!(inverted, 1, "code: {code}");
        assert!(code.contains("a==b"), "code: {code}");
        assert!(!code.contains("not("), "code: {code}");
    }

    #[test]
    fn inverts_unparenthesized_operand_built_by_other_passes() {
        // A `not` node whose operand is a bare comparison (no Paren
        // node) arises when another pass constructs it; the printer
        // would parenthesize it, and the inversion must still fire.
        let (mut ast, root) = parse_source("local x = not (a == b) return x").expect("parse");
        // Rebuild the same shape without the Paren node: find the Un
        // node, unwrap its Paren operand, and rewrite in place.
        let mut un_id = None;
        storm_lua_syntax::ast_utils::walk(&ast, root, &mut |n| {
            if matches!(ast.node(n), Node::Un(op, _) if op == "not") {
                un_id = Some(n);
            }
        });
        let un_id = un_id.expect("not node");
        let Node::Un(_, inner) = ast.node(un_id).clone() else {
            panic!("not a Un node");
        };
        let Node::Paren(bin) = ast.node(inner).clone() else {
            panic!("operand is not a Paren node");
        };
        ast.nodes
            .rewrite(un_id, Node::Un("not".into(), bin), "test-setup");
        let result = invert_negated_comparisons(&mut ast, root);
        let code = Printer::new(&ast, false).output(result.root);
        assert!(code.contains("a~=b"), "code: {code}");
        assert!(!code.contains("not("), "code: {code}");
    }

    #[test]
    fn leaves_ordering_comparisons_alone_for_nan() {
        let (code, inverted) = run("local x = not (a < b) local y = not (a >= b) return x and y");
        assert_eq!(inverted, 0, "code: {code}");
        assert!(code.contains("not(a<b)"), "code: {code}");
        assert!(code.contains("not(a>=b)"), "code: {code}");
    }

    #[test]
    fn leaves_plain_negation_alone() {
        let (code, inverted) = run("local x = not a local y = not not b return x and y");
        assert_eq!(inverted, 0, "code: {code}");
        assert!(code.contains("not a"), "code: {code}");
    }

    #[test]
    fn inverts_inside_conditions_and_nested_contexts() {
        let (code, inverted) = run("if not (a == b) then return 1 else return 2 end");
        assert_eq!(inverted, 1, "code: {code}");
        assert!(code.contains("a~=b"), "code: {code}");
    }
}
