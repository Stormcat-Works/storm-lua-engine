//! Lua tokenization with source positions.

// Canonical Lua lexer。最終TS版の token behavior は frozen migration regression で固定。
// 位置（line/col）は診断用のみで、AST ダンプ（parity 判定対象）には含めない。
// JS `\s` の非 ASCII 空白（U+FEFF/BOM 含む）は TS と同様にスキップする。
// サロゲート（非 BMP 文字をまたぐ位置整合）は厳密な parity 対象外（コーパスは ASCII）。

use std::fmt;

/// Lexical token categories; literal spellings are retained in Token.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenKind {
    /// End of source.
    Eof,
    /// Lua keyword.
    Kw,
    /// Identifier.
    Id,
    /// Operator or punctuation.
    Op,
    /// Numeric literal.
    Num,
    /// String literal.
    Str,
}

/// One token and its original UTF-8 source position.
#[derive(Clone, Debug)]
pub struct Token {
    /// Token category.
    pub k: TokenKind,
    /// Token spelling retained from the source.
    pub v: String,
    /// Zero-based UTF-8 byte offset in the source.
    pub p: usize,
    /// One-based source line.
    pub line: u32,
    /// One-based UTF-8 byte column in the source line.
    pub col: u32,
}

/// A token borrowing its unchanged spelling from the original source.
/// Intended for lexical tooling that does not need an owned token array.
#[derive(Clone, Copy, Debug)]
pub struct BorrowedToken<'a> {
    /// Lexical category, identical to the owned token API.
    pub k: TokenKind,
    /// Exact source spelling; literals are not decoded or normalized.
    pub v: &'a str,
    /// Zero-based UTF-8 source offset.
    pub p: usize,
    /// One-based source line.
    pub line: u32,
    /// One-based source byte-column with the lexer's existing whitespace rules.
    pub col: u32,
}
impl BorrowedToken<'_> {
    fn into_owned(self) -> Token {
        Token {
            k: self.k,
            v: self.v.to_owned(),
            p: self.p,
            line: self.line,
            col: self.col,
        }
    }
}

/// `--@storm` プレフィックスの行コメント1件（位置つき生テキスト）。
/// 本文（`ignore(...)` 等）の解析は P2 の仕事。ここでは捕捉のみ行う。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StormComment {
    /// One-based source line.
    pub line: u32,
    /// One-based UTF-8 byte column in the source line.
    pub col: u32,
    /// 先頭の `--` を含む生テキスト（行末まで、改行は含まない）。
    pub text: String,
}

/// A real comment token for source tooling; never a string-content match.
#[derive(Clone, Debug, serde::Serialize)]
pub struct SourceComment {
    /// Inclusive UTF-8 byte offset.
    pub start: usize,
    /// Exclusive UTF-8 byte offset.
    pub end: usize,
    /// One-based source line.
    pub line: u32,
    /// Full comment text, including delimiters.
    pub text: String,
    /// True for a Lua long-bracket comment.
    pub long: bool,
}

/// A lexical failure with a human-readable location and message.
#[derive(Debug)]
pub struct LexError(pub String);

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for LexError {}

// The public ordered vocabulary and membership code have one literal source.
// This lets the compiler dispatch by length/bytes instead of scanning every
// keyword for each identifier. Matching remains exact and case-sensitive.
macro_rules! lua_keywords {
    ($($word:literal),+ $(,)?)=>{
        /// Lua keywords that cannot be emitted as ordinary identifier names.
        pub const KEYWORDS:&[&str]=&[$($word),+];
        pub(crate) fn is_keyword(word:&str)->bool {matches!(word,$($word)|+)}
    };
}
lua_keywords! {

    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
}

/// TS の MULTI（`...` が先）と同じ順序。`starts_with` はこの順で試す。
const MULTI: &[&[u8]] = &[
    b"...", b"//", b"<<", b">>", b"==", b"~=", b"<=", b">=", b"::", b"..",
];

