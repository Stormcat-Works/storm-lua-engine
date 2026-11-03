//! Straight-line member-read caching (target-size layouts only).
//!
//! A repeated value read `B.K` in straight-line code pays one table
//! lookup per occurrence. When the same `(base, key)` pair is read
//! at least twice inside one straight-line segment, caching the
//! value once (`local c = B.K`, then `c` at each site) removes one
//! lookup per extra occurrence. This is the B group of the cycle-69
//! census: non-loop-block straight-line repeated member reads
//! (851 groups / 339 files, 1,247 surplus reads on winner code)
//! that neither `loop_invariant` (host calls and arithmetic only),
//! nor `available_expressions` (non-builtin index reads are
//! barriers there), nor `arithmetic_cse` (binary/unary only), nor
//! `loop_member_reads` (loop bodies only) absorbs.
//!
//! This transform is wired only into target-size layouts
//! (`joint-api-local-smc` and `joint-api-local-licm-smc` in
//! `search.rs`), never into target-free compilation, and it is a
//! layout-construction transform in the LICM/S1/MR sense, not a
//! phase pass: it is deliberately not registered in the canonical
//! pass registry, so it adds no `effectivePassToggles` entry and no
//! map churn for files whose winner does not use it. The target
//! budget is the profitability gate: a layout that exceeds the
//! target loses to every fitting candidate through the existing
//! over-target demotion in `select_best_candidates`, and the speed
//! rank orders the SMC layouts above their base layouts but below
//! the MR layouts (so an SMC layout can only win when no
//! faster-fitting layout exists, and MR -- whose saving scales
//! with trip count -- is preferred when both fit). Composite
//! layouts (SMC on top of an S1 or MR layout, see search.rs)
//! rank above both of their components, since they combine
//! two disjoint savings. Partial composites (full S1 or MR with
//! the first-group-only partial form of this transform) rank
//! directly below their full composite.
//!
//! The transform is intentionally conservative:
//! - Only a single-level value read qualifies: the object must be a
//!   bare `Name` and the key a dot/string literal or a numeric
//!   literal. Reads used as a call callee never occur here: a
//!   segment contains no calls at all. Reads in loop headers are
//!   never collected, because headers belong to no segment.
//! - A segment is a maximal run of `local` / assignment statements
//!   in one block, plus a trailing call-free `return` list. Any
//!   other statement kind ends the segment: control flow, call
//!   statements, and statements whose right-hand sides contain a
//!   call (a call could mutate the base through a captured
//!   closure). An assignment with an index target does not end the
//!   segment: it stays as a barrier statement whose value reads
//!   belong to the pre-write side (Lua evaluates the values before
//!   the writes take effect), and which then closes only the groups
//!   it can affect -- exactly the (base, key) it writes, every
//!   group of that base for a dynamic key, and every group for a
//!   write through a non-Name object (which may alias any base).
//!   Groups are formed per segment only, never across a segment
//!   boundary, and never across a block boundary: every block is
//!   processed on its own, and reads inside nested function bodies
//!   belong to that function's blocks, not to the enclosing
//!   segment.
//! - The base proof is exactly `loop_member_reads`' proof, reused
//!   unchanged: a `local` with write count zero whose declaration
//!   initializer is a table constructor, or a global with exactly
//!   one table-constructor defining assignment (chunk-top form, or
//!   the same-block form with the definition preceding the
//!   segment's first statement and no call or table-field write in
//!   between), in a goto/label-free enclosing function, and never
//!   escaping anywhere in the program (every `Name` resolving to
//!   it must be the object of an `Index` node). The straight-line
//!   setting makes this proof sufficient with no additional
//!   barrier scan: between two occurrences of one group, only the
//!   segment's own statements execute, and those statements contain
//!   no calls and (by the write counts) no write to the base, and a
//!   group's sub-segment contains no write that can affect its
//!   field (same-key writes split the group, other writes are
//!   confined to other groups or, for unresolvable objects, split
//!   every group), so the field's value cannot change between a
//!   sub-group's cache definition and its reads. The base is
//!   a plain table at the segment, so the cache definition
//!   (`local c = B.K`) is total and side-effect-free, and moving
//!   the first read's evaluation to just before its statement
//!   cannot change any observable behaviour.
//! - The cache local's name is the shortest single-letter
//!   identifier absent from the program's string table, mirroring
//!   S1/MR: a name absent from the table cannot be declared or
//!   referenced by user code anywhere, so the inserted local can
//!   shadow nothing.
//!
//! A first-group-only partial variant exists as
//! `cache_straightline_member_reads_partial`: when the all-groups
//! layout exceeds the target budget, the single-group layout (about
//! ten characters over its base layout) may still fit, so it is
//! offered as a lower-speed-rank layout between the full SMC layout
//! and its base layout. The subset is deliberately the
//! traversal-first group only; ordering groups by expected benefit
//! would need execution-frequency data that is not available
//! statically, and is left as future work.
//!
//! Deliberately out of scope: deeper base chains (`a.b.c` first
//! levels are still cached, since the inner `a.b` read itself
//! qualifies), bracket keys with dynamic expressions,
//! non-constructor initializers, parameters and local functions as
//! bases, and groups split by a call (each side is then a separate
//! segment and may still cache on its own).

use std::collections::HashMap;

use crate::pass::PassResult;
use crate::passes::loop_member_reads::{
    base_proven, key_text, name_bid, subtree_has_goto_label, LoopPos,
};
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId};
use storm_lua_syntax::ast_utils;

const REASON: &str = "straightline-member-read-caching";

