//! Source origins carried independently of AST equality and optimization decisions.
//! Unknown origin is `None`, never a nearby source position or a synthetic claim.

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
    pub related: Vec<SourceSpan>,
    /// Last explicitly recorded transformation, not a timing-dependent decision.
    pub transformation: Option<Arc<str>>,
    /// What the primary attribution describes.
    pub precision: OriginPrecision,
    /// Original spelling for a source identifier occurrence, when known.
    pub name: Option<Arc<str>>,
}

impl Origin {
    /// Attribute a construct to one original source range.
    pub fn source(span: SourceSpan, precision: OriginPrecision, name: Option<Arc<str>>) -> Self {
        Self {
            kind: OriginKind::Source,
            primary: Some(span),
            related: Vec::new(),
            transformation: None,
            precision,
            name,
        }
    }
    /// Describe compiler-created code without inventing a source location.
    pub fn synthetic(transformation: &str) -> Self {
        Self {
            kind: OriginKind::Synthetic,
            primary: None,
            related: Vec::new(),
            transformation: Some(transformation.into()),
            precision: OriginPrecision::Group,
            name: None,
        }
    }
    /// Preserve source attribution but identify it as a transformation result.
    pub fn derived(&self, transformation: &str) -> Self {
        let mut result = self.clone();
        if result.kind != OriginKind::Synthetic {
            result.kind = OriginKind::Derived;
        }
        result.transformation = Some(transformation.into());
        result
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
        for span in self.primary.iter().chain(self.related.iter()) {
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
        Ok(())
    }
}

/// One arena slot's origin, shared when candidate ASTs are cloned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NodeOrigin {
    pub origin: Option<Origin>,
    // A vector keeps parameterized NameSite variants valid in JSON (not object keys).
    pub names: Vec<(NameSite, Origin)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArenaOrigins {
    pub sources: Arc<Vec<SourceSnapshot>>,
    pub slots: Vec<Option<Arc<NodeOrigin>>>,
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
        let result = Self { sources, slots };
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
pub struct OriginMapping {
    /// Inclusive generated UTF-8 byte offset.
    pub start: usize,
    /// Exclusive generated UTF-8 byte offset.
    pub end: usize,
    /// Index in GeneratedOrigins.origins, or unknown attribution.
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
        };
        let mut interned = HashMap::new();
        let mut cursor = 0;
        for (offset, start, index) in events {
            if cursor < offset {
                let origin = active.first().and_then(|&(_, _, index)| {
                    let emission = &printed.emissions[index];
                    let slot = arena.slots[emission.node as usize].as_ref()?;
                    emission
                        .site
                        .and_then(|site| {
                            slot.names
                                .iter()
                                .find(|(key, _)| *key == site)
                                .map(|(_, value)| value)
                        })
                        .or(slot.origin.as_ref())
                });
                result.append(cursor, offset, origin, &mut interned);
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
        Some(result)
    }
    fn append(
        &mut self,
        start: usize,
        end: usize,
        origin: Option<&Origin>,
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
        if let Some(previous) = self
            .mappings
            .last_mut()
            .filter(|previous| previous.end == start && previous.origin == id)
        {
            previous.end = end;
        } else {
            self.mappings.push(OriginMapping {
                start,
                end,
                origin: id,
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
            mappings: if source.is_empty() {
                Vec::new()
            } else {
                vec![OriginMapping {
                    start: 0,
                    end: source.len(),
                    origin: Some(0),
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
            output.append(token.start, token.end, Some(&origin), &mut interned);
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
