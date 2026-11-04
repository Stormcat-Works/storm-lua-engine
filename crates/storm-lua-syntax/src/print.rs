//! Compact and readable Lua source emission.

// Canonical compact/pretty printer。最終TS版の出力は frozen migration regression として保持。
// 2 系統（compact / pretty）は zeroCostNewlines フラグで切り替える。

use std::borrow::Cow;

use crate::ast::{Ast, Node, NodeId, TableField};
use crate::lexer::is_keyword;
#[cfg(test)]
use crate::numeric::qstr;
use crate::numeric::{
    decode_lua_string, integer_literal_value, normalize_num_literal, qstr_bracket_key,
};

/// 演算子の結合優先度 [lhs_assoc_prec, rhs_assoc_prec]（lexer.ts の BIN_PREC 相当）。
pub(crate) fn bin_prec(op: &str) -> (u8, u8) {
    match op {
        "or" => (1, 2),
        "and" => (2, 3),
        "<" | ">" | "<=" | ">=" | "~=" | "==" => (3, 4),
        "|" => (4, 5),
        "~" => (5, 6),
        "&" => (6, 7),
        "<<" | ">>" => (7, 8),
        ".." => (9, 8),
        "+" | "-" => (10, 11),
        "*" | "/" | "//" | "%" => (11, 12),
        "^" => (14, 13),
        _ => (0, 0),
    }
}

/// Return the shortest prefix of an assignment/local initializer list that
/// preserves Lua's value-adjustment semantics when the removed suffix consists
/// only of literal `nil` expressions. Missing RHS values are initialized to nil,
/// but making a call/vararg the final expression can expose additional results.
/// Keep one trailing nil in that case when there are still unfilled targets.
pub(crate) fn trimmed_nil_rhs_len(
    ast: &Ast,
    target_count: usize,
    values: &[NodeId],
    allow_empty: bool,
) -> usize {
    let mut len = values.len();
    while len > 0 && matches!(ast.node(values[len - 1]), Node::Nil) {
        len -= 1;
    }
    if len == values.len() {
        return len;
    }
    if len == 0 {
        return if allow_empty { 0 } else { 1 };
    }
    if target_count > len && matches!(ast.node(values[len - 1]), Node::Call(..) | Node::Vararg) {
        len += 1;
    }
    len
}

/// Whether a condition expression is provably free of observable effects
/// and cannot throw, so skipping its evaluation is unobservable. This
/// mirrors the effect model's rules for these node kinds
/// (storm-lua-analysis effects: `and`/`or`/`==`/`~=` and `not` add no
/// throw, Index always throws, calls and arithmetic/concatenation may
/// throw or call). A table constructor is safe only when every field is
/// an Arr/Name field with a safe value: a computed KVar key could be nil,
/// which throws. Function creation and vararg reads have no effects.
/// Anything else is conservatively unsafe.
fn is_effect_free_condition(ast: &Ast, cond: NodeId) -> bool {
    let cond = match ast.node(cond) {
        Node::Paren(inner) => return is_effect_free_condition(ast, *inner),
        _ => cond,
    };
    match ast.node(cond) {
        Node::Nil | Node::Bool(_) | Node::Num(_) | Node::Str(_) => true,
        Node::Name(_) => true,
        Node::Vararg => true,
        Node::Function(..) => true,
        Node::Bin(op, left, right) => {
            matches!(op.as_str(), "and" | "or" | "==" | "~=")
                && is_effect_free_condition(ast, *left)
                && is_effect_free_condition(ast, *right)
        }
        Node::Un(op, operand) => op.as_str() == "not" && is_effect_free_condition(ast, *operand),
        Node::Table(fields) => fields.iter().all(|field| match field {
            TableField::Arr(value) => is_effect_free_condition(ast, *value),
            TableField::Name(_, value) => is_effect_free_condition(ast, *value),
            TableField::KVar(_, _) => false,
        }),
        _ => false,
    }
}

/// Whether a statement is a dead `if` that printing can drop entirely: a
/// single arm whose then block is empty, with no else block or an empty
/// else block, and an effect-free, non-throwing condition. Dropping it
/// evaluates nothing and executes nothing, so it is unobservable. The
/// check runs on the AST block before printing, so no emissions are
/// recorded for the dropped statement (like the empty-else omission).
/// One `local` statement inside a print-time merge group: its statement
/// node, its binding names, and the initializer prefix the printer keeps
/// (trailing literal nils are already dropped by `trimmed_nil_rhs_len`).
pub(crate) struct LocalMember {
    pub(crate) stmt: NodeId,
    pub(crate) names: Vec<crate::ast::SymbolId>,
    pub(crate) kept: Vec<NodeId>,
}

/// One item of a block after grouping: a lone statement, or a run of
/// consecutive `local` statements printed as one merged statement.
pub(crate) enum BlockItem {
    Single(NodeId),
    Merged(Vec<LocalMember>),
}

/// Whether any initializer expression in `exprs` mentions (anywhere in its
/// subtree, including nested function bodies) a name from `names`. Such a
/// reference resolves to the new binding in the separate form but to the
/// outer binding in the merged form, so it blocks merging.
fn rhs_mentions(ast: &Ast, exprs: &[NodeId], names: &[crate::ast::SymbolId]) -> bool {
    let mut stack: Vec<NodeId> = exprs.to_vec();
    while let Some(id) = stack.pop() {
        if let Node::Name(sym) = ast.node(id) {
            if names.contains(sym) {
                return true;
            }
        }
        crate::ast_utils::for_each_child_key(ast, id, &mut |_, child| stack.push(child));
    }
    false
}

/// Mapping-aware variant of `rhs_mentions`: compare post-substitution
/// spellings instead of stored spellings, so merge decisions on an
/// unrenamed tree agree with the stock printer's decisions on the
/// renamed tree (where the same occurrences print with substituted
/// spellings). `names` are post-substitution member spellings.
/// Every `Name` node in the walked subtree is a reference: binding
/// names live in their declaration nodes (`Local`/`Forin`/`Function`),
/// never as `Name` children.
fn rhs_mentions_mapped(
    ast: &Ast,
    exprs: &[NodeId],
    names: &[&str],
    resolver: &dyn crate::size::SizeNameResolver,
) -> bool {
    let mut stack: Vec<NodeId> = exprs.to_vec();
    while let Some(id) = stack.pop() {
        if let Node::Name(sym) = ast.node(id) {
            let spelling = resolver
                .replacement(id, crate::parser::NameSite::Reference)
                .unwrap_or_else(|| ast.strings.get(*sym));
            if names.contains(&spelling) {
                return true;
            }
        }
        crate::ast_utils::for_each_child_key(ast, id, &mut |_, child| stack.push(child));
    }
    false
}

/// Group a block's statements exactly as printing shapes them:
/// dead `if` statements print as nothing and are filtered out, so
/// the remaining statements join with exactly one separator each
/// (filtering after printing would leave a doubled one).
/// Consecutive `local` statements are merged into one when the
/// merge preserves Lua's value-adjustment semantics:
///  * every non-final member keeps exactly one initializer per
///    name (otherwise later values would shift into its names);
///  * a member's initializers never mention a name bound by an
///    earlier member of the group (in the separate form that
///    name already means the new binding, including inside
///    nested function bodies captured by the initializer);
///  * no name repeats across members;
///  * a member with no kept initializers may only close a group
///    whose last kept initializer is not a call/vararg (which
///    would otherwise expand into the new names).
///
/// A member whose kept prefix differs in length from its names
/// (trailing nils, or extra values) closes the group, since its
/// trailing values are only positionally safe at the very end.
///
/// Shared by `Printer::block` and size measurement (`crate::size`),
/// so the measured size of a block always matches its printed shape.
pub(crate) fn block_items(
    ast: &Ast,
    statements: &[NodeId],
    name_resolver: Option<&dyn crate::size::SizeNameResolver>,
) -> Vec<BlockItem> {
    let live: Vec<NodeId> = statements
        .iter()
        .copied()
        .filter(|statement| !is_dead_if_statement(ast, *statement))
        .collect();
    let mut items: Vec<BlockItem> = Vec::new();
    for stmt in live {
        let (names, es) = match ast.node(stmt) {
            Node::Local(names, es) => (names.clone(), es.clone()),
            _ => {
                items.push(BlockItem::Single(stmt));
                continue;
            }
        };
        let keep = trimmed_nil_rhs_len(ast, names.len(), &es, true);
        let member = LocalMember {
            stmt,
            names,
            kept: es[..keep].to_vec(),
        };
        let can_append = match items.last() {
            Some(BlockItem::Merged(members)) => {
                let open = members.iter().all(|m| m.kept.len() == m.names.len());
                // With a spelling substitution, merge decisions must
                // compare post-substitution spellings: on the renamed
                // tree the stock printer sees exactly those spellings,
                // and shadowed same-spelling members (which the stock
                // printer blocks on the unrenamed tree) become
                // distinct names there. Without a substitution the
                // stock path below is unchanged.
                let (overlap, mentions) = match name_resolver {
                    Some(resolver) => {
                        let group_spellings: Vec<&str> = members
                            .iter()
                            .flat_map(|m| {
                                m.names.iter().enumerate().map(|(i, n)| {
                                    mapped_spelling_of(
                                        ast,
                                        name_resolver,
                                        m.stmt,
                                        crate::parser::NameSite::Binding(i as u32),
                                        *n,
                                    )
                                })
                            })
                            .collect();
                        let overlap = member.names.iter().enumerate().any(|(i, n)| {
                            let spelling = mapped_spelling_of(
                                ast,
                                name_resolver,
                                member.stmt,
                                crate::parser::NameSite::Binding(i as u32),
                                *n,
                            );
                            group_spellings.contains(&spelling)
                        });
                        let mentions =
                            rhs_mentions_mapped(ast, &member.kept, &group_spellings, resolver);
                        (overlap, mentions)
                    }
                    None => {
                        let group_names: Vec<crate::ast::SymbolId> = members
                            .iter()
                            .flat_map(|m| m.names.iter().copied())
                            .collect();
                        let overlap = member.names.iter().any(|n| group_names.contains(n));
                        let mentions = rhs_mentions(ast, &member.kept, &group_names);
                        (overlap, mentions)
                    }
                };
                let expands = member.kept.is_empty()
                    && match members.last().and_then(|m| m.kept.last()) {
                        Some(last) => {
                            matches!(ast.node(*last), Node::Call(..) | Node::Vararg)
                        }
                        None => false,
                    };
                open && !overlap && !mentions && !expands
            }
            _ => false,
        };
        if can_append {
            if let Some(BlockItem::Merged(members)) = items.last_mut() {
                members.push(member);
            }
        } else {
            items.push(BlockItem::Merged(vec![member]));
        }
    }
    items
}

/// Post-substitution spelling for one identifier occurrence: the
/// resolver's replacement when present, else the AST's stored spelling.
fn mapped_spelling_of<'x>(
    ast: &'x Ast,
    resolver: Option<&'x dyn crate::size::SizeNameResolver>,
    node: NodeId,
    site: crate::parser::NameSite,
    symbol: crate::ast::SymbolId,
) -> &'x str {
    if let Some(resolver) = resolver {
        if let Some(replacement) = resolver.replacement(node, site) {
            return replacement;
        }
    }
    ast.strings.get(symbol)
}

fn is_dead_if_statement(ast: &Ast, statement: NodeId) -> bool {
    let Node::If(arms, else_block) = ast.node(statement) else {
        return false;
    };
    if arms.len() != 1 {
        return false;
    }
    let then_empty = matches!(
        ast.node(arms[0].body),
        Node::Block(statements) if statements.is_empty()
    );
    if !then_empty {
        return false;
    }
    let else_empty = match else_block {
        None => true,
        Some(block) => matches!(
            ast.node(*block),
            Node::Block(statements) if statements.is_empty()
        ),
    };
    else_empty && is_effect_free_condition(ast, arms[0].cond)
}