/// TS の単文字演算子集合 `'+-*/%^#&~|<>=(){}[];:,.'`。
fn is_single_op(b: u8) -> bool {
    matches!(
        b,
        b'+' | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'^'
            | b'#'
            | b'&'
            | b'~'
            | b'|'
            | b'<'
            | b'>'
            | b'='
            | b'('
            | b')'
            | b'{'
            | b'}'
            | b'['
            | b']'
            | b';'
            | b':'
            | b','
            | b'.'
    )
}

fn is_id_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}
fn is_id_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
fn is_digit(b: u8) -> bool {
    b.is_ascii_digit()
}
fn is_hex(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Stateful UTF-8 lexer. It borrows source and does not read files.
pub struct Lexer<'a> {
    source: &'a str,
    s: &'a [u8],
    i: usize,
    line: u32,
    col: u32,
    capture_storm_comments: bool,
    storm_comments: Vec<StormComment>,
    source_comments: Option<Vec<SourceComment>>,
}

impl<'a> Lexer<'a> {
    /// Create a lexer over UTF-8 source without capturing comments.
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            s: source.as_bytes(),
            i: 0,
            line: 1,
            col: 1,
            capture_storm_comments: false,
            storm_comments: Vec::new(),
            source_comments: None,
        }
    }

    /// Capture real comments for source editing. Optimization lexing remains unchanged.
    pub fn with_comment_capture(mut self) -> Self {
        self.source_comments = Some(Vec::new());
        self
    }
    /// Captured comments in lexical order.
    pub fn comments(&self) -> &[SourceComment] {
        self.source_comments.as_deref().unwrap_or(&[])
    }

    /// `--@storm` で始まる行コメントのみ位置つきで保持するモードを有効にする
    /// (解析段専用。長括弧コメントは対象外)。デフォルト(無効)は現行どおり全コメント破棄で、
    /// minify 経路の挙動・性能を変えない。
    pub fn with_storm_comment_capture(mut self) -> Self {
        self.capture_storm_comments = true;
        self
    }

    /// Borrow captured directive comments in source order.
    pub fn storm_comments(&self) -> &[StormComment] {
        &self.storm_comments
    }

    /// 直前に返したトークンの直後（後続の空白・コメントを消費する前）のバイトオフセット。
    /// リンク段（P1b）が require 文のソーステキスト区間を厳密に切り出すために使う。
    pub fn pos(&self) -> usize {
        self.i
    }

    fn adv(&mut self, n: usize) {
        for _ in 0..n {
            if self.i >= self.s.len() {
                return;
            }
            if self.s[self.i] == b'\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
            self.i += 1;
        }
    }

    fn long_bracket(&self, pos: usize) -> Option<usize> {
        if pos >= self.s.len() || self.s[pos] != b'[' {
            return None;
        }
        let mut j = pos + 1;
        while j < self.s.len() && self.s[j] == b'=' {
            j += 1;
        }
        if j < self.s.len() && self.s[j] == b'[' {
            Some(j - pos - 1)
        } else {
            None
        }
    }

    fn skip(&mut self) -> Result<(), LexError> {
        while self.i < self.s.len() {
            let b = self.s[self.i];
            if b < 0x80 {
                if is_js_ascii_whitespace(b) {
                    self.adv(1);
                    continue;
                }
            } else if let Some(ch) = decode_char(self.s, self.i) {
                // JS `\s` は非 ASCII 空白（U+00A0/U+FEFF/U+3000 等）も含む。UTF-8 BOM もここで消費する。
                if is_js_unicode_whitespace(ch) {
                    self.i += ch.len_utf8();
                    self.col += 1;
                    continue;
                }
            }
            if self.s[self.i..].starts_with(b"--") {
                let level = self.long_bracket(self.i + 2);
                if let Some(lv) = level {
                    let (start, line) = (self.i, self.line);
                    self.adv(2);
                    self.read_long(lv)?;
                    if let Some(comments) = &mut self.source_comments {
                        comments.push(SourceComment {
                            start,
                            end: self.i,
                            line,
                            text: String::from_utf8_lossy(&self.s[start..self.i]).into_owned(),
                            long: true,
                        });
                    }
                    continue;
                }
                let start = self.i;
                let line = self.line;
                let col = self.col;
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.adv(1);
                }
                if let Some(comments) = &mut self.source_comments {
                    comments.push(SourceComment {
                        start,
                        end: self.i,
                        line,
                        text: String::from_utf8_lossy(&self.s[start..self.i]).into_owned(),
                        long: false,
                    });
                }
                if self.capture_storm_comments && self.s[start..self.i].starts_with(b"--@storm") {
                    let text = self.source[start..self.i].to_owned();
                    self.storm_comments.push(StormComment { line, col, text });
                }
                continue;
            }
            break;
        }
        Ok(())
    }

    fn read_long(&mut self, level: usize) -> Result<(), LexError> {
        let start = self.i;
        self.adv(2 + level);
        let close = format!("]{}]", "=".repeat(level));
        let close_bytes = close.as_bytes();
        let found = find_sub(&self.s[self.i..], close_bytes);
        let j = match found {
            Some(off) => self.i + off,
            None => return Err(LexError(format!("unclosed long bracket at {}", start))),
        };
        self.adv(j - self.i + close_bytes.len());
        Ok(())
    }

    /// Read one owned token, preserving the existing public lexer contract.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Token, LexError> {
        self.next_borrowed().map(BorrowedToken::into_owned)
    }

    /// Read the next token, or report a lexical error with its location.
    pub fn next_borrowed(&mut self) -> Result<BorrowedToken<'a>, LexError> {
        self.skip()?;
        if self.i >= self.s.len() {
            return Ok(BorrowedToken {
                k: TokenKind::Eof,
                v: "",
                p: self.i,
                line: self.line,
                col: self.col,
            });
        }
        let p = self.i;
        let line = self.line;
        let col = self.col;
        let ch = self.s[self.i];

        if let Some(level) = self.long_bracket(self.i) {
            self.read_long(level)?;
            let v = &self.source[p..self.i];
            return Ok(BorrowedToken {
                k: TokenKind::Str,
                v,
                p,
                line,
                col,
            });
        }
        if ch == b'\'' || ch == b'"' {
            let quote = ch;
            self.adv(1);
            let mut closed = false;
            while self.i < self.s.len() {
                let x = self.s[self.i];
                if x == b'\\' {
                    self.adv(1);
                    self.adv(1);
                    continue;
                }
                self.adv(1);
                if x == quote {
                    closed = true;
                    break;
                }
            }
            if !closed {
                return Err(LexError(format!("unclosed string at {}:{}", line, col)));
            }
            let v = &self.source[p..self.i];
            return Ok(BorrowedToken {
                k: TokenKind::Str,
                v,
                p,
                line,
                col,
            });
        }
        if is_id_start(ch) {
            self.adv(1);
            while self.i < self.s.len() && is_id_char(self.s[self.i]) {
                self.adv(1);
            }
            let v = &self.source[p..self.i];
            let k = if is_keyword(v) {
                TokenKind::Kw
            } else {
                TokenKind::Id
            };
            return Ok(BorrowedToken { k, v, p, line, col });
        }
        if is_digit(ch) || (ch == b'.' && self.i + 1 < self.s.len() && is_digit(self.s[self.i + 1]))
        {
            let m = number_len(&self.s[self.i..])
                .ok_or_else(|| LexError(format!("bad number at {}:{}", line, col)))?;
            self.adv(m);
            let v = &self.source[p..self.i];
            return Ok(BorrowedToken {
                k: TokenKind::Num,
                v,
                p,
                line,
                col,
            });
        }
        for op in MULTI {
            if self.s[self.i..].starts_with(op) {
                self.adv(op.len());
                let v = &self.source[p..self.i];
                return Ok(BorrowedToken {
                    k: TokenKind::Op,
                    v,
                    p,
                    line,
                    col,
                });
            }
        }
        if is_single_op(ch) {
            self.adv(1);
            let v = &self.source[p..self.i];
            return Ok(BorrowedToken {
                k: TokenKind::Op,
                v,
                p,
                line,
                col,
            });
        }
        Err(LexError(format!(
            "unexpected {} at {}:{}",
            display_unexpected_char(ch),
            line,
            col
        )))
    }

    /// Read a borrowed token stream, including EOF, without copying spellings.
    /// Tokens remain valid for the source lifetime, independently of this lexer.
    pub fn all_borrowed(&mut self) -> Result<Vec<BorrowedToken<'a>>, LexError> {
        let mut tokens = Vec::new();
        loop {
            let token = self.next_borrowed()?;
            let eof = token.k == TokenKind::Eof;
            tokens.push(token);
            if eof {
                return Ok(tokens);
            }
        }
    }

    /// Read the complete token stream, including the end-of-file token.
    pub fn all(&mut self) -> Result<Vec<Token>, LexError> {
        let mut out = Vec::new();
        loop {
            let t = self.next()?;
            let eof = t.k == TokenKind::Eof;
            out.push(t);
            if eof {
                return Ok(out);
            }
        }
    }
}

