//! Slice-compatible copy-on-write node storage with an optional provenance side table.
//! Read-only candidate clones share buffers. The first actual mutation detaches
//! them; the serialized node/metadata representation is unchanged.
//! Mutable indexing is deliberately untracked: it invalidates that slot's origin.
//! Annotated transformations explicitly preserve, derive, merge, or copy origins.
use crate::ast::{Node, NodeId, SymbolId};
use crate::deferred_origin::StoredOrigin;
use crate::explanation::{
    InlineContext, OptimizationReason, ReasonOperation, RelationRole, SourceDisposition,
    SourceRelation,
};
use crate::parser::NameSite;
use crate::provenance::{ArenaOrigins, NodeOrigin, Origin, SourceSnapshot};
use serde::{Deserialize, Deserializer, Serialize};
use std::ops::{Deref, Index, IndexMut};
use std::sync::Arc;

/// Arena node storage. Equality compares syntax only, never provenance metadata.
/// Ast preserves the historical untracked JSON shape; binary Worker contexts use
/// a sized representation and must be exchanged between the same compiler version.
#[derive(Clone, Default, Serialize)]
pub struct NodeArena {
    #[serde(rename = "nodes")]
    values: Arc<Vec<Node>>,
    origins: Option<ArenaOrigins>,
}

impl PartialEq for NodeArena {
    fn eq(&self, other: &Self) -> bool {
        self.values == other.values
    }
}
impl Eq for NodeArena {}
impl Deref for NodeArena {
    type Target = [Node];
    fn deref(&self) -> &[Node] {
        &self.values
    }
}
impl Index<usize> for NodeArena {
    type Output = Node;
    fn index(&self, index: usize) -> &Node {
        &self.values[index]
    }
}
impl IndexMut<usize> for NodeArena {
    fn index_mut(&mut self, index: usize) -> &mut Node {
        if let Some(origins) = &mut self.origins {
            Arc::make_mut(&mut origins.slots)[index] = None;
        }
        &mut Arc::make_mut(&mut self.values)[index]
    }
}
impl<'a> IntoIterator for &'a NodeArena {
    type Item = &'a Node;
    type IntoIter = std::slice::Iter<'a, Node>;
    fn into_iter(self) -> Self::IntoIter {
        self.values.iter()
    }
}
impl<'a> IntoIterator for &'a mut NodeArena {
    type Item = &'a mut Node;
    type IntoIter = std::slice::IterMut<'a, Node>;
    fn into_iter(self) -> Self::IntoIter {
        if let Some(origins) = &mut self.origins {
            Arc::make_mut(&mut origins.slots).fill(None);
        }
        Arc::make_mut(&mut self.values).iter_mut()
    }
}
impl<'de> Deserialize<'de> for NodeArena {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            nodes: Vec<Node>,
            origins: Option<ArenaOrigins>,
        }
        let value = Wire::deserialize(deserializer)?;
        if let Some(origins) = &value.origins {
            origins
                .validate(value.nodes.len())
                .map_err(serde::de::Error::custom)?;
        }
        Ok(Self {
            values: Arc::new(value.nodes),
            origins: value.origins,
        })
    }
}

/// An explicitly captured slot provenance. It is not inferred from a reused NodeId.
#[derive(Clone)]
pub struct OriginSnapshot {
    sources: Arc<Vec<SourceSnapshot>>,
    node: Node,
    origin: Arc<NodeOrigin>,
}

#[derive(Clone)]
struct ContributionSnapshot {
    sources: Arc<Vec<SourceSnapshot>>,
    origin: Origin,
}

#[derive(PartialEq)]
enum NameValue<'a> {
    Symbol(SymbolId),
    Text(&'a str),
}
fn name_value(node: &Node, site: NameSite) -> Option<NameValue<'_>> {
    let symbol = match (node, site) {
        (Node::Name(symbol), NameSite::Reference)
        | (Node::Localfunc(symbol, _), NameSite::Binding(0))
        | (Node::Fornum(symbol, ..), NameSite::Binding(0))
        | (Node::Methodname(_, symbol), NameSite::Member)
        | (Node::Goto(symbol) | Node::Label(symbol), NameSite::Label) => Some(*symbol),
        (Node::Local(symbols, _) | Node::Forin(symbols, ..), NameSite::Binding(index))
        | (Node::Function(symbols, ..), NameSite::Parameter(index)) => {
            symbols.get(index as usize).copied()
        }
        (Node::Table(fields), NameSite::Field(index)) => match fields.get(index as usize) {
            Some(crate::TableField::Name(symbol, _)) => Some(*symbol),
            _ => None,
        },
        (Node::Call(_, _, Some(method)), NameSite::Member) => return Some(NameValue::Text(method)),
        // A dot field is represented by a string node. Its explicit parser member
        // slot remains meaningful only while that exact literal stays unchanged.
        (Node::Str(text), NameSite::Member) => return Some(NameValue::Text(text)),
        _ => None,
    };
    symbol.map(NameValue::Symbol)
}

impl NodeArena {
    pub(crate) fn from_parts(
        values: Vec<Node>,
        origins: Option<ArenaOrigins>,
    ) -> Result<Self, String> {
        if let Some(table) = &origins {
            table.validate(values.len())?;
        }
        Ok(Self {
            values: Arc::new(values),
            origins,
        })
    }

