//! Source-size measurement using the same lexical rules as emission.

// Canonical size model。measureSize(root) == Printer::print(root).len、
// measureExpr(node) == Printer::expr(node).len を文字列を組み立てずに長さ計算だけで満たす。
//
// 正確性の根拠: printer.rs（オラクル②で byte parity 確立済み）の出力形状と同一の規則を
// 長さ計算として再現する。tests/measure-size.test.ts の差分化テスト相当を Rust 側でも行う。

use crate::ast::{Ast, Node, NodeId, TableField};
use crate::numeric::{qstr_bracket_key_shape, qstr_shape};
use crate::parser::NameSite;
use crate::print::{
    bin_prec, block_items, plain_name_from_string_literal, render_num, trimmed_nil_rhs_len,
    BlockItem, LocalMember,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Seg {
    len: usize,
    first: Option<char>,
    last: Option<char>,
}

impl Seg {
    const ZERO: Seg = Seg {
        len: 0,
        first: None,
        last: None,
    };
}

/// needsSepSeg: 先頭・末尾 1 文字だけで区切り要否を判定（printer の needs_sep と同一規則）。
fn needs_len_sep(a: Option<char>, b: Option<char>) -> bool {
    let (Some(x), Some(y)) = (a, b) else {
        return false;
    };
    if (x.is_ascii_alphanumeric() || x == '_') && (y.is_ascii_alphanumeric() || y == '_') {
        return true;
    }
    if x == '-' && y == '-' {
        return true;
    }
    if x == '.' && y == '.' {
        return true;
    }
    // Match the printer's numeric-token boundary (for example `1 ..2`).
    if x.is_ascii_digit() && y == '.' {
        return true;
    }
    false
}

/// Supplies alternate identifier spellings for exact cost-only size measurement.
/// Unmentioned sites retain the spelling stored in the AST.
pub trait SizeNameResolver {
    /// Replacement spelling for one syntactic identifier occurrence.
    /// Returning None keeps the symbol spelling stored in the AST.
    fn replacement(&self, node: NodeId, site: NameSite) -> Option<&str>;
    /// Cost models may replace this call's callee with an identifier. Its
    /// arguments, method name, parentheses and shorthand retain printer rules.
    fn call_target(&self, _call: NodeId) -> Option<&str> {
        None
    }
}

struct SizeCtx<'a, 'n> {
    ast: &'a Ast,
    names: Option<&'n dyn SizeNameResolver>,
    /// `measure_size_with_empty_if_check` 用: 空の arms を持つ if 文を
    /// 走査中に見つけたら立つ（他の計測では常に false のまま）。
    empty_if: std::cell::Cell<bool>,
}

impl<'a, 'n> SizeCtx<'a, 'n> {
    fn seg_of(&self, s: &str) -> Seg {
        let len = s.len();
        if len == 0 {
            return Seg::ZERO;
        }
        Seg {
            len,
            first: s.chars().next(),
            last: s.chars().next_back(),
        }
    }

    fn raw_name(&self, id: crate::ast::SymbolId) -> &str {
        self.ast.strings.get(id)
    }

    fn name_seg(&self, node: NodeId, site: NameSite, id: crate::ast::SymbolId) -> Seg {
        self.seg_of(
            self.names
                .and_then(|resolver| resolver.replacement(node, site))
                .unwrap_or_else(|| self.raw_name(id)),
        )
    }

    fn qstr_seg(&self, raw: &str) -> Seg {
        let (len, first, last) = qstr_shape(raw);
        Seg {
            len,
            first: Some(first),
            last: Some(last),
        }
    }

    fn bracket_key_seg(&self, id: NodeId) -> Seg {
        if let Node::Str(raw) = self.ast.node(id) {
            let (len, first, last) = qstr_bracket_key_shape(raw);
            let key = Seg {
                len,
                first: Some(first),
                last: Some(last),
            };
            if first == '[' {
                return self.concat_segs(&[self.seg_of(" "), key]);
            }
            return key;
        }
        self.expr_seg(id, 0)
    }

    fn cat_seg(&self, a: Seg, b: Seg) -> Seg {
        if b.len == 0 {
            return a;
        }
        if a.len == 0 {
            return b;
        }
        let sep = usize::from(needs_len_sep(a.last, b.first));
        Seg {
            len: a.len + sep + b.len,
            first: a.first,
            last: b.last,
        }
    }

    fn concat_segs(&self, parts: &[Seg]) -> Seg {
        let mut len = 0usize;
        let mut first = None;
        let mut last = None;
        for p in parts {
            if p.len == 0 {
                continue;
            }
            if first.is_none() {
                first = p.first;
            }
            last = p.last;
            len += p.len;
        }
        Seg { len, first, last }
    }

    fn join_lex_segs(&self, parts: &[Seg]) -> Seg {
        let mut len = 0usize;
        let mut first = None;
        let mut last = None;
        for p in parts {
            if p.len == 0 {
                continue;
            }
            if len > 0 && needs_len_sep(last, p.first) {
                len += 1;
            }
            if first.is_none() {
                first = p.first;
            }
            last = p.last;
            len += p.len;
        }
        Seg { len, first, last }
    }

    fn join_comma_segs(&self, parts: impl IntoIterator<Item = Seg>) -> Seg {
        let mut len = 0usize;
        let mut first = None;
        let mut last = None;
        for (i, p) in parts.into_iter().enumerate() {
            if i > 0 {
                len += 1;
            }
            if first.is_none() {
                first = p.first;
            }
            last = p.last;
            len += p.len;
        }
        Seg { len, first, last }
    }

    fn block_seg(&self, id: NodeId) -> Seg {
        let Node::Block(ss) = &self.ast.nodes[id as usize] else {
            unreachable!()
        };
        // Shape the block exactly as the printer does: dead-`if` removal
        // and print-time `local` merging live in `print::block_items`,
        // shared with `Printer::block`, so the measured size of a block
        // always equals its printed length.
        let items = block_items(self.ast, ss, self.names);
        let mut len = 0usize;
        let mut first = None;
        let mut last = None;
        let mut previous: Option<Seg> = None;
        for item in &items {
            let p = match item {
                BlockItem::Single(statement) => self.stat_seg(*statement),
                BlockItem::Merged(members) if members.len() == 1 => self.stat_seg(members[0].stmt),
                BlockItem::Merged(members) => self.merged_local_seg(members),
            };
            if let Some(prev) = previous {
                let sep = p.first == Some('(') || needs_len_sep(prev.last, p.first);
                if sep {
                    len += 1;
                }
            }
            if first.is_none() {
                first = p.first;
            }
            if p.len > 0 {
                last = p.last;
                len += p.len;
            }
            // Preserve the immediate predecessor even when its print is empty.
            previous = Some(p);
        }
        Seg { len, first, last }
    }

    /// Size of a merged `local` run, mirroring `Printer::merged_local`:
    /// one `local` keyword, every member's names joined by commas, and
    /// every member's kept initializers joined by commas.
    fn merged_local_seg(&self, members: &[LocalMember]) -> Seg {
        let name_parts = members.iter().flat_map(|member| {
            member.names.iter().enumerate().map(|(index, name)| {
                self.name_seg(member.stmt, NameSite::Binding(index as u32), *name)
            })
        });
        let value_parts = members
            .iter()
            .flat_map(|member| member.kept.iter())
            .map(|expr| self.expr_seg(*expr, 0))
            .collect::<Vec<_>>();
        if value_parts.is_empty() {
            self.concat_segs(&[self.seg_of("local "), self.join_comma_segs(name_parts)])
        } else {
            self.concat_segs(&[
                self.seg_of("local "),
                self.join_comma_segs(name_parts),
                self.seg_of("="),
                self.join_comma_segs(value_parts),
            ])
        }
    }

    fn target_seg(&self, id: NodeId) -> Seg {
        if let Node::Methodname(obj, name) = &self.ast.nodes[id as usize] {
            return self.concat_segs(&[
                self.expr_seg(*obj, 0),
                self.seg_of(":"),
                self.seg_of(self.raw_name(*name)),
            ]);
        }
        self.expr_seg(id, 0)
    }

    fn fn_header_seg(&self, prefix: &str, fn_id: NodeId) -> Seg {
        let Node::Function(ps, var, _) = &self.ast.nodes[fn_id as usize] else {
            unreachable!()
        };
        let params = ps
            .iter()
            .enumerate()
            .map(|(index, p)| self.name_seg(fn_id, NameSite::Parameter(index as u32), *p));
        let var_extra = if *var {
            if ps.is_empty() {
                "..."
            } else {
                ",..."
            }
        } else {
            ""
        };
        self.concat_segs(&[
            self.seg_of(prefix),
            self.join_comma_segs(params),
            self.seg_of(var_extra),
            self.seg_of(")"),
        ])
    }

    fn function_expr_seg(&self, id: NodeId) -> Seg {
        let Node::Function(_, _, b) = &self.ast.nodes[id as usize] else {
            unreachable!()
        };
        let h = self.fn_header_seg("function(", id);
        self.cat_seg(h, self.cat_seg(self.block_seg(*b), self.seg_of("end")))
    }

    fn function_tail_seg(&self, id: NodeId) -> Seg {
        let Node::Function(_, _, b) = &self.ast.nodes[id as usize] else {
            unreachable!()
        };
        let h = self.fn_header_seg("(", id);
        self.cat_seg(h, self.cat_seg(self.block_seg(*b), self.seg_of("end")))
    }

    fn expr_seg(&self, id: NodeId, parent: u8) -> Seg {
        if parent == 15
            && !matches!(
                self.ast.node(id),
                Node::Name(_) | Node::Paren(_) | Node::Index(..) | Node::Call(..)
            )
        {
            return self.concat_segs(&[self.seg_of("("), self.expr_seg(id, 0), self.seg_of(")")]);
        }
        let node = &self.ast.nodes[id as usize];
        match node {
            Node::Name(sym) => self.name_seg(id, NameSite::Reference, *sym),
            Node::Num(v) => {
                let text = render_num(v);
                let segment = self.seg_of(&text);
                if (text.contains('/') && parent > 11) || (text.starts_with('-') && parent >= 14) {
                    self.concat_segs(&[self.seg_of("("), segment, self.seg_of(")")])
                } else {
                    segment
                }
            }
            Node::Str(v) => self.qstr_seg(v),
            Node::Nil => self.seg_of("nil"),
            Node::Bool(b) => self.seg_of(if parent <= 3 {
                if *b {
                    "1>0"
                } else {
                    "1>2"
                }
            } else if *b {
                "true"
            } else {
                "false"
            }),
            Node::Vararg => self.seg_of("..."),
            Node::Paren(e) => {
                self.concat_segs(&[self.seg_of("("), self.expr_seg(*e, 0), self.seg_of(")")])
            }
            Node::Table(fs) => {
                let f_segs = fs.iter().map(|f| match f {
                    TableField::Arr(v) => self.expr_seg(*v, 0),
                    TableField::Name(k, v) => self.concat_segs(&[
                        self.seg_of(self.raw_name(*k)),
                        self.seg_of("="),
                        self.expr_seg(*v, 0),
                    ]),
                    TableField::KVar(k, v) => {
                        if let Node::Str(raw) = &self.ast.nodes[*k as usize] {
                            if let Some(key) = plain_name_from_string_literal(raw) {
                                return self.concat_segs(&[
                                    self.seg_of(&key),
                                    self.seg_of("="),
                                    self.expr_seg(*v, 0),
                                ]);
                            }
                        }
                        let key = self.bracket_key_seg(*k);
                        self.concat_segs(&[
                            self.seg_of("["),
                            key,
                            self.seg_of("]"),
                            self.seg_of("="),
                            self.expr_seg(*v, 0),
                        ])
                    }
                });
                let segment = self.concat_segs(&[
                    self.seg_of("{"),
                    self.join_comma_segs(f_segs),
                    self.seg_of("}"),
                ]);
                if parent == 15 {
                    self.concat_segs(&[self.seg_of("("), segment, self.seg_of(")")])
                } else {
                    segment
                }
            }
            Node::Index(obj, key, _) => {
                let obj_s = self.expr_seg(*obj, 15);
                if let Node::Str(kv) = &self.ast.nodes[*key as usize] {
                    if let Some(k) = plain_name_from_string_literal(kv) {
                        return self.concat_segs(&[obj_s, self.seg_of("."), self.seg_of(&k)]);
                    }
                }
                let key_s = self.bracket_key_seg(*key);
                self.concat_segs(&[obj_s, self.seg_of("["), key_s, self.seg_of("]")])
            }
            Node::Function { .. } => {
                let segment = self.function_expr_seg(id);
                if parent == 15 {
                    self.concat_segs(&[self.seg_of("("), segment, self.seg_of(")")])
                } else {
                    segment
                }
            }
            Node::Call(fn_, args, method) => {
                let fn_s = self
                    .names
                    .and_then(|names| names.call_target(id))
                    .map(|name| self.seg_of(name))
                    .unwrap_or_else(|| self.expr_seg(*fn_, 15));
                let j = self.expr_list_seg(args);
                if args.len() == 1
                    && matches!(self.ast.node(args[0]), Node::Str(_) | Node::Table(_))
                {
                    return if let Some(m) = method {
                        self.concat_segs(&[
                            fn_s,
                            self.seg_of(":"),
                            self.seg_of(self.ast.strings.get(*m)),
                            j,
                        ])
                    } else {
                        self.concat_segs(&[fn_s, j])
                    };
                }
                if let Some(m) = method {
                    self.concat_segs(&[
                        fn_s,
                        self.seg_of(":"),
                        self.seg_of(self.ast.strings.get(*m)),
                        self.seg_of("("),
                        j,
                        self.seg_of(")"),
                    ])
                } else {
                    self.concat_segs(&[fn_s, self.seg_of("("), j, self.seg_of(")")])
                }
            }
            Node::Un(op, e) => {
                let s = self.join_lex_segs(&[self.seg_of(op), self.expr_seg(*e, 12)]);
                if 12 < parent {
                    self.concat_segs(&[self.seg_of("("), s, self.seg_of(")")])
                } else {
                    s
                }
            }
            Node::Bin(op, l, r) => {
                let (lp, rp) = bin_prec(op);
                let s = self.join_lex_segs(&[
                    self.expr_seg(*l, lp),
                    self.seg_of(op),
                    self.expr_seg(*r, rp),
                ]);
                if lp < parent {
                    self.concat_segs(&[self.seg_of("("), s, self.seg_of(")")])
                } else {
                    s
                }
            }
            _ => unreachable!("unknown expression node in size"),
        }
    }

    fn expr_list_seg(&self, ids: &[NodeId]) -> Seg {
        self.join_comma_segs(ids.iter().map(|a| self.expr_seg(*a, 0)))
    }

    fn stat_seg(&self, id: NodeId) -> Seg {
        let node = &self.ast.nodes[id as usize];
        match node {
            Node::Break => self.seg_of("break"),
            Node::Goto(name) => {
                self.concat_segs(&[self.seg_of("goto "), self.seg_of(self.raw_name(*name))])
            }
            Node::Label(name) => self.concat_segs(&[
                self.seg_of("::"),
                self.seg_of(self.raw_name(*name)),
                self.seg_of("::"),
            ]),
            Node::Do(b) => self.cat_seg(
                self.cat_seg(self.seg_of("do"), self.block_seg(*b)),
                self.seg_of("end"),
            ),
            Node::While(e, b) => {
                let head = self.cat_seg(self.seg_of("while"), self.expr_seg(*e, 0));
                let head = self.cat_seg(head, self.seg_of("do"));
                self.cat_seg(self.cat_seg(head, self.block_seg(*b)), self.seg_of("end"))
            }
            Node::Repeat(b, e) => {
                let head = self.cat_seg(self.seg_of("repeat"), self.block_seg(*b));
                let tail = self.cat_seg(self.seg_of("until"), self.expr_seg(*e, 0));
                self.cat_seg(head, tail)
            }
            Node::If(arms, eb) => {
                if arms.is_empty() {
                    // 空 if は不変条件違反として呼び出し側が検出できるよう
                    // 記録して打ち切る（arms[0] を引くとここで panic する）。
                    self.empty_if.set(true);
                    return Seg::ZERO;
                }
                let c0 = arms[0].cond;
                let b0 = arms[0].body;
                let head = self.cat_seg(self.seg_of("if"), self.expr_seg(c0, 0));
                let head = self.cat_seg(head, self.seg_of("then"));
                let mut out = self.cat_seg(head, self.block_seg(b0));
                for arm in arms.iter().skip(1) {
                    out = self.cat_seg(out, self.seg_of("elseif"));
                    out = self.cat_seg(out, self.expr_seg(arm.cond, 0));
                    out = self.cat_seg(out, self.seg_of("then"));
                    out = self.cat_seg(out, self.block_seg(arm.body));
                }
                if let Some(e) = eb {
                    out = self.cat_seg(out, self.seg_of("else"));
                    out = self.cat_seg(out, self.block_seg(*e));
                }
                self.cat_seg(out, self.seg_of("end"))
            }
            Node::Fornum(name, a, b, c, body) => {
                let step = c.map_or(Seg::ZERO, |c| {
                    self.concat_segs(&[self.seg_of(","), self.expr_seg(c, 0)])
                });
                let header = self.concat_segs(&[
                    self.seg_of("for "),
                    self.name_seg(id, NameSite::Binding(0), *name),
                    self.seg_of("="),
                    self.expr_seg(*a, 0),
                    self.seg_of(","),
                    self.expr_seg(*b, 0),
                    step,
                ]);
                let header = self.cat_seg(header, self.seg_of("do"));
                self.cat_seg(
                    self.cat_seg(header, self.block_seg(*body)),
                    self.seg_of("end"),
                )
            }
            Node::Forin(names, es, body) => {
                let n_segs = names
                    .iter()
                    .enumerate()
                    .map(|(index, n)| self.name_seg(id, NameSite::Binding(index as u32), *n));
                let e_segs = es.iter().map(|e| self.expr_seg(*e, 0));
                let header = self.concat_segs(&[
                    self.seg_of("for "),
                    self.join_comma_segs(n_segs),
                    self.seg_of(" in"),
                ]);
                let header = self.cat_seg(header, self.join_comma_segs(e_segs));
                let header = self.cat_seg(header, self.seg_of("do"));
                self.cat_seg(
                    self.cat_seg(header, self.block_seg(*body)),
                    self.seg_of("end"),
                )
            }
            Node::Funcstat(target, fn_) => self.concat_segs(&[
                self.seg_of("function "),
                self.target_seg(*target),
                self.function_tail_seg(*fn_),
            ]),
            Node::Localfunc(name, fn_) => self.concat_segs(&[
                self.seg_of("local function "),
                self.name_seg(id, NameSite::Binding(0), *name),
                self.function_tail_seg(*fn_),
            ]),
            Node::Local(names, es) => {
                let n_segs = names
                    .iter()
                    .enumerate()
                    .map(|(index, n)| self.name_seg(id, NameSite::Binding(index as u32), *n));
                let keep = trimmed_nil_rhs_len(self.ast, names.len(), es, true);
                self.concat_segs(&[
                    self.seg_of("local "),
                    self.join_comma_segs(n_segs),
                    self.seg_of(if keep == 0 { "" } else { "=" }),
                    self.expr_list_seg(&es[..keep]),
                ])
            }
            Node::Return(es) => {
                if es.is_empty() {
                    return self.seg_of("return");
                }
                self.cat_seg(self.seg_of("return"), self.expr_list_seg(es))
            }
            Node::Callstat(e) => self.expr_seg(*e, 0),
            Node::Assign(vs, es) => {
                let keep = trimmed_nil_rhs_len(self.ast, vs.len(), es, false);
                self.concat_segs(&[
                    self.expr_list_seg(vs),
                    self.seg_of("="),
                    self.expr_list_seg(&es[..keep]),
                ])
            }
            _ => unreachable!("unknown statement node in size"),
        }
    }

    fn measure_size(&self, root: NodeId) -> usize {
        self.block_seg(root).len
    }
}

/// `measure_size` と同時に、空の arms を持つ if 文が木に残っていないかも
/// 返す。サイズ計測は到達可能な全ての文を走査するため、不変条件検査を
/// 同じ走査に同乗させられる（従来は `walk` による独立した全木走査で
/// 検査していた）。空 if がある場合のサイズ値は不変条件違反で呼び出し側が
/// 破棄するため、意味を持たない。
pub fn measure_size_with_empty_if_check(ast: &Ast, root: NodeId) -> (usize, bool) {
    let ctx = SizeCtx {
        ast,
        names: None,
        empty_if: std::cell::Cell::new(false),
    };
    let size = ctx.measure_size(root);
    (size, ctx.empty_if.get())
}

/// measureSize 相当。root は block ノードであること。
pub fn measure_size(ast: &Ast, root: NodeId) -> usize {
    SizeCtx {
        ast,
        names: None,
        empty_if: std::cell::Cell::new(false),
    }
    .measure_size(root)
}

/// measureExpr 相当。単一式のサイズを返す。
pub fn measure_expr(ast: &Ast, id: NodeId) -> usize {
    SizeCtx {
        ast,
        names: None,
        empty_if: std::cell::Cell::new(false),
    }
    .expr_seg(id, 0)
    .len
}

/// 文単位のサイズ（デバッグ・テスト用）。
pub fn measure_stmt(ast: &Ast, id: NodeId) -> usize {
    SizeCtx {
        ast,
        names: None,
        empty_if: std::cell::Cell::new(false),
    }
    .stat_seg(id)
    .len
}

/// Exact size using alternate spellings for selected identifier occurrences.
/// This avoids constructing a renamed AST for cost-only measurements.
pub fn measure_size_with_names(ast: &Ast, root: NodeId, names: &dyn SizeNameResolver) -> usize {
    SizeCtx {
        ast,
        names: Some(names),
        empty_if: std::cell::Cell::new(false),
    }
    .measure_size(root)
}

/// Statement size with explicit identifier/callee substitutions, without AST mutation.
pub fn measure_stmt_with_names(ast: &Ast, node: NodeId, names: &dyn SizeNameResolver) -> usize {
    SizeCtx {
        ast,
        names: Some(names),
        empty_if: std::cell::Cell::new(false),
    }
    .stat_seg(node)
    .len
}
/// Expression size with explicit identifier/callee substitutions, without AST mutation.
pub fn measure_expr_with_names(ast: &Ast, node: NodeId, names: &dyn SizeNameResolver) -> usize {
    SizeCtx {
        ast,
        names: Some(names),
        empty_if: std::cell::Cell::new(false),
    }
    .expr_seg(node, 0)
    .len
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::ast::Node;
    use crate::parser::parse_source;

    #[test]
    fn empty_if_check_matches_measure_size_on_valid_trees() {
        for source in [
            "if true then return 1 end",
            "local x = 1\nif x then x = 2 else x = 3 end\nreturn x",
            "while true do if false then break end end",
        ] {
            let (ast, root) = parse_source(source).expect("parse ok");
            let (size, empty) = measure_size_with_empty_if_check(&ast, root);
            assert_eq!(size, measure_size(&ast, root), "{source}");
            assert!(!empty, "{source}");
        }
    }

    #[test]
    fn empty_if_check_detects_emptied_if_without_panicking() {
        let (mut ast, root) = parse_source("if true then return 1 end").expect("parse ok");
        let mut targets = vec![];
        crate::ast_utils::walk(&ast, root, &mut |id| {
            if matches!(ast.node(id), Node::If(_, _)) {
                targets.push(id);
            }
        });
        assert_eq!(targets.len(), 1);
        let eb = match ast.node(targets[0]) {
            Node::If(_, eb) => *eb,
            _ => unreachable!(),
        };
        ast.nodes[targets[0] as usize] = Node::If(vec![], eb);
        let (_size, empty) = measure_size_with_empty_if_check(&ast, root);
        assert!(empty);
    }

    fn assert_measure_matches_print(source: &str) {
        let (ast, root) = parse_source(source).expect("parse ok");
        let printed = crate::print::Printer::new(&ast, false).output(root);
        assert_eq!(
            measure_size(&ast, root),
            printed.len(),
            "measure/print diverged for {source:?}: printed {printed:?}"
        );
    }

    /// Regression: the printer merges a run of consecutive `local`
    /// statements into one statement at block level (and drops dead `if`
    /// statements before grouping), but size measurement counted every
    /// statement separately, overcounting each merged run by one
    /// `local ` per extra member (StormMin v0.4.0 regression cycle:
    /// printed 35 / measured 41 on the minimal case). Measurement must
    /// mirror the printer's block pipeline exactly, including every
    /// condition that blocks a merge.
    #[test]
    fn measure_matches_print_for_block_level_merges() {
        for source in [
            // Plain mergeable runs.
            "local a=1 local b=2 return a+b",
            "local a=1 local b=2 local c=3 return a+b+c",
            "local a=1 local b return a",
            "function f() local a=1 local b=2 return a+b end return f()",
            "do local a=1 local b=2 end return 0",
            "if x then local a=1 local b=2 end return 0",
            // Trailing literal nils are trimmed before the merge decision.
            "local a=1,nil local b=2 return b",
            // A dead `if` between locals is dropped before grouping.
            "local a=1 if true then end local b=2 return a+b",
            "if 1 then end return 2",
            // Merge blocked: repeated name.
            "local a=1 local a=2 return a",
            // Merge blocked: an initializer mentions an earlier group name.
            "local a=1 local b=a return b",
            // Merge blocked: first member is not exactly one value per name.
            "local a,b=1 local c=2 return c",
            // Merge blocked: empty member after a call initializer.
            "local a=f() local b return a",
            // Runs do not cross other statements.
            "local a=1 a=3 local b=2 return b",
        ] {
            assert_measure_matches_print(source);
        }
    }

    /// The block-level transforms are the known divergence class; this
    /// sweep pins measure == printed length across the statement and
    /// expression constructs the size model covers, so a future printer
    /// or size change that reopens a divergence fails here.
    #[test]
    fn measure_matches_print_across_constructs() {
        for source in [
            "local t={1,2,x=3,[\"k\"]=4} return t.x",
            "local s=\"a\"..\"b\" return s",
            "local s=\"a\\nb\" return s",
            "for i=1,10,2 do print(i) end",
            "for k,v in pairs({}) do print(k,v) end",
            "while true do break end",
            "repeat local x=1 until x>0",
            "if a then return 1 elseif b then return 2 else return 3 end",
            "local f=function(a,...) return a end return f(1,2)",
            "goto skip ::skip:: return",
            "local a,b,c=1,2 return a",
            "x={f(1),g(2)} return x",
            "a.b.c(1):d(2) return a",
            "return 1,2,3",
            "local a=-1 local b=2^-3 return a+b",
            "local x=1 (print)(x)",
            "local a=1 local b=2 return function() return a+b end",
        ] {
            assert_measure_matches_print(source);
        }
    }
}
