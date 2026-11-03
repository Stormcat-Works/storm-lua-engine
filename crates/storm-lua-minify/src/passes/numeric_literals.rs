//! Numeric literal approximation (`passes/numeric-literals.ts`).
//!
//! Non-integral literals are rounded only when the configured tolerance allows
//! it and the literal does not feed an exact route (bit/radix operations,
//! numeric-for bounds, table indexes, string.pack/unpack, comparisons, or a
//! binding/function transitively used by one of those routes). Comparisons
//! are exact routes because a within-tolerance rounding of either operand
//! can flip the discrete result (candidate P, w9 cycle 66), and the
//! discretizing builtins math.floor / math.ceil / math.tointeger are
//! exact calls for the same reason (candidate Q, w9 cycle 66). The
//! exact-call set also covers every builtin that coerces an argument to
//! an integer or renders it exactly (string.format / rep / sub / byte /
//! char / find-family, table.insert / remove / concat / move, tostring,
//! tonumber, select, math.type), and a callee is recognized by its
//! member name -- chased through single-definition alias bindings --
//! because a chunk that mutates a builtin root poisons builtin
//! resolution while the captured member still decides what the call
//! does with its arguments (candidate U, w9 cycle 67). A member that
//! is itself assigned anywhere in the chunk is an exact route too:
//! the call can be rebound to a discretizing function before it runs
//! (`math.min = math.ceil` between loop iterations), which amplifies
//! a within-tolerance rounding into a whole integer (candidate AD,
//! w9 cycle 68). The identification chain is fail-closed: a callee
//! that cannot be positively identified as a tolerant builtin or a
//! chunk function -- an unchaseable base or binding (parameter,
//! multiple definitions, opaque definition, root rebound through
//! any of those) -- is an exact route, because identification
//! failure means the callee is unknown, not tolerant (candidate S5,
//! w6 cycle 71).

use std::collections::{HashMap, HashSet};

use crate::config::NumericTolerance;
use crate::pass::PassResult;
#[cfg(test)]
use crate::provenance_audit_support::Printer;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{api_roots, resolve, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::numeric::{num_val, short_num, shortest_exact_hex_float_literal};
use storm_lua_syntax::print::render_num;
use storm_lua_syntax::size::measure_expr;

const EXACT_BINARY: &[&str] = &[
    "&", "|", "~", "<<", ">>", "//", "%", "==", "~=", "<", "<=", ">", ">=",
];
const PER_LITERAL_ABS_CAP: f64 = 1e-12;
const PER_LITERAL_REL_CAP: f64 = 1e-9;

/// Canonicalize source/generated floating-point tokens to a shorter exact
/// hexadecimal spelling once before the search fan-out. This is deliberately
/// separate from `render_num`: the printer is invoked repeatedly while scoring
/// candidates, so doing IEEE-754 candidate synthesis there is disproportionately
/// expensive. Integer-source tokens are rejected by the shared helper.
pub fn canonicalize_exact_float_literals(ast: &mut Ast, root: NodeId) -> PassResult {
    let mut ids = Vec::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |id| {
        if matches!(ast.node(id), Node::Num(_)) {
            ids.push(id);
        }
    });

    let mut saved = 0u64;
    let mut changed = 0usize;
    for id in ids {
        let Node::Num(raw) = ast.node(id).clone() else {
            continue;
        };
        // Tiny numeric tokens cannot beat the shortest legal hexadecimal float
        // (`0x.1`), and skipping them keeps the common integer/draw path cheap.
        if raw.len() < 7 {
            continue;
        }
        let current_len = render_num(&raw).len();
        let Some(hex) = shortest_exact_hex_float_literal(&raw) else {
            continue;
        };
        if hex.len() >= current_len {
            continue;
        }
        saved += (current_len - hex.len()) as u64;
        changed += 1;
        ast.nodes
            .rewrite(id, Node::Num(hex.into()), "exact-float-spelling");
    }

    PassResult {
        root,
        saved: Some(saved),
        details: (changed > 0).then(|| vec![format!("canonicalized={changed}")]),
    }
}

fn within_tolerance(original: f64, candidate: f64, tolerance: NumericTolerance) -> bool {
    (candidate - original).abs() <= tolerance.abs + tolerance.rel * original.abs()
}

/// `Number(value.toPrecision(digits))`. Only the rounded numeric value matters;
/// Rust's scientific formatter supplies exactly `digits` significant decimal
/// digits and parses back through IEEE-754 round-to-nearest.
fn to_precision_number(value: f64, digits: usize) -> Option<f64> {
    let text = format!("{:.*e}", digits.saturating_sub(1), value);
    text.parse::<f64>().ok()
}

fn shortest_approximation(
    ast: &mut Ast,
    value: f64,
    tolerance: NumericTolerance,
) -> Option<String> {
    let budget = NumericTolerance {
        abs: tolerance.abs.min(PER_LITERAL_ABS_CAP),
        rel: tolerance.rel.min(PER_LITERAL_REL_CAP),
    };
    let mut best: Option<String> = None;
    for digits in 1..=16 {
        let Some(rounded) = to_precision_number(value, digits) else {
            continue;
        };
        if !rounded.is_finite() || !within_tolerance(value, rounded, budget) {
            continue;
        }
        let printed = short_num(rounded);
        if best
            .as_ref()
            .is_none_or(|current| printed.len() < current.len())
        {
            best = Some(printed);
        }
    }
    let exact = ast.num(&short_num(value));
    let exact_len = measure_expr(ast, exact);
    best.filter(|candidate| candidate.len() < exact_len)
}

fn collect_definitions(
    ast: &Ast,
    res: &Resolution,
    root: NodeId,
) -> HashMap<BindingId, Vec<NodeId>> {
    let mut definitions: HashMap<BindingId, Vec<NodeId>> = HashMap::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |id| match ast.node(id) {
        Node::Assign(vs, es) => {
            for (index, target) in vs.iter().enumerate() {
                let Some(expression) = es.get(index) else {
                    continue;
                };
                if !matches!(ast.node(*target), Node::Name(_)) {
                    continue;
                }
                if let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() {
                    definitions.entry(bid).or_default().push(*expression);
                }
            }
        }
        Node::Local(_, es) => {
            let bids = res
                .node_bids
                .get(id as usize)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            for (index, bid) in bids.iter().enumerate() {
                if let Some(expression) = es.get(index) {
                    definitions.entry(*bid).or_default().push(*expression);
                }
            }
        }
        _ => {}
    });
    definitions
}

