//! Source origins carried independently of AST equality and optimization decisions.
//! Unknown origin is `None`, never a nearby source position or a synthetic claim.

use crate::deferred_origin::StoredOrigin;
use crate::explanation::{
    InlineContext, OptimizationReason, ReasonOperation, SourceDisposition, SourceRelation,
    SyntaxSite,
};
use crate::origin_slots::OriginSlots;
use crate::{Ast, NameSite, Node, NodeId, NodePositions, PrintedSource};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;

/// Small deterministic FxHash-style hasher for internal origin interning only.
/// Output order never comes from hash order: origins, contexts and mappings are
/// all appended in first-seen order, so the hasher affects speed, not output.
/// It must stay non-cryptographic and dependency-free: this map is rebuilt on
/// every resolve and never crosses a trust boundary.
///
/// The state is a fixed-width `u64`, never `usize`: the seed literal does not
/// fit a 32-bit `usize` (wasm32 failed to compile), and a fixed width keeps
/// the hash sequence identical on every target. On 64-bit targets the
/// sequence is bit-for-bit the one the earlier `usize` state produced.
#[derive(Default)]
struct FastHasher {
    hash: u64,
}

const FAST_HASH_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FastHasher {
    #[inline]
    fn mix(&mut self, value: u64) {
        self.hash = (self.hash.rotate_left(5) ^ value).wrapping_mul(FAST_HASH_SEED);
    }
}

impl Hasher for FastHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(std::mem::size_of::<u64>()) {
            let mut value = 0u64;
            for (index, &byte) in chunk.iter().enumerate() {
                value |= (byte as u64) << (index * 8);
            }
            self.mix(value);
        }
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.mix(value as u64);
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.mix(value);
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.mix(value as u64);
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.mix(value as u64);
    }
}

/// Origin interning table keyed by hash instead of by cloned key.
///
/// A plain `FastMap<Origin, u32>` clones every origin once for the map key
/// and once more for the `origins` vector, even though every origin value
/// is interned exactly once in practice. This table stores only the hash
/// (from the same `FastHasher` the old map used) mapped to the id, and
/// checks equality against the already-stored `origins` entry, so a
/// distinct origin is cloned exactly once. Hash collisions cannot change
/// results: ids sharing a hash live in `overflow` and are compared by value.
#[derive(Default)]
struct OriginInterner {
    by_hash: FastMap<u64, u32>,
    overflow: Vec<(u64, u32)>,
}

impl OriginInterner {
    fn reserve(&mut self, additional: usize) {
        self.by_hash.reserve(additional);
    }

    /// Return the id for `origin`, pushing one clone into `origins` on a miss.
    fn intern(&mut self, origins: &mut Vec<Origin>, origin: &Origin) -> u32 {
        let mut hasher = FastHasher::default();
        std::hash::Hash::hash(origin, &mut hasher);
        let hash = hasher.finish();
        if let Some(&id) = self.by_hash.get(&hash) {
            if origins[id as usize] == *origin {
                return id;
            }
            for &(other_hash, other_id) in &self.overflow {
                if other_hash == hash && origins[other_id as usize] == *origin {
                    return other_id;
                }
            }
            let id = origins.len() as u32;
            origins.push(origin.clone());
            self.overflow.push((hash, id));
            return id;
        }
        let id = origins.len() as u32;
        origins.push(origin.clone());
        self.by_hash.insert(hash, id);
        id
    }
}

type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FastHasher>>;

/// Immutable source snapshot. A filename alone does not identify source contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSnapshot {
    /// Host-facing source name, not a filesystem lookup.
    pub name: Arc<str>,
    /// Original UTF-8 source, shared between optimization candidates.
    pub text: Arc<str>,
}

/// Half-open byte range within the associated source snapshot table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSpan {
    /// Index in this provenance payload's source table.
    pub source: u32,
    /// Inclusive UTF-8 byte offset.
    pub start: usize,
    /// Exclusive UTF-8 byte offset.
    pub end: usize,
}

/// How a surviving construct was associated with its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OriginKind {
    /// A construct retained from the parsed source (not a character-for-character guarantee).
    Source,
    /// A transformation explicitly attributes this construct to its input.
    Derived,
    /// A transformation explicitly created helper code without an original location.
    Synthetic,
}

/// Granularity of the attribution; it does not claim single-character equivalence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OriginPrecision {
    /// One explicit identifier occurrence.
    Name,
    /// One copied lexical token.
    Token,
    /// An original expression.
    Expression,
    /// An original statement.
    Statement,
    /// A block or merged group of source constructs.
    Group,
}

/// A source attribution. Only current surviving provenance, not an unbounded pass history.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Origin {
    /// Direct source, an explicit derivation, or explicitly synthesized code.
    pub kind: OriginKind,
    /// Primary source range; absent only for a synthetic origin.
    pub primary: Option<SourceSpan>,
    /// Additional contributing ranges, in deterministic first-seen order.
    pub related: Arc<Vec<SourceSpan>>,
    /// Last explicitly recorded transformation, not a timing-dependent decision.
    pub transformation: Option<Arc<str>>,
    /// What the primary attribution describes.
    pub precision: OriginPrecision,
    /// Original spelling for a source identifier occurrence, when known.
    pub name: Option<Arc<str>>,
    /// Typed source relationships; related is their untyped projection.
    pub relations: Arc<Vec<SourceRelation>>,
    /// Recorded actions retained on this construct, not all search trials.
    pub reasons: Arc<Vec<OptimizationReason>>,
}

// These are immutable empty values, not caches keyed by user source. Most
// parsed nodes have no relationships or reasons; allocating identical empty
// Vec headers per token/node amplifies source-map-only allocation pressure.
pub(crate) fn empty_related() -> Arc<Vec<SourceSpan>> {
    static EMPTY: std::sync::OnceLock<Arc<Vec<SourceSpan>>> = std::sync::OnceLock::new();
    Arc::clone(EMPTY.get_or_init(Arc::default))
}
pub(crate) fn empty_relations() -> Arc<Vec<SourceRelation>> {
    static EMPTY: std::sync::OnceLock<Arc<Vec<SourceRelation>>> = std::sync::OnceLock::new();
    Arc::clone(EMPTY.get_or_init(Arc::default))
}
pub(crate) fn empty_reasons() -> Arc<Vec<OptimizationReason>> {
    static EMPTY: std::sync::OnceLock<Arc<Vec<OptimizationReason>>> = std::sync::OnceLock::new();
    Arc::clone(EMPTY.get_or_init(Arc::default))
}

pub(crate) fn empty_dispositions() -> Arc<Vec<SourceDisposition>> {
    static EMPTY: std::sync::OnceLock<Arc<Vec<SourceDisposition>>> = std::sync::OnceLock::new();
    Arc::clone(EMPTY.get_or_init(Arc::default))
}
pub(crate) fn empty_tokens() -> Arc<Vec<(SyntaxSite, StoredOrigin)>> {
    static EMPTY: std::sync::OnceLock<Arc<Vec<(SyntaxSite, StoredOrigin)>>> =
        std::sync::OnceLock::new();
    Arc::clone(EMPTY.get_or_init(Arc::default))
}

fn validate_source_span(span: SourceSpan, sources: &[SourceSnapshot]) -> Result<(), String> {
    let source = sources
        .get(span.source as usize)
        .ok_or("origin source index is out of bounds")?;
    if span.start > span.end
        || span.end > source.text.len()
        || !source.text.is_char_boundary(span.start)
        || !source.text.is_char_boundary(span.end)
    {
        return Err("origin range is not a valid UTF-8 source slice".into());
    }
    Ok(())
}