/// Whether a subtree (nested functions excluded: their bodies do
/// not run at this point) contains a call.
fn subtree_has_call(source: &Ast, node: NodeId) -> bool {
    let mut found = false;
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if found {
            break;
        }
        if current != node && matches!(source.node(current), Node::Function(..)) {
            continue;
        }
        if matches!(source.node(current), Node::Call(..)) {
            found = true;
            break;
        }
        let mut children = Vec::new();
        ast_utils::for_each_child(source, current, &mut |child| children.push(child));
        for child in children {
            stack.push(child);
        }
    }
    found
}

/// Whether a statement may belong to a segment: a `local` or an
/// assignment with name-only targets, with no call in its subtree
/// (nested functions excluded) and no index/method target.
fn segment_eligible(source: &Ast, statement: NodeId) -> bool {
    match source.node(statement) {
        Node::Local(..) => !subtree_has_call(source, statement),
        Node::Assign(targets, _) => {
            if targets
                .iter()
                .any(|t| matches!(source.node(*t), Node::Index(..) | Node::Methodname(..)))
            {
                return false;
            }
            !subtree_has_call(source, statement)
        }
        _ => false,
    }
}

/// Whether a `return` statement may close a segment: its list must
/// contain no call (nested functions excluded).
fn return_eligible(source: &Ast, statement: NodeId) -> bool {
    matches!(source.node(statement), Node::Return(_)) && !subtree_has_call(source, statement)
}

/// An index-write barrier carried by one statement: which groups
/// it closes, applied after the statement's own value reads (which
/// execute before the writes take effect).
enum Barrier {
    /// Write to one (base, key): closes only that group.
    Specific(BindingId, String),
    /// Write through one base with a dynamic key: closes every
    /// group of that base.
    BaseWide(BindingId),
    /// Write through a non-Name object, or through a Name the
    /// resolver cannot bind: it may alias any base, so it closes
    /// every group.
    Unknown,
}

/// Index-write barriers of an assignment statement, when the
/// statement may stay inside a segment as a barrier statement: no
/// call in its subtree (nested functions excluded) and no method
/// target. `None` means the statement is not a barrier statement
/// (it ends the segment instead).
fn index_write_barriers(
    source: &Ast,
    resolution: &Resolution,
    statement: NodeId,
) -> Option<Vec<Barrier>> {
    let Node::Assign(targets, _) = source.node(statement) else {
        return None;
    };
    if targets
        .iter()
        .any(|t| matches!(source.node(*t), Node::Methodname(..)))
    {
        return None;
    }
    if subtree_has_call(source, statement) {
        return None;
    }
    let mut barriers = Vec::new();
    let mut any_index = false;
    for target in targets {
        let Node::Index(obj, key, _) = source.node(*target) else {
            continue;
        };
        any_index = true;
        if matches!(source.node(*obj), Node::Name(_)) {
            if let Some(bid) = name_bid(resolution, *obj) {
                if let Some(key) = key_text(source, *key) {
                    barriers.push(Barrier::Specific(bid, key));
                } else {
                    barriers.push(Barrier::BaseWide(bid));
                }
            } else {
                barriers.push(Barrier::Unknown);
            }
        } else {
            barriers.push(Barrier::Unknown);
        }
    }
    if any_index {
        Some(barriers)
    } else {
        None
    }
}

/// Collect value-position member reads in a segment statement's
/// subtree (nested function bodies excluded), in scan order. A
/// read qualifies when its object is a bare `Name` and its key is a
/// literal. Normal segment statements contain no calls and no index
/// targets, so no callee/target special-casing is needed here;
/// barrier statements collect their values only (see
/// `collect_assign_value_occurrences`).
fn collect_occurrences(
    source: &Ast,
    resolution: &Resolution,
    node: NodeId,
    out: &mut Vec<(BindingId, String, NodeId)>,
) {
    if matches!(source.node(node), Node::Function(..)) {
        return;
    }
    if let Node::Index(obj, key, _) = source.node(node) {
        if matches!(source.node(*obj), Node::Name(_)) {
            if let (Some(bid), Some(key)) = (name_bid(resolution, *obj), key_text(source, *key)) {
                out.push((bid, key, node));
            }
        }
    }
    let mut children = Vec::new();
    ast_utils::for_each_child(source, node, &mut |child| children.push(child));
    for child in children {
        collect_occurrences(source, resolution, child, out);
    }
}

/// Collect value-position member reads in an assignment
/// statement's values only: targets are writes, not reads, and
/// must not count as occurrences of their group.
fn collect_assign_value_occurrences(
    source: &Ast,
    resolution: &Resolution,
    statement: NodeId,
    out: &mut Vec<(BindingId, String, NodeId)>,
) {
    let Node::Assign(_, values) = source.node(statement) else {
        return;
    };
    for value in values {
        collect_occurrences(source, resolution, *value, out);
    }
}

/// Clone a subtree from `source` into `output` with fresh node ids.
fn clone_subtree(output: &mut Ast, source: &Ast, node: NodeId) -> NodeId {
    let (mapped, _) = ast_utils::map_children(source.node(node), &mut |child| {
        clone_subtree(output, source, child)
    });
    output.push(mapped)
}

fn clone_source(source: &Ast) -> Ast {
    let mut ast = ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

/// Allocate the cache local's name: the shortest single-letter
/// identifier absent from the program's string table, mirroring S1/MR.
fn fresh_cache_symbol(output: &mut Ast) -> SymbolId {
    for candidate in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ".chars() {
        let name = candidate.to_string();
        if !output.strings.contains(&name) {
            return output.strings.intern(&name);
        }
    }
    output.strings.fresh("__slmembercache")
}

