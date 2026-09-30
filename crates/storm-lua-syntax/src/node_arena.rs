//! Vec-like node storage with an optional provenance side table.
//! Mutable indexing is deliberately untracked: it invalidates that slot's origin.
//! Annotated transformations explicitly preserve, derive, merge, or copy origins.
use crate::ast::{Node, NodeId, SymbolId};
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
    values: Vec<Node>,
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
            origins.slots[index] = None;
        }
        &mut self.values[index]
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
            origins.slots.fill(None);
        }
        self.values.iter_mut()
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
            values: value.nodes,
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
        Ok(Self { values, origins })
    }

    /// Append a node. New nodes have unknown origin until a transformation attributes them.
    pub fn push(&mut self, node: Node) {
        self.values.push(node);
        if let Some(origins) = &mut self.origins {
            origins.slots.push(None);
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
        for (id, node) in self.values.iter_mut().enumerate() {
            let Node::Block(statements) = node else {
                continue;
            };
            let old_len = statements.len();
            statements.retain(|&statement| {
                let retain = keep(statement);
                if !retain && self.origins.is_some() {
                    removed.push((id as NodeId, statement));
                }
                retain
            });
            if statements.len() != old_len {
                if let Some(slot) = self
                    .origins
                    .as_mut()
                    .and_then(|table| table.slots[id].as_mut())
                {
                    let origin = Arc::make_mut(slot);
                    origin.origin = origin
                        .origin
                        .as_ref()
                        .map(|value| value.derived(transformation));
                }
            }
        }
        for (owner, node) in removed {
            self.record_removal(owner, node, transformation);
        }
    }

    /// Discard trial nodes and their origins together. Reused slots start unknown.
    pub fn truncate(&mut self, len: usize) {
        self.values.truncate(len);
        if let Some(origins) = &mut self.origins {
            origins.slots.truncate(len);
        }
    }
    /// Create an empty arena sharing only source snapshots, not node-number attributions.
    pub fn empty_like(&self) -> Self {
        Self {
            values: Vec::new(),
            origins: self.origins.as_ref().map(|origins| ArenaOrigins {
                sources: Arc::clone(&origins.sources),
                slots: Vec::new(),
                dispositions: Arc::clone(&origins.dispositions),
            }),
        }
    }
    /// Whether source provenance is being collected for this arena.
    pub fn tracks_origins(&self) -> bool {
        self.origins.is_some()
    }
    pub(crate) fn provenance(&self) -> Option<&ArenaOrigins> {
        self.origins.as_ref()
    }
    pub(crate) fn set_provenance(&mut self, origins: ArenaOrigins) {
        self.origins = Some(origins);
    }
    /// Return the current source attribution. An untracked mutation yields None.
    pub fn origin(&self, node: NodeId) -> Option<&Origin> {
        self.origins
            .as_ref()?
            .slots
            .get(node as usize)?
            .as_ref()?
            .origin
            .as_ref()
    }
    /// Return one identifier occurrence's attribution, independently of the node's anchor.
    pub fn name_origin(&self, node: NodeId, site: NameSite) -> Option<&Origin> {
        self.origins
            .as_ref()?
            .slots
            .get(node as usize)?
            .as_ref()?
            .names
            .iter()
            .find(|(key, _)| *key == site)
            .map(|(_, value)| value)
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
    fn restore_shared(&mut self, node: NodeId, snapshot: &OriginSnapshot) -> bool {
        let Some(origins) = &mut self.origins else {
            return false;
        };
        if !Arc::ptr_eq(&origins.sources, &snapshot.sources) {
            return false;
        }
        origins.slots[node as usize] = Some(Arc::clone(&snapshot.origin));
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
            .chain(imported.names.iter_mut().map(|(_, origin)| origin))
            .chain(imported.tokens.iter_mut().map(|(_, origin)| origin))
        {
            for range in value
                .primary
                .iter_mut()
                .chain(Arc::make_mut(&mut value.related).iter_mut())
                .chain(
                    Arc::make_mut(&mut value.relations)
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
        let mut imported = self.import(&snapshot);
        if !unchanged {
            let reason = OptimizationReason::rewrite(
                transformation,
                &snapshot.node,
                &self.values[node as usize],
            );
            imported.origin = imported.origin.as_ref().map(|value| {
                let mut out = value.derived(transformation);
                if out
                    .reasons
                    .last()
                    .is_some_and(|r| r.code == reason.code && r.before.is_none())
                {
                    Arc::make_mut(&mut out.reasons).pop();
                }
                out.add_reason(reason.clone());
                out
            });
            if let Some(original) = imported.origin.as_ref().and_then(|o| o.primary) {
                if matches!(
                    self.values[node as usize],
                    Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
                ) && !matches!(
                    snapshot.node,
                    Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
                ) {
                    Arc::make_mut(&mut imported.dispositions).push(SourceDisposition {
                        original,
                        reason: reason.clone(),
                        replacement_sources: Vec::new(),
                    });
                }
            }
            if let (Node::Block(old), Node::Block(new)) =
                (&snapshot.node, &self.values[node as usize])
            {
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
                    if let Some(original) = self.origin(id).and_then(|o| o.primary) {
                        Arc::make_mut(&mut imported.dispositions).push(SourceDisposition {
                            original,
                            reason: self.removal_reason(id, transformation),
                            replacement_sources: Vec::new(),
                        });
                    }
                }
            }
            imported.tokens.retain(|(site, _)| {
                crate::explanation::syntax_text(&snapshot.node, *site)
                    == crate::explanation::syntax_text(&self.values[node as usize], *site)
            });
            imported.names.retain(|(site, _)| {
                name_value(&snapshot.node, *site).is_some()
                    && name_value(&snapshot.node, *site)
                        == name_value(&self.values[node as usize], *site)
            });
        }
        if let Some(origins) = &mut self.origins {
            origins.slots[node as usize] = Some(Arc::new(imported));
        }
    }
    /// In-place structural rewrite whose primary source is the old construct.
    /// This is not a copy of some other node's provenance.
    pub fn rewrite(&mut self, node: NodeId, value: Node, transformation: &str) {
        if self.origins.is_some() && self.values[node as usize] == value {
            // This is an explicitly annotated no-op, not recovery after a raw
            // mutable write. Keep its shared origin without another allocation.
            self.values[node as usize] = value;
            return;
        }
        let snapshot = self.capture_origin(node);
        self.values[node as usize] = value;
        if let Some(origins) = &mut self.origins {
            origins.slots[node as usize] = None;
        }
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
        let mut imported = self.import(&snapshot);
        if changed {
            imported.origin = imported
                .origin
                .as_ref()
                .map(|value| value.derived("scope-renaming"));
            for (_, origin) in &mut imported.names {
                *origin = origin.derived("scope-renaming");
                if let Some(reason) = Arc::make_mut(&mut origin.reasons).last_mut() {
                    reason.operation = ReasonOperation::Rename;
                }
            }
        }
        if let Some(origins) = &mut self.origins {
            origins.slots[node as usize] = Some(Arc::new(imported));
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
        let snapshot = source.capture_origin(original);
        if let Some(origins) = &mut self.origins {
            origins.slots[node as usize] = None;
        }
        self.finish_rewrite(node, snapshot, transformation);
    }
    /// Explicitly identify synthesized helper code. Unknown is not synthetic by default.
    pub fn mark_synthetic(&mut self, node: NodeId, transformation: &str) {
        if let Some(origins) = &mut self.origins {
            origins.slots[node as usize] = Some(Arc::new(NodeOrigin {
                origin: Some(Origin::synthetic(transformation)),
                names: Vec::new(),
                contexts: Arc::default(),
                tokens: Vec::new(),
                dispositions: Arc::default(),
            }));
        }
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
            source.capture_origin(original),
            transformation,
            RelationRole::Contribution,
        );
    }
    /// Add the original use/call site when copying a value inside this arena.
    pub fn relate_within(&mut self, node: NodeId, original: NodeId, transformation: &str) {
        self.relate_snapshot(
            node,
            self.capture_origin(original),
            transformation,
            RelationRole::Contribution,
        );
    }
    fn relate_snapshot(
        &mut self,
        node: NodeId,
        snapshot: Option<OriginSnapshot>,
        transformation: &str,
        role: RelationRole,
    ) {
        let Some(snapshot) = snapshot.filter(|_| self.tracks_origins()) else {
            return;
        };

        let imported = self.import(&snapshot);
        let Some(contributor) = imported.origin else {
            return;
        };
        let Some(target) = self
            .origins
            .as_mut()
            .and_then(|origins| origins.slots[node as usize].as_mut())
        else {
            return;
        };
        let target = Arc::make_mut(target);
        let Some(origin) = target.origin.as_mut() else {
            return;
        };
        *origin = origin.derived(transformation);
        if let Some(reason) = Arc::make_mut(&mut origin.reasons).last_mut() {
            reason.operation = ReasonOperation::Relate;
        }
        for span in contributor.primary.iter().chain(contributor.related.iter()) {
            if origin.primary != Some(*span) && !origin.related.contains(span) {
                Arc::make_mut(&mut origin.related).push(*span);
            }
            let relation = SourceRelation {
                role: if contributor.primary == Some(*span) {
                    role
                } else {
                    RelationRole::Contribution
                },
                span: *span,
            };
            if !origin.relations.contains(&relation) {
                Arc::make_mut(&mut origin.relations).push(relation);
            }
        }
        for relation in contributor.relations.iter() {
            if !origin.relations.contains(relation) {
                Arc::make_mut(&mut origin.relations).push(relation.clone());
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
        let copied = self.name_origin(original, original_site).cloned();
        let Some(origins) = &mut self.origins else {
            return;
        };
        let target =
            origins.slots[node as usize].get_or_insert_with(|| Arc::new(NodeOrigin::default()));
        let target = Arc::make_mut(target);
        target.names.retain(|(key, _)| *key != site);
        if let Some(copied) = copied {
            target.names.push((site, copied));
            target.names.sort_by_key(|(site, _)| *site);
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
        let slot =
            origins.slots[node as usize].get_or_insert_with(|| Arc::new(NodeOrigin::default()));
        let slot = Arc::make_mut(slot);
        slot.names.retain(|(key, _)| *key != site);
        if let Some(copied) = copied {
            slot.names
                .push((site, copied.derived("quoted-field-renaming")));
            slot.names.sort_by_key(|(site, _)| *site);
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
            .into_iter()
            .find(|(key, _)| *key == original_site)
            .map(|(_, value)| value);
        let Some(origins) = self.origins.as_mut() else {
            return;
        };
        let target =
            origins.slots[node as usize].get_or_insert_with(|| Arc::new(NodeOrigin::default()));
        let target = Arc::make_mut(target);
        target.names.retain(|(key, _)| *key != site);
        if let Some(origin) = origin {
            target.names.push((site, origin));
            target.names.sort_by_key(|(site, _)| *site);
        }
    }
}

impl NodeArena {
    /// Attach an expansion context independently of the generated leaf's own origin.
    pub fn add_inline_context(&mut self, node: NodeId, definition: NodeId, call: NodeId) {
        let Some(definition) = self.origin(definition).and_then(|o| o.primary) else {
            return;
        };
        let Some(call_site) = self.origin(call).and_then(|o| o.primary) else {
            return;
        };
        let Some(origins) = self.origins.as_mut() else {
            return;
        };
        let slot =
            origins.slots[node as usize].get_or_insert_with(|| Arc::new(NodeOrigin::default()));
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
        let recorded = self.origin(node).and_then(|o| {
            o.reasons
                .iter()
                .rev()
                .find(|r| r.operation == ReasonOperation::Remove)
        });
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
        let Some(original) = self.origin(node).and_then(|o| o.primary) else {
            return;
        };
        let reason = self.removal_reason(node, transformation);
        if let Some(slot) = self
            .origins
            .as_mut()
            .and_then(|table| table.slots[owner as usize].as_mut())
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
        self.relate_snapshot(node, source.capture_origin(original), transformation, role);
    }
    /// Record a typed relationship within this candidate arena.
    pub fn relate_within_role(
        &mut self,
        node: NodeId,
        original: NodeId,
        transformation: &str,
        role: RelationRole,
    ) {
        self.relate_snapshot(node, self.capture_origin(original), transformation, role);
    }
}

impl NodeArena {
    /// Attach facts collected by the accepting transformation. Unknown stays unknown.
    pub fn explain(&mut self, node: NodeId, reason: OptimizationReason) {
        if let Some(origin) = self
            .origins
            .as_mut()
            .and_then(|table| table.slots[node as usize].as_mut())
            .and_then(|slot| Arc::make_mut(slot).origin.as_mut())
        {
            origin.add_reason(reason);
        }
    }
}
