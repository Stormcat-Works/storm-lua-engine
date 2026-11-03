//! Redundant `screen.setColor` elimination (pixel identity, transparent background).
//!
//! Within one block, a literal `screen.setColor` call is redundant when an
//! earlier literal set of the same color in the same block already defines
//! the current color, and nothing between them can change it. Removing such
//! a call shortens the generated code and the draw-command stream without
//! changing any pixel: every draw after the removed call still runs under
//! the same color, because the earlier same-color set is still in effect.
//!
//! This pass is intentionally conservative (v1):
//! - Only literal colors count: every argument must be a numeric literal
//!   (3 or 4 arguments; arity is part of the key, so a 3-argument set is
//!   never merged with a 4-argument set even when the missing alpha would
//!   default to the same value). Color equality is exact on the parsed
//!   `f64` values, not on clamped bytes, so values that merely clamp to
//!   the same byte are never merged.
//! - The first setColor in a block is never removed. The monitor
//!   background is transparent, not black opaque: an undrawn pixel is not
//!   a black pixel, so no set is ever treated as matching an initial or
//!   background color.
//! - A set is a barrier when it is non-literal (its color is unknown), and
//!   any statement other than a pure local binding or a `screen.draw*`
//!   call is a barrier: user calls, control flow, and index writes could
//!   change the color state. Draw calls read the color but never change
//!   it, and `screen.drawClear` does not reset the color, so both are
//!   transparent for the base removal below. (For reordering, `drawClear`
//!   is a hard barrier, because it erases what earlier draws painted.)
//! - Reordering (the second half of v1): a later literal set `S_k` whose
//!   color already appeared earlier in the same segment may be merged
//!   into that earlier set when its draw group can move across every
//!   intervening draw without changing any pixel. The group (the draws
//!   after `S_k` up to the next set) moves to just after the earlier
//!   set's group, and `S_k` itself is removed. The move is allowed only
//!   when every moved draw and every crossed draw has a known
//!   conservative axis-aligned bounding box (literal coordinates of
//!   `drawRect` / `drawRectF`, `drawCircle` / `drawCircleF`,
//!   `drawTriangle` / `drawTriangleF`, or `drawLine`) and every
//!   moved/crossed pair is disjoint (touching counts as overlapping).
//!   Draws with unknown regions (`drawText`, `drawTextBox`, non-literal
//!   coordinates, and any other draw), unknown calls, control flow,
//!   non-literal sets, and `drawClear` all block the move. The relative
//!   order of overlapping draws never changes, and a moved draw's
//!   effective color is unchanged at its destination because it lands
//!   under the same color. One more condition: the candidate set must
//!   not be the segment's last set. Color state persists past the
//!   segment (across statements, ticks, and callbacks), so the last
//!   surviving set defines the outgoing state; removing a last-set
//!   candidate would replace its color with an intervening set's.
//!   A later set in the segment re-establishes the outgoing state and
//!   makes the move sound.
//! - The callee must resolve to `screen.setColor` / `screen.draw*`
//!   through the effect analyzer (direct or alias form, never a method
//!   call), and the pass bails out entirely when `screen` appears in the
//!   program-wide `mutated_builtin_roots` set, so a script that replaces
//!   the host table is never rewritten.
//! - Only statements are removed or moved; no names are introduced, so
//!   no scope renaming is needed afterwards.
//!
//! This pass runs before `ordered-screen-call-factoring`, so factoring
//! groups the surviving literal setColor calls only.

use std::collections::HashSet;