/// Member names whose call observes an argument's exact value or
/// integrality: the discretizing math members, the string members that
/// coerce arguments to integers or render them exactly, and the table
/// members with integer index arguments.
const EXACT_MEMBER_NAMES: &[&str] = &[
    "floor",
    "ceil",
    "tointeger",
    "type",
    "format",
    "rep",
    "sub",
    "byte",
    "char",
    "find",
    "match",
    "gmatch",
    "gsub",
    "pack",
    "unpack",
    "insert",
    "remove",
    "concat",
    "move",
];

/// Whether the expression heading a call reads a member with an
/// exact-observing name, chasing through bindings with exactly one
/// definition (the shape api-alias localization leaves behind: the
/// callee is a bare local whose sole definition is the member read).
/// The chase deliberately does not consult builtin resolution: a chunk
/// that mutates the `math` table poisons that resolution, while the
/// member name on the captured value still decides what the call does
/// with its arguments -- and if the member was replaced by user code,
/// treating the arguments as exact is the safe direction anyway
/// (candidate U, w9 cycle 67).
/// The member name a call's callee reads, if it has the member-read
/// shape (directly, or through single-definition alias bindings).
fn callee_member_name(
    ast: &Ast,
    res: &Resolution,
    definitions: &HashMap<BindingId, Vec<NodeId>>,
    function: NodeId,
) -> Option<String> {
    let mut current = function;
    let mut visited = HashSet::new();
    loop {
        match ast.node(current) {
            Node::Index(_, key, _) => {
                return match ast.node(*key) {
                    Node::Str(raw) => storm_lua_syntax::numeric::decode_lua_string(raw),
                    _ => None,
                };
            }
            Node::Name(_) => {
                current = chase_single_definition(ast, res, definitions, current, &mut visited)?;
            }
            _ => return None,
        }
    }
}

/// Member writes in the chunk, for exact-route determination
/// (candidates AD and AE).
///
/// `names` holds the member names written through a literal key
/// (plain assignment or method definition), through any base: a
/// call through one of these names can denote a different function
/// at run time than the member read suggests, so its arguments are
/// exact routes (candidate AD).
///
/// A write through a dynamic (non-literal) key can rebind any
/// member of its base table, which the name set cannot express
/// (candidate AE): when the base chases to a binding whose single
/// definition is a table constructor, the write is recorded
/// against that binding's root; for every other base shape the
/// write is recorded as poisoning member calls through any base.
struct WrittenMembers {
    names: HashSet<String>,
    dynamic_bases: HashSet<BindingId>,
    dynamic_anywhere: bool,
}

fn unparen(ast: &Ast, mut node: NodeId) -> NodeId {
    while let Node::Paren(inner) = ast.node(node) {
        node = *inner;
    }
    node
}

/// Follow the unique definition expression of the binding a name
/// node resolves to. Returns `None` when the name does not resolve,
/// the binding does not have exactly one definition, or the chase
/// revisits a binding.
///
/// The chases in this module used to stop after four hops, which
/// made exactness depend on how much alias folding other passes
/// (single-use-local-sinking) had already done: with sinking off, a
/// depth-four alias chain was out of reach and discretizer calls
/// through it were approximated (candidate S3). The per-step
/// justification is unchanged at any depth -- a binding with
/// exactly one definition yields that definition's value at every
/// read -- and termination is guaranteed by the visited set: a
/// genuine alias chain cannot revisit a binding (a definition
/// cannot transitively be its own value; self-references resolve
/// to an outer binding), and even a cyclic definition graph formed
/// by mutual single assignments (`x = y; y = x`) bails out
/// conservatively instead of looping.
fn chase_single_definition(
    ast: &Ast,
    res: &Resolution,
    definitions: &HashMap<BindingId, Vec<NodeId>>,
    node: NodeId,
    visited: &mut HashSet<BindingId>,
) -> Option<NodeId> {
    let bid = res
        .node_bid
        .get(unparen(ast, node) as usize)
        .copied()
        .flatten()?;
    if !visited.insert(bid) {
        return None;
    }
    let exprs = definitions.get(&bid)?;
    if exprs.len() != 1 {
        return None;
    }
    Some(exprs[0])
}

/// Chase a base expression through single-definition name bindings
/// to the binding whose single definition is a table constructor,
/// when the chain ends at one. Calls and writes keyed by the
/// returned root binding provably address the same table object
/// (any alias of it chases to the same root, and a rebinding of an
/// intermediate binding would break the single-definition chain).
fn table_root_binding(
    ast: &Ast,
    res: &Resolution,
    definitions: &HashMap<BindingId, Vec<NodeId>>,
    mut node: NodeId,
) -> Option<BindingId> {
    let mut visited = HashSet::new();
    loop {
        let Node::Name(_) = ast.node(unparen(ast, node)) else {
            return None;
        };
        let bid = res
            .node_bid
            .get(unparen(ast, node) as usize)
            .copied()
            .flatten()?;
        let definition = chase_single_definition(ast, res, definitions, node, &mut visited)?;
        let definition = unparen(ast, definition);
        if matches!(ast.node(definition), Node::Table(_)) {
            return Some(bid);
        }
        node = definition;
    }
}

fn collect_written_members(
    ast: &Ast,
    res: &Resolution,
    definitions: &HashMap<BindingId, Vec<NodeId>>,
    root: NodeId,
) -> WrittenMembers {
    let mut written = WrittenMembers {
        names: HashSet::new(),
        dynamic_bases: HashSet::new(),
        dynamic_anywhere: false,
    };
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |id| {
        let targets: &[NodeId] = match ast.node(id) {
            Node::Assign(targets, _) => targets,
            Node::Funcstat(target, _) => std::slice::from_ref(target),
            _ => return,
        };
        for target in targets {
            match ast.node(*target) {
                Node::Index(base, key, _) => {
                    if let Node::Str(raw) = ast.node(*key) {
                        if let Some(name) = storm_lua_syntax::numeric::decode_lua_string(raw) {
                            written.names.insert(name.to_string());
                        }
                    } else if let Some(bid) = table_root_binding(ast, res, definitions, *base) {
                        written.dynamic_bases.insert(bid);
                    } else {
                        written.dynamic_anywhere = true;
                    }
                }
                Node::Methodname(_, name) => {
                    written.names.insert(ast.strings.get(*name).to_string());
                }
                _ => {}
            }
        }
    });
    written
}

/// The (base, key) of the index a call's callee reads, chasing
/// through bindings with exactly one definition like
/// `callee_member_name`.
fn callee_index(
    ast: &Ast,
    res: &Resolution,
    definitions: &HashMap<BindingId, Vec<NodeId>>,
    function: NodeId,
) -> Option<(NodeId, NodeId)> {
    let mut current = function;
    let mut visited = HashSet::new();
    loop {
        match ast.node(current) {
            Node::Index(base, key, _) => return Some((*base, *key)),
            Node::Name(_) => {
                current = chase_single_definition(ast, res, definitions, current, &mut visited)?;
            }
            _ => return None,
        }
    }
}