impl Origin {
    /// Attribute a construct to one original source range.
    pub fn source(span: SourceSpan, precision: OriginPrecision, name: Option<Arc<str>>) -> Self {
        Self {
            kind: OriginKind::Source,
            primary: Some(span),
            related: empty_related(),
            transformation: None,
            precision,
            name,
            relations: empty_relations(),
            reasons: empty_reasons(),
        }
    }
    /// Describe compiler-created code without inventing a source location.
    pub fn synthetic(transformation: &str) -> Self {
        Self {
            kind: OriginKind::Synthetic,
            primary: None,
            related: empty_related(),
            transformation: Some(transformation.into()),
            precision: OriginPrecision::Group,
            name: None,
            relations: empty_relations(),
            reasons: Arc::new(vec![OptimizationReason::action(
                transformation,
                ReasonOperation::Synthesize,
            )]),
        }
    }
    /// Preserve source attribution but identify it as a transformation result.
    pub fn derived(&self, transformation: &str) -> Self {
        let mut result = self.clone();
        result.derive_in_place(transformation);
        result
    }
    /// Record the same derivation on an already uniquely owned origin. Callers
    /// updating a retained slot avoid sharing its vectors with a temporary clone
    /// immediately before appending a reason (which forces a full COW copy).
    pub(crate) fn derive_in_place(&mut self, transformation: &str) {
        if self.kind != OriginKind::Synthetic {
            self.kind = OriginKind::Derived;
        }
        if self.transformation.as_deref() != Some(transformation) {
            self.transformation = Some(transformation.into());
        }
        self.add_reason(OptimizationReason::action(
            transformation,
            ReasonOperation::Rewrite,
        ));
    }
    pub(crate) fn add_reason(&mut self, reason: OptimizationReason) {
        // Equal rule names do not mean equal decisions: operands and checked
        // facts can differ at successive applications of the same optimization.
        if self.reasons.last().is_some_and(|last| last == &reason) {
            return;
        }
        Arc::make_mut(&mut self.reasons).push(reason);
    }
    pub(crate) fn validate(&self, sources: &[SourceSnapshot]) -> Result<(), String> {
        match self.kind {
            OriginKind::Source | OriginKind::Derived if self.primary.is_none() => {
                return Err("source origin has no source range".into())
            }
            OriginKind::Synthetic if self.primary.is_some() || self.name.is_some() => {
                return Err("synthetic origin invents a source location".into())
            }
            _ => {}
        }
        if self.kind != OriginKind::Source
            && self
                .transformation
                .as_ref()
                .is_none_or(|value| value.is_empty())
        {
            return Err("transformed origin has no transformation".into());
        }
        for span in self
            .primary
            .iter()
            .chain(self.related.iter())
            .chain(self.relations.iter().map(|r| &r.span))
        {
            validate_source_span(*span, sources)?;
        }
        if let (Some(span), Some(name)) = (self.primary, self.name.as_ref()) {
            if &sources[span.source as usize].text[span.start..span.end] != name.as_ref() {
                return Err("original name does not match its source snapshot".into());
            }
        }
        for reason in self.reasons.iter() {
            reason.validate()?;
        }
        Ok(())
    }
}

/// Name attributions of one arena slot, serialized as a JSON array.
/// Up to two entries live inline: a renamed single name, or a two-name
/// local binding. Larger bindings spill to a heap buffer exactly like the
/// previous plain Vec, so this only removes the per-node buffer allocation
/// for the common cases; it does not share the list between nodes, because
/// a renamed node's names are rewritten in place after its origin is
/// cloned, and sharing would force a copy-on-write duplication per node.
/// Cloning an origin with empty names still allocates nothing.
pub(crate) type NameOrigins = smallvec::SmallVec<[(NameSite, StoredOrigin); 2]>;

/// Node-level inline contexts and dispositions, stored inline while small:
/// most nodes carry zero or one entry, and the previous shared-empty
/// Arc<Vec> paid one Arc box plus one buffer allocation on the first
/// mutation. A single entry now lives in the slot itself (no allocation);
/// larger lists spill to a heap buffer exactly like the previous plain
/// Vec, and serialization stays a plain JSON array, so the recorded
/// origins are unchanged.
pub(crate) type ContextList = smallvec::SmallVec<[InlineContext; 1]>;
pub(crate) type DispositionList = smallvec::SmallVec<[SourceDisposition; 1]>;

/// One arena slot's origin, shared when candidate ASTs are cloned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NodeOrigin {
    pub origin: Option<StoredOrigin>,
    pub names: NameOrigins,
    pub contexts: ContextList,
    pub tokens: Arc<Vec<(SyntaxSite, StoredOrigin)>>,
    pub dispositions: DispositionList,
}

impl Default for NodeOrigin {
    fn default() -> Self {
        Self {
            origin: None,
            names: NameOrigins::new(),
            contexts: ContextList::new(),
            tokens: empty_tokens(),
            dispositions: DispositionList::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArenaOrigins {
    pub sources: Arc<Vec<SourceSnapshot>>,
    pub slots: Arc<OriginSlots>,
    pub dispositions: Arc<Vec<SourceDisposition>>,
}
/// One lexed token as seen by origin seeding: a byte position, its end,
/// and a spelling comparison. Fresh-lexed borrowed tokens carry the spelling
/// directly; the parser's own tokens slice it from the source text on demand,
/// so seeding never needs an intermediate borrowed-token vector. The
/// comparison returns `None` when the spelling cannot be recovered from
/// `text` (a parser token outside its source); borrowed tokens always
/// return `Some`.
pub(crate) trait SeedToken {
    fn position(&self) -> usize;
    fn end(&self) -> usize;
    fn spelling_is(&self, text: &str, expected: &str) -> Option<bool>;
}

impl SeedToken for crate::lexer::BorrowedToken<'_> {
    fn position(&self) -> usize {
        self.p
    }
    fn end(&self) -> usize {
        self.p + self.v.len()
    }
    fn spelling_is(&self, _text: &str, expected: &str) -> Option<bool> {
        Some(self.v == expected)
    }
}

impl ArenaOrigins {
    /// Seed from an already-lexed token stream. The parser lexes the source
    /// once while parsing; reusing its tokens here avoids a second lex pass
    /// for the same text. Token spellings and positions are identical to a
    /// fresh lex, so seeded origins are unchanged. Callers may pass either
    /// fresh-lexed borrowed tokens or the parser's own tokens directly, so
    /// the origin path needs no intermediate borrowed-token vector.
    pub(crate) fn seed_with_tokens<T: SeedToken>(
        ast: &Ast,
        positions: &NodePositions,
        name: &str,
        text: &str,
        tokens: &[T],
    ) -> Result<Self, String> {
        let sources = Arc::new(vec![SourceSnapshot {
            name: name.into(),
            text: text.into(),
        }]);
        let mut slots = Vec::with_capacity(ast.nodes.len());
        for (id, node) in ast.nodes.iter().enumerate() {
            let (start, end) = positions
                .span(id as NodeId)
                .ok_or("parsed node is missing its source span")?;
            let precision = match node {
                Node::Block(_) => OriginPrecision::Group,
                Node::Nil
                | Node::Bool(_)
                | Node::Vararg
                | Node::Num(_)
                | Node::Str(_)
                | Node::Function(..)
                | Node::Table(_)
                | Node::Un(..)
                | Node::Bin(..)
                | Node::Paren(_)
                | Node::Call(..)
                | Node::Name(_)
                | Node::Index(..)
                | Node::Methodname(..) => OriginPrecision::Expression,
                _ => OriginPrecision::Statement,
            };
            let mut syntax_tokens = Vec::new();
            for site in [
                SyntaxSite::Operator,
                SyntaxSite::Begin,
                SyntaxSite::Body,
                SyntaxSite::End,
            ] {
                let Some(expected) = crate::explanation::syntax_text(node, site) else {
                    continue;
                };
                let token = match site {
                    SyntaxSite::Begin => {
                        tokens.get(tokens.partition_point(|t| t.position() < start))
                    }
                    SyntaxSite::End => tokens.get(
                        tokens
                            .partition_point(|t| t.position() < end)
                            .saturating_sub(1),
                    ),
                    SyntaxSite::Body => {
                        let next = match node {
                            Node::While(_, body)
                            | Node::Fornum(_, _, _, _, body)
                            | Node::Forin(_, _, body) => Some(*body),
                            Node::If(arms, _) => arms.first().map(|arm| arm.body),
                            Node::Repeat(_, condition) => Some(*condition),
                            _ => None,
                        }
                        .and_then(|id| positions.span(id))
                        .map(|span| span.0);
                        next.and_then(|start| {
                            tokens
                                .partition_point(|t| t.position() < start)
                                .checked_sub(1)
                        })
                        .and_then(|i| tokens.get(i))
                    }
                    SyntaxSite::Operator => {
                        let offset = if let Node::Bin(_, left, _) = node {
                            positions.span(*left).ok_or("operator left span missing")?.1
                        } else {
                            start
                        };
                        tokens.get(tokens.partition_point(|t| t.position() < offset))
                    }
                };
                let Some(token) = token else { continue };
                let matches = token
                    .spelling_is(text, expected)
                    .ok_or("parser token is outside its source")?;
                if matches && token.position() >= start && token.end() <= end {
                    syntax_tokens.push((
                        site,
                        Origin::source(
                            SourceSpan {
                                source: 0,
                                start: token.position(),
                                end: token.end(),
                            },
                            OriginPrecision::Token,
                            None,
                        )
                        .into(),
                    ));
                }
            }
            slots.push(Some(Arc::new(NodeOrigin {
                origin: Some(
                    Origin::source(
                        SourceSpan {
                            source: 0,
                            start,
                            end,
                        },
                        precision,
                        None,
                    )
                    .into(),
                ),
                names: NameOrigins::new(),
                contexts: ContextList::new(),
                tokens: if syntax_tokens.is_empty() {
                    empty_tokens()
                } else {
                    Arc::new(syntax_tokens)
                },
                dispositions: DispositionList::new(),
            })));
        }
        for ((node, site), (start, end)) in positions.name_spans() {
            let original = text
                .get(start..end)
                .ok_or("identifier span is outside its original source")?;
            let slot = slots
                .get_mut(node as usize)
                .and_then(Option::as_mut)
                .ok_or("name refers to an unknown parsed node")?;
            Arc::make_mut(slot).names.push((
                site,
                Origin::source(
                    SourceSpan {
                        source: 0,
                        start,
                        end,
                    },
                    OriginPrecision::Name,
                    Some(original.into()),
                )
                .into(),
            ));
        }
        let result = Self {
            sources,
            slots: Arc::new(OriginSlots::from(slots)),
            dispositions: empty_dispositions(),
        };
        result.validate(ast.nodes.len())?;
        Ok(result)
    }
    pub fn validate(&self, nodes: usize) -> Result<(), String> {
        if self.slots.len() != nodes {
            return Err("origin slot count does not match node arena".into());
        }
        for slot in self.slots.iter().flatten() {
            if let Some(origin) = &slot.origin {
                origin.get().validate(&self.sources)?;
            }
            for (_, origin) in slot.tokens.iter() {
                origin.get().validate(&self.sources)?;
            }
            for context in slot.contexts.iter() {
                for span in [context.definition, context.call_site] {
                    Origin::source(span, OriginPrecision::Expression, None)
                        .validate(&self.sources)?;
                }
            }
            for disposition in slot.dispositions.iter() {
                for span in std::iter::once(&disposition.original)
                    .chain(disposition.replacement_sources.iter())
                {
                    Origin::source(*span, OriginPrecision::Statement, None)
                        .validate(&self.sources)?;
                }
                disposition.reason.validate()?;
            }
            for reason in slot
                .origin
                .iter()
                .flat_map(|o| o.get().reasons.iter())
                .chain(slot.names.iter().flat_map(|(_, o)| o.get().reasons.iter()))
            {
                if reason.code.is_empty() {
                    return Err("optimization reason has empty code".into());
                }
            }
            // Name lists are almost always tiny, so a linear duplicate scan
            // avoids one BTreeSet allocation per named slot while reporting
            // exactly the same duplicates.
            for (index, (site, origin)) in slot.names.iter().enumerate() {
                if slot.names[..index].iter().any(|(other, _)| other == site) {
                    return Err("duplicate name occurrence in origin table".into());
                }
                origin.get().validate(&self.sources)?;
            }
        }
        Ok(())
    }
}

/// Nonoverlapping interval of actual final output. `None` explicitly means unknown/unmapped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct OriginMapping {
    /// Inclusive generated UTF-8 byte offset.
    pub start: usize,
    /// Exclusive generated UTF-8 byte offset.
    pub end: usize,
    /// Index in GeneratedOrigins.origins, or unknown attribution.
    pub origin: Option<u32>,
    /// Exact source slice when byte-for-byte correspondence was proven during emission.
    pub copied: Option<SourceSpan>,
    /// Enclosing inline contexts, outer to inner.
    pub inline_contexts: Vec<u32>,
}

/// An emitted enclosing construct retained alongside the most-specific interval map.
/// Its range may overlap others; it is not automatically an executable stop site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedConstruct {
    /// Inclusive UTF-8 output offset.
    pub start: usize,
    /// Exclusive UTF-8 output offset.
    pub end: usize,
    /// Source attribution in the same origin table; none is explicitly unknown.
    pub origin: Option<u32>,
}