/// One item of a segment: a normal statement, or a barrier
/// statement carrying the groups its index writes close.
enum SegItem {
    Stmt(NodeId),
    Barrier(NodeId, Vec<Barrier>),
}

struct Transformer<'a> {
    source: &'a Ast,
    resolution: &'a Resolution,
    root: NodeId,
    output: Ast,
    cached_groups: usize,
    replaced: usize,
    /// Enclosing function bodies on the current transform path
    /// (innermost last); empty at the chunk top level.
    fn_stack: Vec<NodeId>,
    fn_params: Vec<usize>,
    fn_local_max: HashMap<NodeId, usize>,
    fn_inserted: HashMap<NodeId, usize>,
    /// Base proofs, keyed by (binding, enclosing-function goto flag,
    /// segment block, segment first statement): the global
    /// same-block form depends on the segment's position.
    proof_cache: HashMap<(BindingId, bool, NodeId, NodeId), bool>,
    /// Total groups cached (`None` = all groups): the partial
    /// variant caps this at one group.
    group_limit: Option<usize>,
}

impl<'a> Transformer<'a> {
    fn new(
        source: &'a Ast,
        root: NodeId,
        resolution: &'a Resolution,
        group_limit: Option<usize>,
    ) -> Self {
        Self {
            source,
            resolution,
            root,
            output: clone_source(source),
            cached_groups: 0,
            replaced: 0,
            fn_stack: Vec::new(),
            fn_params: Vec::new(),
            fn_local_max: HashMap::new(),
            fn_inserted: HashMap::new(),
            proof_cache: HashMap::new(),
            group_limit,
        }
    }

    fn proven(&mut self, bid: BindingId, fn_has_goto: bool, pos: LoopPos) -> bool {
        let key = (bid, fn_has_goto, pos.block, pos.stmt);
        if let Some(proven) = self.proof_cache.get(&key) {
            return *proven;
        }
        let proven = base_proven(
            self.source,
            self.resolution,
            self.root,
            bid,
            fn_has_goto,
            pos,
        );
        self.proof_cache.insert(key, proven);
        proven
    }

    /// Build a cache local for one group (`local c = <first read>`)
    /// and rewrite every occurrence to the cache name. Returns the
    /// cache local node; the caller places it before the group's
    /// first-occurrence statement. Callers pass at least two
    /// occurrences.
    fn make_cache(&mut self, occurrences: &[NodeId]) -> Option<NodeId> {
        let template = occurrences.first().copied()?;
        let symbol = fresh_cache_symbol(&mut self.output);
        let cloned = clone_subtree(&mut self.output, self.source, template);
        let mut cloned_nodes = Vec::new();
        ast_utils::walk(&self.output, cloned, &mut |id| cloned_nodes.push(id));
        for id in cloned_nodes {
            self.output.nodes.mark_synthetic(id, REASON);
        }
        let local = self.output.push(Node::Local(vec![symbol], vec![cloned]));
        self.output.nodes.mark_synthetic(local, REASON);
        for occurrence in occurrences {
            self.output
                .nodes
                .rewrite(*occurrence, Node::Name(symbol), REASON);
            super::origins::derive(
                &mut self.output,
                *occurrence,
                self.source,
                &[*occurrence],
                REASON,
            );
        }
        self.cached_groups += 1;
        self.replaced += occurrences.len();
        Some(local)
    }

    /// Move one group's open sub-group to the closed list, when it
    /// holds at least one occurrence.
    fn close_group(
        current: &mut HashMap<(BindingId, String), Vec<NodeId>>,
        current_first: &mut HashMap<(BindingId, String), NodeId>,
        closed: &mut Vec<((BindingId, String), Vec<NodeId>, NodeId)>,
        group: &(BindingId, String),
    ) {
        if let Some(list) = current.remove(group) {
            if !list.is_empty() {
                if let Some(first) = current_first.remove(group) {
                    closed.push((group.clone(), list, first));
                }
            }
        }
    }