/// How the value a table constructor stores under the callee's
/// member name is classified for exact-route determination.
///
/// `Exact`: the value is itself an exact observer (an exact-observing
/// member read chased through aliases, a value read through a dynamic
/// key, or a chunk function, whose parameter the exact-route
/// propagation protects once the call is exact). A discretizing
/// builtin stored under an unrelated field name is invisible to the
/// name-based checks (candidate AE).
///
/// `Tolerant`: the value is positively identified as a builtin member
/// read that does not discretize, so the call's arguments may be
/// approximated.
///
/// `Unknown`: the field's value could not be identified -- the base
/// is not a plain name, the binding has no single definition to
/// chase (multiple definitions, an opaque definition such as a call
/// result, a parameter), the constructor chase fails, or the member
/// is absent. An unidentified value may be an exact observer, so the
/// caller must treat the call as exact; falling through to
/// approximation on identification failure was candidate S5
/// (w6 cycle 71).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FieldValueClass {
    Exact,
    Tolerant,
    Unknown,
}

fn callee_field_value_class(
    ast: &Ast,
    res: &Resolution,
    definitions: &HashMap<BindingId, Vec<NodeId>>,
    analyzer: &EffectAnalyzer<'_>,
    written_members: &WrittenMembers,
    base: NodeId,
    key: NodeId,
) -> FieldValueClass {
    let Node::Str(raw) = ast.node(key) else {
        return FieldValueClass::Unknown;
    };
    let Some(name) = storm_lua_syntax::numeric::decode_lua_string(raw) else {
        return FieldValueClass::Unknown;
    };
    let base = unparen(ast, base);
    let Node::Name(_) = ast.node(base) else {
        return FieldValueClass::Unknown;
    };
    let Some(bid) = res.node_bid.get(base as usize).copied().flatten() else {
        return FieldValueClass::Unknown;
    };
    let Some(exprs) = definitions.get(&bid) else {
        return FieldValueClass::Unknown;
    };
    if exprs.len() != 1 {
        return FieldValueClass::Unknown;
    }
    // The base's single definition may itself be an alias of the
    // constructor (`local t = {min = math.ceil}; math = t`): chase
    // the alias chain so this check reaches the pre-normalization
    // form instead of depending on single-use-local-sinking having
    // folded the alias into the root assignment (candidate S3).
    let mut visited = HashSet::new();
    let mut current = unparen(ast, exprs[0]);
    let fields = loop {
        match ast.node(current) {
            Node::Table(fields) => break fields.clone(),
            Node::Name(_) => {
                let Some(next) =
                    chase_single_definition(ast, res, definitions, current, &mut visited)
                else {
                    return FieldValueClass::Unknown;
                };
                current = unparen(ast, next);
            }
            _ => return FieldValueClass::Unknown,
        }
    };
    // The last field that can set the member wins, as at
    // construction time. A field with a non-literal key can set any
    // member, so it shadows every earlier field with an unknown
    // value: the member may hold an exact observer stored under a
    // key the name-based scan cannot see, so treat the call as
    // exact (the safe direction).
    let mut value = None;
    let mut shadowed_by_dynamic = false;
    for field in &fields {
        match field {
            storm_lua_syntax::TableField::Name(symbol, field_value) => {
                if ast.strings.get(*symbol) == name {
                    value = Some(*field_value);
                    shadowed_by_dynamic = false;
                }
            }
            storm_lua_syntax::TableField::KVar(field_key, field_value) => {
                if let Node::Str(field_raw) = ast.node(*field_key) {
                    if storm_lua_syntax::numeric::decode_lua_string(field_raw)
                        .is_some_and(|field_name| field_name == name)
                    {
                        value = Some(*field_value);
                        shadowed_by_dynamic = false;
                    }
                } else {
                    value = None;
                    shadowed_by_dynamic = true;
                }
            }
            storm_lua_syntax::TableField::Arr(_) => {}
        }
    }
    if shadowed_by_dynamic {
        return FieldValueClass::Exact;
    }
    let Some(value) = value else {
        return FieldValueClass::Unknown;
    };
    classify_field_value(
        ast,
        res,
        definitions,
        analyzer,
        written_members,
        current,
        unparen(ast, value),
    )
}

/// Classify one constructor field value (see `FieldValueClass`).
/// Name values are chased through single-definition bindings; any
/// chase failure is `Unknown`, never tolerant.
fn classify_field_value(
    ast: &Ast,
    res: &Resolution,
    definitions: &HashMap<BindingId, Vec<NodeId>>,
    analyzer: &EffectAnalyzer<'_>,
    written_members: &WrittenMembers,
    ctor_node: NodeId,
    value: NodeId,
) -> FieldValueClass {
    if callee_is_exact_member(ast, res, definitions, value) {
        return FieldValueClass::Exact;
    }
    match ast.node(value) {
        Node::Function(..) => FieldValueClass::Exact,
        Node::Name(_) => {
            let is_chunk_function = res
                .node_bid
                .get(value as usize)
                .copied()
                .flatten()
                .is_some_and(|bid| res.binding(bid).function_node.is_some());
            if is_chunk_function {
                return FieldValueClass::Exact;
            }
            let mut visited = HashSet::new();
            match chase_single_definition(ast, res, definitions, value, &mut visited) {
                Some(definition) => classify_field_value(
                    ast,
                    res,
                    definitions,
                    analyzer,
                    written_members,
                    ctor_node,
                    unparen(ast, definition),
                ),
                None => FieldValueClass::Unknown,
            }
        }
        Node::Index(index_base, field_key, _) => {
            if !matches!(ast.node(*field_key), Node::Str(_)) {
                // A value read through a dynamic key has no statically
                // known identity (existing exact treatment).
                return FieldValueClass::Exact;
            }
            // A member read with a non-exact name: tolerant when
            // builtin resolution positively identifies the read (its
            // root is unwritten and proven). Resolution is poisoned
            // when the root is written anywhere in the chunk, which
            // includes the very rebinding under analysis (`math = t`
            // for the constructor being classified): the field's
            // value was read at construction time, before that
            // rebinding executed, so fall back to a name-based
            // identification of the read's root. The fallback applies
            // only when the member name is never written in the
            // chunk and the root is either never rebound or rebound
            // solely to this same constructor (whose fields are the
            // ones being classified); through any other base the
            // read can denote any function.
            if analyzer.resolve_builtin_reference(value).is_some() {
                return FieldValueClass::Tolerant;
            }
            let Node::Str(raw) = ast.node(*field_key) else {
                return FieldValueClass::Unknown;
            };
            let Some(member) = storm_lua_syntax::numeric::decode_lua_string(raw) else {
                return FieldValueClass::Unknown;
            };
            if written_members.names.contains(member.as_str()) {
                return FieldValueClass::Unknown;
            }
            let Some(root_bid) = member_read_root_binding(ast, res, definitions, *index_base)
            else {
                return FieldValueClass::Unknown;
            };
            match definitions.get(&root_bid) {
                None => FieldValueClass::Tolerant,
                Some(exprs) if exprs.len() == 1 => {
                    let mut visited = HashSet::new();
                    let mut current = unparen(ast, exprs[0]);
                    loop {
                        match ast.node(current) {
                            Node::Table(_) => {
                                return if current == ctor_node {
                                    FieldValueClass::Tolerant
                                } else {
                                    FieldValueClass::Unknown
                                };
                            }
                            Node::Name(_) => {
                                let Some(next) = chase_single_definition(
                                    ast,
                                    res,
                                    definitions,
                                    current,
                                    &mut visited,
                                ) else {
                                    return FieldValueClass::Unknown;
                                };
                                current = unparen(ast, next);
                            }
                            _ => return FieldValueClass::Unknown,
                        }
                    }
                }
                _ => FieldValueClass::Unknown,
            }
        }
        _ => FieldValueClass::Unknown,
    }
}