/// Whether a numeric-for step is an integer literal 1, so omitting it is
/// exactly the default step. Lua's default step is integer 1, so the loop
/// kind (integer vs float) is unchanged only for integer spellings: a
/// float step such as `1.0` or `1e0` must keep printing, because dropping
/// it would turn a float loop into an integer loop (observable via
/// math.type, overflow, and integer division). Paren-wrapped steps are
/// unwrapped: parentheses add no value. The printer's render_num
/// preserves the integer/float subtype, so an integer spelling here is
/// an integer step in the printed code too.
fn is_int_one_step(ast: &Ast, step: NodeId) -> bool {
    let step = match ast.node(step) {
        Node::Paren(inner) => return is_int_one_step(ast, *inner),
        _ => step,
    };
    let Node::Num(raw) = ast.node(step) else {
        return false;
    };
    let raw: &str = raw.as_ref();
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        return !hex.is_empty()
            && hex.bytes().all(|b| b.is_ascii_hexdigit())
            && hex.trim_start_matches('0') == "1";
    }
    !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()) && raw.trim_start_matches('0') == "1"
}

fn needs_sep(a: &str, b: &str) -> bool {
    let (Some(x), Some(y)) = (a.chars().next_back(), b.chars().next()) else {
        return false;
    };
    needs_sep_chars(x, y)
}

fn needs_sep_chars(x: char, y: char) -> bool {
    if (x.is_ascii_alphanumeric() || x == '_') && (y.is_ascii_alphanumeric() || y == '_') {
        return true;
    }
    if x == '-' && y == '-' {
        return true;
    }
    if x == '.' && y == '.' {
        return true;
    }
    // A number followed by `..` must not be printed as `1..`: the lexer can
    // consume the first dot as part of the numeric token.  Keep the operator
    // boundary explicit while retaining the compact form everywhere else.
    if x.is_ascii_digit() && y == '.' {
        return true;
    }
    false
}

#[inline]
fn lexical_boundary_may_retokenize(left: u8, right: u8) -> bool {
    matches!(
        (left, right),
        (b'/', b'/')
            | (b'<', b'<')
            | (b'>', b'>')
            | (b'=', b'=')
            | (b'~', b'=')
            | (b'<', b'=')
            | (b'>', b'=')
            | (b':', b':')
            // A separated '[' followed by '[' or '=' can become a long
            // bracket opener once later '='/'[' tokens are concatenated.
            | (b'[', b'[' | b'=')
    ) || (left == b'.' && right.is_ascii_digit())
        // A numeral ending in '.' followed by a name start is one glued
        // token stream for the lexer (T10: keyword glue is malformed).
        // Byte-level this also covers '.' + name member access, which
        // simply takes the validation path; over-validation is safe,
        // under-validation would emit unloadable text.
        || (left == b'.' && (right.is_ascii_alphabetic() || right == b'_'))
}

// Operator gaps use a zero-cost separator newline when separator newlines are
// uncharged, else a space. The replacement is 1 byte for 1 byte, so raw size,
// winner selection, and recorded byte ranges are unchanged; only the reported
// newline count grows. Newlines are pure whitespace between tokens in Lua
// (verified: `return 5` and `return\n5` compile identically), so the token
// stream -- and therefore the parse -- is identical.
impl<'a> Printer<'a> {
    /// Separator between a keyword and the following token: a counted
    /// zero-cost newline when separator newlines are uncharged, else a space.
    fn sep(&mut self) -> Fragment<'a> {
        if self.zero {
            self.newline_count += 1;
            Fragment::literal("\n")
        } else {
            Fragment::literal(" ")
        }
    }

    fn join_lex(&mut self, parts: impl IntoIterator<Item = Fragment<'a>>) -> Fragment<'a> {
        let mut out = Fragment::default();
        for part in parts {
            if part.text.is_empty() {
                continue;
            }
            if needs_sep(&out.text, &part.text) {
                if self.zero {
                    self.newline_count += 1;
                    out.text.to_mut().push('\n');
                } else {
                    out.text.to_mut().push(' ');
                }
            }
            out.append(part);
        }
        out
    }
}

/// Render a numeric literal without losing its integer/float interpretation.
pub fn render_num(raw: &str) -> Cow<'_, str> {
    // Canonical positive decimal integers up to 15 digits already have the
    // shortest decimal integer spelling. An exponent would change Lua's subtype.
    // Avoid f64 parsing/formatting for the overwhelmingly common draw coordinates.
    let digits = raw.as_bytes();
    if !digits.is_empty()
        && digits.len() <= 15
        && (digits.len() == 1 || digits[0] != b'0')
        && digits.iter().all(u8::is_ascii_digit)
    {
        return Cow::Borrowed(raw);
    }
    // The public historical canonicalizer operates on f64; Lua 5.3 printing
    // must additionally retain the integer/float subtype. This matters beyond
    // math.type: concatenation, overflow and integer division can observe it.
    if let Some(value) = integer_literal_value(raw) {
        if value.unsigned_abs() > (1u64 << 53) {
            return Cow::Borrowed(raw);
        }
        let normalized = normalize_num_literal(raw);
        if integer_literal_value(&normalized) == Some(value) {
            return Cow::Owned(normalized);
        }
        // An exponent spelling, e.g. 1e5, is floating point in Lua. Do not use
        // it to shorten an integer token such as 100000.
        let decimal = value.to_string();
        return if decimal.len() < raw.len() {
            Cow::Owned(decimal)
        } else {
            Cow::Borrowed(raw)
        };
    }
    if !crate::numeric::num_val(raw).is_finite() {
        return Cow::Borrowed(raw);
    }
    let mut normalized = normalize_num_literal(raw);
    if integer_literal_value(&normalized).is_some() {
        normalized.push_str(".0");
    }
    if normalized.len() > raw.len() {
        Cow::Borrowed(raw)
    } else {
        Cow::Owned(normalized)
    }
}

/// One emitted range in the final printed code, identified by its owning AST node.
/// Ranges may nest. An identifier slot is more specific than its containing node.
/// These are generated UTF-8 byte offsets, not original-source locations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeEmission {
    /// ID in the exact AST supplied to this printer.
    pub node: NodeId,
    /// Explicit identifier occurrence, or the entire node when absent.
    pub site: Option<crate::parser::NameSite>,
    /// Explicit syntactic token; absent for synthesized punctuation or a whole node.
    pub token: Option<crate::explanation::SyntaxSite>,
    /// Literal spelling transformation observed directly by the printer.
    pub reason: Option<crate::explanation::OptimizationReason>,
    /// Inclusive generated byte position.
    pub start: usize,
    /// Exclusive generated byte position.
    pub end: usize,
}

/// Printed Lua and its generated ranges. This is not yet an optimized source map:
/// the caller must supply source provenance for the same AST and node generation.
#[derive(Debug, Clone)]
pub struct PrintedSource {
    /// Selected root, including when optimization leaves no emitted statements.
    pub root: NodeId,
    /// Exact final code, including chosen separators and normalized literals.
    pub code: String,
    /// Node/name ranges recorded while constructing this code; no text matching.
    pub emissions: Vec<NodeEmission>,
}

// A recursive print result with relative ranges. The ordinary printer keeps the
// segment list empty (no mapping allocation). Ranges themselves live exactly once
// in the printer's arena; a fragment only holds the root of a segment rope, so
// composition links rope nodes instead of copying every segment, and no range
// is shifted until the final print flattens the rope.
//
// A rope node is one of:
// - a leaf: one recorded range, with its offset relative to its fragment;
// - a concat: the left and right subtrees of an append, where the right
//   subtree's ranges shift by the left text length at append time;
// - a shift: a whole subtree shifted by an append into a segment-free prefix.
//
// Nodes live in a thread-local arena that a tracked print clears before
// building and after flattening, so no node outlives its print and capacity
// is reused across prints on the same thread. Plain prints never create a
// node (their fragments have no root), so the untracked path stays
// allocation-free for segments. Fragments never escape a print, and a print
// never nests another print, so clearing at print boundaries is sufficient
// even if an earlier print was abandoned.
enum RopeNode {
    Leaf {
        index: usize,
        offset: usize,
    },
    Concat {
        left: usize,
        right: usize,
        right_offset: usize,
    },
    Shift {
        child: usize,
        offset: usize,
    },
}

thread_local! {
    static ROPE_NODES: std::cell::RefCell<Vec<RopeNode>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn rope_push(node: RopeNode) -> usize {
    ROPE_NODES.with(|nodes| {
        let mut nodes = nodes.borrow_mut();
        let index = nodes.len();
        nodes.push(node);
        index
    })
}

fn rope_clear() {
    ROPE_NODES.with(|nodes| nodes.borrow_mut().clear());
}

#[derive(Default)]
struct Fragment<'a> {
    text: Cow<'a, str>,
    root: Option<usize>,
}
impl<'a> From<String> for Fragment<'a> {
    fn from(text: String) -> Self {
        Self {
            text: Cow::Owned(text),
            root: None,
        }
    }
}
impl<'a> From<&'a str> for Fragment<'a> {
    fn from(text: &'a str) -> Self {
        Self {
            text: Cow::Borrowed(text),
            root: None,
        }
    }
}
impl<'a> From<Cow<'a, str>> for Fragment<'a> {
    fn from(text: Cow<'a, str>) -> Self {
        Self { text, root: None }
    }
}
impl<'a> Fragment<'a> {
    fn literal(text: &'static str) -> Self {
        Self {
            text: Cow::Borrowed(text),
            root: None,
        }
    }
    /// Record one range covering this whole fragment. A recorded range always
    /// starts at its fragment's start, so its leaf offset is zero even when
    /// the fragment already holds ranges (a whole-fragment record overlays
    /// its parts); the concat below keeps the earlier ranges first.
    fn push_segment(&mut self, index: usize) {
        let leaf = rope_push(RopeNode::Leaf { index, offset: 0 });
        match self.root {
            None => self.root = Some(leaf),
            Some(left) => {
                self.root = Some(rope_push(RopeNode::Concat {
                    left,
                    right: leaf,
                    right_offset: 0,
                }));
            }
        }
    }
    fn append(&mut self, other: Self) {
        if other.text.is_empty() {
            return;
        }
        if self.text.is_empty() && self.root.is_none() && matches!(&self.text, Cow::Borrowed(_)) {
            *self = other;
            return;
        }
        let offset = self.text.len();
        self.text.to_mut().push_str(&other.text);
        match (self.root, other.root) {
            (None, None) => {}
            (None, Some(right)) => {
                // Our own text has no ranges, so the other side's ranges all
                // shift by the prefix we already held.
                self.root = if offset == 0 {
                    Some(right)
                } else {
                    Some(rope_push(RopeNode::Shift {
                        child: right,
                        offset,
                    }))
                };
            }
            (Some(_), None) => {}
            (Some(left), Some(right)) => {
                self.root = Some(rope_push(RopeNode::Concat {
                    left,
                    right,
                    right_offset: offset,
                }));
            }
        }
    }
    fn join(parts: impl IntoIterator<Item = Self>, separator: &str) -> Self {
        let mut output = Self::default();
        for (i, part) in parts.into_iter().enumerate() {
            if i > 0 && !separator.is_empty() {
                output.text.to_mut().push_str(separator);
            }
            output.append(part);
        }
        output
    }
}
macro_rules! fragments {
    ($($piece:expr),+ $(,)?) => {
        Fragment::join([$(Fragment::from($piece)),+], "")
    };
}

/// Emitter for an arena whose node IDs and child kinds satisfy the syntax contract.
pub struct Printer<'a> {
    ast: &'a Ast,
    zero: bool,
    newline_count: u32,
    capture_emissions: bool,
    arena: std::cell::RefCell<Vec<NodeEmission>>,
    /// Optional spelling substitution for identifier occurrences, keyed by
    /// (node, site) exactly like the size walker's `SizeNameResolver`.
    /// When set, `name()` emits the replacement spelling instead of the
    /// AST's stored spelling, and local-merge decisions in `block()`
    /// compare post-substitution spellings. This is the mapping-aware
    /// print used to measure a scope-renamed size without materializing
    /// the rename: the substitution is a scope-rename plan computed on
    /// this same tree. `None` preserves the stock behavior exactly.
    name_resolver: Option<&'a dyn crate::size::SizeNameResolver>,
}

