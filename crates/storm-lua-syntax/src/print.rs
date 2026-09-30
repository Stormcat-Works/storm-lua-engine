//! Compact and readable Lua source emission.

// Canonical compact/pretty printer。最終TS版の出力は frozen migration regression として保持。
// 2 系統（compact / pretty）は zeroCostNewlines フラグで切り替える。

use std::borrow::Cow;

use crate::ast::{Ast, Node, NodeId, TableField};
use crate::lexer::KEYWORDS;
use crate::numeric::{
    decode_lua_string, integer_literal_value, normalize_num_literal, qstr, qstr_bracket_key,
};

/// 演算子の結合優先度 [lhs_assoc_prec, rhs_assoc_prec]（lexer.ts の BIN_PREC 相当）。
pub(crate) fn bin_prec(op: &str) -> (u8, u8) {
    match op {
        "or" => (1, 2),
        "and" => (2, 3),
        "<" | ">" | "<=" | ">=" | "~=" | "==" => (3, 4),
        "|" => (4, 5),
        "~" => (5, 6),
        "&" => (6, 7),
        "<<" | ">>" => (7, 8),
        ".." => (9, 8),
        "+" | "-" => (10, 11),
        "*" | "/" | "//" | "%" => (11, 12),
        "^" => (14, 13),
        _ => (0, 0),
    }
}

/// Return the shortest prefix of an assignment/local initializer list that
/// preserves Lua's value-adjustment semantics when the removed suffix consists
/// only of literal `nil` expressions. Missing RHS values are initialized to nil,
/// but making a call/vararg the final expression can expose additional results.
/// Keep one trailing nil in that case when there are still unfilled targets.
pub(crate) fn trimmed_nil_rhs_len(
    ast: &Ast,
    target_count: usize,
    values: &[NodeId],
    allow_empty: bool,
) -> usize {
    let mut len = values.len();
    while len > 0 && matches!(ast.node(values[len - 1]), Node::Nil) {
        len -= 1;
    }
    if len == values.len() {
        return len;
    }
    if len == 0 {
        return if allow_empty { 0 } else { 1 };
    }
    if target_count > len && matches!(ast.node(values[len - 1]), Node::Call(..) | Node::Vararg) {
        len += 1;
    }
    len
}

fn needs_sep(a: &str, b: &str) -> bool {
    let (Some(x), Some(y)) = (a.chars().next_back(), b.chars().next()) else {
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
    // A number followed by `..` must not be printed as `1..`: the lexer can
    // consume the first dot as part of the numeric token.  Keep the operator
    // boundary explicit while retaining the compact form everywhere else.
    if x.is_ascii_digit() && y == '.' {
        return true;
    }
    false
}

// Operators use a space, not a zero-cost separator newline. Compose the actual
// bytes and relative emission ranges together so spacing cannot shift origins.
fn join_lex(parts: Vec<Fragment>) -> Fragment {
    let mut out = Fragment::default();
    for part in parts {
        if part.text.is_empty() {
            continue;
        }
        if needs_sep(&out.text, &part.text) {
            out.text.to_mut().push(' ');
        }
        out.append(part);
    }
    out
}

/// Render a numeric literal without losing its integer/float interpretation.
pub fn render_num(raw: &str) -> Cow<'_, str> {
    // Canonical positive decimal integers up to 15 digits already have the
    // shortest decimal integer spelling. An exponent would change Lua's subtype.
    // Avoid f64 parsing/formatting for the overwhelmingly common draw coordinates.
    let digits = raw.as_bytes();
    if !digits.is_empty()
        && digits.len() <= 15
        && (digits.len() == 1 || digits[0] != b'0')
        && digits.iter().all(u8::is_ascii_digit)
    {
        return Cow::Borrowed(raw);
    }
    // The public historical canonicalizer operates on f64; Lua 5.3 printing
    // must additionally retain the integer/float subtype. This matters beyond
    // math.type: concatenation, overflow and integer division can observe it.
    if let Some(value) = integer_literal_value(raw) {
        if value.unsigned_abs() > (1u64 << 53) {
            return Cow::Borrowed(raw);
        }
        let normalized = normalize_num_literal(raw);
        if integer_literal_value(&normalized) == Some(value) {
            return Cow::Owned(normalized);
        }
        // An exponent spelling, e.g. 1e5, is floating point in Lua. Do not use
        // it to shorten an integer token such as 100000.
        let decimal = value.to_string();
        return if decimal.len() < raw.len() {
            Cow::Owned(decimal)
        } else {
            Cow::Borrowed(raw)
        };
    }
    let mut normalized = normalize_num_literal(raw);
    if integer_literal_value(&normalized).is_some() {
        normalized.push_str(".0");
    }
    if normalized.len() > raw.len() {
        Cow::Borrowed(raw)
    } else {
        Cow::Owned(normalized)
    }
}

