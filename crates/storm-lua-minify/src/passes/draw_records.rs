//! Ordered literal draw records, including simple drawing-wrapper calls.
//!
//! A batch replays precisely the original calls in precisely the original order.
//! We do not merge rectangles, reorder colors, round coordinates, or assume a
//! rasterizer. Only scalar literal arguments are stored ahead of the calls.
//! Callees are passed to a private replay helper; eligibility proves they cannot
//! be rebound by that replay. Simple root-level forwarding wrappers (H/V -> R)
//! can be normalized without expanding R's dynamic coordinate transformations.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use crate::pass::PassResult;
use crate::scope_rename::scope_rename_fast;
use storm_lua_analysis::resolver::{resolve, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId, TableField};
use storm_lua_syntax::numeric::{integer_literal_value, quote_lua};
use storm_lua_syntax::size::{measure_expr, measure_size, measure_stmt};

mod dictionary;
mod encoding;
mod grouped;
mod palette;
mod predictors;
mod prefix;
mod provenance;
mod regular;
mod rice;
mod shared;
mod translated;

use encoding::{emit_helper, helper_cost};

const MAX_PERIOD: usize = 4;
const MAX_COLUMNS: usize = 24;
const MAX_HELPERS: usize = 24;

// Keep the original literal syntax rather than using f64 or printed text as a
// semantic key. Integer/float subtype, signs, and exact 64-bit integers survive.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Atom {
    Num(Arc<str>),
    Str(Arc<str>),
    Bool(bool),
    Nil,
    Neg(Box<Atom>),
}
impl Atom {
    fn read(ast: &Ast, id: NodeId) -> Option<Self> {
        match ast.node(id) {
            Node::Num(s) => Some(Self::Num(s.clone())),
            Node::Str(s) => Some(Self::Str(s.clone())),
            Node::Bool(b) => Some(Self::Bool(*b)),
            Node::Nil => Some(Self::Nil),
            Node::Paren(n) => Self::read(ast, *n),
            Node::Un(op, n) if op == "-" && matches!(ast.node(*n), Node::Num(_)) => {
                Some(Self::Neg(Box::new(Self::read(ast, *n)?)))
            }
            _ => None,
        }
    }
    fn integer(&self) -> Option<i64> {
        match self {
            Self::Num(s) => integer_literal_value(s),
            Self::Neg(a) => a.integer().map(i64::wrapping_neg),
            _ => None,
        }
    }
    fn emit(&self, ast: &mut Ast) -> NodeId {
        match self {
            Self::Num(s) => ast.push(Node::Num(s.clone())),
            Self::Str(s) => ast.push(Node::Str(s.clone())),
            Self::Bool(b) => ast.push(Node::Bool(*b)),
            Self::Nil => ast.push(Node::Nil),
            Self::Neg(a) => {
                let n = a.emit(ast);
                ast.push(Node::Un("-".into(), n))
            }
        }
    }
}

fn draw_builtin(name: &str) -> bool {
    matches!(
        name,
        "screen.setColor"
            | "screen.drawClear"
            | "screen.drawLine"
            | "screen.drawRect"
            | "screen.drawRectF"
            | "screen.drawCircle"
            | "screen.drawCircleF"
            | "screen.drawTriangle"
            | "screen.drawTriangleF"
            | "screen.drawText"
            | "screen.drawTextBox"
            | "screen.drawMap"
    )
}

fn no_calls(ast: &Ast, id: NodeId) -> bool {
    match ast.node(id) {
        Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil | Node::Name(_) => true,
        Node::Un(_, n) | Node::Paren(n) => no_calls(ast, *n),
        Node::Bin(_, a, b) | Node::Index(a, b, _) => no_calls(ast, *a) && no_calls(ast, *b),
        _ => false,
    }
}

fn wrapper_call(ast: &Ast, function: NodeId) -> Option<NodeId> {
    let Node::Function(_, false, body) = ast.node(function) else {
        return None;
    };
    let Node::Block(stmts) = ast.node(*body) else {
        return None;
    };
    if stmts.len() != 1 {
        return None;
    }
    let call = match ast.node(stmts[0]) {
        Node::Callstat(c) => *c,
        Node::Return(values) if values.len() == 1 => values[0],
        _ => return None,
    };
    let Node::Call(_, args, None) = ast.node(call) else {
        return None;
    };
    args.iter().all(|&n| no_calls(ast, n)).then_some(call)
}

struct Call {
    statement: NodeId,
    callee: NodeId,
    key: String,
    // Reading a bare name is total; indexing an uninitialized API alias is not.
    // Later callees must not throw before earlier commands have executed.
    total_read: bool,
    args: Vec<Atom>,
    origin_call: NodeId,
    // Normalized argument source nodes, collected only when tracing is enabled.
    origin_arguments: Vec<NodeId>,
}

