//! Lua parsing into the syntax arena, with optional source positions.

// Canonical Lua parser。Phase 0〜1で最終TS版と構造parityを確立済み。
// `function(...)`（可変長のみ・名前なし）は TS 版が閉じ括弧を消費しない既存挙動のまま再現する。

use std::fmt;

use crate::ast::{Ast, IfArm, NodeId, TableField};
use crate::lexer::{LexError, Lexer, Token, TokenKind};

/// A syntactic failure after successful tokenization.
#[derive(Debug)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for ParseError {}

/// NodeId → (line, col) の位置サイドテーブル。AST 本体（`Ast`）は無位置のまま持たせない。
///
/// **有効範囲はパース直後の AST のみ**。最適化パスはノードを新規 push するため、
/// パス通過後は NodeId の対応が崩れる — 再マッピング・世代管理はここでは実装しない(YAGNI)。
/// リンク段・解析段（P1〜）の診断専用。
#[derive(Debug, Clone, Default)]
pub struct NodePositions {
    /// index = NodeId。
    positions: Vec<(u32, u32)>,
}

impl NodePositions {
    /// Return the one-based line and byte-column for a parsed node, or None for an unknown ID.
    pub fn get(&self, id: NodeId) -> Option<(u32, u32)> {
        self.positions.get(id as usize).copied()
    }
}

/// Parser state and the arena it constructs from a source chunk.
pub struct Parser {
    ts: Vec<Token>,
    i: usize,
    ast: Ast,
    capture_positions: bool,
    positions: Vec<(u32, u32)>,
}

const STOPS_END: &[&str] = &["end"];
const STOPS_UNTIL: &[&str] = &["until"];
const STOPS_IF: &[&str] = &["elseif", "else", "end"];

fn bin_prec(v: &str) -> Option<(u32, u32)> {
    Some(match v {
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
        _ => return None,
    })
}

fn is_unary(v: &str) -> bool {
    matches!(v, "not" | "#" | "-" | "~")
}

impl Parser {
    /// Tokenize the source and prepare an empty parser arena.
    pub fn new(source: &str) -> Result<Self, LexError> {
        let ts = Lexer::new(source).all()?;
        Ok(Self {
            ts,
            i: 0,
            ast: Ast::new(),
            capture_positions: false,
            positions: Vec::new(),
        })
    }

    /// 位置サイドテーブルを記録するモードで parser を構築する。既定の `new` は
    /// 記録コストを一切払わない（`parse_source` のホットパス — search.rs の
    /// 再パース検証等 — に影響を与えないため）。
    pub fn new_capturing_positions(source: &str) -> Result<Self, LexError> {
        let mut parser = Self::new(source)?;
        parser.capture_positions = true;
        Ok(parser)
    }

    /// Take ownership of the arena after parsing.
    pub fn into_ast(self) -> Ast {
        self.ast
    }

    /// Take the parsed arena and its optional position side table.
    pub fn into_ast_with_positions(self) -> (Ast, NodePositions) {
        (
            self.ast,
            NodePositions {
                positions: self.positions,
            },
        )
    }

    /// ノード構築ヘルパー。`capture_positions` が有効な場合のみ位置を記録する。
    /// `Ast::push` は Parser 経由でのみ呼ばれる（パス実行はここを通らない）ため、
    /// 記録漏れがなければ `self.positions` は常に `self.ast.nodes` と同じ長さで揃う。
    fn mark(&mut self, id: NodeId, line: u32, col: u32) {
        if !self.capture_positions {
            return;
        }
        debug_assert_eq!(
            self.positions.len() as u32,
            id,
            "position side table out of sync with NodeId"
        );
        self.positions.push((line, col));
    }

