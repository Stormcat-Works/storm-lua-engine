//! Standard Source Map v3 plus the versioned Storm optimization explanation.
//! This layer owns encoding and content fingerprints, not optimization semantics.
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::json;
use serde_json::Value;
use sha2::{Digest, Sha256};
#[cfg(test)]
use sourcemap::SourceMapBuilder;
#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use storm_lua_minify::search::SearchStats;
use storm_lua_minify::CompileOptions;
#[cfg(test)]
use storm_lua_minify::{CompileMode, NumericMode, SearchMode};
use storm_lua_syntax::explanation::{
    InlineContext, OptimizationReason, RelationRole, SourceDisposition, SourceRelation,
};
use storm_lua_syntax::provenance::{
    GeneratedConstruct, GeneratedOrigins, LexicalCopiedOrigins, Origin, OriginKind, OriginMapping,
    OriginPrecision, SourceSnapshot, SourceSpan,
};
use storm_lua_syntax::{lexer::Lexer, source_position::LineIndex};

/// Fast deterministic hasher for the output stage's interning tables only.
///
/// The tables are keyed by full values (reasons, relations, spans) and are
/// never iterated for output: the serialized tables keep insertion order in
/// separate `Vec`s, and every lookup is verified by `Eq`. The hasher therefore
/// only affects lookup speed, never the output bytes or integrity values.
/// It hashes content, never pointer identity: deserialized or freshly built
/// reasons with equal content must share one ID regardless of allocation.
/// This is a multiply-xor (FxHash-style) hasher, chosen over the default
/// `RandomState` (SipHash) because these keys are short, numerous, and
/// internal to one compilation; no new dependency is introduced.
#[derive(Default)]
struct FastHasher {
    hash: u64,
}

