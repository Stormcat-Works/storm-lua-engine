//! Resolver/effects migration snapshot 用の canonical dump format。
//!
//! アノテーション付き AST / bindings / scopes / globals / apiAliases / summaries の
//! 6 区画連結ダンプを TS と byte 単位で一致させる。決定性の契約（D-51）は
//! tests/resolver_parity.rs が expected-resolve/ 105 件で検証する。

use crate::effects::EffectAnalyzer;
use crate::resolver::{BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};

fn esc(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        let cp = c as u32;
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ if cp < 0x20 => out.push_str(&format!("\\u00{:02x}", cp)),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn t(v: bool) -> &'static str {
    if v {
        "true"
    } else {
        "false"
    }
}

fn l(xs: &[u32]) -> String {
    let s: Vec<String> = xs.iter().map(u32::to_string).collect();
    format!("[{}]", s.join(","))
}

fn dump_annotated(ast: &Ast, res: &Resolution, nid: NodeId, out: &mut String) {
    match &ast.nodes[nid as usize] {
        Node::Block(ss) => {
            out.push_str("block(ss:[");
            for (i, s) in ss.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                dump_annotated(ast, res, *s, out);
            }
            out.push_str("],scopeId:");
            out.push_str(&scope_id_str(res, nid));
            out.push(')');
        }
        Node::Break => out.push_str("break()"),
        Node::Goto(sid) => {
            out.push_str("goto(name:");
            out.push_str(&esc(ast.strings.get(*sid)));
            out.push(')');
        }
        Node::Label(sid) => {
            out.push_str("label(name:");
            out.push_str(&esc(ast.strings.get(*sid)));
            out.push(')');
        }
        Node::Do(b) => {
            out.push_str("do(b:");
            dump_annotated(ast, res, *b, out);
            out.push(')');
        }
        Node::While(e, b) => {
            out.push_str("while(e:");
            dump_annotated(ast, res, *e, out);
            out.push_str(",b:");
            dump_annotated(ast, res, *b, out);
            out.push(')');
        }
        Node::Repeat(b, e) => {
            out.push_str("repeat(b:");
            dump_annotated(ast, res, *b, out);
            out.push_str(",e:");
            dump_annotated(ast, res, *e, out);
            out.push(')');
        }
        Node::If(arms, eb) => {
            out.push_str("if(arms:[");
            for (i, arm) in arms.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('[');
                dump_annotated(ast, res, arm.cond, out);
                out.push(',');
                dump_annotated(ast, res, arm.body, out);
                out.push(']');
            }
            out.push_str("],eb:");
            match eb {
                Some(e) => dump_annotated(ast, res, *e, out),
                None => out.push_str("null"),
            }
            out.push(')');
        }
        Node::Fornum(name, a, b, c, body) => {
            out.push_str("fornum(name:");
            out.push_str(&esc(ast.strings.get(*name)));
            out.push_str(",a:");
            dump_annotated(ast, res, *a, out);
            out.push_str(",b:");
            dump_annotated(ast, res, *b, out);
            out.push_str(",c:");
            match c {
                Some(c) => dump_annotated(ast, res, *c, out),
                None => out.push_str("null"),
            }
            out.push_str(",body:");
            dump_annotated(ast, res, *body, out);
            out.push_str(",bid:");
            out.push_str(&bid_str(res, nid));
            out.push_str(",scopeId:");
            out.push_str(&scope_id_str(res, nid));
            out.push(')');
        }
        Node::Forin(names, es, body) => {
            out.push_str("forin(names:[");
            for (i, n) in names.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&esc(ast.strings.get(*n)));
            }
            out.push_str("],es:[");
            for (i, e) in es.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                dump_annotated(ast, res, *e, out);
            }
            out.push_str("],body:");
            dump_annotated(ast, res, *body, out);
            out.push_str(",bids:");
            out.push_str(&bids_str(res, nid));
            out.push_str(",scopeId:");
            out.push_str(&scope_id_str(res, nid));
            out.push(')');
        }
        Node::Funcstat(target, fn_) => {
            out.push_str("funcstat(target:");
            dump_annotated(ast, res, *target, out);
            out.push_str(",fn:");
            dump_annotated(ast, res, *fn_, out);
            out.push(')');
        }
        Node::Localfunc(name, fn_) => {
            out.push_str("localfunc(name:");
            out.push_str(&esc(ast.strings.get(*name)));
            out.push_str(",bid:");
            out.push_str(&bid_str(res, nid));
            out.push_str(",fn:");
            dump_annotated(ast, res, *fn_, out);
            out.push(')');
        }
        Node::Local(names, es) => {
            out.push_str("local(names:[");
            for (i, n) in names.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&esc(ast.strings.get(*n)));
            }
            out.push_str("],es:[");
            for (i, e) in es.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                dump_annotated(ast, res, *e, out);
            }
            out.push_str("],bids:");
            out.push_str(&bids_str(res, nid));
            out.push(')');
        }
        Node::Return(es) => {
            out.push_str("return(es:[");
            for (i, e) in es.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                dump_annotated(ast, res, *e, out);
            }
            out.push(']');
            out.push(')');
        }
        Node::Callstat(e) => {
            out.push_str("callstat(e:");
            dump_annotated(ast, res, *e, out);
            out.push(')');
        }
        Node::Assign(vs, es) => {
            out.push_str("assign(vs:[");
            for (i, v) in vs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                dump_annotated(ast, res, *v, out);
            }
            out.push_str("],es:[");
            for (i, e) in es.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                dump_annotated(ast, res, *e, out);
            }
            out.push(']');
            out.push(')');
        }
        Node::Nil => out.push_str("nil()"),
        Node::Bool(v) => {
            out.push_str("bool(v:");
            out.push_str(t(*v));
            out.push(')');
        }
        Node::Vararg => out.push_str("vararg()"),
        Node::Num(v) => {
            out.push_str("num(v:");
            out.push_str(&esc(v));
            out.push(')');
        }
        Node::Str(v) => {
            out.push_str("str(v:");
            out.push_str(&esc(v));
            out.push(')');
        }
        Node::Function(ps, var, b) => {
            out.push_str("function(ps:[");
            for (i, p) in ps.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&esc(ast.strings.get(*p)));
            }
            out.push_str("],var:");
            out.push_str(t(*var));
            out.push_str(",b:");
            dump_annotated(ast, res, *b, out);
            out.push_str(",bids:");
            out.push_str(&bids_str(res, nid));
            out.push_str(",scopeId:");
            out.push_str(&scope_id_str(res, nid));
            out.push(')');
        }
        Node::Table(fs) => {
            out.push_str("table(fs:[");
            for (i, f) in fs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                match f {
                    storm_lua_syntax::ast::TableField::Arr(v) => {
                        out.push_str("[arr,null,");
                        dump_annotated(ast, res, *v, out);
                        out.push(']');
                    }
                    storm_lua_syntax::ast::TableField::Name(k, v) => {
                        out.push_str("[name,");
                        out.push_str(&esc(ast.strings.get(*k)));
                        out.push(',');
                        dump_annotated(ast, res, *v, out);
                        out.push(']');
                    }
                    storm_lua_syntax::ast::TableField::KVar(k, v) => {
                        out.push_str("[kv,");
                        dump_annotated(ast, res, *k, out);
                        out.push(',');
                        dump_annotated(ast, res, *v, out);
                        out.push(']');
                    }
                }
            }
            out.push(']');
            out.push(')');
        }
        Node::Un(op, e) => {
            out.push_str("un(op:");
            out.push_str(&esc(op));
            out.push_str(",e:");
            dump_annotated(ast, res, *e, out);
            out.push(')');
        }
        Node::Bin(op, l, r) => {
            out.push_str("bin(op:");
            out.push_str(&esc(op));
            out.push_str(",l:");
            dump_annotated(ast, res, *l, out);
            out.push_str(",r:");
            dump_annotated(ast, res, *r, out);
            out.push(')');
        }
        Node::Paren(e) => {
            out.push_str("paren(e:");
            dump_annotated(ast, res, *e, out);
            out.push(')');
        }
        Node::Call(fn_, args, method) => {
            out.push_str("call(fn:");
            dump_annotated(ast, res, *fn_, out);
            out.push_str(",args:[");
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                dump_annotated(ast, res, *a, out);
            }
            out.push_str("],method:");
            match method {
                Some(m) => out.push_str(&esc(ast.strings.get(*m))),
                None => out.push_str("null"),
            }
            out.push(')');
        }
        Node::Name(sid) => {
            out.push_str("name(v:");
            out.push_str(&esc(ast.strings.get(*sid)));
            out.push_str(",bid:");
            out.push_str(&bid_str(res, nid));
            out.push_str(",write:");
            out.push_str(t(res.node_write[nid as usize]));
            out.push(')');
        }
        Node::Index(obj, key, dot) => {
            out.push_str("index(obj:");
            dump_annotated(ast, res, *obj, out);
            out.push_str(",key:");
            dump_annotated(ast, res, *key, out);
            out.push_str(",dot:");
            out.push_str(t(*dot));
            out.push(')');
        }
        Node::Methodname(obj, name) => {
            out.push_str("methodname(obj:");
            dump_annotated(ast, res, *obj, out);
            out.push_str(",name:");
            out.push_str(&esc(ast.strings.get(*name)));
            out.push(')');
        }
    }
}