    fn cur(&self) -> &Token {
        &self.ts[self.i]
    }
    fn cur_v(&self) -> &str {
        &self.ts[self.i].v
    }
    fn cur_k(&self) -> TokenKind {
        self.ts[self.i].k
    }
    fn at(&self, v: Option<&str>, k: Option<TokenKind>) -> bool {
        let t = &self.ts[self.i];
        v.is_none_or(|vv| t.v == vv) && k.is_none_or(|kk| t.k == kk)
    }
    fn identifier(&mut self) -> Result<String, ParseError> {
        if self.cur_k() != TokenKind::Id {
            let token = self.cur();
            return Err(ParseError(format!(
                "expected identifier, got {} at {}:{}",
                token.v, token.line, token.col
            )));
        }
        Ok(self.pop(None)?.v)
    }
    fn pop(&mut self, v: Option<&str>) -> Result<Token, ParseError> {
        let t = self.ts[self.i].clone();
        if let Some(vv) = v {
            if t.v != vv {
                return Err(ParseError(format!(
                    "expected {}, got {} at {}:{}",
                    vv, t.v, t.line, t.col
                )));
            }
        }
        self.i += 1;
        Ok(t)
    }
    fn accept(&mut self, v: &str) -> bool {
        if self.ts[self.i].v == v {
            self.i += 1;
            return true;
        }
        false
    }

    fn block(&mut self, stops: &[&str]) -> Result<NodeId, ParseError> {
        let (line, col) = (self.cur().line, self.cur().col);
        let mut ss = Vec::new();
        loop {
            if self.cur_k() == TokenKind::Eof {
                break;
            }
            if stops.contains(&self.cur_v()) {
                break;
            }
            if self.accept(";") {
                continue;
            }
            ss.push(self.stat()?);
        }
        let id = self.ast.block(ss);
        self.mark(id, line, col);
        Ok(id)
    }