impl FastHasher {
    fn add(&mut self, value: u64) {
        self.hash = (self.hash.rotate_left(5) ^ value).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl std::hash::Hasher for FastHasher {
    fn finish(&self) -> u64 {
        self.hash
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for chunk in chunks.by_ref() {
            let mut value = 0u64;
            for (i, byte) in chunk.iter().enumerate() {
                value |= (*byte as u64) << (i * 8);
            }
            self.add(value);
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut buf = [0u8; 8];
            buf[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(buf));
        }
    }

    fn write_u8(&mut self, n: u8) {
        self.add(n as u64);
    }

    fn write_u16(&mut self, n: u16) {
        self.add(n as u64);
    }

    fn write_u32(&mut self, n: u32) {
        self.add(n as u64);
    }

    fn write_u64(&mut self, n: u64) {
        self.add(n);
    }

    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }
}

type FastBuildHasher = std::hash::BuildHasherDefault<FastHasher>;
type FastMap<K, V> = HashMap<K, V, FastBuildHasher>;
type FastSet<T> = std::collections::HashSet<T, FastBuildHasher>;

/// Encoding version of x_storm, independent of both Source Map and engine versions.
pub const SCHEMA_VERSION: u32 = 2;
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
    /// Schema-2 token table: first-appearance name spellings, used only when
    /// `tokens` is non-empty. Empty for general maps and for lexical-shape
    /// maps that fall back to the materialized layout.
    pub token_names: Vec<Arc<str>>,
    /// Schema-2 token table: `[generated_start, source_start, source_end,
    /// name_id]` per token, with `-1` for a non-name token. Unknown gaps
    /// between tokens are omitted on the wire and restored by `validate`.
    /// Token records. Four elements `[generated_start, source_start,
    /// source_end, name_id]` describe an exact copy (the generated span
    /// length equals the source span length). Five elements append
    /// `generated_end` and describe a renamed token: the origin still names
    /// the original spelling at its source span, but the generated span has
    /// its own length and carries no exact-copy attribution (`copied: None`
    /// on expansion). Any other length is rejected by expansion.
    ///
    /// When the map has more than one source, records instead carry an
    /// explicit source index as the second element:
    /// `[generated_start, source, source_start, source_end, name_id]` for
    /// an exact copy, plus a sixth `generated_end` element for a renamed
    /// token. The layout is selected by the source count, so single-source
    /// records keep the original meaning (in particular a five-element
    /// record in a single-source map is always a renamed token) and
    /// multi-source tables are rejected by readers that only support the
    /// single-source layout. Any other length is rejected by expansion.
    ///
    /// When `origins`/`mappings` are also non-empty alongside a token
    /// table, the map is in mixed form: the arrays carry only the residual
    /// (non-simple) origins and their mappings, and expansion splices the
    /// residual mappings into the unknown gaps of the token expansion (see
    /// `expand_token_table` and `validate`).
    pub tokens: Vec<Vec<i64>>,
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
    ids: &mut FastMap<OptimizationReason, u32>,
) -> u32 {
    if let Some(&id) = ids.get(reason) {
        return id;
    }
    *ids.entry(reason.clone()).or_insert_with(|| {
        let id = table.len() as u32;
        table.push(reason.clone());
        id
    })
}
fn extension(code: &str, origins: &GeneratedOrigins, compilation: Value) -> MapExtension {
    let mut reasons = Vec::new();
    let mut ids: FastMap<OptimizationReason, u32> = FastMap::default();
    let mut relation_table = Vec::new();
    let mut relation_ids: FastMap<SourceRelation, u32> = FastMap::default();
    let mut present: FastSet<SourceSpan> = FastSet::default();
    let records = origins
        .origins
        .iter()
        .map(|origin| {
            let mut related = origin.relations.as_ref().clone();
            if !origin.related.is_empty() {
                present.clear();
                present.extend(related.iter().map(|r| r.span));
                for &span in origin.related.iter() {
                    if present.insert(span) {
                        related.push(SourceRelation {
                            role: RelationRole::Contribution,
                            span,
                        });
                    }
                }
            }
            MapOrigin {
                kind: origin.kind,
                primary: origin.primary,
                related: related
                    .into_iter()
                    .map(|relation| match relation_ids.get(&relation) {
                        Some(&id) => id,
                        None => {
                            let id = relation_table.len() as u32;
                            relation_table.push(relation.clone());
                            relation_ids.insert(relation, id);
                            id
                        }
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
            seen.insert(key).then_some(MapDisposition {
                original: d.original,
                reason,
                replacement_sources: d.replacement_sources.clone(),
            })
        })
        .collect();
    let generated = identity(code);
    let sources = origins
        .sources
        .iter()
        .map(|s| {
            if s.text.as_ref() == code {
                generated.clone()
            } else {
                identity(&s.text)
            }
        })
        .collect();
    let source_texts: Vec<&str> = origins
        .sources
        .iter()
        .map(|snapshot| snapshot.text.as_ref())
        .collect();
    let mut extension = MapExtension {
        schema_version: SCHEMA_VERSION,
        producer: producer(),
        coordinate_unit: "utf8-bytes".into(),
        generated,
        sources,
        compilation,
        origins: records,
        reasons,
        relations: relation_table,
        contexts: origins.contexts.clone(),
        mappings: origins.mappings.clone(),
        constructs: origins.constructs.clone(),
        dispositions,
        token_names: Vec::new(),
        tokens: Vec::new(),
        integrity: String::new(),
    };
    extract_token_table(&mut extension, &source_texts);
    extension
}
fn extension_owned(code: &str, origins: GeneratedOrigins, compilation: Value) -> MapExtension {
    let GeneratedOrigins {
        sources,
        origins,
        mappings,
        contexts,
        constructs,
        dispositions,
    } = origins;
    let mut reasons = Vec::new();
    let mut ids: FastMap<OptimizationReason, u32> = FastMap::default();
    let mut relation_table = Vec::new();
    let mut relation_ids: FastMap<SourceRelation, u32> = FastMap::default();
    let mut present: FastSet<SourceSpan> = FastSet::default();
    let records = origins
        .into_iter()
        .map(|origin| {
            let mut related = Arc::unwrap_or_clone(origin.relations);
            if !origin.related.is_empty() {
                present.clear();
                present.extend(related.iter().map(|r| r.span));
                for span in origin.related.iter().copied() {
                    if present.insert(span) {
                        related.push(SourceRelation {
                            role: RelationRole::Contribution,
                            span,
                        });
                    }
                }
            }
            MapOrigin {
                kind: origin.kind,
                primary: origin.primary,
                related: related
                    .into_iter()
                    .map(|relation| match relation_ids.get(&relation) {
                        Some(&id) => id,
                        None => {
                            let id = relation_table.len() as u32;
                            relation_table.push(relation.clone());
                            relation_ids.insert(relation, id);
                            id
                        }
                    })
                    .collect(),
                precision: origin.precision,
                name: origin.name,
                transformation: origin.transformation,
                reasons: origin
                    .reasons
                    .iter()
                    .map(|r| reason_id(r, &mut reasons, &mut ids))
                    .collect(),
            }
        })
        .collect();
    let mut seen = BTreeSet::new();
    let dispositions = Arc::unwrap_or_clone(dispositions)
        .into_iter()
        .filter_map(|d| {
            let reason = reason_id(&d.reason, &mut reasons, &mut ids);
            let key = (d.original.source, d.original.start, d.original.end, reason);
            seen.insert(key).then_some(MapDisposition {
                original: d.original,
                reason,
                replacement_sources: d.replacement_sources,
            })
        })
        .collect();
    let generated = identity(code);
    let source_identities = sources
        .iter()
        .map(|s| {
            if s.text.as_ref() == code {
                generated.clone()
            } else {
                identity(&s.text)
            }
        })
        .collect();
    let source_texts: Vec<&str> = sources
        .iter()
        .map(|snapshot| snapshot.text.as_ref())
        .collect();
    let mut extension = MapExtension {
        schema_version: SCHEMA_VERSION,
        producer: producer(),
        coordinate_unit: "utf8-bytes".into(),
        generated,
        sources: source_identities,
        compilation,
        origins: records,
        reasons,
        relations: relation_table,
        contexts,
        mappings,
        constructs,
        dispositions,
        token_names: Vec::new(),
        tokens: Vec::new(),
        integrity: String::new(),
    };
    extract_token_table(&mut extension, &source_texts);
    extension
}

/// Upper bound on the residual (non-simple) origin count of a mixed-form
/// token table. The mixed form exists for maps that are simple except for
/// a bounded amount of synthetic or template material (for example a
/// lifeboat preamble); general optimized maps stay in the full layout.
const MAX_MIXED_RESIDUAL_ORIGINS: usize = 256;

/// Whether an origin can be represented by the token table: a source
/// origin with a Name/Token precision, an optional name matching its
/// precision, and no transformation, related spans or reasons (token-table
/// expansion reconstructs exactly this shape).
fn token_table_simple_origin(origin: &MapOrigin) -> bool {
    origin.kind == OriginKind::Source
        && origin.primary.is_some()
        && matches!(
            origin.precision,
            OriginPrecision::Name | OriginPrecision::Token
        )
        && origin.transformation.is_none()
        && origin.related.is_empty()
        && origin.reasons.is_empty()
        && (origin.precision == OriginPrecision::Name) == origin.name.is_some()
}

/// Split a map whose origins are mostly token-table-simple into the mixed
/// form: simple origins and their mappings move into the token table, the
/// remaining (residual) origins keep the full layout with remapped
/// references, and reasons/relations pools stay shared. Every condition
/// mirrors a check the reader performs on expansion or merge, so the
/// emitted map always validates against the same generated code and
/// sources; when any condition fails the extension is left untouched.
fn extract_token_table(extension: &mut MapExtension, source_texts: &[&str]) {
    if !extension.contexts.is_empty()
        || !extension.constructs.is_empty()
        || !extension.dispositions.is_empty()
    {
        return;
    }
    if extension.origins.is_empty()
        || extension.mappings.is_empty()
        || extension.sources.is_empty()
        || extension.sources.len() != source_texts.len()
    {
        return;
    }
    let multi_source = extension.sources.len() > 1;
    let simple: Vec<bool> = extension
        .origins
        .iter()
        .map(token_table_simple_origin)
        .collect();
    let simple_count = simple.iter().filter(|&&flag| flag).count();
    let residual_count = simple.len() - simple_count;
    if simple_count == 0
        || residual_count > MAX_MIXED_RESIDUAL_ORIGINS
        || residual_count > simple_count
    {
        return;
    }
    // Simple tuples must be unique (the reader deduplicates by tuple and
    // would silently merge distinct origins), the name of a Name origin
    // must match its primary source slice (the reader rejects mismatches),
    // and primary source references must be in range.
    let mut seen_tuples: std::collections::HashSet<(u32, usize, usize, Option<String>, bool)> =
        std::collections::HashSet::new();
    for (origin, &is_simple) in extension.origins.iter().zip(&simple) {
        if !is_simple {
            continue;
        }
        let Some(primary) = origin.primary else {
            return;
        };
        let Some(text) = source_texts.get(primary.source as usize) else {
            return;
        };
        if let Some(name) = &origin.name {
            if text.get(primary.start..primary.end) != Some(name.as_ref()) {
                return;
            }
        }
        if !seen_tuples.insert((
            primary.source,
            primary.start,
            primary.end,
            origin.name.as_deref().map(str::to_owned),
            origin.precision == OriginPrecision::Name,
        )) {
            return;
        }
    }
    // Name pool in simple-origin order, as the reader reconstructs it.
    let mut name_ids: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut token_names: Vec<Arc<str>> = Vec::new();
    for (origin, &is_simple) in extension.origins.iter().zip(&simple) {
        if is_simple {
            if let Some(name) = &origin.name {
                if !name_ids.contains_key(name.as_ref()) {
                    name_ids.insert(name.as_ref(), token_names.len());
                    token_names.push(Arc::clone(name));
                }
            }
        }
    }
    // Residual origin ids in original order.
    let mut residual_ids: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for (index, &is_simple) in simple.iter().enumerate() {
        if !is_simple {
            residual_ids.insert(index as u32, residual_ids.len() as u32);
        }
    }
    let mut tokens: Vec<Vec<i64>> = Vec::new();
    let mut residual_mappings: Vec<OriginMapping> = Vec::new();
    let mut first_order: Vec<u32> = Vec::new();
    let mut seen_simple: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut cursor = 0usize;
    for mapping in &extension.mappings {
        if mapping.start != cursor || mapping.end < mapping.start {
            return;
        }
        cursor = mapping.end;
        if !mapping.inline_contexts.is_empty() {
            return;
        }
        let Some(id) = mapping.origin else {
            if mapping.copied.is_some() {
                return;
            }
            // Unknown mappings stay residual; the merge splices them into
            // the expansion gaps, which reconstructs them identically.
            residual_mappings.push(mapping.clone());
            continue;
        };
        let Some(origin) = extension.origins.get(id as usize) else {
            return;
        };
        if !simple[id as usize] {
            let mut mapping = mapping.clone();
            mapping.origin = Some(residual_ids[&id]);
            residual_mappings.push(mapping);
            continue;
        }
        let Some(primary) = origin.primary else {
            return;
        };
        let name_id = match &origin.name {
            Some(name) => name_ids[name.as_ref()] as i64,
            None => -1,
        };
        match mapping.copied {
            Some(copied) => {
                if Some(copied) != origin.primary
                    || mapping.end - mapping.start != copied.end - copied.start
                {
                    return;
                }
                if multi_source {
                    tokens.push(vec![
                        mapping.start as i64,
                        copied.source as i64,
                        copied.start as i64,
                        copied.end as i64,
                        name_id,
                    ]);
                } else {
                    tokens.push(vec![
                        mapping.start as i64,
                        copied.start as i64,
                        copied.end as i64,
                        name_id,
                    ]);
                }
            }
            None => {
                if origin.precision != OriginPrecision::Name {
                    return;
                }
                if multi_source {
                    tokens.push(vec![
                        mapping.start as i64,
                        primary.source as i64,
                        primary.start as i64,
                        primary.end as i64,
                        name_id,
                        mapping.end as i64,
                    ]);
                } else {
                    tokens.push(vec![
                        mapping.start as i64,
                        primary.start as i64,
                        primary.end as i64,
                        name_id,
                        mapping.end as i64,
                    ]);
                }
            }
        }
        if seen_simple.insert(id) {
            first_order.push(id);
        }
    }
    if cursor != extension.generated.bytes || tokens.is_empty() {
        return;
    }
    // Every simple origin must be referenced (expansion only creates
    // referenced origins) and first-appearance order must equal the
    // relative origin order (expansion renumbers in appearance order).
    let simple_in_origin_order: Vec<u32> = (0..extension.origins.len() as u32)
        .filter(|&index| simple[index as usize])
        .collect();
    if first_order != simple_in_origin_order {
        return;
    }
    // Standard names are pooled in the original origin order, but mixed
    // expansion reconstructs the simple origins first and appends the
    // residual origins. Only extract when that regrouping preserves the
    // unique-name sequence; otherwise the expanded origins would pool the
    // same names in a different order and the standard mappings' name IDs
    // would no longer agree with the wire form.
    let mut original_names: Vec<&str> = Vec::new();
    for origin in &extension.origins {
        if let Some(name) = origin.name.as_deref() {
            if !original_names.contains(&name) {
                original_names.push(name);
            }
        }
    }
    let mut expanded_names: Vec<&str> = Vec::new();
    for (origin, &is_simple) in extension.origins.iter().zip(&simple) {
        if is_simple {
            if let Some(name) = origin.name.as_deref() {
                if !expanded_names.contains(&name) {
                    expanded_names.push(name);
                }
            }
        }
    }
    for (origin, &is_simple) in extension.origins.iter().zip(&simple) {
        if !is_simple {
            if let Some(name) = origin.name.as_deref() {
                if !expanded_names.contains(&name) {
                    expanded_names.push(name);
                }
            }
        }
    }
    if original_names != expanded_names {
        return;
    }
    let residual_origins: Vec<MapOrigin> = extension
        .origins
        .iter()
        .zip(&simple)
        .filter(|(_, &is_simple)| !is_simple)
        .map(|(origin, _)| origin.clone())
        .collect();
    extension.origins = residual_origins;
    extension.mappings = residual_mappings;
    extension.token_names = token_names;
    extension.tokens = tokens;
}
#[cfg(test)]
fn numeric(value: f64) -> String {
    format!("{:016x}", value.to_bits())
}
/// Resolved, deterministic compilation assumptions. No wall-clock timings or loser logs.
#[expect(
    clippy::expect_used,
    reason = "Compilation metadata serializes only strings, integer ranges, booleans, options and IEEE bit strings; no fallible custom format or nonfinite JSON number"
)]
pub fn compilation_settings(options: &CompileOptions, stats: Option<&SearchStats>) -> Value {
    serde_json::to_value(crate::map_settings::Settings { options, stats })
        .expect("serializable compiler settings")
}

#[cfg(test)]
fn reference_compilation_settings(options: &CompileOptions, stats: Option<&SearchStats>) -> Value {
    let folding = storm_lua_minify::config::resolve_folding_tolerance(options.numeric_tolerance);
    let literal = storm_lua_minify::config::resolve_literal_tolerance(options.numeric_tolerance);
    let resolved =
        storm_lua_minify::config::resolve_pass_toggles(&options.pass_toggles, options.numeric_mode);
    let effective_passes = storm_lua_minify::pass_ids::OPTIMIZATION_PASS_IDS
        .iter()
        .map(|&id| {
            (
                id.to_owned(),
                Value::Bool(storm_lua_minify::config::pass_enabled(&resolved, id)),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let property=options.property.as_ref().map(|p|json!({"mode":match p.mode {storm_lua_minify::config::PropertyMode::Runtime=>"runtime",storm_lua_minify::config::PropertyMode::Hardcode=>"hardcode"},
        "numbers":p.numbers.as_ref().map(|m|m.iter().map(|(k,v)|(k,numeric(*v))).collect::<BTreeMap<_,_>>()),"bools":p.bools,"texts":p.texts}));
    let selection=stats.map(|s|json!({"criterion":if s.winner_structural=="lexical" {"conservative-token-only"} else if s.stopped_early {"valid-candidate-meeting-target"} else {"smallest-valid-evaluated-candidate"},
        "structural":s.winner_structural,"layout":s.winner_layout,"targetMet":s.target_met,"stoppedEarly":s.stopped_early}));
    let mut settings = json!({"environment":options.environment,"hostBindings":options.host_bindings,
        "mode":if options.mode==CompileMode::Safe {"safe"} else {"smallest"},
        "numericMode":if options.numeric_mode==NumericMode::Exact {"exact"}else{"tolerant"},
        "numericEncoding":"ieee754-binary64-hex","numericTolerance":options.numeric_tolerance.map(|t|json!({"abs":numeric(t.abs),"rel":numeric(t.rel)})),
        "effectiveNumericTolerances":{"constantFolding":{"abs":numeric(folding.abs),"rel":numeric(folding.rel)},"literalApproximationBeforeCaps":{"abs":numeric(literal.abs),"rel":numeric(literal.rel)}},
        "property":property,"passToggles":options.pass_toggles,
        "searchMode":if options.search_mode==SearchMode::Fast {"fast"}else{"exhaustive"},"searchBeamWidth":options.search_beam_width,
        "targetSize":options.target_size,"zeroCostNewlines":options.zero_cost_newlines,"selection":selection});
    settings["effectivePassToggles"] = Value::Object(effective_passes);
    settings
}

/// Encode final positions and optimization explanations as one standard-compatible map.
/// Copied intervals get token/line anchors; other intervals retain their declared precision.
pub fn encode(
    code: &str,
    origins: &GeneratedOrigins,
    compilation: Value,
) -> Result<String, String> {
    origins.validate_for_code(code)?;
    let map = standard_fields(code, origins)?;
    crate::canonical_map::encode(&map, extension(code, origins, compilation))
}
/// Compiler-owned path writes resolved settings directly into final map JSON.
/// The public Value-based encode API remains available for arbitrary metadata.
pub(crate) fn encode_for_options(
    code: &str,
    origins: &GeneratedOrigins,
    options: &CompileOptions,
    stats: Option<&SearchStats>,
) -> Result<String, String> {
    origins.validate_for_code(code)?;
    let standard = standard_fields(code, origins)?;
    crate::canonical_map::encode_with_settings(
        &standard,
        extension(code, origins, Value::Null),
        &crate::map_settings::Settings { options, stats },
    )
}
/// Compiler-owned path consumes provenance discarded after final map encoding.
/// Moving final arrays avoids cloning every mapping/context/construct immediately
/// before serializing exactly the same schema-1 JSON.
pub(crate) fn encode_owned_for_options(
    code: &str,
    origins: GeneratedOrigins,
    options: &CompileOptions,
    stats: Option<&SearchStats>,
) -> Result<String, String> {
    let simple_copies = lexical_copy_only(code, &origins);
    if simple_copies {
        debug_assert!(origins.validate_for_code(code).is_ok());
    } else {
        origins.validate_for_code(code)?;
    }
    let standard = standard_fields_with_copy_hint(code, &origins, simple_copies)?;
    if simple_copies {
        let generated = identity(code);
        let sources = origins
            .sources
            .iter()
            .map(|source| {
                if source.text.as_ref() == code {
                    generated.clone()
                } else {
                    identity(&source.text)
                }
            })
            .collect::<Vec<_>>();
        let producer = producer();
        return crate::canonical_map::encode_simple_copy_with_settings(
            &standard,
            &producer,
            &generated,
            &sources,
            &origins.origins,
            &origins.mappings,
            &crate::map_settings::Settings { options, stats },
        );
    }
    crate::canonical_map::encode_with_settings(
        &standard,
        extension_owned(code, origins, Value::Null),
        &crate::map_settings::Settings { options, stats },
    )
}
// One producer for the standard view. Validation compares that view with the
// detailed ranges, rather than trusting a checksum recomputed over mismatches.
// Standard-map anchors are already unique and sorted by generated position.
// Encode their deltas directly instead of collecting/sorting another token
// arena, serializing a temporary JSON object, and immediately parsing it back.
// The independent SourceMapBuilder reference below checks exact v3 output.
#[derive(Default)]
struct MappingEncoder {
    output: String,
    previous: [u32; 5],
    line: u32,
    emitted: bool,
}
impl MappingEncoder {
    fn push(&mut self, line: u32, column: u32, source: Option<(u32, u32, u32, Option<u32>)>) {
        if line != self.line {
            for _ in self.line..line {
                self.output.push(';');
            }
            self.line = line;
            self.previous[0] = 0;
        } else if self.emitted {
            self.output.push(',');
        }
        self.delta(0, column);
        if let Some((source, line, column, name)) = source {
            self.delta(1, source);
            self.delta(2, line);
            self.delta(3, column);
            if let Some(name) = name {
                self.delta(4, name);
            }
        }
        self.emitted = true;
    }
    fn delta(&mut self, index: usize, value: u32) {
        let signed = i64::from(value) - i64::from(self.previous[index]);
        self.previous[index] = value;
        write_vlq(&mut self.output, signed);
    }
}
fn write_vlq(output: &mut String, value: i64) {
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    // Deltas are between u32 coordinates/IDs, so this shift cannot overflow.
    let mut bits = (value.unsigned_abs() << 1) | u64::from(value < 0);
    loop {
        let part = (bits & 31) as usize;
        bits >>= 5;
        output.push(DIGITS[part | if bits == 0 { 0 } else { 32 }] as char);
        if bits == 0 {
            break;
        }
    }
}

// Canonical field order matches the prior sorted Value object. Snapshot text
// remains borrowed until it is written into the final JSON, without copying it
// into a temporary Value tree first.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StandardFields {
    mappings: String,
    names: Vec<Arc<str>>,
    sources: Vec<Arc<str>>,
    sources_content: Vec<Arc<str>>,
    version: u32,
}
fn lexical_copy_only(code: &str, origins: &GeneratedOrigins) -> bool {
    origins.contexts.is_empty()
        && origins.constructs.is_empty()
        && origins.dispositions.is_empty()
        && origins.origins.iter().all(|origin| {
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
        })
        && origins.mappings.iter().all(|mapping| match mapping.origin {
            None => {
                mapping.copied.is_none()
                    && mapping.inline_contexts.is_empty()
                    && code
                        .get(mapping.start..mapping.end)
                        .is_some_and(|gap| gap.bytes().all(|byte| byte.is_ascii_whitespace()))
            }
            Some(id) => {
                let origin = origins.origins.get(id as usize);
                mapping.inline_contexts.is_empty()
                    && match (mapping.copied, origin) {
                        (Some(copied), Some(origin)) => origin.primary == Some(copied),
                        // Renamed token: no exact copy, but the origin still
                        // names the original spelling at its primary span.
                        // The token-table gate carries the generated end
                        // explicitly for this shape.
                        (None, Some(origin)) => {
                            origin.precision == OriginPrecision::Name
                                && origin.name.as_ref().is_some_and(|name| {
                                    origin.primary.is_some_and(|primary| {
                                        origins.sources.get(primary.source as usize).and_then(
                                            |snapshot| {
                                                snapshot.text.get(primary.start..primary.end)
                                            },
                                        ) == Some(name.as_ref())
                                    })
                                })
                        }
                        _ => false,
                    }
            }
        })
}

fn standard_fields_from_lexical(
    code: &str,
    lexical: &LexicalCopiedOrigins,
) -> Result<StandardFields, String> {
    lexical.validate_for_code(code)?;
    let generated = LineIndex::new(code);
    let source_same = lexical.source_text.as_ref() == code;
    let source_index = (!source_same).then(|| LineIndex::new(&lexical.source_text));
    let mut generated_cursor = generated.utf16_cursor();
    let mut source_cursor = source_index.as_ref().map(LineIndex::utf16_cursor);

    let mut names = HashMap::<&str, u32>::new();
    let mut name_values = Vec::new();
    for token in &lexical.tokens {
        if token.is_name {
            let text = &lexical.source_text[token.source_start..token.source_end];
            names.entry(text).or_insert_with(|| {
                let id = name_values.len() as u32;
                name_values.push(Arc::<str>::from(text));
                id
            });
        }
    }

    let mut mappings = MappingEncoder::default();
    let mut boundaries = lexical
        .tokens
        .iter()
        .flat_map(|token| [token.start, token.end()])
        .peekable();
    let mut lines = code
        .bytes()
        .enumerate()
        .filter_map(|(i, b)| (b == b'\n').then_some(i + 1))
        .peekable();
    let mut byte = 0;
    let mut current = 0;
    loop {
        while current < lexical.tokens.len() && lexical.tokens[current].end() <= byte {
            current += 1;
        }
        let token = lexical
            .tokens
            .get(current)
            .filter(|token| token.start <= byte && byte < token.end());
        let (line, col) = generated_cursor
            .position(byte)
            .ok_or("invalid generated lexical anchor")?;
        if let Some(token) = token {
            let offset = token.source_start + byte - token.start;
            let (src_line, src_col) = if source_same && offset == byte {
                (line, col)
            } else {
                source_cursor
                    .as_mut()
                    .ok_or("compact lexical source cursor is unavailable")?
                    .position(offset)
                    .ok_or("invalid compact lexical source anchor")?
            };
            let name_id = token.is_name.then(|| {
                let text = &lexical.source_text[token.source_start..token.source_end];
                names[text]
            });
            mappings.push(line, col, Some((0, src_line, src_col, name_id)));
        } else {
            mappings.push(line, col, None);
        }
        if byte == code.len() {
            break;
        }
        while boundaries.peek().is_some_and(|&position| position <= byte) {
            boundaries.next();
        }
        while lines.peek().is_some_and(|&position| position <= byte) {
            lines.next();
        }
        byte = boundaries
            .peek()
            .copied()
            .unwrap_or(code.len())
            .min(lines.peek().copied().unwrap_or(code.len()));
    }

    Ok(StandardFields {
        mappings: mappings.output,
        names: name_values,
        sources: vec![Arc::clone(&lexical.source_name)],
        sources_content: vec![Arc::clone(&lexical.source_text)],
        version: 3,
    })
}

#[doc(hidden)]
pub fn encode_compact_lexical_for_options(
    code: &str,
    lexical: &LexicalCopiedOrigins,
    options: &CompileOptions,
    stats: Option<&SearchStats>,
) -> Result<String, String> {
    let standard = standard_fields_from_lexical(code, lexical)?;
    let generated = identity(code);
    let source = if lexical.source_text.as_ref() == code {
        generated.clone()
    } else {
        identity(&lexical.source_text)
    };
    let producer = producer();
    crate::canonical_map::encode_lexical_copy_with_settings(
        &standard,
        &producer,
        &generated,
        &[source],
        lexical,
        &crate::map_settings::Settings { options, stats },
    )
}

fn standard_fields(code: &str, origins: &GeneratedOrigins) -> Result<StandardFields, String> {
    standard_fields_with_copy_hint(code, origins, lexical_copy_only(code, origins))
}

fn standard_fields_with_copy_hint(
    code: &str,
    origins: &GeneratedOrigins,
    simple_copies: bool,
) -> Result<StandardFields, String> {
    let generated = LineIndex::new(code);
    let indexes = origins
        .sources
        .iter()
        .map(|s| (s.text.as_ref() != code).then(|| LineIndex::new(&s.text)))
        .collect::<Vec<_>>();
    let mut generated_cursor = generated.utf16_cursor();
    let mut copied_cursor = generated.utf16_cursor();
    let mut source_cursors = indexes
        .iter()
        .map(|index| index.as_ref().map(LineIndex::utf16_cursor))
        .collect::<Vec<_>>();
    // Deserialized origins do not retain Arc interning. Equal spellings must
    // share one standard name ID regardless of their allocation identity.
    let mut names = HashMap::new();
    let mut name_values = Vec::new();
    for origin in &origins.origins {
        if let Some(name) = &origin.name {
            names.entry(name.as_ref()).or_insert_with(|| {
                let id = name_values.len() as u32;
                name_values.push(Arc::clone(name));
                id
            });
        }
    }
    let mut mappings = MappingEncoder::default();
    let mut boundaries = origins
        .mappings
        .iter()
        .flat_map(|m| [m.start, m.end])
        .peekable();
    let mut lines = code
        .bytes()
        .enumerate()
        .filter_map(|(i, b)| (b == b'\n').then_some(i + 1))
        .peekable();
    // Pure lexical-copy maps already contain every token boundary as a mapping
    // boundary. Re-lexing the generated source only rediscovers anchors we have.
    let mut lexer = (!simple_copies).then(|| Lexer::new(code));
    let mut token = if let Some(lexer) = lexer.as_mut() {
        let token = lexer.next_borrowed().map_err(|e| e.to_string())?;
        (token.k, token.p)
    } else {
        (storm_lua_syntax::lexer::TokenKind::Eof, code.len())
    };
    let mut byte = 0;
    let mut current = 0;
    loop {
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
        let (line, col) = generated_cursor
            .position(byte)
            .ok_or("invalid generated anchor")?;
        if let Some((source, offset, name)) = original {
            let (src_line, src_col) = if indexes[source as usize].is_none() && offset == byte {
                (line, col)
            } else {
                source_cursors[source as usize]
                    .as_mut()
                    .unwrap_or(&mut copied_cursor)
                    .position(offset)
                    .ok_or("invalid original anchor")?
            };
            let name_id = name.map(|name| names[name]);
            mappings.push(line, col, Some((source, src_line, src_col, name_id)));
        } else {
            mappings.push(line, col, None);
        }
        if byte == code.len() {
            break;
        }
        while boundaries.peek().is_some_and(|&p| p <= byte) {
            boundaries.next();
        }
        while lines.peek().is_some_and(|&p| p <= byte) {
            lines.next();
        }
        while token.0 != storm_lua_syntax::lexer::TokenKind::Eof && token.1 <= byte {
            let Some(lexer) = lexer.as_mut() else {
                return Err("source-map lexer state is unavailable".into());
            };
            let next = lexer.next_borrowed().map_err(|e| e.to_string())?;
            token = (next.k, next.p);
        }
        // All streams are sorted by construction; repeated boundaries are
        // removed as they are consumed, with no sort or anchor allocation.
        byte = token
            .1
            .min(boundaries.peek().copied().unwrap_or(code.len()))
            .min(lines.peek().copied().unwrap_or(code.len()));
    }
    Ok(StandardFields {
        mappings: mappings.output,
        names: name_values,
        sources: origins
            .sources
            .iter()
            .map(|s| Arc::clone(&s.name))
            .collect(),
        sources_content: origins
            .sources
            .iter()
            .map(|s| Arc::clone(&s.text))
            .collect(),
        version: 3,
    })
}
#[cfg(test)]
fn standard_map(code: &str, origins: &GeneratedOrigins) -> Result<Value, String> {
    serde_json::to_value(standard_fields(code, origins)?).map_err(|e| e.to_string())
}

/// Expand a schema-2 token table into the materialized mappings and origins
/// that schema-1 maps carried directly. The expansion is order-preserving:
/// origins are deduplicated by (source, primary span, name, precision) in
/// first-appearance order, and unknown gaps are restored from the cursor
/// walk, exactly as the lexical-copy writer emits them. The caller must
/// already have checked the integrity digest over the wire form.
///
/// The record layout is selected by the source count (see the
/// `MapExtension::tokens` field documentation): single-source maps use the
/// original layout without a source index, maps with more than one source
/// carry the source index as the second element. In both layouts a final
/// extra element marks a renamed token and carries its generated end.
fn expand_token_table(
    details: &MapExtension,
    snapshots: &[SourceSnapshot],
    generated_len: usize,
) -> Result<(Vec<OriginMapping>, Vec<MapOrigin>), String> {
    if details.sources.is_empty() || snapshots.len() != details.sources.len() {
        return Err("token table source snapshots are unavailable".into());
    }
    let multi_source = details.sources.len() > 1;
    let mut origins: Vec<MapOrigin> = Vec::new();
    let mut origin_ids: HashMap<(u32, usize, usize, Option<String>, bool), u32> = HashMap::new();
    let mut mappings: Vec<OriginMapping> = Vec::new();
    let mut cursor = 0usize;
    for record in &details.tokens {
        // A final extra element marks a renamed token and carries its
        // generated end explicitly; without it the generated end is
        // derived from the source length, which is only valid for exact
        // copies.
        let base_len = if multi_source { 5 } else { 4 };
        let renamed = match record.len() {
            len if len == base_len => false,
            len if len == base_len + 1 => true,
            _ => return Err("token table record has an unsupported length".into()),
        };
        let (generated_start, source, source_start, source_end, name_id) = if multi_source {
            (record[0], record[1], record[2], record[3], record[4])
        } else {
            (record[0], 0, record[1], record[2], record[3])
        };
        if generated_start < 0 || source < 0 || source_start < 0 || source_end < 0 {
            return Err("token table has a negative coordinate".into());
        }
        let (generated_start, source_start, source_end) = (
            generated_start as usize,
            source_start as usize,
            source_end as usize,
        );
        let source = source as usize;
        let source_text = snapshots
            .get(source)
            .map(|snapshot| snapshot.text.as_ref())
            .ok_or("token table has an invalid source reference")?;
        if generated_start < cursor {
            return Err("token table is not in generated order".into());
        }
        if source_start >= source_end || source_end > source_text.len() {
            return Err("token table has an invalid source range".into());
        }
        let generated_end = if renamed {
            let end = record[base_len];
            if end < 0 {
                return Err("token table has a negative coordinate".into());
            }
            end as usize
        } else {
            generated_start
                .checked_add(source_end - source_start)
                .ok_or("token table generated end overflow")?
        };
        if generated_end <= generated_start {
            return Err("token table has an invalid generated range".into());
        }
        if generated_end > generated_len {
            return Err("token table exceeds the generated code".into());
        }
        if cursor < generated_start {
            mappings.push(OriginMapping {
                start: cursor,
                end: generated_start,
                origin: None,
                copied: None,
                inline_contexts: Vec::new(),
            });
        }
        let name: Option<Arc<str>> = if name_id < 0 {
            None
        } else {
            Some(
                details
                    .token_names
                    .get(name_id as usize)
                    .cloned()
                    .ok_or("token table has an invalid name reference")?,
            )
        };
        if let Some(name) = &name {
            if source_text.get(source_start..source_end) != Some(name.as_ref()) {
                return Err("token table name does not match the source slice".into());
            }
        }
        let is_name = name.is_some();
        let primary = SourceSpan {
            source: source as u32,
            start: source_start,
            end: source_end,
        };
        let key = (
            source as u32,
            source_start,
            source_end,
            name.as_deref().map(str::to_owned),
            is_name,
        );
        let id = *origin_ids.entry(key).or_insert_with(|| {
            let id = origins.len() as u32;
            origins.push(MapOrigin {
                kind: OriginKind::Source,
                primary: Some(primary),
                related: Vec::new(),
                precision: if is_name {
                    OriginPrecision::Name
                } else {
                    OriginPrecision::Token
                },
                name: name.clone(),
                transformation: None,
                reasons: Vec::new(),
            });
            id
        });
        mappings.push(OriginMapping {
            start: generated_start,
            end: generated_end,
            origin: Some(id),
            // Renamed tokens are not exact copies: the origin names the
            // original spelling, but the generated spelling differs, so no
            // exact-copy attribution is recorded (mirrors the materialized
            // form produced by `GeneratedOrigins::copied_tokens`).
            copied: if renamed { None } else { Some(primary) },
            inline_contexts: Vec::new(),
        });
        cursor = generated_end;
    }
    if cursor < generated_len {
        mappings.push(OriginMapping {
            start: cursor,
            end: generated_len,
            origin: None,
            copied: None,
            inline_contexts: Vec::new(),
        });
    }
    Ok((mappings, origins))
}

/// Merge the token-table expansion with the residual (non-simple) part of
/// a mixed-form map. Residual mappings must lie entirely inside single
/// unknown gaps of the expansion and must not overlap each other; gaps are
/// split around them. The merged origin list is the expanded simple
/// origins followed by the residual origins, so residual mapping origin
/// references are offset by the simple origin count. Anything else is
/// rejected (this only adds restrictions on top of the pure token-table
/// expansion; it never relaxes a check).
fn merge_token_and_residual(
    token_mappings: Vec<OriginMapping>,
    token_origins: Vec<MapOrigin>,
    residual_mappings: Vec<OriginMapping>,
    residual_origins: Vec<MapOrigin>,
) -> Result<(Vec<OriginMapping>, Vec<MapOrigin>), String> {
    let offset = token_origins.len() as u32;
    let mut merged: Vec<OriginMapping> = Vec::new();
    let mut residual = residual_mappings.into_iter().peekable();
    for segment in token_mappings {
        if segment.origin.is_some() {
            merged.push(segment);
            continue;
        }
        // Unknown gap: splice the residual mappings contained in it.
        let mut cursor = segment.start;
        while residual
            .peek()
            .is_some_and(|m| m.start >= segment.start && m.end <= segment.end)
        {
            let Some(mut mapping) = residual.next() else {
                break;
            };
            if mapping.start < cursor {
                return Err("mixed token table residual mappings overlap".into());
            }
            if cursor < mapping.start {
                merged.push(OriginMapping {
                    start: cursor,
                    end: mapping.start,
                    origin: None,
                    copied: None,
                    inline_contexts: Vec::new(),
                });
            }
            if let Some(id) = mapping.origin {
                mapping.origin = Some(
                    id.checked_add(offset)
                        .ok_or("mixed token table origin reference overflow")?,
                );
            }
            cursor = mapping.end;
            merged.push(mapping);
        }
        if cursor < segment.end {
            merged.push(OriginMapping {
                start: cursor,
                end: segment.end,
                origin: None,
                copied: None,
                inline_contexts: Vec::new(),
            });
        }
    }
    if residual.next().is_some() {
        return Err("mixed token table residual mapping is outside the token gaps".into());
    }
    let mut origins = token_origins;
    origins.extend(residual_origins);
    Ok((merged, origins))
}

/// Reject mismatched code, corrupted extension data or an unsupported schema.
/// This checks integrity, not authenticity: an author can intentionally create a new map.
pub fn validate(code: &str, map: &str) -> Result<MapExtension, String> {
    let mut json: Value = serde_json::from_str(map).map_err(|e| e.to_string())?;
    if json["version"] != 3 {
        return Err("unsupported Source Map version".into());
    }
    let mut details: MapExtension = serde_json::from_value(json["x_storm"].clone())
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
    if !details.tokens.is_empty() || !details.token_names.is_empty() {
        if details.tokens.is_empty() {
            return Err("token names without a token table".into());
        }
        let (token_mappings, token_origins) =
            expand_token_table(&details, &snapshots, details.generated.bytes)?;
        if details.mappings.is_empty() && details.origins.is_empty() {
            details.mappings = token_mappings;
            details.origins = token_origins;
        } else {
            // Mixed form: the wire arrays carry only the residual
            // (non-simple) origins and mappings; splice them into the
            // unknown gaps of the token expansion.
            let residual_mappings = std::mem::take(&mut details.mappings);
            let residual_origins = std::mem::take(&mut details.origins);
            let (mappings, origins) = merge_token_and_residual(
                token_mappings,
                token_origins,
                residual_mappings,
                residual_origins,
            )?;
            details.mappings = mappings;
            details.origins = origins;
        }
        // The returned extension is the normalized materialized form:
        // the token table is consumed by the expansion above, so a
        // re-encode of the returned value emits the full layout and
        // validates to exactly the same details.
        details.tokens = Vec::new();
        details.token_names = Vec::new();
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
    let expected = standard_fields(code, &generated)?;
    if json.get("mappings").and_then(Value::as_str) != Some(expected.mappings.as_str()) {
        return Err("standard mappings do not agree with the detailed origins".into());
    }
    if !json
        .get("names")
        .and_then(Value::as_array)
        .is_some_and(|names| {
            names.len() == expected.names.len()
                && names
                    .iter()
                    .zip(&expected.names)
                    .all(|(a, b)| a.as_str() == Some(b.as_ref()))
        })
    {
        return Err("standard names do not agree with the detailed origins".into());
    }
    // Verify that the standard part is independently decodable, not just JSON.
    sourcemap::SourceMap::from_slice(map.as_bytes())
        .map_err(|e| format!("invalid standard source map: {e}"))?;
    Ok(details)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod contiguous_anchor_tests {
    use super::*;
    use storm_lua_syntax::{parse_source_with_origins, Printer};
    fn reference_standard_map(code: &str, origins: &GeneratedOrigins) -> Result<Value, String> {
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
                .all_borrowed()
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

    fn compare(code: &str, origins: &GeneratedOrigins) {
        origins.validate_for_code(code).unwrap();
        assert_eq!(
            standard_map(code, origins).unwrap(),
            reference_standard_map(code, origins).unwrap()
        );
        let map = encode(
            code,
            origins,
            json!({"integrity":"", "text":"\"integrity\":\"\""}),
        )
        .unwrap();
        validate(code, &map).unwrap();
    }
    #[test]
    fn contiguous_anchors_match_the_previous_ordered_set_and_source_contents() {
        for source in [
            "",
            "-- only comment
",
            "-- 雪😀\r\nfunction onTick()output.setNumber(1,2*3)end\r\n",
            "local _ENV=_ENV
function onDraw()screen.drawText(0,0,'雪😀')end",
            "function f(x)return x+1 end
function onTick()output.setNumber(1,f(2))end",
        ] {
            let identity = GeneratedOrigins::identity("same.lua", source);
            compare(source, &identity);
            let (code, copied) =
                storm_lua_syntax::print::lexical_minify_with_origins("same.lua", source).unwrap();
            compare(&code, &copied);
            let (ast, root) = parse_source_with_origins("same.lua", source).unwrap();
            for newlines in [false, true] {
                let printed = Printer::new(&ast, newlines).output_with_positions(root);
                let mapped = GeneratedOrigins::from_print(&ast, &printed).unwrap();
                compare(&printed.code, &mapped);
            }
            let mut two = identity.clone();
            Arc::make_mut(&mut two.sources).push(SourceSnapshot {
                name: "same.lua".into(),
                text: "different snapshot".into(),
            });
            compare(source, &two);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod direct_mapping_tests {
    use super::*;
    #[test]
    fn vlq_matches_independent_library_at_coordinate_and_continuation_boundaries() {
        for value in [
            -(u32::MAX as i64),
            -65536,
            -4096,
            -1024,
            -512,
            -32,
            -31,
            -16,
            -15,
            -1,
            0,
            1,
            15,
            16,
            31,
            32,
            511,
            512,
            1024,
            4096,
            65535,
            u32::MAX as i64,
        ] {
            let mut actual = String::new();
            write_vlq(&mut actual, value);
            assert_eq!(
                actual,
                sourcemap::vlq::generate_vlq_segment(&[value]).unwrap()
            );
        }
    }
    #[test]
    fn direct_delta_stream_matches_library_for_unknowns_names_and_sparse_lines() {
        let mut builder = SourceMapBuilder::new(None);
        let mut direct = MappingEncoder::default();
        for name in ["one", "two", "three"] {
            builder.add_source(name);
            builder.add_name(name);
        }
        let mut line = 0;
        let mut column = 0;
        for i in 0u32..2000 {
            if i % 11 == 0 {
                // The production caller provides sorted unique anchors. A
                // reset column must move to a later generated line as well.
                line += 1 + i % 4;
                column = 0;
            }
            column += 1 + i % 37;
            let original = if i % 5 == 0 {
                None
            } else {
                Some((
                    i % 3,
                    (i * 19) % 127,
                    (i * 31) % 8192,
                    (i % 7 != 0).then_some(i % 3),
                ))
            };
            direct.push(line, column, original);
            match original {
                Some((src, sl, sc, n)) => {
                    builder.add_raw(line, column, sl, sc, Some(src), n, false);
                }
                None => {
                    builder.add_raw(line, column, 0, 0, None, None, false);
                }
            }
        }
        let mut bytes = Vec::new();
        builder.into_sourcemap().to_writer(&mut bytes).unwrap();
        let expected: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(direct.output, expected["mappings"].as_str().unwrap());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod borrowed_settings_tests {
    use super::*;
    use storm_lua_minify::config::{NumericTolerance, PropertyConfig, PropertyMode};
    #[test]
    fn borrowed_settings_preserve_value_and_canonical_bytes_including_numeric_bits() {
        let base =
            storm_lua_minify::search::compile_code("return 1", &CompileOptions::default()).unwrap();
        for n in 0..48 {
            let mut options = CompileOptions {
                mode: if n % 2 == 0 {
                    CompileMode::Safe
                } else {
                    CompileMode::Smallest
                },
                numeric_mode: if n % 3 == 0 {
                    NumericMode::Exact
                } else {
                    NumericMode::Tolerant
                },
                numeric_tolerance: if n % 4 == 0 {
                    None
                } else {
                    Some(NumericTolerance {
                        abs: if n % 2 == 0 { -0.0 } else { 1e-8 },
                        rel: 1e-13,
                    })
                },
                search_mode: if n % 5 == 0 {
                    SearchMode::Fast
                } else {
                    SearchMode::Exhaustive
                },
                target_size: (n % 3 == 0).then_some(n * 1024),
                search_beam_width: 1 + n as u32 % 16,
                zero_cost_newlines: n % 2 != 0,
                ..CompileOptions::default()
            };
            if n % 3 != 0 {
                options.host_bindings = vec!["user.value".into(), "custom".into()];
            }
            options.pass_toggles = storm_lua_minify::pass_ids::OPTIMIZATION_PASS_IDS
                .iter()
                .enumerate()
                .filter(|(i, _)| i % 5 == n % 5)
                .map(|(i, &id)| (id, i % 2 == 0))
                .collect();
            if n % 4 != 0 {
                options.property = Some(PropertyConfig {
                    mode: if n % 2 == 0 {
                        PropertyMode::Hardcode
                    } else {
                        PropertyMode::Runtime
                    },
                    numbers: Some(BTreeMap::from([
                        ("negativeZero".into(), -0.0),
                        ("infinity".into(), f64::INFINITY),
                        ("nanPayload".into(), f64::from_bits(0x7ff800000000002a)),
                        ("普通😀".into(), 1.23),
                    ])),
                    bools: Some(BTreeMap::from([("toggle".into(), true)])),
                    texts: Some(BTreeMap::from([(
                        "name".into(),
                        "\"integrity\":\"\"雪😀\r\n".into(),
                    )])),
                });
            }
            let mut stats = base.stats.clone();
            stats.winner_structural = if n % 3 == 0 {
                "lexical".into()
            } else {
                "same-candidate".into()
            };
            stats.stopped_early = n % 2 == 0;
            stats.target_met = n % 4 == 0;
            for stats in [None, Some(&stats)] {
                let expected = reference_compilation_settings(&options, stats);
                assert_eq!(compilation_settings(&options, stats), expected);
                assert_eq!(
                    serde_json::to_string(&crate::map_settings::Settings {
                        options: &options,
                        stats
                    })
                    .unwrap(),
                    serde_json::to_string(&expected).unwrap()
                );
                let code = "return 1";
                let origins = GeneratedOrigins::identity("settings.lua", code);
                let expected_map = encode(code, &origins, expected).unwrap();
                let actual = encode_for_options(code, &origins, &options, stats).unwrap();
                assert_eq!(
                    actual, expected_map,
                    "canonical full map including fingerprint changed"
                );
                validate(code, &actual).unwrap();
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod compact_lexical_map_tests {
    use super::*;

    #[test]
    fn compact_lexical_encoding_matches_materialized_provenance_exactly() {
        let repeated = "output.setNumber(1,value) ".repeat(128);
        let cases = [
            String::new(),
            "-- comment only\r\n".to_string(),
            "local _ENV=_ENV -- 雪😀\r\nfunction onTick()output.setNumber(1,2)end\r\n".to_string(),
            "local x='雪😀' return x,x,x\n".to_string(),
            repeated,
        ];
        for source in cases {
            let options = CompileOptions {
                origin_source: Some("lexical.lua".into()),
                ..CompileOptions::default()
            };
            let (code, compact) = storm_lua_syntax::print::lexical_minify_with_compact_origins(
                "lexical.lua",
                &source,
            )
            .unwrap();
            let materialized = compact.clone().into_generated();
            let expected = encode_owned_for_options(&code, materialized, &options, None).unwrap();
            let actual =
                encode_compact_lexical_for_options(&code, &compact, &options, None).unwrap();
            assert_eq!(actual, expected, "compact lexical map bytes changed");
        }
    }

    #[test]
    fn malformed_compact_ranges_are_rejected_without_indexing() {
        let (code, lexical) = storm_lua_syntax::print::lexical_minify_with_compact_origins(
            "input.lua",
            "local _ENV=_ENV return '雪😀'",
        )
        .unwrap();
        for (start, end) in [(usize::MAX, 2), (0, usize::MAX), (1, 2)] {
            let mut invalid = lexical.clone();
            invalid.tokens[0].source_start = start;
            invalid.tokens[0].source_end = end;
            assert!(encode_compact_lexical_for_options(
                &code,
                &invalid,
                &CompileOptions::default(),
                None
            )
            .is_err());
        }
        let mut invalid = lexical.clone();
        invalid.tokens.reverse();
        assert!(encode_compact_lexical_for_options(
            &code,
            &invalid,
            &CompileOptions::default(),
            None
        )
        .is_err());
        let mut invalid = lexical;
        invalid.tokens[0].start = usize::MAX;
        assert!(encode_compact_lexical_for_options(
            &code,
            &invalid,
            &CompileOptions::default(),
            None
        )
        .is_err());
    }

    #[test]
    fn unknown_code_retains_standard_token_anchors() {
        let code = "return 1";
        let mut unknown = GeneratedOrigins::identity("unknown.lua", code);
        unknown.origins.clear();
        for mapping in &mut unknown.mappings {
            mapping.origin = None;
            mapping.copied = None;
        }
        let map = encode(code, &unknown, serde_json::Value::Null).unwrap();
        let json: Value = serde_json::from_str(&map).unwrap();
        assert_eq!(json["mappings"], "A,O,C");
        let details = validate(code, &map).unwrap();
        assert!(details
            .mappings
            .iter()
            .all(|mapping| mapping.origin.is_none()));
    }

    #[test]
    fn schema_two_token_table_expands_to_materialized_provenance() {
        let source = "local _ENV=_ENV function onTick()output.setNumber(1,2)end";
        let (code, compact) =
            storm_lua_syntax::print::lexical_minify_with_compact_origins("lexical.lua", source)
                .unwrap();
        let materialized = compact.clone().into_generated();
        let map =
            encode_compact_lexical_for_options(&code, &compact, &CompileOptions::default(), None)
                .unwrap();
        let json: Value = serde_json::from_str(&map).unwrap();
        let extension = &json["x_storm"];
        assert_eq!(extension["schemaVersion"], 2);
        assert!(extension["mappings"].as_array().unwrap().is_empty());
        assert!(extension["origins"].as_array().unwrap().is_empty());
        assert!(!extension["tokens"].as_array().unwrap().is_empty());
        let details = validate(&code, &map).unwrap();
        assert_eq!(details.schema_version, 2);
        assert_eq!(details.mappings.len(), materialized.mappings.len());
        assert_eq!(details.origins.len(), materialized.origins.len());
        for (expanded, original) in details.mappings.iter().zip(&materialized.mappings) {
            assert_eq!(expanded.start, original.start);
            assert_eq!(expanded.end, original.end);
            assert_eq!(expanded.origin, original.origin);
            assert_eq!(expanded.copied, original.copied);
        }
        // A schema-1 wire version is rejected even before the digest check.
        let mut old = json.clone();
        old["x_storm"]["schemaVersion"] = serde_json::json!(1);
        assert!(validate(&code, &old.to_string()).is_err());
    }

    #[test]
    fn derived_named_origin_never_emits_a_mixed_map_with_reordered_names() {
        let source = "return a+b";
        let (code, base) =
            storm_lua_syntax::print::lexical_minify_with_origins("example.lua", source).unwrap();
        let name_index = |origins: &GeneratedOrigins, name: &str| {
            origins
                .origins
                .iter()
                .position(|origin| origin.name.as_deref() == Some(name))
                .unwrap()
        };

        // Deriving the first named origin makes it residual. Mixed
        // expansion would append it after the token-table origin for `b`,
        // changing the standard name pool from [a, b] to [b, a], so the
        // encoder must keep the full layout for this shape.
        let mut first = base.clone();
        let a = name_index(&first, "a");
        first.origins[a] = first.origins[a].derived("example-rewrite");
        let map = encode(&code, &first, Value::Null).unwrap();
        let json: Value = serde_json::from_str(&map).unwrap();
        assert!(json["x_storm"]["tokens"].as_array().unwrap().is_empty());
        validate(&code, &map).unwrap();

        // Deriving the last named origin leaves the name sequence intact:
        // the mixed form remains available and validates.
        let mut last = base;
        let b = name_index(&last, "b");
        last.origins[b] = last.origins[b].derived("example-rewrite");
        let map = encode(&code, &last, Value::Null).unwrap();
        let json: Value = serde_json::from_str(&map).unwrap();
        assert!(!json["x_storm"]["tokens"].as_array().unwrap().is_empty());
        assert_eq!(json["x_storm"]["tokenNames"], json!(["a"]));
        validate(&code, &map).unwrap();
    }

    #[test]
    fn large_lexical_maps_validate_after_names_are_deserialized() {
        let source = format!(
            "local _ENV=_ENV function onTick() {} end",
            "output.setNumber(1,2) ".repeat(128)
        );
        let result = crate::public_api::compile(
            &source,
            &crate::public_api::ApiCompileOptions {
                source_map: Some(true),
                ..Default::default()
            },
        );
        assert!(result.ok, "{:?}", result.diagnostics);
        let code = result.code.unwrap();
        let map = result.map.unwrap();
        let details = validate(&code, &map).unwrap();
        let json: Value = serde_json::from_str(&map).unwrap();
        assert!(details.origins.len() > 64);
        assert!(json["names"].as_array().unwrap().len() < 10);
    }

    #[test]
    fn renamed_lexical_map_validates_and_uses_five_element_records() {
        let source = "local longname=1\n_ENV.x=longname\nreturn longname\n";
        let plan: Vec<(usize, usize, String)> = source
            .match_indices("longname")
            .map(|(i, _)| (i, i + "longname".len(), "a".to_string()))
            .collect();
        assert_eq!(plan.len(), 3);
        let options = CompileOptions {
            origin_source: Some("lexical.lua".into()),
            ..CompileOptions::default()
        };
        let (code, compact) = storm_lua_syntax::print::lexical_minify_renamed_with_compact_origins(
            "lexical.lua",
            source,
            &plan,
        )
        .unwrap();
        assert!(compact.tokens.iter().any(|t| t.renamed));
        let map = encode_compact_lexical_for_options(&code, &compact, &options, None).unwrap();
        let json: Value = serde_json::from_str(&map).unwrap();
        let tokens = json["x_storm"]["tokens"].as_array().unwrap();
        assert!(!tokens.is_empty());
        assert!(tokens.iter().any(|t| t.as_array().unwrap().len() == 5));
        assert!(tokens
            .iter()
            .all(|t| matches!(t.as_array().unwrap().len(), 4 | 5)));
        let details = validate(&code, &map).unwrap();
        // Expanded renamed mappings carry no exact-copy attribution.
        assert!(details
            .mappings
            .iter()
            .any(|m| m.origin.is_some() && m.copied.is_none()));
        // The materialized form agrees byte-for-byte with the compact form.
        let materialized = compact.clone().into_generated();
        let expected = encode_owned_for_options(&code, materialized, &options, None).unwrap();
        assert_eq!(
            map, expected,
            "renamed compact and materialized maps differ"
        );
    }

    #[test]
    fn unrenamed_lexical_map_keeps_four_element_records() {
        let source = "local _ENV=_ENV function onTick()output.setNumber(1,2)end";
        let (code, compact) =
            storm_lua_syntax::print::lexical_minify_with_compact_origins("lexical.lua", source)
                .unwrap();
        assert!(compact.tokens.iter().all(|t| !t.renamed));
        let map =
            encode_compact_lexical_for_options(&code, &compact, &CompileOptions::default(), None)
                .unwrap();
        let json: Value = serde_json::from_str(&map).unwrap();
        let tokens = json["x_storm"]["tokens"].as_array().unwrap();
        assert!(!tokens.is_empty());
        assert!(tokens.iter().all(|t| t.as_array().unwrap().len() == 4));
        validate(&code, &map).unwrap();
    }
}
