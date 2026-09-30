//! Standard Source Map v3 plus the versioned Storm optimization explanation.
//! This layer owns encoding and content fingerprints, not optimization semantics.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sourcemap::SourceMapBuilder;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use storm_lua_minify::search::SearchStats;
use storm_lua_minify::{CompileMode, CompileOptions, NumericMode, SearchMode};
use storm_lua_syntax::explanation::{
    InlineContext, OptimizationReason, RelationRole, SourceDisposition, SourceRelation,
};
use storm_lua_syntax::provenance::{
    GeneratedConstruct, GeneratedOrigins, Origin, OriginKind, OriginMapping, OriginPrecision,
    SourceSnapshot, SourceSpan,
};
use storm_lua_syntax::{lexer::Lexer, source_position::LineIndex};

/// Encoding version of x_storm, independent of both Source Map and engine versions.
pub const SCHEMA_VERSION: u32 = 1;
/// Producer identity captured at build time. Unknown source revisions stay explicit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MapProducer {
    /// Tool identity, not the source file name.
    pub name: String,
    /// Cargo workspace version used to build the encoder.
    pub version: String,
    /// Git revision, or "unknown" for a source archive without revision metadata.
    pub revision: String,
    /// Whether build-time tracked source modifications were present, if known.
    pub dirty: Option<bool>,
}
/// Content identity of one UTF-8 source buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BufferIdentity {
    /// Exact number of UTF-8 bytes, including newlines.
    pub bytes: usize,
    /// Lowercase SHA-256 hex digest.
    pub sha256: String,
}
/// One origin with pooled reason identifiers to avoid repeating large explanations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MapOrigin {
    /// Source, explicit derivation or compiler-created code.
    pub kind: OriginKind,
    /// Primary source range; never invented for generated control code.
    pub primary: Option<SourceSpan>,
    /// Ordered indexes into MapExtension.relations; pooled to avoid repeated spans.
    pub related: Vec<u32>,
    /// Granularity of the primary attribution.
    pub precision: OriginPrecision,
    /// Original identifier spelling, if present at this exact source occurrence.
    pub name: Option<Arc<str>>,
    /// Last explicitly recorded transformation; use reasons for retained prior actions.
    pub transformation: Option<Arc<str>>,
    /// Indexes into MapExtension.reasons in recorded order.
    pub reasons: Vec<u32>,
}
/// Recorded removal/replacement, with pooled reason and source-level alternatives.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MapDisposition {
    /// Original affected construct, not a runtime stop site.
    pub original: SourceSpan,
    /// Index into MapExtension.reasons.
    pub reason: u32,
    /// Original ranges of explicitly known replacements.
    pub replacement_sources: Vec<SourceSpan>,
}
/// Storm-owned explanation embedded as the x_storm property of a standard map.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MapExtension {
    /// Version of this extension, never the engine release version.
    pub schema_version: u32,
    /// Compiler identity.
    pub producer: MapProducer,
    /// Unit of all spans in this extension; standard mappings use UTF-16 columns.
    pub coordinate_unit: String,
    /// Identity of the exact generated Lua.
    pub generated: BufferIdentity,
    /// Identities aligned with the standard sources/sourcesContent arrays.
    pub sources: Vec<BufferIdentity>,
    /// Actual resolved settings and winner-selection facts. Numeric inputs use bit strings.
    pub compilation: Value,
    /// Detailed source/derived/synthetic origins.
    pub origins: Vec<MapOrigin>,
    /// Deduplicated structured actions recorded on the surviving candidate.
    pub reasons: Vec<OptimizationReason>,
    /// Shared typed relationships, referenced by each origin's related indexes.
    pub relations: Vec<SourceRelation>,
    /// Enclosing inline expansion contexts.
    pub contexts: Vec<InlineContext>,
    /// Most-specific nonoverlapping ranges; explicit unknowns remain unmapped.
    pub mappings: Vec<OriginMapping>,
    /// Enclosing construct intervals; these alone do not imply executable locations.
    pub constructs: Vec<GeneratedConstruct>,
    /// Explicit selected-candidate removals or replacements.
    pub dispositions: Vec<MapDisposition>,
    /// Digest of canonical map JSON with this field replaced by the empty string.
    /// Detects mismatched data; this is not an authenticated digital signature.
    pub integrity: String,
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn identity(text: &str) -> BufferIdentity {
    BufferIdentity {
        bytes: text.len(),
        sha256: digest(text.as_bytes()),
    }
}
fn producer() -> MapProducer {
    MapProducer {
        name: "storm-lua-engine".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        revision: option_env!("STORM_LUA_SOURCE_REVISION")
            .unwrap_or("unknown")
            .into(),
        dirty: match option_env!("STORM_LUA_SOURCE_DIRTY") {
            Some("true") => Some(true),
            Some("false") => Some(false),
            _ => None,
        },
    }
}
fn reason_id(
    reason: &OptimizationReason,
    table: &mut Vec<OptimizationReason>,
    ids: &mut HashMap<OptimizationReason, u32>,
) -> u32 {
    *ids.entry(reason.clone()).or_insert_with(|| {
        let id = table.len() as u32;
        table.push(reason.clone());
        id
    })
}
fn extension(code: &str, origins: &GeneratedOrigins, compilation: Value) -> MapExtension {
    let mut reasons = Vec::new();
    let mut ids = HashMap::new();
    let mut relation_table = Vec::new();
    let mut relation_ids = HashMap::new();
    let records = origins
        .origins
        .iter()
        .map(|origin| {
            let mut related = origin.relations.as_ref().clone();
            let mut present = related
                .iter()
                .map(|r| r.span)
                .collect::<std::collections::HashSet<_>>();
            for &span in origin.related.iter() {
                if present.insert(span) {
                    related.push(SourceRelation {
                        role: RelationRole::Contribution,
                        span,
                    });
                }
            }
            MapOrigin {
                kind: origin.kind,
                primary: origin.primary,
                related: related
                    .into_iter()
                    .map(|relation| {
                        *relation_ids.entry(relation.clone()).or_insert_with(|| {
                            let id = relation_table.len() as u32;
                            relation_table.push(relation);
                            id
                        })
                    })
                    .collect(),
                precision: origin.precision,
                name: origin.name.clone(),
                transformation: origin.transformation.clone(),
                reasons: origin
                    .reasons
                    .iter()
                    .map(|r| reason_id(r, &mut reasons, &mut ids))
                    .collect(),
            }
        })
        .collect();
    let mut seen = BTreeSet::new();
    let dispositions = origins
        .dispositions
        .iter()
        .filter_map(|d| {
            let reason = reason_id(&d.reason, &mut reasons, &mut ids);
            let key = (d.original.source, d.original.start, d.original.end, reason);
            seen.insert(key).then(|| MapDisposition {
                original: d.original,
                reason,
                replacement_sources: d.replacement_sources.clone(),
            })
        })
        .collect();
    MapExtension {
        schema_version: SCHEMA_VERSION,
        producer: producer(),
        coordinate_unit: "utf8-bytes".into(),
        generated: identity(code),
        sources: origins.sources.iter().map(|s| identity(&s.text)).collect(),
        compilation,
        origins: records,
        reasons,
        relations: relation_table,
        contexts: origins.contexts.clone(),
        mappings: origins.mappings.clone(),
        constructs: origins.constructs.clone(),
        dispositions,
        integrity: String::new(),
    }
}
fn numeric(value: f64) -> String {
    format!("{:016x}", value.to_bits())
}
/// Resolved, deterministic compilation assumptions. No wall-clock timings or loser logs.
pub fn compilation_settings(options: &CompileOptions, stats: Option<&SearchStats>) -> Value {
    let folding = storm_lua_minify::config::resolve_folding_tolerance(options.numeric_tolerance);
    let literal = storm_lua_minify::config::resolve_literal_tolerance(options.numeric_tolerance);
    let resolved =
        storm_lua_minify::config::resolve_pass_toggles(&options.pass_toggles, options.numeric_mode);
    let effective_passes = storm_lua_minify::pass_ids::OPTIMIZATION_PASS_IDS
        .iter()
        .map(|&id| (id, storm_lua_minify::config::pass_enabled(&resolved, id)))
        .collect::<BTreeMap<_, _>>();
    let property=options.property.as_ref().map(|p|json!({"mode":match p.mode {storm_lua_minify::config::PropertyMode::Runtime=>"runtime",storm_lua_minify::config::PropertyMode::Hardcode=>"hardcode"},
        "numbers":p.numbers.as_ref().map(|m|m.iter().map(|(k,v)|(k,numeric(*v))).collect::<BTreeMap<_,_>>()),"bools":p.bools,"texts":p.texts}));
    let selection=stats.map(|s|json!({"criterion":if s.winner_structural=="lexical" {"conservative-token-only"} else if s.stopped_early {"valid-candidate-meeting-target"} else {"smallest-valid-evaluated-candidate"},
        "structural":s.winner_structural,"layout":s.winner_layout,"targetMet":s.target_met,"stoppedEarly":s.stopped_early}));
    json!({"environment":options.environment,"hostBindings":options.host_bindings,
        "mode":if options.mode==CompileMode::Safe {"safe"} else {"smallest"},
        "numericMode":if options.numeric_mode==NumericMode::Exact {"exact"}else{"tolerant"},
        "numericEncoding":"ieee754-binary64-hex","numericTolerance":options.numeric_tolerance.map(|t|json!({"abs":numeric(t.abs),"rel":numeric(t.rel)})),
        "effectiveNumericTolerances":{"constantFolding":{"abs":numeric(folding.abs),"rel":numeric(folding.rel)},"literalApproximationBeforeCaps":{"abs":numeric(literal.abs),"rel":numeric(literal.rel)}},
        "property":property,"passToggles":options.pass_toggles,"effectivePassToggles":effective_passes,
        "searchMode":if options.search_mode==SearchMode::Fast {"fast"}else{"exhaustive"},"searchBeamWidth":options.search_beam_width,
        "targetSize":options.target_size,"zeroCostNewlines":options.zero_cost_newlines,"selection":selection})
}