    fn stat(&mut self) -> Result<NodeId, ParseError> {
        let (line, col) = (self.cur().line, self.cur().col);
        if self.accept("break") {
            let id = self.ast.break_();
            self.mark(id, line, col);
            return Ok(id);
        }
        if self.accept("goto") {
            let name = self.identifier()?;
            let id = self.ast.goto(&name);
            self.mark(id, line, col);
            return Ok(id);
        }
        if self.accept("::") {
            let name = self.identifier()?;
            self.pop(Some("::"))?;
            let id = self.ast.label(&name);
            self.mark(id, line, col);
            return Ok(id);
        }
        if self.accept("do") {
            let b = self.block(STOPS_END)?;
            self.pop(Some("end"))?;
            let id = self.ast.do_(b);
            self.mark(id, line, col);
            return Ok(id);
        }
        if self.accept("while") {
            let e = self.expr()?;
            self.pop(Some("do"))?;
            let b = self.block(STOPS_END)?;
            self.pop(Some("end"))?;
            let id = self.ast.while_(e, b);
            self.mark(id, line, col);
            return Ok(id);
        }
        if self.accept("repeat") {
            let b = self.block(STOPS_UNTIL)?;
            self.pop(Some("until"))?;
            let e = self.expr()?;
            let id = self.ast.repeat(b, e);
            self.mark(id, line, col);
            return Ok(id);
        }
        if self.accept("if") {
            let mut arms = Vec::new();
            let mut e = self.expr()?;
            self.pop(Some("then"))?;
            arms.push(IfArm {
                cond: e,
                body: self.block(STOPS_IF)?,
            });
            while self.accept("elseif") {
                e = self.expr()?;
                self.pop(Some("then"))?;
                arms.push(IfArm {
                    cond: e,
                    body: self.block(STOPS_IF)?,
                });
            }
            let eb = if self.accept("else") {
                Some(self.block(STOPS_END)?)
            } else {
                None
            };
            self.pop(Some("end"))?;
            let id = self.ast.if_(arms, eb);
            self.mark(id, line, col);
            return Ok(id);
        }
        if self.accept("for") {
            let name = self.identifier()?;
            if self.accept("=") {
                let a = self.expr()?;
                self.pop(Some(","))?;
                let b = self.expr()?;
                let c = if self.accept(",") {
                    Some(self.expr()?)
                } else {
                    None
                };
                self.pop(Some("do"))?;
                let body = self.block(STOPS_END)?;
                self.pop(Some("end"))?;
                let id = self.ast.fornum(&name, a, b, c, body);
                self.mark(id, line, col);
                return Ok(id);
            }
            let mut names = vec![name];
            while self.accept(",") {
                names.push(self.identifier()?);
            }
            self.pop(Some("in"))?;
            let es = self.expr_list()?;
            self.pop(Some("do"))?;
            let body = self.block(STOPS_END)?;
            self.pop(Some("end"))?;
            let id = self.ast.forin(names, es, body);
            self.mark(id, line, col);
            return Ok(id);
        }
        if self.accept("function") {
            let target = self.func_name()?;
            let fn_ = self.func_body()?;
            let id = self.ast.funcstat(target, fn_);
            self.mark(id, line, col);
            return Ok(id);
        }
        if self.accept("local") {
            if self.accept("function") {
                let name = self.identifier()?;
                let fn_ = self.func_body()?;
                let id = self.ast.localfunc(&name, fn_);
                self.mark(id, line, col);
                return Ok(id);
            }
            let mut names = vec![self.identifier()?];
            while self.accept(",") {
                names.push(self.identifier()?);
            }
            let es = if self.accept("=") {
                self.expr_list()?
            } else {
                Vec::new()
            };
            let id = self.ast.local(names, es);
            self.mark(id, line, col);
            return Ok(id);
        }
        if self.accept("return") {
            let next_v = self.cur_v();
            let es = if ["end", "else", "elseif", "until", ";"].contains(&next_v)
                || self.cur_k() == TokenKind::Eof
            {
                Vec::new()
            } else {
                self.expr_list()?
            };
            self.accept(";");
            let id = self.ast.ret(es);
            self.mark(id, line, col);
            return Ok(id);
        }
        let first = self.prefix_exp()?;
        if self.ast.node(first).is_call() {
            let id = self.ast.callstat(first);
            self.mark(id, line, col);
            return Ok(id);
        }
        let mut vs = vec![first];
        while self.accept(",") {
            vs.push(self.prefix_exp()?);
        }
        self.pop(Some("="))?;
        let es = self.expr_list()?;
        let id = self.ast.assign(vs, es);
        self.mark(id, line, col);
        Ok(id)
    }

    fn func_name(&mut self) -> Result<NodeId, ParseError> {
        let (line, col) = (self.cur().line, self.cur().col);
        let name = self.identifier()?;
        let mut e = self.ast.name(&name);
        self.mark(e, line, col);
        while self.accept(".") {
            let (line, col) = (self.cur().line, self.cur().col);
            let name = self.identifier()?;
            let key = self.ast.str(quote_lua(&name));
            self.mark(key, line, col);
            e = self.ast.index(e, key, true);
            self.mark(e, line, col);
        }
        if self.accept(":") {
            let (line, col) = (self.cur().line, self.cur().col);
            let name = self.identifier()?;
            e = self.ast.methodname(e, &name);
            self.mark(e, line, col);
        }
        Ok(e)
    }

    fn func_body(&mut self) -> Result<NodeId, ParseError> {
        let (line, col) = (self.cur().line, self.cur().col);
        self.pop(Some("("))?;
        let mut ps: Vec<String> = Vec::new();
        let mut variadic = false;
        if !self.accept(")") {
            if self.accept("...") {
                variadic = true;
                // TS 版と同じく閉じ括弧を消費しない（function(...) はエラーになる既存挙動）
            } else {
                ps.push(self.identifier()?);
                loop {
                    if !self.accept(",") {
                        break;
                    }
                    if self.accept("...") {
                        variadic = true;
                        break;
                    }
                    ps.push(self.identifier()?);
                }
                self.pop(Some(")"))?;
            }
        }
        let b = self.block(STOPS_END)?;
        self.pop(Some("end"))?;
        let id = self.ast.function(ps, variadic, b);
        self.mark(id, line, col);
        Ok(id)
    }

