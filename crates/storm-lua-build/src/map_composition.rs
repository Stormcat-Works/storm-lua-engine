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

/// Whether `input` has exactly the lexical materialized shape: every origin
/// is a plain Source/Name/Token origin, every mapping is an exact copy, a
/// renamed token (copied None, Name origin), or an unknown gap, and no
/// auxiliary tables are present. The token-table gate accepts this shape
/// (modulo its ordering/coverage checks), so preserving it preserves
/// eligibility.
fn is_simple_shape(input: &GeneratedOrigins) -> bool {
    if !input.contexts.is_empty() || !input.constructs.is_empty() || !input.dispositions.is_empty()
    {
        return false;
    }
    for origin in &input.origins {
        if origin.kind != OriginKind::Source
            || origin.primary.is_none()
            || !matches!(
                origin.precision,
                OriginPrecision::Name | OriginPrecision::Token
            )
            || origin.transformation.is_some()
            || !origin.related.is_empty()
            || !origin.relations.is_empty()
            || !origin.reasons.is_empty()
            || (origin.precision == OriginPrecision::Name) != origin.name.is_some()
        {
            return false;
        }
    }
    for mapping in &input.mappings {
        if !mapping.inline_contexts.is_empty() {
            return false;
        }
        match (mapping.origin, mapping.copied) {
            (None, None) => {}
            (Some(id), Some(copied)) => {
                let Some(origin) = input.origins.get(id as usize) else {
                    return false;
                };
                if Some(copied) != origin.primary {
                    return false;
                }
            }
            (Some(id), None) => {
                let Some(origin) = input.origins.get(id as usize) else {
                    return false;
                };
                if origin.precision != OriginPrecision::Name || origin.name.is_none() {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

/// Remap a simple-shaped input in place: each origin keeps its id with its
/// primary remapped through the link ranges, and each copied mapping keeps
/// its origin id with its copied span remapped. Returns `None` (caller
/// falls back to the legacy path) when any primary or copied span splits
/// across ranges, vanishes into linker glue, or fails final validation.
/// The accepted case is exactly the one where the legacy path would clone
/// one origin per mapping part and append a synthetic origin: the remapped
/// primaries, names and copied spans agree, only the origin table stays
/// compact and gate-eligible.
fn try_compose_simple(
    copies: &Copies<'_>,
    code: &str,
    mut output: GeneratedOrigins,
) -> Option<GeneratedOrigins> {
    for origin in &mut output.origins {
        let primary = origin.primary?;
        let mapped = copies.span(primary);
        if mapped.len() != 1 {
            return None;
        }
        let first = mapped[0];
        origin.primary = Some(first);
        if origin.name.as_ref().is_some_and(|name| {
            output.sources[first.source as usize]
                .text
                .get(first.start..first.end)
                != Some(name.as_ref())
        }) {
            origin.name = None;
        }
    }
    for mapping in &mut output.mappings {
        let Some(copy) = mapping.copied else {
            continue;
        };
        // Covered linked intervals, computed exactly like the legacy loop
        // below: a single interval equal to the whole copied span is the
        // only case this path accepts.
        let mut covered: Option<(usize, usize, SourceSpan)> = None;
        for range in &copies.link.ranges {
            if range.output_end_byte <= copy.start {
                continue;
            }
            if range.output_start_byte >= copy.end {
                break;
            }
            let start = copy.start.max(range.output_start_byte);
            let end = copy.end.min(range.output_end_byte);
            if start >= end {
                continue;
            }
            let source = SourceSpan {
                source: copies.source_ids[range.module.as_str()],
                start: range.source_start_byte + start - range.output_start_byte,
                end: range.source_start_byte + end - range.output_start_byte,
            };
            match &mut covered {
                None => covered = Some((start, end, source)),
                Some(_) => return None,
            }
        }
        let (start, end, source) = covered?;
        if start != copy.start || end != copy.end {
            return None;
        }
        mapping.copied = Some(source);
    }
    output.validate_for_code(code).ok()?;
    Some(output)
}

/// Whether an origin kept its token-table-simple shape through the legacy
/// origins transformation: a single Source primary, Name/Token precision,
/// and no transformation, related spans, relations or reasons.
fn is_simple_stable(origin: &Origin) -> bool {
    origin.kind == OriginKind::Source
        && origin.primary.is_some()
        && matches!(
            origin.precision,
            OriginPrecision::Name | OriginPrecision::Token
        )
        && origin.transformation.is_none()
        && origin.related.is_empty()
        && origin.relations.is_empty()
        && origin.reasons.is_empty()
        && (origin.precision == OriginPrecision::Name) == origin.name.is_some()
}

/// Partial simple preservation for mixed inputs. The origins, contexts and
/// dispositions transformations are exactly the legacy ones, but a copied
/// mapping whose transformed origin is simple-stable and whose copied span
/// is covered by exactly one link range equal to that origin's primary
/// keeps its single origin instead of a per-part clone (the legacy clone
/// would carry the same primary and pass the same name-slice check, so the
/// standard derivation of the mapping is identical; only the origins table
/// stays compact). All other mappings follow the legacy per-part emission,
/// with the synthetic gap origin added lazily on first use. Returns
/// `None` — leaving the caller to run the legacy path on the pristine
/// input — when no mapping took the fast branch or the result fails
/// validation, so inputs without preservable mappings compose exactly as
/// before.
fn try_compose_partial(
    copies: &Copies<'_>,
    code: &str,
    mut output: GeneratedOrigins,
    input: &GeneratedOrigins,
) -> Option<GeneratedOrigins> {
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
    let mut generated_id: Option<u32> = None;
    let mut fast_mappings = 0usize;
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
        // Fast branch: one covering range whose source span is the
        // transformed origin's primary (see the function docs).
        let fast_source = mapping.origin.and_then(|id| {
            let origin = output.origins.get(id as usize)?;
            if !is_simple_stable(origin) {
                return None;
            }
            let mut covered: Option<(usize, usize, SourceSpan)> = None;
            for range in &copies.link.ranges {
                if range.output_end_byte <= copy.start {
                    continue;
                }
                if range.output_start_byte >= copy.end {
                    break;
                }
                let start = copy.start.max(range.output_start_byte);
                let end = copy.end.min(range.output_end_byte);
                if start >= end {
                    continue;
                }
                let source = SourceSpan {
                    source: copies.source_ids[range.module.as_str()],
                    start: range.source_start_byte + start - range.output_start_byte,
                    end: range.source_start_byte + end - range.output_start_byte,
                };
                match &mut covered {
                    None => covered = Some((start, end, source)),
                    Some(_) => return None,
                }
            }
            let (start, end, source) = covered?;
            if start != copy.start || end != copy.end || origin.primary != Some(source) {
                return None;
            }
            Some(source)
        });
        if let Some(source) = fast_source {
            fast_mappings += 1;
            output.mappings.push(OriginMapping {
                start: mapping.start,
                end: mapping.end,
                origin: mapping.origin,
                copied: Some(source),
                inline_contexts: contexts,
            });
            continue;
        }
        let mut cursor = copy.start;
        for range in &copies.link.ranges {
            if range.output_end_byte <= copy.start {
                continue;
            }
            if range.output_start_byte >= copy.end {
                break;
            }
            let start = copy.start.max(range.output_start_byte);
            let end = copy.end.min(range.output_end_byte);
            if cursor < start {
                let gap_id = *generated_id.get_or_insert_with(|| {
                    let id = output.origins.len() as u32;
                    output.origins.push(Origin::synthetic("linker-generated"));
                    id
                });
                output.mappings.push(OriginMapping {
                    start: mapping.start + cursor - copy.start,
                    end: mapping.start + start - copy.start,
                    origin: Some(gap_id),
                    copied: None,
                    inline_contexts: contexts.clone(),
                });
            }
            let source = SourceSpan {
                source: copies.source_ids[range.module.as_str()],
                start: range.source_start_byte + start - range.output_start_byte,
                end: range.source_start_byte + end - range.output_start_byte,
            };
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
            let gap_id = *generated_id.get_or_insert_with(|| {
                let id = output.origins.len() as u32;
                output.origins.push(Origin::synthetic("linker-generated"));
                id
            });
            output.mappings.push(OriginMapping {
                start: mapping.start + cursor - copy.start,
                end: mapping.end,
                origin: Some(gap_id),
                copied: None,
                inline_contexts: contexts,
            });
        }
    }
    if fast_mappings == 0 {
        return None;
    }
    output.validate_for_code(code).ok()?;
    Some(output)
}

/// Preserve selected-candidate explanations while replacing linked-source coordinates.
/// Generated linker glue is explicitly synthetic; unknown origins are never filled in.
///
/// Simple-shaped inputs (the lexical materialized shape: Source origins with
/// Name/Token precision, no relations, reasons, contexts, constructs or
/// dispositions, and mappings that are either exact copies or renamed
/// tokens) take a fast path that remaps origins in place instead of cloning
/// one origin per mapping part. That preserves the shape the schema-2
/// token-table gate accepts, so a composed lexical map can still ship as a
/// token table. Mixed inputs take a partial path that preserves the
/// simple-stable mappings the same way and follows the legacy emission for
/// the rest. Any input or remapping outside those shapes falls back to
/// the legacy path below, whose output is unchanged.
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
    // Simple-shape fast path (see the module docs above). On any failure
    // the pristine `output` below still runs through the legacy path.
    if is_simple_shape(&output) {
        if let Some(simple) = try_compose_simple(&copies, code, output.clone()) {
            return Ok(simple);
        }
    }
    // Partial simple preservation for mixed inputs (see
    // `try_compose_partial`): also runs on a clone, so a rejection leaves
    // the pristine `output` for the legacy path below.
    if let Some(partial) = try_compose_partial(&copies, code, output.clone(), input) {
        return Ok(partial);
    }
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::link::{LinkResult, LinkedRange};
    use storm_lua_minify::CompileOptions;

    fn identity_link(source: &str) -> (LuaProject, LinkResult) {
        let project = LuaProject {
            entry: "main".into(),
            modules: BTreeMap::from([("main".to_string(), source.to_string())]),
            ambient: BTreeMap::new(),
        };
        let link = LinkResult {
            diagnostics: Vec::new(),
            linked_source: Some(source.to_string()),
            used_modules: vec!["main".to_string()],
            ranges: vec![LinkedRange {
                output_start_line: 0,
                output_end_line: source.lines().count() as u32,
                module: "main".to_string(),
                source_start_line: 0,
                output_start_byte: 0,
                output_end_byte: source.len(),
                source_start_byte: 0,
            }],
            injected_ambient: BTreeMap::new(),
        };
        (project, link)
    }

    #[test]
    fn simple_lexical_compose_preserves_origin_count_and_token_table() {
        let source = "local longname=1\n_ENV.x=longname\nreturn longname\n";
        let plan: Vec<(usize, usize, String)> = source
            .match_indices("longname")
            .map(|(i, _)| (i, i + "longname".len(), "a".to_string()))
            .collect();
        let (code, compact) = storm_lua_syntax::print::lexical_minify_renamed_with_compact_origins(
            "lexical.lua",
            source,
            &plan,
        )
        .unwrap();
        let input = compact.into_generated();
        let (project, link) = identity_link(source);
        let composed = compose(&project, &link, &code, &input).unwrap();
        // No per-part clones and no synthetic origin: the table stays compact.
        assert_eq!(composed.origins.len(), input.origins.len());
        assert_eq!(composed.mappings.len(), input.mappings.len());
        assert_eq!(composed.sources.len(), 1);
        let map = crate::optimized_source_map::encode_owned_for_options(
            &code,
            composed,
            &CompileOptions::default(),
            None,
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_str(&map).unwrap();
        assert!(!json["x_storm"]["tokens"].as_array().unwrap().is_empty());
        crate::optimized_source_map::validate(&code, &map).unwrap();
    }

    #[test]
    fn unrenamed_lexical_compose_preserves_origin_count_and_token_table() {
        let source = "local _ENV=_ENV function onTick()output.setNumber(1,2)end";
        let (code, compact) =
            storm_lua_syntax::print::lexical_minify_with_compact_origins("lexical.lua", source)
                .unwrap();
        let input = compact.into_generated();
        let (project, link) = identity_link(source);
        let composed = compose(&project, &link, &code, &input).unwrap();
        assert_eq!(composed.origins.len(), input.origins.len());
        assert_eq!(composed.mappings.len(), input.mappings.len());
        let map = crate::optimized_source_map::encode_owned_for_options(
            &code,
            composed,
            &CompileOptions::default(),
            None,
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_str(&map).unwrap();
        assert!(!json["x_storm"]["tokens"].as_array().unwrap().is_empty());
        crate::optimized_source_map::validate(&code, &map).unwrap();
    }

    #[test]
    fn non_simple_input_falls_back_to_legacy_compose() {
        let source = "local _ENV=_ENV function onTick()output.setNumber(1,2)end";
        let (code, compact) =
            storm_lua_syntax::print::lexical_minify_with_compact_origins("lexical.lua", source)
                .unwrap();
        let mut input = compact.into_generated();
        // An auxiliary table plus origins that cannot stay simple-stable:
        // no mapping can take a preservation branch, so the legacy path
        // must run. (Auxiliary tables alone do not disqualify the partial
        // path; contexts and dispositions remap identically there.)
        input
            .contexts
            .push(storm_lua_syntax::explanation::InlineContext {
                definition: SourceSpan {
                    source: 0,
                    start: 0,
                    end: 5,
                },
                call_site: SourceSpan {
                    source: 0,
                    start: 6,
                    end: 11,
                },
            });
        for origin in &mut input.origins {
            origin.precision = OriginPrecision::Group;
            origin.name = None;
        }
        let (project, link) = identity_link(source);
        let composed = compose(&project, &link, &code, &input).unwrap();
        // Legacy path clones one origin per mapping part and appends the
        // synthetic origin, so the table grows as before.
        assert!(composed.origins.len() > input.origins.len());
    }

    #[test]
    fn partial_compose_preserves_simple_origins_in_mixed_input() {
        let source = "local _ENV=_ENV function onTick()output.setNumber(1,2)end";
        let (code, compact) =
            storm_lua_syntax::print::lexical_minify_with_compact_origins("lexical.lua", source)
                .unwrap();
        let mut input = compact.into_generated();
        // Make one origin non-simple (template-flavored): its copied
        // mappings must follow the legacy per-part cloning, while every
        // other origin keeps its single table entry. Pick the origin whose
        // first appearance is latest among copied-referenced origins, so
        // its appended clones cannot disturb the first-appearance order
        // the token-table extraction requires.
        let mut first_seen: std::collections::HashMap<u32, usize> =
            std::collections::HashMap::new();
        for (index, mapping) in input.mappings.iter().enumerate() {
            if let Some(id) = mapping.origin {
                first_seen.entry(id).or_insert(index);
            }
        }
        let target = input
            .mappings
            .iter()
            .filter(|m| m.copied.is_some())
            .filter_map(|m| m.origin)
            .max_by_key(|id| first_seen[id])
            .unwrap();
        let target_parts = input
            .mappings
            .iter()
            .filter(|m| m.origin == Some(target) && m.copied.is_some())
            .count();
        assert!(target_parts > 0);
        // A derived origin (source attribution kept, transformation and
        // reason recorded) is the valid non-simple shape with a primary.
        let derived = input.origins[target as usize].derived("template");
        input.origins[target as usize] = derived;
        let (project, link) = identity_link(source);
        let composed = compose(&project, &link, &code, &input).unwrap();
        // Only the non-simple origin's parts were cloned; no blanket
        // synthetic origin is appended (the identity link leaves no gaps).
        assert_eq!(composed.origins.len(), input.origins.len() + target_parts);
        let map = crate::optimized_source_map::encode_owned_for_options(
            &code,
            composed,
            &CompileOptions::default(),
            None,
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_str(&map).unwrap();
        // The simple part reaches the mixed token-table form and the whole
        // map still validates.
        assert!(!json["x_storm"]["tokens"].as_array().unwrap().is_empty());
        crate::optimized_source_map::validate(&code, &map).unwrap();
    }

    #[test]
    fn multi_module_compose_uses_multi_source_token_table() {
        let main = "local _ENV=_ENV function onTick()output.setNumber(1,2)end\n";
        let helper = "function onInput()output.setNumber(2,3)end";
        let linked = format!("{main}{helper}");
        let (code, compact) =
            storm_lua_syntax::print::lexical_minify_with_compact_origins("lexical.lua", &linked)
                .unwrap();
        let input = compact.into_generated();
        let project = LuaProject {
            entry: "main".into(),
            modules: BTreeMap::from([
                ("main".to_string(), main.to_string()),
                ("helper".to_string(), helper.to_string()),
            ]),
            ambient: BTreeMap::new(),
        };
        let link = LinkResult {
            diagnostics: Vec::new(),
            linked_source: Some(linked.clone()),
            used_modules: vec!["main".to_string(), "helper".to_string()],
            ranges: vec![
                crate::link::LinkedRange {
                    output_start_line: 0,
                    output_end_line: 1,
                    module: "main".to_string(),
                    source_start_line: 0,
                    output_start_byte: 0,
                    output_end_byte: main.len(),
                    source_start_byte: 0,
                },
                crate::link::LinkedRange {
                    output_start_line: 1,
                    output_end_line: 2,
                    module: "helper".to_string(),
                    source_start_line: 0,
                    output_start_byte: main.len(),
                    output_end_byte: main.len() + helper.len(),
                    source_start_byte: 0,
                },
            ],
            injected_ambient: BTreeMap::new(),
        };
        let composed = compose(&project, &link, &code, &input).unwrap();
        assert_eq!(composed.sources.len(), 2);
        assert_eq!(composed.origins.len(), input.origins.len());
        let map = crate::optimized_source_map::encode_owned_for_options(
            &code,
            composed,
            &CompileOptions::default(),
            None,
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_str(&map).unwrap();
        let tokens = json["x_storm"]["tokens"].as_array().unwrap();
        assert!(!tokens.is_empty());
        // Multi-source layout: exact copies carry 5 elements, renamed
        // tokens 6, and both source indexes appear.
        let mut seen_sources = std::collections::BTreeSet::new();
        for record in tokens {
            let record = record.as_array().unwrap();
            assert!(record.len() == 5 || record.len() == 6);
            seen_sources.insert(record[1].as_i64().unwrap());
        }
        assert_eq!(seen_sources.len(), 2);
        let validated = crate::optimized_source_map::validate(&code, &map).unwrap();
        assert!(validated
            .origins
            .iter()
            .all(|origin| origin.primary.is_some_and(|span| span.source < 2)));
    }
}