impl<'a> Printer<'a> {
    /// Create a printer for a structurally valid AST; choose whether separator newlines are uncharged.
    pub fn new(ast: &'a Ast, zero_cost_newlines: bool) -> Self {
        Printer {
            ast,
            zero: zero_cost_newlines,
            newline_count: 0,
            capture_emissions: false,
            arena: std::cell::RefCell::new(Vec::new()),
            name_resolver: None,
        }
    }
    /// Create a printer that emits substituted spellings for identifier
    /// occurrences (see `name_resolver`). The resolver must outlive the print.
    pub fn with_name_resolver(
        ast: &'a Ast,
        zero_cost_newlines: bool,
        resolver: &'a dyn crate::size::SizeNameResolver,
    ) -> Self {
        Printer {
            ast,
            zero: zero_cost_newlines,
            newline_count: 0,
            capture_emissions: false,
            arena: std::cell::RefCell::new(Vec::new()),
            name_resolver: Some(resolver),
        }
    }
    /// Return the number of separator newlines emitted so far.
    pub fn newline_count(&self) -> u32 {
        self.newline_count
    }

    fn record(
        &self,
        mut fragment: Fragment<'a>,
        node: NodeId,
        site: Option<crate::parser::NameSite>,
    ) -> Fragment<'a> {
        if self.capture_emissions && !fragment.text.is_empty() {
            let emission = NodeEmission {
                node,
                site,
                token: None,
                reason: if site.is_none() {
                    self.literal_reason(node, &fragment.text)
                } else {
                    None
                },
                start: 0,
                end: fragment.text.len(),
            };
            // The range is recorded once in the printer's arena; the fragment
            // only keeps a rope leaf for the arena index. Composition links
            // rope nodes, and the final print resolves absolute ranges from
            // arena entries. record runs while no arena borrow is held, so
            // this borrow cannot overlap another one.
            let mut arena = self.arena.borrow_mut();
            let index = arena.len();
            arena.push(emission);
            fragment.push_segment(index);
        }
        fragment
    }
    fn literal_reason(
        &self,
        node: NodeId,
        text: &str,
    ) -> Option<crate::explanation::OptimizationReason> {
        use crate::explanation::{OptimizationReason, ReasonOperation};
        // Literal spellings repeat the same rule codes, basis, and empty
        // facts at every literal, so share one Arc per constant instead of
        // allocating a fresh Arc for each reason (the values are identical).
        // The string branch additionally formats its length tags into a
        // stack buffer, so each tag costs one Arc allocation, not a String
        // plus an Arc.
        fn shared(
            cell: &std::sync::OnceLock<std::sync::Arc<str>>,
            value: &str,
        ) -> std::sync::Arc<str> {
            std::sync::Arc::clone(cell.get_or_init(|| std::sync::Arc::from(value)))
        }
        fn bytes_tag(len: usize) -> std::sync::Arc<str> {
            const PREFIX: &[u8] = b"literal-bytes:";
            let mut buf = [0u8; PREFIX.len() + 20];
            buf[..PREFIX.len()].copy_from_slice(PREFIX);
            let mut digits = [0u8; 20];
            let mut n = len;
            let mut count = 0;
            loop {
                digits[count] = b'0' + (n % 10) as u8;
                count += 1;
                n /= 10;
                if n == 0 {
                    break;
                }
            }
            let start = PREFIX.len();
            for i in 0..count {
                buf[start + i] = digits[count - 1 - i];
            }
            // The buffer holds only ASCII (a fixed prefix plus decimal digits),
            // so this is always valid UTF-8 and borrows without allocating.
            let text = std::string::String::from_utf8_lossy(&buf[..start + count]);
            std::sync::Arc::from(text.as_ref())
        }
        static NUMERIC_CODE: std::sync::OnceLock<std::sync::Arc<str>> = std::sync::OnceLock::new();
        static STRING_CODE: std::sync::OnceLock<std::sync::Arc<str>> = std::sync::OnceLock::new();
        static BOOLEAN_CODE: std::sync::OnceLock<std::sync::Arc<str>> = std::sync::OnceLock::new();
        static BASIS: std::sync::OnceLock<std::sync::Arc<str>> = std::sync::OnceLock::new();
        static EMPTY_FACTS: std::sync::OnceLock<
            std::sync::Arc<Vec<crate::explanation::ReasonFact>>,
        > = std::sync::OnceLock::new();
        let (code, before, after) = match self.ast.node(node) {
            Node::Num(value) if value.as_ref() != text => (
                shared(&NUMERIC_CODE, "numeric-literal-spelling"),
                std::sync::Arc::from(format!("number:{value}")),
                std::sync::Arc::from(format!("number:{text}")),
            ),
            Node::Str(value) if value.as_ref() != text => (
                shared(&STRING_CODE, "string-literal-spelling"),
                bytes_tag(value.len()),
                bytes_tag(text.len()),
            ),
            Node::Bool(value) if text != if *value { "true" } else { "false" } => (
                shared(&BOOLEAN_CODE, "boolean-literal-shortening"),
                std::sync::Arc::from(format!("boolean:{value}")),
                std::sync::Arc::from(format!("expression:{text}")),
            ),
            _ => return None,
        };
        Some(OptimizationReason {
            code,
            operation: ReasonOperation::Rewrite,
            before: Some(before),
            after: Some(after),
            basis: Some(shared(&BASIS, "canonical-literal-emission")),
            facts: std::sync::Arc::clone(
                EMPTY_FACTS.get_or_init(|| std::sync::Arc::new(Vec::new())),
            ),
        })
    }
    fn syntax(
        &self,
        text: &'a str,
        node: NodeId,
        site: crate::explanation::SyntaxSite,
    ) -> Fragment<'a> {
        self.record_syntax(Fragment::from(text), node, site)
    }
    fn keyword(
        &self,
        text: &'static str,
        node: NodeId,
        site: crate::explanation::SyntaxSite,
    ) -> Fragment<'a> {
        self.record_syntax(Fragment::literal(text), node, site)
    }
    fn record_syntax(
        &self,
        mut fragment: Fragment<'a>,
        node: NodeId,
        site: crate::explanation::SyntaxSite,
    ) -> Fragment<'a> {
        if self.capture_emissions {
            if crate::explanation::syntax_text(self.ast.node(node), site)
                != Some(fragment.text.as_ref())
            {
                return self.record(fragment, node, None);
            }
            let mut arena = self.arena.borrow_mut();
            let index = arena.len();
            arena.push(NodeEmission {
                node,
                site: None,
                token: Some(site),
                reason: None,
                start: 0,
                end: fragment.text.len(),
            });
            fragment.push_segment(index);
        }
        fragment
    }
    fn name(
        &self,
        symbol: crate::ast::SymbolId,
        node: NodeId,
        site: crate::parser::NameSite,
    ) -> Fragment<'a> {
        if let Some(resolver) = self.name_resolver {
            if let Some(replacement) = resolver.replacement(node, site) {
                return self.record(replacement.into(), node, Some(site));
            }
        }
        self.record(self.ast.strings.get(symbol).into(), node, Some(site))
    }
    fn member(&self, text: impl Into<Cow<'a, str>>, node: NodeId) -> Fragment<'a> {
        self.record(
            Fragment::from(text.into()),
            node,
            Some(crate::parser::NameSite::Member),
        )
    }
    fn bracket_key_expr(&mut self, id: NodeId) -> Fragment<'a> {
        if let Node::Str(raw) = self.ast.node(id) {
            let key = qstr_bracket_key(raw);
            let space = key.starts_with('[');
            let key = self.record(key.into(), id, None);
            return if space {
                let sp = self.sep();
                fragments!(sp, key)
            } else {
                key
            };
        }
        self.expr(id, 0)
    }
    fn cat(&mut self, mut a: Fragment<'a>, b: Fragment<'a>) -> Fragment<'a> {
        if b.text.is_empty() {
            return a;
        }
        if needs_sep(&a.text, &b.text) {
            if self.zero {
                self.newline_count += 1;
                a.text.to_mut().push('\n');
            } else {
                a.text.to_mut().push(' ');
            }
        }
        a.append(b);
        a
    }
    fn expr(&mut self, id: NodeId, parent: u8) -> Fragment<'a> {
        let result = if parent == 15
            && !matches!(
                self.ast.node(id),
                Node::Name(_) | Node::Paren(_) | Node::Index(..) | Node::Call(..)
            ) {
            fragments!(
                Fragment::literal("("),
                self.expr_inner(id, 0),
                Fragment::literal(")")
            )
        } else {
            self.expr_inner(id, parent)
        };
        self.record(result, id, None)
    }
    #[expect(
        clippy::panic,
        reason = "Invalid low-level AST expression shapes fail loudly; source is parsed before printing"
    )]
    fn expr_inner(&mut self, id: NodeId, parent: u8) -> Fragment<'a> {
        use crate::parser::NameSite;
        let node = &self.ast.nodes[id as usize];
        let s: Fragment<'a>;
        let prec: u8;
        match node {
            Node::Name(sym) => return self.name(*sym, id, NameSite::Reference),
            Node::Num(v) => {
                let text = render_num(v);
                if (text.contains('/') && parent > 11) || (text.starts_with('-') && parent >= 14) {
                    return fragments!(
                        Fragment::literal("("),
                        Fragment::from(text),
                        Fragment::literal(")")
                    );
                }
                return text.into();
            }
            Node::Str(v) => return crate::numeric::render_quoted(v).into(),
            Node::Nil => return Fragment::literal("nil"),
            Node::Bool(b) => {
                if parent <= 3 {
                    return if *b { "1>0" } else { "1>2" }.into();
                }
                return if *b { "true" } else { "false" }.into();
            }
            Node::Vararg => return Fragment::literal("..."),
            Node::Paren(e) => {
                return fragments!(
                    Fragment::literal("("),
                    self.expr(*e, 0),
                    Fragment::literal(")")
                )
            }
            Node::Table(fs) => {
                let parts = fs.iter().enumerate().map(|(index, field)| match field {
                    TableField::Arr(v) => self.expr(*v, 0),
                    TableField::Name(k, v) => fragments!(
                        self.name(*k, id, NameSite::Field(index as u32)),
                        Fragment::literal("="),
                        self.expr(*v, 0)
                    ),
                    TableField::KVar(k, v) => {
                        if let Node::Str(raw) = &self.ast.nodes[*k as usize] {
                            if let Some(key) = plain_name_from_string_literal(raw) {
                                let key = self.record(Fragment::from(key), *k, None);
                                return fragments!(key, Fragment::literal("="), self.expr(*v, 0));
                            }
                        }
                        fragments!(
                            Fragment::literal("["),
                            self.bracket_key_expr(*k),
                            Fragment::literal("]="),
                            self.expr(*v, 0)
                        )
                    }
                });
                let table = fragments!(
                    Fragment::literal("{"),
                    Fragment::join(parts, ","),
                    Fragment::literal("}")
                );
                return if parent == 15 {
                    fragments!(Fragment::literal("("), table, Fragment::literal(")"))
                } else {
                    table
                };
            }
            Node::Index(obj, key, _) => {
                let obj_s = self.expr(*obj, 15);
                if let Node::Str(kv) = &self.ast.nodes[*key as usize] {
                    if let Some(k) = plain_name_from_string_literal(kv) {
                        let member = self.record(self.member(k, *key), *key, None);
                        return fragments!(obj_s, Fragment::literal("."), member);
                    }
                }
                return fragments!(
                    obj_s,
                    Fragment::literal("["),
                    self.bracket_key_expr(*key),
                    Fragment::literal("]")
                );
            }
            Node::Function(..) => {
                let function = self.function(id, true);
                return if parent == 15 {
                    fragments!(Fragment::literal("("), function, Fragment::literal(")"))
                } else {
                    function
                };
            }
            Node::Un(op, e) => {
                prec = 12;
                let op_s = self.syntax(op, id, crate::explanation::SyntaxSite::Operator);
                let e_s = self.expr(*e, 12);
                s = self.join_lex([op_s, e_s]);
            }
            Node::Bin(op, l, r) => {
                let (lp, rp) = bin_prec(op);
                prec = lp;
                let mut l_s = self.expr(*l, lp);
                // Right-associative operators (`..`, `^`): a bare left child
                // that is itself an application of the same operator would
                // be re-parsed right-nested from the flat text, changing
                // evaluation order and values (e.g. `(a .. b) .. c` printed
                // as `a..b..c` reads back as `a .. (b .. c)`). Parsed source
                // always carries a Paren node in this position, but passes
                // that strip or rebuild parentheses can leave the bare
                // shape behind, so the printer parenthesizes it itself
                // (candidate T, w9 cycle 67).
                if matches!(op.as_str(), ".." | "^") {
                    if let Node::Bin(child_op, ..) = self.ast.node(*l) {
                        if child_op == op {
                            l_s = fragments!(Fragment::literal("("), l_s, Fragment::literal(")"));
                        }
                    }
                }
                let op_s = self.syntax(op, id, crate::explanation::SyntaxSite::Operator);
                let r_s = self.expr(*r, rp);
                s = self.join_lex([l_s, op_s, r_s]);
            }
            Node::Call(fn_, args, method) => {
                let fn_s = self.expr(*fn_, 15);
                let args_s = Fragment::join(args.iter().map(|a| self.expr(*a, 0)), ",");
                let sugar = args.len() == 1
                    && matches!(self.ast.node(args[0]), Node::Str(_) | Node::Table(_));
                return match (method, sugar) {
                    (Some(m), true) => {
                        fragments!(
                            fn_s,
                            Fragment::literal(":"),
                            self.member(self.ast.strings.get(*m), id),
                            args_s
                        )
                    }
                    (None, true) => fragments!(fn_s, args_s),
                    (Some(m), false) => fragments!(
                        fn_s,
                        Fragment::literal(":"),
                        self.member(self.ast.strings.get(*m), id),
                        Fragment::literal("("),
                        args_s,
                        Fragment::literal(")")
                    ),
                    (None, false) => {
                        fragments!(fn_s, Fragment::literal("("), args_s, Fragment::literal(")"))
                    }
                };
            }
            _ => panic!("unknown expression node"),
        }
        if prec < parent {
            fragments!(Fragment::literal("("), s, Fragment::literal(")"))
        } else {
            s
        }
    }
    fn function(&mut self, id: NodeId, keyword: bool) -> Fragment<'a> {
        use crate::parser::NameSite;
        if let Node::Function(ps, variadic, b) = &self.ast.nodes[id as usize] {
            let params = ps
                .iter()
                .enumerate()
                .map(|(i, p)| self.name(*p, id, NameSite::Parameter(i as u32)));
            let header = fragments!(
                if keyword {
                    fragments!(
                        self.keyword("function", id, crate::explanation::SyntaxSite::Begin),
                        Fragment::literal("(")
                    )
                } else {
                    Fragment::literal("(")
                },
                Fragment::join(params, ","),
                var_suffix(ps.len(), *variadic),
                Fragment::literal(")")
            );
            let body = self.block(*b);
            let end = self.cat(
                body,
                self.keyword("end", id, crate::explanation::SyntaxSite::End),
            );
            return self.cat(header, end);
        }
        unreachable!()
    }
    fn function_tail(&mut self, id: NodeId) -> Fragment<'a> {
        let result = self.function(id, false);
        self.record(result, id, None)
    }
    /// Whether an Assign target may be printed as a function-statement
    /// target: a bare Name, or a pure dot chain rooted at a Name
    /// (`a.b.c`). Bracket targets, calls, and parenthesized targets are
    /// not valid function-statement names and stay as assignments.
    fn is_funcstat_target(&self, id: NodeId) -> bool {
        match &self.ast.nodes[id as usize] {
            Node::Name(_) => true,
            Node::Index(obj, key, dot) => {
                if !dot {
                    return false;
                }
                let key_ok = match &self.ast.nodes[*key as usize] {
                    Node::Str(kv) => plain_name_from_string_literal(kv).is_some(),
                    _ => false,
                };
                key_ok && self.is_funcstat_target(*obj)
            }
            _ => false,
        }
    }
    fn target(&mut self, id: NodeId) -> Fragment<'a> {
        if let Node::Methodname(obj, name) = &self.ast.nodes[id as usize] {
            let result = fragments!(
                self.expr(*obj, 0),
                Fragment::literal(":"),
                self.name(*name, id, crate::parser::NameSite::Member)
            );
            return self.record(result, id, None);
        }
        self.expr(id, 0)
    }
    fn stat(&mut self, id: NodeId) -> Fragment<'a> {
        let result = self.stat_inner(id);
        self.record(result, id, None)
    }
    #[expect(
        clippy::panic,
        reason = "A statement block only contains statement nodes; invalid low-level ASTs fail loudly"
    )]
    fn stat_inner(&mut self, id: NodeId) -> Fragment<'a> {
        use crate::parser::NameSite;
        match &self.ast.nodes[id as usize] {
            Node::Break => self.keyword("break", id, crate::explanation::SyntaxSite::Begin),
            Node::Goto(name) => {
                let sp = self.sep();
                fragments!(
                    Fragment::literal("goto"),
                    sp,
                    self.name(*name, id, NameSite::Label)
                )
            }
            Node::Label(name) => fragments!(
                Fragment::literal("::"),
                self.name(*name, id, NameSite::Label),
                Fragment::literal("::")
            ),
            Node::Do(b) => {
                let body = self.block(*b);
                let a = self.cat(
                    self.keyword("do", id, crate::explanation::SyntaxSite::Begin),
                    body,
                );
                self.cat(
                    a,
                    self.keyword("end", id, crate::explanation::SyntaxSite::End),
                )
            }
            Node::While(e, b) => {
                let cond = self.expr(*e, 0);
                let body = self.block(*b);
                let head = self.cat(
                    self.keyword("while", id, crate::explanation::SyntaxSite::Begin),
                    cond,
                );
                let head = self.cat(
                    head,
                    self.keyword("do", id, crate::explanation::SyntaxSite::Body),
                );
                let a = self.cat(head, body);
                self.cat(
                    a,
                    self.keyword("end", id, crate::explanation::SyntaxSite::End),
                )
            }
            Node::Repeat(b, e) => {
                let body = self.block(*b);
                let until = self.expr(*e, 0);
                let a = self.cat(
                    self.keyword("repeat", id, crate::explanation::SyntaxSite::Begin),
                    body,
                );
                let tail = self.cat(
                    self.keyword("until", id, crate::explanation::SyntaxSite::Body),
                    until,
                );
                self.cat(a, tail)
            }
            Node::If(arms, eb) => {
                let e0 = self.expr(arms[0].cond, 0);
                let b0 = self.block(arms[0].body);
                let head = self.cat(
                    self.keyword("if", id, crate::explanation::SyntaxSite::Begin),
                    e0,
                );
                let head = self.cat(
                    head,
                    self.keyword("then", id, crate::explanation::SyntaxSite::Body),
                );
                let mut out = self.cat(head, b0);
                let mut rest = Vec::new();
                for arm in &arms[1..] {
                    rest.push((self.expr(arm.cond, 0), self.block(arm.body)));
                }
                for (e, b) in rest {
                    out = self.cat(out, Fragment::literal("elseif"));
                    out = self.cat(out, e);
                    out = self.cat(out, Fragment::literal("then"));
                    out = self.cat(out, b);
                }
                if let Some(else_b) = eb {
                    // An empty else branch is a semantic no-op: dropping it
                    // yields exactly the no-else output. Check the AST block
                    // before printing so no emissions are recorded for it.
                    let is_empty = matches!(
                        self.ast.node(*else_b),
                        Node::Block(statements) if statements.is_empty()
                    );
                    if !is_empty {
                        let body = self.block(*else_b);
                        out = self.cat(out, Fragment::literal("else"));
                        out = self.cat(out, body);
                    }
                }
                self.cat(
                    out,
                    self.keyword("end", id, crate::explanation::SyntaxSite::End),
                )
            }
            Node::Fornum(name, a, b, c, body) => {
                let sp = self.sep();
                let mut head = fragments!(
                    self.keyword("for", id, crate::explanation::SyntaxSite::Begin),
                    sp,
                    self.name(*name, id, NameSite::Binding(0)),
                    Fragment::literal("="),
                    self.expr(*a, 0),
                    Fragment::literal(","),
                    self.expr(*b, 0)
                );
                // An integer-literal step of 1 is exactly the default step,
                // so omitting it changes nothing (loop kind included). A
                // float step (1.0, 1e0, ...) must stay: dropping it would
                // turn a float loop into an integer loop.
                let skip_step = match c {
                    Some(step) => is_int_one_step(self.ast, *step),
                    None => false,
                };
                if let Some(step) = c {
                    if !skip_step {
                        head.append(fragments!(Fragment::literal(","), self.expr(*step, 0)));
                    }
                }
                let head = self.cat(
                    head,
                    self.keyword("do", id, crate::explanation::SyntaxSite::Body),
                );
                let body = self.block(*body);
                let out = self.cat(head, body);
                self.cat(
                    out,
                    self.keyword("end", id, crate::explanation::SyntaxSite::End),
                )
            }
            Node::Forin(names, es, body) => {
                let names = Fragment::join(
                    names
                        .iter()
                        .enumerate()
                        .map(|(i, n)| self.name(*n, id, NameSite::Binding(i as u32))),
                    ",",
                );
                let values = Fragment::join(es.iter().map(|e| self.expr(*e, 0)), ",");
                let sp1 = self.sep();
                let sp2 = self.sep();
                let head = fragments!(
                    self.keyword("for", id, crate::explanation::SyntaxSite::Begin),
                    sp1,
                    names,
                    sp2,
                    Fragment::literal("in")
                );
                let head = self.cat(head, values);
                let head = self.cat(
                    head,
                    self.keyword("do", id, crate::explanation::SyntaxSite::Body),
                );
                let body = self.block(*body);
                let out = self.cat(head, body);
                self.cat(
                    out,
                    self.keyword("end", id, crate::explanation::SyntaxSite::End),
                )
            }
            Node::Funcstat(target, function) => {
                let sp = self.sep();
                fragments!(
                    Fragment::literal("function"),
                    sp,
                    self.target(*target),
                    self.function_tail(*function)
                )
            }
            Node::Localfunc(name, function) => {
                let sp1 = self.sep();
                let sp2 = self.sep();
                fragments!(
                    Fragment::literal("local"),
                    sp1,
                    Fragment::literal("function"),
                    sp2,
                    self.name(*name, id, NameSite::Binding(0)),
                    self.function_tail(*function)
                )
            }
            Node::Local(names, es) => {
                let ns = Fragment::join(
                    names
                        .iter()
                        .enumerate()
                        .map(|(i, n)| self.name(*n, id, NameSite::Binding(i as u32))),
                    ",",
                );
                let keep = trimmed_nil_rhs_len(self.ast, names.len(), es, true);
                if keep == 0 {
                    let sp = self.sep();
                    fragments!(Fragment::literal("local"), sp, ns)
                } else {
                    let sp = self.sep();
                    let values = Fragment::join(es[..keep].iter().map(|e| self.expr(*e, 0)), ",");
                    fragments!(
                        Fragment::literal("local"),
                        sp,
                        ns,
                        Fragment::literal("="),
                        values
                    )
                }
            }
            Node::Return(es) => {
                if es.is_empty() {
                    self.keyword("return", id, crate::explanation::SyntaxSite::Begin)
                } else {
                    let values = Fragment::join(es.iter().map(|e| self.expr(*e, 0)), ",");
                    self.cat(
                        self.keyword("return", id, crate::explanation::SyntaxSite::Begin),
                        values,
                    )
                }
            }
            Node::Callstat(e) => self.expr(*e, 0),
            Node::Assign(vs, es) => {
                let keep = trimmed_nil_rhs_len(self.ast, vs.len(), es, false);
                if vs.len() == 1
                    && keep == 1
                    && matches!(&self.ast.nodes[es[0] as usize], Node::Function(..))
                    && self.is_funcstat_target(vs[0])
                {
                    // `x = function...end` prints as `function x...end`:
                    // the "=" is replaced by the separator after
                    // "function", so the raw size is unchanged while the
                    // effective size (zero-cost separators excluded) drops
                    // by one. A function statement assigns its target
                    // exactly like an assignment (resolver treats both
                    // targets as lvalues), so this is a pure print form.
                    // The keyword keeps its recorded Begin segment (as in
                    // the assignment form) so the Source Map neither loses
                    // nor invents a correspondence for it.
                    let sp = self.sep();
                    fragments!(
                        self.keyword("function", es[0], crate::explanation::SyntaxSite::Begin),
                        sp,
                        self.target(vs[0]),
                        self.function_tail(es[0])
                    )
                } else {
                    let targets = Fragment::join(vs.iter().map(|e| self.expr(*e, 0)), ",");
                    let values = Fragment::join(es[..keep].iter().map(|e| self.expr(*e, 0)), ",");
                    fragments!(targets, Fragment::literal("="), values)
                }
            }
            _ => panic!("unknown statement node"),
        }
    }
    /// Print a run of merged `local` statements as one statement. Each
    /// name keeps its own statement node and binding index, so its segment
    /// attribution is unchanged; only the repeated `local` keywords and
    /// the between-statement separators disappear.
    fn merged_local(&mut self, members: &[LocalMember]) -> Fragment<'a> {
        let mut name_parts = Vec::new();
        for m in members {
            for (i, n) in m.names.iter().enumerate() {
                name_parts.push(self.name(*n, m.stmt, crate::parser::NameSite::Binding(i as u32)));
            }
        }
        let ns = Fragment::join(name_parts, ",");
        let mut value_parts = Vec::new();
        for e in members.iter().flat_map(|m| m.kept.iter()) {
            value_parts.push(self.expr(*e, 0));
        }
        if value_parts.is_empty() {
            let sp = self.sep();
            fragments!(Fragment::literal("local"), sp, ns)
        } else {
            let sp = self.sep();
            let values = Fragment::join(value_parts, ",");
            fragments!(
                Fragment::literal("local"),
                sp,
                ns,
                Fragment::literal("="),
                values
            )
        }
    }
    fn block(&mut self, id: NodeId) -> Fragment<'a> {
        if let Node::Block(statements) = &self.ast.nodes[id as usize] {
            // Dead-`if` removal and `local` merging are shared with
            // size measurement via `block_items`.
            let items = block_items(self.ast, statements, self.name_resolver);
            let parts = items
                .into_iter()
                .map(|item| match item {
                    BlockItem::Single(stmt) => self.stat(stmt),
                    BlockItem::Merged(members) => {
                        if members.len() == 1 {
                            self.stat(members[0].stmt)
                        } else {
                            self.merged_local(&members)
                        }
                    }
                })
                .collect::<Vec<_>>();
            let mut out = Fragment {
                text: Cow::Owned(String::with_capacity(
                    parts.iter().map(|p| p.text.len()).sum::<usize>() + parts.len(),
                )),
                root: None,
            };
            for (i, part) in parts.into_iter().enumerate() {
                if i > 0 {
                    if part.text.starts_with('(') {
                        out.text.to_mut().push(';');
                    } else if needs_sep(&out.text, &part.text) {
                        if self.zero {
                            self.newline_count += 1;
                            out.text.to_mut().push('\n');
                        } else {
                            out.text.to_mut().push(' ');
                        }
                    }
                }
                out.append(part);
            }
            return self.record(out, id, None);
        }
        unreachable!()
    }
    /// Emit the root block without collecting positions.
    pub fn output(&mut self, root: NodeId) -> String {
        self.block(root).text.into_owned()
    }
    /// Emit the same code while recording exact generated node and identifier ranges.
    /// Original provenance must belong to this exact AST; parsed NodePositions alone
    /// cannot describe a transformed arena without explicit origin propagation.
    pub fn output_with_positions(&mut self, root: NodeId) -> PrintedSource {
        // Clear any nodes left by an abandoned print, then build this print's
        // rope from scratch; flattening clears it again below.
        rope_clear();
        self.capture_emissions = true;
        let result = self.block(root);
        self.capture_emissions = false;
        // Resolve absolute ranges once. The walk is iterative: a concat tree can
        // be as deep as the append chain that built it.
        // Move arena entries out by leaf index instead of cloning them: each
        // arena entry is referenced by exactly one rope leaf, so replacing a
        // visited slot with a dummy yields the same emissions in the same
        // order without building an intermediate Vec<Option<_>> first.
        let mut arena_entries = {
            let mut arena = self.arena.borrow_mut();
            std::mem::take(&mut *arena)
        };
        #[cfg(debug_assertions)]
        let mut seen = vec![false; arena_entries.len()];
        let mut emissions = Vec::with_capacity(arena_entries.len());
        if let Some(root_node) = result.root {
            let mut stack = vec![(root_node, 0usize)];
            ROPE_NODES.with(|nodes| {
                let nodes = nodes.borrow();
                while let Some((index, base)) = stack.pop() {
                    match &nodes[index] {
                        RopeNode::Leaf { index, offset } => {
                            #[cfg(debug_assertions)]
                            {
                                debug_assert!(
                                    !seen[*index],
                                    "each arena entry is referenced by exactly one rope leaf"
                                );
                                seen[*index] = true;
                            }
                            let mut emission = std::mem::replace(
                                &mut arena_entries[*index],
                                NodeEmission {
                                    node: 0,
                                    site: None,
                                    token: None,
                                    reason: None,
                                    start: 0,
                                    end: 0,
                                },
                            );
                            let shift = base + *offset;
                            emission.start += shift;
                            emission.end += shift;
                            emissions.push(emission);
                        }
                        RopeNode::Concat {
                            left,
                            right,
                            right_offset,
                        } => {
                            stack.push((*right, base + *right_offset));
                            stack.push((*left, base));
                        }
                        RopeNode::Shift { child, offset } => {
                            stack.push((*child, base + *offset));
                        }
                    }
                }
            });
        }
        rope_clear();
        PrintedSource {
            root,
            code: result.text.into_owned(),
            emissions,
        }
    }
    /// Print one statement, without recording positions.
    pub fn stat_public(&mut self, id: NodeId) -> String {
        self.stat(id).text.into_owned()
    }
    /// Print one expression, without recording positions.
    pub fn expr_public(&mut self, id: NodeId) -> String {
        self.expr(id, 0).text.into_owned()
    }
}