/// One emitted range in the final printed code, identified by its owning AST node.
/// Ranges may nest. An identifier slot is more specific than its containing node.
/// These are generated UTF-8 byte offsets, not original-source locations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeEmission {
    /// ID in the exact AST supplied to this printer.
    pub node: NodeId,
    /// Explicit identifier occurrence, or the entire node when absent.
    pub site: Option<crate::parser::NameSite>,
    /// Inclusive generated byte position.
    pub start: usize,
    /// Exclusive generated byte position.
    pub end: usize,
}

/// Printed Lua and its generated ranges. This is not yet an optimized source map:
/// the caller must supply source provenance for the same AST and node generation.
#[derive(Debug, Clone)]
pub struct PrintedSource {
    /// Exact final code, including chosen separators and normalized literals.
    pub code: String,
    /// Node/name ranges recorded while constructing this code; no text matching.
    pub emissions: Vec<NodeEmission>,
}

// A recursive print result with relative ranges. The ordinary printer keeps the
// range vector empty (no mapping allocation). Composition shifts ranges when it
// appends actual bytes; formatting cannot accidentally discard the side table.
#[derive(Default)]
struct Fragment {
    text: Cow<'static, str>,
    emissions: Vec<NodeEmission>,
}
impl From<String> for Fragment {
    fn from(text: String) -> Self {
        Self {
            text: Cow::Owned(text),
            emissions: Vec::new(),
        }
    }
}
impl From<&str> for Fragment {
    fn from(text: &str) -> Self {
        text.to_string().into()
    }
}
impl Fragment {
    fn literal(text: &'static str) -> Self {
        Self {
            text: Cow::Borrowed(text),
            emissions: Vec::new(),
        }
    }
    fn append(&mut self, other: Self) {
        if other.text.is_empty() {
            return;
        }
        if self.text.is_empty()
            && self.emissions.is_empty()
            && matches!(&self.text, Cow::Borrowed(_))
        {
            *self = other;
            return;
        }
        let offset = self.text.len();
        self.text.to_mut().push_str(&other.text);
        self.emissions
            .extend(other.emissions.into_iter().map(|mut emission| {
                emission.start += offset;
                emission.end += offset;
                emission
            }));
    }
    fn join(parts: impl IntoIterator<Item = Self>, separator: &str) -> Self {
        let mut output = Self::default();
        for (i, part) in parts.into_iter().enumerate() {
            if i > 0 && !separator.is_empty() {
                output.text.to_mut().push_str(separator);
            }
            output.append(part);
        }
        output
    }
}
macro_rules! fragments {
    ($($piece:expr),+ $(,)?) => {
        Fragment::join([$(Fragment::from($piece)),+], "")
    };
}

/// Emitter for an arena whose node IDs and child kinds satisfy the syntax contract.
pub struct Printer<'a> {
    ast: &'a Ast,
    zero: bool,
    newline_count: u32,
    capture_emissions: bool,
}

impl<'a> Printer<'a> {
    /// Create a printer for a structurally valid AST; choose whether separator newlines are uncharged.
    pub fn new(ast: &'a Ast, zero_cost_newlines: bool) -> Self {
        Printer {
            ast,
            zero: zero_cost_newlines,
            newline_count: 0,
            capture_emissions: false,
        }
    }
    /// Return the number of separator newlines emitted so far.
    pub fn newline_count(&self) -> u32 {
        self.newline_count
    }