    fn expr_list(&mut self) -> Result<Vec<NodeId>, ParseError> {
        let mut out = vec![self.expr()?];
        while self.accept(",") {
            out.push(self.expr()?);
        }
        Ok(out)
    }

    fn expr(&mut self) -> Result<NodeId, ParseError> {
        self.expr_min(0)
    }

    fn expr_min(&mut self, minp: u32) -> Result<NodeId, ParseError> {
        let tok = self.ts[self.i].clone();
        let mut left: NodeId;
        if is_unary(&tok.v) {
            self.pop(None)?;
            let e = self.expr_min(12)?;
            left = self.ast.un(&tok.v, e);
            self.mark(left, tok.line, tok.col);
        } else if self.accept("nil") {
            left = self.ast.nil();
            self.mark(left, tok.line, tok.col);
        } else if self.accept("true") {
            left = self.ast.bool_(true);
            self.mark(left, tok.line, tok.col);
        } else if self.accept("false") {
            left = self.ast.bool_(false);
            self.mark(left, tok.line, tok.col);
        } else if self.accept("...") {
            left = self.ast.vararg();
            self.mark(left, tok.line, tok.col);
        } else if tok.k == TokenKind::Num {
            self.pop(None)?;
            left = self.ast.num(tok.v);
            self.mark(left, tok.line, tok.col);
        } else if tok.k == TokenKind::Str {
            self.pop(None)?;
            left = self.ast.str(tok.v);
            self.mark(left, tok.line, tok.col);
        } else if self.accept("function") {
            left = self.func_body()?;
        } else if self.at(Some("{"), None) {
            left = self.table()?;
        } else {
            left = self.prefix_exp()?;
        }
        while let Some((lp, rp)) = bin_prec(self.cur_v()) {
            if lp < minp {
                break;
            }
            let op_tok = self.cur().clone();
            let op = op_tok.v.clone();
            self.pop(None)?;
            let r = self.expr_min(rp)?;
            left = self.ast.bin(&op, left, r);
            self.mark(left, op_tok.line, op_tok.col);
        }
        Ok(left)
    }

    fn table(&mut self) -> Result<NodeId, ParseError> {
        let (line, col) = (self.cur().line, self.cur().col);
        self.pop(Some("{"))?;
        let mut fs: Vec<TableField> = Vec::new();
        loop {
            if self.accept("}") {
                break;
            }
            if self.accept("[") {
                let k = self.expr()?;
                self.pop(Some("]"))?;
                self.pop(Some("="))?;
                let v = self.expr()?;
                fs.push(TableField::KVar(k, v));
            } else if self.cur_k() == TokenKind::Id
                && self.ts.get(self.i + 1).is_some_and(|t| t.v == "=")
            {
                let k = self.identifier()?;
                self.pop(Some("="))?;
                let v = self.expr()?;
                fs.push(TableField::Name(self.ast.strings.intern(&k), v));
            } else {
                let v = self.expr()?;
                fs.push(TableField::Arr(v));
            }
            if !(self.accept(",") || self.accept(";")) {
                self.pop(Some("}"))?;
                break;
            }
            if self.accept("}") {
                break;
            }
        }
        let id = self.ast.table(fs);
        self.mark(id, line, col);
        Ok(id)
    }