/// Internal final-source attribution. Source Map v3 encoding belongs to the build layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedOrigins {
    /// Source snapshots for the referenced ranges.
    pub sources: Arc<Vec<SourceSnapshot>>,
    /// Interned origin descriptions in deterministic output order.
    pub origins: Vec<Origin>,
    /// Final generated ranges. Gaps and unknown children never inherit a nearby mapping.
    pub mappings: Vec<OriginMapping>,
    /// Interned expansion contexts, independent of leaf positions.
    pub contexts: Vec<InlineContext>,
    /// Enclosing source constructs for expression/statement reasoning and navigation.
    pub constructs: Vec<GeneratedConstruct>,
    /// Explicit removals/replacements for this candidate.
    pub dispositions: Arc<Vec<SourceDisposition>>,
}

impl GeneratedOrigins {
    /// Resolve the most specific recorded construct at each final byte interval.
    /// A missing child origin masks a known enclosing expression, not vice versa.
    pub fn from_print(ast: &Ast, printed: &PrintedSource) -> Option<Self> {
        let arena = ast.nodes.provenance()?;
        Self::from_arena(arena, printed)
    }
    fn from_arena(arena: &ArenaOrigins, printed: &PrintedSource) -> Option<Self> {
        // Start and end events are merged below. End events stay
        // comparison-sorted (their input order is already nearly sorted,
        // so that sort is cheap). Start events are ordered by a radix
        // sort over 8-bit offset digits instead: merge decisions only
        // compare offsets -- at equal offsets an end event always
        // precedes a start event, and same-offset starts commute
        // because the active set is order-independent -- so sorting
        // start indices by offset is enough, and a linear radix pass
        // beats a comparison sort on the thousands of inversions in
        // start order. The radix sort is stable, so equal offsets keep
        // emission order.
        let mut ends = Vec::with_capacity(printed.emissions.len());
        for (index, emission) in printed.emissions.iter().enumerate() {
            assert!(emission.start < emission.end && emission.end <= printed.code.len());
            ends.push((emission.end, false, index));
        }
        let mut start_order: Vec<usize> = (0..printed.emissions.len()).collect();
        let mut start_buf = vec![0usize; printed.emissions.len()];
        let max_start = printed.emissions.iter().map(|e| e.start).max().unwrap_or(0);
        let mut shift = 0;
        while (max_start >> shift) != 0 {
            let mut counts = vec![0usize; 1 << 8];
            for &index in &start_order {
                counts[(printed.emissions[index].start >> shift) & 0xFF] += 1;
            }
            let mut sum = 0usize;
            for count in counts.iter_mut() {
                let next = *count;
                *count = sum;
                sum += next;
            }
            for &index in &start_order {
                let digit = (printed.emissions[index].start >> shift) & 0xFF;
                start_buf[counts[digit]] = index;
                counts[digit] += 1;
            }
            std::mem::swap(&mut start_order, &mut start_buf);
            shift += 8;
        }
        // The merge below walks start_order directly instead of
        // materializing a second starts vector of tuples: a start event
        // is fully described by its emission index, and at equal offsets
        // an end event precedes a start event, which the strict
        // start-offset < end-offset comparison below encodes.
        ends.sort_unstable();
        // More specific ranges win; name sites win ties; children are emitted before parents.
        let key = |index: usize| {
            let emission = &printed.emissions[index];
            (
                emission.end - emission.start,
                emission.site.is_none(),
                index,
            )
        };
        // Active ranges are keyed by (span length, name-site tiebreak, emission
        // index), so keys are unique and the set stays tiny (nesting depth).
        // A sorted Vec gives the same first()/insert/remove order as the previous
        // BTreeSet without a heap allocation per overlapping range.
        let mut active = Vec::<(usize, bool, usize)>::new();
        let mut active_contexts = Vec::<(usize, bool, usize)>::new();
        let mut result = Self {
            sources: Arc::clone(&arena.sources),
            origins: Vec::new(),
            mappings: Vec::new(),
            contexts: Vec::new(),
            constructs: Vec::new(),
            dispositions: Arc::clone(&arena.dispositions),
        };
        // Each gap adds at most one mapping and one interned origin, and each
        // construct emission adds at most one construct, so reserving for the
        // emission count avoids repeated table/vector growth (and rehashing)
        // while resolving. The bounds are capacity hints only; growth past
        // them still works, and the resolved values are unchanged.
        let mut interned = OriginInterner::default();
        interned.reserve(printed.emissions.len());
        result.origins.reserve(printed.emissions.len());
        result.mappings.reserve(printed.emissions.len());
        result.constructs.reserve(printed.emissions.len());
        let mut context_ids = FastMap::<InlineContext, u32>::default();
        // Interning dominates resolving: every gap clones and hashes its
        // origin, and a construct's plain main origin was usually already
        // interned as some gap's winner, so both are memoized below.
        // A gap winner is identified by its emission index: the selected
        // stored origin for an emission is fixed by its node and site,
        // and arena slots are immutable while resolving, so a recorded
        // id is exactly what interning that winner again would return.
        // The memo is used only for reason-free gaps (a reason would
        // derive a fresh origin). On dense-normal this skips interning
        // for 1,920 of 9,284 gaps, while the previous single-entry
        // last-winner cache never hit there because the winner changes
        // at nearly every gap.
        let mut winner_ids: Vec<Option<u32>> = vec![None; printed.emissions.len()];
        let mut main_origin_ids: Vec<Option<u32>> = vec![None; arena.slots.len()];
        let mut cursor = 0;
        let (mut start_cursor, mut end_cursor) = (0, 0);
        while start_cursor < start_order.len() || end_cursor < ends.len() {
            // End events precede start events at the same offset, which
            // the strict comparison below encodes (equal offsets take
            // the end event). Start events are already in radix order.
            let take_start = if end_cursor >= ends.len() {
                true
            } else if start_cursor >= start_order.len() {
                false
            } else {
                printed.emissions[start_order[start_cursor]].start < ends[end_cursor].0
            };
            let (offset, start, index) = if take_start {
                let index = start_order[start_cursor];
                start_cursor += 1;
                (printed.emissions[index].start, true, index)
            } else {
                let event = ends[end_cursor];
                end_cursor += 1;
                event
            };
            if cursor < offset {
                let winner = active.first().copied();
                let emission = winner.map(|(_, _, index)| &printed.emissions[index]);
                let winner_index = winner.map(|(_, _, index)| index);
                let selected = emission.and_then(|emission| {
                    let slot = arena.slots[emission.node as usize].as_ref()?;
                    emission
                        .token
                        .and_then(|site| {
                            slot.tokens
                                .iter()
                                .find(|(key, _)| *key == site)
                                .map(|(_, o)| (o, false))
                        })
                        .or_else(|| {
                            emission
                                .site
                                .and_then(|site| {
                                    slot.names
                                        .iter()
                                        .find(|(key, _)| *key == site)
                                        .map(|(_, value)| (value, false))
                                })
                                .or_else(|| slot.origin.as_ref().map(|o| (o, true)))
                        })
                });
                let origin = selected.map(|(stored, _)| stored.get());
                let printed_origin = origin.zip(emission).and_then(|(origin, e)| {
                    e.reason.as_ref().map(|reason| {
                        let mut changed = origin.clone();
                        if changed.kind != OriginKind::Synthetic {
                            changed.kind = OriginKind::Derived;
                        }
                        changed.transformation = Some(reason.code.clone());
                        changed.add_reason(reason.clone());
                        changed
                    })
                });
                let origin = printed_origin.as_ref().or(origin);
                let copied = origin
                    .and_then(|o| o.primary)
                    .zip(emission)
                    .and_then(|(span, e)| {
                        let text = &arena.sources[span.source as usize].text;
                        if span.end - span.start == e.end - e.start
                            && text.get(span.start..span.end) == printed.code.get(e.start..e.end)
                        {
                            Some(SourceSpan {
                                source: span.source,
                                start: span.start + cursor - e.start,
                                end: span.start + offset - e.start,
                            })
                        } else {
                            None
                        }
                    });
                let mut contexts = Vec::new();
                for &(_, _, index) in active_contexts.iter().rev() {
                    let slot = arena.slots[printed.emissions[index].node as usize].as_ref()?;
                    for context in slot.contexts.iter() {
                        let id = *context_ids.entry(context.clone()).or_insert_with(|| {
                            let id = result.contexts.len() as u32;
                            result.contexts.push(context.clone());
                            id
                        });
                        if !contexts.contains(&id) {
                            contexts.push(id);
                        }
                    }
                }
                // Reuse the id recorded when this winner emission won an
                // earlier gap; the id for an unchanged origin value is
                // exactly what interning would return.
                let has_reason = emission.is_some_and(|e| e.reason.is_some());
                let cached = if has_reason {
                    None
                } else {
                    winner_index.and_then(|index| winner_ids.get(index).copied().flatten())
                };
                let id = if let Some(id) = cached {
                    result.append_with_id(cursor, offset, Some(id), copied, contexts);
                    Some(id)
                } else {
                    result.append_detail_hashed(
                        cursor,
                        offset,
                        origin,
                        copied,
                        contexts,
                        &mut interned,
                    )
                };
                if !has_reason {
                    if let (Some((_, is_main)), Some(id)) = (selected, id) {
                        if let Some(index) = winner_index {
                            winner_ids[index] = Some(id);
                        }
                        if is_main {
                            if let Some(winner) = emission {
                                if let Some(slot) = main_origin_ids.get_mut(winner.node as usize) {
                                    *slot = Some(id);
                                }
                            }
                        }
                    }
                }
                cursor = offset;
            }
            let event_key = key(index);
            let carries_context = arena.slots[printed.emissions[index].node as usize]
                .as_ref()
                .is_some_and(|slot| !slot.contexts.is_empty());
            if start {
                if let Err(position) = active.binary_search(&event_key) {
                    active.insert(position, event_key);
                }
                if carries_context {
                    if let Err(position) = active_contexts.binary_search(&event_key) {
                        active_contexts.insert(position, event_key);
                    }
                }
            } else {
                if let Ok(position) = active.binary_search(&event_key) {
                    active.remove(position);
                }
                if carries_context {
                    if let Ok(position) = active_contexts.binary_search(&event_key) {
                        active_contexts.remove(position);
                    }
                }
            }
        }
        if cursor < printed.code.len() {
            result.append_with_id(cursor, printed.code.len(), None, None, Vec::new());
        }
        // Include the selected root even when every statement was eliminated.
        // Dead speculative arena slots are deliberately not inspected.
        // Emission and root node ids index the same arena, so a bitset
        // dedupes dispositions without hashing every node id.
        let mut disposition_nodes = vec![false; arena.slots.len()];
        for node in std::iter::once(printed.root).chain(printed.emissions.iter().map(|e| e.node)) {
            if !disposition_nodes[node as usize] {
                disposition_nodes[node as usize] = true;
                if let Some(slot) = arena.slots[node as usize].as_ref() {
                    if !slot.dispositions.is_empty() {
                        Arc::make_mut(&mut result.dispositions)
                            .extend(slot.dispositions.iter().cloned());
                    }
                }
            }
        }
        for emission in printed
            .emissions
            .iter()
            .filter(|e| e.site.is_none() && e.token.is_none())
        {
            let origin = arena.slots[emission.node as usize]
                .as_ref()
                .and_then(|slot| slot.origin.as_ref())
                .map(StoredOrigin::get);
            let printed_origin = origin.and_then(|origin| {
                emission.reason.as_ref().map(|reason| {
                    let mut changed = origin.clone();
                    if changed.kind != OriginKind::Synthetic {
                        changed.kind = OriginKind::Derived;
                    }
                    changed.transformation = Some(reason.code.clone());
                    changed.add_reason(reason.clone());
                    changed
                })
            });
            let origin = printed_origin.as_ref().or(origin);
            let id = if emission.reason.is_none() && origin.is_some() {
                match main_origin_ids
                    .get(emission.node as usize)
                    .copied()
                    .flatten()
                {
                    Some(id) => Some(id),
                    None => origin.map(|o| result.intern_hashed(o, &mut interned)),
                }
            } else {
                origin.map(|o| result.intern_hashed(o, &mut interned))
            };
            result.constructs.push(GeneratedConstruct {
                start: emission.start,
                end: emission.end,
                origin: id,
            });
        }
        Some(result)
    }
    #[cfg(test)]
    fn append<S: std::hash::BuildHasher>(
        &mut self,
        start: usize,
        end: usize,
        origin: Option<&Origin>,
        interned: &mut HashMap<Origin, u32, S>,
    ) {
        self.append_detail(start, end, origin, None, Vec::new(), interned);
    }
    fn intern_hashed(&mut self, origin: &Origin, table: &mut OriginInterner) -> u32 {
        table.intern(&mut self.origins, origin)
    }