    fn record(
        &self,
        mut fragment: Fragment,
        node: NodeId,
        site: Option<crate::parser::NameSite>,
    ) -> Fragment {
        if self.capture_emissions && !fragment.text.is_empty() {
            fragment.emissions.push(NodeEmission {
                node,
                site,
                start: 0,
                end: fragment.text.len(),
            });
        }
        fragment
    }
    fn name(
        &self,
        symbol: crate::ast::SymbolId,
        node: NodeId,
        site: crate::parser::NameSite,
    ) -> Fragment {
        self.record(self.ast.strings.get(symbol).into(), node, Some(site))
    }
    fn member(&self, text: &str, node: NodeId) -> Fragment {
        self.record(text.into(), node, Some(crate::parser::NameSite::Member))
    }
    fn bracket_key_expr(&mut self, id: NodeId) -> Fragment {
        if let Node::Str(raw) = self.ast.node(id) {
            let key = qstr_bracket_key(raw);
            let space = key.starts_with('[');
            let key = self.record(key.into(), id, None);
            return if space {
                fragments!(Fragment::literal(" "), key)
            } else {
                key
            };
        }
        self.expr(id, 0)
    }
    fn cat(&mut self, mut a: Fragment, b: Fragment) -> Fragment {
        if b.text.is_empty() {
            return a;
        }
        if needs_sep(&a.text, &b.text) {
            if self.zero {
                self.newline_count += 1;
                a.text.to_mut().push('\n');
            } else {
                a.text.to_mut().push(' ');
            }
        }
        a.append(b);
        a
    }
    fn expr(&mut self, id: NodeId, parent: u8) -> Fragment {
        let result = self.expr_inner(id, parent);
        self.record(result, id, None)
    }
    #[expect(
        clippy::panic,
        reason = "Invalid low-level AST expression shapes fail loudly; source is parsed before printing"
    )]
    fn expr_inner(&mut self, id: NodeId, parent: u8) -> Fragment {
        use crate::parser::NameSite;
        let node = &self.ast.nodes[id as usize];
        let s: Fragment;
        let prec: u8;
        match node {
            Node::Name(sym) => return self.name(*sym, id, NameSite::Reference),
            Node::Num(v) => return render_num(v).into_owned().into(),
            Node::Str(v) => return qstr(v).into(),
            Node::Nil => return Fragment::literal("nil"),
            Node::Bool(b) => {
                if parent <= 3 {
                    return if *b { "1>0" } else { "1>2" }.into();
                }
                return if *b { "true" } else { "false" }.into();
            }
            Node::Vararg => return Fragment::literal("..."),
            Node::Paren(e) => {
                return fragments!(
                    Fragment::literal("("),
                    self.expr(*e, 0),
                    Fragment::literal(")")
                )
            }
            Node::Table(fs) => {
                let parts: Vec<_> = fs
                    .iter()
                    .enumerate()
                    .map(|(index, field)| match field {
                        TableField::Arr(v) => self.expr(*v, 0),
                        TableField::Name(k, v) => fragments!(
                            self.name(*k, id, NameSite::Field(index as u32)),
                            Fragment::literal("="),
                            self.expr(*v, 0)
                        ),
                        TableField::KVar(k, v) => {
                            if let Node::Str(raw) = &self.ast.nodes[*k as usize] {
                                if let Some(key) = plain_name_from_string_literal(raw) {
                                    let key = self.record(Fragment::from(key.as_ref()), *k, None);
                                    return fragments!(
                                        key,
                                        Fragment::literal("="),
                                        self.expr(*v, 0)
                                    );
                                }
                            }
                            fragments!(
                                Fragment::literal("["),
                                self.bracket_key_expr(*k),
                                Fragment::literal("]="),
                                self.expr(*v, 0)
                            )
                        }
                    })
                    .collect();
                return fragments!(
                    Fragment::literal("{"),
                    Fragment::join(parts, ","),
                    Fragment::literal("}")
                );
            }
            Node::Index(obj, key, _) => {
                let obj_s = self.expr(*obj, 15);
                if let Node::Str(kv) = &self.ast.nodes[*key as usize] {
                    if let Some(k) = plain_name_from_string_literal(kv) {
                        let member = self.record(self.member(k.as_ref(), *key), *key, None);
                        return fragments!(obj_s, Fragment::literal("."), member);
                    }
                }
                return fragments!(
                    obj_s,
                    Fragment::literal("["),
                    self.bracket_key_expr(*key),
                    Fragment::literal("]")
                );
            }
            Node::Function(..) => return self.function(id, true),
            Node::Un(op, e) => {
                prec = 12;
                s = join_lex(vec![op.as_str().into(), self.expr(*e, 12)]);
            }
            Node::Bin(op, l, r) => {
                let (lp, rp) = bin_prec(op);
                prec = lp;
                s = join_lex(vec![
                    self.expr(*l, lp),
                    op.as_str().into(),
                    self.expr(*r, rp),
                ]);
            }
            Node::Call(fn_, args, method) => {
                let fn_s = self.expr(*fn_, 15);
                let args_s = Fragment::join(
                    args.iter().map(|a| self.expr(*a, 0)).collect::<Vec<_>>(),
                    ",",
                );
                let sugar = args.len() == 1
                    && matches!(self.ast.node(args[0]), Node::Str(_) | Node::Table(_));
                return match (method, sugar) {
                    (Some(m), true) => {
                        fragments!(fn_s, Fragment::literal(":"), self.member(m, id), args_s)
                    }
                    (None, true) => fragments!(fn_s, args_s),
                    (Some(m), false) => fragments!(
                        fn_s,
                        Fragment::literal(":"),
                        self.member(m, id),
                        Fragment::literal("("),
                        args_s,
                        Fragment::literal(")")
                    ),
                    (None, false) => {
                        fragments!(fn_s, Fragment::literal("("), args_s, Fragment::literal(")"))
                    }
                };
            }
            _ => panic!("unknown expression node"),
        }
        if prec < parent {
            fragments!(Fragment::literal("("), s, Fragment::literal(")"))
        } else {
            s
        }
    }
    fn function(&mut self, id: NodeId, keyword: bool) -> Fragment {
        use crate::parser::NameSite;
        if let Node::Function(ps, variadic, b) = &self.ast.nodes[id as usize] {
            let params = ps
                .iter()
                .enumerate()
                .map(|(i, p)| self.name(*p, id, NameSite::Parameter(i as u32)))
                .collect::<Vec<_>>();
            let header = fragments!(
                Fragment::literal(if keyword { "function(" } else { "(" }),
                Fragment::join(params, ","),
                var_suffix(ps.len(), *variadic),
                Fragment::literal(")")
            );
            let body = self.block(*b);
            let end = self.cat(body, Fragment::literal("end"));
            return self.cat(header, end);
        }
        unreachable!()
    }
    fn function_tail(&mut self, id: NodeId) -> Fragment {
        let result = self.function(id, false);
        self.record(result, id, None)
    }
    fn target(&mut self, id: NodeId) -> Fragment {
        if let Node::Methodname(obj, name) = &self.ast.nodes[id as usize] {
            let result = fragments!(
                self.expr(*obj, 0),
                Fragment::literal(":"),
                self.name(*name, id, crate::parser::NameSite::Member)
            );
            return self.record(result, id, None);
        }
        self.expr(id, 0)
    }
    fn stat(&mut self, id: NodeId) -> Fragment {
        let result = self.stat_inner(id);
        self.record(result, id, None)
    }
    #[expect(
        clippy::panic,
        reason = "A statement block only contains statement nodes; invalid low-level ASTs fail loudly"
    )]
    fn stat_inner(&mut self, id: NodeId) -> Fragment {
        use crate::parser::NameSite;
        match &self.ast.nodes[id as usize] {
            Node::Break => Fragment::literal("break"),
            Node::Goto(name) => fragments!(
                Fragment::literal("goto "),
                self.name(*name, id, NameSite::Label)
            ),
            Node::Label(name) => fragments!(
                Fragment::literal("::"),
                self.name(*name, id, NameSite::Label),
                Fragment::literal("::")
            ),
            Node::Do(b) => {
                let body = self.block(*b);
                let a = self.cat(Fragment::literal("do"), body);
                self.cat(a, Fragment::literal("end"))
            }
            Node::While(e, b) => {
                let cond = self.expr(*e, 0);
                let body = self.block(*b);
                let head = self.cat(Fragment::literal("while"), cond);
                let head = self.cat(head, Fragment::literal("do"));
                let a = self.cat(head, body);
                self.cat(a, Fragment::literal("end"))
            }
            Node::Repeat(b, e) => {
                let body = self.block(*b);
                let until = self.expr(*e, 0);
                let a = self.cat(Fragment::literal("repeat"), body);
                let tail = self.cat(Fragment::literal("until"), until);
                self.cat(a, tail)
            }
            Node::If(arms, eb) => {
                let e0 = self.expr(arms[0].cond, 0);
                let b0 = self.block(arms[0].body);
                let head = self.cat(Fragment::literal("if"), e0);
                let head = self.cat(head, Fragment::literal("then"));
                let mut out = self.cat(head, b0);
                let mut rest = Vec::new();
                for arm in &arms[1..] {
                    rest.push((self.expr(arm.cond, 0), self.block(arm.body)));
                }
                for (e, b) in rest {
                    out = self.cat(out, Fragment::literal("elseif"));
                    out = self.cat(out, e);
                    out = self.cat(out, Fragment::literal("then"));
                    out = self.cat(out, b);
                }
                if let Some(else_b) = eb {
                    let body = self.block(*else_b);
                    out = self.cat(out, Fragment::literal("else"));
                    out = self.cat(out, body);
                }
                self.cat(out, Fragment::literal("end"))
            }
            Node::Fornum(name, a, b, c, body) => {
                let mut head = fragments!(
                    Fragment::literal("for "),
                    self.name(*name, id, NameSite::Binding(0)),
                    Fragment::literal("="),
                    self.expr(*a, 0),
                    Fragment::literal(","),
                    self.expr(*b, 0)
                );
                if let Some(step) = c {
                    head.append(fragments!(Fragment::literal(","), self.expr(*step, 0)));
                }
                let head = self.cat(head, Fragment::literal("do"));
                let body = self.block(*body);
                let out = self.cat(head, body);
                self.cat(out, Fragment::literal("end"))
            }
            Node::Forin(names, es, body) => {
                let names = Fragment::join(
                    names
                        .iter()
                        .enumerate()
                        .map(|(i, n)| self.name(*n, id, NameSite::Binding(i as u32)))
                        .collect::<Vec<_>>(),
                    ",",
                );
                let values =
                    Fragment::join(es.iter().map(|e| self.expr(*e, 0)).collect::<Vec<_>>(), ",");
                let head = fragments!(Fragment::literal("for "), names, Fragment::literal(" in"));
                let head = self.cat(head, values);
                let head = self.cat(head, Fragment::literal("do"));
                let body = self.block(*body);
                let out = self.cat(head, body);
                self.cat(out, Fragment::literal("end"))
            }
            Node::Funcstat(target, function) => fragments!(
                Fragment::literal("function "),
                self.target(*target),
                self.function_tail(*function)
            ),
            Node::Localfunc(name, function) => fragments!(
                Fragment::literal("local function "),
                self.name(*name, id, NameSite::Binding(0)),
                self.function_tail(*function)
            ),
            Node::Local(names, es) => {
                let ns = Fragment::join(
                    names
                        .iter()
                        .enumerate()
                        .map(|(i, n)| self.name(*n, id, NameSite::Binding(i as u32)))
                        .collect::<Vec<_>>(),
                    ",",
                );
                let keep = trimmed_nil_rhs_len(self.ast, names.len(), es, true);
                if keep == 0 {
                    fragments!(Fragment::literal("local "), ns)
                } else {
                    let values = Fragment::join(
                        es[..keep]
                            .iter()
                            .map(|e| self.expr(*e, 0))
                            .collect::<Vec<_>>(),
                        ",",
                    );
                    fragments!(
                        Fragment::literal("local "),
                        ns,
                        Fragment::literal("="),
                        values
                    )
                }
            }
            Node::Return(es) => {
                if es.is_empty() {
                    Fragment::literal("return")
                } else {
                    let values = Fragment::join(
                        es.iter().map(|e| self.expr(*e, 0)).collect::<Vec<_>>(),
                        ",",
                    );
                    self.cat(Fragment::literal("return"), values)
                }
            }
            Node::Callstat(e) => self.expr(*e, 0),
            Node::Assign(vs, es) => {
                let targets =
                    Fragment::join(vs.iter().map(|e| self.expr(*e, 0)).collect::<Vec<_>>(), ",");
                let keep = trimmed_nil_rhs_len(self.ast, vs.len(), es, false);
                let values = Fragment::join(
                    es[..keep]
                        .iter()
                        .map(|e| self.expr(*e, 0))
                        .collect::<Vec<_>>(),
                    ",",
                );
                fragments!(targets, Fragment::literal("="), values)
            }
            _ => panic!("unknown statement node"),
        }
    }
    fn block(&mut self, id: NodeId) -> Fragment {
        if let Node::Block(statements) = &self.ast.nodes[id as usize] {
            let parts = statements
                .iter()
                .map(|statement| self.stat(*statement))
                .collect::<Vec<_>>();
            let mut out = Fragment {
                text: Cow::Owned(String::with_capacity(
                    parts.iter().map(|p| p.text.len()).sum::<usize>() + parts.len(),
                )),
                emissions: Vec::new(),
            };
            for (i, part) in parts.into_iter().enumerate() {
                if i > 0 {
                    if part.text.starts_with('(') {
                        out.text.to_mut().push(';');
                    } else if needs_sep(&out.text, &part.text) {
                        if self.zero {
                            self.newline_count += 1;
                            out.text.to_mut().push('\n');
                        } else {
                            out.text.to_mut().push(' ');
                        }
                    }
                }
                out.append(part);
            }
            return self.record(out, id, None);
        }
        unreachable!()
    }
    /// Emit the root block without collecting positions.
    pub fn output(&mut self, root: NodeId) -> String {
        self.block(root).text.into_owned()
    }
    /// Emit the same code while recording exact generated node and identifier ranges.
    /// Original provenance must belong to this exact AST; parsed NodePositions alone
    /// cannot describe a transformed arena without explicit origin propagation.
    pub fn output_with_positions(&mut self, root: NodeId) -> PrintedSource {
        self.capture_emissions = true;
        let result = self.block(root);
        self.capture_emissions = false;
        PrintedSource {
            code: result.text.into_owned(),
            emissions: result.emissions,
        }
    }
    /// Print one statement, without recording positions.
    pub fn stat_public(&mut self, id: NodeId) -> String {
        self.stat(id).text.into_owned()
    }
    /// Print one expression, without recording positions.
    pub fn expr_public(&mut self, id: NodeId) -> String {
        self.expr(id, 0).text.into_owned()
    }
}

