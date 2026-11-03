//! 冗長な括弧の除去（TS `passes/parentheses.ts` の移植）。
//!
//! 前置位置（`call.fn` / `index.obj` / `methodname.obj`）にある括弧で、
//! 内側が function か table の場合は構文保存のため残す。それ以外は外す。
//! この pass は binding/effect に依存しない（純構文変換）。

use crate::pass::PassResult;
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::ast_utils::map_children;

/// `removeRedundantParentheses` の移植。
///
/// TS の `rewrite(node, parent, parentKey)` に相当。括弧ノードの子は
/// 括弧自身と同じコンテキスト（親・parentKey）を引き継ぐ。
pub fn remove_redundant_parentheses(ast: &mut Ast, root: NodeId) -> PassResult {
    let mut removed = 0u64;
    let new_root = rewrite(ast, root, Role::Normal, &mut removed);
    PassResult {
        root: new_root,
        saved: Some(removed),
        details: None,
    }
}

/// このノードが親の中でどう扱われるか（TS の parent + parentKey の要約）。
#[derive(Clone, Copy, PartialEq)]
enum Role {
    /// ルート / 前置位置ではない通常の子
    Normal,
    /// `call.fn` / `index.obj` / `methodname.obj` — 括弧を残す前置位置
    Prefix,
}

/// Outcome of rewriting one node: either the node survives with a new value
/// (applied by the caller through `NodeArena::rewrite`, which takes the old
/// value out of the arena instead of cloning it), or it was a removed
/// parenthesis replaced by an existing child.
enum RewriteOutcome {
    Kept(Node),
    Replaced(NodeId),
}

fn rewrite(ast: &mut Ast, id: NodeId, role: Role, removed: &mut u64) -> NodeId {
    match rewrite_inner(ast, id, role, removed) {
        RewriteOutcome::Kept(value) => {
            // `rewrite` keeps the shared origin without another allocation
            // when the value is unchanged, and otherwise runs the same
            // `finish_rewrite` path with the same "parentheses" reason as
            // the previous capture + assign + finish sequence. A removed
            // parenthesis returns an existing child below, preserving that
            // child's own origin rather than overwriting it with the
            // discarded parent's range.
            ast.nodes.rewrite(id, value, "parentheses");
            id
        }
        RewriteOutcome::Replaced(inner) => inner,
    }
}

fn rewrite_inner(ast: &mut Ast, id: NodeId, role: Role, removed: &mut u64) -> RewriteOutcome {
    let node = ast.nodes[id as usize].clone();
    match node {
        Node::Paren(e) => {
            // 括弧の子は括弧自身の role を引き継ぐ（TS: rewrite(node.e, parent, parentKey)）
            let inner = rewrite(ast, e, role, removed);
            let keep = role == Role::Prefix
                && matches!(
                    ast.nodes[inner as usize],
                    Node::Function(..) | Node::Table(_)
                )
                // In Lua, a parenthesized call or vararg expression is
                // adjusted to exactly one result.  Removing these parentheses
                // changes multiple-return assignment/argument semantics
                // (e.g. `local a,b=(f())` or `return (...)` after a local
                // bound to `...` is sunk into an expansion position).
                || matches!(ast.nodes[inner as usize], Node::Call(..) | Node::Vararg);
            if keep {
                RewriteOutcome::Kept(Node::Paren(inner))
            } else {
                *removed += 1;
                RewriteOutcome::Replaced(inner)
            }
        }
        other => {
            let (new_node, _) = map_children_role(&other, &mut |c, r| rewrite(ast, c, r, removed));
            RewriteOutcome::Kept(new_node)
        }
    }
}

/// 各子へ、親における role を渡して写す。`call.fn` / `index.obj` / `methodname.obj` のみ Prefix。
fn map_children_role(node: &Node, f: &mut dyn FnMut(NodeId, Role) -> NodeId) -> (Node, bool) {
    match node {
        Node::Call(fn_node, args, method) => {
            let mfn = f(*fn_node, Role::Prefix);
            let mut out = Vec::with_capacity(args.len());
            let mut changed = mfn != *fn_node;
            for a in args {
                let m = f(*a, Role::Normal);
                if m != *a {
                    changed = true;
                }
                out.push(m);
            }
            (Node::Call(mfn, out, *method), changed)
        }
        Node::Index(obj, key, dot) => {
            let mo = f(*obj, Role::Prefix);
            let mk = f(*key, Role::Normal);
            (Node::Index(mo, mk, *dot), mo != *obj || mk != *key)
        }
        Node::Methodname(obj, name) => {
            let mo = f(*obj, Role::Prefix);
            (Node::Methodname(mo, *name), mo != *obj)
        }
        _ => map_children_normal(node, f),
    }
}

