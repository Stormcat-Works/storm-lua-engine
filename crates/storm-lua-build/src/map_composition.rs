//! Compose optimization origins through the linker's exact copied byte ranges.
use crate::link::LinkResult;
use crate::source_map::{module_key_to_path, source_text};
use std::collections::BTreeMap;
use std::sync::Arc;
use storm_lua_analysis::project::LuaProject;
use storm_lua_syntax::explanation::{RelationRole, SourceDisposition, SourceRelation};
use storm_lua_syntax::provenance::{
    GeneratedOrigins, Origin, OriginKind, OriginMapping, OriginPrecision, SourceSnapshot,
    SourceSpan,
};

struct Copies<'a> {
    link: &'a LinkResult,
    source_ids: BTreeMap<&'a str, u32>,
}
impl Copies<'_> {
    fn span(&self, span: SourceSpan) -> Vec<SourceSpan> {
        let mut output = Vec::new();
        for range in &self.link.ranges {
            if range.output_end_byte <= span.start {
                continue;
            }
            if range.output_start_byte >= span.end {
                break;
            }
            let start = span.start.max(range.output_start_byte);
            let end = span.end.min(range.output_end_byte);
            if start < end {
                let value = SourceSpan {
                    source: self.source_ids[range.module.as_str()],
                    start: range.source_start_byte + start - range.output_start_byte,
                    end: range.source_start_byte + end - range.output_start_byte,
                };
                if let Some(last) = output
                    .last_mut()
                    .filter(|v: &&mut SourceSpan| v.source == value.source && v.end == value.start)
                {
                    last.end = value.end;
                } else if !output.contains(&value) {
                    output.push(value);
                }
            }
        }
        output
    }
}
/// Preserve selected-candidate explanations while replacing linked-source coordinates.
/// Generated linker glue is explicitly synthetic; unknown origins are never filled in.
pub(crate) fn compose(
    project: &LuaProject,
    link: &LinkResult,
    code: &str,
    input: &GeneratedOrigins,
) -> Result<GeneratedOrigins, String> {
    let linked = link
        .linked_source
        .as_deref()
        .ok_or("cannot compose a failed link")?;
    input.validate_for_code(code)?;
    if input.sources.len() != 1 || input.sources[0].text.as_ref() != linked {
        return Err("optimization map does not belong to this linked source".into());
    }
    let mut source_ids = BTreeMap::new();
    let mut sources = Vec::new();
    for key in &link.used_modules {
        if !source_ids.contains_key(key.as_str()) {
            let text = source_text(project, key).ok_or("used module snapshot is missing")?;
            source_ids.insert(key.as_str(), sources.len() as u32);
            sources.push(SourceSnapshot {
                name: module_key_to_path(key).into(),
                text: text.into(),
            });
        }
    }
    for range in &link.ranges {
        if !source_ids.contains_key(range.module.as_str()) {
            let id = sources.len() as u32;
            let text =
                source_text(project, &range.module).ok_or("linked source snapshot is missing")?;
            sources.push(SourceSnapshot {
                name: module_key_to_path(&range.module).into(),
                text: text.into(),
            });
            source_ids.insert(range.module.as_str(), id);
        }
    }
    let copies = Copies { link, source_ids };
    let mut output = input.clone();
    output.sources = Arc::new(sources);
    for origin in &mut output.origins {
        let primary = origin
            .primary
            .map(|span| copies.span(span))
            .unwrap_or_default();
        let mut related = origin
            .related
            .iter()
            .flat_map(|s| copies.span(*s))
            .collect::<Vec<_>>();
        let mut relations = origin
            .relations
            .iter()
            .flat_map(|r| {
                copies
                    .span(r.span)
                    .into_iter()
                    .map(move |span| SourceRelation { role: r.role, span })
            })
            .collect::<Vec<_>>();
        if let Some(first) = primary.first().copied() {
            origin.primary = Some(first);
            for &span in &primary[1..] {
                related.push(span);
                relations.push(SourceRelation {
                    role: RelationRole::Contribution,
                    span,
                });
            }
            if primary.len() > 1 {
                origin.precision = OriginPrecision::Group;
                origin.name = None;
            }
            if origin.name.as_ref().is_some_and(|name| {
                output.sources[first.source as usize]
                    .text
                    .get(first.start..first.end)
                    != Some(name.as_ref())
            }) {
                origin.name = None;
            }
        } else if origin.primary.is_some() {
            origin.kind = OriginKind::Synthetic;
            origin.primary = None;
            origin.name = None;
            origin.precision = OriginPrecision::Group;
            origin.transformation = Some("linker-generated".into());
            let synthesized = Origin::synthetic("linker-generated");
            Arc::make_mut(&mut origin.reasons).extend(synthesized.reasons.iter().cloned());
        }
        related.dedup();
        relations.dedup();
        origin.related = Arc::new(related);
        origin.relations = Arc::new(relations);
    }
    let mut context_ids = Vec::with_capacity(input.contexts.len());
    output.contexts.clear();
    for context in &input.contexts {
        let defs = copies.span(context.definition);
        let calls = copies.span(context.call_site);
        // One context can span several original modules. Keep each actual pair,
        // rather than fabricating a continuous cross-file range.
        let mut mapped = Vec::new();
        for &definition in &defs {
            for &call_site in &calls {
                mapped.push(output.contexts.len() as u32);
                output
                    .contexts
                    .push(storm_lua_syntax::explanation::InlineContext {
                        definition,
                        call_site,
                    });
            }
        }
        context_ids.push(mapped);
    }
    output.dispositions = Arc::new(
        input
            .dispositions
            .iter()
            .flat_map(|d| {
                copies
                    .span(d.original)
                    .into_iter()
                    .map(|original| SourceDisposition {
                        original,
                        reason: d.reason.clone(),
                        replacement_sources: d
                            .replacement_sources
                            .iter()
                            .flat_map(|s| copies.span(*s))
                            .collect(),
                    })
            })
            .collect(),
    );
    output.mappings.clear();
    let generated_id = output.origins.len() as u32;
    output.origins.push(Origin::synthetic("linker-generated"));
    for mapping in &input.mappings {
        let contexts = mapping
            .inline_contexts
            .iter()
            .flat_map(|i| context_ids[*i as usize].iter().copied())
            .collect::<Vec<_>>();
        let Some(copy) = mapping.copied else {
            let mut m = mapping.clone();
            m.inline_contexts = contexts;
            output.mappings.push(m);
            continue;
        };
        let mut cursor = copy.start;
        for range in &link.ranges {
            if range.output_end_byte <= copy.start {
                continue;
            }
            if range.output_start_byte >= copy.end {
                break;
            }
            let start = copy.start.max(range.output_start_byte);
            let end = copy.end.min(range.output_end_byte);
            if cursor < start {
                output.mappings.push(OriginMapping {
                    start: mapping.start + cursor - copy.start,
                    end: mapping.start + start - copy.start,
                    origin: Some(generated_id),
                    copied: None,
                    inline_contexts: contexts.clone(),
                });
            }
            let source = SourceSpan {
                source: copies.source_ids[range.module.as_str()],
                start: range.source_start_byte + start - range.output_start_byte,
                end: range.source_start_byte + end - range.output_start_byte,
            };
            // A single identity mapping may straddle many source files. Each copied
            // part needs its own primary source, not the broad linked root anchor.
            let id = output.origins.len() as u32;
            let mut o = mapping
                .origin
                .map(|i| output.origins[i as usize].clone())
                .unwrap_or_else(|| Origin::source(source, OriginPrecision::Token, None));
            o.primary = Some(source);
            if o.kind == OriginKind::Synthetic {
                o.kind = OriginKind::Source;
                o.transformation = None;
            }
            if o.name.as_ref().is_some_and(|name| {
                output.sources[source.source as usize]
                    .text
                    .get(source.start..source.end)
                    != Some(name.as_ref())
            }) {
                o.name = None;
            }
            output.origins.push(o);
            output.mappings.push(OriginMapping {
                start: mapping.start + start - copy.start,
                end: mapping.start + end - copy.start,
                origin: Some(id),
                copied: Some(source),
                inline_contexts: contexts.clone(),
            });
            cursor = end;
        }
        if cursor < copy.end {
            output.mappings.push(OriginMapping {
                start: mapping.start + cursor - copy.start,
                end: mapping.end,
                origin: Some(generated_id),
                copied: None,
                inline_contexts: contexts,
            });
        }
    }
    output.validate_for_code(code)?;
    Ok(output)
}