    /// Decide the groups of one segment and record their cache
    /// locals, keyed by the statement before which each local is
    /// inserted (the sub-group's first-occurrence statement).
    /// Barrier statements split groups per barrier, not the whole
    /// segment: a group's occurrences between two barriers form
    /// one sub-group, cached on its own when it holds at least two
    /// occurrences.
    fn process_segment(
        &mut self,
        block: NodeId,
        fn_has_goto: bool,
        segment: &[SegItem],
        insertions: &mut HashMap<NodeId, Vec<NodeId>>,
    ) {
        if segment.is_empty() {
            return;
        }
        // The proof position stays the segment's first statement,
        // exactly as before: the base proof is about the base being
        // a plain table when the segment starts, and per-group
        // barriers separately guarantee that no write affecting a
        // sub-group's field intervenes between its cache definition
        // and its reads.
        let proof_stmt = match segment.first() {
            Some(SegItem::Stmt(statement)) | Some(SegItem::Barrier(statement, _)) => *statement,
            None => return,
        };
        let mut closed: Vec<((BindingId, String), Vec<NodeId>, NodeId)> = Vec::new();
        let mut current: HashMap<(BindingId, String), Vec<NodeId>> = HashMap::new();
        let mut current_first: HashMap<(BindingId, String), NodeId> = HashMap::new();
        // First-appearance order within this segment: multi-group
        // closes and the final flush follow it, so the order of
        // closed sub-groups (and hence which group the partial
        // variant picks) is deterministic, not HashMap order.
        let mut order: Vec<(BindingId, String)> = Vec::new();
        for item in segment {
            let (statement, barriers) = match item {
                SegItem::Stmt(statement) => (*statement, None),
                SegItem::Barrier(statement, barriers) => (*statement, Some(barriers)),
            };
            let mut occurrences = Vec::new();
            if barriers.is_some() {
                collect_assign_value_occurrences(
                    self.source,
                    self.resolution,
                    statement,
                    &mut occurrences,
                );
            } else {
                collect_occurrences(self.source, self.resolution, statement, &mut occurrences);
            }
            for (bid, key, node) in occurrences {
                let group = (bid, key);
                if !order.contains(&group) {
                    order.push(group.clone());
                }
                if !current.contains_key(&group) {
                    current.insert(group.clone(), Vec::new());
                    current_first.insert(group.clone(), statement);
                }
                if let Some(list) = current.get_mut(&group) {
                    list.push(node);
                }
            }
            if let Some(barriers) = barriers {
                for barrier in barriers {
                    match barrier {
                        Barrier::Specific(bid, key) => {
                            let group = (*bid, key.clone());
                            Self::close_group(
                                &mut current,
                                &mut current_first,
                                &mut closed,
                                &group,
                            );
                        }
                        Barrier::BaseWide(bid) => {
                            let groups: Vec<(BindingId, String)> = order
                                .iter()
                                .filter(|group| group.0 == *bid && current.contains_key(*group))
                                .cloned()
                                .collect();
                            for group in groups {
                                Self::close_group(
                                    &mut current,
                                    &mut current_first,
                                    &mut closed,
                                    &group,
                                );
                            }
                        }
                        Barrier::Unknown => {
                            let groups: Vec<(BindingId, String)> = order
                                .iter()
                                .filter(|group| current.contains_key(*group))
                                .cloned()
                                .collect();
                            for group in groups {
                                Self::close_group(
                                    &mut current,
                                    &mut current_first,
                                    &mut closed,
                                    &group,
                                );
                            }
                        }
                    }
                }
            }
        }
        let groups: Vec<(BindingId, String)> = order
            .iter()
            .filter(|group| current.contains_key(*group))
            .cloned()
            .collect();
        for group in groups {
            Self::close_group(&mut current, &mut current_first, &mut closed, &group);
        }
        // Cycle 94 (Codex finding 7, SMC form): per-function
        // local budget for cache insertions, mirroring the S1/MR
        // checks. The w6 cycle-47 gg2 probe's slack32 winner is an
        // SMC layout whose body cache local is the 201st local.
        let fn_body = self.fn_stack.last().copied().unwrap_or(self.root);
        let fn_params = self.fn_params.last().copied().unwrap_or(0);
        let source = self.source;
        let local_max = *self.fn_local_max.entry(fn_body).or_insert_with(|| {
            super::single_member_cache::max_active_locals(source, fn_params, fn_body)
        });
        let mut inserted = self.fn_inserted.get(&fn_body).copied().unwrap_or(0);
        for (group, occurrences, first) in closed {
            if occurrences.len() < 2 {
                continue;
            }
            if local_max + inserted + 1 > super::single_member_cache::MAX_LOCALS {
                continue;
            }
            if self.proven(
                group.0,
                fn_has_goto,
                LoopPos {
                    block,
                    stmt: proof_stmt,
                },
            ) {
                if let Some(limit) = self.group_limit {
                    if self.cached_groups >= limit {
                        continue;
                    }
                }
                if let Some(local) = self.make_cache(&occurrences) {
                    insertions.entry(first).or_default().push(local);
                    inserted += 1;
                }
            }
        }
        self.fn_inserted.insert(fn_body, inserted);
    }

    /// Split one block's statements into segments, decide their
    /// groups, and return the cache locals to insert before each
    /// first-occurrence statement.
    fn process_block_segments(
        &mut self,
        block: NodeId,
        statements: &[NodeId],
    ) -> HashMap<NodeId, Vec<NodeId>> {
        let fn_body = self.fn_stack.last().copied().unwrap_or(self.root);
        let fn_has_goto = subtree_has_goto_label(self.source, fn_body);
        let mut insertions: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        let mut segment: Vec<SegItem> = Vec::new();
        for statement in statements {
            if segment_eligible(self.source, *statement) {
                segment.push(SegItem::Stmt(*statement));
            } else if let Some(barriers) =
                index_write_barriers(self.source, self.resolution, *statement)
            {
                segment.push(SegItem::Barrier(*statement, barriers));
            } else if return_eligible(self.source, *statement) {
                segment.push(SegItem::Stmt(*statement));
                self.process_segment(block, fn_has_goto, &segment, &mut insertions);
                segment.clear();
            } else {
                self.process_segment(block, fn_has_goto, &segment, &mut insertions);
                segment.clear();
            }
        }
        self.process_segment(block, fn_has_goto, &segment, &mut insertions);
        insertions
    }

    fn transform_block(&mut self, block: NodeId) {
        let Node::Block(statements) = self.source.node(block).clone() else {
            return;
        };
        let insertions = self.process_block_segments(block, &statements);
        let mut rewritten = Vec::with_capacity(statements.len());
        for statement in &statements {
            if let Some(locals) = insertions.get(statement) {
                for local in locals {
                    rewritten.push(*local);
                }
            }
            self.transform_statement(*statement);
            rewritten.push(*statement);
        }
        if rewritten != statements {
            self.output
                .nodes
                .rewrite(block, Node::Block(rewritten), REASON);
        }
    }