/// JS `\s` の ASCII メンバー（高速パス）。
fn is_js_ascii_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// JS `\s` の非 ASCII メンバー（U+0085 は含まない）。
fn is_js_unicode_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// バイト位置 i から UTF-8 文字を 1 つデコードする（境界外・不正時は None）。
fn decode_char(s: &[u8], i: usize) -> Option<char> {
    std::str::from_utf8(&s[i..]).ok()?.chars().next()
}

fn display_unexpected_char(b: u8) -> String {
    if b.is_ascii_graphic() || b == b' ' {
        format!("\"{}\"", b as char)
    } else {
        format!("0x{:02x}", b)
    }
}

/// JS 数値正規表現 `^(?:0[xX]...|\d+...)` の手書き再現。一致長を返す。
fn skip_hex(s: &[u8], mut k: usize) -> usize {
    while k < s.len() && is_hex(s[k]) {
        k += 1;
    }
    k
}
fn skip_digit(s: &[u8], mut k: usize) -> usize {
    while k < s.len() && is_digit(s[k]) {
        k += 1;
    }
    k
}

fn number_len(s: &[u8]) -> Option<usize> {
    // JS 正規表現 `^(?:0[xX](?:hex+(?:\.hex*)?|\.hex+)(?:[pP][+-]?\d+)?|(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?)`
    // の再現。hex 本体が無い場合（`0x` / `0x.` / `0xg` 等）は JS 同様に **decimal へフォールスルー**し、
    // 先頭の `0` を `\d+` で消費する（TS では `num:0 id:x` になる）。
    if s.len() >= 2 && s[0] == b'0' && (s[1] == b'x' || s[1] == b'X') {
        let mut k = 2;
        let mut matched = false;
        let d1 = skip_hex(s, k);
        if d1 > k {
            k = d1;
            matched = true;
            if k < s.len() && s[k] == b'.' {
                k += 1;
                k = skip_hex(s, k); // hex* は空でもよい
            }
        } else if k < s.len() && s[k] == b'.' {
            k += 1;
            let after = skip_hex(s, k);
            if after > k {
                k = after;
                matched = true;
            }
        }
        if matched {
            if k < s.len() && (s[k] == b'p' || s[k] == b'P') {
                let mut k2 = k + 1;
                if k2 < s.len() && (s[k2] == b'+' || s[k2] == b'-') {
                    k2 += 1;
                }
                let digits = skip_digit(s, k2);
                if digits > k2 {
                    k = digits;
                }
                // 指数に数字が無い場合は optional として消費しない（regex と同じ）
            }
            return Some(k);
        }
        // hex 本体無し → decimal 分岐へフォールスルー
    }
    // (?:digit+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?
    let mut k = 0;
    let mut matched = false;
    if is_digit(s[0]) {
        k = skip_digit(s, 0);
        matched = true;
        if k < s.len() && s[k] == b'.' {
            k += 1;
            k = skip_digit(s, k); // \d* は空でもよい
        }
    } else if s[0] == b'.' {
        k = 1;
        let after = skip_digit(s, k);
        if after > k {
            k = after;
            matched = true;
        }
    }
    if !matched {
        return None;
    }
    if k < s.len() && (s[k] == b'e' || s[k] == b'E') {
        let mut k2 = k + 1;
        if k2 < s.len() && (s[k2] == b'+' || s[k2] == b'-') {
            k2 += 1;
        }
        let digits = skip_digit(s, k2);
        if digits > k2 {
            k = digits;
        }
    }
    Some(k)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn numbers_match_js_regex() {
        // 各入力 → (matched_len, 次のバイト)
        let cases: &[(&str, Option<usize>)] = &[
            ("0", Some(1)),
            ("1.5", Some(3)),
            ("1.", Some(2)),
            (".5", Some(2)),
            ("1e5", Some(3)),
            ("1e+5", Some(4)),
            ("1E-3", Some(4)),
            // 指数部に数字が無い場合は optional として消費しない（regex と同じ）
            ("1e", Some(1)),
            ("1.", Some(2)),
            ("1.e5", Some(4)),
            (".5e2", Some(4)),
            ("1.e2", Some(4)),
            ("0x1", Some(3)),
            ("0x1.", Some(4)),
            ("0x.5", Some(4)),
            ("0x1.f", Some(5)),
            ("0x1p2", Some(5)),
            ("0x1p-2", Some(6)),
            ("0x1P+2", Some(6)),
            ("0X1", Some(3)),
            // hex 本体が無い場合は decimal へフォールバック（TS と同じ: `0` を消費）
            ("0x", Some(1)),
            ("0x.", Some(1)),
            ("0xg", Some(1)),
            ("0xpp", Some(1)),
            ("0x1p", Some(3)),  // 指数は p のみで数字なし → 消費しない
            ("0x1px", Some(3)), // p の後 x → optional として消費しない
            ("0x0x", Some(3)),  // 0x0 まで（x は hex ではない）
        ];
        for (input, expected) in cases {
            assert_eq!(number_len(input.as_bytes()), *expected, "input: {}", input);
        }
    }

    #[test]
    fn default_lexer_discards_all_comments() {
        let mut lexer = Lexer::new("x=1 --@storm ignore(foo)\ny=2 -- plain\n");
        let tokens = lexer.all().expect("lex ok");
        assert!(tokens.iter().all(|t| !t.v.contains("storm")));
        assert!(lexer.storm_comments().is_empty());
    }

    #[test]
    fn capture_mode_keeps_only_storm_comments_with_position() {
        let mut lexer = Lexer::new("x=1 -- plain\ny=2 --@storm ignore(foo)\nz=3 -- also plain")
            .with_storm_comment_capture();
        lexer.all().expect("lex ok");
        let comments = lexer.storm_comments();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].text, "--@storm ignore(foo)");
        assert_eq!(comments[0].line, 2);
        assert_eq!(comments[0].col, 5);
    }

    #[test]
    fn capture_mode_finds_storm_comment_mid_line_on_multiple_lines() {
        let mut lexer = Lexer::new(
            "local a = 1 --@storm ignore(undefined-global)\nlocal b = 2\nlocal c = a --@storm ignore(unused-local)",
        )
        .with_storm_comment_capture();
        lexer.all().expect("lex ok");
        let comments = lexer.storm_comments();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].line, 1);
        assert_eq!(comments[0].text, "--@storm ignore(undefined-global)");
        assert_eq!(comments[1].line, 3);
        assert_eq!(comments[1].text, "--@storm ignore(unused-local)");
    }

    #[test]
    fn capture_mode_ignores_long_bracket_comments() {
        let mut lexer =
            Lexer::new("--[[ --@storm ignore(foo) ]]\nx=1").with_storm_comment_capture();
        lexer.all().expect("lex ok");
        assert!(lexer.storm_comments().is_empty());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod borrowed_token_tests {
    use super::*;
    #[test]
    fn borrowed_tokens_reference_source_and_match_owned_positions() {
        let examples = [
            "local a=1\noutput.setNumber(1,a)",
            "--[==[ignored 雪😀\r\n[=[nested]=]]==]\r\nlocal t={x='雪😀',[[raw]]};return t.x",
            "--@storm ignore(test)\r\nlocal _ENV=_ENV return 0x10,0x1p2,.5,1..2",
            "\u{feff}\u{3000}local a=1 -- source\r\n return a//2<<1~=3",
            "",
            "-- only a comment",
            "[=[long\r\nstring]=]",
            "x='a\\\\b\\\"c' ::loop:: goto loop",
        ];
        for source in examples {
            let mut owned = Lexer::new(source)
                .with_comment_capture()
                .with_storm_comment_capture();
            let expected = owned.all().unwrap();
            let mut borrowed = Lexer::new(source)
                .with_comment_capture()
                .with_storm_comment_capture();
            let actual = borrowed.all_borrowed().unwrap();
            assert_eq!(expected.len(), actual.len());
            for (a, b) in actual.iter().zip(&expected) {
                assert_eq!(
                    (a.k, a.v, a.p, a.line, a.col),
                    (b.k, b.v.as_str(), b.p, b.line, b.col)
                );
                assert_eq!(a.v, &source[a.p..a.p + a.v.len()]);
                if !a.v.is_empty() {
                    assert_eq!(a.v.as_ptr(), source[a.p..].as_ptr());
                }
            }
            assert_eq!(owned.pos(), borrowed.pos());
            assert_eq!(owned.storm_comments(), borrowed.storm_comments());
            assert_eq!(
                serde_json::to_value(owned.comments()).unwrap(),
                serde_json::to_value(borrowed.comments()).unwrap()
            );
        }
    }
    #[test]
    fn lexical_failures_and_long_comment_delimiters_remain_explicit() {
        for (source, message) in [
            ("'unfinished", "unclosed string at 1:1"),
            ("[=[ unfinished", "unclosed long bracket at 0"),
            ("--[==[ unfinished", "unclosed long bracket at 2"),
            ("local x=`", "unexpected \"`\" at 1:9"),
        ] {
            assert_eq!(
                Lexer::new(source).all_borrowed().unwrap_err().to_string(),
                message
            );
            assert_eq!(Lexer::new(source).all().unwrap_err().to_string(), message);
        }
        for width in [0, 1, 2, 7, 33] {
            let eq = "=".repeat(width);
            let source = format!("--[{eq}[ comment\n雪😀 ]{eq}]\nreturn [={eq}[ payload ]={eq}]");
            let tokens = Lexer::new(&source).all_borrowed().unwrap();
            assert_eq!(tokens.len(), 3);
            assert_eq!(tokens[0].v, "return");
            assert_eq!(tokens[0].line, 3);
            assert_eq!(tokens[1].k, TokenKind::Str);
            assert!(tokens[1].v.contains("payload"));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod keyword_dispatch_tests {
    use super::*;
    #[test]
    fn generated_dispatch_matches_the_ordered_registry_and_token_boundaries() {
        let expected = [
            "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto",
            "if", "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until",
            "while",
        ];
        assert_eq!(KEYWORDS, expected);
        for word in KEYWORDS {
            for value in [
                word.to_string(),
                word.to_ascii_uppercase(),
                format!("_{word}"),
                format!("{word}0"),
                format!("{word}_"),
                format!("x{word}"),
                format!("{word}x"),
            ] {
                assert_eq!(is_keyword(&value), KEYWORDS.contains(&value.as_str()));
                let token = Lexer::new(&value).next_borrowed().unwrap();
                assert_eq!(
                    token.k,
                    if KEYWORDS.contains(&value.as_str()) {
                        TokenKind::Kw
                    } else {
                        TokenKind::Id
                    }
                );
                assert_eq!(token.v, value);
                assert_eq!(token.p, 0);
            }
            for cut in 0..word.len() {
                assert_eq!(is_keyword(&word[..cut]), KEYWORDS.contains(&&word[..cut]));
            }
        }
        let mut seed = 91u64;
        for n in 0..32768 {
            let mut bytes = Vec::new();
            for _ in 0..n % 16 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                bytes.push(b'a' + ((seed >> 32) % 26) as u8);
            }
            let text = String::from_utf8(bytes).unwrap();
            assert_eq!(is_keyword(&text), KEYWORDS.contains(&text.as_str()));
        }
        for text in ["雪", "😀", "İF", "lοcal", "if\0", "return\n", ""] {
            assert_eq!(is_keyword(text), KEYWORDS.contains(&text));
        }
    }
}
