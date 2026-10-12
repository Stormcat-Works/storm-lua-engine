//! Leading tagged metadata removal, shared by both build profiles.
use crate::link::{exclude_blanked_ranges, LinkResult};
use std::{borrow::Cow, collections::BTreeMap, ops::Range};
use storm_lua_analysis::LuaProject;
use storm_lua_syntax::Lexer;

fn metadata(source: &str, tag: &str) -> Result<(String, Vec<Range<usize>>), String> {
    if tag.is_empty() || tag.len() > 128 || tag.contains(['\r', '\n']) {
        return Err("metadataTag must be a single non-empty line of at most 128 bytes".into());
    }
    let mut lexer = Lexer::new(source).with_comment_capture();
    lexer.all().map_err(|e| e.to_string())?;
    let mut bytes = source.as_bytes().to_vec();
    let mut removed = Vec::new();
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
        removed.push(comment.start..comment.end);
        for byte in &mut bytes[comment.start..comment.end] {
            if !matches!(*byte, b'\r' | b'\n') {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(bytes)
        .map(|source| (source, removed))
        .map_err(|e| e.to_string())
}

/// Blanked source and the explicit original intervals removed before linking.
pub(crate) struct PreparedMetadata<'a> {
    pub project: Cow<'a, LuaProject>,
    original: &'a LuaProject,
    removed: BTreeMap<String, Vec<Range<usize>>>,
}

pub(crate) fn prepare<'a>(
    project: &'a LuaProject,
    tag: Option<&str>,
) -> Result<PreparedMetadata<'a>, (String, String)> {
    let Some(tag) = tag else {
        return Ok(PreparedMetadata {
            project: Cow::Borrowed(project),
            original: project,
            removed: BTreeMap::new(),
        });
    };
    let mut prepared = project.clone();
    let mut removed = BTreeMap::new();
    for (name, source) in &mut prepared.modules {
        let (text, spans) = metadata(source, tag).map_err(|e| (name.clone(), e))?;
        *source = text;
        if !spans.is_empty() {
            removed.insert(name.clone(), spans);
        }
    }
    Ok(PreparedMetadata {
        project: Cow::Owned(prepared),
        original: project,
        removed,
    })
}

impl PreparedMetadata<'_> {
    /// Translate recorded metadata edits through the linker and remove their copy claims.
    /// No snapshot replacement or inference from string equality is involved.
    pub fn exclude_removed_ranges(&self, link: &mut LinkResult) -> Result<(), String> {
        if self.removed.is_empty() {
            return Ok(());
        }
        let Some(code) = link.linked_source.as_deref() else {
            return Ok(());
        };
        let mut removed = Vec::new();
        for range in &link.ranges {
            if let Some(spans) = self.removed.get(&range.module) {
                let source_end =
                    range.source_start_byte + range.output_end_byte - range.output_start_byte;
                for span in spans {
                    let start = span.start.max(range.source_start_byte);
                    let end = span.end.min(source_end);
                    if start < end {
                        removed.push(
                            range.output_start_byte + start - range.source_start_byte
                                ..range.output_start_byte + end - range.source_start_byte,
                        );
                    }
                }
            }
        }
        if removed.is_empty() {
            return Ok(());
        }
        link.ranges = exclude_blanked_ranges(
            self.original,
            code,
            std::mem::take(&mut link.ranges),
            removed,
        )?;
        Ok(())
    }
}