    fn append_detail_hashed(
        &mut self,
        start: usize,
        end: usize,
        origin: Option<&Origin>,
        copied: Option<SourceSpan>,
        contexts: Vec<u32>,
        table: &mut OriginInterner,
    ) -> Option<u32> {
        let id = match origin {
            Some(origin) => Some(table.intern(&mut self.origins, origin)),
            None => None,
        };
        self.append_with_id(start, end, id, copied, contexts);
        id
    }

    #[cfg(test)]
    fn append_detail<S: std::hash::BuildHasher>(
        &mut self,
        start: usize,
        end: usize,
        origin: Option<&Origin>,
        copied: Option<SourceSpan>,
        contexts: Vec<u32>,
        interned: &mut HashMap<Origin, u32, S>,
    ) -> Option<u32> {
        // One hash per origin: entry() hashes once, while get()+insert()
        // hashed every miss twice. The cloned entry key is dropped on a hit;
        // ids are still assigned in first-appearance order, so resolved
        // origins, mappings and constructs are identical.
        let id = origin.map(|origin| {
            *interned.entry(origin.clone()).or_insert_with(|| {
                let id = self.origins.len() as u32;
                self.origins.push(origin.clone());
                id
            })
        });
        self.append_with_id(start, end, id, copied, contexts);
        id
    }
    fn append_with_id(
        &mut self,
        start: usize,
        end: usize,
        id: Option<u32>,
        copied: Option<SourceSpan>,
        contexts: Vec<u32>,
    ) {
        if let Some(previous) = self.mappings.last_mut().filter(|previous| {
            previous.end == start
                && previous.origin == id
                && previous.inline_contexts == contexts
                && match (previous.copied, copied) {
                    (None, None) => true,
                    (Some(a), Some(b)) => a.source == b.source && a.end == b.start,
                    _ => false,
                }
        }) {
            previous.end = end;
            if let (Some(a), Some(b)) = (previous.copied.as_mut(), copied) {
                a.end = b.end;
            }
        } else {
            self.mappings.push(OriginMapping {
                start,
                end,
                origin: id,
                copied,
                inline_contexts: contexts,
            });
        }
    }
    /// Attribute unchanged source bytes. Token-level origins are recorded by lexical emitters.
    pub fn identity(name: &str, source: &str) -> Self {
        Self {
            sources: Arc::new(vec![SourceSnapshot {
                name: name.into(),
                text: source.into(),
            }]),
            origins: vec![Origin::source(
                SourceSpan {
                    source: 0,
                    start: 0,
                    end: source.len(),
                },
                OriginPrecision::Group,
                None,
            )],
            contexts: Vec::new(),
            constructs: Vec::new(),
            dispositions: empty_dispositions(),
            mappings: if source.is_empty() {
                Vec::new()
            } else {
                vec![OriginMapping {
                    start: 0,
                    end: source.len(),
                    origin: Some(0),
                    copied: Some(SourceSpan {
                        source: 0,
                        start: 0,
                        end: source.len(),
                    }),
                    inline_contexts: Vec::new(),
                }]
            },
        }
    }
    /// Count bytes whose origin has not yet been established by the compiler.
    pub fn unknown_bytes(&self) -> usize {
        self.mappings
            .iter()
            .filter(|mapping| mapping.origin.is_none())
            .map(|mapping| mapping.end - mapping.start)
            .sum()
    }
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopiedToken {
    pub start: usize,
    pub source_start: usize,
    pub source_end: usize,
    pub is_name: bool,
    /// True when the generated spelling differs from the source spelling
    /// (a renamed local identifier). Only renamed tokens may have a
    /// generated length different from their source length.
    #[serde(default)]
    pub renamed: bool,
    /// Generated-side end offset. Equal to `start + (source_end -
    /// source_start)` for unrenamed tokens; for a renamed identifier
    /// token it is `start + new_name.len()`, which may differ from the
    /// source length. The origin name stays the original spelling
    /// (from `source_text[source_start..source_end]`).
    pub generated_end: usize,
}
impl CopiedToken {
    #[inline]
    pub fn end(&self) -> usize {
        self.generated_end
    }
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LexicalCopiedOrigins {
    pub source_name: Arc<str>,
    pub source_text: Arc<str>,
    pub generated_len: usize,
    pub tokens: Vec<CopiedToken>,
}

impl LexicalCopiedOrigins {
    pub fn new(name: &str, source: &str, generated_len: usize, tokens: Vec<CopiedToken>) -> Self {
        Self {
            source_name: name.into(),
            source_text: source.into(),
            generated_len,
            tokens,
        }
    }

