//! Deterministic syntax-tree snapshots for diagnostics and regression tests.
//! The migration snapshot's field order and escape rules are fixed. Write into
//! one buffer instead of allocating a complete String at every recursive edge.
use crate::ast::{Ast, Node, NodeId, TableField};

/// Produce the unchanged canonical snapshot of a valid AST.
pub fn dump(ast: &Ast, root: NodeId) -> String {
    let mut out = String::new();
    write_node(ast, root, &mut out);
    out
}
fn quoted(out: &mut String, value: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push('"');
    let mut copied = 0;
    for (i, b) in value.bytes().enumerate() {
        if b == b'"' || b == b'\\' || b < 0x20 {
            out.push_str(&value[copied..i]);
            match b {
                b'"' => out.push_str("\\\""),
                b'\\' => out.push_str("\\\\"),
                b'\n' => out.push_str("\\n"),
                b'\r' => out.push_str("\\r"),
                b'\t' => out.push_str("\\t"),
                _ => {
                    out.push_str("\\u00");
                    out.push(HEX[(b >> 4) as usize] as char);
                    out.push(HEX[(b & 15) as usize] as char);
                }
            }
            copied = i + 1;
        }
    }
    out.push_str(&value[copied..]);
    out.push('"');
}
fn boolean(out: &mut String, value: bool) {
    out.push_str(if value { "true" } else { "false" });
}
fn list<T>(out: &mut String, values: &[T], write: impl Fn(&mut String, &T)) {
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write(out, value);
    }
}
fn optional(ast: &Ast, out: &mut String, value: Option<NodeId>) {
    if let Some(id) = value {
        write_node(ast, id, out);
    } else {
        out.push_str("null");
    }
}
fn write_node(ast: &Ast, id: NodeId, out: &mut String) {
    let intern = &ast.strings;
    match ast.node(id) {
        Node::Block(values) => {
            out.push_str("block(ss:[");
            list(out, values, |o, v| write_node(ast, *v, o));
            out.push_str("])");
        }
        Node::Break => out.push_str("break()"),
        Node::Goto(name) => {
            out.push_str("goto(name:");
            quoted(out, intern.get(*name));
            out.push(')');
        }
        Node::Label(name) => {
            out.push_str("label(name:");
            quoted(out, intern.get(*name));
            out.push(')');
        }
        Node::Do(body) => {
            out.push_str("do(b:");
            write_node(ast, *body, out);
            out.push(')');
        }
        Node::While(e, b) => {
            out.push_str("while(e:");
            write_node(ast, *e, out);
            out.push_str(",b:");
            write_node(ast, *b, out);
            out.push(')');
        }
        Node::Repeat(b, e) => {
            out.push_str("repeat(b:");
            write_node(ast, *b, out);
            out.push_str(",e:");
            write_node(ast, *e, out);
            out.push(')');
        }
        Node::If(arms, eb) => {
            out.push_str("if(arms:[");
            list(out, arms, |o, a| {
                o.push('[');
                write_node(ast, a.cond, o);
                o.push(',');
                write_node(ast, a.body, o);
                o.push(']');
            });
            out.push_str("],eb:");
            optional(ast, out, *eb);
            out.push(')');
        }
        Node::Fornum(name, a, b, c, body) => {
            out.push_str("fornum(name:");
            quoted(out, intern.get(*name));
            out.push_str(",a:");
            write_node(ast, *a, out);
            out.push_str(",b:");
            write_node(ast, *b, out);
            out.push_str(",c:");
            optional(ast, out, *c);
            out.push_str(",body:");
            write_node(ast, *body, out);
            out.push(')');
        }
        Node::Forin(names, es, body) => {
            out.push_str("forin(names:[");
            list(out, names, |o, n| quoted(o, intern.get(*n)));
            out.push_str("],es:[");
            list(out, es, |o, n| write_node(ast, *n, o));
            out.push_str("],body:");
            write_node(ast, *body, out);
            out.push(')');
        }
        Node::Funcstat(target, fun) => {
            out.push_str("funcstat(target:");
            write_node(ast, *target, out);
            out.push_str(",fn:");
            write_node(ast, *fun, out);
            out.push(')');
        }
        Node::Localfunc(name, fun) => {
            out.push_str("localfunc(name:");
            quoted(out, intern.get(*name));
            out.push_str(",fn:");
            write_node(ast, *fun, out);
            out.push(')');
        }
        Node::Local(names, es) => {
            out.push_str("local(names:[");
            list(out, names, |o, n| quoted(o, intern.get(*n)));
            out.push_str("],es:[");
            list(out, es, |o, n| write_node(ast, *n, o));
            out.push_str("])");
        }
        Node::Return(es) => {
            out.push_str("return(es:[");
            list(out, es, |o, n| write_node(ast, *n, o));
            out.push_str("])");
        }
        Node::Callstat(e) => {
            out.push_str("callstat(e:");
            write_node(ast, *e, out);
            out.push(')');
        }
        Node::Assign(vs, es) => {
            out.push_str("assign(vs:[");
            list(out, vs, |o, n| write_node(ast, *n, o));
            out.push_str("],es:[");
            list(out, es, |o, n| write_node(ast, *n, o));
            out.push_str("])");
        }
        Node::Nil => out.push_str("nil()"),
        Node::Bool(v) => {
            out.push_str("bool(v:");
            boolean(out, *v);
            out.push(')');
        }
        Node::Vararg => out.push_str("vararg()"),
        Node::Num(v) => {
            out.push_str("num(v:");
            quoted(out, v);
            out.push(')');
        }
        Node::Str(v) => {
            out.push_str("str(v:");
            quoted(out, v);
            out.push(')');
        }
        Node::Function(ps, var, b) => {
            out.push_str("function(ps:[");
            list(out, ps, |o, n| quoted(o, intern.get(*n)));
            out.push_str("],var:");
            boolean(out, *var);
            out.push_str(",b:");
            write_node(ast, *b, out);
            out.push(')');
        }
        Node::Table(fs) => {
            out.push_str("table(fs:[");
            list(out, fs, |o, f| match f {
                TableField::Arr(v) => {
                    o.push_str("[arr,null,");
                    write_node(ast, *v, o);
                    o.push(']');
                }
                TableField::Name(k, v) => {
                    o.push_str("[name,");
                    quoted(o, intern.get(*k));
                    o.push(',');
                    write_node(ast, *v, o);
                    o.push(']');
                }
                TableField::KVar(k, v) => {
                    o.push_str("[kv,");
                    write_node(ast, *k, o);
                    o.push(',');
                    write_node(ast, *v, o);
                    o.push(']');
                }
            });
            out.push_str("])");
        }
        Node::Un(op, e) => {
            out.push_str("un(op:");
            quoted(out, op);
            out.push_str(",e:");
            write_node(ast, *e, out);
            out.push(')');
        }
        Node::Bin(op, l, r) => {
            out.push_str("bin(op:");
            quoted(out, op);
            out.push_str(",l:");
            write_node(ast, *l, out);
            out.push_str(",r:");
            write_node(ast, *r, out);
            out.push(')');
        }
        Node::Paren(e) => {
            out.push_str("paren(e:");
            write_node(ast, *e, out);
            out.push(')');
        }
        Node::Call(fun, args, method) => {
            out.push_str("call(fn:");
            write_node(ast, *fun, out);
            out.push_str(",args:[");
            list(out, args, |o, n| write_node(ast, *n, o));
            out.push_str("],method:");
            if let Some(m) = method {
                quoted(out, m);
            } else {
                out.push_str("null");
            }
            out.push(')');
        }
        Node::Name(v) => {
            out.push_str("name(v:");
            quoted(out, intern.get(*v));
            out.push(')');
        }
        Node::Index(obj, key, dot) => {
            out.push_str("index(obj:");
            write_node(ast, *obj, out);
            out.push_str(",key:");
            write_node(ast, *key, out);
            out.push_str(",dot:");
            boolean(out, *dot);
            out.push(')');
        }
        Node::Methodname(obj, name) => {
            out.push_str("methodname(obj:");
            write_node(ast, *obj, out);
            out.push_str(",name:");
            quoted(out, intern.get(*name));
            out.push(')');
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    mod reference {

        // オラクル①（parser parity）検証用のカノニカル AST ダンプ。
        // Migration snapshot の canonical AST dump format を維持する。
        // 全ノード種に固定フィールド順を定義し、文字列は共通のエスケープ規則で出力する。
        // 位置情報（line/col）は含めない。

        use crate::ast::{Ast, Node, NodeId, TableField};

        fn esc(s: &str) -> String {
            let mut out = String::from("\"");
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => {
                        out.push_str(&format!("\\u00{:02x}", c as u32));
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }

        fn join(items: impl Iterator<Item = String>, sep: &str) -> String {
            items.collect::<Vec<_>>().join(sep)
        }

        fn t(v: bool) -> &'static str {
            if v {
                "true"
            } else {
                "false"
            }
        }

        /// Produce a deterministic structural snapshot of a valid AST.
        pub fn dump(ast: &Ast, root: NodeId) -> String {
            dump_node(ast, root)
        }

        fn dump_node(ast: &Ast, id: NodeId) -> String {
            let intern = &ast.strings;
            match &ast.nodes[id as usize] {
                Node::Block(ss) => format!(
                    "block(ss:[{}])",
                    join(ss.iter().map(|s| dump_node(ast, *s)), ",")
                ),
                Node::Break => "break()".to_string(),
                Node::Goto(name) => format!("goto(name:{})", esc(intern.get(*name))),
                Node::Label(name) => format!("label(name:{})", esc(intern.get(*name))),
                Node::Do(b) => format!("do(b:{})", dump_node(ast, *b)),
                Node::While(e, b) => {
                    format!("while(e:{},b:{})", dump_node(ast, *e), dump_node(ast, *b))
                }
                Node::Repeat(b, e) => {
                    format!("repeat(b:{},e:{})", dump_node(ast, *b), dump_node(ast, *e))
                }
                Node::If(arms, eb) => format!(
                    "if(arms:[{}],eb:{})",
                    join(
                        arms.iter().map(|arm| format!(
                            "[{},{}]",
                            dump_node(ast, arm.cond),
                            dump_node(ast, arm.body)
                        )),
                        ","
                    ),
                    eb.map_or_else(|| "null".to_string(), |e| dump_node(ast, e))
                ),
                Node::Fornum(name, a, b, c, body) => format!(
                    "fornum(name:{},a:{},b:{},c:{},body:{})",
                    esc(intern.get(*name)),
                    dump_node(ast, *a),
                    dump_node(ast, *b),
                    c.map_or_else(|| "null".to_string(), |x| dump_node(ast, x)),
                    dump_node(ast, *body)
                ),
                Node::Forin(names, es, body) => format!(
                    "forin(names:[{}],es:[{}],body:{})",
                    join(names.iter().map(|n| esc(intern.get(*n))), ","),
                    join(es.iter().map(|e| dump_node(ast, *e)), ","),
                    dump_node(ast, *body)
                ),
                Node::Funcstat(target, fn_) => format!(
                    "funcstat(target:{},fn:{})",
                    dump_node(ast, *target),
                    dump_node(ast, *fn_)
                ),
                Node::Localfunc(name, fn_) => format!(
                    "localfunc(name:{},fn:{})",
                    esc(intern.get(*name)),
                    dump_node(ast, *fn_)
                ),
                Node::Local(names, es) => format!(
                    "local(names:[{}],es:[{}])",
                    join(names.iter().map(|n| esc(intern.get(*n))), ","),
                    join(es.iter().map(|e| dump_node(ast, *e)), ",")
                ),
                Node::Return(es) => format!(
                    "return(es:[{}])",
                    join(es.iter().map(|e| dump_node(ast, *e)), ",")
                ),
                Node::Callstat(e) => format!("callstat(e:{})", dump_node(ast, *e)),
                Node::Assign(vs, es) => format!(
                    "assign(vs:[{}],es:[{}])",
                    join(vs.iter().map(|v| dump_node(ast, *v)), ","),
                    join(es.iter().map(|e| dump_node(ast, *e)), ",")
                ),
                Node::Nil => "nil()".to_string(),
                Node::Bool(v) => format!("bool(v:{})", t(*v)),
                Node::Vararg => "vararg()".to_string(),
                Node::Num(v) => format!("num(v:{})", esc(v)),
                Node::Str(v) => format!("str(v:{})", esc(v)),
                Node::Function(ps, variadic, b) => format!(
                    "function(ps:[{}],var:{},b:{})",
                    join(ps.iter().map(|p| esc(intern.get(*p))), ","),
                    t(*variadic),
                    dump_node(ast, *b)
                ),
                Node::Table(fs) => format!(
                    "table(fs:[{}])",
                    join(
                        fs.iter().map(|f| match f {
                            TableField::Arr(v) => format!("[arr,null,{}]", dump_node(ast, *v)),
                            TableField::Name(k, v) =>
                                format!("[name,{},{}]", esc(intern.get(*k)), dump_node(ast, *v)),
                            TableField::KVar(k, v) =>
                                format!("[kv,{},{}]", dump_node(ast, *k), dump_node(ast, *v)),
                        }),
                        ","
                    )
                ),
                Node::Un(op, e) => format!("un(op:{},e:{})", esc(op), dump_node(ast, *e)),
                Node::Bin(op, l, r) => format!(
                    "bin(op:{},l:{},r:{})",
                    esc(op),
                    dump_node(ast, *l),
                    dump_node(ast, *r)
                ),
                Node::Paren(e) => format!("paren(e:{})", dump_node(ast, *e)),
                Node::Call(fn_, args, method) => format!(
                    "call(fn:{},args:[{}],method:{})",
                    dump_node(ast, *fn_),
                    join(args.iter().map(|a| dump_node(ast, *a)), ","),
                    method.as_deref().map_or_else(|| "null".to_string(), esc)
                ),
                Node::Name(v) => format!("name(v:{})", esc(intern.get(*v))),
                Node::Index(obj, key, dot) => format!(
                    "index(obj:{},key:{},dot:{})",
                    dump_node(ast, *obj),
                    dump_node(ast, *key),
                    t(*dot)
                ),
                Node::Methodname(obj, name) => format!(
                    "methodname(obj:{},name:{})",
                    dump_node(ast, *obj),
                    esc(intern.get(*name))
                ),
            }
        }
    }
    #[test]
    fn streaming_dump_matches_the_frozen_recursive_format_for_all_nodes() {
        let fixtures=[
            "local x,y=nil,true;::again:: do x=false end while x do break end repeat x=x+1 until x>3 if x then y=1 elseif y then y=2 else goto again end",
            "local function f(x,...)return ... end function a.b:method(x)for i=1,5,2 do f(i)end for k,v in pairs(x)do f(k,v)end end",
            "local table={1,field='雪😀',[ [=[long]=] ]=4}; return -(-1),1 .. 'a',(table)['x'],f{},a:m'hello',function()return 1 end",
            "return 0x10,16,1.0,1e0,[==[a\nb]==], '\\u{1f600}', true,false,nil,...",
        ];
        for source in fixtures {
            let (ast, _) = crate::parse_source(source).unwrap();
            for id in 0..ast.nodes.len() as NodeId {
                assert_eq!(dump(&ast, id), reference::dump(&ast, id));
            }
            let (tracked, _) = crate::parse_source_with_origins("fixture.lua", source).unwrap();
            for id in 0..tracked.nodes.len() as NodeId {
                assert_eq!(dump(&tracked, id), reference::dump(&tracked, id));
            }
        }
        let source = format!("return {}2{}", "1+(".repeat(128), ")".repeat(128));
        let (ast, root) = crate::parse_source(&source).unwrap();
        assert_eq!(dump(&ast, root), reference::dump(&ast, root));
    }
    #[test]
    fn quote_bytes_match_control_ascii_and_multibyte_text_without_normalization() {
        let mut ast = Ast::new();
        for s in [
            String::new(),
            (0u8..=127).map(char::from).collect(),
            "雪😀/\"\\\r\n\t\u{2028}".into(),
        ] {
            let id = ast.str(s);
            assert_eq!(dump(&ast, id), reference::dump(&ast, id));
        }
    }
}

/// Compare canonical node snapshots without materializing their text. This has
/// exactly `dump(ast,a)==dump(ast,b)` semantics: raw literals, parentheses,
/// field kinds and identifier spellings matter; positions and origin IDs do not.
/// Both nodes must belong to the same valid, acyclic AST.
pub fn same(ast: &Ast, a: NodeId, b: NodeId) -> bool {
    let left = ast.node(a);
    let right = ast.node(b);
    if a == b {
        return true;
    }
    let symbol = |a, b| ast.strings.get(a) == ast.strings.get(b);
    let symbols = |a: &[crate::ast::SymbolId], b: &[crate::ast::SymbolId]| {
        a.len() == b.len() && a.iter().zip(b).all(|(&a, &b)| symbol(a, b))
    };
    let nodes = |a: &[NodeId], b: &[NodeId]| {
        a.len() == b.len() && a.iter().zip(b).all(|(&a, &b)| same(ast, a, b))
    };
    let optional = |a: Option<NodeId>, b: Option<NodeId>| match (a, b) {
        (Some(a), Some(b)) => same(ast, a, b),
        (None, None) => true,
        _ => false,
    };
    match (left, right) {
        (Node::Block(a), Node::Block(b)) | (Node::Return(a), Node::Return(b)) => nodes(a, b),
        (Node::Nil, Node::Nil) | (Node::Break, Node::Break) | (Node::Vararg, Node::Vararg) => true,
        (Node::Bool(a), Node::Bool(b)) => a == b,
        (Node::Name(a), Node::Name(b))
        | (Node::Goto(a), Node::Goto(b))
        | (Node::Label(a), Node::Label(b)) => symbol(*a, *b),
        (Node::Num(a), Node::Num(b)) | (Node::Str(a), Node::Str(b)) => a == b,
        (Node::Do(a), Node::Do(b))
        | (Node::Callstat(a), Node::Callstat(b))
        | (Node::Paren(a), Node::Paren(b)) => same(ast, *a, *b),
        (Node::While(a, b), Node::While(c, d))
        | (Node::Repeat(a, b), Node::Repeat(c, d))
        | (Node::Funcstat(a, b), Node::Funcstat(c, d)) => same(ast, *a, *c) && same(ast, *b, *d),
        (Node::Assign(a, b), Node::Assign(c, d)) => nodes(a, c) && nodes(b, d),
        (Node::Local(a, b), Node::Local(c, d)) => symbols(a, c) && nodes(b, d),
        (Node::Localfunc(a, b), Node::Localfunc(c, d)) => symbol(*a, *c) && same(ast, *b, *d),
        (Node::If(a, b), Node::If(c, d)) => {
            a.len() == c.len()
                && a.iter()
                    .zip(c)
                    .all(|(a, b)| same(ast, a.cond, b.cond) && same(ast, a.body, b.body))
                && optional(*b, *d)
        }
        (Node::Fornum(a, b, c, d, e), Node::Fornum(f, g, h, i, j)) => {
            symbol(*a, *f)
                && same(ast, *b, *g)
                && same(ast, *c, *h)
                && optional(*d, *i)
                && same(ast, *e, *j)
        }
        (Node::Forin(a, b, c), Node::Forin(d, e, f)) => {
            symbols(a, d) && nodes(b, e) && same(ast, *c, *f)
        }
        (Node::Function(a, b, c), Node::Function(d, e, f)) => {
            symbols(a, d) && b == e && same(ast, *c, *f)
        }
        (Node::Table(a), Node::Table(b)) => {
            a.len() == b.len()
                && a.iter().zip(b).all(|(a, b)| match (a, b) {
                    (TableField::Arr(a), TableField::Arr(b)) => same(ast, *a, *b),
                    (TableField::Name(a, b), TableField::Name(c, d)) => {
                        symbol(*a, *c) && same(ast, *b, *d)
                    }
                    (TableField::KVar(a, b), TableField::KVar(c, d)) => {
                        same(ast, *a, *c) && same(ast, *b, *d)
                    }
                    _ => false,
                })
        }
        (Node::Un(a, b), Node::Un(c, d)) => a == c && same(ast, *b, *d),
        (Node::Bin(a, b, c), Node::Bin(d, e, f)) => {
            a == d && same(ast, *b, *e) && same(ast, *c, *f)
        }
        (Node::Call(a, b, c), Node::Call(d, e, f)) => same(ast, *a, *d) && nodes(b, e) && c == f,
        (Node::Index(a, b, c), Node::Index(d, e, f)) => {
            same(ast, *a, *d) && same(ast, *b, *e) && c == f
        }
        (Node::Methodname(a, b), Node::Methodname(c, d)) => same(ast, *a, *c) && symbol(*b, *d),
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod structural_snapshot_tests {
    use super::*;
    #[test]
    fn direct_comparison_matches_dump_for_every_node_kind_and_raw_spelling() {
        for source in [
            "local a,b=1,2 do a=b end ::again:: if a then a=a-1 elseif b then b=3 else break end goto again",
            "local function f(x,...)while x do repeat x=x-1 until x<2 end return ... end function tab:run(v)for i=1,3,2 do f(i)end for k,v in pairs(tab)do tab[k]=v end end",
            "local t={1,field=2,['field']=2,[x+1]=f(3)} local s='雪😀' return {t},function(a)return a end, (f)(2),tab:run(3),nil,true,false,-(2),~1,1.0,0x1,'a',\"a\"",
            "if a then f(a)end if a then f(a)end x=t.a y=t['a'] z=t[a]",
        ] {
            let (mut ast,_)=crate::parse_source_with_origins("same.lua",source).unwrap();
            let len=ast.nodes.len();
            // Equal nodes at different arena offsets cannot depend on origin IDs.
            for id in 0..len as NodeId {ast.push(ast.node(id).clone());}
            let expected=(0..ast.nodes.len() as NodeId).map(|id|dump(&ast,id)).collect::<Vec<_>>();
            for a in 0..ast.nodes.len() as NodeId {
                for b in 0..ast.nodes.len() as NodeId {
                    assert_eq!(same(&ast,a,b),expected[a as usize]==expected[b as usize],"nodes {a}/{b}");
                }
            }
            let json=serde_json::to_vec(&ast).unwrap();let restored:Ast=serde_json::from_slice(&json).unwrap();
            for a in 0..len as NodeId {assert!(same(&restored,a,a+len as NodeId));}
        }
    }
    #[test]
    fn raw_literals_field_spelling_and_aliasing_are_not_semantically_collapsed() {
        let mut ast = Ast::new();
        let roots = ["1", "1.0", "01", "0x1", "9223372036854775807", "-0", "-0.0"]
            .map(|n| ast.push(Node::Num(n.into())));
        for (i, &a) in roots.iter().enumerate() {
            for (j, &b) in roots.iter().enumerate() {
                assert_eq!(same(&ast, a, b), i == j);
            }
        }
        let a = ast.push(Node::Str("'a'".into()));
        let b = ast.push(Node::Str("\"a\"".into()));
        assert!(!same(&ast, a, b));
        let symbol = ast.strings.intern("x");
        let x = ast.push(Node::Name(symbol));
        let y = ast.push(Node::Name(symbol));
        let a = ast.push(Node::Bin("+".into(), x, x));
        let b = ast.push(Node::Bin("+".into(), y, x));
        assert!(same(&ast, a, b));
        let c = ast.push(Node::Paren(b));
        assert!(!same(&ast, a, c));
    }
}