    /// Append a node. New nodes have unknown origin until a transformation attributes them.
    pub fn push(&mut self, node: Node) {
        Arc::make_mut(&mut self.values).push(node);
        if let Some(origins) = &mut self.origins {
            Arc::make_mut(&mut origins.slots).push(None);
        }
    }
    /// Remove statements without invalidating unrelated nodes inspected by the
    /// cleanup traversal. Surviving children retain their own source origins.
    pub fn retain_block_statements(
        &mut self,
        mut keep: impl FnMut(NodeId) -> bool,
        transformation: &str,
    ) {
        let mut removed = Vec::new();
        let mut changes = Vec::new();
        for (id, node) in self.values.iter().enumerate() {
            let Node::Block(statements) = node else {
                continue;
            };
            let Some(first) = statements.iter().position(|&statement| !keep(statement)) else {
                continue;
            };
            let mut retained = statements[..first].to_vec();
            if self.origins.is_some() {
                removed.push((id as NodeId, statements[first]));
            }
            for &statement in &statements[first + 1..] {
                if keep(statement) {
                    retained.push(statement);
                } else if self.origins.is_some() {
                    removed.push((id as NodeId, statement));
                }
            }
            changes.push((id, retained));
        }
        for (id, statements) in changes {
            Arc::make_mut(&mut self.values)[id] = Node::Block(statements);
            if let Some(slot) = self
                .origins
                .as_mut()
                .and_then(|table| Arc::make_mut(&mut table.slots)[id].as_mut())
            {
                if let Some(origin) = Arc::make_mut(slot).origin.as_mut() {
                    origin.derive_in_place(transformation);
                }
            }
        }
        for (owner, node) in removed {
            self.record_removal(owner, node, transformation);
        }
    }

    /// Discard trial nodes and their origins together. Reused slots start unknown.
    pub fn truncate(&mut self, len: usize) {
        if len >= self.values.len() {
            return;
        }
        Arc::make_mut(&mut self.values).truncate(len);
        if let Some(origins) = &mut self.origins {
            Arc::make_mut(&mut origins.slots).truncate(len);
        }
    }
    /// Create an empty arena sharing only source snapshots, not node-number attributions.
    pub fn empty_like(&self) -> Self {
        Self {
            values: Arc::default(),
            origins: self.origins.as_ref().map(|origins| ArenaOrigins {
                sources: Arc::clone(&origins.sources),
                slots: Arc::default(),
                dispositions: Arc::clone(&origins.dispositions),
            }),
        }
    }
    /// Whether source provenance is being collected for this arena.
    pub fn tracks_origins(&self) -> bool {
        self.origins.is_some()
    }
    /// Clone syntax for a temporary cost model without carrying debug provenance.
    /// The returned NodeIds and symbol IDs stay identical; optimizers must never
    /// commit this arena when source attribution is required.
    pub(crate) fn clone_without_origins(&self) -> Self {
        Self {
            values: self.values.clone(),
            origins: None,
        }
    }
    pub(crate) fn provenance(&self) -> Option<&ArenaOrigins> {
        self.origins.as_ref()
    }
    pub(crate) fn set_provenance(&mut self, origins: ArenaOrigins) {
        self.origins = Some(origins);
    }
    /// Return the current source attribution. An untracked mutation yields None.
    pub fn origin(&self, node: NodeId) -> Option<&Origin> {
        self.origin_record(node).map(StoredOrigin::get)
    }
    fn origin_record(&self, node: NodeId) -> Option<&StoredOrigin> {
        self.origins
            .as_ref()?
            .slots
            .get(node as usize)?
            .as_ref()?
            .origin
            .as_ref()
    }
    /// Test for available attribution without materializing its reason history.
    pub fn has_origin(&self, node: NodeId) -> bool {
        self.origin_record(node).is_some()
    }
    /// Read the explicit primary source without flattening candidate explanations.
    pub fn primary_origin(&self, node: NodeId) -> Option<crate::provenance::SourceSpan> {
        self.origin_record(node).and_then(|o| o.value.primary)
    }
    /// Return one identifier occurrence's attribution, independently of the node's anchor.
    pub fn name_origin(&self, node: NodeId, site: NameSite) -> Option<&Origin> {
        self.name_record(node, site).map(StoredOrigin::get)
    }
    fn name_record(&self, node: NodeId, site: NameSite) -> Option<&StoredOrigin> {
        self.origins
            .as_ref()?
            .slots
            .get(node as usize)?
            .as_ref()?
            .names
            .iter()
            .find(|(key, _)| *key == site)
            .map(|(_, origin)| origin)
    }
    /// Capture before a known transformation. Untracked compilation does not clone a node.
    pub fn capture_origin(&self, node: NodeId) -> Option<OriginSnapshot> {
        let origins = self.origins.as_ref()?;
        let origin = origins.slots.get(node as usize)?.as_ref()?;
        Some(OriginSnapshot {
            sources: Arc::clone(&origins.sources),
            node: self.values[node as usize].clone(),
            origin: Arc::clone(origin),
        })
    }

    fn capture_contribution(&self, node: NodeId) -> Option<ContributionSnapshot> {
        let origins = self.origins.as_ref()?;
        let origin = origins
            .slots
            .get(node as usize)?
            .as_ref()?
            .origin
            .as_ref()?
            .value
            .clone();
        Some(ContributionSnapshot {
            sources: Arc::clone(&origins.sources),
            origin,
        })
    }
    fn restore_shared(&mut self, node: NodeId, snapshot: &OriginSnapshot) -> bool {
        let Some(origins) = &mut self.origins else {
            return false;
        };
        if !Arc::ptr_eq(&origins.sources, &snapshot.sources) {
            return false;
        }
        if origins.slots[node as usize]
            .as_ref()
            .is_some_and(|slot| Arc::ptr_eq(slot, &snapshot.origin))
        {
            return true;
        }
        Arc::make_mut(&mut origins.slots)[node as usize] = Some(Arc::clone(&snapshot.origin));
        true
    }

