//! Slice-compatible copy-on-write node storage with an optional provenance side table.
//! Read-only candidate clones share buffers. The first actual mutation detaches
//! them; the serialized node/metadata representation is unchanged.
//! Mutable indexing is deliberately untracked: it invalidates that slot's origin.
//! Annotated transformations explicitly preserve, derive, merge, or copy origins.
use crate::ast::{Node, NodeId, SymbolId};
use crate::deferred_origin::{SharedTail, StoredOrigin};
use crate::explanation::{
    InlineContext, OptimizationReason, ReasonOperation, RelationRole, SourceDisposition,
    SourceRelation, SyntaxSite,
};
use crate::origin_slots::OriginSlots;
use crate::parser::NameSite;
use crate::provenance::{
    empty_tokens, ArenaOrigins, NodeOrigin, Origin, SourceSnapshot, SourceSpan,
};
use serde::{Deserialize, Deserializer, Serialize};
use std::ops::{Deref, Index, IndexMut};
use std::sync::Arc;

/// Interned synthetic origins for `mark_synthetic`: one shared node per
/// transformation name, capped like the reason-tail tables in
/// `deferred_origin.rs`. Beyond the cap, fresh nodes are used as before.
fn shared_synthetic_origin(transformation: &str) -> Arc<NodeOrigin> {
    type SyntheticIntern = std::sync::Mutex<Vec<(Arc<str>, Arc<NodeOrigin>)>>;
    static SYNTHETICS: std::sync::OnceLock<SyntheticIntern> = std::sync::OnceLock::new();
    let mut cache = SYNTHETICS
        .get_or_init(|| std::sync::Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((_, shared)) = cache.iter().find(|(key, _)| key.as_ref() == transformation) {
        return Arc::clone(shared);
    }
    let shared = Arc::new(NodeOrigin {
        origin: Some(Origin::synthetic(transformation).into()),
        ..NodeOrigin::default()
    });
    if cache.len() < 4096 {
        cache.push((Arc::from(transformation), Arc::clone(&shared)));
    }
    shared
}

/// Interned singleton vectors for the relate fast path below.
/// A contributor with a single primary span and no other relations,
/// related to an origin holding no related spans or relations yet,
/// records exactly one related span and one typed relation. Those
/// singleton vectors are fully determined by their contents, so
/// identical ones across origins share one allocation instead of one
/// Arc box plus one buffer per vector per call. Capped like the other
/// interns in this file; beyond the cap, fresh vectors are used as
/// before. Later edits go through Arc::make_mut, so sharers never
/// observe each other edits and the recorded content is identical
/// to the push path.
fn shared_singleton_related(span: SourceSpan) -> Arc<Vec<SourceSpan>> {
    type RelatedIntern = std::sync::Mutex<Vec<(SourceSpan, Arc<Vec<SourceSpan>>)>>;
    static RELATED: std::sync::OnceLock<RelatedIntern> = std::sync::OnceLock::new();
    let mut cache = RELATED
        .get_or_init(|| std::sync::Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((_, shared)) = cache.iter().find(|(key, _)| *key == span) {
        return Arc::clone(shared);
    }
    let shared = Arc::new(vec![span]);
    if cache.len() < 4096 {
        cache.push((span, Arc::clone(&shared)));
    }
    shared
}

fn shared_singleton_relation(role: RelationRole, span: SourceSpan) -> Arc<Vec<SourceRelation>> {
    type RelationIntern =
        std::sync::Mutex<Vec<((RelationRole, SourceSpan), Arc<Vec<SourceRelation>>)>>;
    static RELATIONS: std::sync::OnceLock<RelationIntern> = std::sync::OnceLock::new();
    let mut cache = RELATIONS
        .get_or_init(|| std::sync::Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let key = (role, span);
    if let Some((_, shared)) = cache.iter().find(|(k, _)| *k == key) {
        return Arc::clone(shared);
    }
    let shared = Arc::new(vec![SourceRelation { role, span }]);
    if cache.len() < 4096 {
        cache.push((key, Arc::clone(&shared)));
    }
    shared
}

/// Arena node storage. Equality compares syntax only, never provenance metadata.
/// Ast preserves the historical untracked JSON shape; binary Worker contexts use
/// a sized representation and must be exchanged between the same compiler version.
#[derive(Clone, Default, Serialize)]
pub struct NodeArena {
    #[serde(rename = "nodes")]
    values: Arc<Vec<Node>>,
    origins: Option<ArenaOrigins>,
    /// Monotonic count of syntax mutations, advanced exactly once per
    /// mutation batch through the single choke point [`NodeArena::values_mut`].
    /// Provenance-only edits (origins) never advance it, because measurement
    /// consumers depend on syntax only. Clones copy the current value, and a
    /// clone that later detaches via copy-on-write keeps the generation of
    /// the contents it still holds, so (values buffer identity, generation)
    /// pins the exact syntax of an arena. Not serialized: a deserialized
    /// arena is a fresh lineage and starts at zero.
    #[serde(skip)]
    generation: u64,
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
        &mut self.values_mut()[index]
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
        self.values_mut().iter_mut()
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
            generation: 0,
        })
    }
}

/// Shared reason-tail nodes for one batch rename, built once from the
/// batch's transformation code. See `SharedTail`: the nodes are only
/// installed on origins whose own tail is still empty, so sharing them
/// cannot change any recorded origin. Construct this only when origins
/// are tracked; untracked callers keep using `finish_rename_with_code`.
pub struct RenameSharedTails {
    rewrite: SharedTail,
    rename: SharedTail,
}

impl RenameSharedTails {
    /// Build the shared tail nodes for one batch rename with this code.
    pub fn new(code: &Arc<str>) -> Self {
        Self {
            rewrite: SharedTail::new(code, ReasonOperation::Rewrite),
            rename: SharedTail::new(code, ReasonOperation::Rename),
        }
    }
}

