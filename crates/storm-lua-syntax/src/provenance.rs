//! Source origins carried independently of AST equality and optimization decisions.
//! Unknown origin is `None`, never a nearby source position or a synthetic claim.

use crate::explanation::{
    InlineContext, OptimizationReason, ReasonOperation, SourceDisposition, SourceRelation,
    SyntaxSite,
};
use crate::{Ast, NameSite, Node, NodeId, NodePositions, PrintedSource};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

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

impl Origin {
    /// Attribute a construct to one original source range.
    pub fn source(span: SourceSpan, precision: OriginPrecision, name: Option<Arc<str>>) -> Self {
        Self {
            kind: OriginKind::Source,
            primary: Some(span),
            related: Arc::default(),
            transformation: None,
            precision,
            name,
            relations: Arc::default(),
            reasons: Arc::default(),
        }
    }
    /// Describe compiler-created code without inventing a source location.
    pub fn synthetic(transformation: &str) -> Self {
        Self {
            kind: OriginKind::Synthetic,
            primary: None,
            related: Arc::default(),
            transformation: Some(transformation.into()),
            precision: OriginPrecision::Group,
            name: None,
            relations: Arc::default(),
            reasons: Arc::new(vec![OptimizationReason::action(
                transformation,
                ReasonOperation::Synthesize,
            )]),
        }
    }
    /// Preserve source attribution but identify it as a transformation result.
    pub fn derived(&self, transformation: &str) -> Self {
        let mut result = self.clone();
        if result.kind != OriginKind::Synthetic {
            result.kind = OriginKind::Derived;
        }
        result.transformation = Some(transformation.into());
        result.add_reason(OptimizationReason::action(
            transformation,
            ReasonOperation::Rewrite,
        ));
        result
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

/// One arena slot's origin, shared when candidate ASTs are cloned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NodeOrigin {
    pub origin: Option<Origin>,
    // A vector keeps parameterized NameSite variants valid in JSON (not object keys).
    pub names: Vec<(NameSite, Origin)>,
    pub contexts: Arc<Vec<InlineContext>>,
    pub tokens: Vec<(SyntaxSite, Origin)>,
    pub dispositions: Arc<Vec<SourceDisposition>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArenaOrigins {
    pub sources: Arc<Vec<SourceSnapshot>>,
    pub slots: Vec<Option<Arc<NodeOrigin>>>,
    pub dispositions: Arc<Vec<SourceDisposition>>,
}
impl ArenaOrigins {
    pub fn seed(
        ast: &Ast,
        positions: &NodePositions,
        name: &str,
        text: &str,
    ) -> Result<Self, String> {
        let sources = Arc::new(vec![SourceSnapshot {
            name: name.into(),
            text: text.into(),
        }]);
        let tokens = crate::lexer::Lexer::new(text)
            .all()
            .map_err(|e| e.to_string())?;
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
                    SyntaxSite::Begin => tokens.get(tokens.partition_point(|t| t.p < start)),
                    SyntaxSite::End => {
                        tokens.get(tokens.partition_point(|t| t.p < end).saturating_sub(1))
                    }
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
                            tokens.partition_point(|t| t.p < start).checked_sub(1)
                        })
                        .and_then(|i| tokens.get(i))
                    }
                    SyntaxSite::Operator => {
                        let offset = if let Node::Bin(_, left, _) = node {
                            positions.span(*left).ok_or("operator left span missing")?.1
                        } else {
                            start
                        };
                        tokens.get(tokens.partition_point(|t| t.p < offset))
                    }
                };
                if let Some(token) =
                    token.filter(|t| t.v == expected && t.p >= start && t.p + t.v.len() <= end)
                {
                    syntax_tokens.push((
                        site,
                        Origin::source(
                            SourceSpan {
                                source: 0,
                                start: token.p,
                                end: token.p + token.v.len(),
                            },
                            OriginPrecision::Token,
                            None,
                        ),
                    ));
                }
            }
            slots.push(Some(Arc::new(NodeOrigin {
                origin: Some(Origin::source(
                    SourceSpan {
                        source: 0,
                        start,
                        end,
                    },
                    precision,
                    None,
                )),
                names: Vec::new(),
                contexts: Arc::default(),
                tokens: syntax_tokens,
                dispositions: Arc::default(),
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
                ),
            ));
        }
        let result = Self {
            sources,
            slots,
            dispositions: Arc::default(),
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
                origin.validate(&self.sources)?;
            }
            for (_, origin) in &slot.tokens {
                origin.validate(&self.sources)?;
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
                .flat_map(|o| o.reasons.iter())
                .chain(slot.names.iter().flat_map(|(_, o)| o.reasons.iter()))
            {
                if reason.code.is_empty() {
                    return Err("optimization reason has empty code".into());
                }
            }
            let mut sites = BTreeSet::new();
            for (site, origin) in &slot.names {
                if !sites.insert(*site) {
                    return Err("duplicate name occurrence in origin table".into());
                }
                origin.validate(&self.sources)?;
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
        let mut events = Vec::with_capacity(printed.emissions.len() * 2);
        for (index, emission) in printed.emissions.iter().enumerate() {
            assert!(emission.start < emission.end && emission.end <= printed.code.len());
            events.push((emission.start, true, index));
            events.push((emission.end, false, index));
        }
        events.sort_unstable();
        // More specific ranges win; name sites win ties; children are emitted before parents.
        let key = |index: usize| {
            let emission = &printed.emissions[index];
            (
                emission.end - emission.start,
                emission.site.is_none(),
                index,
            )
        };
        let mut active = BTreeSet::<(usize, bool, usize)>::new();
        let mut result = Self {
            sources: Arc::clone(&arena.sources),
            origins: Vec::new(),
            mappings: Vec::new(),
            contexts: Vec::new(),
            constructs: Vec::new(),
            dispositions: Arc::clone(&arena.dispositions),
        };
        let mut interned = HashMap::new();
        let mut context_ids = HashMap::<InlineContext, u32>::new();
        let has_contexts = arena
            .slots
            .iter()
            .flatten()
            .any(|slot| !slot.contexts.is_empty());
        let mut cursor = 0;
        for (offset, start, index) in events {
            if cursor < offset {
                let emission = active
                    .first()
                    .map(|&(_, _, index)| &printed.emissions[index]);
                let origin = emission.and_then(|emission| {
                    let slot = arena.slots[emission.node as usize].as_ref()?;
                    emission
                        .token
                        .and_then(|site| {
                            slot.tokens
                                .iter()
                                .find(|(key, _)| *key == site)
                                .map(|(_, o)| o)
                        })
                        .or_else(|| {
                            emission
                                .site
                                .and_then(|site| {
                                    slot.names
                                        .iter()
                                        .find(|(key, _)| *key == site)
                                        .map(|(_, value)| value)
                                })
                                .or(slot.origin.as_ref())
                        })
                });
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
                if has_contexts {
                    for &(_, _, index) in active.iter().rev() {
                        if let Some(slot) =
                            arena.slots[printed.emissions[index].node as usize].as_ref()
                        {
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
                    }
                }
                result.append_detail(cursor, offset, origin, copied, contexts, &mut interned);
                cursor = offset;
            }
            if start {
                active.insert(key(index));
            } else {
                active.remove(&key(index));
            }
        }
        if cursor < printed.code.len() {
            result.append(cursor, printed.code.len(), None, &mut interned);
        }
        // Include the selected root even when every statement was eliminated.
        // Dead speculative arena slots are deliberately not inspected.
        let mut disposition_nodes = std::collections::HashSet::new();
        for node in std::iter::once(printed.root).chain(printed.emissions.iter().map(|e| e.node)) {
            if disposition_nodes.insert(node) {
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
                .and_then(|slot| slot.origin.as_ref());
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
            let id = origin.map(|origin| {
                *interned.entry(origin.clone()).or_insert_with(|| {
                    let id = result.origins.len() as u32;
                    result.origins.push(origin.clone());
                    id
                })
            });
            result.constructs.push(GeneratedConstruct {
                start: emission.start,
                end: emission.end,
                origin: id,
            });
        }
        Some(result)
    }
    fn append(
        &mut self,
        start: usize,
        end: usize,
        origin: Option<&Origin>,
        interned: &mut HashMap<Origin, u32>,
    ) {
        self.append_detail(start, end, origin, None, Vec::new(), interned);
    }
    fn append_detail(
        &mut self,
        start: usize,
        end: usize,
        origin: Option<&Origin>,
        copied: Option<SourceSpan>,
        contexts: Vec<u32>,
        interned: &mut HashMap<Origin, u32>,
    ) {
        let id = origin.map(|origin| {
            if let Some(&id) = interned.get(origin) {
                return id;
            }
            let id = self.origins.len() as u32;
            self.origins.push(origin.clone());
            interned.insert(origin.clone(), id);
            id
        });
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
            dispositions: Arc::default(),
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

pub(crate) struct CopiedToken {
    pub start: usize,
    pub end: usize,
    pub source_start: usize,
    pub source_end: usize,
    pub is_name: bool,
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
            dispositions: Arc::default(),
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
                token.end,
                Some(&origin),
                Some(SourceSpan {
                    source: 0,
                    start: token.source_start,
                    end: token.source_end,
                }),
                Vec::new(),
                &mut interned,
            );
            cursor = token.end;
        }
        if cursor < generated_len {
            output.append(cursor, generated_len, None, &mut interned);
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
                Origin::source(span, OriginPrecision::Expression, None).validate(&self.sources)?;
            }
        }
        for disposition in self.dispositions.iter() {
            for span in
                std::iter::once(&disposition.original).chain(disposition.replacement_sources.iter())
            {
                Origin::source(*span, OriginPrecision::Statement, None).validate(&self.sources)?;
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
                Origin::source(copied, OriginPrecision::Token, None).validate(&self.sources)?;
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
        let temporary = trial.num("3".into());
        trial.nodes.mark_synthetic(temporary, "test-helper");
        trial.nodes.truncate(checkpoint);
        let reused = trial.num("4".into());
        assert_eq!(temporary, reused);
        assert_eq!(trial.nodes.origin(reused), None);
        trial.nodes = base.nodes.clone();
        assert_eq!(output(&trial, root), output(&base, root));
    }

    #[test]
    fn an_empty_inherited_arena_does_not_inherit_origin_by_node_number() {
        let (source, _) = tracked("return 1");
        let mut target = crate::ast_utils::inherit_ast(&source);
        let node = target.num("99".into());
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
        let to = target.num("1".into());
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