    fn import(&mut self, snapshot: &OriginSnapshot) -> NodeOrigin {
        let Some(origins) = self.origins.as_mut() else {
            unreachable!("origin import requires a tracked target")
        };
        let mut imported = (*snapshot.origin).clone();
        if Arc::ptr_eq(&origins.sources, &snapshot.sources) || origins.sources == snapshot.sources {
            return imported;
        }
        let mut source_ids = Vec::with_capacity(snapshot.sources.len());
        for source in snapshot.sources.iter() {
            let id = if let Some(id) = origins
                .sources
                .iter()
                .position(|existing| existing == source)
            {
                id
            } else {
                let values = Arc::make_mut(&mut origins.sources);
                let id = values.len();
                values.push(source.clone());
                id
            };
            source_ids.push(id as u32);
        }
        for value in imported
            .origin
            .iter_mut()
            .chain(
                Arc::make_mut(&mut imported.names)
                    .iter_mut()
                    .map(|(_, origin)| origin),
            )
            .chain(
                Arc::make_mut(&mut imported.tokens)
                    .iter_mut()
                    .map(|(_, origin)| origin),
            )
        {
            value.invalidate();
            for range in value
                .value
                .primary
                .iter_mut()
                .chain(Arc::make_mut(&mut value.value.related).iter_mut())
                .chain(
                    Arc::make_mut(&mut value.value.relations)
                        .iter_mut()
                        .map(|r| &mut r.span),
                )
            {
                range.source = source_ids[range.source as usize];
            }
        }
        for disposition in Arc::make_mut(&mut imported.dispositions) {
            disposition.original.source = source_ids[disposition.original.source as usize];
            for span in &mut disposition.replacement_sources {
                span.source = source_ids[span.source as usize];
            }
        }
        for context in Arc::make_mut(&mut imported.contexts) {
            context.definition.source = source_ids[context.definition.source as usize];
            context.call_site.source = source_ids[context.call_site.source as usize];
        }
        imported
    }
    // A detached slot can often be moved, not cloned. Consuming the snapshot
    // prevents its own temporary Arc from forcing a copy of every reason vector.
    fn import_owned(&mut self, snapshot: OriginSnapshot) -> (Node, NodeOrigin) {
        if self
            .origins
            .as_ref()
            .is_some_and(|table| Arc::ptr_eq(&table.sources, &snapshot.sources))
        {
            (snapshot.node, Arc::unwrap_or_clone(snapshot.origin))
        } else {
            let imported = self.import(&snapshot);
            (snapshot.node, imported)
        }
    }