    fn prefix_exp(&mut self) -> Result<NodeId, ParseError> {
        let (line0, col0) = (self.cur().line, self.cur().col);
        let mut e: NodeId;
        if self.at(None, Some(TokenKind::Id)) {
            let name = self.identifier()?;
            e = self.ast.name(&name);
            self.mark(e, line0, col0);
        } else if self.accept("(") {
            let inner = self.expr()?;
            e = self.ast.paren(inner);
            self.mark(e, line0, col0);
            self.pop(Some(")"))?;
        } else {
            let t = self.cur().clone();
            return Err(ParseError(format!(
                "expected expression at {}:{}, got {}",
                t.line, t.col, t.v
            )));
        }
        loop {
            let (line, col) = (self.cur().line, self.cur().col);
            if self.accept("[") {
                let key = self.expr()?;
                self.pop(Some("]"))?;
                e = self.ast.index(e, key, false);
                self.mark(e, line, col);
            } else if self.accept(".") {
                let name = self.identifier()?;
                let key = self.ast.str(quote_lua(&name));
                self.mark(key, line, col);
                e = self.ast.index(e, key, true);
                self.mark(e, line, col);
            } else if self.accept(":") {
                let method = self.identifier()?;
                let args = self.args()?;
                e = self.ast.call(e, args, Some(method));
                self.mark(e, line, col);
            } else if self.at(Some("("), None)
                || self.at(Some("{"), None)
                || self.at(None, Some(TokenKind::Str))
            {
                let args = self.args()?;
                e = self.ast.call(e, args, None);
                self.mark(e, line, col);
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn args(&mut self) -> Result<Vec<NodeId>, ParseError> {
        if self.accept("(") {
            if self.accept(")") {
                return Ok(Vec::new());
            }
            let es = self.expr_list()?;
            self.pop(Some(")"))?;
            return Ok(es);
        }
        if self.at(Some("{"), None) {
            return Ok(vec![self.table()?]);
        }
        Ok(vec![self.expr()?])
    }

    /// Parse a complete chunk and return the root block ID.
    pub fn parse(&mut self) -> Result<NodeId, ParseError> {
        let root = self.block(&[])?;
        if self.cur_k() != TokenKind::Eof {
            return Err(ParseError("trailing input".to_string()));
        }
        Ok(root)
    }
}

/// quoteLua = JSON.stringify 相当。引数は Lua 識別子（ASCII）のみなので `"name"` 形式で一致する。
fn quote_lua(name: &str) -> String {
    format!("\"{}\"", name)
}

/// Parse one Lua source chunk without collecting diagnostic positions.
pub fn parse_source(source: &str) -> Result<(Ast, NodeId), ParserError> {
    let mut parser = Parser::new(source).map_err(ParserError::Lex)?;
    let root = parser.parse().map_err(ParserError::Parse)?;
    Ok((parser.into_ast(), root))
}

/// `parse_source` に加えて位置サイドテーブル（NodeId → line/col）を返す。
/// リンク段・解析段（P1〜）の診断専用。`compile()` のホットパスは
/// 記録コストなしの `parse_source` のまま変えない。
pub fn parse_source_with_positions(
    source: &str,
) -> Result<(Ast, NodeId, NodePositions), ParserError> {
    let mut parser = Parser::new_capturing_positions(source).map_err(ParserError::Lex)?;
    let root = parser.parse().map_err(ParserError::Parse)?;
    let (ast, positions) = parser.into_ast_with_positions();
    Ok((ast, root, positions))
}

/// Distinguishes lexical and syntactic source failures.
#[derive(Debug)]
pub enum ParserError {
    /// Tokenization failed.
    Lex(LexError),
    /// The token stream is not a valid Lua chunk.
    Parse(ParseError),
}

impl fmt::Display for ParserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParserError::Lex(e) => write!(f, "{}", e),
            ParserError::Parse(e) => write!(f, "{}", e),
        }
    }
}
impl std::error::Error for ParserError {}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod position_tests {
    use super::*;
    use crate::ast::Node;

    fn find<F: Fn(&Node) -> bool>(ast: &Ast, pred: F) -> Vec<NodeId> {
        (0..ast.nodes.len() as NodeId)
            .filter(|&id| pred(ast.node(id)))
            .collect()
    }

    /// 既定の `parse_source` は位置サイドテーブルを一切作らない
    /// (search.rs の再パース検証等ホットパスの挙動・性能を変えない)。
    #[test]
    fn parse_source_is_unaffected() {
        let source = "local a = 1\nlocal b = a + 2\n";
        let (ast, root) = parse_source(source).expect("parse ok");
        assert!(matches!(ast.node(root), Node::Block(_)));
    }

