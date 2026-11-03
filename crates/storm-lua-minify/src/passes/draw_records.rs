//! Ordered literal draw records, including simple drawing-wrapper calls.
//!
//! A batch replays precisely the original calls in precisely the original order.
//! We do not merge rectangles, reorder colors, round coordinates, or assume a
//! rasterizer. Only scalar literal arguments are stored ahead of the calls.
//! Callees are passed to a private replay helper; eligibility proves they cannot
//! be rebound by that replay. Simple root-level forwarding wrappers (H/V -> R)
//! can be normalized without expanding R's dynamic coordinate transformations.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use crate::pass::PassResult;
use crate::scope_rename::measure_renamed_size;
use crate::scope_rename::scope_rename_fast;
use storm_lua_analysis::resolver::{resolve, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId, TableField};
use storm_lua_syntax::numeric::{
    integer_literal_value, qstr, qstr_len, quote_lua, quoted_bytes_len, PrintableFeatures,
};
use storm_lua_syntax::print::render_num;
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

use encoding::helper_cost;

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

#[derive(Clone)]
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

/// Facts about one input AST derived from its content and resolution
/// alone: API aliases, drawing wrappers, lexical order and definition
/// sites. The data owns no reference to the AST or the resolution, so it
/// can be stored next to the input it was derived from (see
/// `InputAnalysis`) without threading borrows through every consumer.
struct ClassifierData {
    aliases: HashMap<BindingId, String>,
    wrappers: BTreeSet<BindingId>,
    order: HashMap<NodeId, usize>,
    definitions: HashMap<NodeId, usize>,
}