/// Encode final positions and optimization explanations as one standard-compatible map.
/// Copied intervals get token/line anchors; other intervals retain their declared precision.
pub fn encode(
    code: &str,
    origins: &GeneratedOrigins,
    compilation: Value,
) -> Result<String, String> {
    origins.validate_for_code(code)?;
    let mut map = standard_map(code, origins)?;
    map["x_storm"] =
        serde_json::to_value(extension(code, origins, compilation)).map_err(|e| e.to_string())?;
    let integrity = digest(&serde_json::to_vec(&map).map_err(|e| e.to_string())?);
    map["x_storm"]["integrity"] = Value::String(integrity);
    serde_json::to_string(&map).map_err(|e| e.to_string())
}
// One producer for the standard view. Validation compares that view with the
// detailed ranges, rather than trusting a checksum recomputed over mismatches.
fn standard_map(code: &str, origins: &GeneratedOrigins) -> Result<Value, String> {
    let generated = LineIndex::new(code);
    let indexes = origins
        .sources
        .iter()
        .map(|s| LineIndex::new(&s.text))
        .collect::<Vec<_>>();
    let mut builder = SourceMapBuilder::new(None);
    // Temporary unique labels avoid merging two snapshots with the same display name.
    for (id, source) in origins.sources.iter().enumerate() {
        let actual = builder.add_source(&format!("source-{id}"));
        if actual as usize != id {
            return Err("source map source ordering changed".into());
        }
        builder.set_source_contents(actual, Some(&source.text));
    }
    let mut names = HashMap::new();
    for origin in &origins.origins {
        if let Some(name) = &origin.name {
            names
                .entry(name.as_ref())
                .or_insert_with(|| builder.add_name(name));
        }
    }
    let mut anchors = BTreeSet::from([0, code.len()]);
    anchors.extend(
        code.bytes()
            .enumerate()
            .filter_map(|(i, b)| (b == b'\n').then_some(i + 1)),
    );
    for mapping in &origins.mappings {
        anchors.insert(mapping.start);
        anchors.insert(mapping.end);
    }
    anchors.extend(
        Lexer::new(code)
            .all()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|t| t.p),
    );
    let mut current = 0;
    for byte in anchors {
        while current < origins.mappings.len() && origins.mappings[current].end <= byte {
            current += 1;
        }
        let mapping = origins
            .mappings
            .get(current)
            .filter(|m| m.start <= byte && byte < m.end);
        let original = mapping.and_then(|m| {
            let origin = m.origin.map(|i| &origins.origins[i as usize])?;
            if let Some(copy) = m.copied {
                Some((
                    copy.source,
                    copy.start + byte - m.start,
                    origin.name.as_deref(),
                ))
            } else {
                origin
                    .primary
                    .map(|s| (s.source, s.start, origin.name.as_deref()))
            }
        });
        let (line, col) = generated
            .utf16_position(byte)
            .ok_or("invalid generated anchor")?;
        if let Some((source, offset, name)) = original {
            let (src_line, src_col) = indexes[source as usize]
                .utf16_position(offset)
                .ok_or("invalid original anchor")?;
            builder.add_raw(
                line,
                col,
                src_line,
                src_col,
                Some(source),
                name.map(|n| names[n]),
                false,
            );
        } else {
            builder.add_raw(line, col, 0, 0, None, None, false);
        }
    }
    let mut bytes = Vec::new();
    builder
        .into_sourcemap()
        .to_writer(&mut bytes)
        .map_err(|e| e.to_string())?;
    let mut map: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    map["sources"] = json!(origins
        .sources
        .iter()
        .map(|s| s.name.as_ref())
        .collect::<Vec<_>>());
    map["sourcesContent"] = json!(origins
        .sources
        .iter()
        .map(|s| s.text.as_ref())
        .collect::<Vec<_>>());
    Ok(map)
}