fn bid_str(res: &Resolution, nid: NodeId) -> String {
    match res.node_bid[nid as usize] {
        Some(b) => b.to_string(),
        None => "null".to_string(),
    }
}

fn bids_str(res: &Resolution, nid: NodeId) -> String {
    l(&res.node_bids[nid as usize])
}

fn scope_id_str(res: &Resolution, nid: NodeId) -> String {
    match res.node_scope_id[nid as usize] {
        Some(v) => v.to_string(),
        None => "null".to_string(),
    }
}

/// walk（TS walk 順）で全ノードを走査して Resolution ダンプを生成する。
pub struct ResolveDumper<'a> {
    pub ast: &'a Ast,
    pub res: &'a Resolution,
    pub analyzer: &'a EffectAnalyzer<'a>,
}

impl<'a> ResolveDumper<'a> {
    pub fn new(ast: &'a Ast, res: &'a Resolution, analyzer: &'a EffectAnalyzer<'a>) -> Self {
        Self { ast, res, analyzer }
    }

    pub fn dump_resolution(&self, root: NodeId) -> String {
        let mut parts: Vec<String> = Vec::new();
        parts.push("---annotated---".to_string());
        let mut ann = String::new();
        dump_annotated(self.ast, self.res, root, &mut ann);
        parts.push(ann);
        parts.push("---bindings---".to_string());
        for id in 1..self.res.bindings.len() as u32 {
            let b = &self.res.bindings[id as usize];
            parts.push(format!(
                "{}:name={} kind={} scope={} fixed={} decl={} last={} freq={} group={} fn={}",
                id,
                esc(self.ast.strings.get(b.name)),
                kind_str(&b.kind),
                b.scope,
                t(b.fixed),
                b.decl,
                b.last,
                b.freq,
                b.group,
                if b.function_node.is_some() { 1 } else { 0 }
            ));
        }
        parts.push("---scopes---".to_string());
        for s in &self.res.scopes {
            let parent = match s.parent {
                Some(p) => p.to_string(),
                None => "null".to_string(),
            };
            parts.push(format!(
                "{}:parent={} bindings={} children={} refs={}",
                s.id,
                parent,
                l(&s.bindings),
                l(&s.children),
                l(&s.refs)
            ));
        }
        parts.push("---globals---".to_string());
        for (name, id) in &self.res.globals {
            parts.push(format!("{}:{}", esc(self.ast.strings.get(*name)), id));
        }
        parts.push("---apiAliases---".to_string());
        for (bid, alias) in &self.analyzer.api_alias_order {
            parts.push(format!("{}:{}", bid, esc(alias)));
        }
        parts.push("---summaries---".to_string());
        for id in 1..self.res.bindings.len() as u32 {
            let Some(s) = &self.analyzer.summaries[id as usize] else {
                continue;
            };
            let reads: Vec<String> = s.reads.iter().map(u32::to_string).collect();
            let writes: Vec<String> = s.writes.iter().map(u32::to_string).collect();
            parts.push(format!(
                "{}:pure={} total={} reads=[{}] writes=[{}] calls={} ordered={} mayThrow={} stable={}",
                id,
                t(s.pure),
                t(s.total),
                reads.join(","),
                writes.join(","),
                t(s.calls),
                t(s.ordered),
                t(s.may_throw),
                t(s.stable)
            ));
        }
        parts.join("\n") + "\n"
    }
}

fn kind_str(k: &BindingKind) -> &'static str {
    match k {
        BindingKind::Global => "global",
        BindingKind::Local => "local",
        BindingKind::Param => "param",
        BindingKind::Localfunc => "localfunc",
        BindingKind::For => "for",
    }
}