impl ClassifierData {
    fn build(ast: &Ast, res: &Resolution, root: NodeId) -> Self {
        let mut out = Self {
            aliases: super::closed_fields::api_aliases(ast, res, root),
            wrappers: BTreeSet::new(),
            order: HashMap::new(),
            definitions: HashMap::new(),
        };
        fn visit(ast: &Ast, data: &mut ClassifierData, node: NodeId, top: bool) {
            let index = data.order.len();
            data.order.entry(node).or_insert(index);
            if top {
                if let Node::Funcstat(_, f) | Node::Localfunc(_, f) = ast.node(node) {
                    data.definitions.entry(*f).or_insert(index);
                }
            }
            match ast.node(node) {
                Node::Block(stmts) => {
                    for &s in stmts {
                        visit(ast, data, s, top);
                    }
                }
                Node::Do(b) => visit(ast, data, *b, top),
                _ => storm_lua_analysis::resolver::for_each_child(ast, node, &mut |n| {
                    visit(ast, data, n, false)
                }),
            }
        }
        visit(ast, &mut out, root, true);
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
                if out.is_draw(ast, res, *f) {
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

    fn is_draw(&self, ast: &Ast, res: &Resolution, node: NodeId) -> bool {
        if super::closed_fields::builtin_reference(ast, res, &self.aliases, node)
            .is_some_and(|name| draw_builtin(&name))
        {
            return true;
        }
        matches!(ast.node(node), Node::Name(_))
            && res.node_bid[node as usize].is_some_and(|b| self.wrappers.contains(&b))
    }
    fn total_callee_read(&self, ast: &Ast, res: &Resolution, node: NodeId) -> bool {
        match ast.node(node) {
            Node::Name(_) => true,
            Node::Paren(inner) => self.total_callee_read(ast, res, *inner),
            Node::Index(object, _, _) => {
                // Only the immutable host screen itself is unconditionally
                // initialized. S=screen may occur after this callback or not at
                // all. The closed-key-space gate excludes mutation/reflection.
                matches!(ast.node(*object), Node::Name(_))
                    && res.node_bid[*object as usize].is_some_and(|b| {
                        let binding = res.binding(b);
                        binding.kind == BindingKind::Global
                            && ast.strings.get(binding.name) == "screen"
                            && res.binding_write_counts[b as usize] == 0
                    })
            }
            _ => false,
        }
    }
    fn same_visible_binding(&self, res: &Resolution, target: BindingId, at: NodeId) -> bool {
        let binding = res.binding(target);
        let mut scope = res.node_scope_id[at as usize];
        while let Some(s) = scope {
            let current = res.scope(s);
            let candidates = current
                .bindings
                .iter()
                .copied()
                .filter(|&b| res.binding(b).name == binding.name)
                .collect::<Vec<_>>();
            if !candidates.is_empty() {
                return candidates.iter().all(|&b| b == target);
            }
            scope = current.parent;
        }
        binding.kind == BindingKind::Global
    }
    fn normalize(&self, ast: &Ast, res: &Resolution, call: &mut Call) {
        let mut seen = HashSet::new();
        loop {
            if !matches!(ast.node(call.callee), Node::Name(_)) {
                break;
            }
            let Some(b) = res.node_bid[call.callee as usize] else {
                break;
            };
            if !self.wrappers.contains(&b) || !seen.insert(b) {
                break;
            }
            let binding = res.binding(b);
            let Some(function) = binding.function_node else {
                break;
            };
            // Root declarations must precede this call lexically. Functions
            // declared conditionally or inside a callback are not forwarded.
            if self
                .definitions
                .get(&function)
                .is_none_or(|&d| d >= self.order[&call.statement])
            {
                break;
            }
            let Some(inner) = wrapper_call(ast, function) else {
                break;
            };
            let Node::Call(target, args, None) = ast.node(inner) else {
                break;
            };
            if !matches!(ast.node(*target), Node::Name(_)) {
                break;
            }
            let Some(target_bid) = res.node_bid[*target as usize] else {
                break;
            };
            // Stop at the terminal drawing wrapper; do not expand its dynamic
            // expressions (rotation, scaling, scrolling) or a host API access.
            if !self.wrappers.contains(&target_bid)
                || !self.same_visible_binding(res, target_bid, call.statement)
            {
                break;
            }
            let Node::Function(params, false, _) = ast.node(function) else {
                break;
            };
            if call.args.len() != params.len() {
                break;
            }
            let param_bids = &res.node_bids[function as usize];
            let mut mapped = Vec::new();
            let mut origin_arguments = Vec::new();
            for &arg in args {
                if let Some(atom) = Atom::read(ast, arg) {
                    mapped.push(atom);
                    if ast.nodes.tracks_origins() {
                        origin_arguments.push(arg);
                    }
                } else if matches!(ast.node(arg), Node::Name(_)) {
                    let pos = res.node_bid[arg as usize]
                        .and_then(|b| param_bids.iter().position(|&p| p == b));
                    let Some(pos) = pos else { return };
                    let Some(value) = call.args.get(pos) else {
                        return;
                    };
                    mapped.push(value.clone());
                    if ast.nodes.tracks_origins() {
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
    fn call(&self, ast: &Ast, res: &Resolution, statement: NodeId) -> Option<Call> {
        let Node::Callstat(expression) = ast.node(statement) else {
            return None;
        };
        let Node::Call(f, args, None) = ast.node(*expression) else {
            return None;
        };
        if args.len() > 8 || !self.is_draw(ast, res, *f) {
            return None;
        }
        let atoms = args
            .iter()
            .map(|&n| Atom::read(ast, n))
            .collect::<Option<Vec<_>>>()?;
        let mut call = Call {
            statement,
            callee: *f,
            key: String::new(),
            total_read: false,
            args: atoms,
            origin_call: *expression,
            origin_arguments: if ast.nodes.tracks_origins() {
                args.clone()
            } else {
                Vec::new()
            },
        };
        self.normalize(ast, res, &mut call);
        call.total_read = self.total_callee_read(ast, res, call.callee);
        call.key = super::immutable_values::expression_key(ast, res, call.callee);
        Some(call)
    }
}

/// Borrowing view for the standalone consumers that already hold an AST
/// and its resolution (global literal draw calls, forwarder removal, and
/// the classifier tests). The shared setup path goes through
/// `SharedInput`, which owns the input and its `InputAnalysis`.
struct Classifier<'a> {
    ast: &'a Ast,
    res: &'a Resolution,
    data: ClassifierData,
}

impl<'a> Classifier<'a> {
    fn new(ast: &'a Ast, res: &'a Resolution, root: NodeId) -> Self {
        Self {
            ast,
            res,
            data: ClassifierData::build(ast, res, root),
        }
    }
    fn is_draw(&self, node: NodeId) -> bool {
        self.data.is_draw(self.ast, self.res, node)
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
    /// Table payload kept in its source columns, with its precomputed
    /// `size()`: the encoder sums the per-column lengths (see
    /// `column_lens`); the total equals the direct scan in `values_size`
    /// of the flattened values because the summands are the same and
    /// addition commutes. Most generated table candidates are priced
    /// and discarded, so the row-major flattening is built only if the
    /// candidate is adopted (in `emit`), which reproduces the exact
    /// field order.
    Columns(Vec<Vec<Atom>>, usize, usize),
    Bytes(BytesPayload),
}
impl Payload {
    fn emit(&self, ast: &mut Ast) -> NodeId {
        match self {
            Self::Count(n) => num(ast, *n as i64),
            Self::Columns(columns, count, _) => {
                let fields = (0..*count)
                    .flat_map(|r| columns.iter().map(move |c| &c[r]))
                    .map(|v| TableField::Arr(v.emit(ast)))
                    .collect();
                ast.push(Node::Table(fields))
            }
            Self::Bytes(b) => ast.push(Node::Str(quote_lua(&*b.text()).into())),
        }
    }
    /// A table payload measured by the direct scan (`values_size`):
    /// one single-row column per value, whose row-major flattening is
    /// exactly `values`.
    #[cfg(test)]
    fn values(values: Vec<Atom>) -> Self {
        let size = values_size(&values);
        let columns = values.into_iter().map(|v| vec![v]).collect();
        Self::Columns(columns, 1, size)
    }
    fn size(&self) -> usize {
        // Direct length computation: no fresh Ast is built and no measure
        // traversal runs. Each arm mirrors exactly what measure_expr
        // computes for the node emit() would have produced.
        match self {
            // emit() casts to i64 first (see num()), so keep that cast:
            // for counts above i64::MAX the emitted literal is negative.
            Self::Count(n) => render_num(&(*n as i64).to_string()).len(),
            Self::Columns(_, _, size) => *size,
            // The emitted node is Str(quote_lua(s)); measure_expr on it
            // is qstr_shape(quote_lua(s)).0. quoted_bytes_len computes
            // exactly that length from the payload itself, so neither
            // the quote_lua string nor its decode round trip is built
            // here; quote_lua is still built where emit() needs it.
            Self::Bytes(b) => b.size(),
        }
    }

    /// The payload's byte string, when it is a byte payload: the
    /// plan-stage sharing of repeated payloads is the one pricing-side
    /// consumer that needs the text, and asking for it is an
    /// adoption-side operation (a recipe builds its string here).
    fn bytes_text(&self) -> Option<Cow<'_, str>> {
        match self {
            Self::Bytes(b) => Some(b.text()),
            _ => None,
        }
    }
}

/// A byte-string payload: the encoded form of a numeric column set.
///
/// Most generated byte candidates are priced by size and discarded, so
/// codec payloads are kept as their generation recipe: the rendered
/// size comes from the byte features accumulated while the recipe was
/// built, and the string itself is materialized only when the
/// candidate is adopted (`emit`, or the plan-stage payload sharing via
/// `Payload::bytes_text`). Literal strings cover producers without a
/// codec recipe and keep the exact previous behavior.
#[derive(Clone)]
enum BytesPayload {
    Literal(String),
    Recipe(BytesRecipe),
}

impl BytesPayload {
    fn size(&self) -> usize {
        match self {
            Self::Literal(s) => quoted_bytes_len(s.as_bytes()),
            Self::Recipe(recipe) => recipe.size(),
        }
    }

    fn text(&self) -> Cow<'_, str> {
        match self {
            Self::Literal(s) => Cow::Borrowed(s),
            Self::Recipe(recipe) => Cow::Owned(recipe.build()),
        }
    }
}

/// The generation recipe of one codec byte payload: the ingredients
/// needed to rebuild the exact byte string, plus the
/// [`PrintableFeatures`] of those bytes, accumulated by the same byte
/// walk that builds the string, so the reported size cannot diverge
/// from the built bytes.
///
/// The integer source is shared by every candidate one encoder call
/// derives from the same column set, so a recipe never copies the
/// integers it encodes. The ingredients are private and no operation
/// combines a recipe with another input's columns: pricing code can
/// only ask for the size, and the text is reachable only through the
/// adoption-side operations on `BytesPayload`.
#[derive(Clone)]
enum BytesRecipe {
    Bias {
        source: Arc<Vec<Vec<i64>>>,
        biases: Vec<i64>,
        count: usize,
        features: PrintableFeatures,
    },
    Packed {
        source: Arc<Vec<Vec<i64>>>,
        biases: Vec<i64>,
        radices: Vec<i64>,
        width: usize,
        count: usize,
        features: PrintableFeatures,
    },
    Grouped {
        source: Arc<Vec<Vec<i64>>>,
        biases: Vec<i64>,
        radices: Vec<i64>,
        width: usize,
        rows: usize,
        count: usize,
        features: PrintableFeatures,
    },
    Fixed {
        source: Arc<Vec<Vec<i64>>>,
        biases: Vec<i64>,
        radices: Vec<i64>,
        width: usize,
        prefixes: Option<usize>,
        count: usize,
        features: PrintableFeatures,
    },
    Rice {
        specs: Vec<rice::Column>,
        values: Vec<Vec<i64>>,
        count: usize,
        features: PrintableFeatures,
    },
}

impl BytesRecipe {
    fn features(&self) -> &PrintableFeatures {
        match self {
            Self::Bias { features, .. }
            | Self::Packed { features, .. }
            | Self::Grouped { features, .. }
            | Self::Fixed { features, .. }
            | Self::Rice { features, .. } => features,
        }
    }

    /// The rendered size of the payload string. The feature aggregates
    /// determine it for every realistic byte string; the pathological
    /// case (at least 65 distinct long-bracket patterns blocked) falls
    /// back to building the string and measuring it, which stays exact.
    fn size(&self) -> usize {
        self.features()
            .quoted_len()
            .unwrap_or_else(|| quoted_bytes_len(self.build().as_bytes()))
    }

    /// Build the exact byte string the original constructor produced.
    /// Each arm replays the codec's single byte walk with a string
    /// sink — the same walk the features were accumulated with.
    #[expect(
        clippy::expect_used,
        reason = "The fixed-width walk succeeded on these same ingredients when the recipe was constructed, so re-walking them cannot fail"
    )]
    fn build(&self) -> String {
        let mut out = String::with_capacity(self.features().len());
        match self {
            Self::Bias {
                source,
                biases,
                count,
                ..
            } => walk_bias_bytes(source, biases, *count, &mut |b| out.push(b as char)),
            Self::Packed {
                source,
                biases,
                radices,
                width,
                count,
                ..
            } => walk_packed_bytes(source, biases, radices, *width, *count, &mut |b| {
                out.push(b as char)
            }),
            Self::Grouped {
                source,
                biases,
                radices,
                width,
                rows,
                count,
                ..
            } => grouped::walk_grouped_bytes(
                source,
                biases,
                radices,
                *width,
                *rows,
                radices.iter().product(),
                *count,
                &mut |b| out.push(b as char),
            ),
            Self::Fixed {
                source,
                biases,
                radices,
                width,
                prefixes,
                count,
                ..
            } => {
                // The walk succeeded on these same ingredients when
                // the recipe was constructed, so it cannot fail here.
                shared::walk_fixed_bytes(
                    biases,
                    radices,
                    *width,
                    *prefixes,
                    source,
                    *count,
                    &mut |b| out.push(b as char),
                )
                .expect("recipe ingredients re-walk");
            }
            Self::Rice {
                specs,
                values,
                count,
                ..
            } => rice::walk_rice_bytes(values, specs, *count, &mut |b| out.push(b as char)),
        }
        out
    }
}

/// The biased one-byte-per-column byte walk, row-major: one byte per
/// value, `(value - bias + 35)`. This is the bias codec's only byte
/// generation; building the payload string and accumulating its size
/// features are two sinks over the walk, so they cannot diverge.
fn walk_bias_bytes(columns: &[Vec<i64>], biases: &[i64], count: usize, sink: &mut impl FnMut(u8)) {
    for r in 0..count {
        for (c, bias) in columns.iter().zip(biases) {
            sink((c[r] - bias + 35) as u8);
        }
    }
}

/// The whole-row packing byte walk: each row's values compose into
/// one mixed-radix number, emitted as `width` base-92 digits offset by
/// 35. The packed codec's only byte generation (see `walk_bias_bytes`
/// for the sink discipline).
fn walk_packed_bytes(
    columns: &[Vec<i64>],
    biases: &[i64],
    radices: &[i64],
    width: usize,
    count: usize,
    sink: &mut impl FnMut(u8),
) {
    let capacity = 92_i64.pow(width as u32);
    for row in 0..count {
        let mut value = 0i64;
        for ((column, bias), radix) in columns.iter().zip(biases).zip(radices) {
            value = value * radix + column[row] - bias;
        }
        let mut divisor = capacity / 92;
        for _ in 0..width {
            sink((value / divisor % 92 + 35) as u8);
            divisor /= 92;
        }
    }
}

/// The biased one-byte-per-column form, as a recipe: a column's bias
/// is 0 when the column already fits 0..=91 and its minimum otherwise.
/// Returns the biases (for the codec) and the recipe, or None when any
/// column escapes the +-1e9 window or spans more than 92 values.
fn bias_recipe(source: &Arc<Vec<Vec<i64>>>, count: usize) -> Option<(Vec<i64>, BytesRecipe)> {
    let biases = source
        .iter()
        .map(|c| {
            let min = *c.iter().min()?;
            let max = *c.iter().max()?;
            if min < -1_000_000_000 || max > 1_000_000_000 || max - min > 91 {
                return None;
            }
            Some(if min >= 0 && max <= 91 { 0 } else { min })
        })
        .collect::<Option<Vec<_>>>()?;
    let mut features = PrintableFeatures::new();
    walk_bias_bytes(source, &biases, count, &mut |b| features.push(b));
    let recipe = BytesRecipe::Bias {
        source: source.clone(),
        biases: biases.clone(),
        count,
        features,
    };
    Some((biases, recipe))
}

/// The recipe for one computed packed layout.
fn packed_recipe_from_layout(
    source: &Arc<Vec<Vec<i64>>>,
    layout: &PackedLayout,
    count: usize,
) -> BytesRecipe {
    let mut features = PrintableFeatures::new();
    walk_packed_bytes(
        source,
        &layout.biases,
        &layout.radices,
        layout.width,
        count,
        &mut |b| features.push(b),
    );
    BytesRecipe::Packed {
        source: source.clone(),
        biases: layout.biases.clone(),
        radices: layout.radices.clone(),
        width: layout.width,
        count,
        features,
    }
}

/// The whole-row packing as codec + recipe, under a byte-width limit.
fn packed_recipe(
    source: &Arc<Vec<Vec<i64>>>,
    count: usize,
    max_bytes: u32,
) -> Option<(Codec, BytesRecipe)> {
    let layout = packed_layout(source, max_bytes)?;
    let recipe = packed_recipe_from_layout(source, &layout, count);
    Some((packed_codec(layout), recipe))
}

/// Length of a table payload's values as emitted fields: "{" + fields
/// joined by "," + "}". measure_expr on a Table does exactly this
/// (join_comma_segs adds one comma per field after the first; no
/// separator is added next to the braces). Each field contributes
/// `atom_len(atom, 0)`, a pure function of the atom, so a payload
/// flattened from columns measures the same as the sum of the
/// per-column lengths (see `column_lens`).
#[cfg(test)]
fn values_size(values: &[Atom]) -> usize {
    let mut total = 2;
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            total += 1;
        }
        total += atom_len(value, 0);
    }
    total
}

/// Length of an Atom as an expression, mirroring size::expr_seg for the
/// Atom subset only. `parent` is the size-model precedence context
/// (0 at the top level / in a table field, 12 inside a unary minus).
fn atom_len(atom: &Atom, parent: u8) -> usize {
    // At precedence 15 expr_seg wraps every Atom form (none is
    // Name/Paren/Index/Call) in "(" ... ")".
    if parent == 15 {
        return atom_len(atom, 0) + 2;
    }
    match atom {
        Atom::Num(s) => {
            let text = render_num(s);
            let mut len = text.len();
            if (text.contains('/') && parent > 11) || (text.starts_with('-') && parent >= 14) {
                len += 2;
            }
            len
        }
        Atom::Str(s) => qstr_len(s),
        Atom::Bool(b) => {
            if parent <= 3 {
                3 // "1>0" / "1>2"
            } else if *b {
                4
            } else {
                5
            }
        }
        Atom::Nil => 3,
        Atom::Neg(inner) => {
            // Un: join_lex_segs(["-", inner at 12]); a separator is
            // inserted only when the inner segment starts with '-',
            // i.e. exactly needs_len_sep('-', '-') in the size model.
            let mut len = 1 + atom_len(inner, 12);
            if atom_first(inner, 12) == Some('-') {
                len += 1;
            }
            if 12 < parent {
                len += 2;
            }
            len
        }
    }
}

/// First character of an Atom's size-model segment, used only for the
/// unary-minus separator decision above. For Str this builds the quoted
/// form, but Str never occurs inside Neg (Atom::read only builds Neg
/// around a Num), so this path is not on any hot evaluation.
fn atom_first(atom: &Atom, parent: u8) -> Option<char> {
    if parent == 15 {
        return Some('(');
    }
    match atom {
        Atom::Num(s) => {
            let text = render_num(s);
            if (text.contains('/') && parent > 11) || (text.starts_with('-') && parent >= 14) {
                Some('(')
            } else {
                text.chars().next()
            }
        }
        Atom::Str(s) => qstr(s).chars().next(),
        Atom::Bool(b) => {
            if parent <= 3 {
                Some('1')
            } else if *b {
                Some('t')
            } else {
                Some('f')
            }
        }
        Atom::Nil => Some('n'),
        Atom::Neg(_) => {
            if 12 < parent {
                Some('(')
            } else {
                Some('-')
            }
        }
    }
}
struct Plan {
    origins: Vec<Call>,
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

/// Integer view of a column set, computed once per `proposals` call and
/// shared by every encoding path (projection, palette, encoders) instead of
/// re-parsing `Atom::integer` per path. `Atom::integer` is a pure function
/// of the atom, so the shared values are by definition identical to
/// per-path recomputation.
/// A projected column set: the rewritten shape, the kept columns, and
/// their shared integer views and length sums (aligned with the kept
/// columns).
type ProjectedColumns = (Shape, Vec<Vec<Atom>>, Vec<Option<Vec<i64>>>, Vec<usize>);

/// Sum of `atom_len(atom, 0)` per column, computed once per
/// `proposals` call and shared by every encoding path, mirroring
/// `column_ints`. A table payload flattened from these columns (in
/// row-major order) measures `2 + (n - 1) + lens.iter().sum()`: the
/// same total `values_size` computes on the flattened values, since
/// both sum `atom_len(atom, 0)` over the same atoms and count one
/// comma per field after the first.
fn column_lens(columns: &[Vec<Atom>]) -> Vec<usize> {
    columns
        .iter()
        .map(|c| c.iter().map(|a| atom_len(a, 0)).sum())
        .collect()
}

fn column_ints(columns: &[Vec<Atom>]) -> Vec<Option<Vec<i64>>> {
    columns
        .iter()
        .map(|c| c.iter().map(Atom::integer).collect())
        .collect()
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
    let ints = column_ints(&columns);
    let lens = column_lens(&columns);
    let canonical = if enhanced && columns.len() < arguments_width(&shape) {
        let mut all = Vec::new();
        let mut all_ints = Vec::new();
        let mut all_lens = Vec::new();
        let mut canonical = shape.clone();
        for (slot, args) in canonical.args.iter_mut().enumerate() {
            for (arg, value) in args.iter_mut().enumerate() {
                *value = Arg::Column(all.len());
                let column = (0..count)
                    .map(|row| calls[row * period + slot].args[arg].clone())
                    .collect::<Vec<_>>();
                // Most canonical columns are the deduplicated `columns`
                // themselves; constant columns appear only here and are
                // parsed directly.
                let (shared, shared_len) = match columns.iter().position(|c| *c == column) {
                    Some(at) => (ints[at].clone(), lens[at]),
                    None => (
                        column.iter().map(Atom::integer).collect(),
                        column.iter().map(|a| atom_len(a, 0)).sum(),
                    ),
                };
                all.push(column);
                all_ints.push(shared);
                all_lens.push(shared_len);
            }
        }
        canonical.columns = all.len();
        Some((canonical, all, all_ints, all_lens))
    } else {
        None
    };
    let projected = enhanced
        .then(|| project_columns(&shape, &columns, &ints, &lens))
        .flatten();
    let mut out = if dense {
        palette::proposals(&shape, &targets, &columns, &ints, &lens, count)
    } else {
        Vec::new()
    };
    out.extend(encode_columns(
        shape,
        targets.clone(),
        columns,
        ints,
        &lens,
        count,
        enhanced,
        dense,
    ));
    if let Some((shape, columns, kept_ints, kept_lens)) = projected {
        out.extend(encode_columns(
            shape,
            targets.clone(),
            columns,
            kept_ints,
            &kept_lens,
            count,
            enhanced,
            dense,
        ));
    }
    if let Some((shape, columns, all_ints, all_lens)) = canonical {
        out.extend(encode_columns(
            shape, targets, columns, all_ints, &all_lens, count, false, false,
        ));
    }
    out
}

fn arguments_width(shape: &Shape) -> usize {
    shape.args.iter().map(Vec::len).sum()
}

fn project_columns(
    shape: &Shape,
    columns: &[Vec<Atom>],
    ints: &[Option<Vec<i64>>],
    lens: &[usize],
) -> Option<ProjectedColumns> {
    let mut kept = Vec::<Vec<Atom>>::new();
    let mut integers = Vec::<Option<Vec<i64>>>::new();
    let mut kept_lens = Vec::new();
    let mut mapping = Vec::new();
    for (index, column) in columns.iter().enumerate() {
        let numbers = ints[index].clone();
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
            kept_lens.push(lens[index]);
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
    Some((projected, kept, integers, kept_lens))
}

#[allow(clippy::too_many_arguments)]
fn encode_columns(
    mut shape: Shape,
    targets: Vec<NodeId>,
    columns: Vec<Vec<Atom>>,
    ints: Vec<Option<Vec<i64>>>,
    lens: &[usize],
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
        // The size is the `values_size` total rearranged: the sum of
        // the per-column lengths plus one comma per field after the
        // first (see `column_lens`). The payload keeps the columns and
        // flattens only if adopted (see `Payload::Columns`); the field
        // count is `count * columns.len()` either way.
        debug_assert_eq!(lens.len(), columns.len());
        let size = 2 + (count * columns.len()).saturating_sub(1) + lens.iter().sum::<usize>();
        out.push((
            shape.clone(),
            targets.clone(),
            Payload::Columns(columns, count, size),
        ));
    }
    let numbers = ints.into_iter().collect::<Option<Vec<_>>>();
    if let Some(numbers) = numbers {
        // The byte candidates below share this one integer source
        // through their recipes instead of copying it per candidate.
        let numbers = Arc::new(numbers);
        if dense {
            if let Some((codec, recipe)) = rice::payload(&numbers, count) {
                let mut candidate = shape.clone();
                candidate.codec = codec;
                out.push((
                    candidate,
                    targets.clone(),
                    Payload::Bytes(BytesPayload::Recipe(recipe)),
                ));
            }
        }
        if dense
            && !shape
                .args
                .iter()
                .flatten()
                .any(|a| matches!(a, Arg::Series(_)))
        {
            for (codec, recipe) in grouped::payloads(&numbers, count) {
                let mut candidate = shape.clone();
                candidate.codec = codec;
                out.push((
                    candidate,
                    targets.clone(),
                    Payload::Bytes(BytesPayload::Recipe(recipe)),
                ));
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
                    let mut delta_ints = vec![0];
                    for pair in column.windows(2) {
                        delta_ints.push(pair[1].checked_sub(pair[0])?);
                    }
                    Some(delta_ints)
                })
                .collect::<Option<Vec<_>>>();
            if let Some(delta_ints) = deltas {
                let delta_ints = Arc::new(delta_ints);
                // The candidates over the deltas used to come from a
                // recursive encode whose table candidate the codec filter
                // around it always discarded. Build exactly the two forms
                // that survived that filter, in the same order, directly
                // from the delta integers; the delta atoms and their
                // flattened table are never materialized.
                let firsts: Vec<i64> = numbers.iter().map(|c| c[0]).collect();
                if let Some((codec, recipe)) = packed_recipe(&delta_ints, count, 6) {
                    let mut candidate = shape.clone();
                    candidate.codec = Codec::Delta {
                        firsts: firsts.clone(),
                        inner: Box::new(codec),
                    };
                    out.push((
                        candidate,
                        targets.clone(),
                        Payload::Bytes(BytesPayload::Recipe(recipe)),
                    ));
                }
                if let Some((biases, recipe)) = bias_recipe(&delta_ints, count) {
                    let mut candidate = shape.clone();
                    candidate.codec = Codec::Delta {
                        firsts,
                        inner: Box::new(Codec::Bytes(biases)),
                    };
                    out.push((
                        candidate,
                        targets.clone(),
                        Payload::Bytes(BytesPayload::Recipe(recipe)),
                    ));
                }
            }
        }
        // Try whole-row packing independently of the one-byte-per-column
        // codec. Wider coordinates can still beat a decimal literal table.
        if enhanced {
            if let Some(layout) = packed_layout(&numbers, 8) {
                // Only widths 7..=8 are candidates here; the layout is
                // computed first so narrower packings (already covered
                // by the packed candidate below) never build their bytes.
                if (7..=8).contains(&layout.width) {
                    let recipe = packed_recipe_from_layout(&numbers, &layout, count);
                    let mut packed_shape = shape.clone();
                    packed_shape.codec = packed_codec(layout);
                    out.push((
                        packed_shape,
                        targets.clone(),
                        Payload::Bytes(BytesPayload::Recipe(recipe)),
                    ));
                }
            }
        }
        if let Some((codec, recipe)) = packed_recipe(&numbers, count, 6) {
            let mut packed_shape = shape.clone();
            packed_shape.codec = codec;
            out.push((
                packed_shape,
                targets.clone(),
                Payload::Bytes(BytesPayload::Recipe(recipe)),
            ));
        }
        if let Some((biases, recipe)) = bias_recipe(&numbers, count) {
            shape.codec = Codec::Bytes(biases);
            out.push((shape, targets, Payload::Bytes(BytesPayload::Recipe(recipe))));
        }
    }
    out
}

#[cfg(test)]
fn packed_payload(columns: &[Vec<i64>], count: usize) -> Option<(Codec, String)> {
    packed_payload_with_limit(columns, count, 6)
}

#[cfg(test)]
fn packed_payload_with_limit(
    columns: &[Vec<i64>],
    count: usize,
    max_bytes: u32,
) -> Option<(Codec, String)> {
    let layout = packed_layout(columns, max_bytes)?;
    let bytes = packed_bytes(columns, count, &layout);
    Some((packed_codec(layout), bytes))
}

/// The mixed-radix layout of a whole-row packing — per-column biases
/// and radices plus the byte width their cardinality implies — computed
/// without building the payload bytes. Callers that only need the codec
/// (grouped and shared forms) or that gate on the width (the enhanced
/// 7..=8 form) never pay for a string they would discard.
struct PackedLayout {
    biases: Vec<i64>,
    radices: Vec<i64>,
    width: usize,
}

fn packed_layout(columns: &[Vec<i64>], max_bytes: u32) -> Option<PackedLayout> {
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
    Some(PackedLayout {
        biases,
        radices,
        width,
    })
}

fn packed_codec(layout: PackedLayout) -> Codec {
    Codec::PackedBytes {
        biases: layout.biases,
        radices: layout.radices,
        width: layout.width,
    }
}

#[cfg(test)]
fn packed_bytes(columns: &[Vec<i64>], count: usize, layout: &PackedLayout) -> String {
    let mut bytes = String::with_capacity(count * layout.width);
    walk_packed_bytes(
        columns,
        &layout.biases,
        &layout.radices,
        layout.width,
        count,
        &mut |b| bytes.push(b as char),
    );
    bytes
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
    // Pure copy: only child numbering differs from the source chain, so the
    // clone attribution shares the source slot instead of recording a
    // rewrite reason for each Index/Paren node. Falls back to derive_from
    // when kinds differ or source snapshots are not shared.
    target
        .nodes
        .derive_from_clone(copy, &source.nodes, node, "draw-record-callee-copy");
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
/// Guard predicates of one variant input, each a pure function of the
/// input's content. Computed once per distinct input and shared by every
/// consumer (the record planner and the regular/shared synthesizers used
/// to re-derive them with separate walks). Thresholds stay at the
/// consumers; only the predicates are shared.
struct InputFacts {
    screen: bool,
    potential: usize,
    jumps: bool,
}

impl InputFacts {
    fn compute(ast: &Ast, root: NodeId) -> Self {
        let mut facts = Self {
            screen: false,
            potential: 0,
            jumps: false,
        };
        storm_lua_syntax::ast_utils::walk(ast, root, &mut |n| {
            facts.screen |= matches!(ast.node(n), Node::Name(s) if ast.strings.get(*s) == "screen");
            facts.jumps |= matches!(ast.node(n), Node::Goto(_) | Node::Label(_));
            if let Node::Callstat(c) = ast.node(n) {
                if let Node::Call(_, args, None) = ast.node(*c) {
                    if args.len() <= 8 && args.iter().all(|&a| Atom::read(ast, a).is_some()) {
                        facts.potential += 1;
                    }
                }
            }
        });
        facts
    }
}

/// The most inherited (non-global) bindings visible in any one scope of
/// the resolution: a pure function of the resolution, thresholded
/// differently by each consumer.
fn max_inherited_locals(res: &Resolution) -> usize {
    let mut inherited = vec![0usize; res.scopes.len()];
    for scope in &res.scopes {
        inherited[scope.id as usize] = scope.parent.map_or(0, |p| inherited[p as usize])
            + scope
                .bindings
                .iter()
                .filter(|&&b| res.binding(b).kind != BindingKind::Global)
                .count();
    }
    inherited.into_iter().max().unwrap_or(0)
}

/// Everything the variant consumers derive from one input: its
/// resolution, classifier data, block list and the inherited-locals
/// maximum. Built only from the owning `SharedInput`'s private clone, so
/// the derived data belongs to that input by construction.
struct InputAnalysis {
    res: Resolution,
    data: ClassifierData,
    blocks: Vec<NodeId>,
    max_inherited: usize,
}

impl InputAnalysis {
    fn build(input: &Ast, root: NodeId) -> Self {
        let res = resolve(input, root);
        let data = ClassifierData::build(input, &res, root);
        let mut blocks = Vec::new();
        storm_lua_syntax::ast_utils::walk(input, root, &mut |n| {
            if matches!(input.node(n), Node::Block(_)) {
                blocks.push(n);
            }
        });
        let max_inherited = max_inherited_locals(&res);
        Self {
            res,
            data,
            blocks,
            max_inherited,
        }
    }
}

/// One variant input plus everything the consumers share about it. The
/// bundle owns a clone of the input; the heavier predicates (closed key
/// space, chunk locals) and the full analysis are computed lazily, at
/// most once, on first demand, so an input no consumer accepts still
/// costs no resolution. Consumers query the analysis only through this
/// bundle, whose methods pair the derived data with the bundle's own
/// input: there is no API for applying one input's analysis to another
/// input, which is what makes the sharing sound by construction rather
/// than by a documented precondition.
struct SharedInput {
    input: Ast,
    root: NodeId,
    facts: InputFacts,
    closed_key_space: std::cell::OnceCell<bool>,
    chunk_locals: std::cell::OnceCell<usize>,
    analysis: std::cell::OnceCell<InputAnalysis>,
}

impl SharedInput {
    fn new(input: Ast, root: NodeId, facts: InputFacts) -> Self {
        Self {
            input,
            root,
            facts,
            closed_key_space: std::cell::OnceCell::new(),
            chunk_locals: std::cell::OnceCell::new(),
            analysis: std::cell::OnceCell::new(),
        }
    }

    fn from_input(input: Ast, root: NodeId) -> Self {
        let facts = InputFacts::compute(&input, root);
        Self::new(input, root, facts)
    }

    fn facts(&self) -> &InputFacts {
        &self.facts
    }

    fn closed_key_space(&self) -> bool {
        *self
            .closed_key_space
            .get_or_init(|| super::closed_fields::has_closed_key_space(&self.input, self.root))
    }

    fn chunk_locals(&self) -> usize {
        *self
            .chunk_locals
            .get_or_init(|| chunk_locals(&self.input, self.root))
    }

    /// The bundle's own input. Consumers treat it as read-only: rewrites
    /// go to a separate clone, as they did with their private setups.
    fn source(&self) -> &Ast {
        &self.input
    }

    fn analysis(&self) -> &InputAnalysis {
        self.analysis
            .get_or_init(|| InputAnalysis::build(&self.input, self.root))
    }

    fn blocks(&self) -> &[NodeId] {
        &self.analysis().blocks
    }

    fn max_inherited(&self) -> usize {
        self.analysis().max_inherited
    }

    fn call(&self, statement: NodeId) -> Option<Call> {
        let analysis = self.analysis();
        analysis.data.call(&self.input, &analysis.res, statement)
    }
}

pub(crate) fn record_variants(ast: &Ast, root: NodeId, rename: bool) -> Vec<(Ast, PassResult)> {
    // Most onTick programs contain no eligible drawing input. Do not pay
    // for several resolvers, planners and final cleanups on that path.
    let facts = InputFacts::compute(ast, root);
    if !facts.screen || facts.potential < 4 {
        return vec![(
            ast.clone(),
            PassResult {
                root,
                saved: Some(0),
                details: None,
            },
        )];
    }
    let input = SharedInput::new(ast.clone(), root, facts);
    let mut shared_costs = BTreeMap::<Shape, usize>::new();
    let mut legacy = ast.clone();
    let result = pack_impl(
        &mut legacy,
        root,
        rename,
        false,
        false,
        &mut shared_costs,
        &input,
    );
    let mut enhanced = ast.clone();
    let extra = pack_impl(
        &mut enhanced,
        root,
        rename,
        true,
        false,
        &mut shared_costs,
        &input,
    );
    let mut out = vec![(legacy, result)];
    if enhanced.nodes != out[0].0.nodes || !enhanced.strings.same_symbols(&out[0].0.strings) {
        out.push((enhanced, extra));
    }
    let mut regular = ast.clone();
    let loops = regular::synthesize(&mut regular, root, rename, &input);
    let dense_regular = (loops > 0).then(|| regular.clone());
    // Both packs below run on the synthesized content, so they share one bundle.
    let reg_input = (loops > 0).then(|| SharedInput::from_input(regular.clone(), root));
    if let Some(reg_input) = &reg_input {
        let mut result = pack_impl(
            &mut regular,
            root,
            rename,
            true,
            false,
            &mut shared_costs,
            reg_input,
        );
        result.saved =
            Some(measure_size(ast, root).saturating_sub(measure_size(&regular, root)) as u64);
        result
            .details
            .get_or_insert_with(Vec::new)
            .push(format!("exact_inline_loops={loops}"));
        out.push((regular, result));
    }
    let mut dense = ast.clone();
    let result = pack_impl(
        &mut dense,
        root,
        rename,
        true,
        true,
        &mut shared_costs,
        &input,
    );
    if !out
        .iter()
        .any(|(a, _)| a.nodes == dense.nodes && a.strings.same_symbols(&dense.strings))
    {
        out.push((dense, result));
    }
    if let (Some(mut dense_regular), Some(reg_input)) = (dense_regular, &reg_input) {
        let mut result = pack_impl(
            &mut dense_regular,
            root,
            rename,
            true,
            true,
            &mut shared_costs,
            reg_input,
        );
        result.saved =
            Some(measure_size(ast, root).saturating_sub(measure_size(&dense_regular, root)) as u64);
        if !out.iter().any(|(a, _)| {
            a.nodes == dense_regular.nodes && a.strings.same_symbols(&dense_regular.strings)
        }) {
            out.push((dense_regular, result));
        }
    }
    let mut shared = ast.clone();
    let dictionaries = shared::synthesize(&mut shared, root, rename, &input);
    if dictionaries > 0 {
        let shared_input = SharedInput::from_input(shared.clone(), root);
        let mut result = pack_impl(
            &mut shared,
            root,
            rename,
            true,
            true,
            &mut shared_costs,
            &shared_input,
        );
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

fn pack_impl(
    ast: &mut Ast,
    root: NodeId,
    rename: bool,
    enhanced: bool,
    dense: bool,
    costs: &mut BTreeMap<Shape, usize>,
    input: &SharedInput,
) -> PassResult {
    let unchanged = || PassResult {
        root,
        saved: Some(0),
        details: None,
    };
    if input.facts().potential < 4 || input.facts().jumps || !input.closed_key_space() {
        return unchanged();
    }
    let source = input.source();
    let mut helpers = Vec::<Helper>::new();
    let mut by_shape = BTreeMap::<Shape, usize>::new();
    let mut plans = BTreeMap::<NodeId, Vec<Plan>>::new();
    let helper_budget = MAX_HELPERS.min(175usize.saturating_sub(input.chunk_locals()));
    if helper_budget == 0 {
        return unchanged();
    }
    for block in input.blocks().iter().copied() {
        let Node::Block(stmts) = source.node(block) else {
            unreachable!()
        };
        let mut cursor = 0;
        while cursor < stmts.len() {
            let mut calls = Vec::new();
            for &stmt in &stmts[cursor..] {
                if let Some(call) = input.call(stmt) {
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
            // Prefix sums of per-statement sizes: every `old` total below
            // covers a contiguous range of this cluster, so measure each
            // statement once instead of re-measuring per (period, width).
            let mut stmt_prefix = Vec::with_capacity(calls.len() + 1);
            let mut stmt_acc = 0usize;
            stmt_prefix.push(stmt_acc);
            for call in &calls {
                stmt_acc += measure_stmt(source, call.statement);
                stmt_prefix.push(stmt_acc);
            }
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
                        let old = stmt_prefix[at + count * period] - stmt_prefix[at];
                        for (shape, targets, payload) in
                            proposals(slice, period, count, enhanced, dense)
                        {
                            let exists = by_shape.contains_key(&shape);
                            if !exists && helpers.len() >= helper_budget {
                                continue;
                            }
                            let definition = if exists {
                                0
                            } else if let Some(&cached) = costs.get(&shape) {
                                cached + 1
                            } else {
                                let computed = helper_cost(&shape);
                                costs.insert(shape.clone(), computed);
                                computed + 1
                            };
                            // call_size is always positive, so a definition
                            // that alone reaches `old` can never win; skip
                            // measuring the call in that case.
                            if definition >= old {
                                continue;
                            }
                            let call_size = 3
                                + payload.size()
                                + targets
                                    .iter()
                                    .map(|&n| measure_expr(source, n) + 1)
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
                        let old = stmt_prefix[at + stop] - stmt_prefix[at];
                        let definition = if exists {
                            0
                        } else if let Some(&cached) = costs.get(&shape) {
                            cached + 1
                        } else {
                            let computed = helper_cost(&shape);
                            costs.insert(shape.clone(), computed);
                            computed + 1
                        };
                        // Same short-circuit as the main proposal loop.
                        if definition >= old {
                            continue;
                        }
                        let call_size = 3
                            + payload.size()
                            + targets
                                .iter()
                                .map(|&n| measure_expr(source, n) + 1)
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
                        origins: if source.nodes.tracks_origins() {
                            calls[at..best.stop].to_vec()
                        } else {
                            Vec::new()
                        },
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
    // Include a pessimistic allowance for pooled payloads below.
    if input.max_inherited() + helpers.len() + 24 > 245 {
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
        let first = target.nodes.len();
        let mut trace = source.nodes.tracks_origins().then(|| {
            let batches = plans
                .values()
                .flatten()
                .filter(|p| p.helper == i)
                .map(|p| p.origins.as_slice())
                .collect::<Vec<_>>();
            provenance::Trace::batches(&batches, helper.shape.targets.len())
        });
        let definition = encoding::emit_helper_recording(
            &mut target,
            &helper.shape,
            helper_names[i],
            serial + i,
            &mut trace,
        );
        if let Some(trace) = trace {
            trace.finish(&mut target, source, first, definition);
        }
        definitions.push(definition);
    }
    // Repeated byte payloads (e.g. the common background of nine cards) share
    // one immutable string. No table allocation or identity is shared.
    let mut payload_counts = BTreeMap::<String, usize>::new();
    for plan in plans.values().flatten() {
        if let Some(text) = plan.payload.bytes_text() {
            *payload_counts.entry(text.into_owned()).or_default() += 1;
        }
    }
    let mut payload_symbols = BTreeMap::new();
    let mut symbols = Vec::new();
    let mut values = Vec::new();
    for (bytes, count) in payload_counts {
        if symbols.len() >= 24 || input.chunk_locals() + helpers.len() + symbols.len() >= 180 {
            break;
        }
        // The shared payload's rendered size, identical to the size
        // the payload itself reported when priced: for a recipe that
        // is the feature-computed length of this same string.
        let size = quoted_bytes_len(bytes.as_bytes());
        if count < 2 || count * (size.saturating_sub(2)) <= size + 10 {
            continue;
        }
        let symbol = fresh(&mut target, &mut taken, "draw_data", &mut serial);
        symbols.push(symbol);
        let payload = Payload::Bytes(BytesPayload::Literal(bytes.clone())).emit(&mut target);
        if target.nodes.tracks_origins() {
            let originals = plans
                .values()
                .flatten()
                .filter(|p| {
                    p.payload
                        .bytes_text()
                        .is_some_and(|text| text.as_ref() == bytes.as_str())
                })
                .flat_map(|p| p.origins.iter())
                .flat_map(|c| c.origin_arguments.iter().copied())
                .collect::<Vec<_>>();
            provenance::derive(
                &mut target,
                payload,
                source,
                &originals,
                "draw-record-shared-payload",
            );
        }
        if dense {
            dictionary::compress_literal(&mut target, payload);
        }
        values.push(payload);
        payload_symbols.insert(bytes, symbol);
    }
    if !symbols.is_empty() {
        let storage = target.push(Node::Local(symbols, values));
        target
            .nodes
            .mark_synthetic(storage, "draw-record-payload-storage");
        definitions.push(storage);
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
                .map(|&n| copy_callee(source, &mut target, n))
                .collect::<Vec<_>>();
            let payload = plan
                .payload
                .bytes_text()
                .and_then(|text| payload_symbols.get(text.as_ref()).copied())
                .map(|symbol| name(&mut target, symbol));
            let emitted = payload.unwrap_or_else(|| plan.payload.emit(&mut target));
            if target.nodes.tracks_origins() {
                provenance::payload(
                    &mut target,
                    emitted,
                    source,
                    &plan.origins,
                    &helpers[plan.helper].shape,
                );
            }
            if dense && payload.is_none() && matches!(plan.payload, Payload::Bytes(_)) {
                dictionary::compress_literal(&mut target, emitted);
            }
            args.push(emitted);
            let f = name(&mut target, helper_names[plan.helper]);
            let c = target.push(Node::Call(f, args, None));
            let stmt = target.push(Node::Callstat(c));
            if target.nodes.tracks_origins() {
                target
                    .nodes
                    .mark_synthetic(f, "draw-record-helper-reference");
                let originals = plan
                    .origins
                    .iter()
                    .map(|c| c.origin_call)
                    .collect::<Vec<_>>();
                provenance::derive(&mut target, c, source, &originals, "draw-record-batch-call");
                provenance::derive(
                    &mut target,
                    stmt,
                    source,
                    &originals,
                    "draw-record-batch-call",
                );
            }
            stmts.push(stmt);
            cursor = plan.stop;
        }
        stmts.extend_from_slice(&old[cursor..]);
        target
            .nodes
            .rewrite(block, Node::Block(stmts), "draw-record-run-replacement");
    }
    if let Node::Block(stmts) = target.node(root).clone() {
        definitions.extend(stmts);
        target.nodes.rewrite(
            root,
            Node::Block(definitions),
            "draw-record-helper-insertion",
        );
    } else {
        return unchanged();
    }
    remove_unused_forwarders(&mut target, root);
    if rename {
        target = scope_rename_fast(&target, root).ast;
    }
    let before = measure_size(source, root);
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
                    classifier.data.wrappers.contains(&b)
                        && !res.binding(b).fixed
                        && !reads.contains(&b)
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod payload_size_tests {
    use super::*;

    /// The pre-change computation, kept as a test oracle only: build the
    /// node emit() would have produced in a fresh Ast and measure it.
    fn old_size(payload: &Payload) -> usize {
        let mut ast = Ast::new();
        let n = payload.emit(&mut ast);
        measure_expr(&ast, n)
    }

    fn num_atom(s: &str) -> Atom {
        Atom::Num(Arc::from(s))
    }

    fn str_atom(s: &str) -> Atom {
        Atom::Str(Arc::from(s))
    }

    #[test]
    fn payload_size_matches_ast_measure() {
        let nums = [
            "0",
            "1",
            "9",
            "10",
            "99",
            "100",
            "123456789012345",
            "1234567890123456",
            "0.5",
            ".5",
            "1.5",
            "1e5",
            "0xFF",
            "0x10",
            "100000",
            "4290772991",
            "0.0000396",
            "3.96e-5",
            "0.0",
            "00",
            "01",
        ];
        // Raw string-literal texts, as stored in Node::Str (with quotes).
        let strs = [
            "\"abc\"",
            "\"a\\\"b\"",
            "'abc'",
            "'it\\'s'",
            "\"\\065\\0\"",
            "\"\"",
            "''",
            "\"a\\nb\"",
            "\"a\\rb\"",
            "\"a\\tb\"",
            "\"a\\\\b\"",
            "\"\\a\\v\\b\\f\"",
            "\"雪😀\"",
            "\"'\\\"\\\\\"",
            "[[abc]]",
            "[=[a]]b]=]",
            "\"x\"",
            "\"a\\u{2028}b\"",
        ];
        let mut payloads: Vec<Payload> = Vec::new();
        for n in [0usize, 1, 9, 10, 99, 160, 1_000_000, usize::MAX] {
            payloads.push(Payload::Count(n));
        }
        payloads.push(Payload::values(vec![]));
        for s in nums {
            payloads.push(Payload::values(vec![num_atom(s)]));
            payloads.push(Payload::values(vec![Atom::Neg(Box::new(num_atom(s)))]));
            payloads.push(Payload::values(vec![Atom::Neg(Box::new(Atom::Neg(
                Box::new(num_atom(s)),
            )))]));
        }
        for s in strs {
            payloads.push(Payload::values(vec![str_atom(s)]));
        }
        payloads.push(Payload::values(vec![
            num_atom("12"),
            str_atom("\"hi\""),
            Atom::Bool(true),
            Atom::Bool(false),
            Atom::Nil,
            Atom::Neg(Box::new(num_atom("7"))),
        ]));
        let mixed: Vec<Atom> = nums
            .iter()
            .map(|s| num_atom(s))
            .chain(strs.iter().map(|s| str_atom(s)))
            .collect();
        payloads.push(Payload::values(mixed));
        // Byte payloads: the emitted Str is quote_lua(s), so s ranges over
        // decoded values, including control characters, quotes, backslash,
        // unicode, CR/LF and long-bracket candidates.
        let byte_values = [
            "",
            "a",
            "abc",
            "a\"b",
            "a'b",
            "a\\b",
            "a\nb",
            "a\rb",
            "a\tb",
            "a\u{7}b",
            "a\u{b}b",
            "a\u{0}b",
            "a\u{1b}b",
            "雪😀",
            "'\"\\",
            "[[x]]",
            "]]",
            "]=]",
            "a]]b",
            "\nabc",
            "a\r\nb",
            "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        ];
        for s in byte_values {
            payloads.push(Payload::Bytes(BytesPayload::Literal(s.to_string())));
        }
        for (i, payload) in payloads.iter().enumerate() {
            assert_eq!(
                payload.size(),
                old_size(payload),
                "payload size mismatch at index {i}"
            );
        }
    }

    #[test]
    fn bytes_recipes_build_and_size_match_the_string_constructors() {
        // Bias and packed recipes over one shared source: the built
        // bytes and the feature-computed sizes agree with the string
        // constructors and with `quoted_bytes_len` of the built text.
        let source = Arc::new(vec![
            vec![3, 40, 91, 0, 17, 88, 12, 65],
            vec![-5, 20, 7, 33, -40, 2, 19, 51],
        ]);
        let (biases, recipe) = bias_recipe(&source, 8).expect("bias form");
        assert_eq!(biases, vec![0, -40]);
        let built = recipe.build();
        assert_eq!(built.len(), 16);
        assert_eq!(recipe.size(), quoted_bytes_len(built.as_bytes()));
        let (codec, text) = packed_payload(&source, 8).expect("packed form");
        let (recipe_codec, recipe) = packed_recipe(&source, 8, 6).expect("packed recipe");
        let Codec::PackedBytes {
            biases: want_biases,
            radices: want_radices,
            width: want_width,
        } = &codec
        else {
            panic!("packed codec expected")
        };
        let Codec::PackedBytes {
            biases: got_biases,
            radices: got_radices,
            width: got_width,
        } = &recipe_codec
        else {
            panic!("packed recipe codec expected")
        };
        assert_eq!(
            (want_biases, want_radices, want_width),
            (got_biases, got_radices, got_width)
        );
        assert_eq!(recipe.build(), text);
        assert_eq!(recipe.size(), quoted_bytes_len(text.as_bytes()));
        // A payload priced through the recipe reports the same size
        // as the literal payload of the same bytes.
        let via_recipe = Payload::Bytes(BytesPayload::Recipe(recipe));
        let via_literal = Payload::Bytes(BytesPayload::Literal(text));
        assert_eq!(via_recipe.size(), via_literal.size());
        // Every byte payload encode_columns returns over these columns
        // (packed, bias and both delta forms) prices exactly what the
        // emit-and-measure oracle computes for it.
        let columns: Vec<Vec<i64>> = vec![
            (0..16).map(|i| 100 + i * 37).collect(),
            (0..16).map(|i| 5000 - i * 91).collect(),
            (0..16).map(|i| 300 + i * 3).collect(),
        ];
        let atoms: Vec<Vec<Atom>> = columns
            .iter()
            .map(|c| c.iter().map(|v| Atom::Num(v.to_string().into())).collect())
            .collect();
        let ints = column_ints(&atoms);
        let lens = column_lens(&atoms);
        let shape = Shape {
            targets: vec![0],
            args: vec![vec![Arg::Column(0), Arg::Column(1), Arg::Column(2)]],
            columns: 3,
            codec: Codec::Counter,
            repeats: 1,
            translations: Vec::new(),
        };
        let out = encode_columns(shape, vec![0], atoms, ints, &lens, 16, true, true);
        let mut byte_payloads = 0;
        for (_, _, payload) in &out {
            if matches!(payload, Payload::Bytes(_)) {
                byte_payloads += 1;
                assert_eq!(payload.size(), old_size(payload));
            }
        }
        assert!(byte_payloads >= 3);
    }

    #[test]
    fn column_lens_sum_matches_flattened_scan() {
        // The lens-derived size used by encode_columns must equal the
        // direct scan over the row-major flattening, for any columns.
        let columns: Vec<Vec<Atom>> = vec![
            vec![num_atom("12"), num_atom("-3"), num_atom("100000")],
            vec![str_atom("\"hi\""), Atom::Bool(true), Atom::Nil],
            vec![
                Atom::Neg(Box::new(num_atom("7"))),
                num_atom("0"),
                num_atom("42"),
            ],
        ];
        let lens = column_lens(&columns);
        let flat: Vec<Atom> = (0..3)
            .flat_map(|r| columns.iter().map(move |c| c[r].clone()))
            .collect();
        assert_eq!(
            2 + flat.len().saturating_sub(1) + lens.iter().sum::<usize>(),
            values_size(&flat)
        );
        assert_eq!(values_size(&[]), 2);
        assert_eq!(column_lens(&[]), Vec::<usize>::new());
    }

    #[test]
    fn encode_columns_stored_sizes_match_scan() {
        // Every values payload encode_columns returns — base, packed
        // and delta paths included — must store the size the direct
        // scan computes for its atoms; the column form is checked
        // against the scan of its flattening.
        let columns: Vec<Vec<i64>> = vec![
            (0..16).map(|i| 100 + i * 37).collect(),
            (0..16).map(|i| 5000 - i * 91).collect(),
            (0..16).map(|i| 300 + i * 3).collect(),
        ];
        let atoms: Vec<Vec<Atom>> = columns
            .iter()
            .map(|c| c.iter().map(|v| Atom::Num(v.to_string().into())).collect())
            .collect();
        let ints = column_ints(&atoms);
        let lens = column_lens(&atoms);
        let shape = Shape {
            targets: vec![0],
            args: vec![vec![Arg::Column(0), Arg::Column(1), Arg::Column(2)]],
            columns: 3,
            codec: Codec::Counter,
            repeats: 1,
            translations: Vec::new(),
        };
        let out = encode_columns(shape, vec![0], atoms, ints, &lens, 16, true, true);
        assert!(out
            .iter()
            .any(|(s, _, _)| matches!(s.codec, Codec::Delta { .. })));
        for (_, _, payload) in &out {
            if let Payload::Columns(columns, count, size) = payload {
                let flat: Vec<Atom> = (0..*count)
                    .flat_map(|r| columns.iter().map(move |c| c[r].clone()))
                    .collect();
                assert_eq!(*size, values_size(&flat));
            }
        }
    }
}