/// Chase a member read's base expression through single-definition
/// name bindings to the global API root binding it denotes (`math`,
/// `string`, ...), if it denotes one. Returns `None` when the base
/// is not a name chain or the chain does not end at a global API
/// root binding.
fn member_read_root_binding(
    ast: &Ast,
    res: &Resolution,
    definitions: &HashMap<BindingId, Vec<NodeId>>,
    base: NodeId,
) -> Option<BindingId> {
    let mut current = unparen(ast, base);
    let mut visited = HashSet::new();
    loop {
        let Node::Name(_) = ast.node(current) else {
            return None;
        };
        let bid = res.node_bid.get(current as usize).copied().flatten()?;
        let binding = res.binding(bid);
        if binding.kind == BindingKind::Global && api_roots(ast.strings.get(binding.name)) {
            return Some(bid);
        }
        current = unparen(
            ast,
            chase_single_definition(ast, res, definitions, current, &mut visited)?,
        );
    }
}

/// Whether a call's callee is positively a chunk function: a
/// function literal, a binding declared as a chunk function, or a
/// name chasing through single definitions to one of those. Such
/// callees are identified, and their calls stay tolerant routes by
/// design (their parameters are protected by exact-route propagation
/// when the function itself is used in an exact route); every other
/// unidentified callee defaults to exact (candidate S5).
fn callee_is_chunk_function(
    ast: &Ast,
    res: &Resolution,
    definitions: &HashMap<BindingId, Vec<NodeId>>,
    function: NodeId,
) -> bool {
    let mut current = unparen(ast, function);
    let mut visited = HashSet::new();
    loop {
        match ast.node(current) {
            Node::Function(..) => return true,
            Node::Name(_) => {
                if let Some(bid) = res.node_bid.get(current as usize).copied().flatten() {
                    if res.binding(bid).function_node.is_some() {
                        return true;
                    }
                }
                match chase_single_definition(ast, res, definitions, current, &mut visited) {
                    Some(next) => current = unparen(ast, next),
                    None => return false,
                }
            }
            _ => return false,
        }
    }
}

fn callee_is_exact_member(
    ast: &Ast,
    res: &Resolution,
    definitions: &HashMap<BindingId, Vec<NodeId>>,
    function: NodeId,
) -> bool {
    let mut current = function;
    let mut visited = HashSet::new();
    loop {
        match ast.node(current) {
            Node::Index(_, key, _) => {
                // Member keys are raw quoted literals (), so the
                // name must be decoded before comparison -- comparing
                // the raw token against the bare name never matches.
                return match ast.node(*key) {
                    Node::Str(raw) => storm_lua_syntax::numeric::decode_lua_string(raw)
                        .is_some_and(|name| EXACT_MEMBER_NAMES.iter().any(|c| *c == name)),
                    _ => false,
                };
            }
            Node::Name(_) => {
                let Some(next) =
                    chase_single_definition(ast, res, definitions, current, &mut visited)
                else {
                    return false;
                };
                current = next;
            }
            _ => return false,
        }
    }
}

/// Builtin references whose calls observe an argument's exact value
/// or integrality (integer coercion or exact rendering).
const EXACT_BUILTIN_REFERENCES: &[&str] = &[
    "string.pack",
    "string.unpack",
    "string.format",
    "string.rep",
    "string.sub",
    "string.byte",
    "string.char",
    "string.find",
    "string.match",
    "string.gmatch",
    "string.gsub",
    "table.insert",
    "table.remove",
    "table.concat",
    "table.move",
    "math.floor",
    "math.ceil",
    "math.tointeger",
    "math.type",
    "tostring",
    "tonumber",
    "select",
];

fn is_exact_call(
    ast: &Ast,
    res: &Resolution,
    definitions: &HashMap<BindingId, Vec<NodeId>>,
    analyzer: &EffectAnalyzer<'_>,
    written_members: &WrittenMembers,
    id: NodeId,
) -> bool {
    let Node::Call(function, _, _) = ast.node(id) else {
        return false;
    };
    // The exact-observing member names are exact through any base:
    // by the time this pass runs inside a candidate, `math.ceil` may
    // already have been rewritten to an api-alias member read
    // (`a.ceil`), which builtin resolution no longer recognizes. A
    // user-defined `.floor` / `.ceil` method merely loses a rounding
    // opportunity, which is the safe direction.
    if callee_is_exact_member(ast, res, definitions, *function) {
        return true;
    }
    // A member assigned anywhere in the chunk can be rebound before
    // this call runs -- in particular to a discretizing builtin -- so
    // the call's arguments are exact routes as well (candidate AD).
    if let Some(name) = callee_member_name(ast, res, definitions, *function) {
        if written_members.names.contains(&name) {
            return true;
        }
    }
    // A callee read through a dynamic key has no statically known
    // identity, a member call through any base may have been
    // rebound by a dynamic-key write the name set cannot express,
    // and a constructor field can hold an exact observer under an
    // unrelated name: in each case the callee may discretize its
    // arguments, so the call is an exact route (candidate AE).
    if let Some((base, key)) = callee_index(ast, res, definitions, *function) {
        if !matches!(ast.node(key), Node::Str(_)) {
            return true;
        }
        if written_members.dynamic_anywhere {
            return true;
        }
        if let Some(root) = table_root_binding(ast, res, definitions, base) {
            if written_members.dynamic_bases.contains(&root) {
                return true;
            }
        }
        match callee_field_value_class(ast, res, definitions, analyzer, written_members, base, key)
        {
            FieldValueClass::Exact => return true,
            // The constructor field is positively identified as a
            // non-discretizing builtin member read, and every hazard
            // check above has passed.
            FieldValueClass::Tolerant => return false,
            FieldValueClass::Unknown => {}
        }
    }
    // Builtin resolution positively identifies the callee (an
    // unwritten root, or an alias the analyzer validated): the
    // resolved name decides in both directions.
    if let Some(name) = analyzer.resolve_builtin_reference(*function) {
        return EXACT_BUILTIN_REFERENCES.contains(&name.as_str());
    }
    // The callee could not be identified: it may be an exact
    // observer reached through a base or binding the identification
    // chain cannot chase (a parameter base, multiple definitions,
    // an opaque definition such as a call result, a rebinding of
    // the root through either), so the arguments are exact routes.
    // Identification failure is not evidence of a tolerant callee
    // (candidate S5, w6 cycle 71). A positively identified chunk
    // function is the one exception: its calls stay tolerant routes
    // by design.
    !callee_is_chunk_function(ast, res, definitions, *function)
}

