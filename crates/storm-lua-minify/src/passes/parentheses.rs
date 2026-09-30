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

fn rewrite(ast: &mut Ast, id: NodeId, role: Role, removed: &mut u64) -> NodeId {
    let origin = ast.nodes.capture_origin(id);
    let result = rewrite_inner(ast, id, role, removed);
    if result == id {
        ast.nodes.finish_rewrite(id, origin, "parentheses");
    }
    // A removed parenthesis returns an existing child, preserving that child's
    // own origin rather than overwriting it with the discarded parent's range.
    result
}

fn rewrite_inner(ast: &mut Ast, id: NodeId, role: Role, removed: &mut u64) -> NodeId {
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
                // In Lua, a parenthesized call is adjusted to exactly one
                // result.  Removing these parentheses changes multiple-return
                // assignment/argument semantics (e.g. `local a,b=(f())`).
                || matches!(ast.nodes[inner as usize], Node::Call(..));
            if keep {
                ast.nodes[id as usize] = Node::Paren(inner);
                id
            } else {
                *removed += 1;
                inner
            }
        }
        other => {
            let (new_node, _) = map_children_role(&other, &mut |c, r| rewrite(ast, c, r, removed));
            ast.nodes[id as usize] = new_node;
            id
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
            (Node::Call(mfn, out, method.clone()), changed)
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
    use storm_lua_syntax::parser::parse_source;
    use storm_lua_syntax::print::Printer;

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