use crate::pass::PassResult;
use storm_lua_analysis::effects::{mutated_builtin_roots, EffectAnalyzer};
use storm_lua_analysis::resolver::{resolve, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::ast_utils;

fn clone_source(source: &Ast) -> Ast {
    let mut ast = ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

/// A literal color: parsed `f64` bits of every argument, arity included.
type Color = Vec<u64>;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Aabb {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Aabb {
    fn overlaps(self, other: Aabb) -> bool {
        // Touching counts as overlapping: conservative for pixel identity.
        self.x0 <= other.x1 && other.x0 <= self.x1 && self.y0 <= other.y1 && other.y0 <= self.y1
    }
}

/// Classification of one statement inside a block scan.
enum StmtKind {
    /// Literal setColor with its color key.
    SetColor(Color),
    /// A draw call with a known bounding box.
    Draw(Aabb),
    /// A draw call whose painted region is unknown (text, map, ...).
    /// Transparent for base removal, blocking for reordering.
    UnknownDraw,
    /// `screen.drawClear`: transparent for base removal (it does not
    /// reset the color), a hard barrier for reordering.
    DrawClear,
    /// A pure local binding: literal/name right-hand sides, name targets.
    PureBinding,
    /// Anything else: a barrier for both base removal and reordering.
    Barrier,
}

fn call_parts(source: &Ast, statement: NodeId) -> Option<(NodeId, Vec<NodeId>)> {
    let Node::Callstat(expression) = source.node(statement) else {
        return None;
    };
    let Node::Call(function, arguments, method) = source.node(*expression).clone() else {
        return None;
    };
    if method.is_some() {
        return None;
    }
    Some((function, arguments))
}

fn literal_num(source: &Ast, id: NodeId) -> Option<f64> {
    let Node::Num(text) = source.node(id) else {
        return None;
    };
    let value: f64 = text.parse().ok()?;
    if value.is_finite() {
        Some(value)
    } else {
        None
    }
}

fn literal_color(source: &Ast, arguments: &[NodeId]) -> Option<Color> {
    if arguments.len() != 3 && arguments.len() != 4 {
        return None;
    }
    let mut color = Vec::with_capacity(arguments.len());
    for argument in arguments {
        color.push(literal_num(source, *argument)?.to_bits());
    }
    Some(color)
}

fn literal_coords(source: &Ast, arguments: &[NodeId]) -> Option<Vec<f64>> {
    let mut coords = Vec::with_capacity(arguments.len());
    for argument in arguments {
        coords.push(literal_num(source, *argument)?);
    }
    Some(coords)
}

fn draw_aabb(name: &str, coords: &[f64]) -> Option<Aabb> {
    let (x0, y0, x1, y1) = match name {
        "screen.drawRect" | "screen.drawRectF" if coords.len() == 4 => {
            let (x, y, w, h) = (coords[0], coords[1], coords[2], coords[3]);
            (x.min(x + w), y.min(y + h), x.max(x + w), y.max(y + h))
        }
        "screen.drawCircle" | "screen.drawCircleF" if coords.len() == 3 => {
            let (x, y, r) = (coords[0], coords[1], coords[2]);
            (x - r, y - r, x + r, y + r)
        }
        "screen.drawTriangle" | "screen.drawTriangleF" if coords.len() == 6 => {
            let xs = [coords[0], coords[2], coords[4]];
            let ys = [coords[1], coords[3], coords[5]];
            (
                xs.iter().cloned().fold(f64::INFINITY, f64::min),
                ys.iter().cloned().fold(f64::INFINITY, f64::min),
                xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
                ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
            )
        }
        "screen.drawLine" if coords.len() == 4 => {
            let (x0, y0, x1, y1) = (coords[0], coords[1], coords[2], coords[3]);
            (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
        }
        _ => return None,
    };
    Some(Aabb { x0, y0, x1, y1 })
}

fn is_simple_pure_expr(source: &Ast, id: NodeId) -> bool {
    matches!(
        source.node(id),
        Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil | Node::Name(_)
    )
}

fn classify(source: &Ast, analyzer: &EffectAnalyzer<'_>, statement: NodeId) -> StmtKind {
    match source.node(statement).clone() {
        Node::Local(_, expressions) => {
            if expressions.iter().all(|e| is_simple_pure_expr(source, *e)) {
                StmtKind::PureBinding
            } else {
                StmtKind::Barrier
            }
        }
        Node::Assign(targets, expressions) => {
            let targets_ok = targets
                .iter()
                .all(|t| matches!(source.node(*t), Node::Name(_)));
            if targets_ok && expressions.iter().all(|e| is_simple_pure_expr(source, *e)) {
                StmtKind::PureBinding
            } else {
                StmtKind::Barrier
            }
        }
        Node::Callstat(_) => {
            let Some((function, arguments)) = call_parts(source, statement) else {
                return StmtKind::Barrier;
            };
            let Some(builtin) = analyzer.resolve_builtin_reference(function) else {
                return StmtKind::Barrier;
            };
            if builtin == "screen.setColor" {
                return match literal_color(source, &arguments) {
                    Some(color) => StmtKind::SetColor(color),
                    None => StmtKind::Barrier,
                };
            }
            if builtin == "screen.drawClear" {
                return StmtKind::DrawClear;
            }
            if builtin.starts_with("screen.draw") {
                return match literal_coords(source, &arguments)
                    .and_then(|coords| draw_aabb(&builtin, &coords))
                {
                    Some(aabb) => StmtKind::Draw(aabb),
                    None => StmtKind::UnknownDraw,
                };
            }
            StmtKind::Barrier
        }
        _ => StmtKind::Barrier,
    }
}

struct Transformer<'a> {
    source: &'a Ast,
    analyzer: EffectAnalyzer<'a>,
    output: Ast,
    removed_base: usize,
    removed_reorder: usize,
    moved_draws: usize,
}

impl<'a> Transformer<'a> {
    fn new(source: &'a Ast, root: NodeId, resolution: &'a Resolution) -> Self {
        let analyzer = EffectAnalyzer::new(source, resolution, root, true);
        Self {
            source,
            analyzer,
            output: clone_source(source),
            removed_base: 0,
            removed_reorder: 0,
            moved_draws: 0,
        }
    }

    /// Base removal + reordering for one block. Returns the new statement
    /// list, or `None` when the block is unchanged.
    fn rewrite_block_statements(&mut self, statements: &[NodeId]) -> Option<Vec<NodeId>> {
        let kinds: Vec<StmtKind> = statements
            .iter()
            .map(|s| classify(self.source, &self.analyzer, *s))
            .collect();

        // Phase 1: base removal, forward scan. The tracked color survives
        // draws (including drawClear) and pure bindings; any barrier clears
        // it. A set equal to the tracked color is redundant.
        let mut kept: Vec<NodeId> = Vec::with_capacity(statements.len());
        let mut kept_kinds: Vec<StmtKind> = Vec::with_capacity(statements.len());
        let mut current: Option<Color> = None;
        for (index, statement) in statements.iter().enumerate() {
            match &kinds[index] {
                StmtKind::SetColor(color) => {
                    if current.as_ref() == Some(color) {
                        self.removed_base += 1;
                    } else {
                        current = Some(color.clone());
                        kept.push(*statement);
                        kept_kinds.push(StmtKind::SetColor(color.clone()));
                    }
                }
                StmtKind::Barrier => {
                    current = None;
                    kept.push(*statement);
                    kept_kinds.push(StmtKind::Barrier);
                }
                StmtKind::Draw(aabb) => {
                    kept.push(*statement);
                    kept_kinds.push(StmtKind::Draw(*aabb));
                }
                StmtKind::UnknownDraw => {
                    kept.push(*statement);
                    kept_kinds.push(StmtKind::UnknownDraw);
                }
                StmtKind::DrawClear => {
                    kept.push(*statement);
                    kept_kinds.push(StmtKind::DrawClear);
                }
                StmtKind::PureBinding => {
                    kept.push(*statement);
                    kept_kinds.push(StmtKind::PureBinding);
                }
            }
        }

        // Phase 2: reordering, greedily one move at a time. Each move
        // removes one setColor, so the loop terminates.
        while let Some((set_pos, insert_pos, moved)) = find_reorder_move(&kept, &kept_kinds) {
            // `moved` holds positions (into `kept`) of the group draws, in
            // order. Build the new list: everything except the moved draws
            // and the candidate set, with the moved draws inserted at
            // `insert_pos` (an index into the list without those items).
            let moved_set: HashSet<usize> = moved.iter().copied().collect();
            let mut rest: Vec<NodeId> = Vec::with_capacity(kept.len());
            let mut rest_kinds: Vec<StmtKind> = Vec::with_capacity(kept.len());
            for (index, statement) in kept.iter().enumerate() {
                if index != set_pos && !moved_set.contains(&index) {
                    rest.push(*statement);
                    rest_kinds.push(match &kept_kinds[index] {
                        StmtKind::SetColor(c) => StmtKind::SetColor(c.clone()),
                        StmtKind::Draw(a) => StmtKind::Draw(*a),
                        StmtKind::UnknownDraw => StmtKind::UnknownDraw,
                        StmtKind::DrawClear => StmtKind::DrawClear,
                        StmtKind::PureBinding => StmtKind::PureBinding,
                        StmtKind::Barrier => StmtKind::Barrier,
                    });
                }
            }
            // Adjust the insertion index: it was computed on the list
            // without the candidate set and the moved draws, in the same
            // order as `rest`, so it indexes `rest` directly.
            let mut next: Vec<NodeId> = Vec::with_capacity(kept.len());
            let mut next_kinds: Vec<StmtKind> = Vec::with_capacity(kept.len());
            for (index, statement) in rest.iter().enumerate() {
                if index == insert_pos {
                    for moved_pos in &moved {
                        next.push(kept[*moved_pos]);
                        next_kinds.push(match &kept_kinds[*moved_pos] {
                            StmtKind::Draw(a) => StmtKind::Draw(*a),
                            _ => unreachable!("moved items are draws"),
                        });
                    }
                }
                next.push(*statement);
                next_kinds.push(match &rest_kinds[index] {
                    StmtKind::SetColor(c) => StmtKind::SetColor(c.clone()),
                    StmtKind::Draw(a) => StmtKind::Draw(*a),
                    StmtKind::UnknownDraw => StmtKind::UnknownDraw,
                    StmtKind::DrawClear => StmtKind::DrawClear,
                    StmtKind::PureBinding => StmtKind::PureBinding,
                    StmtKind::Barrier => StmtKind::Barrier,
                });
            }
            if insert_pos == rest.len() {
                for moved_pos in &moved {
                    next.push(kept[*moved_pos]);
                    next_kinds.push(StmtKind::Draw(match &kept_kinds[*moved_pos] {
                        StmtKind::Draw(a) => *a,
                        _ => unreachable!("moved items are draws"),
                    }));
                }
            }
            self.removed_reorder += 1;
            self.moved_draws += moved.len();
            kept = next;
            kept_kinds = next_kinds;
        }

        if kept != statements {
            Some(kept)
        } else {
            None
        }
    }

    fn transform_block(&mut self, block: NodeId) {
        let Node::Block(statements) = self.source.node(block).clone() else {
            return;
        };
        for statement in &statements {
            self.transform_statement(*statement);
        }
        if let Some(kept) = self.rewrite_block_statements(&statements) {
            self.output
                .nodes
                .rewrite(block, Node::Block(kept), "redundant-setcolor-elimination");
        }
    }

    fn transform_statement(&mut self, statement: NodeId) {
        match self.source.node(statement).clone() {
            Node::Block(_) => self.transform_block(statement),
            Node::Do(body) => self.transform_block(body),
            Node::While(condition, body) => {
                self.transform_expr(condition);
                self.transform_block(body);
            }
            Node::Repeat(body, condition) => {
                self.transform_block(body);
                self.transform_expr(condition);
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
                self.transform_expr(function);
            }
            Node::Localfunc(_, function) => self.transform_expr(function),
            Node::Local(_, expressions) | Node::Return(expressions) => {
                for expression in expressions {
                    self.transform_expr(expression);
                }
            }
            Node::Callstat(expression) => self.transform_expr(expression),
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
            Node::Function(_, _, body) => self.transform_block(body),
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

/// Find one full-move reorder candidate in a post-base-removal statement
/// list. Returns `(candidate set position, insertion position in the
/// list without the candidate set and the moved draws, moved draw
/// positions)`. Segments are split by barriers and drawClear; pure
/// bindings and unknown draws stay inside segments (unknown draws block
/// any move they participate in).
fn find_reorder_move(
    statements: &[NodeId],
    kinds: &[StmtKind],
) -> Option<(usize, usize, Vec<usize>)> {
    // Segment boundaries: positions of barriers and drawClear.
    let mut seg_start = 0usize;
    let mut index = 0usize;
    while index <= statements.len() {
        if index == statements.len()
            || matches!(kinds[index], StmtKind::Barrier | StmtKind::DrawClear)
        {
            if let Some(found) = find_reorder_in_segment(statements, kinds, seg_start, index) {
                return Some(found);
            }
            seg_start = index + 1;
        }
        index += 1;
    }
    None
}

fn find_reorder_in_segment(
    statements: &[NodeId],
    kinds: &[StmtKind],
    start: usize,
    end: usize,
) -> Option<(usize, usize, Vec<usize>)> {
    // Collect set positions in the segment, in order.
    let mut sets: Vec<usize> = Vec::new();
    for (pos, kind) in kinds.iter().enumerate().skip(start).take(end - start) {
        if matches!(kind, StmtKind::SetColor(_)) {
            sets.push(pos);
        }
    }
    for (set_idx, &set_pos) in sets.iter().enumerate() {
        let StmtKind::SetColor(color) = &kinds[set_pos] else {
            continue;
        };
        // The candidate needs an earlier same-color set in this segment.
        // The nearest one minimizes the crossed draws.
        let Some(earlier) = sets[..set_idx]
            .iter()
            .rev()
            .find(|&&pos| match &kinds[pos] {
                StmtKind::SetColor(other) => other == color,
                _ => false,
            })
            .copied()
        else {
            continue;
        };
        // The candidate must not be the segment's last set. The move
        // removes the candidate and lets the group draws run under the
        // earlier same-color set, so every draw keeps its color — but
        // the color state that flows OUT of the segment is the last
        // surviving set's color. When the candidate is the last set,
        // removing it would hand that role to an intervening
        // different-color set, changing the state observed by everything
        // after the segment (color persists across statements, ticks,
        // and callbacks). A later set in the segment re-establishes the
        // outgoing state, so only then is the move sound. (Candidate S7:
        // the intervening set arrived via one-use-function-fusion, but
        // the defect is in this rule, not in the fusion.)
        if sets.get(set_idx + 1).is_none() {
            continue;
        }
        // The candidate's group: draws after it up to the next set.
        let next_set = sets.get(set_idx + 1).copied().unwrap_or(end);
        let mut group: Vec<usize> = Vec::new();
        let mut group_ok = true;
        for (pos, kind) in kinds
            .iter()
            .enumerate()
            .skip(set_pos + 1)
            .take(next_set - (set_pos + 1))
        {
            match kind {
                StmtKind::Draw(_) => group.push(pos),
                StmtKind::UnknownDraw => group_ok = false,
                StmtKind::PureBinding => {}
                _ => {}
            }
        }
        if group.is_empty() || !group_ok {
            continue;
        }
        // The earlier set's group ends at the next set after it.
        let earlier_next = sets
            .iter()
            .find(|&&pos| pos > earlier)
            .copied()
            .unwrap_or(end);
        // Crossed draws: draws strictly between the earlier group's end
        // and the candidate set. Every one must have a known box and be
        // disjoint from every group draw.
        let mut crossed: Vec<Aabb> = Vec::new();
        let mut crossed_ok = true;
        for kind in kinds.iter().skip(earlier_next).take(set_pos - earlier_next) {
            match kind {
                StmtKind::Draw(aabb) => crossed.push(*aabb),
                StmtKind::UnknownDraw => crossed_ok = false,
                _ => {}
            }
        }
        if !crossed_ok {
            continue;
        }
        let group_boxes: Vec<Aabb> = group
            .iter()
            .map(|&pos| match kinds[pos] {
                StmtKind::Draw(aabb) => aabb,
                _ => unreachable!("group holds draws only"),
            })
            .collect();
        if group_boxes
            .iter()
            .any(|g| crossed.iter().any(|c| g.overlaps(*c)))
        {
            continue;
        }
        // Insertion position in the list without the candidate set and
        // the group draws: count the surviving statements before the
        // earlier group's end (i.e. before `earlier_next`, excluding the
        // candidate set, which is after that point, and excluding group
        // draws, which are also after that point).
        let insert_pos = (0..earlier_next)
            .filter(|&pos| pos != set_pos && !group.contains(&pos))
            .count();
        let _ = statements;
        return Some((set_pos, insert_pos, group));
    }
    None
}

/// Remove redundant `screen.setColor` calls (base + reorder moves).
/// Returns the counts in `details` when anything changed.
pub fn eliminate_redundant_setcolors(ast: &mut Ast, root: NodeId) -> PassResult {
    if !matches!(ast.node(root), Node::Block(_)) {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let source = clone_source(ast);
    let resolution = resolve(&source, root);
    if mutated_builtin_roots(&source, &resolution, root).contains("screen") {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let mut transformer = Transformer::new(&source, root, &resolution);
    transformer.transform_block(root);
    let removed_base = transformer.removed_base;
    let removed_reorder = transformer.removed_reorder;
    let moved_draws = transformer.moved_draws;
    if removed_base + removed_reorder == 0 {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    *ast = transformer.output;
    PassResult {
        root,
        saved: None,
        details: Some(vec![format!(
            "removed_base={removed_base} removed_reorder={removed_reorder} moved_draws={moved_draws}"
        )]),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn run(source: &str) -> (String, usize, usize, usize) {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = eliminate_redundant_setcolors(&mut ast, root);
        let code = Printer::new(&ast, false).output(result.root);
        let (mut base, mut reorder, mut moved) = (0, 0, 0);
        if let Some(details) = &result.details {
            if let Some(first) = details.first() {
                for part in first.split_whitespace() {
                    let (key, value) = part.split_once('=').unwrap_or(("", ""));
                    let value: usize = value.parse().unwrap_or(0);
                    match key {
                        "removed_base" => base = value,
                        "removed_reorder" => reorder = value,
                        "moved_draws" => moved = value,
                        _ => {}
                    }
                }
            }
        }
        (code, base, reorder, moved)
    }

    fn count(code: &str, needle: &str) -> usize {
        code.matches(needle).count()
    }

    #[test]
    fn removes_second_same_color_set_across_draws() {
        let (code, base, reorder, moved) = run(
            "function onDraw() screen.setColor(255,0,0) screen.drawRectF(0,0,10,10) screen.setColor(255,0,0) screen.drawRectF(20,0,10,10) end",
        );
        assert_eq!((base, reorder, moved), (1, 0, 0), "code: {code}");
        assert_eq!(count(&code, "setColor"), 1, "code: {code}");
        assert_eq!(count(&code, "drawRectF"), 2, "code: {code}");
    }

    #[test]
    fn keeps_first_set_never_removed() {
        // Transparent background: a lone first set is never redundant.
        let (code, base, reorder, moved) =
            run("function onDraw() screen.setColor(0,0,0) screen.drawRectF(0,0,10,10) end");
        assert_eq!((base, reorder, moved), (0, 0, 0), "code: {code}");
        assert_eq!(count(&code, "setColor"), 1, "code: {code}");
    }

    #[test]
    fn different_color_between_blocks_base_removal() {
        let (code, base, reorder, moved) = run(
            "function onDraw() screen.setColor(255,0,0) screen.drawRectF(0,0,10,10) screen.setColor(0,255,0) screen.drawRectF(20,0,10,10) screen.setColor(255,0,0) screen.drawRectF(40,0,10,10) end",
        );
        // The third set differs from the tracked (green) color, so the
        // base rule alone keeps it. It would be a reorder candidate,
        // but it is the segment's last set: removing it would change
        // the outgoing color state from red to green (candidate S7),
        // so no move happens and all three sets survive.
        assert_eq!(base, 0, "code: {code}");
        assert_eq!(reorder, 0, "code: {code}");
        assert_eq!(moved, 0, "code: {code}");
        assert_eq!(count(&code, "setColor"), 3, "code: {code}");
    }

    #[test]
    fn non_literal_set_is_a_barrier() {
        let (code, base, reorder, moved) = run(
            "function onDraw() screen.setColor(255,0,0) screen.drawRectF(0,0,10,10) screen.setColor(r,0,0) screen.drawRectF(20,0,10,10) screen.setColor(255,0,0) screen.drawRectF(40,0,10,10) end",
        );
        assert_eq!((base, reorder, moved), (0, 0, 0), "code: {code}");
        assert_eq!(count(&code, "setColor"), 3, "code: {code}");
    }

    #[test]
    fn unknown_call_is_a_barrier() {
        let (code, base, reorder, moved) = run(
            "function onDraw() screen.setColor(255,0,0) screen.drawRectF(0,0,10,10) foo() screen.setColor(255,0,0) screen.drawRectF(20,0,10,10) end",
        );
        assert_eq!((base, reorder, moved), (0, 0, 0), "code: {code}");
        assert_eq!(count(&code, "setColor"), 2, "code: {code}");
    }

    #[test]
    fn drawclear_does_not_reset_base_color() {
        let (code, base, reorder, moved) = run(
            "function onDraw() screen.setColor(255,0,0) screen.drawClear() screen.setColor(255,0,0) screen.drawRectF(0,0,10,10) end",
        );
        assert_eq!((base, reorder, moved), (1, 0, 0), "code: {code}");
        assert_eq!(count(&code, "setColor"), 1, "code: {code}");
    }

    #[test]
    fn reorder_moves_disjoint_group_and_removes_set() {
        // Red rect, green overlapping circle, red triangle disjoint from
        // both, and a trailing blue set. The triangle moves up under the
        // first red set and the second red set is removed. The trailing
        // set is what makes the move sound: it re-establishes the color
        // state that flows out of the segment (see the next test).
        let (code, base, reorder, moved) = run(
            "function onDraw() screen.setColor(255,0,0) screen.drawRectF(0,0,10,10) screen.setColor(0,255,0) screen.drawCircleF(5,5,5) screen.setColor(255,0,0) screen.drawTriangleF(50,50,60,50,55,60) screen.setColor(0,0,255) end",
        );
        assert_eq!((base, reorder, moved), (0, 1, 1), "code: {code}");
        assert_eq!(count(&code, "setColor"), 3, "code: {code}");
        // The triangle now precedes the green set.
        let tri = code.find("drawTriangleF").unwrap();
        let green = code.find("setColor(0,255,0)").unwrap();
        assert!(tri < green, "code: {code}");
    }

    #[test]
    fn reorder_blocked_when_candidate_is_the_segments_last_set() {
        // Same shape without the trailing set: the candidate red set is
        // the segment's last set, so its color is the state that flows
        // out of the block (color state persists across statements,
        // ticks, and callbacks). Removing it would leave the intervening
        // green as the outgoing color. No move may happen.
        let (code, base, reorder, moved) = run(
            "function onDraw() screen.setColor(255,0,0) screen.drawRectF(0,0,10,10) screen.setColor(0,255,0) screen.drawCircleF(5,5,5) screen.setColor(255,0,0) screen.drawTriangleF(50,50,60,50,55,60) end",
        );
        assert_eq!((base, reorder, moved), (0, 0, 0), "code: {code}");
        assert_eq!(count(&code, "setColor"), 3, "code: {code}");
    }

    #[test]
    fn reorder_blocked_for_fused_setter_shape() {
        // Candidate S7 shape (post-fusion layout): red set, draw, blue
        // set inlined from a one-use function, red set, draw. The second
        // red set is the block's last set; removing it and moving the
        // second draw ahead of the blue set would change the outgoing
        // color from red to blue.
        let (code, base, reorder, moved) = run(
            "function onTick() screen.setColor(255,0,0) screen.drawRect(1,1,4,4) screen.setColor(0,0,255) screen.setColor(255,0,0) screen.drawRect(6,1,4,4) end",
        );
        assert_eq!((base, reorder, moved), (0, 0, 0), "code: {code}");
        assert_eq!(count(&code, "setColor"), 3, "code: {code}");
        assert_eq!(count(&code, "drawRect"), 2, "code: {code}");
    }

    #[test]
    fn reorder_blocked_by_overlap() {
        let (code, base, reorder, moved) = run(
            "function onDraw() screen.setColor(255,0,0) screen.drawRectF(0,0,10,10) screen.setColor(0,255,0) screen.drawCircleF(5,5,5) screen.setColor(255,0,0) screen.drawTriangleF(8,8,20,8,14,20) end",
        );
        assert_eq!((base, reorder, moved), (0, 0, 0), "code: {code}");
        assert_eq!(count(&code, "setColor"), 3, "code: {code}");
    }

    #[test]
    fn reorder_blocked_by_unknown_region_draw() {
        let (code, base, reorder, moved) = run(
            "function onDraw() screen.setColor(255,0,0) screen.drawRectF(0,0,10,10) screen.setColor(0,255,0) screen.drawText(0,0,'hi') screen.setColor(255,0,0) screen.drawTriangleF(50,50,60,50,55,60) end",
        );
        assert_eq!((base, reorder, moved), (0, 0, 0), "code: {code}");
        assert_eq!(count(&code, "setColor"), 3, "code: {code}");
    }

    #[test]
    fn reorder_blocked_by_drawclear() {
        let (code, base, reorder, moved) = run(
            "function onDraw() screen.setColor(255,0,0) screen.drawRectF(0,0,10,10) screen.drawClear() screen.setColor(0,255,0) screen.drawCircleF(5,5,5) screen.setColor(255,0,0) screen.drawTriangleF(50,50,60,50,55,60) end",
        );
        assert_eq!((base, reorder, moved), (0, 0, 0), "code: {code}");
        assert_eq!(count(&code, "setColor"), 3, "code: {code}");
    }

    #[test]
    fn skips_mutated_screen_root() {
        let (code, base, reorder, moved) = run(
            "screen.setColor=function() end function onDraw() screen.setColor(255,0,0) screen.drawRectF(0,0,10,10) screen.setColor(255,0,0) screen.drawRectF(20,0,10,10) end",
        );
        assert_eq!((base, reorder, moved), (0, 0, 0), "code: {code}");
        assert_eq!(count(&code, "setColor"), 3, "code: {code}");
    }
}