#[allow(clippy::too_many_arguments)]
fn visit_function_returns(
    ast: &Ast,
    res: &Resolution,
    definitions: &HashMap<BindingId, Vec<NodeId>>,
    analyzer: &EffectAnalyzer<'_>,
    written_members: &WrittenMembers,
    function_node: NodeId,
    current: NodeId,
    exact_bids: &mut HashSet<BindingId>,
    exact_functions: &mut HashSet<BindingId>,
) {
    if current != function_node && matches!(ast.node(current), Node::Function(..)) {
        return;
    }
    if let Node::Return(expressions) = ast.node(current) {
        for expression in expressions {
            visit_exact_uses(
                ast,
                res,
                definitions,
                analyzer,
                written_members,
                *expression,
                true,
                exact_bids,
                exact_functions,
            );
        }
    }
    storm_lua_analysis::resolver::for_each_child(ast, current, &mut |child| {
        visit_function_returns(
            ast,
            res,
            definitions,
            analyzer,
            written_members,
            function_node,
            child,
            exact_bids,
            exact_functions,
        );
    });
}

fn child_is_exact(ast: &Ast, parent: NodeId, key: &str, inherited: bool, exact_call: bool) -> bool {
    if inherited || exact_call {
        return true;
    }
    match ast.node(parent) {
        Node::Bin(op, _, _) if EXACT_BINARY.contains(&op.as_str()) => true,
        Node::Fornum(..) if matches!(key, "a" | "b" | "c") => true,
        Node::Index(_, _, _) if key == "key" => true,
        _ => false,
    }
}