    /// 複数行にまたがるトップレベル文の位置が正確であること。
    #[test]
    fn top_level_statement_positions_span_multiple_lines() {
        let source = "local a = 1\nlocal b = 2\n";
        let (ast, _root, positions) = parse_source_with_positions(source).expect("parse ok");
        let locals = find(&ast, |n| matches!(n, Node::Local(..)));
        assert_eq!(locals.len(), 2);
        assert_eq!(positions.get(locals[0]), Some((1, 1)));
        assert_eq!(positions.get(locals[1]), Some((2, 1)));
    }

    /// 行頭(インデント)・行中の双方でノード位置が正確であること。
    #[test]
    fn statement_and_operator_positions_are_accurate_at_line_start_and_mid_line() {
        let source = "  if x > 10 then\n    y = 1\n  end\n";
        let (ast, _root, positions) = parse_source_with_positions(source).expect("parse ok");
        // if 文はインデント後の行頭 (line 1, col 3)
        let ifs = find(&ast, |n| matches!(n, Node::If(..)));
        assert_eq!(positions.get(ifs[0]), Some((1, 3)));
        // 比較演算子 `>` は行中 (line 1, col 8)
        let bins = find(&ast, |n| matches!(n, Node::Bin(op, _, _) if op == ">"));
        assert_eq!(positions.get(bins[0]), Some((1, 8)));
        // 代入文 `y = 1` はネストしたブロック内・インデント後の行頭 (line 2, col 5)
        let assigns = find(&ast, |n| matches!(n, Node::Assign(..)));
        assert_eq!(positions.get(assigns[0]), Some((2, 5)));
    }

    /// ネスト構造(関数定義の中の if)でも内側ノードの位置が正確であること。
    #[test]
    fn nested_function_and_if_positions_are_accurate() {
        let source = "function f()\n  if true then\n    return 1\n  end\nend\n";
        let (ast, _root, positions) = parse_source_with_positions(source).expect("parse ok");
        let funcstats = find(&ast, |n| matches!(n, Node::Funcstat(..)));
        assert_eq!(positions.get(funcstats[0]), Some((1, 1)));
        let ifs = find(&ast, |n| matches!(n, Node::If(..)));
        assert_eq!(positions.get(ifs[0]), Some((2, 3)));
        let rets = find(&ast, |n| matches!(n, Node::Return(..)));
        assert_eq!(positions.get(rets[0]), Some((3, 5)));
    }

    /// 未知の NodeId には位置がない(範囲外は None、パニックしない)。
    #[test]
    fn unknown_node_id_returns_none() {
        let (_ast, _root, positions) = parse_source_with_positions("x = 1\n").expect("parse ok");
        assert_eq!(positions.get(u32::MAX), None);
    }

    /// Source positions cover all nodes in a nested, synthetic program.
    #[test]
    fn side_table_covers_every_node_in_nested_program() {
        let source = "local x={a=1} function onTick() for i=1,8 do if i%2==0 then x.a=x.a+i end end output.setNumber(1,x.a) end";
        let (ast, _, positions) =
            parse_source_with_positions(source).expect("synthetic program parses");
        for id in 0..ast.nodes.len() as NodeId {
            assert!(
                positions.get(id).is_some(),
                "missing position for node {id}"
            );
        }
    }
}

#[cfg(test)]
mod identifier_tests {
    use super::parse_source;
    #[test]
    fn rejects_non_names_at_identifier_positions() {
        for source in [
            "local =",
            "local 1",
            "local function true()end",
            "function false()end",
            "function f(1)end",
            "for true=1,3 do end",
            "goto 3",
            "::true::",
            "a.3()",
            "a:3()",
        ] {
            assert!(
                parse_source(source).is_err(),
                "accepted invalid identifier: {source}"
            );
        }
    }
}