/// (ローカル変数接尾辞: vararg が真のとき ",..." か "..." を追記)
fn var_suffix(params: usize, variadic: bool) -> &'static str {
    if !variadic {
        ""
    } else if params > 0 {
        ",..."
    } else {
        "..."
    }
}

fn is_plain_name(s: &str) -> bool {
    let mut it = s.chars();
    match it.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    it.all(|c| c.is_ascii_alphanumeric() || c == '_') && !KEYWORDS.contains(&s)
}

pub(crate) fn plain_name_from_string_literal(raw: &str) -> Option<Cow<'_, str>> {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[0], b'\'' | b'"') && bytes[bytes.len() - 1] == bytes[0] {
        let inner = &raw[1..raw.len() - 1];
        if !inner.as_bytes().contains(&b'\\') {
            return is_plain_name(inner).then_some(Cow::Borrowed(inner));
        }
    }
    let decoded = decode_lua_string(raw);
    is_plain_name(&decoded).then_some(Cow::Owned(decoded))
}

/// tokenMinify 相当（Lexer 列 → 最短トークン文字列）。
/// TS の tokenMinify は lexer エラー時に throw する。Rust は握りつぶさず
/// `Err(LexError)` で伝播させる（AGENTS §7: エラーの握りつぶし禁止）。
pub fn token_minify(source: &str) -> Result<String, crate::lexer::LexError> {
    use crate::lexer::{Lexer, TokenKind};
    let mut out = String::with_capacity(source.len());
    let mut lexer = Lexer::new(source);
    let ts = lexer.all()?;
    for t in ts {
        if t.k == TokenKind::Eof {
            continue;
        }
        let value = match t.k {
            TokenKind::Str => Cow::Owned(qstr(&t.v)),
            TokenKind::Num => render_num(&t.v),
            _ => Cow::Borrowed(t.v.as_str()),
        };
        if needs_sep(&out, &value) {
            out.push(' ');
        }
        out.push_str(&value);
    }
    Ok(out)
}