/// (ローカル変数接尾辞: vararg が真のとき ",..." か "..." を追記)
fn var_suffix(params: usize, variadic: bool) -> &'static str {
    if !variadic {
        ""
    } else if params > 0 {
        ",..."
    } else {
        "..."
    }
}

fn is_plain_name(s: &str) -> bool {
    let Some((&first, rest)) = s.as_bytes().split_first() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && rest.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_')
        && !is_keyword(s)
}

pub(crate) fn plain_name_from_string_literal(raw: &str) -> Option<Cow<'_, str>> {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[0], b'\'' | b'"') && bytes[bytes.len() - 1] == bytes[0] {
        let inner = &raw[1..raw.len() - 1];
        if !inner.as_bytes().contains(&b'\\') {
            return is_plain_name(inner).then_some(Cow::Borrowed(inner));
        }
    }
    let decoded = decode_lua_string(raw)?;
    is_plain_name(&decoded).then_some(Cow::Owned(decoded))
}

/// tokenMinify 相当（Lexer 列 → 最短トークン文字列）。
/// TS の tokenMinify は lexer エラー時に throw する。Rust は握りつぶさず
/// `Err(LexError)` で伝播させる（AGENTS §7: エラーの握りつぶし禁止）。
pub fn token_minify(source: &str) -> Result<String, crate::lexer::LexError> {
    let mut out = String::with_capacity(source.len());
    emit_compact_tokens(source, |piece| out.push_str(piece))?;
    Ok(out)
}