fn table_key_nodes(ast: &Ast, node: NodeId) -> HashSet<NodeId> {
    match ast.node(node) {
        Node::Table(fields) => fields
            .iter()
            .filter_map(|field| match field {
                storm_lua_syntax::TableField::KVar(key, _) => Some(*key),
                _ => None,
            })
            .collect(),
        _ => HashSet::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn visit_exact_uses(
    ast: &Ast,
    res: &Resolution,
    definitions: &HashMap<BindingId, Vec<NodeId>>,
    analyzer: &EffectAnalyzer<'_>,
    written_members: &WrittenMembers,
    id: NodeId,
    exact: bool,
    exact_bids: &mut HashSet<BindingId>,
    exact_functions: &mut HashSet<BindingId>,
) {
    if exact && matches!(ast.node(id), Node::Name(_)) {
        if let Some(bid) = res.node_bid.get(id as usize).copied().flatten() {
            exact_bids.insert(bid);
        }
    }
    if exact {
        if let Node::Call(function, _, _) = ast.node(id) {
            if matches!(ast.node(*function), Node::Name(_)) {
                if let Some(bid) = res.node_bid.get(*function as usize).copied().flatten() {
                    if exact_functions.insert(bid) {
                        if let Some(function_node) = res.binding(bid).function_node {
                            if let Node::Function(_, _, body) = ast.node(function_node) {
                                visit_function_returns(
                                    ast,
                                    res,
                                    definitions,
                                    analyzer,
                                    written_members,
                                    function_node,
                                    *body,
                                    exact_bids,
                                    exact_functions,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    let exact_call = is_exact_call(ast, res, definitions, analyzer, written_members, id);
    let table_keys = table_key_nodes(ast, id);
    storm_lua_syntax::ast_utils::for_each_child_key(ast, id, &mut |key, child| {
        visit_exact_uses(
            ast,
            res,
            definitions,
            analyzer,
            written_members,
            child,
            child_is_exact(ast, id, key, exact, exact_call) || table_keys.contains(&child),
            exact_bids,
            exact_functions,
        );
    });
}

fn propagate_exact_definitions(
    ast: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    definitions: &HashMap<BindingId, Vec<NodeId>>,
    written_members: &WrittenMembers,
    exact_bids: &mut HashSet<BindingId>,
    exact_functions: &mut HashSet<BindingId>,
) {
    loop {
        let previous = (exact_bids.len(), exact_functions.len());
        for bid in exact_bids.iter().copied().collect::<Vec<_>>() {
            for expression in definitions.get(&bid).into_iter().flatten() {
                visit_exact_uses(
                    ast,
                    res,
                    definitions,
                    analyzer,
                    written_members,
                    *expression,
                    true,
                    exact_bids,
                    exact_functions,
                );
            }
        }
        // Exact consumers can reach a function through an intermediate binding.
        // Protect the function's local computations and their external inputs too.
        for bid in exact_functions.iter().copied().collect::<Vec<_>>() {
            if let Some(function) = res.binding(bid).function_node {
                visit_exact_uses(
                    ast,
                    res,
                    definitions,
                    analyzer,
                    written_members,
                    function,
                    true,
                    exact_bids,
                    exact_functions,
                );
            }
        }
        if previous == (exact_bids.len(), exact_functions.len()) {
            break;
        }
    }
}

struct RewriteContext<'a> {
    source: &'a Ast,
    res: &'a Resolution,
    definitions: &'a HashMap<BindingId, Vec<NodeId>>,
    analyzer: &'a EffectAnalyzer<'a>,
    written_members: &'a WrittenMembers,
    exact_bids: &'a HashSet<BindingId>,
    exact_function_nodes: HashSet<NodeId>,
    tolerance: NumericTolerance,
    shortened: usize,
}

fn rewrite(target: &mut Ast, context: &mut RewriteContext<'_>, id: NodeId, exact: bool) -> NodeId {
    let exact = exact || context.exact_function_nodes.contains(&id);
    if let Node::Num(raw) = context.source.node(id) {
        if !exact {
            let value = num_val(raw);
            if value.is_finite() && value.fract() != 0.0 {
                if let Some(candidate) = shortest_approximation(target, value, context.tolerance) {
                    let original_len = measure_expr(context.source, id);
                    if candidate.len() < original_len {
                        context.shortened += 1;
                        let replacement = target.num(&candidate);
                        target.nodes.derive_from(
                            replacement,
                            &context.source.nodes,
                            id,
                            "numeric-literal-approximation",
                        );
                        if target.nodes.tracks_origins() {
                            let Node::Num(emitted) = target.node(replacement) else {
                                unreachable!()
                            };
                            let after = num_val(emitted);
                            let budget_abs = context.tolerance.abs.min(PER_LITERAL_ABS_CAP);
                            let budget_rel = context.tolerance.rel.min(PER_LITERAL_REL_CAP);
                            target.nodes.explain(
                                replacement,
                                storm_lua_syntax::explanation::OptimizationReason::decision(
                                    "numeric-literal-approximation",
                                    "finite-nonintegral-literal-within-budget-and-shorter",
                                    [
                                        ("originalBits", format!("{:016x}", value.to_bits())),
                                        ("replacementBits", format!("{:016x}", after.to_bits())),
                                        (
                                            "absoluteErrorBits",
                                            format!("{:016x}", (after - value).abs().to_bits()),
                                        ),
                                        (
                                            "absoluteBudgetBits",
                                            format!("{:016x}", budget_abs.to_bits()),
                                        ),
                                        (
                                            "relativeBudgetBits",
                                            format!("{:016x}", budget_rel.to_bits()),
                                        ),
                                        ("expressionSizeBefore", original_len.to_string()),
                                    ],
                                ),
                            );
                        }
                        return replacement;
                    }
                }
            }
        }
    }

    let node = context.source.node(id).clone();
    let exact_call = is_exact_call(
        context.source,
        context.res,
        context.definitions,
        context.analyzer,
        context.written_members,
        id,
    );
    let table_keys = table_key_nodes(context.source, id);
    let rewritten = match node {
        Node::Assign(vs, es) => {
            let new_vs = vs
                .iter()
                .map(|child| {
                    rewrite(
                        target,
                        context,
                        *child,
                        child_is_exact(context.source, id, "vs", exact, exact_call),
                    )
                })
                .collect::<Vec<_>>();
            let new_es = es
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    let target_exact = vs
                        .get(index)
                        .and_then(|target_id| {
                            if matches!(context.source.node(*target_id), Node::Name(_)) {
                                context
                                    .res
                                    .node_bid
                                    .get(*target_id as usize)
                                    .copied()
                                    .flatten()
                            } else {
                                None
                            }
                        })
                        .is_some_and(|bid| context.exact_bids.contains(&bid));
                    rewrite(target, context, *child, exact || exact_call || target_exact)
                })
                .collect();
            Node::Assign(new_vs, new_es)
        }
        Node::Local(names, es) => {
            let bids = context
                .res
                .node_bids
                .get(id as usize)
                .cloned()
                .unwrap_or_default();
            let new_es = es
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    let target_exact = bids
                        .get(index)
                        .is_some_and(|bid| context.exact_bids.contains(bid));
                    rewrite(target, context, *child, exact || exact_call || target_exact)
                })
                .collect();
            Node::Local(names, new_es)
        }
        other => {
            let mut child_index = 0usize;
            let mut keyed_children = Vec::new();
            storm_lua_syntax::ast_utils::for_each_child_key(
                context.source,
                id,
                &mut |key, child| {
                    keyed_children.push((key, child));
                },
            );
            let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&other, &mut |child| {
                let key = keyed_children
                    .get(child_index)
                    .map(|entry| entry.0)
                    .unwrap_or("");
                child_index += 1;
                rewrite(
                    target,
                    context,
                    child,
                    child_is_exact(context.source, id, key, exact, exact_call)
                        || table_keys.contains(&child),
                )
            });
            mapped
        }
    };
    target
        .nodes
        .rewrite(id, rewritten, "numeric-literal-approximation");
    id
}

pub fn shorten_numeric_literals_with_options(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    tolerance: NumericTolerance,
) -> PassResult {
    if !aggressive || (tolerance.abs <= 0.0 && tolerance.rel <= 0.0) {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let res = resolve(ast, root);
    let analyzer = EffectAnalyzer::new(ast, &res, root, true);
    let definitions = collect_definitions(ast, &res, root);
    let written_members = collect_written_members(ast, &res, &definitions, root);
    let mut exact_bids = HashSet::new();
    let mut exact_functions = HashSet::new();
    visit_exact_uses(
        ast,
        &res,
        &definitions,
        &analyzer,
        &written_members,
        root,
        false,
        &mut exact_bids,
        &mut exact_functions,
    );
    propagate_exact_definitions(
        ast,
        &res,
        &analyzer,
        &definitions,
        &written_members,
        &mut exact_bids,
        &mut exact_functions,
    );

    let source_nodes = ast.nodes.clone();
    let mut source = storm_lua_syntax::ast_utils::inherit_ast(ast);
    source.nodes = source_nodes;
    let source_res = resolve(&source, root);
    let source_analyzer = EffectAnalyzer::new(&source, &source_res, root, true);
    let mut context = RewriteContext {
        source: &source,
        res: &source_res,
        definitions: &definitions,
        analyzer: &source_analyzer,
        written_members: &written_members,
        exact_bids: &exact_bids,
        exact_function_nodes: exact_functions
            .iter()
            .filter_map(|bid| res.binding(*bid).function_node)
            .collect(),
        tolerance,
        shortened: 0,
    };
    let out = rewrite(ast, &mut context, root, false);
    PassResult {
        root: out,
        saved: Some(context.shortened as u64),
        details: None,
    }
}

pub fn shorten_numeric_literals(ast: &mut Ast, root: NodeId) -> PassResult {
    shorten_numeric_literals_with_options(
        ast,
        root,
        true,
        NumericTolerance {
            abs: 1e-6,
            rel: 1e-6,
        },
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = shorten_numeric_literals(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn shortens_tolerant_non_integral_literals() {
        assert_eq!(output("x=1.23456789012345"), "x=1.23456789");
    }

    #[test]
    fn preserves_bit_and_index_routes() {
        let out = output("x=1.23456789012345&3 y=t[1.23456789012345]");
        assert_eq!(out.matches("1.23456789012345").count(), 2, "{out}");
    }

    #[test]
    fn preserves_comparison_routes() {
        // Candidate P (w9 cycle 66): 1.0000000001 rounds to 1 within
        // tolerance, which would flip the equality to true.
        let out = output("x = (math.abs(1.0000000001) == 1)");
        assert!(out.contains("1.0000000001"), "{out}");
        let out = output("x = (1.0000000001 < y)");
        assert!(out.contains("1.0000000001"), "{out}");
    }

    #[test]
    fn preserves_discretizing_call_routes() {
        // Candidate Q (w9 cycle 66): 3.0000000000000004 rounds to 3
        // within tolerance, but math.ceil of the rounded value is 3,
        // not 4.
        let out = output("x = math.sqrt(math.ceil(math.max(2.5, 3.0000000000000004)))");
        assert!(out.contains("3.0000000000000004"), "{out}");
    }

    #[test]
    fn rendering_and_integer_coercing_calls_are_exact_routes() {
        // Candidate U (w9 cycle 67): string.format coerces a %d
        // argument to an integer (3.000000001 has no integer
        // representation and is an error), string.rep coerces its
        // count the same way, and tostring renders the exact value
        // (3.000000001 is eleven characters, 3 is one).  Approximating
        // any of these literals changes observable behavior.
        let out = output("x = string.format(\"n=%d\", 3.000000001)");
        assert!(out.contains("3.000000001"), "{out}");
        let out = output("x = string.rep(\"x\", 2.9999999999)");
        assert!(out.contains("2.9999999999"), "{out}");
        let out = output("x = tostring(3.000000001)");
        assert!(out.contains("3.000000001"), "{out}");
    }

    #[test]
    fn rebound_member_call_is_an_exact_route() {
        // Candidate AD (w9 cycle 68): `math.min` is rebound to
        // `math.ceil` inside the loop, so from the second iteration
        // the call discretizes its argument; rounding
        // -2.999999999 to -3 within tolerance changed the printed
        // value from -2 to -3.  A member assigned anywhere in the
        // chunk makes calls through that member name exact routes.
        let out = output(
            "for k, e in pairs(T) do screen.drawRect(math.min(-2.999999999, math.exp(27)), 52, 8, 3) math.min = math.ceil end",
        );
        assert!(out.contains("-2.999999999"), "{out}");
        // Without any rebinding of the member, the same call still
        // rounds within tolerance.
        let out = output("x = math.min(-2.999999999, math.exp(27))");
        assert!(!out.contains("-2.999999999"), "{out}");
    }

    #[test]
    fn dynamic_key_write_makes_member_calls_exact_routes() {
        // Candidate AE (w9 cycle 69): the cycle-68 protection
        // recorded only member names written through a literal key.
        // A write through a dynamic key rebinds the same member --
        // here `math.min` becomes `math.ceil` before the call, and
        // rounding -2.999999999 to -3 changed the result from -2
        // to -3.
        let out =
            output("K = \"min\" math[K] = math.ceil x = math.min(-2.999999999, math.exp(27))");
        assert!(out.contains("-2.999999999"), "{out}");
        let out = output(
            "local t = {min = math.ceil} for key, v in pairs(t) do math[key] = v end x = math.min(-2.999999999, math.exp(27))",
        );
        assert!(out.contains("-2.999999999"), "{out}");
        // A dynamic write into a fresh local table poisons only
        // calls through that same table, not unrelated member
        // calls.
        let out = output(
            "local t = {} local k = \"x\" t[k] = 1 x = math.min(-2.999999999, math.exp(27))",
        );
        assert!(!out.contains("-2.999999999"), "{out}");
        // A call through that same table is protected (both by the
        // dynamic write and by the field's value).
        let out =
            output("local t = {go = math.ceil} local k = \"x\" t[k] = 1 x = t.go(-2.999999999)");
        assert!(out.contains("-2.999999999"), "{out}");
    }

    #[test]
    fn deep_alias_chains_are_exact_routes_without_folding() {
        // Candidate S3(ii) (w6 cycles 64-65): the single-definition
        // chases stopped after four hops, so a depth-four bare alias
        // chain was only protected when single-use-local-sinking had
        // already folded the chain away. Fed to this pass in the
        // unfolded (pre-normalization) form, the chain must still
        // reach the member write / the exact member.
        let out = output(
            "math.min = math.ceil local f1 = math.min local f2 = f1 local f3 = f2 local f4 = f3 x = f4(-2.999999999, math.exp(27))",
        );
        assert!(out.contains("-2.999999999"), "{out}");
        let out = output(
            "math.min = math.ceil local f1 = math.min local f2 = f1 local f3 = f2 local f4 = f3 local f5 = f4 local f6 = f5 x = f6(-2.999999999, math.exp(27))",
        );
        assert!(out.contains("-2.999999999"), "{out}");
        // A chain that ends at an exact member, with no write at all.
        let out = output(
            "local f1 = math.ceil local f2 = f1 local f3 = f2 local f4 = f3 local f5 = f4 local f6 = f5 x = f6(3.0000000000000004)",
        );
        assert!(out.contains("3.0000000000000004"), "{out}");
        // A cyclic definition graph (mutual single assignments)
        // bails out instead of looping. The callee cannot be
        // identified, so its arguments are exact routes (candidate
        // S5: identification failure resolves to exact, not to
        // approximation). The call itself can never execute -- both
        // bindings are nil -- so this is unobservable either way.
        let out = output("x = y y = x z = x(3.0000000000000004)");
        assert!(out.contains("z=x(3.0000000000000004)"), "{out}");
    }

    #[test]
    fn root_rebind_through_alias_variable_is_an_exact_route() {
        // Candidate S3(iv) (w6 cycle 65): `math = t` rebinds the
        // root to t's constructor, whose `min` field holds the
        // discretizer. The field-value check required the root's
        // definition to be the constructor itself, so the protection
        // only appeared once sinking folded `t` into the assignment.
        let out =
            output("local t = {min = math.ceil} math = t x = math.min(-2.999999999, math.exp(27))");
        assert!(out.contains("-2.999999999"), "{out}");
        // A dynamic-key field after the named field can overwrite
        // the member at construction time, so the value is unknown
        // and the call is exact; before it, the named field wins.
        let out = output(
            "local k = \"min\" local t = {min = math.sqrt, [k] = math.ceil} math = t x = math.min(-2.999999999, math.exp(27))",
        );
        assert!(out.contains("-2.999999999"), "{out}");
        let out = output(
            "local k = \"min\" local t = {[k] = math.ceil, min = math.sqrt} math = t x = math.min(-2.999999999, math.exp(27))",
        );
        assert!(!out.contains("-2.999999999"), "{out}");
    }

    #[test]
    fn root_rebind_through_global_alias_is_an_exact_route() {
        // The m108_03 shape (w6 cycle 66 mutation of ad14): the
        // intermediate binding is a global assignment, not a local.
        // The constructor chase must fire on a global root as well.
        let out = output("t = {min = math.ceil} math = t x = math.min(-2.999999999, math.exp(27))");
        assert!(out.contains("-2.999999999"), "{out}");
    }

    #[test]
    fn dynamic_key_and_field_alias_calls_are_exact_routes() {
        // Candidate AE: a callee read through a dynamic key has no
        // statically known identity and may be a discretizing
        // builtin.
        let out = output("K = \"ceil\" x = math[K](-2.999999999)");
        assert!(out.contains("-2.999999999"), "{out}");
        // Candidate AE: a table constructor field can hold a
        // discretizing builtin under an unrelated name.
        let out = output("local t = {go = math.ceil} x = t.go(-2.999999999)");
        assert!(out.contains("-2.999999999"), "{out}");
        // ...or a chunk function that discretizes its parameter.
        let out = output(
            "local function disc(v) return math.ceil(v) end local t = {go = disc} x = t.go(-2.999999999)",
        );
        assert!(out.contains("-2.999999999"), "{out}");
    }

    #[test]
    fn aliased_discretizing_call_is_exact_without_builtin_resolution() {
        // Candidate U (w9 cycle 67): when the callee is a bare local
        // whose sole definition is a discretizing member read (the
        // api-alias shape), the literal stays exact even though
        // builtin resolution cannot prove the callee -- and a chunk
        // that mutates the root (as below) poisons that resolution
        // entirely.
        let out = output("local c = math.ceil x = c(3.0000000000000004)");
        assert!(out.contains("3.0000000000000004"), "{out}");
        let out = output(
            "math.floor = math.ceil x = math.sqrt(math.abs(math.ceil(math.max(2.5, 3.0000000000000004))))",
        );
        assert!(out.contains("3.0000000000000004"), "{out}");
    }

    #[test]
    fn exact_binding_dataflow_is_transitive() {
        let out = output("local a=1.23456789012345 local b=a x=b&3");
        assert!(out.contains("1.23456789012345"), "{out}");
    }

    #[test]
    fn exact_hex_canonicalization_is_shorter_and_subtype_safe() {
        let source = "a=.10000000149011612 b=-.6000000238418579 c=65536.0 d=65536 e=5e-324";
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = canonicalize_exact_float_literals(&mut ast, root);
        let out = Printer::new(&ast, false).output(result.root);
        assert_eq!(out, "a=0x.199999a b=-0x.99999a c=0x1p16 d=65536 e=5e-324");
        assert_eq!(result.saved, Some(17));
    }

    #[test]
    fn unidentifiable_callees_are_exact_routes() {
        // Candidate S5 (w6 cycle 71): the exactness identification
        // chain used to fall through to approximation whenever it
        // could not chase the callee -- a parameter base, multiple
        // definitions, an opaque definition (a call result), or a
        // root rebound through any of those. Identification failure
        // means the callee is unknown and may be an exact observer
        // (here `math.ceil` / `math.floor` in every shape), so the
        // arguments are exact routes. Plain/safe returned -2 where
        // opt/target returned -3 for each of these.
        let kept = [
            // Parameter base: `t.min` with t a function parameter.
            "local function g(t) return t.min(-2.999999999, 1e30) end x = g({min = math.ceil})",
            // Root rebound through a multi-definition alias (da11).
            "local t = {min = math.ceil} local u = t u = {min = math.ceil} math = u x = math.min(-2.999999999, 1e30)",
            // Root rebound to a call result (da14).
            "local t = (function() return {min = math.ceil} end)() math = t x = math.min(-2.999999999, 1e30)",
            // Local callee with multiple definitions (da13).
            "math.min = math.ceil local f1 = math.min local f2 = f1 f1 = f2 x = f1(-2.999999999, 1e30)",
            // Constructor binding with multiple definitions.
            "local t = {min = math.ceil} t = {min = math.ceil} x = t.min(-2.999999999, 1e30)",
            // Global alias with multiple definitions.
            "u = {min = math.ceil} u = {min = math.ceil} math = u x = math.min(-2.999999999, 1e30)",
            // Same member read under multiple definitions, with the
            // member rebound to a discretizer.
            "math.min = math.ceil local f = math.min f = math.min x = f(-2.999999999, 1e30)",
            // Index base (`a.b.min`).
            "local a = {b = {min = math.ceil}} x = a.b.min(-2.999999999, 1e30)",
            // Call-result base (`f().min`).
            "local function f() return {min = math.ceil} end x = f().min(-2.999999999, 1e30)",
            // A callee that is itself a parameter.
            "local function g(cb) return cb(-2.999999999) end x = g(math.ceil)",
        ];
        for source in kept {
            let out = output(source);
            assert!(out.contains("-2.999999999"), "{source} -> {out}");
        }
        // The max/floor variant: 2.999999999 must not round to 3.
        let out = output(
            "local function g(t) return t.max(2.999999999, -1e30) end x = g({max = math.floor})",
        );
        assert!(out.contains("2.999999999"), "{out}");
        // Controls that stay tolerant: a positively identified
        // builtin (registered tolerance class) and a constructor
        // field positively identified as a tolerant builtin read,
        // including through a single-definition root rebinding to
        // that same constructor.
        let rounded = [
            "x = math.min(-2.999999999, 1e30)",
            "local t = {min = math.sqrt} x = t.min(-2.999999999, 1e30)",
            "local t = {min = math.sqrt} math = t x = math.min(-2.999999999, 1e30)",
            // A positively identified chunk function stays a
            // tolerant route by design.
            "local function f(v) return v + 1 end x = f(1.23456789012345)",
        ];
        for source in rounded {
            let out = output(source);
            assert!(
                !out.contains("-2.999999999") && !out.contains("1.23456789012345"),
                "{source} -> {out}"
            );
        }
    }

    #[test]
    fn disabled_for_zero_tolerance() {
        let (mut ast, root) = parse_source("x=1.23456789012345").expect("parse");
        let result = shorten_numeric_literals_with_options(
            &mut ast,
            root,
            true,
            NumericTolerance { abs: 0.0, rel: 0.0 },
        );
        assert_eq!(
            Printer::new(&ast, false).output(result.root),
            "x=1.23456789012345"
        );
    }
}
