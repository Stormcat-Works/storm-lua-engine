//! Small straight-line string grammars for generated drawing payloads only.
//! Replacements are literal, non-overlapping and reversed on decode. Lua pattern
//! metacharacters are never markers; '%' in replacement strings is escaped.
//! The final gsub result is parenthesized to retain one-value argument semantics.
use super::*;

fn emit(ast: &mut Ast, bytes: &str, dictionary: &[(char, String)]) -> NodeId {
    let mut value = ast.push(Node::Str(quote_lua(bytes).into()));
    if dictionary.is_empty() {
        return value;
    }
    value = ast.push(Node::Paren(value));
    for (marker, replacement) in dictionary.iter().rev() {
        let marker = ast.push(Node::Str(quote_lua(&marker.to_string()).into()));
        let replacement = replacement.replace('%', "%%");
        let replacement = ast.push(Node::Str(quote_lua(&replacement).into()));
        value = ast.push(Node::Call(
            value,
            vec![marker, replacement],
            Some("gsub".into()),
        ));
    }
    ast.push(Node::Paren(value))
}
fn size(bytes: &str, dictionary: &[(char, String)]) -> usize {
    let mut ast = Ast::new();
    let n = emit(&mut ast, bytes, dictionary);
    measure_expr(&ast, n)
}

pub(super) fn compress_literal(ast: &mut Ast, literal: NodeId) -> bool {
    let Node::Str(raw) = ast.node(literal) else {
        return false;
    };
    let original = storm_lua_syntax::numeric::decode_lua_string(raw);
    if !(80..=8192).contains(&original.len()) || !original.bytes().all(|b| (32..=126).contains(&b))
    {
        return false;
    }
    let mut text = original;
    let mut dictionary = Vec::<(char, String)>::new();
    let mut best_size = measure_expr(ast, literal);
    for _ in 0..8 {
        let Some(marker) =
            "zyxwvutsrqponmlkjihgfedcbaZYXWVUTSRQPONMLKJIHGFEDCBA0123456789!~@#&_=,;:/<>"
                .chars()
                .find(|&c| !text.contains(c))
        else {
            break;
        };
        let mut candidates = Vec::new();
        for width in [3, 4, 6, 8, 12, 16, 24, 32, 48, 64] {
            if width > text.len() {
                continue;
            }
            let mut counts = HashMap::<&str, (usize, usize)>::new();
            for at in 0..=text.len() - width {
                let part = &text[at..at + width];
                let (count, end) = counts.entry(part).or_default();
                if *count == 0 || at >= *end {
                    *count += 1;
                    *end = at + width;
                }
            }
            for (part, (count, _)) in counts {
                // Cheap rank only. Escaping and the complete decoder chain are
                // measured before acceptance, with deterministic lexical ties.
                let gain = (width - 1) * count;
                if count >= 2 && gain > width + 15 {
                    candidates.push((gain - width - 15, part.to_owned()));
                }
            }
        }
        candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let mut best = None;
        for (_, part) in candidates.into_iter().take(12) {
            let candidate = text.replace(&part, &marker.to_string());
            let mut dict = dictionary.clone();
            dict.push((marker, part));
            let candidate_size = size(&candidate, &dict);
            if candidate_size < best_size {
                best_size = candidate_size;
                best = Some((candidate, dict));
            }
        }
        let Some((next, dict)) = best else { break };
        text = next;
        dictionary = dict;
    }
    if dictionary.is_empty() {
        return false;
    }
    let first = ast.nodes.len();
    let origin = ast.nodes.capture_origin(literal);
    let replacement = emit(ast, &text, &dictionary);
    if ast.nodes.tracks_origins() {
        // Identify marker nodes structurally from newly generated gsub calls,
        // never by searching equal strings or looking at unrelated source.
        let markers = (first..ast.nodes.len())
            .filter_map(|n| match ast.node(n as NodeId) {
                Node::Call(_, args, Some(method)) if method == "gsub" => Some(args[0]),
                _ => None,
            })
            .collect::<HashSet<_>>();
        for n in first..ast.nodes.len() {
            let n = n as NodeId;
            if matches!(ast.node(n), Node::Str(_)) && !markers.contains(&n) {
                ast.nodes
                    .finish_rewrite(n, origin.clone(), "payload-dictionary-data");
            } else {
                ast.nodes.mark_synthetic(n, "payload-dictionary-decoder");
            }
        }
    }
    ast.nodes[literal as usize] = ast.node(replacement).clone();
    ast.nodes
        .finish_rewrite(literal, origin, "payload-dictionary-expansion");
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use mlua::Lua;
    #[test]
    fn grammar_preserves_percent_escapes_nested_markers_and_single_result() {
        for text in [
            "AB%CD\\EF'G\"HI".repeat(80),
            "abcdefghi".repeat(150),
            (0..400)
                .map(|i| if i % 7 == 0 { "abcPQRST" } else { "abcUVWXY" })
                .collect::<String>(),
        ] {
            let mut ast = Ast::new();
            let literal = ast.push(Node::Str(quote_lua(&text).into()));
            assert!(compress_literal(&mut ast, literal));
            let return_ = ast.push(Node::Return(vec![literal]));
            let root = ast.push(Node::Block(vec![return_]));
            let source = storm_lua_syntax::print::Printer::new(&ast, false).output(root);
            let lua = Lua::new();
            let values: mlua::MultiValue = lua.load(&source).eval().unwrap();
            assert_eq!(values.len(), 1, "{source}");
            let mlua::Value::String(value) = &values[0] else {
                panic!()
            };
            assert_eq!(value.as_bytes().as_ref(), text.as_bytes());
        }
    }

    #[test]
    fn dictionary_data_and_decoder_have_distinct_origins_and_missing_data_stays_unknown() {
        use storm_lua_syntax::provenance::{GeneratedOrigins, OriginKind};
        let text = "AB%CD\\EF'G\"HI".repeat(80);
        let source = format!("return {}", quote_lua(&text));
        let (original, root) =
            storm_lua_syntax::parse_source_with_origins("dictionary.lua", &source).unwrap();
        let Node::Block(stmts) = original.node(root) else {
            panic!()
        };
        let Node::Return(values) = original.node(stmts[0]) else {
            panic!()
        };
        let literal = values[0];
        for missing in [false, true] {
            let mut ast = original.clone();
            if missing {
                let value = ast.node(literal).clone();
                ast.nodes[literal as usize] = value;
            }
            assert!(compress_literal(&mut ast, literal));
            let printed = storm_lua_syntax::Printer::new(&ast, false).output_with_positions(root);
            let origins = GeneratedOrigins::from_print(&ast, &printed).unwrap();
            origins.validate_for_code(&printed.code).unwrap();
            if missing {
                assert!(origins.unknown_bytes() > 0);
            } else {
                assert_eq!(origins.unknown_bytes(), 0);
                assert!(origins.origins.iter().any(|o| o.transformation.as_deref()
                    == Some("payload-dictionary-data")
                    && o.kind == OriginKind::Derived));
                assert!(origins.origins.iter().any(|o| o.transformation.as_deref()
                    == Some("payload-dictionary-decoder")
                    && o.kind == OriginKind::Synthetic));
            }
            let lua = Lua::new();
            let values: mlua::MultiValue = lua.load(&printed.code).eval().unwrap();
            assert_eq!(values.len(), 1);
            let mlua::Value::String(actual) = &values[0] else {
                panic!()
            };
            assert_eq!(actual.as_bytes().as_ref(), text.as_bytes());
        }
    }
}
