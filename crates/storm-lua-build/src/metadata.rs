//! Leading tagged metadata removal, shared by both build profiles.
use storm_lua_syntax::Lexer;

pub(crate) fn metadata(source: &str, tag: Option<&str>) -> Result<String, String> {
    let Some(tag) = tag else {
        return Ok(source.into());
    };
    if tag.is_empty() || tag.len() > 128 || tag.contains(['\r', '\n']) {
        return Err("metadataTag must be a single non-empty line of at most 128 bytes".into());
    }
    let mut lexer = Lexer::new(source).with_comment_capture();
    lexer.all().map_err(|e| e.to_string())?;
    let mut bytes = source.as_bytes().to_vec();
    for comment in lexer.comments() {
        if !comment.long
            || !source[..comment.start]
                .trim_start_matches('\u{feff}')
                .trim()
                .is_empty()
        {
            continue;
        }
        let raw = &source[comment.start..comment.end];
        let Some(tail) = raw.strip_prefix("--[") else {
            continue;
        };
        let level = tail.bytes().take_while(|b| *b == b'=').count();
        let Some(content) = tail.get(level..).and_then(|s| s.strip_prefix('[')) else {
            continue;
        };
        if content.lines().next().map(str::trim) != Some(tag) {
            continue;
        }
        for byte in &mut bytes[comment.start..comment.end] {
            if !matches!(*byte, b'\r' | b'\n') {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}