/// Count the public UTF-16 token-minified baseline without materializing its
/// source string. Rendering and separator decisions are shared with token_minify.
pub fn token_minified_utf16_len(source: &str) -> Result<usize, crate::lexer::LexError> {
    let mut size = 0;
    emit_compact_tokens(source, |piece| {
        size += crate::source_position::utf16_len(piece)
    })?;
    Ok(size)
}

fn emit_compact_tokens(
    source: &str,
    mut emit: impl FnMut(&str),
) -> Result<(), crate::lexer::LexError> {
    use crate::lexer::{Lexer, TokenKind};
    let mut lexer = Lexer::new(source);
    let mut last = None;
    loop {
        let token = lexer.next_borrowed()?;
        if token.k == TokenKind::Eof {
            break;
        }
        let value = match token.k {
            TokenKind::Str => crate::numeric::render_quoted(token.v),
            TokenKind::Num => render_num(token.v),
            _ => Cow::Borrowed(token.v),
        };
        if let (Some(left), Some(right)) = (last, value.chars().next()) {
            if needs_sep_chars(left, right) {
                emit(" ");
            }
        }
        emit(&value);
        last = value.chars().next_back().or(last);
    }
    Ok(())
}

/// Remove comments and redundant separators without rewriting names, literals, or line endings.
/// Preserving token line positions also preserves Lua error messages observed through pcall.
/// Reflection and host overrides use this path instead of whole-program transformations.
pub fn lexical_minify(source: &str) -> Result<String, crate::lexer::LexError> {
    lexical_minify_impl(source, false).map(|(code, _)| code)
}

#[doc(hidden)]
pub fn lexical_minify_with_compact_origins(
    name: &str,
    source: &str,
) -> Result<(String, crate::provenance::LexicalCopiedOrigins), crate::lexer::LexError> {
    let (code, tokens) = lexical_minify_impl(source, true)?;
    let generated_len = code.len();
    Ok((
        code,
        crate::provenance::LexicalCopiedOrigins::new(name, source, generated_len, tokens),
    ))
}

/// Preserve lexical source locations using the same token-emission path as ordinary
/// conservative minification. This does not guess origins from a generated string.
pub fn lexical_minify_with_origins(
    name: &str,
    source: &str,
) -> Result<(String, crate::provenance::GeneratedOrigins), crate::lexer::LexError> {
    let (code, compact) = lexical_minify_with_compact_origins(name, source)?;
    Ok((code, compact.into_generated()))
}

/// Look up a rename replacement for one lexer token by its exact source
/// span. Spans are sorted and non-overlapping (guaranteed by the caller);
/// a span mismatch is treated as "no rename" for that token.
fn lexical_rename_for(
    renames: &[(usize, usize, String)],
    start: usize,
    end: usize,
) -> Option<&str> {
    let idx = renames.partition_point(|(s, _, _)| *s < start);
    renames
        .get(idx)
        .filter(|(s, e, _)| *s == start && *e == end)
        .map(|(_, _, name)| name.as_str())
}

/// Re-lex `code` and require the exact token kind / line sequence of the
/// source tokens, with renamed identifier spellings substituted. This is
/// the rename step's safety net: identifier character class is unchanged
/// by renaming, but any boundary surprise falls back to the unrenamed
/// output instead of producing a retokenized program.
fn renamed_tokens_match<'a>(
    renames: &[(usize, usize, String)],
    tokens: &[crate::lexer::BorrowedToken<'a>],
    code: &str,
) -> bool {
    use crate::lexer::{Lexer, TokenKind};
    let mut actual = Lexer::new(code);
    for expected in tokens.iter().filter(|t| t.k != TokenKind::Eof) {
        let expected_spelling =
            lexical_rename_for(renames, expected.p, expected.p + expected.v.len())
                .unwrap_or(expected.v);
        match actual.next_borrowed() {
            Ok(token)
                if token.k == expected.k
                    && token.v == expected_spelling
                    && token.line == expected.line => {}
            _ => return false,
        }
    }
    matches!(actual.next_borrowed(), Ok(token) if token.k == TokenKind::Eof)
}

/// Lexical minification with local-only identifier renaming applied to
/// the emitted tokens (see `lexical_rename` in storm-lua-minify). Global
/// names, field/method names, labels, string literals and comments are
/// untouched: only tokens whose source span is in `renames` change, and
/// every such token is an identifier occurrence of a local binding.
/// On any anomaly (retokenization, or renaming not paying off) the
/// result is the unrenamed lexical output, never a broken program.
pub fn lexical_minify_renamed(
    source: &str,
    renames: &[(usize, usize, String)],
) -> Result<String, crate::lexer::LexError> {
    lexical_minify_renamed_impl(source, false, renames).map(|(code, _)| code)
}

