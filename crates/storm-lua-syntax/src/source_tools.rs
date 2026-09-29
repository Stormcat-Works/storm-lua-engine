//! Opt-in, lossless source inspection for IDE declarations and build directives.
//! This projects the shared parser; it never evaluates Lua or optimizes a program.
use crate::lexer::SourceComment;
use crate::{
    parse_source_with_positions, Ast, Lexer, Node, NodeId, NodePositions, TableField, TokenKind,
};
use serde::Serialize;

/// A literal value, or an explicit expression that requires actual Lua execution.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Literal {
    /// Nil literal.
    Nil,
    /// Boolean literal.
    Bool {
        /// Literal truth value.
        value: bool,
    },
    /// Signed Lua integer, decimal text to preserve all 64 bits over JSON.
    Integer {
        /// Exact signed decimal spelling.
        value: String,
    },
    /// Binary64 value as exact hexadecimal bits.
    Number {
        /// Hexadecimal IEEE-754 binary64 bits.
        bits: String,
    },
    /// Lua byte string, never coerced through lossy UTF-8.
    Bytes {
        /// Exact Lua byte string contents.
        value: Vec<u8>,
    },
    /// Ordered literal table fields. Duplicate keys are retained for the consumer to reject or interpret.
    Table {
        /// Ordered key/value entries.
        entries: Vec<(Literal, Literal)>,
    },
    /// A nonliteral expression. Consumers must not treat this as nil or zero.
    Dynamic,
}
/// A statement's original source span and safely inspectable declaration shape.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Statement {
    /// call, do, or other; other statements must not be silently discarded by a declaration editor.
    pub kind: &'static str,
    /// Inclusive UTF-8 byte position.
    pub start: usize,
    /// Exclusive UTF-8 byte position.
    pub end: usize,
    /// One-based original line.
    pub line: u32,
    /// Direct, statically spelled function path, when available.
    pub function: Option<String>,
    /// Colon-call syntax adds an implicit self argument at runtime.
    pub method: bool,
    /// Explicit arguments; nonliteral expressions are marked Dynamic.
    pub arguments: Vec<Literal>,
    /// Statements only for a lexical do-block, not a conditional/function/loop body.
    pub body: Vec<Statement>,
}
/// Owned source inspection. Offsets are UTF-8 bytes, not UTF-16 editor offsets.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    /// False on malformed source or unsupported resource size.
    pub ok: bool,
    /// Actual lexical or syntax failure, not a fabricated empty program.
    pub error: Option<String>,
    /// Root statements, with nested plain do-blocks.
    pub statements: Vec<Statement>,
    /// Actual comment tokens, including long comments.
    pub comments: Vec<SourceComment>,
    /// Root locals/assignments that can replace a declaration namespace before a managed block.
    pub replaced_roots: Vec<String>,
}