    fn transform_statement(&mut self, statement: NodeId) {
        match self.source.node(statement).clone() {
            Node::Block(_) => {
                self.transform_block(statement);
            }
            Node::Do(body) => {
                self.transform_block(body);
            }
            Node::If(arms, otherwise) => {
                for arm in arms {
                    self.transform_expr(arm.cond);
                    self.transform_block(arm.body);
                }
                if let Some(body) = otherwise {
                    self.transform_block(body);
                }
            }
            Node::While(condition, body) => {
                self.transform_expr(condition);
                self.transform_block(body);
            }
            Node::Repeat(body, condition) => {
                self.transform_block(body);
                self.transform_expr(condition);
            }
            Node::Fornum(_, start, end, step, body) => {
                self.transform_expr(start);
                self.transform_expr(end);
                if let Some(step) = step {
                    self.transform_expr(step);
                }
                self.transform_block(body);
            }
            Node::Forin(_, expressions, body) => {
                for expression in expressions {
                    self.transform_expr(expression);
                }
                self.transform_block(body);
            }
            Node::Funcstat(target, function) => {
                self.transform_expr(target);
                if let Node::Function(params, _, body) = self.source.node(function).clone() {
                    // A method definition's implicit `self` is a real
                    // parameter at load time; count it in the budget
                    // context (the Function node cannot see the
                    // Funcstat target). Same accounting as Round 218
                    // (LICM / carrier synthesis); found by the w6
                    // cycle-52 completion audit (a1/a7/a10 shapes).
                    let extra =
                        usize::from(matches!(self.source.node(target), Node::Methodname(..)));
                    self.fn_stack.push(body);
                    self.fn_params.push(params.len() + extra);
                    self.transform_block(body);
                    self.fn_params.pop();
                    self.fn_stack.pop();
                } else {
                    self.transform_expr(function);
                }
            }
            Node::Localfunc(_, function) => {
                self.transform_expr(function);
            }
            Node::Local(_, expressions) | Node::Return(expressions) => {
                for expression in expressions {
                    self.transform_expr(expression);
                }
            }
            Node::Callstat(expression) => {
                self.transform_expr(expression);
            }
            Node::Assign(targets, expressions) => {
                for target in targets {
                    self.transform_expr(target);
                }
                for expression in expressions {
                    self.transform_expr(expression);
                }
            }
            _ => {}
        }
    }

    fn transform_expr(&mut self, node: NodeId) {
        match self.source.node(node).clone() {
            Node::Function(params, _, body) => {
                self.fn_stack.push(body);
                self.fn_params.push(params.len());
                self.transform_block(body);
                self.fn_stack.pop();
                self.fn_params.pop();
            }
            Node::Block(_) => self.transform_block(node),
            _ => {
                let mut children = Vec::new();
                ast_utils::for_each_child(self.source, node, &mut |child| children.push(child));
                for child in children {
                    self.transform_expr(child);
                }
            }
        }
    }
}

/// Shared implementation for the post-alias entry point. Returns
/// `cached=N` (groups) and `replaced=M` (read sites) in `details`
/// (0 when nothing changed, in which case `ast` is left untouched).
fn run_cached(
    ast: &mut Ast,
    root: NodeId,
    group_limit: Option<usize>,
    shared_resolution: Option<&Resolution>,
) -> PassResult {
    if !matches!(ast.node(root), Node::Block(_)) {
        return PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["cached=0".to_string()]),
        };
    }
    let source = clone_source(ast);
    let resolution;
    let resolution = match shared_resolution {
        Some(shared) => shared,
        None => {
            resolution = resolve(&source, root);
            &resolution
        }
    };
    let mut transformer = Transformer::new(&source, root, resolution, group_limit);
    transformer.transform_block(root);
    let cached_groups = transformer.cached_groups;
    let replaced = transformer.replaced;
    if cached_groups == 0 {
        return PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["cached=0".to_string()]),
        };
    }
    *ast = transformer.output;
    PassResult {
        root,
        saved: None,
        details: Some(vec![
            format!("cached={cached_groups}"),
            format!("replaced={replaced}"),
        ]),
    }
}

/// Cache repeated straight-line member reads, post-alias form: runs
/// on an api-aliased layout, where base names are the layout's own
/// locals (see module docs).
pub fn cache_straightline_member_reads(ast: &mut Ast, root: NodeId) -> PassResult {
    run_cached(ast, root, None, None)
}

/// Form over a caller-built shared resolution (see
/// `passes::speed_analysis`): `resolution` must have been built
/// over an AST whose node and string arenas are identical to
/// `ast`'s (a verbatim clone qualifies). The result is identical
/// to [`cache_straightline_member_reads`]; only the repeated
/// `resolve` is saved.
pub fn cache_straightline_member_reads_with_resolution(
    ast: &mut Ast,
    root: NodeId,
    resolution: &Resolution,
) -> PassResult {
    run_cached(ast, root, None, Some(resolution))
}

/// Partial form over a caller-built shared resolution; see
/// [`cache_straightline_member_reads_with_resolution`].
pub fn cache_straightline_member_reads_partial_with_resolution(
    ast: &mut Ast,
    root: NodeId,
    resolution: &Resolution,
) -> PassResult {
    run_cached(ast, root, Some(1), Some(resolution))
}