#[doc(hidden)]
pub fn lexical_minify_renamed_with_compact_origins(
    name: &str,
    source: &str,
    renames: &[(usize, usize, String)],
) -> Result<(String, crate::provenance::LexicalCopiedOrigins), crate::lexer::LexError> {
    let (code, tokens) = lexical_minify_renamed_impl(source, true, renames)?;
    let generated_len = code.len();
    Ok((
        code,
        crate::provenance::LexicalCopiedOrigins::new(name, source, generated_len, tokens),
    ))
}

/// Preserve lexical source locations for a renamed lexical output.
pub fn lexical_minify_renamed_with_origins(
    name: &str,
    source: &str,
    renames: &[(usize, usize, String)],
) -> Result<(String, crate::provenance::GeneratedOrigins), crate::lexer::LexError> {
    let (code, compact) = lexical_minify_renamed_with_compact_origins(name, source, renames)?;
    Ok((code, compact.into_generated()))
}

fn lexical_minify_renamed_impl(
    source: &str,
    track: bool,
    renames: &[(usize, usize, String)],
) -> Result<(String, Vec<crate::provenance::CopiedToken>), crate::lexer::LexError> {
    use crate::lexer::{Lexer, TokenKind};
    let tokens = Lexer::new(source).all_borrowed()?;
    let compact_with = |spaced: bool| {
        let mut output = String::with_capacity(source.len());
        let mut end = 0;
        let mut previous = None::<u8>;
        let mut needs_validation = false;
        let mut copies = Vec::new();
        for token in tokens.iter().filter(|t| t.k != TokenKind::Eof) {
            let gap = &source.as_bytes()[end..token.p];
            let before = output.len();
            for &byte in gap {
                if matches!(byte, b'\n' | b'\r') {
                    output.push(byte as char);
                }
            }
            let renamed_piece = lexical_rename_for(renames, token.p, token.p + token.v.len());
            let piece = renamed_piece.unwrap_or(token.v);
            let first = piece.as_bytes().first().copied();
            let needs_boundary = previous.zip(first).is_some_and(|(x, y)| {
                ((x.is_ascii_alphanumeric() || x == b'_')
                    && (y.is_ascii_alphanumeric() || y == b'_'))
                    || (x == b'-' && y == b'-')
                    || (x == b'.' && y == b'.')
                    || (x.is_ascii_digit() && y == b'.')
            });
            if before == output.len() && !output.is_empty() {
                if spaced || needs_boundary {
                    output.push(' ');
                } else if previous
                    .zip(first)
                    .is_some_and(|(left, right)| lexical_boundary_may_retokenize(left, right))
                {
                    needs_validation = true;
                }
            }
            let generated_start = output.len();
            output.push_str(piece);
            previous = piece.as_bytes().last().copied().or(previous);
            end = token.p + token.v.len();
            if track {
                copies.push(crate::provenance::CopiedToken {
                    start: generated_start,
                    source_start: token.p,
                    source_end: end,
                    is_name: token.k == TokenKind::Id,
                    renamed: renamed_piece.is_some(),
                    generated_end: output.len(),
                });
            }
        }
        for &byte in &source.as_bytes()[end..] {
            if matches!(byte, b'\n' | b'\r') {
                output.push(byte as char);
            }
        }
        (output, copies, needs_validation)
    };
    let (mut compact, mut copies, needs_validation) = compact_with(false);
    let matches = !needs_validation || renamed_tokens_match(renames, &tokens, &compact);
    if !matches {
        let (spaced, spaced_copies, _) = compact_with(true);
        compact = spaced;
        copies = spaced_copies;
    }
    if crate::source_position::utf16_len(&compact) > crate::source_position::utf16_len(source) {
        // The compact form grew (spacing): try the source verbatim with
        // renames applied, which keeps the original separators. If that
        // does not pay off either, fall back to the unrenamed output.
        let mut verbatim = String::with_capacity(source.len());
        let mut end = 0;
        let mut verbatim_copies = Vec::new();
        for token in tokens.iter().filter(|t| t.k != TokenKind::Eof) {
            verbatim.push_str(&source[end..token.p]);
            let renamed_piece = lexical_rename_for(renames, token.p, token.p + token.v.len());
            let piece = renamed_piece.unwrap_or(token.v);
            let generated_start = verbatim.len();
            verbatim.push_str(piece);
            end = token.p + token.v.len();
            if track {
                verbatim_copies.push(crate::provenance::CopiedToken {
                    start: generated_start,
                    source_start: token.p,
                    source_end: end,
                    is_name: token.k == TokenKind::Id,
                    renamed: renamed_piece.is_some(),
                    generated_end: verbatim.len(),
                });
            }
        }
        verbatim.push_str(&source[end..]);
        if crate::source_position::utf16_len(&verbatim) <= crate::source_position::utf16_len(source)
            && renamed_tokens_match(renames, &tokens, &verbatim)
        {
            return Ok((verbatim, verbatim_copies));
        }
        return lexical_minify_impl(source, track);
    }
    Ok((compact, copies))
}