    /// Attribute the rewritten node to its input construct. Only unchanged identifier
    /// slots are preserved. New/reordered names require explicit per-slot transfer.
    pub fn finish_rewrite(
        &mut self,
        node: NodeId,
        snapshot: Option<OriginSnapshot>,
        transformation: &str,
    ) {
        let Some(snapshot) = snapshot.filter(|_| self.tracks_origins()) else {
            return;
        };
        let unchanged = snapshot.node == self.values[node as usize];
        if unchanged && self.restore_shared(node, &snapshot) {
            return;
        }
        let (previous, mut imported) = self.import_owned(snapshot);
        if !unchanged {
            let reason =
                OptimizationReason::rewrite(transformation, &previous, &self.values[node as usize]);
            if let Some(out) = &mut imported.origin {
                out.rewrite_in_place(transformation, reason.clone());
            }
            if let Some(original) = imported.origin.as_ref().and_then(|o| o.value.primary) {
                if matches!(
                    self.values[node as usize],
                    Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
                ) && !matches!(
                    previous,
                    Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
                ) {
                    Arc::make_mut(&mut imported.dispositions).push(SourceDisposition {
                        original,
                        reason: reason.clone(),
                        replacement_sources: Vec::new(),
                    });
                }
            }
            if let (Node::Block(old), Node::Block(new)) = (&previous, &self.values[node as usize]) {
                // Recording detachments must not turn a wide block rewrite into
                // a quadratic scan. Preserve deterministic old-source order.
                let retained = new
                    .iter()
                    .copied()
                    .collect::<std::collections::HashSet<_>>();
                let gone = old
                    .iter()
                    .filter(|id| !retained.contains(id))
                    .copied()
                    .collect::<Vec<_>>();
                for id in gone {
                    if let Some(original) = self.primary_origin(id) {
                        Arc::make_mut(&mut imported.dispositions).push(SourceDisposition {
                            original,
                            reason: self.removal_reason(id, transformation),
                            replacement_sources: Vec::new(),
                        });
                    }
                }
            }
            let same_token = |(site, _): &(crate::explanation::SyntaxSite, StoredOrigin)| {
                crate::explanation::syntax_text(&previous, *site)
                    == crate::explanation::syntax_text(&self.values[node as usize], *site)
            };
            if !imported.tokens.iter().all(same_token) {
                Arc::make_mut(&mut imported.tokens).retain(same_token);
            }
            let same_name = |(site, _): &(NameSite, StoredOrigin)| {
                name_value(&previous, *site).is_some()
                    && name_value(&previous, *site)
                        == name_value(&self.values[node as usize], *site)
            };
            if !imported.names.iter().all(same_name) {
                Arc::make_mut(&mut imported.names).retain(same_name);
            }
        }
        if let Some(origins) = &mut self.origins {
            Arc::make_mut(&mut origins.slots)[node as usize] = Some(Arc::new(imported));
        }
    }
    /// In-place structural rewrite whose primary source is the old construct.
    /// This is not a copy of some other node's provenance.
    pub fn rewrite(&mut self, node: NodeId, value: Node, transformation: &str) {
        if self.values[node as usize] == value {
            // This is an explicitly annotated no-op, not recovery after a raw
            // mutable write. Keep its shared origin without another allocation.
            return;
        }
        let old = std::mem::replace(&mut Arc::make_mut(&mut self.values)[node as usize], value);
        let snapshot = self.origins.as_mut().and_then(|origins| {
            Arc::make_mut(&mut origins.slots)[node as usize]
                .take()
                .map(|origin| OriginSnapshot {
                    sources: Arc::clone(&origins.sources),
                    node: old,
                    origin,
                })
        });
        self.finish_rewrite(node, snapshot, transformation);
    }
    /// Known symbol-only renaming. The caller must not reorder declaration or parameter slots.
    /// New symbol spellings still point to each original identifier occurrence.
    pub fn finish_rename(&mut self, node: NodeId, snapshot: Option<OriginSnapshot>) {
        let Some(snapshot) = snapshot.filter(|_| self.tracks_origins()) else {
            return;
        };
        let changed = snapshot.node != self.values[node as usize];
        if !changed && self.restore_shared(node, &snapshot) {
            return;
        }
        let (_, mut imported) = self.import_owned(snapshot);
        if changed {
            if let Some(value) = &mut imported.origin {
                value.derive_in_place("scope-renaming");
            }
            for (_, origin) in Arc::make_mut(&mut imported.names) {
                origin.derive_action("scope-renaming", ReasonOperation::Rename);
            }
        }
        if let Some(origins) = &mut self.origins {
            Arc::make_mut(&mut origins.slots)[node as usize] = Some(Arc::new(imported));
        }
    }
    /// Copy/derive attribution from an explicitly identified source node, including
    /// transfer between unrelated arenas. Node numbers alone are never matched.
    pub fn derive_from(
        &mut self,
        node: NodeId,
        source: &Self,
        original: NodeId,
        transformation: &str,
    ) {
        let Some(target_origins) = self.origins.as_mut() else {
            return;
        };
        if let Some(source_origins) = source.origins.as_ref() {
            if Arc::ptr_eq(&target_origins.sources, &source_origins.sources)
                && self.values[node as usize] == source.values[original as usize]
            {
                // This is an explicit input node supplied by the transformation,
                // not an attempt to infer a source from equal text or NodeId.
                let source_slot = &source_origins.slots[original as usize];
                let same = match (&target_origins.slots[node as usize], source_slot) {
                    (None, None) => true,
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    _ => false,
                };
                if !same {
                    Arc::make_mut(&mut target_origins.slots)[node as usize] = source_slot.clone();
                }
                return;
            }
        }
        let snapshot = source.capture_origin(original);
        if let Some(origins) = &mut self.origins {
            Arc::make_mut(&mut origins.slots)[node as usize] = None;
        }
        self.finish_rewrite(node, snapshot, transformation);
    }
    /// Explicitly identify synthesized helper code. Unknown is not synthetic by default.
    pub fn mark_synthetic(&mut self, node: NodeId, transformation: &str) {
        if let Some(origins) = &mut self.origins {
            Arc::make_mut(&mut origins.slots)[node as usize] = Some(Arc::new(NodeOrigin {
                origin: Some(Origin::synthetic(transformation).into()),
                ..NodeOrigin::default()
            }));
        }
    }
    fn import_contribution(&mut self, snapshot: ContributionSnapshot) -> Origin {
        let Some(origins) = self.origins.as_mut() else {
            unreachable!("origin import requires a tracked target")
        };
        let mut imported = snapshot.origin;
        if Arc::ptr_eq(&origins.sources, &snapshot.sources) || origins.sources == snapshot.sources {
            return imported;
        }
        let mut source_ids = Vec::with_capacity(snapshot.sources.len());
        for source in snapshot.sources.iter() {
            let id = if let Some(id) = origins
                .sources
                .iter()
                .position(|existing| existing == source)
            {
                id
            } else {
                let values = Arc::make_mut(&mut origins.sources);
                let id = values.len();
                values.push(source.clone());
                id
            };
            source_ids.push(id as u32);
        }
        for range in imported
            .primary
            .iter_mut()
            .chain(Arc::make_mut(&mut imported.related).iter_mut())
            .chain(
                Arc::make_mut(&mut imported.relations)
                    .iter_mut()
                    .map(|relation| &mut relation.span),
            )
        {
            range.source = source_ids[range.source as usize];
        }
        imported
    }