/// Cache only the traversal-first qualifying group, S1-partial
/// style: the partial layout for when the all-groups layout exceeds
/// the target budget.
pub fn cache_straightline_member_reads_partial(ast: &mut Ast, root: NodeId) -> PassResult {
    run_cached(ast, root, Some(1), None)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn run(source: &str) -> (String, usize) {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = cache_straightline_member_reads(&mut ast, root);
        let code = Printer::new(&ast, false).output(result.root);
        let cached = result
            .details
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.strip_prefix("cached="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        (code, cached)
    }

    fn run_partial(source: &str) -> (String, usize) {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = cache_straightline_member_reads_partial(&mut ast, root);
        let code = Printer::new(&ast, false).output(result.root);
        let cached = result
            .details
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.strip_prefix("cached="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        (code, cached)
    }

    fn run_shared(source: &str, partial: bool) -> (String, usize) {
        let (ast, root) = parse_source(source).expect("parse");
        let resolution = resolve(&ast, root);
        let mut shared_ast = ast.clone();
        let result = if partial {
            cache_straightline_member_reads_partial_with_resolution(
                &mut shared_ast,
                root,
                &resolution,
            )
        } else {
            cache_straightline_member_reads_with_resolution(&mut shared_ast, root, &resolution)
        };
        let code = Printer::new(&shared_ast, false).output(result.root);
        let cached = result
            .details
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.strip_prefix("cached="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        (code, cached)
    }

    #[test]
    fn shared_resolution_matches_standalone() {
        // See the S1 `shared_analysis_matches_standalone` test: the
        // shared resolution is a pure function of the arena
        // contents, so results must match the standalone entries.
        let firing = "function onTick() local t={x=1} local a=t.x local b=t.x end";
        let standalone = run(firing);
        assert!(standalone.1 > 0, "input must fire SMC: {standalone:?}");
        assert_eq!(run_shared(firing, false), standalone);
        assert_eq!(run_shared(firing, true), run_partial(firing));
        let quiet = "function onTick() local x=1 end";
        assert_eq!(run_shared(quiet, false), run(quiet));
        assert_eq!(run_shared(quiet, true), run_partial(quiet));
    }

    fn many_param_smc_source(n: usize) -> String {
        let params: Vec<String> = (1..=n).map(|i| format!("p{i}")).collect();
        format!(
            "function onTick({}) local t={{x=1}} local a=t.x local b=t.x return a+b end",
            params.join(",")
        )
    }

    #[test]
    fn skips_cache_when_locals_would_exceed_limit() {
        // Cycle 94 (Codex finding 7, SMC form): 197 parameters
        // plus the table and the two read locals already reach
        // 200 active locals, so the cache local would make the
        // generated code unloadable. SMC must skip the group.
        let (_, cached) = run(&many_param_smc_source(197));
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_when_locals_stay_at_limit() {
        // Boundary control: 196 parameters peak at 199 active
        // locals, leaving room for exactly one cache local.
        let (code, cached) = run(&many_param_smc_source(196));
        assert_eq!(cached, 1, "code: {code}");
    }

    #[test]
    fn skips_cache_in_method_at_limit_counting_self() {
        // Cycle 95 (w6 cycle-52 audit, a10 shape): 196 explicit
        // parameters plus the implicit `self` and the three body
        // locals already reach 200 active locals; the cache local
        // would be the 201st. The pre-fix accounting missed
        // `self` (199) and fired.
        let params: Vec<String> = (1..=196).map(|i| format!("p{i}")).collect();
        let source = format!(
            "local obj = {{}} function obj:m({}) local t={{x=1}} local a=t.x local b=t.x return a+b end",
            params.join(",")
        );
        let (_, cached) = run(&source);
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_in_method_one_below_limit_counting_self() {
        // Boundary control: 195 explicit parameters plus `self`
        // and the three body locals peak at 199, leaving room for
        // exactly one cache local.
        let params: Vec<String> = (1..=195).map(|i| format!("p{i}")).collect();
        let source = format!(
            "local obj = {{}} function obj:m({}) local t={{x=1}} local a=t.x local b=t.x return a+b end",
            params.join(",")
        );
        let (code, cached) = run(&source);
        assert_eq!(cached, 1, "code: {code}");
    }

    #[test]
    fn skips_cache_in_loop_body_at_local_limit() {
        // The w6 cycle-47 gg2 probe shape (Codex finding 7): the
        // repeated reads sit in a numeric-for body; 194 parameters
        // plus two locals and the for controls already reach 200
        // active locals. The 193-parameter control still caches.
        let build = |n: usize| {
            let params: Vec<String> = (1..=n).map(|i| format!("p{i}")).collect();
            format!(
                "function onTick({}) local t={{x=1}} local s=0 for i=1,3 do s=s+t.x+t.x end return s end",
                params.join(",")
            )
        };
        let (_, cached) = run(&build(194));
        assert_eq!(cached, 0);
        let (code, cached) = run(&build(193));
        assert_eq!(cached, 1, "code: {code}");
    }

    #[test]
    fn caches_repeated_member_reads_in_straightline_block() {
        let (code, cached) = run("function onTick() local t={x=1} local a=t.x local b=t.x end");
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t.x").count(), 1, "code: {code}");
        assert!(code.contains("local c=t.x"), "code: {code}");
    }

    #[test]
    fn caches_repeated_reads_spanning_many_statements() {
        let (code, cached) = run(
            "function onTick() local t={x=1,y=2} local a=t.x local q=t.y local b=t.x local r=t.y end",
        );
        assert_eq!(cached, 2, "code: {code}");
        assert_eq!(code.matches("t.x").count(), 1, "code: {code}");
        assert_eq!(code.matches("t.y").count(), 1, "code: {code}");
    }

    #[test]
    fn caches_reads_in_trailing_return_list() {
        let (code, cached) = run("function f() local t={x=1} local a=t.x return t.x end");
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t.x").count(), 1, "code: {code}");
    }

    #[test]
    fn skips_single_occurrence() {
        let (_, cached) = run("function onTick() local t={x=1} local a=t.x local b=2 end");
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_when_base_reassigned() {
        let (_, cached) =
            run("function onTick() local t={x=1} t={x=2} local a=t.x local b=t.x end");
        assert_eq!(cached, 0);
    }

    #[test]
    fn skips_when_base_escapes() {
        let (_, cached) =
            run("function onTick() local t={x=1} local u=t local a=t.x local b=t.x end");
        assert_eq!(cached, 0);
    }

    #[test]
    fn call_between_reads_splits_the_segment() {
        let (_, cached) = run("function onTick() local t={x=1} local a=t.x foo() local b=t.x end");
        assert_eq!(cached, 0);
    }

    #[test]
    fn index_write_between_reads_splits_the_segment() {
        // The write target itself is not counted as a read: if it
        // were, the pre-write side would hold two occurrences and
        // this would cache.
        let (_, cached) = run("function onTick() local t={x=1} local a=t.x t.x=5 local b=t.x end");
        assert_eq!(cached, 0);
    }

    #[test]
    fn index_write_to_another_key_preserves_the_group() {
        // A per-key barrier: a write to `t.y` closes only the
        // (t, y) group, so the (t, x) reads on both sides still
        // form one group.
        let (code, cached) =
            run("function onTick() local t={x=1,y=2} local a=t.x t.y=5 local b=t.x end");
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t.x").count(), 1, "code: {code}");
    }

    #[test]
    fn same_key_write_splits_into_two_caches() {
        let (code, cached) = run(
            "function onTick() local t={x=1} local a=t.x local a2=t.x t.x=5 local b=t.x local b2=t.x end",
        );
        assert_eq!(cached, 2, "code: {code}");
        // Two cache definitions plus the barrier target's own
        // text, which stays a write.
        assert_eq!(code.matches("t.x").count(), 3, "code: {code}");
    }

    #[test]
    fn other_group_survives_a_same_key_write() {
        // (t, y) spans the (t, x) barrier untouched, while (t, x)
        // splits into two single occurrences and is not cached.
        let (code, cached) = run(
            "function onTick() local t={x=1,y=2} local a=t.x local p=t.y t.x=5 local b=t.x local q=t.y end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t.y").count(), 1, "code: {code}");
        // Both (t, x) reads stay, plus the barrier target's text.
        assert_eq!(code.matches("t.x").count(), 3, "code: {code}");
    }

    #[test]
    fn equivalent_numeric_key_write_splits() {
        // Cycle 94 (Codex finding 3, straightline form): the
        // `t[1.0]` write splits the `t[1]` group into two caches,
        // exactly like a same-spelling write.
        let (code, cached) = run(
            "function onTick() local t={9} local a=t[1] local a2=t[1] t[1.0]=3 local b=t[1] local b2=t[1] end",
        );
        assert_eq!(cached, 2, "code: {code}");
    }

    #[test]
    fn dynamic_key_write_to_same_base_splits() {
        // A dynamic key may be any key of the base, so it closes
        // every group of that base.
        let (code, cached) = run(
            "function onTick() local t={x=1} local a=t.x local a2=t.x t[k]=5 local b=t.x local b2=t.x end",
        );
        assert_eq!(cached, 2, "code: {code}");
        assert_eq!(code.matches("t.x").count(), 2, "code: {code}");
    }

    #[test]
    fn write_through_non_name_object_splits_all_groups() {
        // The write object is itself an index read, so it may
        // alias any base and closes every group.
        let (_, cached) =
            run("function onTick() local t={x=1} local a=t.x a.b.y=5 local b=t.x end");
        assert_eq!(cached, 0);
    }

    #[test]
    fn barrier_statement_value_read_belongs_before_the_write() {
        // The barrier statement's own value read executes before
        // the write takes effect, so it joins the pre-write side.
        let (code, cached) =
            run("function onTick() local t={x=1,y=2} local a=t.x t.y=t.x local b=t.x end");
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t.x").count(), 1, "code: {code}");
        assert!(code.contains("t.y=c"), "code: {code}");
    }

    #[test]
    fn reads_across_a_branch_do_not_group() {
        let (_, cached) =
            run("function onTick() local t={x=1} local a=t.x if c then local b=t.x end end");
        assert_eq!(cached, 0);
    }

    #[test]
    fn reads_inside_a_nested_function_do_not_group() {
        let (_, cached) = run(
            "function onTick() local t={x=1} local a=t.x local f=function() return t.x end end",
        );
        assert_eq!(cached, 0);
    }

    #[test]
    fn caches_repeated_reads_through_chunk_global() {
        let (code, cached) = run("g={x=1} function onTick() local a=g.x local b=g.x end");
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("g.x").count(), 1, "code: {code}");
    }

    #[test]
    fn skips_global_initialized_from_call() {
        let (_, cached) = run("g=make() function onTick() local a=g.x local b=g.x end");
        assert_eq!(cached, 0);
    }

    #[test]
    fn dot_and_bracket_string_keys_share_a_group() {
        let (code, cached) =
            run("function onTick() local t={x=1} local a=t.x local b=t[\"x\"] end");
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t.x").count(), 1, "code: {code}");
    }

    #[test]
    fn caches_two_reads_in_one_statement() {
        let (code, cached) = run("function onTick() local t={x=1} local a=t.x+t.x end");
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t.x").count(), 1, "code: {code}");
    }

    #[test]
    fn partial_caches_only_the_first_group() {
        // Two qualifying groups: the partial variant caches the
        // traversal-first one (t.x) and leaves t.y untouched.
        let (code, cached) = run_partial(
            "function onTick() local t={x=1,y=2} local a=t.x local p=t.y local b=t.x local q=t.y end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t.x").count(), 1, "code: {code}");
        assert_eq!(code.matches("t.y").count(), 2, "code: {code}");
    }

    #[test]
    fn partial_skips_an_unproven_first_group() {
        // The first group (parameter base) fails the proof, so it
        // does not consume the one-group budget; the proven group
        // is cached instead.
        let (code, cached) = run_partial(
            "function f(u) local a=u.x local b=u.x local t={x=1} local c=t.x local d=t.x end",
        );
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("u.x").count(), 2, "code: {code}");
        assert_eq!(code.matches("t.x").count(), 1, "code: {code}");
    }

    #[test]
    fn partial_single_group_matches_full() {
        let (code, cached) =
            run_partial("function onTick() local t={x=1} local a=t.x local b=t.x end");
        assert_eq!(cached, 1, "code: {code}");
        assert_eq!(code.matches("t.x").count(), 1, "code: {code}");
    }

    #[test]
    fn skips_when_base_is_a_parameter() {
        let (_, cached) = run("function f(t) local a=t.x local b=t.x end");
        assert_eq!(cached, 0);
    }

    fn cached_count(result: &crate::pass::PassResult) -> usize {
        result
            .details
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.strip_prefix("cached="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    }

    #[test]
    fn composite_mr_then_smc_caches_both() {
        // Mirrors the search.rs `joint-api-local-mr-smc` construction:
        // MR first (loop reads), then SMC on the MR layout
        // (straight-line reads in the same function).
        let source = "function onTick() local t={x=1} local a=t.x local b=t.x local s=0 for i=1,10 do local d=t.x local e=t.x s=d+e end end";
        let (mut ast, root) = parse_source(source).expect("parse");
        let mr = crate::passes::loop_member_reads::cache_loop_member_reads(&mut ast, root);
        assert_eq!(cached_count(&mr), 1, "MR must fire");
        let smc = cache_straightline_member_reads(&mut ast, mr.root);
        assert_eq!(cached_count(&smc), 1, "SMC must fire on the MR layout");
        let code = Printer::new(&ast, false).output(smc.root);
        assert_eq!(code.matches("t.x").count(), 1, "code: {code}");
        parse_source(&code).expect("composite output reparses");
    }

    #[test]
    fn composite_mr_then_smc_partial_caches_first_group_only() {
        // Mirrors the search.rs `joint-api-local-mr-smc-partial`
        // construction: MR first, then the partial form of this
        // transform on the MR layout. Two straight-line groups
        // qualify after MR (t.y and t.z); the partial form caches
        // only the traversal-first one (t.y).
        let source = "function onTick() local t={x=1,y=2,z=3} local a=t.y local b=t.y local c=t.z local d=t.z local s=0 for i=1,10 do local e=t.x local f=t.x s=e+f end end";
        let (mut ast, root) = parse_source(source).expect("parse");
        let mr = crate::passes::loop_member_reads::cache_loop_member_reads(&mut ast, root);
        assert_eq!(cached_count(&mr), 1, "MR must fire");
        let smc = cache_straightline_member_reads_partial(&mut ast, mr.root);
        assert_eq!(cached_count(&smc), 1, "partial SMC must cache one group");
        let code = Printer::new(&ast, false).output(smc.root);
        assert_eq!(code.matches("t.y").count(), 1, "code: {code}");
        assert_eq!(code.matches("t.z").count(), 2, "code: {code}");
        parse_source(&code).expect("composite output reparses");
    }

    #[test]
    fn composite_s1_then_smc_partial_caches_first_group_only() {
        // Mirrors the search.rs `joint-api-local-s1-smc-partial`
        // construction: S1 first, then the partial form of this
        // transform on the S1 layout. Two straight-line groups
        // qualify after S1 (t.x and t.y); the partial form caches
        // only the traversal-first one (t.x).
        let source = "function onTick() local t={x=1,y=2} local a=t.x local b=t.x local p=t.y local q=t.y while true do screen.drawRectF(1,2,3,4) if true then break end end end";
        let (mut ast, root) = parse_source(source).expect("parse");
        let s1 = crate::passes::single_member_cache::cache_single_member_calls_post_alias(
            &mut ast, root,
        );
        assert_eq!(cached_count(&s1), 1, "S1 must fire");
        let smc = cache_straightline_member_reads_partial(&mut ast, s1.root);
        assert_eq!(cached_count(&smc), 1, "partial SMC must cache one group");
        let code = Printer::new(&ast, false).output(smc.root);
        assert_eq!(code.matches("t.x").count(), 1, "code: {code}");
        assert_eq!(code.matches("t.y").count(), 2, "code: {code}");
        parse_source(&code).expect("composite output reparses");
    }

    #[test]
    fn composite_s1_then_smc_caches_both() {
        // Mirrors the search.rs `joint-api-local-s1-smc` construction:
        // S1 first (a repeated member call in a loop), then SMC on
        // the S1 layout (straight-line reads in the same function).
        // The S1 cache definition's own callee read has an unproven
        // host base, so SMC must leave it alone while caching t.x.
        let source = "function onTick() local t={x=1} local a=t.x local b=t.x while true do screen.drawRectF(1,2,3,4) if true then break end end end";
        let (mut ast, root) = parse_source(source).expect("parse");
        let s1 = crate::passes::single_member_cache::cache_single_member_calls_post_alias(
            &mut ast, root,
        );
        assert_eq!(cached_count(&s1), 1, "S1 must fire");
        let smc = cache_straightline_member_reads(&mut ast, s1.root);
        assert_eq!(cached_count(&smc), 1, "SMC must fire on the S1 layout");
        let code = Printer::new(&ast, false).output(smc.root);
        assert_eq!(code.matches("screen.drawRectF").count(), 1, "code: {code}");
        assert_eq!(code.matches("t.x").count(), 1, "code: {code}");
        parse_source(&code).expect("composite output reparses");
    }
}