/// Prefix 位置を含まないノードの子写し（Role::Normal 固定）。
fn map_children_normal(node: &Node, f: &mut dyn FnMut(NodeId, Role) -> NodeId) -> (Node, bool) {
    let g = &mut |c: NodeId| f(c, Role::Normal);
    map_children(node, g)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn run(src: &str) -> (String, u64) {
        let (mut ast, root) = parse_source(src).unwrap();
        let res = remove_redundant_parentheses(&mut ast, root);
        let out = Printer::new(&ast, false).output(res.root);
        (out, res.saved.unwrap())
    }

    #[test]
    fn removes_normal_redundant_parentheses() {
        let (out, saved) = run("local a = (1) + (2)");
        assert_eq!(out, "local a=1+2");
        assert_eq!(saved, 2);
    }

    #[test]
    fn keeps_parentheses_for_functions_in_prefix_position() {
        let (out, saved) = run("local a = (function() return 1 end)()");
        assert_eq!(out, "local a=(function()return 1 end)()");
        assert_eq!(saved, 0);
    }

    #[test]
    fn removes_parentheses_for_functions_in_normal_position() {
        let (out, saved) = run("local a = (function() return 1 end)");
        assert_eq!(out, "local a=function()return 1 end");
        assert_eq!(saved, 1);
    }

    #[test]
    fn keeps_parentheses_for_tables_in_prefix_position() {
        let (out, saved) = run("local a = ({x=1}).x");
        assert_eq!(out, "local a=({x=1}).x");
        assert_eq!(saved, 0);
    }

    #[test]
    fn keeps_parentheses_for_tables_in_method_call() {
        let (out, saved) = run("local a = ({x=1}):y()");
        assert_eq!(out, "local a=({x=1}):y()");
        assert_eq!(saved, 0);
    }

    #[test]
    fn keeps_outer_paren_with_changed_child_and_removed_inner() {
        // Kept node whose child changed (reason path) and a removed outer
        // paren over a kept inner paren (both rewrite outcomes). `run` uses
        // the origin-auditing printer, so surviving bytes must keep known
        // origins through the `NodeArena::rewrite` path.
        let (out, saved) = run("local a = ({x=(1)}).x");
        assert_eq!(out, "local a=({x=1}).x");
        assert_eq!(saved, 1);
        let (out, saved) = run("local a = ((function() return 1 end))()");
        assert_eq!(out, "local a=(function()return 1 end)()");
        assert_eq!(saved, 1);
    }

    #[test]
    fn keeps_parentheses_for_vararg() {
        // A parenthesized vararg adjusts to one value; stripping it in an
        // expansion position (return / multi-name local) would expand the
        // whole vararg list. This is the vararg counterpart of the call
        // case above, and of the parentheses that single-use-local-sinking
        // inserts when it sinks a `...`-bound local.
        let (out, saved) = run("local function f(...) return (...) end");
        assert!(out.contains("return(...)"), "out={out}");
        assert_eq!(saved, 0);
        let (out, saved) = run("local function f(...) local a, b = (...) return a, b end");
        assert!(out.contains("(...)"), "out={out}");
        assert_eq!(saved, 0);
    }

    #[test]
    fn left_nested_right_associative_grouping_survives_printing() {
        // `(a .. b) .. c` and `(a ^ b) ^ c`: this pass strips the Paren
        // nodes, leaving a bare left-nested Bin shape. The printer must
        // re-parenthesize that shape, because the flat text re-parses
        // right-nested and changes evaluation order and values
        // (candidate T, w9 cycle 67).
        let (out, _) = run("local a = ((x .. y) .. z)");
        assert_eq!(out, "local a=(x..y)..z");
        let (out, _) = run("local a = ((x ^ 2) ^ 0.5)");
        assert_eq!(out, "local a=(x^2)^.5");
        // Right-nested shapes need no parentheses and stay flat.
        let (out, _) = run("local a = (x .. (y .. z))");
        assert_eq!(out, "local a=x..y..z");
        let (out, _) = run("local a = (x ^ (2 ^ 0.5))");
        assert_eq!(out, "local a=x^2^.5");
    }

    #[test]
    fn removes_nested_redundant_parentheses() {
        let (out, saved) = run("local a = (((1)))");
        assert_eq!(out, "local a=1");
        assert_eq!(saved, 3);
    }

    #[test]
    fn keeps_outer_parenthesis_for_function_but_removes_inner() {
        let (out, saved) = run("local a = (((function() return 1 end)))()");
        assert_eq!(out, "local a=(function()return 1 end)()");
        assert_eq!(saved, 2);
    }
}
