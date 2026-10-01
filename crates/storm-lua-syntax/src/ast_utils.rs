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

/// 全ノードを写す（TS mapNode）。
/// 子を再帰的に写してから fn_ を現在ノードへ適用し、その戻り id を返す。
pub fn map_node(
    ast: &mut Ast,
    id: NodeId,
    fn_: &mut dyn FnMut(&mut Ast, NodeId) -> NodeId,
) -> NodeId {
    let node = ast.nodes[id as usize].clone();
    let (node, changed) = map_owned_children(node, &mut |c| map_node(ast, c, fn_));
    if changed {
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
    let node = ast.nodes[id as usize].clone();
    let (node, changed) = map_owned_children(node, &mut |c| apply_blocks(ast, c, block_fn));
    if changed {
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
        let node = ast.nodes[id as usize].clone();
        let (node, changed) = map_owned_children(node, &mut |c| apply_blocks(ast, c, block_fn));
        if changed {
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