fn lexical_minify_impl(
    source: &str,
    track: bool,
) -> Result<(String, Vec<crate::provenance::CopiedToken>), crate::lexer::LexError> {
    use crate::lexer::{Lexer, TokenKind};
    let tokens = Lexer::new(source).all_borrowed()?;
    let compact_with = |spaced: bool| {
        let mut output = String::with_capacity(source.len());
        let mut end = 0;
        let mut previous = None::<u8>;
        let mut needs_validation = false;
        let mut copies = Vec::new();
        for token in tokens.iter().filter(|t| t.k != TokenKind::Eof) {
            let gap = &source.as_bytes()[end..token.p];
            let before = output.len();
            for &byte in gap {
                if matches!(byte, b'\n' | b'\r') {
                    output.push(byte as char);
                }
            }
            let first = token.v.as_bytes().first().copied();
            let needs_boundary = previous.zip(first).is_some_and(|(x, y)| {
                ((x.is_ascii_alphanumeric() || x == b'_')
                    && (y.is_ascii_alphanumeric() || y == b'_'))
                    || (x == b'-' && y == b'-')
                    || (x == b'.' && y == b'.')
                    || (x.is_ascii_digit() && y == b'.')
            });
            if before == output.len() && !output.is_empty() {
                if spaced || needs_boundary {
                    output.push(' ');
                } else if previous
                    .zip(first)
                    .is_some_and(|(left, right)| lexical_boundary_may_retokenize(left, right))
                {
                    needs_validation = true;
                }
            }
            let generated_start = output.len();
            output.push_str(token.v);
            previous = token.v.as_bytes().last().copied().or(previous);
            end = token.p + token.v.len();
            if track {
                copies.push(crate::provenance::CopiedToken {
                    start: generated_start,
                    source_start: token.p,
                    source_end: end,
                    is_name: token.k == TokenKind::Id,
                    renamed: false,
                    generated_end: generated_start + token.v.len(),
                });
            }
        }
        for &byte in &source.as_bytes()[end..] {
            if matches!(byte, b'\n' | b'\r') {
                output.push(byte as char);
            }
        }
        (output, copies, needs_validation)
    };
    let (mut compact, mut copies, needs_validation) = compact_with(false);
    // Most token boundaries cannot possibly retokenize. Only run the second
    // lexer when a boundary can form a compound operator, long bracket, or
    // leading-dot number; ambiguous inputs retain the exact historical check.
    let matches = !needs_validation
        || 'tokens: {
            let mut actual = Lexer::new(&compact);
            for expected in tokens.iter().filter(|t| t.k != TokenKind::Eof) {
                match actual.next_borrowed() {
                    Ok(token)
                        if token.k == expected.k
                            && token.v == expected.v
                            && token.line == expected.line => {}
                    _ => break 'tokens false,
                }
            }
            matches!(actual.next_borrowed(),Ok(token) if token.k==TokenKind::Eof)
        };
    // Multi-character delimiters can require an explicit boundary. This branch
    // uses the same exact tokens and original line breaks, not a second parser.
    if !matches {
        let (spaced, spaced_copies, _) = compact_with(true);
        compact = spaced;
        copies = spaced_copies;
    }
    if crate::source_position::utf16_len(&compact) > crate::source_position::utf16_len(source) {
        let copies = if track {
            tokens
                .iter()
                .filter(|token| token.k != TokenKind::Eof)
                .map(|token| crate::provenance::CopiedToken {
                    start: token.p,
                    source_start: token.p,
                    source_end: token.p + token.v.len(),
                    is_name: token.k == TokenKind::Id,
                    renamed: false,
                    generated_end: token.p + token.v.len(),
                })
                .collect()
        } else {
            Vec::new()
        };
        return Ok((source.to_owned(), copies));
    }
    Ok((compact, copies))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod emission_tests {
    use super::*;
    use crate::parser::{parse_source_with_positions, NameSite};

    fn fixtures() -> [&'static str; 5] {
        [
            "local first,second=1,nil;local function f(x,...) if x then return ... elseif second then return false else return first end end output.setNumber(1,f(true))",
            "local t={7,field=1,['plain']=2,[ [=[long]=] ]=3}; function obj.part:method(x,...)for i=1,8,2 do t[i]=x end for key,value in pairs(t)do value=key end end",
            "do local x=0 repeat x=x+1 until x>3 while x<8 do x=x+1 end end ::again:: goto again",
            "local banner='雪😀';local f=function(a)return a end\r\noutput.setNumber(1,(f)(0x10))\r\nscreen.drawText(1,1,'雪😀')",
            "local x=nil local y,z=f(),nil;return -(-1),1 .. 'a',true,false,f{a=1},obj:m'hello'",
        ]
    }

    #[test]
    fn recording_positions_keeps_exact_bytes_and_newline_count() {
        for source in fixtures() {
            let (ast, root, positions) = parse_source_with_positions(source).unwrap();
            for zero in [false, true] {
                let mut plain = Printer::new(&ast, zero);
                let expected = plain.output(root);
                let mut mapped = Printer::new(&ast, zero);
                let actual = mapped.output_with_positions(root);
                assert_eq!(actual.code, expected, "source={source}");
                assert_eq!(mapped.newline_count(), plain.newline_count());
                assert!(!actual.emissions.is_empty());
                for range in actual.emissions {
                    assert!(range.start < range.end && range.end <= actual.code.len());
                    assert!(
                        actual.code.is_char_boundary(range.start)
                            && actual.code.is_char_boundary(range.end)
                    );
                    assert!(positions.span(range.node).is_some());
                }
                assert!(
                    !mapped.capture_emissions,
                    "capture must not leak into a later regular print"
                );
            }
        }
    }

    #[test]
    fn every_explicit_name_slot_maps_to_its_own_original_identifier() {
        for source in fixtures() {
            let (ast, root, positions) = parse_source_with_positions(source).unwrap();
            let printed = Printer::new(&ast, true).output_with_positions(root);
            let mut names = 0;
            for emission in &printed.emissions {
                let Some(site) = emission.site else { continue };
                let Some((start, end)) = positions.name_span(emission.node, site) else {
                    // A quoted key may be printed using dot syntax. It keeps the
                    // literal node's origin, but has no original identifier slot.
                    assert_eq!(site, NameSite::Member);
                    assert!(matches!(ast.node(emission.node), Node::Str(_)));
                    continue;
                };
                assert_eq!(
                    &printed.code[emission.start..emission.end],
                    &source[start..end]
                );
                names += 1;
            }
            assert!(names > 0);
        }
    }

    #[test]
    fn normalized_literals_and_omitted_nil_have_honest_generated_ranges() {
        let source = "local unused=nil;return 0x10,true,false,'hello'";
        let (ast, root, positions) = parse_source_with_positions(source).unwrap();
        let output = Printer::new(&ast, false).output_with_positions(root);
        for (i, node) in ast.nodes.iter().enumerate() {
            let id = i as NodeId;
            let emissions = output
                .emissions
                .iter()
                .filter(|e| e.node == id && e.site.is_none())
                .collect::<Vec<_>>();
            match node {
                Node::Nil => assert!(
                    emissions.is_empty(),
                    "omitted RHS must not get a generated position"
                ),
                Node::Num(_) | Node::Bool(_) | Node::Str(_) => {
                    assert_eq!(emissions.len(), 1);
                    let printed = &output.code[emissions[0].start..emissions[0].end];
                    let (start, end) = positions.span(id).unwrap();
                    let expected = match &source[start..end] {
                        "0x10" => "16",
                        "true" => "1>0",
                        "false" => "1>2",
                        "'hello'" => "\"hello\"",
                        other => panic!("unexpected original literal {other}"),
                    };
                    assert_eq!(printed, expected);
                }
                _ => {}
            }
        }
    }

    #[test]
    fn empty_else_branch_prints_like_no_else() {
        let with_empty = "if a then b() else end";
        let without = "if a then b() end";
        let nonempty = "if a then b() else c() end";
        for zero in [false, true] {
            let (ast, root, _) = parse_source_with_positions(with_empty).unwrap();
            let empty_out = Printer::new(&ast, zero).output(root);
            let (ast, root, _) = parse_source_with_positions(without).unwrap();
            let no_else_out = Printer::new(&ast, zero).output(root);
            assert_eq!(empty_out, no_else_out, "zero={zero}");
            assert!(!empty_out.contains("else"), "zero={zero}");
            let (ast, root, _) = parse_source_with_positions(nonempty).unwrap();
            let kept = Printer::new(&ast, zero).output(root);
            assert!(kept.contains("else"), "zero={zero}");
        }
    }

    #[test]
    fn dead_if_with_effect_free_condition_prints_like_no_statement() {
        // Dropped: bare Name, literal, and/or/==/not chains over safe
        // operands, and a vararg condition.
        let dropped = [
            "if a then end",
            "if true then end",
            "if a and b then end",
            "if a == b then end",
            "if not a then end",
            "if ... then end",
        ];
        // Kept: calls, index reads (may throw on a nil base), arithmetic,
        // concatenation, computed table keys, and non-empty branches.
        let kept = [
            "if f() then end",
            "if a.b then end",
            "if a + b then end",
            "if a .. b then end",
            "if {[a] = 1} then end",
            "if a then b() end",
            "if a then else b() end",
        ];
        for zero in [false, true] {
            for source in dropped {
                let (ast, root, _) = parse_source_with_positions(source).unwrap();
                let out = Printer::new(&ast, zero).output(root);
                assert_eq!(out, "", "dropped {source} zero={zero}");
                // Tracked printing agrees and records no emissions.
                let (ast, root, _) = parse_source_with_positions(source).unwrap();
                let tracked = Printer::new(&ast, zero).output_with_positions(root);
                assert_eq!(tracked.code, "", "dropped {source} zero={zero}");
                assert!(tracked.emissions.is_empty(), "dropped {source} zero={zero}");
            }
            for source in kept {
                let (ast, root, _) = parse_source_with_positions(source).unwrap();
                let out = Printer::new(&ast, zero).output(root);
                assert!(!out.is_empty(), "kept {source} zero={zero}");
                assert!(out.contains("if"), "kept {source} zero={zero}");
            }
        }
        // A dead if between live statements leaves exactly one separator
        // (here the neighbours need one: digit followed by identifier).
        let mixed = "a=1 if b then end c=2";
        for zero in [false, true] {
            let (ast, root, _) = parse_source_with_positions(mixed).unwrap();
            let out = Printer::new(&ast, zero).output(root);
            if zero {
                assert_eq!(out, "a=1\nc=2", "zero={zero}");
            } else {
                assert_eq!(out, "a=1 c=2", "zero={zero}");
            }
        }
    }

    #[test]
    fn numeric_for_step_one_prints_like_no_step() {
        // Omitted: integer spellings of 1, including leading zeros, hex,
        // and a parenthesized step. Each prints exactly like the no-step
        // form in both separator modes.
        let omitted = [
            "for i=1,10,1 do f() end",
            "for i=1,10,01 do f() end",
            "for i=1,10,0x1 do f() end",
            "for i=1,10,(1) do f() end",
        ];
        let no_step = "for i=1,10 do f() end";
        for source in omitted {
            for zero in [false, true] {
                let (ast, root, _) = parse_source_with_positions(source).unwrap();
                let out = Printer::new(&ast, zero).output(root);
                let (ast, root, _) = parse_source_with_positions(no_step).unwrap();
                let expected = Printer::new(&ast, zero).output(root);
                assert_eq!(out, expected, "{source} zero={zero}");
            }
        }
        // Kept: float steps (dropping one would change the loop kind),
        // other steps, and negative steps.
        let kept = [
            "for i=1,10,1.0 do f() end",
            "for i=1,10,1e0 do f() end",
            "for i=1,10,2 do f() end",
            "for i=1,10,-1 do f() end",
        ];
        for source in kept {
            for zero in [false, true] {
                let (ast, root, _) = parse_source_with_positions(source).unwrap();
                let out = Printer::new(&ast, zero).output(root);
                let (ast, root, _) = parse_source_with_positions(no_step).unwrap();
                let plain = Printer::new(&ast, zero).output(root);
                assert_ne!(out, plain, "{source} zero={zero}");
                assert!(out.contains("do"), "{source} zero={zero}");
            }
        }
        // The float spellings stay float: 1.0 prints with its .0, and
        // 1e0 normalizes to the float spelling 1.0, not to integer 1.
        for (source, step_text) in [
            ("for i=1,10,1.0 do f() end", ",1.0"),
            ("for i=1,10,1e0 do f() end", ",1.0"),
        ] {
            let (ast, root, _) = parse_source_with_positions(source).unwrap();
            let out = Printer::new(&ast, false).output(root);
            assert!(out.contains(step_text), "{source} -> {out}");
        }
    }

    #[test]
    fn consecutive_locals_merge_only_when_positionally_safe() {
        // Merged: each eliminated statement saves its `local` keyword (5
        // effective chars); the separator after `local` is a space or a
        // zero-cost newline depending on the mode.
        let merged = [
            ("local a=1 local b=2", "local a,b=1,2", "local\na,b=1,2"),
            (
                "local a=1 local b=2 local c=3",
                "local a,b,c=1,2,3",
                "local\na,b,c=1,2,3",
            ),
            (
                "local a,b=1,2 local c=3",
                "local a,b,c=1,2,3",
                "local\na,b,c=1,2,3",
            ),
            ("local a=1 local b", "local a,b=1", "local\na,b=1"),
            (
                "local a=f() local b=2",
                "local a,b=f(),2",
                "local\na,b=f(),2",
            ),
        ];
        for (source, expected_space, expected_newline) in merged {
            for (zero, expected) in [(false, expected_space), (true, expected_newline)] {
                let (ast, root, _) = parse_source_with_positions(source).unwrap();
                let out = Printer::new(&ast, zero).output(root);
                assert_eq!(out, expected, "merged {source} zero={zero}");
                // Tracked printing agrees on the code.
                let (ast, root, _) = parse_source_with_positions(source).unwrap();
                let tracked = Printer::new(&ast, zero).output_with_positions(root);
                assert_eq!(tracked.code, expected, "merged {source} zero={zero}");
            }
        }
        // Kept separate: the forbidden string is the exact merged form
        // each case would have produced, so a partial match on the first
        // statement alone (e.g. `local a,b=1`) cannot trip the check.
        let separate = [
            ("local a local b=2", "local a,b=2"),
            ("local a,b=1,nil local c=2", "local a,b,c="),
            ("local a=f() local b", "local a,b=f()"),
            ("local a=1 local b=a", "local a,b=1,a"),
            ("local a=1 local a=2", "local a,a="),
            ("local a=1 local f=function() return a end", "local a,f="),
            ("local a=1,2 local b=3", "local a,b=1,2,3"),
            ("local a=1 b() local c=2", "local a,c="),
        ];
        for (source, forbidden) in separate {
            for zero in [false, true] {
                let (ast, root, _) = parse_source_with_positions(source).unwrap();
                let out = Printer::new(&ast, zero).output(root);
                assert!(
                    !out.contains(forbidden),
                    "separate {source} zero={zero} got {out}"
                );
                // No statement vanished: the output still has two `local`
                // keywords (or one `local` plus the breaker statement).
                assert_eq!(
                    out.matches("local").count(),
                    source.matches("local").count(),
                    "separate {source} zero={zero} got {out}"
                );
            }
        }
        // Merging also applies inside a nested block.
        for (zero, expected) in [
            (false, "do local a,b=1,2 end"),
            (true, "do\nlocal\na,b=1,2\nend"),
        ] {
            let (ast, root, _) = parse_source_with_positions("do local a=1 local b=2 end").unwrap();
            let out = Printer::new(&ast, zero).output(root);
            assert_eq!(out, expected, "nested zero={zero}");
        }
    }

    #[test]
    fn function_assignment_prints_as_function_statement() {
        // Converted: bare Name and pure dot-chain targets with a single
        // function value. The "=" is replaced by the separator after
        // "function", so the raw length is unchanged.
        let converted = [
            "g=function() return 1 end",
            "t.m=function(x) return x end",
            "a.b.c=function() return 2 end",
        ];
        for source in converted {
            for zero in [false, true] {
                let (ast, root, _) = parse_source_with_positions(source).unwrap();
                let out = Printer::new(&ast, zero).output(root);
                assert!(
                    out.starts_with("function"),
                    "converted {source} zero={zero} got {out}"
                );
                assert!(
                    !out.contains('='),
                    "converted {source} zero={zero} got {out}"
                );
                let (ast, root, _) = parse_source_with_positions(source).unwrap();
                let tracked = Printer::new(&ast, zero).output_with_positions(root);
                assert_eq!(tracked.code, out, "converted {source} zero={zero}");
                // The moved `function` keyword keeps its recorded segment
                // on the Function node, so the Source Map loses nothing.
                let func_id = ast
                    .nodes
                    .iter()
                    .enumerate()
                    .find(|(_, n)| matches!(n, Node::Function(..)))
                    .map(|(i, _)| i as NodeId)
                    .expect("function node");
                assert!(
                    tracked
                        .emissions
                        .iter()
                        .any(|e| e.node == func_id && &tracked.code[e.start..e.end] == "function"),
                    "converted {source} zero={zero} has no keyword emission"
                );
            }
        }
        // Kept as assignments: bracket targets, a bracket link inside
        // the chain, a parenthesized target, multiple targets or values,
        // and a non-function value.
        let kept = [
            "t[\"m\"]=function() return 1 end",
            "t[\"k\"].m=function() return 1 end",
            "(t).m=function() return 1 end",
            "a,b=function() return 1 end,2",
            "g=function() return 1 end,2",
            "g=1",
        ];
        for source in kept {
            for zero in [false, true] {
                let (ast, root, _) = parse_source_with_positions(source).unwrap();
                let out = Printer::new(&ast, zero).output(root);
                assert!(out.contains('='), "kept {source} zero={zero} got {out}");
                assert!(
                    !out.starts_with("function"),
                    "kept {source} zero={zero} got {out}"
                );
            }
        }
    }

    #[test]
    fn same_spelling_at_different_positions_is_not_resolved_by_text_search() {
        let source = "local x=1 do local x=x+1 output.setNumber(1,x)end output.setNumber(2,x)";
        let (ast, root, positions) = parse_source_with_positions(source).unwrap();
        let output = Printer::new(&ast, true).output_with_positions(root);
        let mut pairs = Vec::new();
        for emission in output.emissions {
            let Some(site) = emission.site else { continue };
            if &output.code[emission.start..emission.end] == "x" {
                pairs.push((
                    emission.start,
                    positions.name_span(emission.node, site).unwrap().0,
                ));
            }
        }
        pairs.sort_unstable();
        assert_eq!(pairs.len(), 5);
        let original = source
            .match_indices('x')
            .map(|(start, _)| start)
            .collect::<Vec<_>>();
        assert_eq!(
            pairs.iter().map(|pair| pair.1).collect::<Vec<_>>(),
            original
        );
        assert!(pairs.windows(2).all(|p| p[0].0 < p[1].0));
    }

    #[test]
    fn untracked_fragments_allocate_no_emission_vectors() {
        for source in fixtures() {
            let (ast, root, _) = parse_source_with_positions(source).unwrap();
            let result = Printer::new(&ast, false).block(root);
            assert!(result.root.is_none());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod token_baseline_size_tests {
    use super::*;
    fn reference_token_minify(source: &str) -> Result<String, crate::lexer::LexError> {
        use crate::lexer::{Lexer, TokenKind};
        let mut out = String::with_capacity(source.len());
        let mut lexer = Lexer::new(source);
        let ts = lexer.all_borrowed()?;
        for t in ts {
            if t.k == TokenKind::Eof {
                continue;
            }
            let value = match t.k {
                TokenKind::Str => Cow::Owned(qstr(t.v)),
                TokenKind::Num => render_num(t.v),
                _ => Cow::Borrowed(t.v),
            };
            if needs_sep(&out, &value) {
                out.push(' ');
            }
            out.push_str(&value);
        }
        Ok(out)
    }

    #[test]
    fn token_baseline_length_and_bytes_match_previous_materialized_stream() {
        let atoms = [
            "x",
            "if",
            "-",
            ".",
            "..",
            "...",
            "1",
            "0x1p3",
            ".5",
            "'雪😀'",
            "[=[literal]=]",
            "'a\\nb'",
            "(",
            ")",
            "[",
            "]",
            ";",
            "123e-2",
        ];
        for left in atoms {
            for right in atoms {
                let source = format!("{left} -- ignored\n {right}");
                let previous = reference_token_minify(&source).unwrap();
                assert_eq!(token_minify(&source).unwrap(), previous);
                assert_eq!(
                    token_minified_utf16_len(&source).unwrap(),
                    previous.encode_utf16().count()
                );
            }
        }
        for source in [
            "",
            "-- comment",
            "a- -1",
            "return 1 .. '雪😀'",
            "local _ENV=_ENV\r\nreturn 2*3",
        ] {
            let previous = reference_token_minify(source).unwrap();
            assert_eq!(token_minify(source).unwrap(), previous);
            assert_eq!(
                token_minified_utf16_len(source).unwrap(),
                previous.encode_utf16().count()
            );
        }
        for source in ["'unclosed", "--[=[unclosed", "`"] {
            assert_eq!(
                token_minified_utf16_len(source).unwrap_err().to_string(),
                reference_token_minify(source).unwrap_err().to_string()
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod streamed_lexical_tests {
    use super::*;
    fn reference_lexical_minify_impl(
        source: &str,
        track: bool,
    ) -> Result<(String, Vec<crate::provenance::CopiedToken>), crate::lexer::LexError> {
        use crate::lexer::{Lexer, TokenKind};
        let tokens = Lexer::new(source).all_borrowed()?;
        let compact_with = |spaced: bool| {
            let mut output = String::with_capacity(source.len());
            let mut end = 0;
            let mut copies = Vec::new();
            for token in tokens.iter().filter(|t| t.k != TokenKind::Eof) {
                let gap = &source[end..token.p];
                let before = output.len();
                output.extend(gap.chars().filter(|c| matches!(c, '\n' | '\r')));
                if before == output.len()
                    && !output.is_empty()
                    && (spaced || needs_sep(&output, token.v))
                {
                    output.push(' ');
                }
                let generated_start = output.len();
                output.push_str(token.v);
                end = token.p + token.v.len();
                if track {
                    copies.push(crate::provenance::CopiedToken {
                        start: generated_start,
                        source_start: token.p,
                        source_end: end,
                        is_name: token.k == TokenKind::Id,
                        renamed: false,
                        generated_end: generated_start + token.v.len(),
                    });
                }
            }
            output.extend(source[end..].chars().filter(|c| matches!(c, '\n' | '\r')));
            (output, copies)
        };
        let (mut compact, mut copies) = compact_with(false);
        let expected: Vec<_> = tokens
            .iter()
            .filter(|t| t.k != TokenKind::Eof)
            .map(|t| (&t.k, t.v, t.line))
            .collect();
        let matches = Lexer::new(&compact).all_borrowed().is_ok_and(|actual| {
            actual
                .iter()
                .filter(|t| t.k != TokenKind::Eof)
                .map(|t| (&t.k, t.v, t.line))
                .eq(expected.iter().copied())
        });
        // Multi-character delimiters can require an explicit boundary. This branch
        // uses the same exact tokens and original line breaks, not a second parser.
        if !matches {
            (compact, copies) = compact_with(true);
        }
        if compact.encode_utf16().count() > source.encode_utf16().count() {
            let copies = if track {
                tokens
                    .iter()
                    .filter(|token| token.k != TokenKind::Eof)
                    .map(|token| crate::provenance::CopiedToken {
                        start: token.p,
                        source_start: token.p,
                        source_end: token.p + token.v.len(),
                        is_name: token.k == TokenKind::Id,
                        renamed: false,
                        generated_end: token.p + token.v.len(),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            return Ok((source.to_owned(), copies));
        }
        Ok((compact, copies))
    }

    #[test]
    fn lexical_fast_guard_covers_operator_and_literal_pair_retokenization() {
        use crate::lexer::{Lexer, TokenKind};
        let atoms = [
            "a", "if", "1", "1.", "0x10", "'s'", "[=[s]=]", "+", "-", "*", "/", "%", "^", "#", "&",
            "~", "|", "<", ">", "=", "(", ")", "{", "}", "[", "]", ";", ":", ",", ".", "...", "//",
            "<<", ">>", "==", "~=", "<=", ">=", "::", "..",
        ];
        let signature = |text: &str| {
            let mut lexer = Lexer::new(text);
            let mut out = Vec::new();
            loop {
                match lexer.next_borrowed() {
                    Ok(token) if token.k != TokenKind::Eof => {
                        out.push((token.k, token.v.to_owned()));
                    }
                    Ok(_) => return Some(out),
                    Err(_) => return None,
                }
            }
        };
        for left in atoms {
            for right in atoms {
                let expected = signature(&format!("{left} {right}"));
                let left_byte = left.as_bytes().last().copied().unwrap();
                let right_byte = right.as_bytes().first().copied().unwrap();
                let separated = needs_sep_chars(left_byte as char, right_byte as char);
                if separated || lexical_boundary_may_retokenize(left_byte, right_byte) {
                    continue;
                }
                assert_eq!(
                    signature(&format!("{left}{right}")),
                    expected,
                    "unguarded lexical boundary {left:?}/{right:?}"
                );
            }
        }
    }

    #[test]
    fn streamed_token_verification_preserves_fallbacks_lines_and_copy_origins() {
        let mut cases = vec![
            "".to_owned(),
            "-- comment\r\n".into(),
            "local x=1 - -2;return x .. '雪😀'".into(),
            "::again:: a=a+1 goto again".into(),
            "local x=[==[one\r\ntwo]==]; -- skip\nreturn x".into(),
            "local _ENV=_ENV\r\nfunction onTick()return 1,2 end".into(),
            "a=1 < <2 a=1 / /2 a=1 . .2 a=1 - -2".into(),
            "a = = b; a ~ = b; a > = b; a : : b; a . 5".into(),
            "[ [x] ] [ = [x] = ] [ = = [x] = = ]".into(),
        ];
        for count in [1, 8, 64] {
            cases.push(format!(
                "-- 😀\r\nfunction f(x){}return x end",
                (0..count)
                    .map(|i| format!("x=x+{i} -- comment\r\n"))
                    .collect::<String>()
            ));
        }
        for source in cases {
            for track in [false, true] {
                let (expected, tokens) = reference_lexical_minify_impl(&source, track).unwrap();
                let (actual, copies) = lexical_minify_impl(&source, track).unwrap();
                assert_eq!(actual, expected);
                if track {
                    let old = crate::provenance::GeneratedOrigins::copied_tokens(
                        "lexical.lua",
                        &source,
                        expected.len(),
                        &tokens,
                    );
                    let new = crate::provenance::GeneratedOrigins::copied_tokens(
                        "lexical.lua",
                        &source,
                        actual.len(),
                        &copies,
                    );
                    assert_eq!(new, old);
                    new.validate_for_code(&actual).unwrap();
                } else {
                    assert!(tokens.is_empty() && copies.is_empty());
                }
            }
        }
        for invalid in ["'unfinished", "[=[unfinished", "--[=[unfinished"] {
            assert_eq!(
                lexical_minify_impl(invalid, false)
                    .err()
                    .map(|e| e.to_string()),
                reference_lexical_minify_impl(invalid, false)
                    .err()
                    .map(|e| e.to_string())
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod borrowed_emission_tests {
    use super::*;
    use crate::{parse_source_with_origins, provenance::GeneratedOrigins};

    #[test]
    fn unchanged_identifier_number_and_string_fragments_borrow_the_immutable_ast() {
        let source = "local long_name=42 local text=\"雪😀\" output.setNumber(1,long_name)";
        let (ast, root) = parse_source_with_origins("borrow.lua", source).unwrap();
        let before = serde_json::to_vec(&ast).unwrap();
        let mut printer = Printer::new(&ast, false);
        let mut checked = 0;
        for (id, node) in ast.nodes.iter().enumerate() {
            let expected = match node {
                Node::Name(symbol) => Some(ast.strings.get(*symbol)),
                Node::Num(raw) if raw.as_ref() == "42" => Some(raw.as_ref()),
                Node::Str(raw) if raw.as_ref() == "\"雪😀\"" => Some(raw.as_ref()),
                _ => None,
            };
            if let Some(expected) = expected {
                let fragment = printer.expr(id as NodeId, 0);
                assert!(matches!(fragment.text, Cow::Borrowed(_)));
                assert_eq!(fragment.text.as_ref(), expected);
                assert_eq!(fragment.text.as_ptr(), expected.as_ptr());
                checked += 1;
            }
        }
        assert!(checked >= 4);
        let output = printer.output_with_positions(root);
        GeneratedOrigins::from_print(&ast, &output)
            .unwrap()
            .validate_for_code(&output.code)
            .unwrap();
        assert_eq!(serde_json::to_vec(&ast).unwrap(), before);
        drop(ast);
        assert!(
            output.code.contains("雪😀"),
            "returned source owns its bytes"
        );
    }

    #[test]
    fn decoded_members_and_quotes_outlive_temporary_spelling_choices() {
        for source in [
            "local t={['\\x61']=1,[\"not a name\"]=2} output.setNumber(1,t['\\97'])",
            "local x={['escaped\\\\name']=\"a\\n\\\"b\",[ [=[long]=] ]=3} return x",
            "function object:method(x,...)return x,... end return object:method'it\\\'s'",
            "function f(x)if x then return 1 elseif x==2 then return 3 elseif x==4 then return 5 else return 6 end end",
        ] {
            let (ast,root)=parse_source_with_origins("temporary.lua",source).unwrap();
            for zero in [false,true] {
                let mut plain=Printer::new(&ast,zero);
                let code=plain.output(root);
                let printed=Printer::new(&ast,zero).output_with_positions(root);
                assert_eq!(code,printed.code);
                crate::parse_source(&code).unwrap();
                let origins=GeneratedOrigins::from_print(&ast,&printed).unwrap();origins.validate_for_code(&code).unwrap();
            }
        }
    }
}

#[cfg(test)]
mod plain_name_dispatch_tests {
    use super::*;
    fn reference(s: &str) -> bool {
        let mut chars = s.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
            _ => return false,
        }
        chars.all(|c| c.is_ascii_alphanumeric() || c == '_') && !crate::lexer::KEYWORDS.contains(&s)
    }
    #[test]
    fn byte_validation_keeps_unicode_rejection_and_exact_keyword_rules() {
        let pieces = [
            "", "_", "a", "A", "Z", "0", "9", "-", ".", " ", "雪", "😀", "é", "and", "function",
            "break",
        ];
        for a in pieces {
            for b in pieces {
                for c in pieces {
                    let s = format!("{a}{b}{c}");
                    assert_eq!(is_plain_name(&s), reference(&s), "{s:?}");
                }
            }
        }
    }
}