    /// Add another contributing construct without changing the primary attribution.
    pub fn relate_from(
        &mut self,
        node: NodeId,
        source: &Self,
        original: NodeId,
        transformation: &str,
    ) {
        self.relate_snapshot(
            node,
            source.capture_contribution(original),
            transformation,
            RelationRole::Contribution,
        );
    }
    /// Add an ordered batch of explicit contributors with the same semantics as
    /// repeated `relate_from`. Large merged columns build membership indexes once
    /// rather than rescanning every accumulated source range on every insertion.
    pub fn relate_many_from(
        &mut self,
        node: NodeId,
        source: &Self,
        originals: &[NodeId],
        transformation: &str,
    ) {
        let Some(target_origins) = self.origins.as_mut() else {
            return;
        };
        let same_sources = source
            .origins
            .as_ref()
            .is_some_and(|other| Arc::ptr_eq(&target_origins.sources, &other.sources));
        if originals.len() < 8 || !same_sources {
            for &original in originals {
                self.relate_from(node, source, original, transformation);
            }
            return;
        }
        let Some(slot) = Arc::make_mut(&mut target_origins.slots)[node as usize].as_mut() else {
            return;
        };
        let Some(origin) = Arc::make_mut(slot).origin.as_mut() else {
            return;
        };
        let mut spans = origin
            .value
            .related
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let mut relations = origin
            .value
            .relations
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        for &original in originals {
            let Some(contributor) = source.origin_record(original) else {
                continue;
            };
            let contributor = &contributor.value;
            origin.derive_action(transformation, ReasonOperation::Relate);
            for span in contributor.primary.iter().chain(contributor.related.iter()) {
                if origin.value.primary != Some(*span) && spans.insert(*span) {
                    Arc::make_mut(&mut origin.value.related).push(*span);
                }
                let relation = SourceRelation {
                    role: RelationRole::Contribution,
                    span: *span,
                };
                if relations.insert(relation.clone()) {
                    Arc::make_mut(&mut origin.value.relations).push(relation);
                }
            }
            for relation in contributor.relations.iter() {
                if relations.insert(relation.clone()) {
                    Arc::make_mut(&mut origin.value.relations).push(relation.clone());
                }
            }
        }
    }

    /// Add the original use/call site when copying a value inside this arena.
    pub fn relate_within(&mut self, node: NodeId, original: NodeId, transformation: &str) {
        self.relate_snapshot(
            node,
            self.capture_contribution(original),
            transformation,
            RelationRole::Contribution,
        );
    }
    fn relate_snapshot(
        &mut self,
        node: NodeId,
        snapshot: Option<ContributionSnapshot>,
        transformation: &str,
        role: RelationRole,
    ) {
        let Some(snapshot) = snapshot.filter(|_| self.tracks_origins()) else {
            return;
        };

        let contributor = self.import_contribution(snapshot);
        let Some(target) = self
            .origins
            .as_mut()
            .and_then(|origins| Arc::make_mut(&mut origins.slots)[node as usize].as_mut())
        else {
            return;
        };
        let target = Arc::make_mut(target);
        let Some(origin) = target.origin.as_mut() else {
            return;
        };
        origin.derive_action(transformation, ReasonOperation::Relate);
        for span in contributor.primary.iter().chain(contributor.related.iter()) {
            if origin.value.primary != Some(*span) && !origin.value.related.contains(span) {
                Arc::make_mut(&mut origin.value.related).push(*span);
            }
            let relation = SourceRelation {
                role: if contributor.primary == Some(*span) {
                    role
                } else {
                    RelationRole::Contribution
                },
                span: *span,
            };
            if !origin.value.relations.contains(&relation) {
                Arc::make_mut(&mut origin.value.relations).push(relation);
            }
        }
        for relation in contributor.relations.iter() {
            if !origin.value.relations.contains(relation) {
                Arc::make_mut(&mut origin.value.relations).push(relation.clone());
            }
        }
    }
    /// Copy a named occurrence within this arena before its original declaration is removed.
    pub fn copy_name_within(
        &mut self,
        node: NodeId,
        site: NameSite,
        original: NodeId,
        original_site: NameSite,
    ) {
        let copied = self.name_record(original, original_site).cloned();
        let Some(origins) = &mut self.origins else {
            return;
        };
        let target = Arc::make_mut(&mut origins.slots)[node as usize]
            .get_or_insert_with(|| Arc::new(NodeOrigin::default()));
        let target = Arc::make_mut(target);
        Arc::make_mut(&mut target.names).retain(|(key, _)| *key != site);
        if let Some(copied) = copied {
            Arc::make_mut(&mut target.names).push((site, copied));
            Arc::make_mut(&mut target.names).sort_by_key(|(site, _)| *site);
        }
    }

    /// Attribute a generated identifier to an explicitly identified original expression,
    /// such as a quoted table key. Keep the expression precision and do not invent an
    /// original identifier spelling. A missing source masks the enclosing node fallback.
    pub fn copy_expression_to_name_from(
        &mut self,
        node: NodeId,
        site: NameSite,
        source: &Self,
        original: NodeId,
    ) {
        if !self.tracks_origins() {
            return;
        }
        let copied = source
            .capture_origin(original)
            .and_then(|snapshot| self.import(&snapshot).origin);
        let Some(origins) = self.origins.as_mut() else {
            return;
        };
        let slot = Arc::make_mut(&mut origins.slots)[node as usize]
            .get_or_insert_with(|| Arc::new(NodeOrigin::default()));
        let slot = Arc::make_mut(slot);
        Arc::make_mut(&mut slot.names).retain(|(key, _)| *key != site);
        if let Some(copied) = copied {
            Arc::make_mut(&mut slot.names).push((site, copied.derived("quoted-field-renaming")));
            Arc::make_mut(&mut slot.names).sort_by_key(|(site, _)| *site);
        } else {
            // The missing key is not explained by the table's wider source span.
            slot.origin = None;
        }
    }