    /// Reject malformed token ranges before indexing serialized candidate data.
    pub fn validate_for_code(&self, code: &str) -> Result<(), String> {
        if self.generated_len != code.len() || self.tokens.len() > u32::MAX as usize {
            return Err("invalid compact lexical provenance length".into());
        }
        let mut generated_end = 0;
        let mut source_end = 0;
        for token in &self.tokens {
            if token.source_end <= token.source_start {
                return Err("invalid compact lexical source range".into());
            }
            if token.generated_end <= token.start {
                return Err("invalid compact lexical generated range".into());
            }
            let end = token.generated_end;
            if token.start < generated_end || token.source_start < source_end {
                return Err("unordered compact lexical token ranges".into());
            }
            let original = self
                .source_text
                .get(token.source_start..token.source_end)
                .ok_or("invalid compact lexical source boundary")?;
            if token.renamed {
                // Renamed token: only bounds / UTF-8 boundaries are
                // checked here; the spelling is established by the
                // rename step's re-lex verification and the FULL
                // corpus comparison.
                if code.get(token.start..end).is_none() {
                    return Err("compact lexical token does not match generated code".into());
                }
            } else if code.get(token.start..end) != Some(original) {
                // Unrenamed token: the generated piece must be the
                // original spelling.
                return Err("compact lexical token does not match generated code".into());
            }
            generated_end = end;
            source_end = token.source_end;
        }
        Ok(())
    }