/// Borrowed origin slot for [`NodeArena::finish_rename_changed`]: the
/// sources and origin of a slot, without the node value clone that
/// [`OriginSnapshot`] carries. Only callers that already know the node
/// changed may use it.
pub struct BorrowedOrigin {
    sources: Arc<Vec<SourceSnapshot>>,
    origin: Arc<NodeOrigin>,
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
        (Node::Call(_, _, Some(method)), NameSite::Member) => {
            return Some(NameValue::Symbol(*method))
        }
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
            generation: 0,
        })
    }

    /// Current syntax generation (see the `generation` field). Consumers
    /// keying a cache by (values buffer identity, generation) observe every
    /// syntax change and no provenance-only change.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Shared handle to the syntax buffer. Holding the returned `Arc` keeps
    /// the buffer (and its address) alive, so cache keys built from buffer
    /// identity cannot be confused by address reuse after a drop.
    pub fn values_arc(&self) -> Arc<Vec<Node>> {
        Arc::clone(&self.values)
    }

    /// The single choke point for syntax mutation: every write to the node
    /// buffer goes through here and advances the generation. Audited write
    /// paths (all in this module; the buffer field is private and there is
    /// no `DerefMut`): `push`, `retain_block_statements_with_code`,
    /// `truncate`, `reset_for_reuse`, `rewrite`, `rewrite_with_code`,
    /// `IndexMut`, and the `&mut` iterator. New mutation paths must route
    /// through this helper; the generation unit tests pin the current set.
    fn values_mut(&mut self) -> &mut Vec<Node> {
        self.generation = self.generation.wrapping_add(1);
        Arc::make_mut(&mut self.values)
    }

    /// Append a node. New nodes have unknown origin until a transformation attributes them.
    pub fn push(&mut self, node: Node) {
        self.values_mut().push(node);
        if let Some(origins) = &mut self.origins {
            Arc::make_mut(&mut origins.slots).push(None);
        }
    }
    /// Remove statements without invalidating unrelated nodes inspected by the
    /// cleanup traversal. Surviving children retain their own source origins.
    pub fn retain_block_statements(
        &mut self,
        keep: impl FnMut(NodeId) -> bool,
        transformation: &str,
    ) {
        self.retain_block_statements_with_code(keep, &Arc::from(transformation));
    }

    /// Same as `retain_block_statements`, but reuses a caller-owned code
    /// string for every block derive and removal reason in the call, so a
    /// batch removal (e.g. stripping many declarations in one pass) pays
    /// for the code string once instead of once per removed statement.
    /// The recorded strings are compared by content, so the recorded
    /// origins are identical; only the allocations are shared.
    pub fn retain_block_statements_with_code(
        &mut self,
        mut keep: impl FnMut(NodeId) -> bool,
        code: &Arc<str>,
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
            self.values_mut()[id] = Node::Block(statements);
            if let Some(slot) = self
                .origins
                .as_mut()
                .and_then(|table| Arc::make_mut(&mut table.slots)[id].as_mut())
            {
                if let Some(origin) = Arc::make_mut(slot).origin.as_mut() {
                    origin.derive_in_place_code(code);
                }
            }
        }
        for (owner, node) in removed {
            self.record_removal_with_code(owner, node, code);
        }
    }

    /// Discard trial nodes and their origins together. Reused slots start unknown.
    pub fn truncate(&mut self, len: usize) {
        if len >= self.values.len() {
            return;
        }
        self.values_mut().truncate(len);
        if let Some(origins) = &mut self.origins {
            Arc::make_mut(&mut origins.slots).truncate(len);
        }
    }
    /// Reset a trial arena for the next call, keeping its buffers.
    /// Values and slots are truncated to zero (the slots keep their
    /// first page, so the next call does not pay page setup again);
    /// the resulting arena is indistinguishable from a fresh
    /// `empty_like` arena except for capacities. Source snapshots
    /// and the arena-level disposition table are untouched.
    pub fn reset_for_reuse(&mut self) {
        self.values_mut().clear();
        if let Some(origins) = &mut self.origins {
            Arc::make_mut(&mut origins.slots).clear_keep_page();
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
            generation: 0,
        }
    }
    /// Attach a fresh origin table to an arena built without provenance, sharing
    /// the source's snapshots so per-node attributions can be transferred onto
    /// the already built values instead of rebuilding the whole tree. One
    /// empty slot is created per existing value. Does nothing when this arena
    /// already tracks origins or the source carries none.
    pub fn attach_origins_like(&mut self, source: &Self) {
        if self.origins.is_some() {
            return;
        }
        let Some(source_origins) = source.origins.as_ref() else {
            return;
        };
        self.origins = Some(ArenaOrigins {
            sources: Arc::clone(&source_origins.sources),
            slots: Arc::new(OriginSlots::from(vec![None; self.len()])),
            dispositions: Arc::clone(&source_origins.dispositions),
        });
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
            generation: self.generation,
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
    fn restore_shared(
        &mut self,
        node: NodeId,
        sources: &Arc<Vec<SourceSnapshot>>,
        origin: &Arc<NodeOrigin>,
    ) -> bool {
        let Some(origins) = &mut self.origins else {
            return false;
        };
        if !Arc::ptr_eq(&origins.sources, sources) {
            return false;
        }
        if origins.slots[node as usize]
            .as_ref()
            .is_some_and(|slot| Arc::ptr_eq(slot, origin))
        {
            return true;
        }
        Arc::make_mut(&mut origins.slots)[node as usize] = Some(Arc::clone(origin));
        true
    }

    fn import(&mut self, sources: &Arc<Vec<SourceSnapshot>>, origin: &NodeOrigin) -> NodeOrigin {
        let Some(origins) = self.origins.as_mut() else {
            unreachable!("origin import requires a tracked target")
        };
        let mut imported = (*origin).clone();
        if Arc::ptr_eq(&origins.sources, sources) || origins.sources == *sources {
            return imported;
        }
        let mut source_ids = Vec::with_capacity(sources.len());
        for source in sources.iter() {
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
        for disposition in &mut imported.dispositions {
            disposition.original.source = source_ids[disposition.original.source as usize];
            for span in &mut disposition.replacement_sources {
                span.source = source_ids[span.source as usize];
            }
        }
        for context in &mut imported.contexts {
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
            let imported = self.import(&snapshot.sources, &snapshot.origin);
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
        self.finish_rewrite_with_code(node, snapshot, &Arc::from(transformation));
    }
    /// Same as `finish_rewrite`, but reuses a caller-owned code string for
    /// the origin, the reason, and the fallback detachment reasons instead of
    /// allocating a fresh `Arc<str>` per node. The recorded origins are
    /// identical; only the allocations are shared. Callers rewriting many
    /// nodes with one rule should build the code string once and use this.
    pub fn finish_rewrite_with_code(
        &mut self,
        node: NodeId,
        snapshot: Option<OriginSnapshot>,
        code: &Arc<str>,
    ) {
        let Some(snapshot) = snapshot.filter(|_| self.tracks_origins()) else {
            return;
        };
        let OriginSnapshot {
            sources,
            node: previous,
            origin,
        } = snapshot;
        self.finish_rewrite_borrowed(node, &previous, sources, origin, code);
    }
    /// Rewrite recording whose previous value is borrowed, not cloned.
    /// `derive_from` callers pass the source arena's value directly: every
    /// use of `previous` below only borrows it (reason shapes, token and
    /// name checks), and the source arena is never mutated during the
    /// call, so the recorded origin is identical to the cloning path
    /// while the per-node value clone disappears.
    fn finish_rewrite_borrowed(
        &mut self,
        node: NodeId,
        previous: &Node,
        sources: Arc<Vec<SourceSnapshot>>,
        origin: Arc<NodeOrigin>,
        code: &Arc<str>,
    ) {
        if !self.tracks_origins() {
            return;
        }
        let unchanged = *previous == self.values[node as usize];
        if unchanged && self.restore_shared(node, &sources, &origin) {
            return;
        }
        let mut imported = if self
            .origins
            .as_ref()
            .is_some_and(|table| Arc::ptr_eq(&table.sources, &sources))
        {
            Arc::unwrap_or_clone(origin)
        } else {
            self.import(&sources, &origin)
        };
        if !unchanged {
            let reason =
                OptimizationReason::rewrite_shared(code, previous, &self.values[node as usize]);
            if let Some(out) = &mut imported.origin {
                out.rewrite_in_place_code(code, reason.clone());
            }
            if let Some(original) = imported.origin.as_ref().and_then(|o| o.value.primary) {
                if matches!(
                    self.values[node as usize],
                    Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
                ) && !matches!(
                    previous,
                    Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
                ) {
                    imported.dispositions.push(SourceDisposition {
                        original,
                        reason: reason.clone(),
                        replacement_sources: Vec::new(),
                    });
                }
            }
            if let (Node::Block(old), Node::Block(new)) = (previous, &self.values[node as usize]) {
                // Recording detachments must not turn a wide block rewrite into
                // a quadratic scan. Preserve deterministic old-source order.
                // Small blocks use a linear scan instead of a HashSet, so a
                // block rewrite that detaches nothing pays no scratch
                // allocation; wide blocks keep the HashSet path. Both paths
                // yield the same set in the same old-source order.
                let gone = if old.len() <= 512 && new.len() <= 512 {
                    old.iter()
                        .filter(|id| !new.contains(id))
                        .copied()
                        .collect::<Vec<_>>()
                } else {
                    let retained = new
                        .iter()
                        .copied()
                        .collect::<std::collections::HashSet<_>>();
                    old.iter()
                        .filter(|id| !retained.contains(id))
                        .copied()
                        .collect::<Vec<_>>()
                };
                // One shared fallback reason for all detached members
                // without a recorded Remove reason: `action` allocates a
                // fresh code string per call, and a wide block rewrite
                // would pay that once per detached member. The recorded
                // struct is identical (same code, operation, and empty
                // before/after/basis/facts), and members with a recorded
                // reason still clone their own.
                let mut fallback: Option<OptimizationReason> = None;
                for id in gone {
                    if let Some(original) = self.primary_origin(id) {
                        let reason = match self
                            .origin_record(id)
                            .and_then(|o| o.find_reason(ReasonOperation::Remove))
                        {
                            Some(recorded) => recorded.clone(),
                            None => fallback
                                .get_or_insert_with(|| {
                                    OptimizationReason::action_shared(
                                        Arc::clone(code),
                                        ReasonOperation::Remove,
                                    )
                                })
                                .clone(),
                        };
                        imported.dispositions.push(SourceDisposition {
                            original,
                            reason,
                            replacement_sources: Vec::new(),
                        });
                    }
                }
            }
            let same_token = |(site, _): &(crate::explanation::SyntaxSite, StoredOrigin)| {
                crate::explanation::syntax_text(previous, *site)
                    == crate::explanation::syntax_text(&self.values[node as usize], *site)
            };
            if !imported.tokens.iter().all(same_token) {
                // A rewrite that drops every token site (for example folding
                // a binary expression to a literal, whose operator site no
                // longer applies) used to clone the shared token vector only
                // to retain nothing from it. Reusing the shared empty vector
                // records exactly the same empty token list without the copy.
                if imported.tokens.iter().any(same_token) {
                    Arc::make_mut(&mut imported.tokens).retain(same_token);
                } else {
                    imported.tokens = empty_tokens();
                }
            }
            let same_name = |(site, _): &(NameSite, StoredOrigin)| {
                name_value(previous, *site).is_some()
                    && name_value(previous, *site) == name_value(&self.values[node as usize], *site)
            };
            if !imported.names.iter().all(same_name) {
                // SmallVec::retain passes &mut items (Vec::retain passed &),
                // so wrap the shared predicate instead of passing it directly.
                imported.names.retain(|entry| same_name(entry));
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
        let old = std::mem::replace(&mut self.values_mut()[node as usize], value);
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
    /// Same as `rewrite`, but reuses a caller-owned code string for the
    /// origin, the reason, and the fallback detachment reasons instead of
    /// allocating a fresh `Arc<str>` per node. Callers rewriting many nodes
    /// with one rule should build the code string once and use this.
    pub fn rewrite_with_code(&mut self, node: NodeId, value: Node, code: &Arc<str>) {
        if self.values[node as usize] == value {
            return;
        }
        let old = std::mem::replace(&mut self.values_mut()[node as usize], value);
        let snapshot = self.origins.as_mut().and_then(|origins| {
            Arc::make_mut(&mut origins.slots)[node as usize]
                .take()
                .map(|origin| OriginSnapshot {
                    sources: Arc::clone(&origins.sources),
                    node: old,
                    origin,
                })
        });
        self.finish_rewrite_with_code(node, snapshot, code);
    }
    /// Known symbol-only renaming. The caller must not reorder declaration or parameter slots.
    /// New symbol spellings still point to each original identifier occurrence.
    pub fn finish_rename(&mut self, node: NodeId, snapshot: Option<OriginSnapshot>) {
        // One shared code string for the origin and every name entry of this
        // node; callers renaming many nodes should use `finish_rename_with_code`
        // to share a single code string across the whole rename instead.
        let code: Arc<str> = Arc::from("scope-renaming");
        self.finish_rename_with_code(node, snapshot, &code);
    }
    /// Same as `finish_rename`, but the transformation code string is supplied
    /// by the caller so a batch rename allocates it once instead of once per
    /// origin and name entry. The recorded origins are identical to
    /// `finish_rename`: the code is compared and stored by string content.
    pub fn finish_rename_with_code(
        &mut self,
        node: NodeId,
        snapshot: Option<OriginSnapshot>,
        code: &Arc<str>,
    ) {
        self.finish_rename_inner(node, snapshot, code, None);
    }
    /// Batch variant of `finish_rename_with_code`: the caller renames many
    /// nodes with one code string and builds `shared` once from that same
    /// code, so seed origins in the batch share one reason-tail node each
    /// for the body and name derivations instead of allocating one per
    /// origin. The recorded origins are identical to `finish_rename`.
    pub fn finish_rename_with_shared(
        &mut self,
        node: NodeId,
        snapshot: Option<OriginSnapshot>,
        code: &Arc<str>,
        shared: &RenameSharedTails,
    ) {
        self.finish_rename_inner(node, snapshot, code, Some(shared));
    }
    /// Borrowed form of [`OriginSnapshot`] for renames: the caller already
    /// knows the node value changed (a renamed spelling differs by
    /// construction), so the snapshot's node clone is never read by the
    /// rename path. Capturing only the sources/origin Arcs avoids cloning
    /// the node value, which for multi-symbol nodes copies several
    /// symbol/value vectors per renamed node.
    pub fn capture_origin_ref(&self, node: NodeId) -> Option<BorrowedOrigin> {
        let origins = self.origins.as_ref()?;
        let origin = origins.slots.get(node as usize)?.as_ref()?;
        Some(BorrowedOrigin {
            sources: Arc::clone(&origins.sources),
            origin: Arc::clone(origin),
        })
    }
    /// Rename finish for a node whose value is known to have changed.
    /// Precondition: the caller changed the node's spelling (so the node
    /// value differs from the captured one) and captured the origin
    /// before mutating it. The recorded origin is identical to
    /// [`Self::finish_rename_with_shared`] for such nodes: the unchanged-value
    /// restore path is unreachable for them, and the imported origin,
    /// derived body/name reasons, and slot assignment are the same.
    pub fn finish_rename_changed(
        &mut self,
        node: NodeId,
        origin: BorrowedOrigin,
        code: &Arc<str>,
        shared: Option<&RenameSharedTails>,
    ) {
        if !self.tracks_origins() {
            return;
        }
        let mut imported = if self
            .origins
            .as_ref()
            .is_some_and(|table| Arc::ptr_eq(&table.sources, &origin.sources))
        {
            (*origin.origin).clone()
        } else {
            // `import` itself returns a plain clone when the sources are
            // equal by value, and remaps source ids otherwise.
            self.import(&origin.sources, &origin.origin)
        };
        if let Some(value) = &mut imported.origin {
            match shared {
                Some(shared) => value.derive_in_place_code_shared(code, &shared.rewrite),
                None => value.derive_in_place_code(code),
            }
        }
        for (_, name_origin) in imported.names.iter_mut() {
            match shared {
                Some(shared) => name_origin.derive_action_code_shared(
                    code,
                    ReasonOperation::Rename,
                    &shared.rename,
                ),
                None => name_origin.derive_action_code(code, ReasonOperation::Rename),
            }
        }
        if let Some(origins) = &mut self.origins {
            Arc::make_mut(&mut origins.slots)[node as usize] = Some(Arc::new(imported));
        }
    }
    fn finish_rename_inner(
        &mut self,
        node: NodeId,
        snapshot: Option<OriginSnapshot>,
        code: &Arc<str>,
        shared: Option<&RenameSharedTails>,
    ) {
        let Some(snapshot) = snapshot.filter(|_| self.tracks_origins()) else {
            return;
        };
        let changed = snapshot.node != self.values[node as usize];
        if !changed && self.restore_shared(node, &snapshot.sources, &snapshot.origin) {
            return;
        }
        let (_, mut imported) = self.import_owned(snapshot);
        if changed {
            if let Some(value) = &mut imported.origin {
                match shared {
                    Some(shared) => value.derive_in_place_code_shared(code, &shared.rewrite),
                    None => value.derive_in_place_code(code),
                }
            }
            for (_, origin) in imported.names.iter_mut() {
                match shared {
                    Some(shared) => origin.derive_action_code_shared(
                        code,
                        ReasonOperation::Rename,
                        &shared.rename,
                    ),
                    None => origin.derive_action_code(code, ReasonOperation::Rename),
                }
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
        // Borrow the previous value from the source arena instead of
        // cloning it into a snapshot: every use in the rewrite path only
        // borrows it, and the source arena is never mutated during the
        // call, so the recorded origin is identical. A missing source
        // origin clears the target slot, exactly like the previous
        // capture-then-finish path with a `None` snapshot.
        let Some(source_origins) = source.origins.as_ref() else {
            if let Some(origins) = &mut self.origins {
                Arc::make_mut(&mut origins.slots)[node as usize] = None;
            }
            return;
        };
        let Some(origin) = source_origins
            .slots
            .get(original as usize)
            .and_then(|slot| slot.as_ref())
        else {
            if let Some(origins) = &mut self.origins {
                Arc::make_mut(&mut origins.slots)[node as usize] = None;
            }
            return;
        };
        if let Some(origins) = &mut self.origins {
            Arc::make_mut(&mut origins.slots)[node as usize] = None;
        }
        let previous = &source.values[original as usize];
        let sources = Arc::clone(&source_origins.sources);
        let origin = Arc::clone(origin);
        self.finish_rewrite_borrowed(node, previous, sources, origin, &Arc::from(transformation));
    }
    /// Same as `derive_from`, but reuses a caller-owned code string for the
    /// rewrite reason instead of allocating a fresh `Arc<str>` per node.
    /// The fast path is identical (no allocation either way); only the slow
    /// path benefits. Callers deriving many nodes under one rule should
    /// build the code string once and use this. The recorded origins are
    /// identical; only the allocations are shared.
    pub fn derive_from_with_code(
        &mut self,
        node: NodeId,
        source: &Self,
        original: NodeId,
        code: &Arc<str>,
    ) {
        let Some(target_origins) = self.origins.as_mut() else {
            return;
        };
        if let Some(source_origins) = source.origins.as_ref() {
            if Arc::ptr_eq(&target_origins.sources, &source_origins.sources)
                && self.values[node as usize] == source.values[original as usize]
            {
                // Same explicit-input-node fast path as `derive_from`.
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
        // Borrow the previous value from the source arena instead of
        // cloning it into a snapshot: every use in the rewrite path only
        // borrows it, and the source arena is never mutated during the
        // call, so the recorded origin is identical. A missing source
        // origin clears the target slot, exactly like the previous
        // capture-then-finish path with a `None` snapshot.
        let Some(source_origins) = source.origins.as_ref() else {
            if let Some(origins) = &mut self.origins {
                Arc::make_mut(&mut origins.slots)[node as usize] = None;
            }
            return;
        };
        let Some(origin) = source_origins
            .slots
            .get(original as usize)
            .and_then(|slot| slot.as_ref())
        else {
            if let Some(origins) = &mut self.origins {
                Arc::make_mut(&mut origins.slots)[node as usize] = None;
            }
            return;
        };
        if let Some(origins) = &mut self.origins {
            Arc::make_mut(&mut origins.slots)[node as usize] = None;
        }
        let previous = &source.values[original as usize];
        let sources = Arc::clone(&source_origins.sources);
        let origin = Arc::clone(origin);
        self.finish_rewrite_borrowed(node, previous, sources, origin, code);
    }
    /// Same as [`Self::derive_from_with_code`], but the contributor was
    /// captured before the caller mutated the node (see
    /// [`Self::capture_origin_ref`]). The caller must already have replaced
    /// the node's value, so the fast path that shares the slot for an
    /// unchanged value is inapplicable; the recorded origin is identical
    /// to deriving from a separate source arena holding the same slot,
    /// because `finish_rewrite_borrowed` only reads the previous value,
    /// the source table, and the captured origin. Callers replaying a
    /// transformation onto the arena it was derived from use this to
    /// avoid cloning the arena just to keep a pristine read source.
    pub fn derive_from_borrowed_with_code(
        &mut self,
        node: NodeId,
        previous: &Node,
        contributor: &BorrowedOrigin,
        code: &Arc<str>,
    ) {
        if let Some(origins) = self.origins.as_mut() {
            Arc::make_mut(&mut origins.slots)[node as usize] = None;
        }
        self.finish_rewrite_borrowed(
            node,
            previous,
            Arc::clone(&contributor.sources),
            Arc::clone(&contributor.origin),
            code,
        );
    }
    /// Same-arena counterpart of [`Self::derive_from_clone`]: attribute a
    /// structurally cloned node to its source construct when the clone
    /// lives in this same arena (the clone's child IDs were remapped by
    /// the caller). The kind-matching path shares the slot exactly like
    /// `derive_from_clone`; the kind-mismatching fallback derives from
    /// the captured origin instead of a second arena, with identical
    /// recorded content. The fallback is unreachable from a
    /// child-remapping clone, which preserves the node kind by
    /// construction.
    pub fn derive_from_clone_self(&mut self, node: NodeId, original: NodeId, transformation: &str) {
        let same_kind = std::mem::discriminant(&self.values[node as usize])
            == std::mem::discriminant(&self.values[original as usize]);
        if !same_kind {
            let previous = self.values[original as usize].clone();
            let code: Arc<str> = Arc::from(transformation);
            match self.capture_origin_ref(original) {
                Some(borrowed) => {
                    self.derive_from_borrowed_with_code(node, &previous, &borrowed, &code)
                }
                None => {
                    if let Some(origins) = self.origins.as_mut() {
                        Arc::make_mut(&mut origins.slots)[node as usize] = None;
                    }
                }
            }
            return;
        }
        let Some(target_origins) = self.origins.as_mut() else {
            return;
        };
        let source_slot = target_origins.slots[original as usize].clone();
        let same = match (&target_origins.slots[node as usize], &source_slot) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        };
        if !same {
            Arc::make_mut(&mut target_origins.slots)[node as usize] = source_slot;
        }
    }
    /// Attribute a structurally cloned node to its source construct when the
    /// clone remapped child IDs. `derive_from`'s fast path requires the whole
    /// node value to be equal, so it misses clones whose subtrees were
    /// skipped or replaced: every later node then has shifted child IDs and
    /// falls through to the rewrite-reason path even though nothing about
    /// the construct itself changed. The caller guarantees `node` is a clone
    /// of `original` (same kind and same identifier/literal sites, only the
    /// child numbering may differ); anything else must use `derive_from`
    /// so a genuine replacement still records its rewrite reason. When the
    /// kind differs, or the arenas do not share source snapshots, this
    /// falls back to `derive_from`. Sharing the slot is sound for the same
    /// reason as `derive_from`'s fast path: later mutation paths copy the
    /// slot before writing, and `IndexMut` clears it first.
    pub fn derive_from_clone(
        &mut self,
        node: NodeId,
        source: &Self,
        original: NodeId,
        transformation: &str,
    ) {
        let same_kind = std::mem::discriminant(&self.values[node as usize])
            == std::mem::discriminant(&source.values[original as usize]);
        let shared_sources = matches!(
            (&self.origins, &source.origins),
            (Some(target), Some(source_origins))
                if Arc::ptr_eq(&target.sources, &source_origins.sources)
        );
        if !same_kind || !shared_sources {
            self.derive_from(node, source, original, transformation);
            return;
        }
        let Some(target_origins) = self.origins.as_mut() else {
            return;
        };
        let Some(source_origins) = source.origins.as_ref() else {
            return;
        };
        let source_slot = &source_origins.slots[original as usize];
        let same = match (&target_origins.slots[node as usize], source_slot) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        };
        if !same {
            Arc::make_mut(&mut target_origins.slots)[node as usize] = source_slot.clone();
        }
    }
    /// Explicitly identify synthesized helper code. Unknown is not synthetic by default.
    ///
    /// A synthetic origin is fully determined by its transformation name
    /// (the rest is `NodeOrigin::default`), so nodes marked with the same
    /// name share one origin node instead of allocating one per node.
    /// Sharing is copy-on-write safe: every in-place origin mutation goes
    /// through `Arc::make_mut`, which clones a shared node before editing
    /// it, so sharers never observe each other's edits and the recorded
    /// origin content is identical to a fresh node.
    pub fn mark_synthetic(&mut self, node: NodeId, transformation: &str) {
        if let Some(origins) = &mut self.origins {
            Arc::make_mut(&mut origins.slots)[node as usize] =
                Some(shared_synthetic_origin(transformation));
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
            None,
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
            None,
        );
    }
    fn relate_snapshot(
        &mut self,
        node: NodeId,
        snapshot: Option<ContributionSnapshot>,
        transformation: &str,
        role: RelationRole,
        code: Option<&Arc<str>>,
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
        // A copied origin often already shares the entire contributor tail.
        // Containment established before append-only updates stays true below.
        // Large exact prefixes get one linear comparison instead of searching
        // the growing target once per item; no per-call hash table is allocated.
        let related_present = Arc::ptr_eq(&origin.value.related, &contributor.related)
            || (contributor.related.len() >= 8
                && origin.value.related.starts_with(&contributor.related));
        let relations_present = Arc::ptr_eq(&origin.value.relations, &contributor.relations)
            || (contributor.relations.len() >= 8
                && origin.value.relations.starts_with(&contributor.relations));
        match code {
            Some(code) => origin.derive_action_code(code, ReasonOperation::Relate),
            None => origin.derive_action(transformation, ReasonOperation::Relate),
        }
        // Fast path: a single-span contributor with no other relations,
        // related to an origin holding no related spans or relations yet.
        // The push path below would record exactly one related span
        // (unless the target already is that span) and one typed relation
        // for the primary span, so assigning the interned singletons
        // records identical content without per-call Arc-box and buffer
        // allocations. This is the immutable-carrier replay shape
        // (hundreds of occurrences related to one definition span).
        if contributor.related.is_empty() && contributor.relations.is_empty() {
            if let Some(span) = contributor.primary {
                if origin.value.related.is_empty() && origin.value.relations.is_empty() {
                    if origin.value.primary != Some(span) {
                        origin.value.related = shared_singleton_related(span);
                    }
                    origin.value.relations = shared_singleton_relation(role, span);
                    return;
                }
            }
        }
        for span in contributor.primary.iter().chain(contributor.related.iter()) {
            let known_related = related_present && contributor.primary != Some(*span);
            if origin.value.primary != Some(*span)
                && !known_related
                && !origin.value.related.contains(span)
            {
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
        if !relations_present {
            for relation in contributor.relations.iter() {
                if !origin.value.relations.contains(relation) {
                    Arc::make_mut(&mut origin.value.relations).push(relation.clone());
                }
            }
        }
    }
    #[cfg(test)]
    fn reference_relate_snapshot(
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
            .and_then(|snapshot| self.import(&snapshot.sources, &snapshot.origin).origin);
        let Some(origins) = self.origins.as_mut() else {
            return;
        };
        let slot = Arc::make_mut(&mut origins.slots)[node as usize]
            .get_or_insert_with(|| Arc::new(NodeOrigin::default()));
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
        let imported = self.import(&snapshot.sources, &snapshot.origin);
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
            contexts.insert(0, context);
        }
    }
    // Only carry checks actually recorded on this detached construct. A prior
    // value rewrite is not a reason to remove it; eligibility is explicitly tagged.
    fn removal_reason_with_code(&self, node: NodeId, code: &Arc<str>) -> OptimizationReason {
        let recorded = self
            .origin_record(node)
            .and_then(|o| o.find_reason(ReasonOperation::Remove));
        recorded.cloned().unwrap_or_else(|| {
            OptimizationReason::action_shared(Arc::clone(code), ReasonOperation::Remove)
        })
    }
    /// Record why a currently selected construct is about to be detached.
    pub fn explain_removal(&mut self, node: NodeId, mut reason: OptimizationReason) {
        reason.operation = ReasonOperation::Remove;
        self.explain(node, reason);
    }
    fn record_removal_with_code(&mut self, owner: NodeId, node: NodeId, code: &Arc<str>) {
        let Some(original) = self.primary_origin(node) else {
            return;
        };
        let reason = self.removal_reason_with_code(node, code);
        if let Some(slot) = self
            .origins
            .as_mut()
            .and_then(|table| Arc::make_mut(&mut table.slots)[owner as usize].as_mut())
        {
            Arc::make_mut(slot).dispositions.push(SourceDisposition {
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
            None,
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
            None,
        );
    }

    /// Record a typed relationship within this candidate arena, reusing
    /// a caller-owned transformation string instead of allocating a
    /// fresh `Arc<str>` per node. The recorded origin is identical;
    /// only the allocation is shared.
    pub fn relate_within_role_code(
        &mut self,
        node: NodeId,
        original: NodeId,
        code: &Arc<str>,
        role: RelationRole,
    ) {
        self.relate_snapshot(
            node,
            self.capture_contribution(original),
            code.as_ref(),
            role,
            Some(code),
        );
    }

    /// Cross-arena counterpart of `relate_within_role_code`: record a typed
    /// relationship from a node of another arena, reusing a caller-owned
    /// transformation string instead of allocating a fresh `Arc<str>` per
    /// node. The recorded origin is identical; only the allocation is
    /// shared. Callers relating many copied nodes with one rule should
    /// build the code string once and use this.
    pub fn relate_from_role_code(
        &mut self,
        node: NodeId,
        source: &Self,
        original: NodeId,
        code: &Arc<str>,
        role: RelationRole,
    ) {
        self.relate_snapshot(
            node,
            source.capture_contribution(original),
            code.as_ref(),
            role,
            Some(code),
        );
    }
    /// Same-arena counterpart of [`Self::relate_from_role_code`] for a
    /// contributor captured before the caller mutated it (see
    /// [`Self::capture_origin_ref`]). The snapshot carries exactly the
    /// contribution `relate_from_role_code` would capture from a pristine
    /// arena, so the recorded relation is identical; a `None`
    /// contributor records nothing, just like an unknown contributor node.
    pub fn relate_from_borrowed_role_code(
        &mut self,
        node: NodeId,
        contributor: Option<&BorrowedOrigin>,
        code: &Arc<str>,
        role: RelationRole,
    ) {
        let snapshot = contributor.and_then(|borrowed| {
            let origin = borrowed.origin.origin.as_ref()?.value.clone();
            Some(ContributionSnapshot {
                sources: Arc::clone(&borrowed.sources),
                origin,
            })
        });
        self.relate_snapshot(node, snapshot, code.as_ref(), role, Some(code));
    }

    /// Record a constant-folded atom's origin in one step, without
    /// replaying the fold that produced it.
    ///
    /// `node` already holds the folded atom value. Its origin starts as
    /// the folded construct's slot, shared exactly as the fold's
    /// kind-matching `derive_from_clone` would have shared it (a
    /// kind-mismatched derive would instead record a spurious rewrite
    /// reason, so the slot is shared here, not derived). This then
    /// applies exactly the origin edits the tiny-arena fold would have
    /// applied to its rewritten root: the rewrite reason for the
    /// `previous` Bin/Un value, the atom-replacement disposition, the
    /// operator-token drop, the name drop, and the optional evaluation
    /// decision reason. Building the final slot in one step avoids the
    /// fold's clone, resolve, and per-node derive work; the recorded
    /// origin content is identical, only its allocations are shared.
    /// `previous` is only read for its kind, operator, and literal
    /// shape, so its child ids are never dereferenced. Returns false
    /// when the source arena's snapshots differ from this arena's, in
    /// which case the caller must fall back to the fold path.
    pub fn record_folded_atom_origin(
        &mut self,
        node: NodeId,
        source: &Self,
        original: NodeId,
        previous: &Node,
        code: &Arc<str>,
        decision: Option<OptimizationReason>,
    ) -> bool {
        if !self.tracks_origins() {
            return true;
        }
        let Some(target_origins) = self.origins.as_ref() else {
            return true;
        };
        let Some(source_origins) = source.origins.as_ref() else {
            return true;
        };
        if !Arc::ptr_eq(&target_origins.sources, &source_origins.sources)
            && target_origins.sources != source_origins.sources
        {
            return false;
        }
        let Some(slot) = source_origins
            .slots
            .get(original as usize)
            .and_then(|slot| slot.as_ref())
        else {
            return true;
        };
        let mut imported = (**slot).clone();
        let current = self.values[node as usize].clone();
        if let Some(out) = imported.origin.as_mut() {
            let reason = OptimizationReason::rewrite_shared(code, previous, &current);
            out.rewrite_in_place_code(code, reason.clone());
            if matches!(
                current,
                Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
            ) && !matches!(
                previous,
                Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
            ) {
                if let Some(original) = out.value.primary {
                    imported.dispositions.push(SourceDisposition {
                        original,
                        reason,
                        replacement_sources: Vec::new(),
                    });
                }
            }
        }
        let same_token = |(site, _): &(SyntaxSite, StoredOrigin)| {
            crate::explanation::syntax_text(previous, *site)
                == crate::explanation::syntax_text(&current, *site)
        };
        if !imported.tokens.iter().all(same_token) {
            if imported.tokens.iter().any(same_token) {
                let kept: Vec<(SyntaxSite, StoredOrigin)> = imported
                    .tokens
                    .iter()
                    .filter(|entry| same_token(entry))
                    .cloned()
                    .collect();
                imported.tokens = Arc::new(kept);
            } else {
                imported.tokens = empty_tokens();
            }
        }
        let same_name = |(site, _): &(NameSite, StoredOrigin)| {
            name_value(previous, *site).is_some()
                && name_value(previous, *site) == name_value(&current, *site)
        };
        if !imported.names.iter().all(same_name) {
            imported.names.retain(|entry| same_name(entry));
        }
        if let Some(decision) = decision {
            if let Some(out) = imported.origin.as_mut() {
                out.add_reason(decision);
            }
        }
        if let Some(origins) = &mut self.origins {
            Arc::make_mut(&mut origins.slots)[node as usize] = Some(Arc::new(imported));
        }
        true
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
    fn rewrite_dropping_all_tokens_reuses_empty_token_vector() {
        // A binary expression carries only its operator token; folding it to
        // a literal drops that site, so the rewrite must record an empty
        // token list. The empty list must be the shared empty vector, not a
        // fresh copy of the dropped token list.
        let (ast, _root) = parse_source_with_origins("cow.lua", "a=1+2").unwrap();
        let mut copy = ast.clone();
        let bin = (0..copy.nodes.len())
            .find(|&i| matches!(copy.node(i as u32), Node::Bin(..)))
            .expect("bin node") as u32;
        let before = copy.nodes.origins.as_ref().unwrap().slots[bin as usize]
            .as_ref()
            .unwrap()
            .tokens
            .len();
        assert_eq!(before, 1);
        let snapshot = copy.nodes.capture_origin(bin);
        copy.nodes[bin as usize] = Node::Num("3".into());
        copy.nodes
            .finish_rewrite_with_code(bin, snapshot, &Arc::from("constant-folding"));
        let slot = copy.nodes.origins.as_ref().unwrap().slots[bin as usize]
            .as_ref()
            .unwrap();
        assert!(slot.tokens.is_empty());
        assert!(Arc::ptr_eq(
            &slot.tokens,
            &crate::provenance::empty_tokens()
        ));
        assert!(copy.node(bin) == &Node::Num("3".into()));
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
    fn finish_rename_with_code_matches_finish_rename() {
        let (ast, root) = parse_source_with_origins(
            "rename.lua",
            "local foo=1 function onTick()output.setNumber(1,foo)end",
        )
        .unwrap();
        let name = ast
            .nodes
            .iter()
            .position(|n| matches!(n, Node::Name(_)))
            .unwrap() as NodeId;
        let mut renamed = ast.clone();
        let replacement = renamed.strings.intern("bar");
        let mut shared = ast.clone();
        let shared_replacement = shared.strings.intern("bar");
        assert_eq!(replacement, shared_replacement);
        let snapshot = renamed.nodes.capture_origin(name);
        renamed.nodes[name as usize] = Node::Name(replacement);
        renamed.nodes.finish_rename(name, snapshot);
        let snapshot = shared.nodes.capture_origin(name);
        shared.nodes[name as usize] = Node::Name(shared_replacement);
        let code: Arc<str> = Arc::from("scope-renaming");
        shared.nodes.finish_rename_with_code(name, snapshot, &code);
        let mut batched = ast.clone();
        let batched_replacement = batched.strings.intern("bar");
        assert_eq!(replacement, batched_replacement);
        let snapshot = batched.nodes.capture_origin(name);
        batched.nodes[name as usize] = Node::Name(batched_replacement);
        let tails = RenameSharedTails::new(&code);
        batched
            .nodes
            .finish_rename_with_shared(name, snapshot, &code, &tails);
        assert_eq!(renamed.nodes.origin(name), batched.nodes.origin(name));
        assert_eq!(
            renamed
                .nodes
                .name_origin(name, crate::parser::NameSite::Reference),
            batched
                .nodes
                .name_origin(name, crate::parser::NameSite::Reference)
        );
        assert_eq!(
            serde_json::to_vec(&renamed).unwrap(),
            serde_json::to_vec(&batched).unwrap()
        );
        assert_eq!(renamed.nodes.origin(name), shared.nodes.origin(name));
        assert_eq!(
            renamed
                .nodes
                .name_origin(name, crate::parser::NameSite::Reference),
            shared
                .nodes
                .name_origin(name, crate::parser::NameSite::Reference)
        );
        assert_eq!(
            serde_json::to_vec(&renamed).unwrap(),
            serde_json::to_vec(&shared).unwrap()
        );
        assert_eq!(
            Printer::new(&renamed, false)
                .output_with_positions(root)
                .code,
            Printer::new(&shared, false)
                .output_with_positions(root)
                .code
        );
    }

    #[test]
    fn finish_rename_changed_matches_finish_rename() {
        let (ast, root) = parse_source_with_origins(
            "rename.lua",
            "local foo,bar=1,2 function onTick()output.setNumber(1,foo+bar)end",
        )
        .unwrap();
        let local = ast
            .nodes
            .iter()
            .position(|n| matches!(n, Node::Local(..)))
            .unwrap() as NodeId;
        let name = ast
            .nodes
            .iter()
            .position(|n| matches!(n, Node::Name(_)))
            .unwrap() as NodeId;
        let code: Arc<str> = Arc::from("scope-renaming");
        // Old path (snapshot with node clone) for both a multi-symbol
        // Local node and a single Name node.
        let mut old = ast.clone();
        let old_local = old.strings.intern("baz");
        let old_name = old.strings.intern("qux");
        let snapshot = old.nodes.capture_origin(local);
        if let Node::Local(symbols, _) = &mut old.nodes[local as usize] {
            symbols[0] = old_local;
        } else {
            unreachable!()
        }
        old.nodes.finish_rename(local, snapshot);
        let snapshot = old.nodes.capture_origin(name);
        old.nodes[name as usize] = Node::Name(old_name);
        old.nodes.finish_rename(name, snapshot);
        // Borrowed path: capture the origin reference before mutating,
        // then finish with the changed-value precondition.
        let mut borrowed = ast.clone();
        let borrowed_local = borrowed.strings.intern("baz");
        let borrowed_name = borrowed.strings.intern("qux");
        assert_eq!(old_local, borrowed_local);
        assert_eq!(old_name, borrowed_name);
        let origin_ref = borrowed.nodes.capture_origin_ref(local);
        if let Node::Local(symbols, _) = &mut borrowed.nodes[local as usize] {
            symbols[0] = borrowed_local;
        } else {
            unreachable!()
        }
        let tails = RenameSharedTails::new(&code);
        if let Some(origin_ref) = origin_ref {
            borrowed
                .nodes
                .finish_rename_changed(local, origin_ref, &code, Some(&tails));
        }
        let origin_ref = borrowed.nodes.capture_origin_ref(name);
        borrowed.nodes[name as usize] = Node::Name(borrowed_name);
        if let Some(origin_ref) = origin_ref {
            borrowed
                .nodes
                .finish_rename_changed(name, origin_ref, &code, Some(&tails));
        }
        assert_eq!(old.nodes.origin(local), borrowed.nodes.origin(local));
        assert_eq!(old.nodes.origin(name), borrowed.nodes.origin(name));
        assert_eq!(
            old.nodes
                .name_origin(local, crate::parser::NameSite::Binding(0)),
            borrowed
                .nodes
                .name_origin(local, crate::parser::NameSite::Binding(0))
        );
        assert_eq!(
            old.nodes
                .name_origin(name, crate::parser::NameSite::Reference),
            borrowed
                .nodes
                .name_origin(name, crate::parser::NameSite::Reference)
        );
        assert_eq!(
            serde_json::to_vec(&old).unwrap(),
            serde_json::to_vec(&borrowed).unwrap()
        );
        assert_eq!(
            Printer::new(&old, false).output_with_positions(root).code,
            Printer::new(&borrowed, false)
                .output_with_positions(root)
                .code
        );
    }

    #[test]
    fn synthetic_marks_share_one_origin_and_cow_mutation_stays_local() {
        let (ast, root) = parse_source_with_origins("synth.lua", "local x=1 local y=2").unwrap();
        let other = ast
            .nodes
            .iter()
            .position(|n| !matches!(n, Node::Block(_)))
            .unwrap() as NodeId;
        let mut marked = ast.clone();
        marked.nodes.mark_synthetic(root, "shared-test");
        marked.nodes.mark_synthetic(other, "shared-test");
        let shared = marked.nodes.origins.as_ref().unwrap().slots[root as usize]
            .as_ref()
            .unwrap()
            .clone();
        assert!(Arc::ptr_eq(
            &shared,
            marked.nodes.origins.as_ref().unwrap().slots[other as usize]
                .as_ref()
                .unwrap()
        ));
        // Dropping one statement from the root block mutates the root's
        // origin in place through copy-on-write: the root detaches, while
        // the other sharer keeps the original shared node.
        let Node::Block(statements) = marked.node(root).clone() else {
            panic!("root is not a block");
        };
        let keep_first = statements[0];
        marked
            .nodes
            .retain_block_statements(|id| id == keep_first, "cow-test");
        let after = marked.nodes.origins.as_ref().unwrap().slots[other as usize]
            .as_ref()
            .unwrap();
        assert!(Arc::ptr_eq(&shared, after));
        assert!(marked.nodes.origin(other).is_some());
    }

    #[test]
    fn rewrite_with_code_matches_rewrite() {
        // Statement rewrite with fixed shapes (local -> assignment) plus a
        // block rewrite that detaches a member (shared fallback Remove
        // reason): both paths must record identical origins and AST bytes.
        let (ast, root) = parse_source_with_origins("rewrite.lua", "local a=1 local b=2").unwrap();
        let Node::Block(statements) = ast.node(root).clone() else {
            panic!("root is not a block");
        };
        let statement = statements[0];
        // Build each side's assignment from its own arena's names, so the
        // two clones stay comparable even though their nodes are distinct.
        let build = |arena: &mut crate::Ast| {
            let Node::Local(names, expressions) = arena.node(statement).clone() else {
                panic!("first statement is not a local");
            };
            let targets = names
                .iter()
                .map(|symbol| {
                    let spelling = arena.strings.get(*symbol).to_string();
                    arena.name(&spelling)
                })
                .collect::<Vec<_>>();
            (targets, expressions)
        };
        let mut plain = ast.clone();
        let (targets, expressions) = build(&mut plain);
        plain
            .nodes
            .rewrite(statement, Node::Assign(targets, expressions), "test-rule");
        let Node::Block(statements) = plain.node(root).clone() else {
            panic!("root is not a block");
        };
        plain
            .nodes
            .rewrite(root, Node::Block(statements[..1].to_vec()), "test-rule");
        let mut shared = ast.clone();
        let code: Arc<str> = Arc::from("test-rule");
        let (targets, expressions) = build(&mut shared);
        shared
            .nodes
            .rewrite_with_code(statement, Node::Assign(targets, expressions), &code);
        let Node::Block(statements) = shared.node(root).clone() else {
            panic!("root is not a block");
        };
        shared
            .nodes
            .rewrite_with_code(root, Node::Block(statements[..1].to_vec()), &code);
        assert_eq!(
            plain.nodes.origin(statement),
            shared.nodes.origin(statement)
        );
        assert_eq!(plain.nodes.origin(root), shared.nodes.origin(root));
        assert_eq!(
            serde_json::to_vec(&plain).unwrap(),
            serde_json::to_vec(&shared).unwrap()
        );
        assert_eq!(
            Printer::new(&plain, false).output_with_positions(root).code,
            Printer::new(&shared, false)
                .output_with_positions(root)
                .code
        );
    }

    #[test]
    fn wide_block_rewrite_uses_the_hashset_detachment_path() {
        // 600 statements exceed the 512 threshold, so the detachment scan
        // takes the HashSet path; the rewrite must keep exactly the first
        // 300 statements and stay deterministic across clones.
        let source = (0..600)
            .map(|i| format!("local a{i}={i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let (ast, root) = parse_source_with_origins("wide.lua", &source).unwrap();
        let rewrite = |ast: &mut crate::Ast| {
            let Node::Block(statements) = ast.node(root).clone() else {
                panic!("root is not a block");
            };
            assert_eq!(statements.len(), 600);
            ast.nodes
                .rewrite(root, Node::Block(statements[..300].to_vec()), "test-rule");
            let Node::Block(kept) = ast.node(root).clone() else {
                panic!("root is not a block");
            };
            assert_eq!(kept.len(), 300);
            Printer::new(ast, false).output_with_positions(root).code
        };
        let mut first = ast.clone();
        let code_first = rewrite(&mut first);
        assert!(code_first.contains("a0"), "{code_first}");
        assert!(!code_first.contains("a599"), "{code_first}");
        let mut second = ast.clone();
        let code_second = rewrite(&mut second);
        assert_eq!(code_first, code_second);
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
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

    #[test]
    fn generation_advances_on_every_syntax_mutation_path() {
        let (ast, root) =
            parse_source_with_origins("gen.lua", "local x=1 output.setNumber(1,x)").unwrap();
        let mut arena = ast.nodes.clone();
        let g0 = arena.generation();

        // Provenance-only edits never advance the generation.
        arena.mark_synthetic(root, "gen-test");
        assert_eq!(arena.generation(), g0);

        // No-op forms of the mutating paths do not advance it either.
        let current = arena[root as usize].clone();
        arena.rewrite(root, current.clone(), "gen-test");
        arena.rewrite_with_code(root, current, &Arc::from("gen-test"));
        arena.retain_block_statements(|_| true, "gen-test");
        arena.truncate(arena.len() + 1);
        assert_eq!(arena.generation(), g0);

        // push
        let extra = arena.len() as NodeId;
        arena.push(Node::Nil);
        assert!(arena.generation() > g0);

        // rewrite / rewrite_with_code with an actually different value
        let g = arena.generation();
        arena.rewrite(extra, Node::Break, "gen-test");
        assert!(arena.generation() > g);
        let g = arena.generation();
        arena.rewrite_with_code(extra, Node::Nil, &Arc::from("gen-test"));
        assert!(arena.generation() > g);

        // truncate back down
        let g = arena.generation();
        arena.truncate(extra as usize);
        assert!(arena.generation() > g);

        // IndexMut
        let g = arena.generation();
        arena[root as usize] = arena[root as usize].clone();
        assert!(arena.generation() > g);

        // &mut iteration (access alone is conservatively a mutation)
        let g = arena.generation();
        for _ in &mut arena {}
        assert!(arena.generation() > g);

        // retain_block_statements with an actual removal
        let (ast2, root2) =
            parse_source_with_origins("gen2.lua", "local a=1 local b=2 output.setNumber(1,a+b)")
                .unwrap();
        let mut arena2 = ast2.nodes.clone();
        let g = arena2.generation();
        let stmts = match &arena2[root2 as usize] {
            Node::Block(stmts) => stmts.clone(),
            _ => panic!("root is not a block"),
        };
        arena2.retain_block_statements(|id| id != stmts[0], "gen-test");
        assert!(arena2.generation() > g);

        // reset_for_reuse
        let g = arena2.generation();
        arena2.reset_for_reuse();
        assert!(arena2.generation() > g);
        assert_eq!(arena2.len(), 0);
    }

    #[test]
    fn generation_follows_clones_and_detach() {
        let (ast, _root) = parse_source_with_origins("gen3.lua", "local x=1").unwrap();
        let mut clone = ast.nodes.clone();
        assert_eq!(clone.generation(), ast.nodes.generation());
        // The origins-free clone shares the buffer and the generation.
        let frozen = ast.nodes.clone_without_origins();
        assert_eq!(frozen.generation(), ast.nodes.generation());
        assert!(Arc::ptr_eq(&frozen.values_arc(), &ast.nodes.values_arc()));
        // Mutating the clone advances only the clone's generation.
        let before = ast.nodes.generation();
        clone.push(Node::Nil);
        assert!(clone.generation() > before);
        assert_eq!(ast.nodes.generation(), before);
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod relation_prefix_tests {
    use super::*;
    use crate::parse_source_with_origins;
    #[test]
    fn shared_and_equal_prefixes_keep_the_exact_order_roles_and_reason_counts() {
        let text = (0..260)
            .map(|i| format!("output.setNumber(1,{i}) "))
            .collect::<String>();
        let (mut source, _) = parse_source_with_origins("relations.lua", &text).unwrap();
        let ids = source
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| matches!(n, Node::Num(_)).then_some(i as NodeId))
            .collect::<Vec<_>>();
        for width in [0, 1, 7, 8, 64, 256] {
            let base = source.clone();
            for &input in &ids[1..=width] {
                source
                    .nodes
                    .relate_from(ids[0], &base.nodes, input, "build-source");
            }
            for detached in [false, true] {
                let mut input = source.clone();
                if detached {
                    input = serde_json::from_slice(&serde_json::to_vec(&input).unwrap()).unwrap();
                }
                for role in [
                    RelationRole::Contribution,
                    RelationRole::Definition,
                    RelationRole::Argument,
                    RelationRole::CallSite,
                ] {
                    let mut expected = source.clone();
                    let mut actual = source.clone();
                    for n in 0..4 {
                        expected.nodes.reference_relate_snapshot(
                            ids[0],
                            input.nodes.capture_contribution(ids[0]),
                            "combine",
                            role,
                        );
                        actual.nodes.relate_snapshot(
                            ids[0],
                            input.nodes.capture_contribution(ids[0]),
                            "combine",
                            role,
                            None,
                        );
                        assert_eq!(
                            serde_json::to_value(&actual).unwrap(),
                            serde_json::to_value(&expected).unwrap(),
                            "width={width} detached={detached} n={n}"
                        );
                        // Appending unrelated contributors leaves a prefix, not
                        // equal vectors. A different primary still gets its role.
                        expected.nodes.reference_relate_snapshot(
                            ids[0],
                            input.nodes.capture_contribution(ids[259 + n]),
                            "append",
                            role,
                        );
                        actual.nodes.relate_snapshot(
                            ids[0],
                            input.nodes.capture_contribution(ids[259 + n]),
                            "append",
                            role,
                            None,
                        );
                    }
                    assert_eq!(
                        serde_json::to_value(&actual).unwrap(),
                        serde_json::to_value(&expected).unwrap()
                    );
                }
            }
        }
    }

    #[test]
    fn record_folded_atom_origin_matches_fold_rewrite() {
        use crate::explanation::{DecisionFactValue, OptimizationReason};
        let code: Arc<str> = Arc::from("constant-folding");
        let cases: Vec<(&str, Node, Node, OptimizationReason)> = vec![
            (
                "return 1+2",
                Node::Bin("+".to_string(), 0, 0),
                Node::Num("3".into()),
                OptimizationReason::decision_shared(
                    "constant-folding",
                    "literal-operands-evaluated-and-size-nonincreasing",
                    [
                        ("operator", DecisionFactValue::Borrowed("+")),
                        ("left", DecisionFactValue::Shared(Arc::from("i64:1"))),
                        ("right", DecisionFactValue::Shared(Arc::from("i64:2"))),
                        ("evaluated", DecisionFactValue::Shared(Arc::from("i64:3"))),
                        ("emitted", DecisionFactValue::Shared(Arc::from("i64:3"))),
                        (
                            "expressionSizeBefore",
                            DecisionFactValue::Shared(Arc::from("3")),
                        ),
                        (
                            "expressionSizeAfter",
                            DecisionFactValue::Shared(Arc::from("1")),
                        ),
                        ("aggressive", DecisionFactValue::Shared(Arc::from("false"))),
                        (
                            "absToleranceBits",
                            DecisionFactValue::Shared(Arc::from("0000000000000000")),
                        ),
                        (
                            "relToleranceBits",
                            DecisionFactValue::Shared(Arc::from("0000000000000000")),
                        ),
                    ],
                ),
            ),
            (
                "return -5",
                Node::Un("-".to_string(), 0),
                Node::Num("-5".into()),
                OptimizationReason::decision_shared(
                    "constant-folding",
                    "literal-unary-evaluation",
                    [
                        ("operator", DecisionFactValue::Borrowed("-")),
                        ("operand", DecisionFactValue::Shared(Arc::from("i64:5"))),
                        ("evaluated", DecisionFactValue::Shared(Arc::from("i64:-5"))),
                        ("emitted", DecisionFactValue::Shared(Arc::from("i64:-5"))),
                        ("aggressive", DecisionFactValue::Shared(Arc::from("false"))),
                        (
                            "absToleranceBits",
                            DecisionFactValue::Shared(Arc::from("0000000000000000")),
                        ),
                        (
                            "relToleranceBits",
                            DecisionFactValue::Shared(Arc::from("0000000000000000")),
                        ),
                    ],
                ),
            ),
        ];
        for (source, previous, atom, decision) in cases {
            let (ast, _root) = parse_source_with_origins("fold-origin.lua", source).unwrap();
            let body = ast
                .nodes
                .iter()
                .enumerate()
                .find_map(|(i, node)| {
                    matches!(node, Node::Bin(..) | Node::Un(..)).then_some(i as NodeId)
                })
                .unwrap();
            // Old path: a root with a body-copy origin, rewritten by the
            // fold's derive + explain steps.
            let mut old = ast.clone();
            let root_t = old.push(previous.clone());
            old.nodes
                .derive_from_clone(root_t, &ast.nodes, body, "function-body-copy");
            let mut target = old.clone();
            target.nodes[root_t as usize] = atom.clone();
            target
                .nodes
                .derive_from_with_code(root_t, &old.nodes, root_t, &code);
            target.nodes.explain(root_t, decision.clone());
            // New path: the folded atom directly, with the single-shot helper.
            let mut new = ast.clone();
            let atom_t = new.push(atom.clone());
            assert!(new.nodes.record_folded_atom_origin(
                atom_t,
                &ast.nodes,
                body,
                &previous,
                &code,
                Some(decision)
            ));
            let old_slot = target
                .nodes
                .origins
                .as_ref()
                .unwrap()
                .slots
                .get(root_t as usize)
                .unwrap()
                .as_ref()
                .unwrap();
            let new_slot = new
                .nodes
                .origins
                .as_ref()
                .unwrap()
                .slots
                .get(atom_t as usize)
                .unwrap()
                .as_ref()
                .unwrap();
            assert_eq!(
                format!("{old_slot:?}"),
                format!("{new_slot:?}"),
                "for {source}"
            );
        }
    }

    #[test]
    fn relate_within_role_code_matches_relate_within_role() {
        let (ast, _root) = parse_source_with_origins("relate-code.lua", "local x = 1 + 2").unwrap();
        let mut nums = ast
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, node)| matches!(node, Node::Num(_)).then_some(i as NodeId));
        let first = nums.next().unwrap();
        let second = nums.next().unwrap();
        let code: Arc<str> = Arc::from("function-inline-site");
        let mut via_plain = ast.nodes.clone();
        via_plain.relate_within_role(
            first,
            second,
            "function-inline-site",
            RelationRole::CallSite,
        );
        let mut via_shared = ast.nodes.clone();
        via_shared.relate_within_role_code(first, second, &code, RelationRole::CallSite);
        assert_eq!(via_plain.origin(first), via_shared.origin(first));
        assert!(!via_shared.origin(first).unwrap().related.is_empty());
    }

    #[test]
    fn relate_from_role_code_matches_relate_from_role() {
        let (ast, _root) = parse_source_with_origins("relate-code.lua", "local x = 1 + 2").unwrap();
        let mut nums = ast
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, node)| matches!(node, Node::Num(_)).then_some(i as NodeId));
        let first = nums.next().unwrap();
        let second = nums.next().unwrap();
        let code: Arc<str> = Arc::from("function-inline-site");
        let mut via_plain = ast.nodes.clone();
        via_plain.relate_from_role(
            first,
            &ast.nodes,
            second,
            "function-inline-site",
            RelationRole::CallSite,
        );
        let mut via_shared = ast.nodes.clone();
        via_shared.relate_from_role_code(first, &ast.nodes, second, &code, RelationRole::CallSite);
        assert_eq!(via_plain.origin(first), via_shared.origin(first));
        assert!(!via_shared.origin(first).unwrap().related.is_empty());
    }

    #[test]
    fn relate_singleton_fast_path_shares_vectors_and_matches_push_path() {
        // Three number nodes in one source: relate two distinct targets
        // to the middle node. Both targets start with empty related
        // spans and relations, and the contributor has a single primary
        // span, so both must record the same singleton contents and
        // share the same interned vectors (pointer equality, not just
        // content equality). A later push onto one sharer must not
        // change the other (copy-on-write via Arc::make_mut).
        let (ast, _root) =
            parse_source_with_origins("relate-singleton.lua", "local x = 1 + 2 + 3").unwrap();
        let mut nums = ast
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, node)| matches!(node, Node::Num(_)).then_some(i as NodeId));
        let first = nums.next().unwrap();
        let second = nums.next().unwrap();
        let third = nums.next().unwrap();
        let mut arena = ast.nodes.clone();
        arena.relate_from_role(
            first,
            &ast.nodes,
            second,
            "immutable-carrier-definition",
            RelationRole::Definition,
        );
        arena.relate_from_role(
            third,
            &ast.nodes,
            second,
            "immutable-carrier-definition",
            RelationRole::Definition,
        );
        let first_origin = arena.origin(first).unwrap();
        let third_origin = arena.origin(third).unwrap();
        let second_origin = ast.nodes.origin(second).unwrap();
        assert_eq!(
            first_origin.related.as_slice(),
            &[second_origin.primary.unwrap()]
        );
        assert_eq!(
            third_origin.related.as_slice(),
            &[second_origin.primary.unwrap()]
        );
        assert_eq!(
            first_origin.relations.as_slice(),
            &[SourceRelation {
                role: RelationRole::Definition,
                span: second_origin.primary.unwrap(),
            }]
        );
        assert!(std::sync::Arc::ptr_eq(
            &first_origin.related,
            &third_origin.related
        ));
        assert!(std::sync::Arc::ptr_eq(
            &first_origin.relations,
            &third_origin.relations
        ));
        // Self-relation: the target already is the contributor span, so
        // related stays empty while the typed relation is still recorded.
        let mut self_arena = ast.nodes.clone();
        self_arena.relate_from_role(
            second,
            &ast.nodes,
            second,
            "immutable-carrier-definition",
            RelationRole::Definition,
        );
        let self_origin = self_arena.origin(second).unwrap();
        assert!(self_origin.related.is_empty());
        assert_eq!(
            self_origin.relations.as_slice(),
            &[SourceRelation {
                role: RelationRole::Definition,
                span: second_origin.primary.unwrap(),
            }]
        );
    }

    #[test]
    fn derive_from_with_code_matches_derive_from() {
        let (ast, _root) = parse_source_with_origins("fold.lua", "local x=1+2").unwrap();
        let num = ast
            .nodes
            .iter()
            .position(|node| matches!(node, Node::Num(_)))
            .unwrap() as NodeId;
        let code: Arc<str> = Arc::from("constant-folding");
        let mut via_plain = ast.nodes.clone();
        via_plain[num as usize] = Node::Nil;
        via_plain.derive_from(num, &ast.nodes, num, "constant-folding");
        let mut via_shared = ast.nodes.clone();
        via_shared[num as usize] = Node::Nil;
        via_shared.derive_from_with_code(num, &ast.nodes, num, &code);
        assert!(via_plain[num as usize] == Node::Nil);
        assert!(via_shared[num as usize] == Node::Nil);
        assert_eq!(via_plain.origin(num), via_shared.origin(num));
    }
    #[test]
    fn derive_from_with_code_matches_snapshot_finish_rewrite() {
        let (ast, _root) = parse_source_with_origins("fold.lua", "local x=1+2").unwrap();
        let bin = ast
            .nodes
            .iter()
            .position(|node| matches!(node, Node::Bin(..)))
            .unwrap() as NodeId;
        let code: Arc<str> = Arc::from("constant-folding");
        // Borrowed path: derive_from_with_code takes the previous value from
        // the source arena without cloning it into a snapshot.
        let mut via_borrowed = ast.nodes.clone();
        via_borrowed[bin as usize] = Node::Num(Arc::from("3"));
        via_borrowed.derive_from_with_code(bin, &ast.nodes, bin, &code);
        // Snapshot path: capture + finish_rewrite_with_code on a second clone.
        let mut via_snapshot = ast.nodes.clone();
        via_snapshot[bin as usize] = Node::Num(Arc::from("3"));
        let snapshot = ast.nodes.capture_origin(bin);
        via_snapshot.finish_rewrite_with_code(bin, snapshot, &code);
        assert_eq!(via_borrowed.origin(bin), via_snapshot.origin(bin));
    }

    #[test]
    fn node_origin_small_lists_keep_array_form_and_order() {
        use crate::explanation::{
            InlineContext, OptimizationReason, ReasonOperation, SourceDisposition,
        };
        use crate::provenance::{NodeOrigin, SourceSpan};
        let span = |start: usize, end: usize| SourceSpan {
            source: 0,
            start,
            end,
        };
        let mut origin = NodeOrigin::default();
        origin.contexts.push(InlineContext {
            definition: span(0, 1),
            call_site: span(2, 3),
        });
        origin.dispositions.push(SourceDisposition {
            original: span(0, 1),
            reason: OptimizationReason::action("test-rule", ReasonOperation::Remove),
            replacement_sources: Vec::new(),
        });
        // Single entries serialize exactly like the previous one-element arrays.
        assert_eq!(
            serde_json::to_value(&origin.contexts)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            serde_json::to_value(&origin.dispositions)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        // Spilled lists keep insertion order and round-trip through NodeOrigin.
        origin.contexts.push(InlineContext {
            definition: span(4, 5),
            call_site: span(6, 7),
        });
        origin.dispositions.push(SourceDisposition {
            original: span(4, 5),
            reason: OptimizationReason::action("test-rule", ReasonOperation::Remove),
            replacement_sources: vec![span(8, 9)],
        });
        assert_eq!(origin.contexts.len(), 2);
        assert_eq!(origin.dispositions.len(), 2);
        let back: NodeOrigin =
            serde_json::from_value(serde_json::to_value(&origin).unwrap()).unwrap();
        assert_eq!(back, origin);
    }

    #[test]
    fn derive_from_borrowed_with_code_matches_derive_from_with_code() {
        let (ast, _root) = parse_source_with_origins("borrowed.lua", "local x=1+2").unwrap();
        let bin = ast
            .nodes
            .iter()
            .position(|node| matches!(node, Node::Bin(..)))
            .unwrap() as NodeId;
        let code: Arc<str> = Arc::from("immutable-carrier-use");
        let mut via_arena = ast.nodes.clone();
        via_arena[bin as usize] = Node::Nil;
        via_arena.derive_from_with_code(bin, &ast.nodes, bin, &code);
        let mut via_borrowed = ast.nodes.clone();
        let previous = ast.node(bin).clone();
        let borrowed = ast.nodes.capture_origin_ref(bin).unwrap();
        via_borrowed[bin as usize] = Node::Nil;
        via_borrowed.derive_from_borrowed_with_code(bin, &previous, &borrowed, &code);
        assert_eq!(via_arena.origin(bin), via_borrowed.origin(bin));
    }
    #[test]
    fn relate_from_borrowed_role_code_matches_relate_from_role_code() {
        let (ast, _root) =
            parse_source_with_origins("relate-borrowed.lua", "local x = 1 + 2").unwrap();
        let mut nums = ast
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, node)| matches!(node, Node::Num(_)).then_some(i as NodeId));
        let first = nums.next().unwrap();
        let second = nums.next().unwrap();
        let code: Arc<str> = Arc::from("function-inline-site");
        let mut via_arena = ast.nodes.clone();
        via_arena.relate_from_role_code(first, &ast.nodes, second, &code, RelationRole::CallSite);
        let mut via_borrowed = ast.nodes.clone();
        let borrowed = ast.nodes.capture_origin_ref(second).unwrap();
        via_borrowed.relate_from_borrowed_role_code(
            first,
            Some(&borrowed),
            &code,
            RelationRole::CallSite,
        );
        assert_eq!(via_arena.origin(first), via_borrowed.origin(first));
        let mut via_none = ast.nodes.clone();
        via_none.relate_from_borrowed_role_code(first, None, &code, RelationRole::CallSite);
        assert_eq!(ast.nodes.origin(first), via_none.origin(first));
    }
    #[test]
    fn derive_from_clone_self_matches_derive_from_clone() {
        let (ast, _root) = parse_source_with_origins("clone-self.lua", "local x = 1 + 2").unwrap();
        let bin = ast
            .nodes
            .iter()
            .position(|node| matches!(node, Node::Bin(..)))
            .unwrap() as NodeId;
        let value = ast.node(bin).clone();
        let mut via_arena = ast.nodes.clone();
        let copied = via_arena.len() as NodeId;
        via_arena.push(value.clone());
        via_arena.derive_from_clone(copied, &ast.nodes, bin, "immutable-carrier-copy");
        let mut via_self = ast.nodes.clone();
        let copied_self = via_self.len() as NodeId;
        via_self.push(value);
        via_self.derive_from_clone_self(copied_self, bin, "immutable-carrier-copy");
        assert_eq!(via_arena.origin(copied), via_self.origin(copied_self));
    }
}