struct Classifier<'a> {
    ast: &'a Ast,
    res: &'a Resolution,
    aliases: HashMap<BindingId, String>,
    wrappers: BTreeSet<BindingId>,
    order: HashMap<NodeId, usize>,
    definitions: HashMap<NodeId, usize>,
}
impl<'a> Classifier<'a> {
    fn new(ast: &'a Ast, res: &'a Resolution, root: NodeId) -> Self {
        let mut out = Self {
            ast,
            res,
            aliases: super::closed_fields::api_aliases(ast, res, root),
            wrappers: BTreeSet::new(),
            order: HashMap::new(),
            definitions: HashMap::new(),
        };
        fn visit(c: &mut Classifier<'_>, node: NodeId, top: bool) {
            let index = c.order.len();
            c.order.entry(node).or_insert(index);
            if top {
                if let Node::Funcstat(_, f) | Node::Localfunc(_, f) = c.ast.node(node) {
                    c.definitions.entry(*f).or_insert(index);
                }
            }
            match c.ast.node(node) {
                Node::Block(stmts) => {
                    for &s in stmts {
                        visit(c, s, top);
                    }
                }
                Node::Do(b) => visit(c, *b, top),
                _ => storm_lua_analysis::resolver::for_each_child(c.ast, node, &mut |n| {
                    visit(c, n, false)
                }),
            }
        }
        visit(&mut out, root, true);
        loop {
            let mut changed = false;
            for (bid, binding) in res.bindings.iter().enumerate().skip(1) {
                let b = bid as BindingId;
                if out.wrappers.contains(&b) {
                    continue;
                }
                let Some(function) = binding.function_node else {
                    continue;
                };
                // Only a single definition; a local initializer followed by an
                // assignment is not single-definition even if writes == 1.
                let expected = match binding.kind {
                    BindingKind::Global => 1,
                    BindingKind::Localfunc => 0,
                    BindingKind::Local => {
                        if binding.decl_node.is_some_and(
                            |n| matches!(ast.node(n), Node::Local(_, v) if v.is_empty()),
                        ) {
                            1
                        } else {
                            0
                        }
                    }
                    _ => continue,
                };
                if res.binding_write_counts[bid] != expected {
                    continue;
                }
                let Some(call) = wrapper_call(ast, function) else {
                    continue;
                };
                let Node::Call(f, _, _) = ast.node(call) else {
                    unreachable!()
                };
                if out.is_draw(*f) {
                    out.wrappers.insert(b);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        out
    }
    fn is_draw(&self, node: NodeId) -> bool {
        if super::closed_fields::builtin_reference(self.ast, self.res, &self.aliases, node)
            .is_some_and(|name| draw_builtin(&name))
        {
            return true;
        }
        matches!(self.ast.node(node), Node::Name(_))
            && self.res.node_bid[node as usize].is_some_and(|b| self.wrappers.contains(&b))
    }
    fn total_callee_read(&self, node: NodeId) -> bool {
        match self.ast.node(node) {
            Node::Name(_) => true,
            Node::Paren(inner) => self.total_callee_read(*inner),
            Node::Index(object, _, _) => {
                // Only the immutable host screen itself is unconditionally
                // initialized. S=screen may occur after this callback or not at
                // all. The closed-key-space gate excludes mutation/reflection.
                matches!(self.ast.node(*object), Node::Name(_))
                    && self.res.node_bid[*object as usize].is_some_and(|b| {
                        let binding = self.res.binding(b);
                        binding.kind == BindingKind::Global
                            && self.ast.strings.get(binding.name) == "screen"
                            && self.res.binding_write_counts[b as usize] == 0
                    })
            }
            _ => false,
        }
    }
    fn same_visible_binding(&self, target: BindingId, at: NodeId) -> bool {
        let binding = self.res.binding(target);
        let mut scope = self.res.node_scope_id[at as usize];
        while let Some(s) = scope {
            let current = self.res.scope(s);
            let candidates = current
                .bindings
                .iter()
                .copied()
                .filter(|&b| self.res.binding(b).name == binding.name)
                .collect::<Vec<_>>();
            if !candidates.is_empty() {
                return candidates.iter().all(|&b| b == target);
            }
            scope = current.parent;
        }
        binding.kind == BindingKind::Global
    }
    fn normalize(&self, call: &mut Call) {
        let mut seen = HashSet::new();
        loop {
            if !matches!(self.ast.node(call.callee), Node::Name(_)) {
                break;
            }
            let Some(b) = self.res.node_bid[call.callee as usize] else {
                break;
            };
            if !self.wrappers.contains(&b) || !seen.insert(b) {
                break;
            }
            let binding = self.res.binding(b);
            let Some(function) = binding.function_node else {
                break;
            };
            // Root declarations must precede this call lexically. Functions
            // declared conditionally or inside a callback are not forwarded.
            if !self
                .definitions
                .get(&function)
                .is_some_and(|&d| d < self.order[&call.statement])
            {
                break;
            }
            let Some(inner) = wrapper_call(self.ast, function) else {
                break;
            };
            let Node::Call(target, args, None) = self.ast.node(inner) else {
                break;
            };
            if !matches!(self.ast.node(*target), Node::Name(_)) {
                break;
            }
            let Some(target_bid) = self.res.node_bid[*target as usize] else {
                break;
            };
            // Stop at the terminal drawing wrapper; do not expand its dynamic
            // expressions (rotation, scaling, scrolling) or a host API access.
            if !self.wrappers.contains(&target_bid)
                || !self.same_visible_binding(target_bid, call.statement)
            {
                break;
            }
            let Node::Function(params, false, _) = self.ast.node(function) else {
                break;
            };
            if call.args.len() != params.len() {
                break;
            }
            let param_bids = &self.res.node_bids[function as usize];
            let mut mapped = Vec::new();
            let mut origin_arguments = Vec::new();
            for &arg in args {
                if let Some(atom) = Atom::read(self.ast, arg) {
                    mapped.push(atom);
                    if self.ast.nodes.tracks_origins() {
                        origin_arguments.push(arg);
                    }
                } else if matches!(self.ast.node(arg), Node::Name(_)) {
                    let pos = self.res.node_bid[arg as usize]
                        .and_then(|b| param_bids.iter().position(|&p| p == b));
                    let Some(pos) = pos else { return };
                    let Some(value) = call.args.get(pos) else {
                        return;
                    };
                    mapped.push(value.clone());
                    if self.ast.nodes.tracks_origins() {
                        origin_arguments.push(call.origin_arguments[pos]);
                    }
                } else {
                    return;
                }
            }
            call.callee = *target;
            call.args = mapped;
            call.origin_arguments = origin_arguments;
        }
    }
    fn call(&self, statement: NodeId) -> Option<Call> {
        let Node::Callstat(expression) = self.ast.node(statement) else {
            return None;
        };
        let Node::Call(f, args, None) = self.ast.node(*expression) else {
            return None;
        };
        if args.len() > 8 || !self.is_draw(*f) {
            return None;
        }
        let atoms = args
            .iter()
            .map(|&n| Atom::read(self.ast, n))
            .collect::<Option<Vec<_>>>()?;
        let mut call = Call {
            statement,
            callee: *f,
            key: String::new(),
            total_read: false,
            args: atoms,
            origin_call: *expression,
            origin_arguments: if self.ast.nodes.tracks_origins() {
                args.clone()
            } else {
                Vec::new()
            },
        };
        self.normalize(&mut call);
        call.total_read = self.total_callee_read(call.callee);
        call.key = super::immutable_values::expression_key(self.ast, self.res, call.callee);
        Some(call)
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Arg {
    Constant(Atom),
    Column(usize),
    Lookup(usize, Vec<Atom>),
    Affine(usize, i64, i64),
    Series(predictors::Series),
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Codec {
    Rice {
        columns: Vec<rice::Column>,
        count: usize,
    },
    Counter,
    Table,
    Bytes(Vec<i64>),
    Delta {
        firsts: Vec<i64>,
        inner: Box<Codec>,
    },
    // A bounded mixed-radix integer encodes a whole row. All arithmetic stays
    // integer-exact; at most eight base-92 bytes keep values below 2^53 as well.
    GroupedBytes {
        biases: Vec<i64>,
        radices: Vec<i64>,
        width: usize,
        rows: usize,
    },
    PrefixBytes {
        biases: Vec<i64>,
        radices: Vec<i64>,
        width: usize,
        prefixes: usize,
    },
    PackedBytes {
        biases: Vec<i64>,
        radices: Vec<i64>,
        width: usize,
    },
}
impl Codec {
    fn record_width(&self, columns: usize) -> usize {
        match self {
            Self::Rice { .. } => 1,
            Self::PackedBytes { width, .. } | Self::GroupedBytes { width, .. } => *width,
            Self::Delta { inner, .. } => inner.record_width(columns),
            _ => columns,
        }
    }
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Shape {
    targets: Vec<usize>,
    args: Vec<Vec<Arg>>,
    columns: usize,
    codec: Codec,
    repeats: usize,
    translations: Vec<Vec<i64>>,
}
#[derive(Clone)]
enum Payload {
    Count(usize),
    Values(Vec<Atom>),
    Bytes(String),
}
impl Payload {
    fn emit(&self, ast: &mut Ast) -> NodeId {
        match self {
            Self::Count(n) => num(ast, *n as i64),
            Self::Values(values) => {
                let fields = values
                    .iter()
                    .map(|v| TableField::Arr(v.emit(ast)))
                    .collect();
                ast.push(Node::Table(fields))
            }
            Self::Bytes(s) => ast.push(Node::Str(quote_lua(s).into())),
        }
    }
    fn size(&self) -> usize {
        let mut ast = Ast::new();
        let n = self.emit(&mut ast);
        measure_expr(&ast, n)
    }
}
struct Plan {
    start: usize,
    stop: usize,
    helper: usize,
    targets: Vec<NodeId>,
    payload: Payload,
}
struct Helper {
    shape: Shape,
}
struct Proposal {
    stop: usize,
    shape: Shape,
    targets: Vec<NodeId>,
    payload: Payload,
    gain: usize,
}

fn num(ast: &mut Ast, n: i64) -> NodeId {
    ast.push(Node::Num(n.to_string().into()))
}
fn name(ast: &mut Ast, symbol: SymbolId) -> NodeId {
    ast.push(Node::Name(symbol))
}
fn offset(ast: &mut Ast, base: NodeId, value: i64) -> NodeId {
    if value == 0 {
        return base;
    }
    let magnitude = num(ast, value.abs());
    ast.push(Node::Bin(
        if value > 0 { "+" } else { "-" }.into(),
        base,
        magnitude,
    ))
}

fn proposals(
    calls: &[Call],
    period: usize,
    count: usize,
    enhanced: bool,
    dense: bool,
) -> Vec<(Shape, Vec<NodeId>, Payload)> {
    let mut keys = Vec::<String>::new();
    let mut targets = Vec::new();
    let mut pattern = Vec::new();
    let mut columns = Vec::<Vec<Atom>>::new();
    let mut arguments = Vec::new();
    for slot in 0..period {
        let target = if let Some(i) = keys.iter().position(|key| *key == calls[slot].key) {
            i
        } else {
            if slot > 0 && !calls[slot].total_read {
                return Vec::new();
            }
            keys.push(calls[slot].key.clone());
            targets.push(calls[slot].callee);
            keys.len() - 1
        };
        pattern.push(target);
        let mut args = Vec::new();
        for arg in 0..calls[slot].args.len() {
            let column = (0..count)
                .map(|r| calls[r * period + slot].args[arg].clone())
                .collect::<Vec<_>>();
            if column.iter().all(|v| *v == column[0]) {
                args.push(Arg::Constant(column[0].clone()));
            } else {
                let at = if let Some(i) = columns.iter().position(|c| *c == column) {
                    i
                } else {
                    columns.push(column);
                    columns.len() - 1
                };
                args.push(Arg::Column(at));
            }
        }
        arguments.push(args);
    }
    let shape = Shape {
        targets: pattern,
        args: arguments,
        columns: columns.len(),
        codec: Codec::Counter,
        repeats: 1,
        translations: Vec::new(),
    };
    let canonical = if enhanced && columns.len() < arguments_width(&shape) {
        let mut all = Vec::new();
        let mut canonical = shape.clone();
        for (slot, args) in canonical.args.iter_mut().enumerate() {
            for (arg, value) in args.iter_mut().enumerate() {
                *value = Arg::Column(all.len());
                all.push(
                    (0..count)
                        .map(|row| calls[row * period + slot].args[arg].clone())
                        .collect(),
                );
            }
        }
        canonical.columns = all.len();
        Some((canonical, all))
    } else {
        None
    };
    let projected = enhanced
        .then(|| project_columns(&shape, &columns))
        .flatten();
    let mut out = if dense {
        palette::proposals(&shape, &targets, &columns, count)
    } else {
        Vec::new()
    };
    out.extend(encode_columns(
        shape,
        targets.clone(),
        columns,
        count,
        enhanced,
        dense,
    ));
    if let Some((shape, columns)) = projected {
        out.extend(encode_columns(
            shape,
            targets.clone(),
            columns,
            count,
            enhanced,
            dense,
        ));
    }
    if let Some((shape, columns)) = canonical {
        out.extend(encode_columns(shape, targets, columns, count, false, false));
    }
    out
}

fn arguments_width(shape: &Shape) -> usize {
    shape.args.iter().map(Vec::len).sum()
}

fn project_columns(shape: &Shape, columns: &[Vec<Atom>]) -> Option<(Shape, Vec<Vec<Atom>>)> {
    let mut kept = Vec::<Vec<Atom>>::new();
    let mut integers = Vec::<Option<Vec<i64>>>::new();
    let mut mapping = Vec::new();
    for column in columns {
        let numbers = column.iter().map(Atom::integer).collect::<Option<Vec<_>>>();
        if let Some(series) = numbers.as_ref().and_then(|c| predictors::fit(c)) {
            mapping.push(Arg::Series(series));
            continue;
        }
        let related = numbers.as_ref().and_then(|c| {
            integers.iter().enumerate().find_map(|(i, old)| {
                let (scale, bias) = predictors::relation(old.as_ref()?, c)?;
                Some(Arg::Affine(i, scale, bias))
            })
        });
        if let Some(relation) = related {
            mapping.push(relation);
        } else {
            mapping.push(Arg::Column(kept.len()));
            kept.push(column.clone());
            integers.push(numbers);
        }
    }
    if kept.len() == columns.len() {
        return None;
    }
    let mut projected = shape.clone();
    projected.columns = kept.len();
    for arg in projected.args.iter_mut().flatten() {
        if let Arg::Column(i) = arg {
            *arg = mapping[*i].clone();
        }
    }
    Some((projected, kept))
}

fn encode_columns(
    mut shape: Shape,
    targets: Vec<NodeId>,
    columns: Vec<Vec<Atom>>,
    count: usize,
    enhanced: bool,
    dense: bool,
) -> Vec<(Shape, Vec<NodeId>, Payload)> {
    if columns.is_empty() {
        return vec![(shape, targets, Payload::Count(count))];
    }
    if columns.len() > MAX_COLUMNS {
        return Vec::new();
    }
    let mut out = Vec::new();
    // A sequence with nil holes has no defined #table border: leave it alone
    // unless nil is a constant column, which is passed directly in the helper.
    if columns.iter().flatten().all(|v| *v != Atom::Nil) {
        shape.codec = Codec::Table;
        let values = (0..count)
            .flat_map(|r| columns.iter().map(move |c| c[r].clone()))
            .collect();
        out.push((shape.clone(), targets.clone(), Payload::Values(values)));
    }
    let numbers = columns
        .iter()
        .map(|c| c.iter().map(Atom::integer).collect::<Option<Vec<_>>>())
        .collect::<Option<Vec<_>>>();
    if let Some(numbers) = numbers {
        if dense {
            if let Some((codec, bytes)) = rice::payload(&numbers, count) {
                let mut candidate = shape.clone();
                candidate.codec = codec;
                out.push((candidate, targets.clone(), Payload::Bytes(bytes)));
            }
        }
        if dense
            && !shape
                .args
                .iter()
                .flatten()
                .any(|a| matches!(a, Arg::Series(_)))
        {
            for (codec, bytes) in grouped::payloads(&numbers, count) {
                let mut candidate = shape.clone();
                candidate.codec = codec;
                out.push((candidate, targets.clone(), Payload::Bytes(bytes)));
            }
        }
        if enhanced && count >= 8 {
            // Seed with the first row and encode a zero first delta. Every
            // subsequent addition reconstructs an original bounded integer.
            // There is no approximate floating-point or wraparound transform.
            let deltas = numbers
                .iter()
                .map(|column| {
                    if column
                        .iter()
                        .any(|v| !(-1_000_000_000..=1_000_000_000).contains(v))
                    {
                        return None;
                    }
                    let mut values = vec![Atom::Num("0".into())];
                    for pair in column.windows(2) {
                        values.push(Atom::Num(pair[1].checked_sub(pair[0])?.to_string().into()));
                    }
                    Some(values)
                })
                .collect::<Option<Vec<_>>>();
            if let Some(deltas) = deltas {
                for (mut candidate, targets, payload) in
                    encode_columns(shape.clone(), targets.clone(), deltas, count, false, false)
                {
                    if matches!(candidate.codec, Codec::Bytes(_) | Codec::PackedBytes { .. }) {
                        candidate.codec = Codec::Delta {
                            firsts: numbers.iter().map(|c| c[0]).collect(),
                            inner: Box::new(candidate.codec),
                        };
                        out.push((candidate, targets, payload));
                    }
                }
            }
        }
        // Try whole-row packing independently of the one-byte-per-column
        // codec. Wider coordinates can still beat a decimal literal table.
        if enhanced {
            if let Some((codec @ Codec::PackedBytes { width: 7..=8, .. }, bytes)) =
                packed_payload_with_limit(&numbers, count, 8)
            {
                let mut packed_shape = shape.clone();
                packed_shape.codec = codec;
                out.push((packed_shape, targets.clone(), Payload::Bytes(bytes)));
            }
        }
        if let Some((codec, bytes)) = packed_payload(&numbers, count) {
            let mut packed_shape = shape.clone();
            packed_shape.codec = codec;
            out.push((packed_shape, targets.clone(), Payload::Bytes(bytes)));
        }
        let biases = numbers
            .iter()
            .map(|c| {
                let min = *c.iter().min()?;
                let max = *c.iter().max()?;
                if min < -1_000_000_000 || max > 1_000_000_000 || max - min > 91 {
                    return None;
                }
                Some(if min >= 0 && max <= 91 { 0 } else { min })
            })
            .collect::<Option<Vec<_>>>();
        if let Some(biases) = biases {
            let mut bytes = String::with_capacity(count * columns.len());
            for r in 0..count {
                for (c, bias) in numbers.iter().zip(&biases) {
                    bytes.push((c[r] - bias + 35) as u8 as char);
                }
            }
            shape.codec = Codec::Bytes(biases);
            out.push((shape, targets, Payload::Bytes(bytes)));
        }
    }
    out
}

fn packed_payload(columns: &[Vec<i64>], count: usize) -> Option<(Codec, String)> {
    packed_payload_with_limit(columns, count, 6)
}

fn packed_payload_with_limit(
    columns: &[Vec<i64>],
    count: usize,
    max_bytes: u32,
) -> Option<(Codec, String)> {
    debug_assert!(max_bytes <= 8);
    let max_cardinality = 92_i64.pow(max_bytes);
    let mut biases = Vec::with_capacity(columns.len());
    let mut radices = Vec::with_capacity(columns.len());
    let mut cardinality = 1i64;
    for column in columns {
        let min = *column.iter().min()?;
        let max = *column.iter().max()?;
        if min < -1_000_000_000 || max > 1_000_000_000 {
            return None;
        }
        let radix = max - min + 1;
        cardinality = cardinality.checked_mul(radix)?;
        if cardinality > max_cardinality {
            return None;
        }
        biases.push(min);
        radices.push(radix);
    }
    let mut width = 1;
    let mut capacity = 92i64;
    while capacity < cardinality {
        width += 1;
        capacity *= 92;
    }
    let mut bytes = String::with_capacity(count * width);
    for row in 0..count {
        let mut value = 0i64;
        for ((column, bias), radix) in columns.iter().zip(&biases).zip(&radices) {
            value = value * radix + column[row] - bias;
        }
        let mut divisor = capacity / 92;
        for _ in 0..width {
            bytes.push((value / divisor % 92 + 35) as u8 as char);
            divisor /= 92;
        }
    }
    Some((
        Codec::PackedBytes {
            biases,
            radices,
            width,
        },
        bytes,
    ))
}

fn copy_callee(source: &Ast, target: &mut Ast, node: NodeId) -> NodeId {
    // Admitted callees are names or statically resolved host member chains.
    // Copy their few nodes so resolver annotations never alias two scopes.
    let rewritten = match source.node(node).clone() {
        Node::Index(a, b, dot) => {
            let a = copy_callee(source, target, a);
            let b = copy_callee(source, target, b);
            Node::Index(a, b, dot)
        }
        Node::Paren(n) => Node::Paren(copy_callee(source, target, n)),
        other => other,
    };
    let copy = target.push(rewritten);
    target
        .nodes
        .derive_from(copy, &source.nodes, node, "draw-record-callee-copy");
    copy
}
fn fresh(ast: &mut Ast, taken: &mut HashSet<String>, stem: &str, serial: &mut usize) -> SymbolId {
    loop {
        let candidate = format!("__stormmin_{stem}_{}", *serial);
        *serial += 1;
        if taken.insert(candidate.clone()) {
            return ast.strings.intern(&candidate);
        }
    }
}
fn chunk_locals(ast: &Ast, node: NodeId) -> usize {
    if matches!(ast.node(node), Node::Function(..)) {
        return 0;
    }
    let own = match ast.node(node) {
        Node::Local(v, _) | Node::Forin(v, ..) => v.len(),
        Node::Localfunc(..) | Node::Fornum(..) => 1,
        _ => 0,
    };
    let mut children = 0;
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |c| {
        children += chunk_locals(ast, c)
    });
    own + children
}

pub fn pack_draw_records(ast: &mut Ast, root: NodeId, rename: bool) -> PassResult {
    let mut variants = record_variants(ast, root, rename);
    variants.sort_by_key(|(a, _)| measure_size(a, root));
    let (best, result) = variants.remove(0);
    *ast = best;
    result
}

// A bounded portfolio retains the old encoding strategy. Local greedy gains
// are not a proof that shared-helper selection yields a smaller whole program.
// The public pipeline compares both after its existing final-stage cleanup.
pub(crate) fn record_variants(ast: &Ast, root: NodeId, rename: bool) -> Vec<(Ast, PassResult)> {
    // Most onTick programs contain no eligible drawing input. Do not pay
    // for several resolvers, planners and final cleanups on that path.
    let mut has_screen = false;
    let mut literals = 0;
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |n| {
        has_screen |= matches!(ast.node(n), Node::Name(s) if ast.strings.get(*s) == "screen");
        if let Node::Callstat(c) = ast.node(n) {
            if let Node::Call(_, args, None) = ast.node(*c) {
                if args.len() <= 8 && args.iter().all(|&a| Atom::read(ast, a).is_some()) {
                    literals += 1;
                }
            }
        }
    });
    if !has_screen || literals < 4 {
        return vec![(
            ast.clone(),
            PassResult {
                root,
                saved: Some(0),
                details: None,
            },
        )];
    }
    let mut legacy = ast.clone();
    let result = pack_impl(&mut legacy, root, rename, false, false);
    let mut enhanced = ast.clone();
    let extra = pack_impl(&mut enhanced, root, rename, true, false);
    let mut out = vec![(legacy, result)];
    if enhanced.nodes != out[0].0.nodes || !enhanced.strings.same_symbols(&out[0].0.strings) {
        out.push((enhanced, extra));
    }
    let mut regular = ast.clone();
    let loops = regular::synthesize(&mut regular, root, rename);
    let dense_regular = (loops > 0).then(|| regular.clone());
    if loops > 0 {
        let mut result = pack_impl(&mut regular, root, rename, true, false);
        result.saved =
            Some(measure_size(ast, root).saturating_sub(measure_size(&regular, root)) as u64);
        result
            .details
            .get_or_insert_with(Vec::new)
            .push(format!("exact_inline_loops={loops}"));
        out.push((regular, result));
    }
    let mut dense = ast.clone();
    let result = pack_impl(&mut dense, root, rename, true, true);
    if !out
        .iter()
        .any(|(a, _)| a.nodes == dense.nodes && a.strings.same_symbols(&dense.strings))
    {
        out.push((dense, result));
    }
    if let Some(mut dense_regular) = dense_regular {
        let mut result = pack_impl(&mut dense_regular, root, rename, true, true);
        result.saved =
            Some(measure_size(ast, root).saturating_sub(measure_size(&dense_regular, root)) as u64);
        if !out.iter().any(|(a, _)| {
            a.nodes == dense_regular.nodes && a.strings.same_symbols(&dense_regular.strings)
        }) {
            out.push((dense_regular, result));
        }
    }
    let mut shared = ast.clone();
    let dictionaries = shared::synthesize(&mut shared, root, rename);
    if dictionaries > 0 {
        let mut result = pack_impl(&mut shared, root, rename, true, true);
        result.saved =
            Some(measure_size(ast, root).saturating_sub(measure_size(&shared, root)) as u64);
        result
            .details
            .get_or_insert_with(Vec::new)
            .push(format!("shared_pair_decoders={dictionaries}"));
        out.push((shared, result));
    }
    out
}

fn pack_impl(ast: &mut Ast, root: NodeId, rename: bool, enhanced: bool, dense: bool) -> PassResult {
    let unchanged = || PassResult {
        root,
        saved: Some(0),
        details: None,
    };
    let mut potential = 0;
    let mut jumps = false;
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |n| {
        jumps |= matches!(ast.node(n), Node::Goto(_) | Node::Label(_));
        if let Node::Callstat(c) = ast.node(n) {
            if let Node::Call(_, args, None) = ast.node(*c) {
                if args.len() <= 8 && args.iter().all(|&n| Atom::read(ast, n).is_some()) {
                    potential += 1;
                }
            }
        }
    });
    if potential < 4 || jumps || !super::closed_fields::has_closed_key_space(ast, root) {
        return unchanged();
    }
    let source = ast.clone();
    let res = resolve(&source, root);
    let classifier = Classifier::new(&source, &res, root);
    let mut blocks = Vec::new();
    storm_lua_syntax::ast_utils::walk(&source, root, &mut |n| {
        if matches!(source.node(n), Node::Block(_)) {
            blocks.push(n)
        }
    });
    let mut helpers = Vec::<Helper>::new();
    let mut by_shape = BTreeMap::<Shape, usize>::new();
    let mut costs = BTreeMap::<Shape, usize>::new();
    let mut plans = BTreeMap::<NodeId, Vec<Plan>>::new();
    let helper_budget = MAX_HELPERS.min(175usize.saturating_sub(chunk_locals(&source, root)));
    if helper_budget == 0 {
        return unchanged();
    }
    for block in blocks {
        let Node::Block(stmts) = source.node(block) else {
            unreachable!()
        };
        let mut cursor = 0;
        while cursor < stmts.len() {
            let mut calls = Vec::new();
            for &stmt in &stmts[cursor..] {
                if let Some(call) = classifier.call(stmt) {
                    calls.push(call)
                } else {
                    break;
                }
            }
            if calls.len() < 4 {
                cursor += calls.len().max(1);
                continue;
            }
            let cluster_start = cursor;
            let mut at = 0;
            while at < calls.len() {
                let mut best = None::<Proposal>;
                for period in 1..=MAX_PERIOD.min((calls.len() - at) / 2) {
                    let slice = &calls[at..];
                    let mut count = 1;
                    while (count + 1) * period <= slice.len()
                        && (0..period).all(|p| {
                            slice[p].key == slice[count * period + p].key
                                && slice[p].args.len() == slice[count * period + p].args.len()
                        })
                    {
                        count += 1;
                    }
                    if count < 2 || count * period < 4 {
                        continue;
                    }
                    let mut widths = vec![count, count.min(64), count.min(16), count.min(4)];
                    widths.sort_unstable();
                    widths.dedup();
                    for count in widths {
                        if count < 2 || count * period < 4 {
                            continue;
                        }
                        let old = slice[..count * period]
                            .iter()
                            .map(|c| measure_stmt(&source, c.statement))
                            .sum::<usize>();
                        for (shape, targets, payload) in
                            proposals(slice, period, count, enhanced, dense)
                        {
                            let exists = by_shape.contains_key(&shape);
                            if !exists && helpers.len() >= helper_budget {
                                continue;
                            }
                            let definition = if exists {
                                0
                            } else {
                                *costs
                                    .entry(shape.clone())
                                    .or_insert_with(|| helper_cost(&shape))
                                    + 1
                            };
                            let call_size = 3
                                + payload.size()
                                + targets
                                    .iter()
                                    .map(|&n| measure_expr(&source, n) + 1)
                                    .sum::<usize>();
                            let cost = definition + call_size;
                            if cost >= old {
                                continue;
                            }
                            let gain = old - cost;
                            if best.as_ref().is_none_or(|b| gain > b.gain) {
                                best = Some(Proposal {
                                    stop: at + count * period,
                                    shape,
                                    targets,
                                    payload,
                                    gain,
                                });
                            }
                        }
                    }
                }
                if enhanced {
                    for (stop, shape, targets, payload) in
                        translated::proposals(&calls[at..], dense)
                    {
                        let exists = by_shape.contains_key(&shape);
                        if !exists && helpers.len() >= helper_budget {
                            continue;
                        }
                        let old = calls[at..at + stop]
                            .iter()
                            .map(|c| measure_stmt(&source, c.statement))
                            .sum::<usize>();
                        let definition = if exists {
                            0
                        } else {
                            *costs
                                .entry(shape.clone())
                                .or_insert_with(|| helper_cost(&shape))
                                + 1
                        };
                        let call_size = 3
                            + payload.size()
                            + targets
                                .iter()
                                .map(|&n| measure_expr(&source, n) + 1)
                                .sum::<usize>();
                        let cost = definition + call_size;
                        if cost < old && best.as_ref().is_none_or(|b| old - cost > b.gain) {
                            best = Some(Proposal {
                                stop: at + stop,
                                shape,
                                targets,
                                payload,
                                gain: old - cost,
                            });
                        }
                    }
                }
                if let Some(best) = best {
                    let helper = if let Some(&id) = by_shape.get(&best.shape) {
                        id
                    } else {
                        let id = helpers.len();
                        by_shape.insert(best.shape.clone(), id);
                        helpers.push(Helper { shape: best.shape });
                        id
                    };
                    plans.entry(block).or_default().push(Plan {
                        start: cluster_start + at,
                        stop: cluster_start + best.stop,
                        helper,
                        targets: best.targets,
                        payload: best.payload,
                    });
                    at = best.stop;
                } else {
                    at += 1;
                }
            }
            cursor = cluster_start + calls.len();
        }
    }
    if plans.is_empty() {
        return unchanged();
    }
    let mut inherited = vec![0usize; res.scopes.len()];
    for scope in &res.scopes {
        inherited[scope.id as usize] = scope.parent.map_or(0, |p| inherited[p as usize])
            + scope
                .bindings
                .iter()
                .filter(|&&b| res.binding(b).kind != BindingKind::Global)
                .count();
    }
    // Include a pessimistic allowance for pooled payloads below.
    if inherited.into_iter().max().unwrap_or(0) + helpers.len() + 24 > 245 {
        return unchanged();
    }
    let mut target = source.clone();
    let mut taken = source
        .strings
        .all_strings()
        .into_iter()
        .collect::<HashSet<_>>();
    let mut serial = 0;
    let helper_names = helpers
        .iter()
        .map(|_| fresh(&mut target, &mut taken, "draw_replay", &mut serial))
        .collect::<Vec<_>>();
    let mut definitions = Vec::new();
    for (i, helper) in helpers.iter().enumerate() {
        definitions.push(emit_helper(
            &mut target,
            &helper.shape,
            helper_names[i],
            serial + i,
        ));
    }
    // Repeated byte payloads (e.g. the common background of nine cards) share
    // one immutable string. No table allocation or identity is shared.
    let mut payload_counts = BTreeMap::<String, usize>::new();
    for plan in plans.values().flatten() {
        if let Payload::Bytes(s) = &plan.payload {
            *payload_counts.entry(s.clone()).or_default() += 1;
        }
    }
    let mut payload_symbols = BTreeMap::new();
    let mut symbols = Vec::new();
    let mut values = Vec::new();
    for (bytes, count) in payload_counts {
        if symbols.len() >= 24 || chunk_locals(&source, root) + helpers.len() + symbols.len() >= 180
        {
            break;
        }
        let size = Payload::Bytes(bytes.clone()).size();
        if count < 2 || count * (size.saturating_sub(2)) <= size + 10 {
            continue;
        }
        let symbol = fresh(&mut target, &mut taken, "draw_data", &mut serial);
        symbols.push(symbol);
        let payload = Payload::Bytes(bytes.clone()).emit(&mut target);
        if dense {
            dictionary::compress_literal(&mut target, payload);
        }
        values.push(payload);
        payload_symbols.insert(bytes, symbol);
    }
    if !symbols.is_empty() {
        definitions.push(target.push(Node::Local(symbols, values)));
    }
    let mut batched = 0;
    for (block, runs) in plans {
        let Node::Block(old) = source.node(block) else {
            unreachable!()
        };
        let mut stmts = Vec::new();
        let mut cursor = 0;
        for plan in runs {
            stmts.extend_from_slice(&old[cursor..plan.start]);
            batched += plan.stop - plan.start;
            let mut args = plan
                .targets
                .iter()
                .map(|&n| copy_callee(&source, &mut target, n))
                .collect::<Vec<_>>();
            let payload = if let Payload::Bytes(s) = &plan.payload {
                payload_symbols
                    .get(s)
                    .copied()
                    .map(|s| name(&mut target, s))
            } else {
                None
            };
            let emitted = payload.unwrap_or_else(|| plan.payload.emit(&mut target));
            if dense && payload.is_none() && matches!(plan.payload, Payload::Bytes(_)) {
                dictionary::compress_literal(&mut target, emitted);
            }
            args.push(emitted);
            let f = name(&mut target, helper_names[plan.helper]);
            let c = target.push(Node::Call(f, args, None));
            stmts.push(target.push(Node::Callstat(c)));
            cursor = plan.stop;
        }
        stmts.extend_from_slice(&old[cursor..]);
        target.nodes[block as usize] = Node::Block(stmts);
    }
    if let Node::Block(stmts) = &mut target.nodes[root as usize] {
        definitions.append(stmts);
        *stmts = definitions;
    } else {
        return unchanged();
    }
    remove_unused_forwarders(&mut target, root);
    if rename {
        target = scope_rename_fast(&target, root).ast;
    }
    let before = measure_size(&source, root);
    let after = measure_size(&target, root);
    if after >= before {
        return unchanged();
    }
    *ast = target;
    PassResult {
        root,
        saved: Some((before - after) as u64),
        details: Some(vec![format!(
            "calls={batched};helpers={};shared_payloads={}",
            helpers.len(),
            payload_symbols.len()
        )]),
    }
}

/// Literal drawing sites safe to move into a root helper without captures.
/// Uses the resolver's binding identity, never compact printed text.
pub(crate) fn global_literal_draw_calls(
    ast: &Ast,
    res: &Resolution,
    root: NodeId,
) -> HashMap<NodeId, String> {
    let classifier = Classifier::new(ast, res, root);
    fn global(ast: &Ast, res: &Resolution, n: NodeId) -> bool {
        match ast.node(n) {
            Node::Name(_) => {
                res.node_bid[n as usize].is_some_and(|b| res.binding(b).kind == BindingKind::Global)
            }
            Node::Paren(n) => global(ast, res, *n),
            Node::Index(a, b, _) => global(ast, res, *a) && matches!(ast.node(*b), Node::Str(_)),
            _ => false,
        }
    }
    let mut out = HashMap::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |n| {
        if let Node::Callstat(call) = ast.node(n) {
            if let Node::Call(f, args, None) = ast.node(*call) {
                if classifier.is_draw(*f)
                    && global(ast, res, *f)
                    && args.iter().all(|&a| Atom::read(ast, a).is_some())
                {
                    out.insert(n, super::immutable_values::expression_key(ast, res, *call));
                }
            }
        }
    });
    out
}

// Literal batching can remove every H/V call. Remove only now-unread root
// drawing wrappers; function values that escape or are compared count as reads.
// A root function declaration has no initializer effects other than allocation.
fn remove_unused_forwarders(ast: &mut Ast, root: NodeId) {
    loop {
        let res = resolve(ast, root);
        let classifier = Classifier::new(ast, &res, root);
        let mut reads = HashSet::new();
        storm_lua_syntax::ast_utils::walk(ast, root, &mut |n| {
            if matches!(ast.node(n), Node::Name(_)) && !res.node_write[n as usize] {
                if let Some(b) = res.node_bid[n as usize] {
                    reads.insert(b);
                }
            }
        });
        let Node::Block(stmts) = ast.node(root) else {
            return;
        };
        let kept = stmts
            .iter()
            .copied()
            .filter(|&n| {
                let b = match ast.node(n) {
                    Node::Funcstat(target, _) => res.node_bid[*target as usize],
                    Node::Localfunc(..) => res.node_bid[n as usize],
                    _ => None,
                };
                !b.is_some_and(|b| {
                    classifier.wrappers.contains(&b) && !res.binding(b).fixed && !reads.contains(&b)
                })
            })
            .collect::<Vec<_>>();
        if kept.len() == stmts.len() {
            return;
        }
        ast.nodes
            .rewrite(root, Node::Block(kept), "unused-draw-forwarder-removal");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod encoding_tests {
    use super::*;

    #[test]
    fn packed_radix_capacity_boundaries_roundtrip_without_overflow() {
        for range in [1i64, 2, 31, 32, 91, 92, 93, 1000, 1_000_001] {
            for columns in 1..=8 {
                let input = (0..columns)
                    .map(|n| vec![-100 + n, -101 + n + range, -100 + n + range / 2])
                    .collect::<Vec<_>>();
                let result = packed_payload_with_limit(&input, 3, 8);
                if range
                    .checked_pow(columns as u32)
                    .is_none_or(|size| size > 92i64.pow(8))
                {
                    assert!(result.is_none());
                    continue;
                }
                let (
                    Codec::PackedBytes {
                        biases,
                        radices,
                        width,
                    },
                    bytes,
                ) = result.unwrap()
                else {
                    panic!("wrong codec")
                };
                assert_eq!(bytes.len(), 3 * width);
                for (row, encoded) in bytes.as_bytes().chunks_exact(width).enumerate() {
                    let mut decoded = encoded
                        .iter()
                        .fold(0i64, |acc, byte| acc * 92 + i64::from(*byte) - 35);
                    for col in (0..input.len()).rev() {
                        assert_eq!(decoded % radices[col] + biases[col], input[col][row]);
                        decoded /= radices[col];
                    }
                    assert_eq!(decoded, 0);
                }
            }
        }
        assert!(packed_payload(&[vec![i64::MIN, i64::MAX]], 2).is_none());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod round3_tests;