/// Decode an entire valid Lua string literal to bytes. No execution or best-effort fallback.
pub fn string_bytes(raw: &str) -> Result<Vec<u8>, String> {
    let b = raw.as_bytes();
    if b.first() == Some(&b'[') {
        let level = b.iter().skip(1).take_while(|&&c| c == b'=').count();
        let start = level + 2;
        if b.get(start - 1) != Some(&b'[') {
            return Err("invalid long string delimiter".into());
        }
        let close = format!("]{}]", "=".repeat(level));
        if !raw.ends_with(&close) || b.len() < start + close.len() {
            return Err("unclosed long string".into());
        }
        let mut inner = &b[start..b.len() - close.len()];
        if inner.starts_with(b"\r\n") || inner.starts_with(b"\n\r") {
            inner = &inner[2..];
        } else if inner.first().is_some_and(|c| *c == b'\r' || *c == b'\n') {
            inner = &inner[1..];
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i < inner.len() {
            let c = inner[i];
            i += 1;
            if c == b'\r' || c == b'\n' {
                if i < inner.len() && (inner[i] == b'\r' || inner[i] == b'\n') && inner[i] != c {
                    i += 1;
                }
                out.push(b'\n');
            } else {
                out.push(c);
            }
        }
        return Ok(out);
    }
    let quote = *b.first().ok_or("empty string literal")?;
    if !matches!(quote, b'\'' | b'"') || b.len() < 2 || b.last() != Some(&quote) {
        return Err("invalid quoted string".into());
    }
    let mut out = Vec::new();
    let mut i = 1;
    let end = b.len() - 1;
    while i < end {
        let c = b[i];
        i += 1;
        if c == quote || c == b'\n' || c == b'\r' {
            return Err("unescaped quote/newline".into());
        }
        if c != b'\\' {
            out.push(c);
            continue;
        }
        if i >= end {
            return Err("unfinished escape".into());
        }
        let c = b[i];
        i += 1;
        match c {
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'n' => out.push(10),
            b'r' => out.push(13),
            b't' => out.push(9),
            b'v' => out.push(11),
            b'\\' | b'\'' | b'"' => out.push(c),
            b'\n' | b'\r' => {
                if i < end && matches!(b[i], b'\r' | b'\n') && b[i] != c {
                    i += 1;
                }
                out.push(10);
            }
            b'z' => {
                while i < end && b[i].is_ascii_whitespace() {
                    i += 1;
                }
            }
            b'x' => {
                if i + 2 > end {
                    return Err("incomplete hexadecimal escape".into());
                }
                let h =
                    std::str::from_utf8(&b[i..i + 2]).map_err(|_| "invalid hexadecimal escape")?;
                out.push(u8::from_str_radix(h, 16).map_err(|_| "invalid hexadecimal escape")?);
                i += 2;
            }
            b'u' => {
                if b.get(i) != Some(&b'{') {
                    return Err("invalid Unicode escape".into());
                }
                i += 1;
                let begin = i;
                while i < end && b[i].is_ascii_hexdigit() {
                    i += 1;
                }
                if i == begin || b.get(i) != Some(&b'}') {
                    return Err("invalid Unicode escape".into());
                }
                let hex =
                    std::str::from_utf8(&b[begin..i]).map_err(|_| "invalid Unicode escape")?;
                let code =
                    u32::from_str_radix(hex, 16).map_err(|_| "Unicode escape out of range")?;
                let ch = char::from_u32(code).ok_or("Unicode escape out of range")?;
                let mut buffer = [0; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
                i += 1;
            }
            b'0'..=b'9' => {
                let mut value = u32::from(c - b'0');
                for _ in 0..2 {
                    if i < end && b[i].is_ascii_digit() {
                        value = value * 10 + u32::from(b[i] - b'0');
                        i += 1;
                    } else {
                        break;
                    }
                }
                out.push(u8::try_from(value).map_err(|_| "decimal escape exceeds 255")?);
            }
            _ => return Err(format!("invalid Lua escape \\{}", c as char)),
        }
    }
    Ok(out)
}
/// Parse a literal value from a shared AST. Expressions are not evaluated.
pub fn literal(ast: &Ast, id: NodeId, depth: usize) -> Result<Literal, String> {
    if depth > 64 {
        return Err("literal nesting exceeds 64".into());
    }
    Ok(match ast.node(id) {
        Node::Nil => Literal::Nil,
        Node::Bool(value) => Literal::Bool { value: *value },
        Node::Str(raw) => Literal::Bytes {
            value: string_bytes(raw)?,
        },
        Node::Num(raw) => number_literal(raw),
        Node::Paren(value) => literal(ast, *value, depth + 1)?,
        Node::Un(op, value) if op == "-" => match literal(ast, *value, depth + 1)? {
            Literal::Integer { value } => Literal::Integer {
                value: value
                    .parse::<i64>()
                    .map_err(|_| "invalid integer")?
                    .wrapping_neg()
                    .to_string(),
            },
            Literal::Number { bits } => Literal::Number {
                bits: format!(
                    "{:016x}",
                    (-f64::from_bits(
                        u64::from_str_radix(&bits, 16).map_err(|_| "invalid float bits")?
                    ))
                    .to_bits()
                ),
            },
            _ => Literal::Dynamic,
        },
        Node::Table(fields) => {
            let mut index = 0_i64;
            let mut entries = Vec::new();
            for field in fields {
                entries.push(match field {
                    TableField::Arr(v) => {
                        index += 1;
                        (
                            Literal::Integer {
                                value: index.to_string(),
                            },
                            literal(ast, *v, depth + 1)?,
                        )
                    }
                    TableField::Name(k, v) => (
                        Literal::Bytes {
                            value: ast.strings.get(*k).as_bytes().to_vec(),
                        },
                        literal(ast, *v, depth + 1)?,
                    ),
                    TableField::KVar(k, v) => {
                        (literal(ast, *k, depth + 1)?, literal(ast, *v, depth + 1)?)
                    }
                });
            }
            Literal::Table { entries }
        }
        _ => Literal::Dynamic,
    })
}
fn number_literal(raw: &str) -> Literal {
    if let Some(value) = crate::numeric::integer_literal_value(raw) {
        Literal::Integer {
            value: value.to_string(),
        }
    } else {
        Literal::Number {
            bits: format!("{:016x}", crate::numeric::num_val(raw).to_bits()),
        }
    }
}
/// Static name path for a call, without guessing dynamic index values.
pub fn name_path(ast: &Ast, id: NodeId) -> Option<String> {
    match ast.node(id) {
        Node::Name(name) => Some(ast.strings.get(*name).into()),
        Node::Index(object, key, _) => {
            let Node::Str(raw) = ast.node(*key) else {
                return None;
            };
            let key = String::from_utf8(string_bytes(raw).ok()?).ok()?;
            Some(format!("{}.{}", name_path(ast, *object)?, key))
        }
        _ => None,
    }
}
fn statements(
    ast: &Ast,
    block: NodeId,
    positions: &NodePositions,
    depth: usize,
) -> Result<Vec<Statement>, String> {
    if depth > 128 {
        return Err("declaration nesting exceeds 128".into());
    }
    let Node::Block(ids) = ast.node(block) else {
        return Err("expected block".into());
    };
    ids.iter()
        .map(|id| {
            let (start, end) = positions.span(*id).ok_or("missing statement span")?;
            let mut statement = Statement {
                kind: "other",
                start,
                end,
                line: positions.get(*id).map_or(1, |p| p.0),
                function: None,
                method: false,
                arguments: vec![],
                body: vec![],
            };
            match ast.node(*id) {
                Node::Callstat(call) => {
                    if let Node::Call(f, args, method) = ast.node(*call) {
                        statement.kind = "call";
                        statement.method = method.is_some();
                        statement.function = name_path(ast, *f).map(|path| {
                            method
                                .as_ref()
                                .map_or(path.clone(), |m| format!("{path}.{m}"))
                        });
                        statement.arguments = args
                            .iter()
                            .map(|arg| literal(ast, *arg, 0))
                            .collect::<Result<_, _>>()?;
                    }
                }
                Node::Do(body) => {
                    statement.kind = "do";
                    statement.body = statements(ast, *body, positions, depth + 1)?;
                }
                _ => {}
            }
            Ok(statement)
        })
        .collect()
}
/// Inspect bounded source with the same lexer/parser used by the compiler. Does not execute Lua.
pub fn inspect_source(source: &str) -> Inspection {
    let mut result = Inspection {
        ok: false,
        error: None,
        statements: vec![],
        comments: vec![],
        replaced_roots: vec![],
    };
    type InspectedParts = (Vec<Statement>, Vec<SourceComment>, Vec<String>);
    let attempt = || -> Result<InspectedParts, String> {
        if source.len() > 2 * 1024 * 1024 {
            return Err("source inspection exceeds 2 MiB".into());
        }
        let mut lexer = Lexer::new(source).with_comment_capture();
        let tokens = lexer.all().map_err(|e| e.to_string())?;
        if tokens.len() > 200_000 {
            return Err("source inspection exceeds token budget".into());
        }
        // Bound lexical nesting before the recursive parser receives untrusted imports.
        let mut depth = 0_i32;
        for token in &tokens {
            if token.k != TokenKind::Str
                && matches!(
                    token.v.as_str(),
                    "(" | "{" | "[" | "function" | "do" | "if" | "repeat"
                )
            {
                depth += 1;
            }
            if token.k != TokenKind::Str
                && matches!(token.v.as_str(), ")" | "}" | "]" | "end" | "until")
            {
                depth = (depth - 1).max(0);
            }
            if depth > 128 {
                return Err("source nesting exceeds inspection limit".into());
            }
        }
        let (ast, root, positions) =
            parse_source_with_positions(source).map_err(|e| e.to_string())?;
        let mut roots = Vec::new();
        if let Node::Block(ids) = ast.node(root) {
            for id in ids {
                match ast.node(*id) {
                    Node::Local(names, _) => {
                        roots.extend(names.iter().map(|n| ast.strings.get(*n).to_string()))
                    }
                    Node::Localfunc(n, _) => roots.push(ast.strings.get(*n).into()),
                    Node::Assign(names, _) => {
                        roots.extend(names.iter().filter_map(|id| match ast.node(*id) {
                            Node::Name(n) => Some(ast.strings.get(*n).to_string()),
                            _ => None,
                        }))
                    }
                    _ => {}
                }
            }
        }
        Ok((
            statements(&ast, root, &positions, 0)?,
            lexer.comments().to_vec(),
            roots,
        ))
    };
    match attempt() {
        Ok((statements, comments, roots)) => {
            result.ok = true;
            result.statements = statements;
            result.comments = comments;
            result.replaced_roots = roots;
        }
        Err(error) => result.error = Some(error),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_bytes_and_escapes_are_not_a_json_approximation() {
        assert_eq!(
            string_bytes(r#""a\t\xFF\000\u{1f431}""#).ok(),
            Some([b'a', 9, 255, 0, 240, 159, 144, 177].to_vec())
        );
        assert_eq!(
            string_bytes("[=[\r\nx\r\ny]=]").ok(),
            Some(b"x\ny".to_vec())
        );
        assert!(string_bytes(r#""\q""#).is_err());
    }
    #[test]
    fn real_comments_and_multiline_calls_have_exact_spans() {
        let source =
            "local text=[=[-- >>> fake]=]\n-- real\ndo\n sim.setProperty(\n '名',1e3)\nend\n";
        let result = inspect_source(source);
        assert!(result.ok, "{:?}", result.error);
        assert_eq!(result.comments.len(), 1);
        let statement = &result.statements[1].body[0];
        assert_eq!(
            &source[statement.start..statement.end],
            "sim.setProperty(\n '名',1e3)"
        );
        assert!(
            matches!(&statement.arguments[1],Literal::Number{bits} if bits=="408f400000000000")
        );
    }
    #[test]
    fn conditional_and_unknown_expressions_are_not_executed_or_treated_as_zero() {
        let result = inspect_source(
            "if false then sim.setProperty('no',1)end\nsim.setProperty('dynamic',calculate())",
        );
        assert!(result.ok);
        assert_eq!(result.statements[0].kind, "other");
        assert!(matches!(
            result.statements[1].arguments[1],
            Literal::Dynamic
        ));
    }
}

#[cfg(test)]
mod lossless_tests {
    #[test]
    fn shared_inspection_preserves_integers_and_bytes_without_running_expressions() {
        use super::{inspect_source, Literal};
        let inspected=inspect_source("do\n sim.setProperty('x',0x7fffffffffffffff)\n sim.setProperty('raw','\\255\\000')\n sim.setProperty('expr',danger())\nend");
        assert!(inspected.ok);
        let calls = &inspected.statements[0].body;
        assert!(
            matches!(&calls[0].arguments[1],Literal::Integer{value} if value=="9223372036854775807")
        );
        assert!(matches!(&calls[1].arguments[1],Literal::Bytes{value} if value==&[255,0]));
        assert!(matches!(calls[2].arguments[1], Literal::Dynamic));
    }
}