    /// Transfer a named slot explicitly when a declaration is merged or reordered.
    pub fn copy_name_from(
        &mut self,
        node: NodeId,
        site: NameSite,
        source: &Self,
        original: NodeId,
        original_site: NameSite,
    ) {
        let Some(snapshot) = source
            .capture_origin(original)
            .filter(|_| self.tracks_origins())
        else {
            return;
        };
        let imported = self.import(&snapshot);
        let origin = imported
            .names
            .iter()
            .find(|(key, _)| *key == original_site)
            .map(|(_, value)| value.clone());
        let Some(origins) = self.origins.as_mut() else {
            return;
        };
        let target = Arc::make_mut(&mut origins.slots)[node as usize]
            .get_or_insert_with(|| Arc::new(NodeOrigin::default()));
        let target = Arc::make_mut(target);
        Arc::make_mut(&mut target.names).retain(|(key, _)| *key != site);
        if let Some(origin) = origin {
            Arc::make_mut(&mut target.names).push((site, origin));
            Arc::make_mut(&mut target.names).sort_by_key(|(site, _)| *site);
        }
    }
}

impl NodeArena {
    /// Attach an expansion context independently of the generated leaf's own origin.
    pub fn add_inline_context(&mut self, node: NodeId, definition: NodeId, call: NodeId) {
        let Some(definition) = self.primary_origin(definition) else {
            return;
        };
        let Some(call_site) = self.primary_origin(call) else {
            return;
        };
        let Some(origins) = self.origins.as_mut() else {
            return;
        };
        let slot = Arc::make_mut(&mut origins.slots)[node as usize]
            .get_or_insert_with(|| Arc::new(NodeOrigin::default()));
        let context = InlineContext {
            definition,
            call_site,
        };
        let contexts = &mut Arc::make_mut(slot).contexts;
        if !contexts.contains(&context) {
            Arc::make_mut(contexts).insert(0, context);
        }
    }
    // Only carry checks actually recorded on this detached construct. A prior
    // value rewrite is not a reason to remove it; eligibility is explicitly tagged.
    fn removal_reason(&self, node: NodeId, transformation: &str) -> OptimizationReason {
        let recorded = self
            .origin_record(node)
            .and_then(|o| o.find_reason(ReasonOperation::Remove));
        recorded
            .cloned()
            .unwrap_or_else(|| OptimizationReason::action(transformation, ReasonOperation::Remove))
    }
    /// Record why a currently selected construct is about to be detached.
    pub fn explain_removal(&mut self, node: NodeId, mut reason: OptimizationReason) {
        reason.operation = ReasonOperation::Remove;
        self.explain(node, reason);
    }
    fn record_removal(&mut self, owner: NodeId, node: NodeId, transformation: &str) {
        let Some(original) = self.primary_origin(node) else {
            return;
        };
        let reason = self.removal_reason(node, transformation);
        if let Some(slot) = self
            .origins
            .as_mut()
            .and_then(|table| Arc::make_mut(&mut table.slots)[owner as usize].as_mut())
        {
            Arc::make_mut(&mut Arc::make_mut(slot).dispositions).push(SourceDisposition {
                original,
                reason,
                replacement_sources: Vec::new(),
            });
        }
    }
}

impl NodeArena {
    /// Record the semantic role of a source relationship at the transformation site.
    pub fn relate_from_role(
        &mut self,
        node: NodeId,
        source: &Self,
        original: NodeId,
        transformation: &str,
        role: RelationRole,
    ) {
        self.relate_snapshot(
            node,
            source.capture_contribution(original),
            transformation,
            role,
        );
    }
    /// Record a typed relationship within this candidate arena.
    pub fn relate_within_role(
        &mut self,
        node: NodeId,
        original: NodeId,
        transformation: &str,
        role: RelationRole,
    ) {
        self.relate_snapshot(
            node,
            self.capture_contribution(original),
            transformation,
            role,
        );
    }
}