/// Remove comments and redundant separators without rewriting names, literals, or line endings.
/// Preserving token line positions also preserves Lua error messages observed through pcall.
/// Reflection and host overrides use this path instead of whole-program transformations.
pub fn lexical_minify(source: &str) -> Result<String, crate::lexer::LexError> {
    use crate::lexer::{Lexer, TokenKind};
    let tokens = Lexer::new(source).all()?;
    let compact_with = |spaced: bool| {
        let mut output = String::with_capacity(source.len());
        let mut end = 0;
        for token in tokens.iter().filter(|t| t.k != TokenKind::Eof) {
            let gap = &source[end..token.p];
            let before = output.len();
            output.extend(gap.chars().filter(|c| matches!(c, '\n' | '\r')));
            if before == output.len()
                && !output.is_empty()
                && (spaced || needs_sep(&output, &token.v))
            {
                output.push(' ');
            }
            output.push_str(&token.v);
            end = token.p + token.v.len();
        }
        output.extend(source[end..].chars().filter(|c| matches!(c, '\n' | '\r')));
        output
    };
    let mut compact = compact_with(false);
    let expected: Vec<_> = tokens
        .iter()
        .filter(|t| t.k != TokenKind::Eof)
        .map(|t| (&t.k, t.v.as_str(), t.line))
        .collect();
    let matches = Lexer::new(&compact).all().is_ok_and(|actual| {
        actual
            .iter()
            .filter(|t| t.k != TokenKind::Eof)
            .map(|t| (&t.k, t.v.as_str(), t.line))
            .eq(expected.iter().copied())
    });
    // Multi-character delimiters can require an explicit boundary. This branch
    // uses the same exact tokens and original line breaks, not a second parser.
    if !matches {
        compact = compact_with(true);
    }
    if compact.encode_utf16().count() > source.encode_utf16().count() {
        return Ok(source.to_owned());
    }
    Ok(compact)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod emission_tests {
    use super::*;
    use crate::parser::{parse_source_with_positions, NameSite};

    fn fixtures() -> [&'static str; 5] {
        [
            "local first,second=1,nil;local function f(x,...) if x then return ... elseif second then return false else return first end end output.setNumber(1,f(true))",
            "local t={7,field=1,['plain']=2,[ [=[long]=] ]=3}; function obj.part:method(x,...)for i=1,8,2 do t[i]=x end for key,value in pairs(t)do value=key end end",
            "do local x=0 repeat x=x+1 until x>3 while x<8 do x=x+1 end end ::again:: goto again",
            "local banner='雪😀';local f=function(a)return a end\r\noutput.setNumber(1,(f)(0x10))\r\nscreen.drawText(1,1,'雪😀')",
            "local x=nil local y,z=f(),nil;return -(-1),1 .. 'a',true,false,f{a=1},obj:m'hello'",
        ]
    }

    #[test]
    fn recording_positions_keeps_exact_bytes_and_newline_count() {
        for source in fixtures() {
            let (ast, root, positions) = parse_source_with_positions(source).unwrap();
            for zero in [false, true] {
                let mut plain = Printer::new(&ast, zero);
                let expected = plain.output(root);
                let mut mapped = Printer::new(&ast, zero);
                let actual = mapped.output_with_positions(root);
                assert_eq!(actual.code, expected, "source={source}");
                assert_eq!(mapped.newline_count(), plain.newline_count());
                assert!(!actual.emissions.is_empty());
                for range in actual.emissions {
                    assert!(range.start < range.end && range.end <= actual.code.len());
                    assert!(
                        actual.code.is_char_boundary(range.start)
                            && actual.code.is_char_boundary(range.end)
                    );
                    assert!(positions.span(range.node).is_some());
                }
                assert!(
                    !mapped.capture_emissions,
                    "capture must not leak into a later regular print"
                );
            }
        }
    }

    #[test]
    fn every_explicit_name_slot_maps_to_its_own_original_identifier() {
        for source in fixtures() {
            let (ast, root, positions) = parse_source_with_positions(source).unwrap();
            let printed = Printer::new(&ast, true).output_with_positions(root);
            let mut names = 0;
            for emission in &printed.emissions {
                let Some(site) = emission.site else { continue };
                let Some((start, end)) = positions.name_span(emission.node, site) else {
                    // A quoted key may be printed using dot syntax. It keeps the
                    // literal node's origin, but has no original identifier slot.
                    assert_eq!(site, NameSite::Member);
                    assert!(matches!(ast.node(emission.node), Node::Str(_)));
                    continue;
                };
                assert_eq!(
                    &printed.code[emission.start..emission.end],
                    &source[start..end]
                );
                names += 1;
            }
            assert!(names > 0);
        }
    }

    #[test]
    fn normalized_literals_and_omitted_nil_have_honest_generated_ranges() {
        let source = "local unused=nil;return 0x10,true,false,'hello'";
        let (ast, root, positions) = parse_source_with_positions(source).unwrap();
        let output = Printer::new(&ast, false).output_with_positions(root);
        for (i, node) in ast.nodes.iter().enumerate() {
            let id = i as NodeId;
            let emissions = output
                .emissions
                .iter()
                .filter(|e| e.node == id && e.site.is_none())
                .collect::<Vec<_>>();
            match node {
                Node::Nil => assert!(
                    emissions.is_empty(),
                    "omitted RHS must not get a generated position"
                ),
                Node::Num(_) | Node::Bool(_) | Node::Str(_) => {
                    assert_eq!(emissions.len(), 1);
                    let printed = &output.code[emissions[0].start..emissions[0].end];
                    let (start, end) = positions.span(id).unwrap();
                    let expected = match &source[start..end] {
                        "0x10" => "16",
                        "true" => "1>0",
                        "false" => "1>2",
                        "'hello'" => "\"hello\"",
                        other => panic!("unexpected original literal {other}"),
                    };
                    assert_eq!(printed, expected);
                }
                _ => {}
            }
        }
    }

    #[test]
    fn same_spelling_at_different_positions_is_not_resolved_by_text_search() {
        let source = "local x=1 do local x=x+1 output.setNumber(1,x)end output.setNumber(2,x)";
        let (ast, root, positions) = parse_source_with_positions(source).unwrap();
        let output = Printer::new(&ast, true).output_with_positions(root);
        let mut pairs = Vec::new();
        for emission in output.emissions {
            let Some(site) = emission.site else { continue };
            if &output.code[emission.start..emission.end] == "x" {
                pairs.push((
                    emission.start,
                    positions.name_span(emission.node, site).unwrap().0,
                ));
            }
        }
        pairs.sort_unstable();
        assert_eq!(pairs.len(), 5);
        let original = source
            .match_indices('x')
            .map(|(start, _)| start)
            .collect::<Vec<_>>();
        assert_eq!(
            pairs.iter().map(|pair| pair.1).collect::<Vec<_>>(),
            original
        );
        assert!(pairs.windows(2).all(|p| p[0].0 < p[1].0));
    }

    #[test]
    fn untracked_fragments_allocate_no_emission_vectors() {
        for source in fixtures() {
            let (ast, root, _) = parse_source_with_positions(source).unwrap();
            let result = Printer::new(&ast, false).block(root);
            assert!(result.emissions.is_empty());
            assert_eq!(result.emissions.capacity(), 0);
        }
    }
}
