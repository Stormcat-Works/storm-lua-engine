//! AST traversal and structural rewrite operations.

// Canonical arena AST traversal/rewrite utilities.
//
// mapNode / mapBlocks / walk を NodeId arena に対して実装する。
//
// 構造共有（restricted in-place, D-29）: パスは `&Ast → &mut Ast` を一度だけ deep コピー
// した後、NodeId を維持したままノードを書き換える。map_node は子を再帰的に写し、
// 子が変わったら自分のスロット（NodeId）をそのまま再利用して書換えるため、
// NodeId が構造のダンダリング（親→子参照）を生まない。fn が新ノードを返す場合は
// arena 末尾へ push して返す。

use crate::ast::{Ast, Node, NodeId, TableField};

fn is_leaf(node: &Node) -> bool {
    matches!(
        node,
        Node::Break
            | Node::Goto(_)
            | Node::Label(_)
            | Node::Nil
            | Node::Bool(_)
            | Node::Vararg
            | Node::Num(_)
            | Node::Str(_)
            | Node::Name(_)
    )
}

// Internal walkers need the original operator only if a child ID changes.
// Keep ordinary Lua operators in static storage while callbacks run. Unknown
// low-level spellings retain an owned snapshot and are never normalized.
enum ChildSnapshot {
    Unary(std::borrow::Cow<'static, str>, NodeId),
    Binary(std::borrow::Cow<'static, str>, NodeId, NodeId),
    Other(Node),
}
fn operator_snapshot(op: &str) -> std::borrow::Cow<'static, str> {
    use std::borrow::Cow;
    match op {
        "+" => Cow::Borrowed("+"),
        "-" => Cow::Borrowed("-"),
        "*" => Cow::Borrowed("*"),
        "/" => Cow::Borrowed("/"),
        "//" => Cow::Borrowed("//"),
        "%" => Cow::Borrowed("%"),
        "^" => Cow::Borrowed("^"),
        ".." => Cow::Borrowed(".."),
        "==" => Cow::Borrowed("=="),
        "~=" => Cow::Borrowed("~="),
        "<" => Cow::Borrowed("<"),
        ">" => Cow::Borrowed(">"),
        "<=" => Cow::Borrowed("<="),
        ">=" => Cow::Borrowed(">="),
        "and" => Cow::Borrowed("and"),
        "or" => Cow::Borrowed("or"),
        "not" => Cow::Borrowed("not"),
        "#" => Cow::Borrowed("#"),
        "~" => Cow::Borrowed("~"),
        "&" => Cow::Borrowed("&"),
        "|" => Cow::Borrowed("|"),
        "<<" => Cow::Borrowed("<<"),
        ">>" => Cow::Borrowed(">>"),
        _ => Cow::Owned(op.to_owned()),
    }
}
impl ChildSnapshot {
    fn new(node: &Node) -> Self {
        match node {
            Node::Un(op, child) => Self::Unary(operator_snapshot(op), *child),
            Node::Bin(op, left, right) => Self::Binary(operator_snapshot(op), *left, *right),
            _ => Self::Other(node.clone()),
        }
    }
    fn map(self, f: &mut dyn FnMut(NodeId) -> NodeId) -> Option<Node> {
        match self {
            Self::Unary(op, child) => {
                let next = f(child);
                (next != child).then(|| Node::Un(op.into_owned(), next))
            }
            Self::Binary(op, left, right) => {
                let a = f(left);
                let b = f(right);
                (a != left || b != right).then(|| Node::Bin(op.into_owned(), a, b))
            }
            Self::Other(node) => {
                let (node, changed) = map_owned_children(node, f);
                changed.then_some(node)
            }
        }
    }
}

/// 全ノードを写す（TS mapNode）。
/// 子を再帰的に写してから fn_ を現在ノードへ適用し、その戻り id を返す。
pub fn map_node(
    ast: &mut Ast,
    id: NodeId,
    fn_: &mut dyn FnMut(&mut Ast, NodeId) -> NodeId,
) -> NodeId {
    if is_leaf(ast.node(id)) {
        return fn_(ast, id);
    }
    let snapshot = ChildSnapshot::new(ast.node(id));
    if let Some(node) = snapshot.map(&mut |c| map_node(ast, c, fn_)) {
        ast.nodes.rewrite(id, node, "child-rewrite");
    }
    fn_(ast, id)
}

/// block ノードにのみ block_fn を適用する（TS mapBlocks）。
/// root 自体は適用対象とせず、入れ子の block にのみ適用する。
pub fn map_blocks(
    ast: &mut Ast,
    id: NodeId,
    block_fn: &mut dyn FnMut(&mut Ast, NodeId) -> NodeId,
) -> NodeId {
    if is_leaf(ast.node(id)) {
        return id;
    }
    let snapshot = ChildSnapshot::new(ast.node(id));
    if let Some(node) = snapshot.map(&mut |c| apply_blocks(ast, c, block_fn)) {
        ast.nodes.rewrite(id, node, "child-rewrite");
    }
    id
}

/// block のとき block_fn、それ以外は子を再帰的に写す。
fn apply_blocks(
    ast: &mut Ast,
    id: NodeId,
    block_fn: &mut dyn FnMut(&mut Ast, NodeId) -> NodeId,
) -> NodeId {
    if matches!(ast.nodes[id as usize], Node::Block(_)) {
        block_fn(ast, id)
    } else {
        if is_leaf(ast.node(id)) {
            return id;
        }
        let snapshot = ChildSnapshot::new(ast.node(id));
        if let Some(node) = snapshot.map(&mut |c| apply_blocks(ast, c, block_fn)) {
            ast.nodes.rewrite(id, node, "child-rewrite");
        }
        id
    }
}

/// Rewrite a borrowed node without mutating its source. Existing callers may
/// retain the original snapshot; owned traversal below avoids cloning it twice.
pub fn map_children(node: &Node, f: &mut dyn FnMut(NodeId) -> NodeId) -> (Node, bool) {
    map_owned_children(node.clone(), f)
}

/// Reuse the snapshot's vectors/strings while rewriting its child IDs. Never
/// move a live arena slot out: callbacks can inspect or modify the parent slot.
fn map_owned_children(mut node: Node, f: &mut dyn FnMut(NodeId) -> NodeId) -> (Node, bool) {
    let mut changed = false;
    let mut visit = |id: &mut NodeId| {
        let mapped = f(*id);
        changed |= mapped != *id;
        *id = mapped;
    };
    match &mut node {
        Node::Block(ids) | Node::Local(_, ids) | Node::Return(ids) => {
            for id in ids {
                visit(id);
            }
        }
        Node::Do(id)
        | Node::Localfunc(_, id)
        | Node::Callstat(id)
        | Node::Function(_, _, id)
        | Node::Un(_, id)
        | Node::Paren(id)
        | Node::Methodname(id, _) => visit(id),
        Node::While(a, b)
        | Node::Repeat(a, b)
        | Node::Funcstat(a, b)
        | Node::Bin(_, a, b)
        | Node::Index(a, b, _) => {
            visit(a);
            visit(b);
        }
        Node::If(arms, otherwise) => {
            for arm in arms {
                visit(&mut arm.cond);
                visit(&mut arm.body);
            }
            if let Some(body) = otherwise {
                visit(body);
            }
        }
        Node::Fornum(_, a, b, c, body) => {
            visit(a);
            visit(b);
            if let Some(c) = c {
                visit(c);
            }
            visit(body);
        }
        Node::Forin(_, values, body) => {
            for value in values {
                visit(value);
            }
            visit(body);
        }
        Node::Assign(targets, values) => {
            for target in targets {
                visit(target);
            }
            for value in values {
                visit(value);
            }
        }
        Node::Table(fields) => {
            for field in fields {
                match field {
                    TableField::Arr(value) | TableField::Name(_, value) => visit(value),
                    TableField::KVar(key, value) => {
                        visit(key);
                        visit(value);
                    }
                }
            }
        }
        Node::Call(function, args, _) => {
            visit(function);
            for arg in args {
                visit(arg);
            }
        }
        Node::Break
        | Node::Goto(_)
        | Node::Label(_)
        | Node::Nil
        | Node::Bool(_)
        | Node::Vararg
        | Node::Num(_)
        | Node::Str(_)
        | Node::Name(_) => {}
    }
    (node, changed)
}

/// 全ノードを走査する。根ノードを含めて fn_ を呼ぶ。
pub fn walk<F: FnMut(NodeId) + ?Sized>(ast: &Ast, root: NodeId, fn_: &mut F) {
    walk_rec(ast, root, fn_);
}

/// TS `Object.entries(node)` と同じフィールド順・キーで子を列挙する。
/// パス内の親コンテキスト判定（parentKey）に使う。メタデータキー
/// （bid/bids/scopeId/write）は Rust では Resolution 側に持つため含めない。
pub fn for_each_child_key(ast: &Ast, id: NodeId, f: &mut dyn FnMut(&'static str, NodeId)) {
    let node = &ast.nodes[id as usize];
    match node {
        Node::Block(ss) => {
            for s in ss {
                f("ss", *s);
            }
        }
        Node::Break
        | Node::Goto(_)
        | Node::Label(_)
        | Node::Nil
        | Node::Bool(_)
        | Node::Vararg
        | Node::Num(_)
        | Node::Str(_)
        | Node::Name(_) => {}
        Node::Do(b) => f("b", *b),
        Node::While(e, b) => {
            f("e", *e);
            f("b", *b);
        }
        Node::Repeat(b, e) => {
            f("b", *b);
            f("e", *e);
        }
        Node::If(arms, eb) => {
            for arm in arms {
                f("c", arm.cond);
                f("b", arm.body);
            }
            if let Some(e) = eb {
                f("eb", *e);
            }
        }
        Node::Fornum(_, a, b, c, body) => {
            f("a", *a);
            f("b", *b);
            if let Some(c) = c {
                f("c", *c);
            }
            f("body", *body);
        }
        Node::Forin(_, es, body) => {
            for e in es {
                f("es", *e);
            }
            f("body", *body);
        }
        Node::Funcstat(target, fn_) => {
            f("target", *target);
            f("fn", *fn_);
        }
        Node::Localfunc(_, fn_) => f("fn", *fn_),
        Node::Local(_, es) => {
            for e in es {
                f("es", *e);
            }
        }
        Node::Return(es) => {
            for e in es {
                f("es", *e);
            }
        }
        Node::Callstat(e) => f("e", *e),
        Node::Assign(vs, es) => {
            for v in vs {
                f("vs", *v);
            }
            for e in es {
                f("es", *e);
            }
        }
        Node::Function(_, _, b) => f("b", *b),
        Node::Table(fs) => {
            for field in fs {
                match field {
                    crate::ast::TableField::Arr(v) => f("fs", *v),
                    crate::ast::TableField::Name(_, v) => f("fs", *v),
                    crate::ast::TableField::KVar(k, v) => {
                        f("fs", *k);
                        f("fs", *v);
                    }
                }
            }
        }
        Node::Un(_, e) => f("e", *e),
        Node::Bin(_, l, r) => {
            f("l", *l);
            f("r", *r);
        }
        Node::Paren(e) => f("e", *e),
        Node::Call(fn_, args, _) => {
            f("fn", *fn_);
            for a in args {
                f("args", *a);
            }
        }
        Node::Index(obj, key, _) => {
            f("obj", *obj);
            f("key", *key);
        }
        Node::Methodname(obj, _) => f("obj", *obj),
    }
}

fn walk_rec<F: FnMut(NodeId) + ?Sized>(ast: &Ast, id: NodeId, fn_: &mut F) {
    fn_(id);
    crate::ast_utils::for_each_child(ast, id, &mut |c| walk_rec(ast, c, fn_));
}

/// 新しい Ast を作り、元の Interner の文字列集合を順序どおり引き継ぐ
/// （SymbolId の整合を保つ。Interner は挿入順で id を割り当てるため）。
pub fn inherit_ast(source: &Ast) -> Ast {
    Ast {
        nodes: source.nodes.empty_like(),
        strings: source.strings.clone(),
    }
}

/// Syntax-only clone for disposable size/ranking trials. It deliberately drops
/// provenance while preserving all NodeId/SymbolId values.
pub fn clone_without_origins(source: &Ast) -> Ast {
    Ast {
        nodes: source.nodes.clone_without_origins(),
        strings: source.strings.clone(),
    }
}

/// Visit direct AST children in evaluation/structural order.
#[inline]
pub fn for_each_child<F: FnMut(NodeId) + ?Sized>(ast: &Ast, id: NodeId, f: &mut F) {
    let node = &ast.nodes[id as usize];
    match node {
        Node::Block(ss) => {
            for s in ss {
                f(*s);
            }
        }
        Node::Break
        | Node::Goto(_)
        | Node::Label(_)
        | Node::Nil
        | Node::Bool(_)
        | Node::Vararg
        | Node::Num(_)
        | Node::Str(_)
        | Node::Name(_) => {}
        Node::Do(b) => f(*b),
        Node::While(e, b) => {
            f(*e);
            f(*b);
        }
        Node::Repeat(b, e) => {
            f(*b);
            f(*e);
        }
        Node::If(arms, eb) => {
            for arm in arms {
                f(arm.cond);
                f(arm.body);
            }
            if let Some(e) = eb {
                f(*e);
            }
        }
        Node::Fornum(_, a, b, c, body) => {
            f(*a);
            f(*b);
            if let Some(c) = c {
                f(*c);
            }
            f(*body);
        }
        Node::Forin(_, es, body) => {
            for e in es {
                f(*e);
            }
            f(*body);
        }
        Node::Funcstat(target, fn_) => {
            f(*target);
            f(*fn_);
        }
        Node::Localfunc(_, fn_) => f(*fn_),
        Node::Local(_, es) => {
            for e in es {
                f(*e);
            }
        }
        Node::Return(es) => {
            for e in es {
                f(*e);
            }
        }
        Node::Callstat(e) => f(*e),
        Node::Assign(vs, es) => {
            for v in vs {
                f(*v);
            }
            for e in es {
                f(*e);
            }
        }
        Node::Function(_, _, b) => f(*b),
        Node::Table(fs) => {
            for field in fs {
                match field {
                    crate::ast::TableField::Arr(v) => f(*v),
                    crate::ast::TableField::Name(_, v) => f(*v),
                    crate::ast::TableField::KVar(k, v) => {
                        f(*k);
                        f(*v);
                    }
                }
            }
        }
        Node::Un(_, e) => f(*e),
        Node::Bin(_, l, r) => {
            f(*l);
            f(*r);
        }
        Node::Paren(e) => f(*e),
        Node::Call(fn_, args, _) => {
            f(*fn_);
            for a in args {
                f(*a);
            }
        }
        Node::Index(obj, key, _) => {
            f(*obj);
            f(*key);
        }
        Node::Methodname(obj, _) => f(*obj),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod leaf_snapshot_tests {
    use super::*;
    use crate::provenance::GeneratedOrigins;
    use crate::{parse_source_with_origins, Printer};
    fn old_node(
        ast: &mut Ast,
        id: NodeId,
        callback: &mut dyn FnMut(&mut Ast, NodeId) -> NodeId,
    ) -> NodeId {
        let node = ast.nodes[id as usize].clone();
        let (node, changed) = map_owned_children(node, &mut |child| old_node(ast, child, callback));
        if changed {
            ast.nodes.rewrite(id, node, "child-rewrite");
        }
        callback(ast, id)
    }
    fn old_blocks(
        ast: &mut Ast,
        id: NodeId,
        callback: &mut dyn FnMut(&mut Ast, NodeId) -> NodeId,
    ) -> NodeId {
        let node = ast.nodes[id as usize].clone();
        let (node, changed) =
            map_owned_children(node, &mut |child| old_apply(ast, child, callback));
        if changed {
            ast.nodes.rewrite(id, node, "child-rewrite");
        }
        id
    }
    fn old_apply(
        ast: &mut Ast,
        id: NodeId,
        callback: &mut dyn FnMut(&mut Ast, NodeId) -> NodeId,
    ) -> NodeId {
        if matches!(ast.nodes[id as usize], Node::Block(_)) {
            callback(ast, id)
        } else {
            old_blocks(ast, id, callback)
        }
    }
    fn exercise(ast: &Ast, root: NodeId, blocks: bool, mode: u32) {
        let mut before = ast.clone();
        let mut after = ast.clone();
        let mut logs = [Vec::new(), Vec::new()];
        let mut roots = [0, 0];
        for (index, target) in [&mut before, &mut after].into_iter().enumerate() {
            let mut callback = |current: &mut Ast, id: NodeId| {
                logs[index].push((id, serde_json::to_value(current.node(id)).unwrap()));
                if mode == 1 {
                    if let Node::Num(value) = current.node(id) {
                        let value = value.clone();
                        let next = current.push(Node::Num(value));
                        current.nodes.derive_from(next, &ast.nodes, id, "copy");
                        return next;
                    }
                }
                if mode == 2 && id != root {
                    // Mutating a parent without changing a child's ID must not
                    // be lost through a stale snapshot when walking that parent.
                    let old = current.node(root).clone();
                    if let Node::Block(mut statements) = old {
                        statements.truncate(1);
                        current
                            .nodes
                            .rewrite(root, Node::Block(statements), "parent-edit");
                    }
                }
                if mode == 3 && matches!(current.node(id), Node::Block(_)) {
                    let copy = current.node(id).clone();
                    let next = current.push(copy);
                    current
                        .nodes
                        .derive_from(next, &ast.nodes, id, "block-copy");
                    return next;
                }
                id
            };
            roots[index] = match (index, blocks) {
                (0, false) => old_node(target, root, &mut callback),
                (0, true) => old_blocks(target, root, &mut callback),
                (_, false) => map_node(target, root, &mut callback),
                (_, true) => map_blocks(target, root, &mut callback),
            };
        }
        assert_eq!(roots[0], roots[1]);
        assert_eq!(logs[0], logs[1]);
        assert_eq!(
            serde_json::to_value(&before).unwrap(),
            serde_json::to_value(&after).unwrap()
        );
        let p = Printer::new(&before, false).output_with_positions(roots[0]);
        let q = Printer::new(&after, false).output_with_positions(roots[1]);
        assert_eq!(p.code, q.code);
        assert_eq!(
            GeneratedOrigins::from_print(&before, &p),
            GeneratedOrigins::from_print(&after, &q)
        );
    }
    #[test]
    fn leaf_elision_preserves_visit_order_mutable_parent_callbacks_and_exact_origins() {
        for text in [
            "local x=1 local y='雪😀' function f(a,...)if a then return x,... else return y end end output.setNumber(1,f(true))",
            "local t={a=1,[2]=3,4} for i=1,3 do t[i]=i end while false do break end repeat t.a=t.a+1 until true",
            "do local x=2 do output.setNumber(1,x)end end return false,nil",
            "",
        ] {
            let (ast,root)=parse_source_with_origins("tree.lua",text).unwrap();
            for blocks in [false,true] {for mode in 0..4 {exercise(&ast,root,blocks,mode);}}
        }
    }
}

#[cfg(test)]
mod operator_snapshot_tests {
    use super::*;
    #[test]
    fn private_snapshot_never_normalizes_operator_spelling_or_reorders_children() {
        for op in [
            "+",
            "-",
            "*",
            "/",
            "//",
            "%",
            "^",
            "..",
            "==",
            "~=",
            "<",
            ">",
            "<=",
            ">=",
            "and",
            "or",
            "not",
            "#",
            "~",
            "&",
            "|",
            "<<",
            ">>",
            "",
            "unexpected",
            "雪😀",
        ] {
            let binary = Node::Bin(op.to_owned(), 7, 11);
            let mut visits = Vec::new();
            assert!(ChildSnapshot::new(&binary)
                .map(&mut |child| {
                    visits.push(child);
                    child
                })
                .is_none());
            assert_eq!(visits, [7, 11]);
            visits.clear();
            let changed = ChildSnapshot::new(&binary).map(&mut |child| {
                visits.push(child);
                child + 1
            });
            assert!(matches!(changed,Some(Node::Bin(ref text,8,12)) if text==op));
            assert_eq!(visits, [7, 11]);
            let unary = Node::Un(op.to_owned(), 5);
            assert!(ChildSnapshot::new(&unary).map(&mut |child| child).is_none());
            assert!(
                matches!(ChildSnapshot::new(&unary).map(&mut |child|child+1),Some(Node::Un(ref text,6)) if text==op)
            );
        }
    }
}