    pub fn into_generated(self) -> GeneratedOrigins {
        GeneratedOrigins::copied_tokens(
            &self.source_name,
            &self.source_text,
            self.generated_len,
            &self.tokens,
        )
    }
}

impl GeneratedOrigins {
    pub(crate) fn copied_tokens(
        name: &str,
        source: &str,
        generated_len: usize,
        tokens: &[CopiedToken],
    ) -> Self {
        let mut output = Self {
            sources: Arc::new(vec![SourceSnapshot {
                name: name.into(),
                text: source.into(),
            }]),
            origins: Vec::new(),
            mappings: Vec::new(),
            contexts: Vec::new(),
            constructs: Vec::new(),
            dispositions: empty_dispositions(),
        };
        // Each lexical token has a distinct source span. Its origin is unique,
        // so interning/cloning every descriptor cannot merge any of these rows.
        output.origins.reserve(tokens.len());
        output.mappings.reserve(tokens.len());
        let mut cursor = 0;
        // Large lexical-only outputs repeat identifier spellings many times.
        // Share only equal text within this one result; origin IDs and exact
        // source occurrences stay distinct. Tiny outputs avoid a hash table.
        let mut identifiers = HashMap::<&str, Arc<str>>::new();
        for token in tokens {
            if cursor < token.start {
                output.mappings.push(OriginMapping {
                    start: cursor,
                    end: token.start,
                    origin: None,
                    copied: None,
                    inline_contexts: Vec::new(),
                });
            }
            let origin = Origin::source(
                SourceSpan {
                    source: 0,
                    start: token.source_start,
                    end: token.source_end,
                },
                if token.is_name {
                    OriginPrecision::Name
                } else {
                    OriginPrecision::Token
                },
                token.is_name.then(|| {
                    let text = &source[token.source_start..token.source_end];
                    if tokens.len() < 64 {
                        return Arc::from(text);
                    }
                    if let Some(existing) = identifiers.get(text) {
                        return Arc::clone(existing);
                    }
                    let name: Arc<str> = text.into();
                    identifiers.insert(text, Arc::clone(&name));
                    name
                }),
            );
            let id = output.origins.len() as u32;
            output.origins.push(origin);
            output.mappings.push(OriginMapping {
                start: token.start,
                end: token.end(),
                origin: Some(id),
                copied: if token.renamed {
                    None
                } else {
                    Some(SourceSpan {
                        source: 0,
                        start: token.source_start,
                        end: token.source_end,
                    })
                },
                inline_contexts: Vec::new(),
            });
            cursor = token.end();
        }
        if cursor < generated_len {
            output.mappings.push(OriginMapping {
                start: cursor,
                end: generated_len,
                origin: None,
                copied: None,
                inline_contexts: Vec::new(),
            });
        }
        output
    }
}

impl GeneratedOrigins {
    /// Validate bounds, UTF-8 boundaries, source references and complete output
    /// coverage. This is a coordinate check, not a content fingerprint; callers
    /// must keep this data paired with the code that produced it.
    pub fn validate_for_code(&self, code: &str) -> Result<(), String> {
        for origin in &self.origins {
            origin.validate(&self.sources)?;
        }
        for construct in &self.constructs {
            if construct.start >= construct.end
                || construct.end > code.len()
                || !code.is_char_boundary(construct.start)
                || !code.is_char_boundary(construct.end)
                || construct
                    .origin
                    .is_some_and(|i| i as usize >= self.origins.len())
            {
                return Err("invalid enclosing construct".into());
            }
        }
        for context in &self.contexts {
            for span in [context.definition, context.call_site] {
                validate_source_span(span, &self.sources)?;
            }
        }
        for disposition in self.dispositions.iter() {
            for span in
                std::iter::once(&disposition.original).chain(disposition.replacement_sources.iter())
            {
                validate_source_span(*span, &self.sources)?;
            }
            disposition.reason.validate()?;
        }
        let mut cursor = 0;
        for mapping in &self.mappings {
            if mapping.start != cursor
                || mapping.start >= mapping.end
                || mapping.end > code.len()
                || !code.is_char_boundary(mapping.start)
                || !code.is_char_boundary(mapping.end)
                || mapping
                    .origin
                    .is_some_and(|id| id as usize >= self.origins.len())
            {
                return Err("invalid or stale generated provenance ranges".into());
            }
            if mapping
                .inline_contexts
                .iter()
                .any(|id| *id as usize >= self.contexts.len())
            {
                return Err("inline context index is out of bounds".into());
            }
            if let Some(copied) = mapping.copied {
                validate_source_span(copied, &self.sources)?;
                if mapping.origin.is_none()
                    || self.origins[mapping.origin.unwrap_or(0) as usize].kind
                        == OriginKind::Synthetic
                    || self.sources[copied.source as usize]
                        .text
                        .get(copied.start..copied.end)
                        != code.get(mapping.start..mapping.end)
                {
                    return Err(
                        "exact-copy attribution does not match source and generated bytes".into(),
                    );
                }
            }
            cursor = mapping.end;
        }
        if cursor != code.len() {
            return Err("generated provenance does not cover its exact code buffer".into());
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::{parse_source, parse_source_with_origins, Printer};

    fn tracked(source: &str) -> (Ast, NodeId) {
        parse_source_with_origins("controller.lua", source).unwrap()
    }
    fn locate(ast: &Ast, predicate: impl Fn(&Node) -> bool) -> NodeId {
        ast.nodes.iter().position(predicate).expect("test node") as NodeId
    }
    fn output(ast: &Ast, root: NodeId) -> (String, GeneratedOrigins) {
        let printed = Printer::new(ast, true).output_with_positions(root);
        let origins = GeneratedOrigins::from_print(ast, &printed).unwrap();
        (printed.code, origins)
    }

    #[test]
    fn tracked_and_plain_ast_compare_equal_but_origins_are_optional() {
        let source = "local value=1 return value";
        let (plain, root) = parse_source(source).unwrap();
        let (tracked, tracked_root) = tracked(source);
        assert!(plain == tracked);
        assert_eq!(root, tracked_root);
        assert!(!plain.nodes.tracks_origins());
        assert!(tracked.nodes.tracks_origins());
        let encoded = serde_json::to_value(&plain).unwrap();
        assert!(encoded["nodes"].is_array());
        assert!(encoded.get("origins").is_none());
        assert_eq!(encoded.as_object().unwrap().len(), 2);
        let (_, generated) = output(&tracked, root);
        assert_eq!(generated.unknown_bytes(), 0);
    }

    #[test]
    fn a_mutated_slot_is_unknown_instead_of_inheriting_its_parent_mapping() {
        let (mut ast, root) = tracked("output.setNumber(1,2)");
        let id = locate(
            &ast,
            |node| matches!(node, Node::Num(value) if value.as_ref()=="2"),
        );
        assert!(ast.nodes.origin(id).is_some());
        ast.nodes[id as usize] = Node::Num("99".into());
        assert!(ast.nodes.origin(id).is_none());
        let (code, origins) = output(&ast, root);
        let unknown = origins
            .mappings
            .iter()
            .filter(|range| range.origin.is_none())
            .map(|range| &code[range.start..range.end])
            .collect::<Vec<_>>();
        assert_eq!(unknown, ["99"]);
        assert_eq!(origins.unknown_bytes(), 2);
    }

    #[test]
    fn clone_rollback_and_slot_reuse_keep_separate_lifetimes() {
        let (base, root) = tracked("return 1");
        let mut trial = base.clone();
        let id = locate(&trial, |node| matches!(node, Node::Num(_)));
        trial.nodes[id as usize] = Node::Num("2".into());
        assert!(base.nodes.origin(id).is_some());
        let checkpoint = trial.nodes.len();
        let temporary = trial.num("3");
        trial.nodes.mark_synthetic(temporary, "test-helper");
        trial.nodes.truncate(checkpoint);
        let reused = trial.num("4");
        assert_eq!(temporary, reused);
        assert_eq!(trial.nodes.origin(reused), None);
        trial.nodes = base.nodes.clone();
        assert_eq!(output(&trial, root), output(&base, root));
    }

    #[test]
    fn an_empty_inherited_arena_does_not_inherit_origin_by_node_number() {
        let (source, _) = tracked("return 1");
        let mut target = crate::ast_utils::inherit_ast(&source);
        let node = target.num("99");
        assert!(target.nodes.tracks_origins());
        assert_eq!(node, 0);
        assert!(source.nodes.origin(0).is_some());
        assert_eq!(target.nodes.origin(node), None);
    }

    #[test]
    fn a_known_rewrite_is_derived_from_the_entire_old_expression() {
        let source = "return 2 * 3";
        let (mut ast, root) = tracked(source);
        let node = locate(&ast, |node| matches!(node, Node::Bin(..)));
        ast.nodes
            .rewrite(node, Node::Num("6".into()), "constant-folding");
        let origin = ast.nodes.origin(node).unwrap();
        assert_eq!(origin.kind, OriginKind::Derived);
        let span = origin.primary.unwrap();
        assert_eq!(&source[span.start..span.end], "2 * 3");
        assert_eq!(origin.precision, OriginPrecision::Expression);
        let (code, origins) = output(&ast, root);
        assert!(code.ends_with('6'));
        assert_eq!(origins.unknown_bytes(), 0);
    }

    #[test]
    fn synthetic_and_unknown_are_distinct_without_invented_originals() {
        let (mut ast, root) = tracked("return 1");
        let node = locate(&ast, |node| matches!(node, Node::Num(_)));
        ast.nodes[node as usize] = Node::Num("42".into());
        assert_eq!(ast.nodes.origin(node), None);
        ast.nodes.mark_synthetic(node, "test-helper");
        let origin = ast.nodes.origin(node).unwrap();
        assert_eq!(origin.kind, OriginKind::Synthetic);
        assert_eq!(origin.primary, None);
        let (_, generated) = output(&ast, root);
        assert!(generated
            .origins
            .iter()
            .any(|origin| origin.kind == OriginKind::Synthetic));
    }

    #[test]
    fn copying_between_arenas_imports_snapshots_instead_of_aliasing_equal_ids() {
        let (source, _) = parse_source_with_origins("same.lua", "local first=1").unwrap();
        let (mut target, _) = parse_source_with_origins("same.lua", "local other=2").unwrap();
        let from = locate(&source, |node| matches!(node, Node::Num(_)));
        let to = target.num("1");
        target
            .nodes
            .derive_from(to, &source.nodes, from, "explicit-copy");
        let origin = target.nodes.origin(to).unwrap();
        let span = origin.primary.unwrap();
        assert_eq!(span.source, 1);
        let table = target.nodes.provenance().unwrap();
        assert_eq!(table.sources.len(), 2);
        assert_eq!(
            &table.sources[span.source as usize].text[span.start..span.end],
            "1"
        );
        assert!(table
            .sources
            .iter()
            .all(|source| source.name.as_ref() == "same.lua"));
    }

    #[test]
    fn copying_an_unknown_source_clears_any_old_destination_origin() {
        let (source, _) = parse_source("return 1").unwrap();
        let (mut target, _) = tracked("return 2");
        assert!(target.nodes.origin(0).is_some());
        target
            .nodes
            .derive_from(0, &source.nodes, 0, "copy-unknown");
        assert_eq!(target.nodes.origin(0), None);
    }

    #[test]
    fn serde_round_trip_retains_provenance_and_rejects_damaged_ranges() {
        let (ast, root) = tracked("-- 😀\r\nlocal x=1 return x");
        let value = serde_json::to_value(&ast).unwrap();
        assert!(value["nodes"].is_array());
        assert!(value["origins"].is_object());
        let restored: Ast = serde_json::from_value(value.clone()).unwrap();
        assert!(restored == ast);
        assert_eq!(output(&restored, root), output(&ast, root));
        let mut damaged = value.clone();
        damaged["origins"]["slots"].as_array_mut().unwrap().pop();
        assert!(serde_json::from_value::<Ast>(damaged).is_err());
        let mut damaged = value.clone();
        damaged["origins"]["slots"][0]["origin"]["primary"]["start"] = 4.into(); // inside emoji
        assert!(serde_json::from_value::<Ast>(damaged).is_err());
        let mut damaged = value;
        damaged["origins"]["slots"][0]["origin"]["primary"]["source"] = 123.into();
        assert!(serde_json::from_value::<Ast>(damaged).is_err());
    }

    #[test]
    fn names_serialize_as_a_json_array_after_the_vec_representation_change() {
        let (ast, _) = tracked("local x=1 return x");
        let value = serde_json::to_value(&ast).unwrap();
        let slots = value["origins"]["slots"].as_array().unwrap();
        let named = slots
            .iter()
            .find(|slot| {
                slot["names"]
                    .as_array()
                    .is_some_and(|names| !names.is_empty())
            })
            .expect("seeded name entry");
        assert!(named["names"].is_array());
        let restored: Ast = serde_json::from_value(value).unwrap();
        assert!(restored == ast);
    }

    #[test]
    fn name_origins_keep_inline_and_spilled_entries_equal_through_clone() {
        // Two entries must stay inline (no heap buffer), a third spills;
        // both shapes clone and serialize to the same JSON array entries.
        let entry = |site: NameSite| {
            (
                site,
                Origin::source(
                    SourceSpan {
                        source: 0,
                        start: 0,
                        end: 1,
                    },
                    OriginPrecision::Name,
                    Some("x".into()),
                )
                .into(),
            )
        };
        let mut inline: NameOrigins = NameOrigins::new();
        inline.push(entry(NameSite::Reference));
        inline.push(entry(NameSite::Binding(0)));
        assert!(!inline.spilled());
        assert_eq!(inline.clone(), inline);
        let mut spilled = inline.clone();
        spilled.push(entry(NameSite::Binding(1)));
        assert!(spilled.spilled());
        assert_eq!(spilled.len(), 3);
        assert_eq!(spilled.clone(), spilled);
        let value = serde_json::to_value(&spilled).unwrap();
        assert!(value.is_array());
        assert_eq!(value.as_array().unwrap().len(), 3);
        let restored: NameOrigins = serde_json::from_value(value).unwrap();
        assert_eq!(restored, spilled);
    }

    #[test]
    fn lexical_emission_retains_each_copied_token_and_leaves_gaps_unmapped() {
        let source = "-- 😀雪\r\nlocal x=0x10  -- tail\r\nreturn x";
        let normal = crate::print::lexical_minify(source).unwrap();
        let (code, origins) =
            crate::print::lexical_minify_with_origins("extended.lua", source).unwrap();
        assert_eq!(code, normal);
        assert_eq!(origins.sources[0].text.as_ref(), source);
        assert!(origins.unknown_bytes() > 0); // retained line breaks and inserted separators
        for mapping in &origins.mappings {
            if let Some(id) = mapping.origin {
                let span = origins.origins[id as usize].primary.unwrap();
                assert_eq!(
                    &code[mapping.start..mapping.end],
                    &source[span.start..span.end]
                );
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod owned_reason_tests {
    use super::*;

    // Independent definition of the prior borrowed derive/replace behavior.
    fn reference(mut origin: Origin, rule: &str, operation: ReasonOperation) -> Origin {
        if origin.kind != OriginKind::Synthetic {
            origin.kind = OriginKind::Derived;
        }
        origin.transformation = Some(rule.into());
        let reason = OptimizationReason::action(rule, ReasonOperation::Rewrite);
        if origin.reasons.last() != Some(&reason) {
            Arc::make_mut(&mut origin.reasons).push(reason);
        }
        if let Some(last) = Arc::make_mut(&mut origin.reasons).last_mut() {
            last.operation = operation;
        }
        origin
    }
    #[test]
    fn owned_derivation_preserves_repeated_relation_and_fact_history() {
        for synthetic in [false, true] {
            let original = if synthetic {
                Origin::synthetic("storage")
            } else {
                Origin::source(
                    SourceSpan {
                        source: 0,
                        start: 2,
                        end: 7,
                    },
                    OriginPrecision::Expression,
                    None,
                )
            };
            let mut actual = original.clone();
            let mut expected = original.clone();
            let decision = OptimizationReason::decision(
                "fold",
                "known-operands",
                [("value", "i64:6".to_owned())],
            );
            actual.add_reason(decision.clone());
            expected.add_reason(decision);
            for i in 0..200 {
                let rule = if i % 3 == 0 { "join" } else { "copy" };
                let operation = if i % 7 == 0 {
                    ReasonOperation::Rename
                } else {
                    ReasonOperation::Relate
                };
                expected = reference(expected, rule, operation);
                actual.derive_in_place(rule);
                Arc::make_mut(&mut actual.reasons)
                    .last_mut()
                    .unwrap()
                    .operation = operation;
            }
            assert_eq!(actual, expected);
            assert_eq!(
                serde_json::to_string(&actual).unwrap(),
                serde_json::to_string(&expected).unwrap()
            );
            assert_eq!(
                original.kind,
                if synthetic {
                    OriginKind::Synthetic
                } else {
                    OriginKind::Source
                }
            );
        }
    }
    #[test]
    fn uniquely_owned_reason_buffer_is_not_copied_on_every_append() {
        let mut origin = Origin::source(
            SourceSpan {
                source: 0,
                start: 0,
                end: 1,
            },
            OriginPrecision::Token,
            None,
        );
        Arc::make_mut(&mut origin.reasons).reserve(128);
        let allocation = Arc::as_ptr(&origin.reasons);
        let buffer = origin.reasons.as_ptr();
        for _ in 0..100 {
            origin.derive_in_place("source-contribution");
            Arc::make_mut(&mut origin.reasons)
                .last_mut()
                .unwrap()
                .operation = ReasonOperation::Relate;
            assert_eq!(Arc::as_ptr(&origin.reasons), allocation);
            assert_eq!(origin.reasons.as_ptr(), buffer);
        }
        assert_eq!(
            origin.reasons.len(),
            100,
            "Repeated Relate actions are not silently deduplicated"
        );
        let snapshot = origin.clone();
        origin.derive_in_place("different-rule");
        assert_eq!(
            snapshot.reasons.len(),
            100,
            "A shared candidate must remain immutable"
        );
        assert_eq!(origin.reasons.len(), 101);
    }
}

/// Delayed resolution of final emitted positions. This is an internal compiler
/// transport optimization: serialization still emits the ordinary GeneratedOrigins.
#[doc(hidden)]
pub struct DeferredGeneratedOrigins {
    input: Option<PendingOriginInput>,
    resolved: std::sync::OnceLock<GeneratedOrigins>,
}
struct PendingOriginInput {
    arena: ArenaOrigins,
    printed: PrintedSource,
}
impl DeferredGeneratedOrigins {
    /// Keep only slots actually referenced by this immutable final print.
    /// Fully referenced pages are shared with the original arena instead of
    /// cloning every slot: resolution only ever looks up slots of printed
    /// nodes (see from_arena), so the resolved origins are unchanged,
    /// while the per-slot clone cost of the old flat copy disappears.
    pub fn from_print(ast: &Ast, printed: PrintedSource) -> Option<Self> {
        let original = ast.nodes.provenance()?;
        let mut referenced = vec![false; original.slots.len()];
        for id in std::iter::once(printed.root).chain(printed.emissions.iter().map(|e| e.node)) {
            referenced[id as usize] = true;
        }
        let arena = ArenaOrigins {
            sources: Arc::clone(&original.sources),
            slots: Arc::new(original.slots.select(&referenced)),
            dispositions: Arc::clone(&original.dispositions),
        };
        Some(Self {
            input: Some(PendingOriginInput { arena, printed }),
            resolved: std::sync::OnceLock::new(),
        })
    }
    /// Ready origins use the same container for identity and lexical paths.
    pub fn ready(value: GeneratedOrigins) -> Self {
        Self {
            input: None,
            resolved: std::sync::OnceLock::from(value),
        }
    }
    /// Resolve once when selected, inspected or sent across a worker boundary.
    #[expect(
        clippy::expect_used,
        reason = "A deferred value is constructed only from a tracked, immutable print; its referenced slots are retained"
    )]
    pub fn get(&self) -> &GeneratedOrigins {
        self.resolved.get_or_init(|| {
            let input = self
                .input
                .as_ref()
                .expect("unresolved print has captured origins");
            GeneratedOrigins::from_arena(&input.arena, &input.printed)
                .expect("captured print retains its origin table")
        })
    }
    /// Move the selected result without retaining discarded print/slot storage.
    #[expect(
        clippy::expect_used,
        reason = "Ready values own a result, deferred values own the exact tracked print"
    )]
    pub fn into_owned(self) -> GeneratedOrigins {
        if let Some(resolved) = self.resolved.into_inner() {
            return resolved;
        }
        let input = self.input.expect("unresolved print has captured origins");
        GeneratedOrigins::from_arena(&input.arena, &input.printed)
            .expect("captured print retains its origin table")
    }
}
impl std::fmt::Debug for DeferredGeneratedOrigins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.get().fmt(f)
    }
}
impl Serialize for DeferredGeneratedOrigins {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.get().serialize(s)
    }
}
impl<'de> Deserialize<'de> for DeferredGeneratedOrigins {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        GeneratedOrigins::deserialize(d).map(Self::ready)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod deferred_final_tests {
    use super::*;
    use crate::{parse_source_with_origins, Printer};
    #[test]
    fn deferred_final_snapshot_is_exact_and_independent_of_later_arena_changes() {
        for text in [
            "local x=2 output.setNumber(1,x)",
            "-- 雪😀\r\nfunction onTick()output.setNumber(1,3)end",
            "local unused=99",
            "",
        ] {
            let (mut ast, root) = parse_source_with_origins("deferred.lua", text).unwrap();
            let before = serde_json::to_vec(&ast).unwrap();
            for zero in [false, true] {
                let printed = Printer::new(&ast, zero).output_with_positions(root);
                let expected = GeneratedOrigins::from_print(&ast, &printed).unwrap();
                let pending = DeferredGeneratedOrigins::from_print(&ast, printed).unwrap();
                assert!(pending.resolved.get().is_none());
                let mut changed = ast.clone();
                let value = changed.node(root).clone();
                changed.nodes[root as usize] = value;
                assert_eq!(pending.get(), &expected);
                assert_eq!(
                    serde_json::to_vec(&pending).unwrap(),
                    serde_json::to_vec(&expected).unwrap()
                );
                let decoded: DeferredGeneratedOrigins =
                    serde_json::from_slice(&serde_json::to_vec(&pending).unwrap()).unwrap();
                assert_eq!(decoded.into_owned(), expected);
                assert_eq!(pending.into_owned(), expected);
            }
            assert_eq!(serde_json::to_vec(&ast).unwrap(), before);
            ast.nodes.retain_block_statements(|_| false, "remove-all");
            let printed = Printer::new(&ast, false).output_with_positions(root);
            let expected = GeneratedOrigins::from_print(&ast, &printed).unwrap();
            let pending = DeferredGeneratedOrigins::from_print(&ast, printed).unwrap();
            assert_eq!(
                pending.into_owned(),
                expected,
                "removed roots survive even empty output"
            );
        }
    }
}

#[cfg(test)]
impl GeneratedOrigins {
    fn reference_copied_tokens(
        name: &str,
        source: &str,
        generated_len: usize,
        tokens: &[CopiedToken],
    ) -> Self {
        let mut output = Self {
            sources: Arc::new(vec![SourceSnapshot {
                name: name.into(),
                text: source.into(),
            }]),
            origins: Vec::new(),
            mappings: Vec::new(),
            contexts: Vec::new(),
            constructs: Vec::new(),
            dispositions: empty_dispositions(),
        };
        let mut interned = HashMap::new();
        let mut cursor = 0;
        for token in tokens {
            if cursor < token.start {
                output.append(cursor, token.start, None, &mut interned);
            }
            let origin = Origin::source(
                SourceSpan {
                    source: 0,
                    start: token.source_start,
                    end: token.source_end,
                },
                if token.is_name {
                    OriginPrecision::Name
                } else {
                    OriginPrecision::Token
                },
                token
                    .is_name
                    .then(|| source[token.source_start..token.source_end].into()),
            );
            output.append_detail(
                token.start,
                token.end(),
                Some(&origin),
                Some(SourceSpan {
                    source: 0,
                    start: token.source_start,
                    end: token.source_end,
                }),
                Vec::new(),
                &mut interned,
            );
            cursor = token.end();
        }
        if cursor < generated_len {
            output.append(cursor, generated_len, None, &mut interned);
        }
        output
    }
}
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod unique_lexical_origin_tests {
    use super::*;
    #[test]
    fn unique_copy_ranges_match_the_generic_interner_including_gaps() {
        for text in [
            "",
            "-- nothing
",
            "-- 雪😀\r\nlocal x='雪😀' return x,x,x",
            "local _ENV=_ENV return [=[text]=]..'x'",
            "if a then b(1)else b(2)end",
        ] {
            let tokens = crate::lexer::Lexer::new(text).all_borrowed().unwrap();
            let copies = tokens
                .iter()
                .filter(|t| t.k != crate::lexer::TokenKind::Eof)
                .map(|t| CopiedToken {
                    start: t.p,
                    source_start: t.p,
                    source_end: t.p + t.v.len(),
                    is_name: t.k == crate::lexer::TokenKind::Id,
                    renamed: false,
                    generated_end: t.p + t.v.len(),
                })
                .collect::<Vec<_>>();
            let previous =
                GeneratedOrigins::reference_copied_tokens("lexical.lua", text, text.len(), &copies);
            let actual = GeneratedOrigins::copied_tokens("lexical.lua", text, text.len(), &copies);
            assert_eq!(actual, previous);
            actual.validate_for_code(text).unwrap();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod repeated_lexical_name_tests {
    use super::*;
    #[test]
    fn repeated_spelling_shares_storage_but_not_origin_identity_or_source_range() {
        let unit = "output.setNumber(1,value) ";
        let text = unit.repeat(128);
        let (code, map) = crate::print::lexical_minify_with_origins("names.lua", &text).unwrap();
        map.validate_for_code(&code).unwrap();
        let names = map
            .origins
            .iter()
            .enumerate()
            .filter(|(_, o)| o.name.as_deref() == Some("output"))
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 128);
        for (n, (id, origin)) in names.iter().enumerate() {
            assert_eq!(
                origin.primary.unwrap(),
                SourceSpan {
                    source: 0,
                    start: n * unit.len(),
                    end: n * unit.len() + 6
                }
            );
            assert_eq!(origin.precision, OriginPrecision::Name);
            assert!(map.mappings.iter().any(|m| m.origin == Some(*id as u32)));
            assert!(Arc::ptr_eq(
                origin.name.as_ref().unwrap(),
                names[0].1.name.as_ref().unwrap()
            ));
        }
        let (_, independent) =
            crate::print::lexical_minify_with_origins("names.lua", &text).unwrap();
        let second = independent
            .origins
            .iter()
            .find(|o| o.name.as_deref() == Some("output"))
            .unwrap();
        assert!(
            !Arc::ptr_eq(
                second.name.as_ref().unwrap(),
                names[0].1.name.as_ref().unwrap()
            ),
            "interning does not retain user text across compilations"
        );
        assert_eq!(map, independent);
    }
}