impl NodeArena {
    /// Attach facts collected by the accepting transformation. Unknown stays unknown.
    pub fn explain(&mut self, node: NodeId, reason: OptimizationReason) {
        if let Some(origin) = self
            .origins
            .as_mut()
            .and_then(|table| Arc::make_mut(&mut table.slots)[node as usize].as_mut())
            .and_then(|slot| Arc::make_mut(slot).origin.as_mut())
        {
            origin.add_reason(reason);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod cow_tests {
    use super::*;
    use crate::parse_source_with_origins;
    use crate::provenance::GeneratedOrigins;
    use crate::Printer;

    #[test]
    fn read_only_candidates_and_no_op_rewrites_do_not_copy_arena_buffers() {
        let (ast, root) = parse_source_with_origins(
            "cow.lua",
            "local x=1 function onTick()output.setNumber(1,x)end",
        )
        .unwrap();
        let frozen = serde_json::to_vec(&ast).unwrap();
        let mut copy = ast.clone();
        assert!(Arc::ptr_eq(&ast.nodes.values, &copy.nodes.values));
        assert!(Arc::ptr_eq(
            &ast.nodes.origins.as_ref().unwrap().slots,
            &copy.nodes.origins.as_ref().unwrap().slots
        ));
        copy.nodes.rewrite(root, copy.node(root).clone(), "no-op");
        copy.nodes.retain_block_statements(|_| true, "no-removal");
        copy.nodes.truncate(copy.nodes.len() + 1);
        let snapshot = copy.nodes.capture_origin(root);
        copy.nodes.finish_rewrite(root, snapshot, "no-op");
        assert!(Arc::ptr_eq(&ast.nodes.values, &copy.nodes.values));
        assert!(Arc::ptr_eq(
            &ast.nodes.origins.as_ref().unwrap().slots,
            &copy.nodes.origins.as_ref().unwrap().slots
        ));
        assert_eq!(serde_json::to_vec(&copy).unwrap(), frozen);
        let syntax = ast.nodes.clone_without_origins();
        assert!(!syntax.tracks_origins());
        assert!(Arc::ptr_eq(&ast.nodes.values, &syntax.values));
    }

    #[test]
    fn actual_mutations_detach_and_raw_writes_still_invalidate_only_the_candidate() {
        let source = "local x=1 output.setNumber(1,x)";
        let (ast, root) = parse_source_with_origins("cow.lua", source).unwrap();
        let frozen = serde_json::to_vec(&ast).unwrap();
        let number = ast
            .nodes
            .iter()
            .position(|n| matches!(n, Node::Num(_)))
            .unwrap();
        let mut copy = ast.clone();
        copy.nodes[number] = Node::Num("9".into());
        assert!(copy.nodes.origin(number as NodeId).is_none());
        assert!(ast.nodes.origin(number as NodeId).is_some());
        assert!(!Arc::ptr_eq(&ast.nodes.values, &copy.nodes.values));
        assert!(!Arc::ptr_eq(
            &ast.nodes.origins.as_ref().unwrap().slots,
            &copy.nodes.origins.as_ref().unwrap().slots
        ));
        assert_eq!(serde_json::to_vec(&ast).unwrap(), frozen);
        let printed = Printer::new(&copy, false).output_with_positions(root);
        assert!(
            GeneratedOrigins::from_print(&copy, &printed)
                .unwrap()
                .unknown_bytes()
                > 0
        );
        copy.nodes.derive_from(
            number as NodeId,
            &ast.nodes,
            number as NodeId,
            "value-rewrite",
        );
        let printed = Printer::new(&copy, false).output_with_positions(root);
        assert_eq!(
            GeneratedOrigins::from_print(&copy, &printed)
                .unwrap()
                .unknown_bytes(),
            0
        );
        let original_length = copy.nodes.len();
        copy.nodes.push(Node::Nil);
        copy.nodes.truncate(original_length);
        assert_eq!(copy.nodes.len(), ast.nodes.len());
        assert_eq!(serde_json::to_vec(&ast).unwrap(), frozen);
    }

    #[test]
    fn cow_storage_has_the_same_wire_shape_and_mutable_slice_contract() {
        let (ast, root) = parse_source_with_origins("cow.lua", "a=1 do b=2 end c=3").unwrap();
        #[derive(Serialize)]
        struct PreviousWire<'a> {
            nodes: &'a [Node],
            origins: Option<&'a ArenaOrigins>,
        }
        let expected = serde_json::to_vec(&PreviousWire {
            nodes: &ast.nodes,
            origins: ast.nodes.origins.as_ref(),
        })
        .unwrap();
        assert_eq!(serde_json::to_vec(&ast.nodes).unwrap(), expected);
        let decoded: NodeArena = serde_json::from_slice(&expected).unwrap();
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), expected);
        let mut raw = ast.clone();
        for node in &mut raw.nodes {
            if matches!(node, Node::Num(_)) {
                *node = Node::Num("7".into());
            }
        }
        assert!(raw.nodes.origin(root).is_none());
        assert!(ast.nodes.origin(root).is_some());
        let mut calls = Vec::new();
        let mut retained = ast.clone();
        retained.nodes.retain_block_statements(
            |id| {
                calls.push(id);
                calls.len() % 2 == 1
            },
            "test-removal",
        );
        let all = ast
            .nodes
            .iter()
            .filter_map(|n| {
                if let Node::Block(s) = n {
                    Some(s.as_slice())
                } else {
                    None
                }
            })
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(
            calls, all,
            "FnMut is invoked once in original arena/block order"
        );
        let printed = Printer::new(&retained, false).output_with_positions(root);
        let origins = GeneratedOrigins::from_print(&retained, &printed).unwrap();
        origins.validate_for_code(&printed.code).unwrap();
        assert_eq!(origins.unknown_bytes(), 0);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod batch_relation_tests {
    use super::*;
    use crate::explanation::RelationRole;
    use crate::provenance::GeneratedOrigins;
    use crate::{parse_source_with_origins, Printer};
    #[test]
    fn ordered_batches_match_scalar_relations_for_shared_and_independent_snapshots() {
        let text = (0..160)
            .map(|i| format!("output.setNumber(1,{}) ", i % 13))
            .collect::<String>();
        let (mut source, root) = parse_source_with_origins("batch.lua", &text).unwrap();
        let nodes = source
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| matches!(n, Node::Num(_)).then_some(i as NodeId))
            .collect::<Vec<_>>();
        // Equal text values are separate source occurrences; some contributors
        // already carry typed relations and one has no attribution at all.
        source
            .nodes
            .relate_within_role(nodes[1], nodes[0], "definition", RelationRole::Definition);
        let missing = nodes[2];
        let value = source.node(missing).clone();
        source.nodes[missing as usize] = value;
        for count in [0, 1, 7, 8, 32, nodes.len()] {
            for independent in [false, true] {
                let (mut left, target_root) = if independent {
                    parse_source_with_origins("other.lua", "output.setNumber(1,42)").unwrap()
                } else {
                    (source.clone(), root)
                };
                let output = if independent {
                    left.nodes
                        .iter()
                        .position(|n| matches!(n, Node::Num(_)))
                        .unwrap() as u32
                } else {
                    nodes[0]
                };
                let mut right = left.clone();
                let mut inputs = nodes[..count].to_vec();
                inputs.extend(nodes[..count].iter().rev());
                for &input in &inputs {
                    left.nodes
                        .relate_from(output, &source.nodes, input, "merge");
                }
                right
                    .nodes
                    .relate_many_from(output, &source.nodes, &inputs, "merge");
                assert_eq!(
                    serde_json::to_vec(&left).unwrap(),
                    serde_json::to_vec(&right).unwrap(),
                    "count={count} independent={independent}"
                );
                let a = Printer::new(&left, false).output_with_positions(target_root);
                let b = Printer::new(&right, false).output_with_positions(target_root);
                assert_eq!(a.code, b.code);
                assert_eq!(
                    GeneratedOrigins::from_print(&left, &a),
                    GeneratedOrigins::from_print(&right, &b)
                );
                let mut synthetic = source.clone();
                let target = synthetic.push(Node::Num("99".into()));
                synthetic.nodes.mark_synthetic(target, "helper");
                let mut expected = synthetic.clone();
                for &input in &inputs {
                    expected
                        .nodes
                        .relate_from(target, &source.nodes, input, "merge");
                }
                synthetic
                    .nodes
                    .relate_many_from(target, &source.nodes, &inputs, "merge");
                assert_eq!(
                    serde_json::to_vec(&expected).unwrap(),
                    serde_json::to_vec(&synthetic).unwrap()
                );
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod ordered_relation_union_tests {
    use super::*;
    use crate::provenance::GeneratedOrigins;
    use crate::{parse_source_with_origins, Printer};
    fn reference(
        arena: &mut NodeArena,
        node: NodeId,
        snapshot: Option<ContributionSnapshot>,
        transformation: &str,
        role: RelationRole,
    ) {
        let Some(snapshot) = snapshot.filter(|_| arena.tracks_origins()) else {
            return;
        };

        let contributor = arena.import_contribution(snapshot);
        let Some(target) = arena
            .origins
            .as_mut()
            .and_then(|origins| Arc::make_mut(&mut origins.slots)[node as usize].as_mut())
        else {
            return;
        };
        let target = Arc::make_mut(target);
        let Some(origin) = target.origin.as_mut() else {
            return;
        };
        origin.derive_action(transformation, ReasonOperation::Relate);
        for span in contributor.primary.iter().chain(contributor.related.iter()) {
            if origin.value.primary != Some(*span) && !origin.value.related.contains(span) {
                Arc::make_mut(&mut origin.value.related).push(*span);
            }
            let relation = SourceRelation {
                role: if contributor.primary == Some(*span) {
                    role
                } else {
                    RelationRole::Contribution
                },
                span: *span,
            };
            if !origin.value.relations.contains(&relation) {
                Arc::make_mut(&mut origin.value.relations).push(relation);
            }
        }
        for relation in contributor.relations.iter() {
            if !origin.value.relations.contains(relation) {
                Arc::make_mut(&mut origin.value.relations).push(relation.clone());
            }
        }
    }

    #[test]
    fn transitive_unions_keep_order_roles_duplicates_and_foreign_source_identity() {
        for count in [0, 1, 8, 31, 32, 33, 128, 1024] {
            let source = (0..count + 3)
                .map(|i| format!("output.setNumber(1,{i}) "))
                .collect::<String>();
            let (mut input, input_root) = parse_source_with_origins("source.lua", &source).unwrap();
            let numbers = input
                .nodes
                .iter()
                .enumerate()
                .filter_map(|(i, n)| matches!(n, Node::Num(_)).then_some(i as NodeId))
                .collect::<Vec<_>>();
            let from = numbers[0];
            let mut rich = input.clone();
            rich.nodes
                .relate_many_from(from, &input.nodes, &numbers[1..count + 1], "gather");
            rich.nodes.relate_from_role(
                from,
                &input.nodes,
                numbers[1],
                "argument",
                RelationRole::Argument,
            );
            for foreign in [false, true] {
                for role in [
                    RelationRole::Contribution,
                    RelationRole::Definition,
                    RelationRole::CallSite,
                    RelationRole::Argument,
                    RelationRole::ParameterUse,
                    RelationRole::UseSite,
                ] {
                    let (mut actual, root) = if foreign {
                        parse_source_with_origins("source.lua", "output.setNumber(1,9)").unwrap()
                    } else {
                        (input.clone(), input_root)
                    };
                    let node = actual
                        .nodes
                        .iter()
                        .position(|n| matches!(n, Node::Num(_)))
                        .unwrap() as NodeId;
                    let mut expected = actual.clone();
                    for iteration in 0..3 {
                        let source = if iteration == 1 { &input } else { &rich };
                        reference(
                            &mut expected.nodes,
                            node,
                            source.nodes.capture_contribution(from),
                            "join",
                            role,
                        );
                        actual
                            .nodes
                            .relate_from_role(node, &source.nodes, from, "join", role);
                        assert_eq!(
                            serde_json::to_value(&actual).unwrap(),
                            serde_json::to_value(&expected).unwrap(),
                            "count={count}, foreign={foreign}, role={role:?}, repeat={iteration}"
                        );
                    }
                    let a = Printer::new(&actual, false).output_with_positions(root);
                    let b = Printer::new(&expected, false).output_with_positions(root);
                    assert_eq!(a.code, b.code);
                    assert_eq!(
                        GeneratedOrigins::from_print(&actual, &a),
                        GeneratedOrigins::from_print(&expected, &b)
                    );
                }
            }
            // Missing attribution is not replaced by a source-number guess.
            let value = input.node(from).clone();
            input.nodes[from as usize] = value;
            let before = serde_json::to_vec(&rich).unwrap();
            rich.nodes.relate_from_role(
                from,
                &input.nodes,
                from,
                "missing",
                RelationRole::Argument,
            );
            assert_eq!(serde_json::to_vec(&rich).unwrap(), before);
        }
    }
}