/// Reject mismatched code, corrupted extension data or an unsupported schema.
/// This checks integrity, not authenticity: an author can intentionally create a new map.
pub fn validate(code: &str, map: &str) -> Result<MapExtension, String> {
    let mut json: Value = serde_json::from_str(map).map_err(|e| e.to_string())?;
    if json["version"] != 3 {
        return Err("unsupported Source Map version".into());
    }
    let details: MapExtension = serde_json::from_value(json["x_storm"].clone())
        .map_err(|e| format!("invalid x_storm extension: {e}"))?;
    if details.schema_version != SCHEMA_VERSION || details.coordinate_unit != "utf8-bytes" {
        return Err("unsupported Storm source map schema".into());
    }
    if details.producer.name != "storm-lua-engine"
        || details.producer.version.is_empty()
        || details.producer.revision.is_empty()
    {
        return Err("source map has no producer identity".into());
    }
    json["x_storm"]["integrity"] = Value::String(String::new());
    if digest(&serde_json::to_vec(&json).map_err(|e| e.to_string())?) != details.integrity {
        return Err("source map integrity mismatch".into());
    }
    if details.generated.bytes != code.len() || details.generated.sha256 != digest(code.as_bytes())
    {
        return Err("source map does not belong to this generated code".into());
    }
    let names = json["sources"].as_array().ok_or("missing source names")?;
    let contents = json["sourcesContent"]
        .as_array()
        .ok_or("missing source snapshots")?;
    if names.len() != contents.len() || names.len() != details.sources.len() {
        return Err("source map snapshot count mismatch".into());
    }
    let mut snapshots = Vec::with_capacity(names.len());
    for ((name, text), expected) in names.iter().zip(contents).zip(&details.sources) {
        let text = text.as_str().ok_or("missing original source content")?;
        if expected.bytes != text.len() || expected.sha256 != digest(text.as_bytes()) {
            return Err("original source fingerprint mismatch".into());
        }
        snapshots.push(SourceSnapshot {
            name: name.as_str().ok_or("invalid source name")?.into(),
            text: text.into(),
        });
    }
    for reason in &details.reasons {
        reason.validate()?;
    }
    for relation in &details.relations {
        let span = relation.span;
        let snapshot = snapshots
            .get(span.source as usize)
            .ok_or("invalid relation source reference")?;
        if snapshot.text.get(span.start..span.end).is_none() {
            return Err("invalid typed relation range".into());
        }
    }
    let mut origins = Vec::new();
    for record in &details.origins {
        let reasons = record
            .reasons
            .iter()
            .map(|&id| {
                details
                    .reasons
                    .get(id as usize)
                    .cloned()
                    .ok_or("invalid reason reference".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        if reasons.iter().any(|r| r.code.is_empty()) {
            return Err("empty optimization reason code".into());
        }
        let relations = record
            .related
            .iter()
            .map(|id| {
                details
                    .relations
                    .get(*id as usize)
                    .cloned()
                    .ok_or("invalid typed relation reference".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut related = Vec::new();
        for r in &relations {
            if record.primary != Some(r.span) && !related.contains(&r.span) {
                related.push(r.span);
            }
        }
        origins.push(Origin {
            kind: record.kind,
            primary: record.primary,
            related: Arc::new(related),
            precision: record.precision,
            name: record.name.clone(),
            transformation: record.transformation.clone(),
            relations: Arc::new(relations),
            reasons: Arc::new(reasons),
        });
    }
    let dispositions = details
        .dispositions
        .iter()
        .map(|d| {
            Ok(SourceDisposition {
                original: d.original,
                reason: details
                    .reasons
                    .get(d.reason as usize)
                    .ok_or("invalid disposition reason")?
                    .clone(),
                replacement_sources: d.replacement_sources.clone(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let generated = GeneratedOrigins {
        sources: Arc::new(snapshots),
        origins,
        mappings: details.mappings.clone(),
        contexts: details.contexts.clone(),
        constructs: details.constructs.clone(),
        dispositions: Arc::new(dispositions),
    };
    generated.validate_for_code(code)?;
    if json.get("sourceRoot").is_some_and(|v| v != "") || json.get("sections").is_some() {
        return Err("unsupported standard source root or indexed map for this extension".into());
    }
    let expected = standard_map(code, &generated)?;
    for key in ["mappings", "names"] {
        if json.get(key) != expected.get(key) {
            return Err(format!(
                "standard {key} do not agree with the detailed origins"
            ));
        }
    }
    // Verify that the standard part is independently decodable, not just JSON.
    sourcemap::SourceMap::from_slice(map.as_bytes())
        .map_err(|e| format!("invalid standard source map: {e}"))?;
    Ok(details)
}
