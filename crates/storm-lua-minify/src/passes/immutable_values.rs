//! Immutable global expression reuse (`passes/immutable-values.ts`).
//!
//! A stable, single-write top-level value may replace identical expressions
//! inside function bodies when doing so reduces the scope-renamed output size.
//! Expression identity includes resolved BindingIds, so shadowed names never
//! collide. Expressions containing table or function literals are rejected
//! because they allocate fresh references on each evaluation.

use crate::pass::PassResult;
use crate::scope_rename::measure_renamed_size;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, TableField};

#[derive(Clone)]
struct Carrier {
    bid: BindingId,
    key: String,
    expression: NodeId,
    /// Root statement index that binds a global carrier; `None` for local
    /// carriers, whose lexical scope already keeps replacement sites from
    /// executing before the binding (see LoadOrderGuard, candidate S9).
    root_index: Option<usize>,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn measured(ast: &Ast, root: NodeId) -> usize {
    measure_renamed_size(ast, root)
}

fn is_atom(node: &Node) -> bool {
    matches!(
        node,
        Node::Name(_) | Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
    )
}

fn is_expression_type(node: &Node) -> bool {
    matches!(
        node,
        Node::Name(_)
            | Node::Num(_)
            | Node::Str(_)
            | Node::Bool(_)
            | Node::Nil
            | Node::Paren(_)
            | Node::Table(_)
            | Node::Index(..)
            | Node::Function(..)
            | Node::Un(..)
            | Node::Bin(..)
            | Node::Call(..)
            | Node::Vararg
    )
}

pub(crate) fn contains_fresh_reference(ast: &Ast, node: NodeId) -> bool {
    if matches!(ast.node(node), Node::Table(_) | Node::Function(..)) {
        return true;
    }
    let mut fresh = false;
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        if !fresh && contains_fresh_reference(ast, child) {
            fresh = true;
        }
    });
    fresh
}

fn push_text(key: &mut String, text: &str) {
    key.push_str(&text.len().to_string());
    key.push(':');
    key.push_str(text);
    key.push(';');
}

fn push_node_key(ast: &Ast, resolution: &Resolution, node: NodeId, key: &mut String) {
    match ast.node(node) {
        Node::Nil => key.push_str("nil;"),
        Node::Bool(value) => key.push_str(if *value { "bool1;" } else { "bool0;" }),
        Node::Vararg => key.push_str("vararg;"),
        Node::Num(value) => {
            key.push_str("num;");
            push_text(key, value);
        }
        Node::Str(value) => {
            key.push_str("str;");
            push_text(key, value);
        }
        Node::Name(symbol) => {
            key.push_str("name;");
            push_text(key, ast.strings.get(*symbol));
            match resolution.node_bid.get(node as usize).copied().flatten() {
                Some(bid) => {
                    key.push_str("bid;");
                    key.push_str(&bid.to_string());
                    key.push(';');
                }
                None => key.push_str("bid:none;"),
            }
        }
        Node::Paren(expression) => {
            key.push_str("paren;");
            push_node_key(ast, resolution, *expression, key);
        }
        Node::Un(operator, expression) => {
            key.push_str("un;");
            push_text(key, operator);
            push_node_key(ast, resolution, *expression, key);
        }
        Node::Bin(operator, left, right) => {
            key.push_str("bin;");
            push_text(key, operator);
            push_node_key(ast, resolution, *left, key);
            push_node_key(ast, resolution, *right, key);
        }
        Node::Index(object, index, dot) => {
            key.push_str(if *dot { "index1;" } else { "index0;" });
            push_node_key(ast, resolution, *object, key);
            push_node_key(ast, resolution, *index, key);
        }
        Node::Methodname(object, name) => {
            key.push_str("methodname;");
            push_node_key(ast, resolution, *object, key);
            push_text(key, ast.strings.get(*name));
        }
        Node::Call(function, arguments, method) => {
            key.push_str("call;");
            push_node_key(ast, resolution, *function, key);
            match method {
                Some(method) => {
                    key.push_str("method;");
                    push_text(key, ast.strings.get(*method));
                }
                None => key.push_str("method:none;"),
            }
            key.push_str("args;");
            key.push_str(&arguments.len().to_string());
            key.push(';');
            for argument in arguments {
                push_node_key(ast, resolution, *argument, key);
            }
        }
        Node::Table(fields) => {
            key.push_str("table;");
            key.push_str(&fields.len().to_string());
            key.push(';');
            for field in fields {
                match field {
                    TableField::Arr(value) => {
                        key.push_str("arr;");
                        push_node_key(ast, resolution, *value, key);
                    }
                    TableField::Name(name, value) => {
                        key.push_str("field-name;");
                        push_text(key, ast.strings.get(*name));
                        push_node_key(ast, resolution, *value, key);
                    }
                    TableField::KVar(field_key, value) => {
                        key.push_str("field-key;");
                        push_node_key(ast, resolution, *field_key, key);
                        push_node_key(ast, resolution, *value, key);
                    }
                }
            }
        }
        Node::Function(parameters, variadic, body) => {
            key.push_str(if *variadic {
                "function-vararg;"
            } else {
                "function;"
            });
            key.push_str(&parameters.len().to_string());
            key.push(';');
            for parameter in parameters {
                push_text(key, ast.strings.get(*parameter));
            }
            push_node_key(ast, resolution, *body, key);
        }
        Node::Block(statements) => {
            key.push_str("block;");
            key.push_str(&statements.len().to_string());
            key.push(';');
            for statement in statements {
                push_node_key(ast, resolution, *statement, key);
            }
        }
        Node::Break => key.push_str("break;"),
        Node::Goto(name) => {
            key.push_str("goto;");
            push_text(key, ast.strings.get(*name));
        }
        Node::Label(name) => {
            key.push_str("label;");
            push_text(key, ast.strings.get(*name));
        }
        Node::Do(body) => {
            key.push_str("do;");
            push_node_key(ast, resolution, *body, key);
        }
        Node::While(condition, body) => {
            key.push_str("while;");
            push_node_key(ast, resolution, *condition, key);
            push_node_key(ast, resolution, *body, key);
        }
        Node::Repeat(body, condition) => {
            key.push_str("repeat;");
            push_node_key(ast, resolution, *body, key);
            push_node_key(ast, resolution, *condition, key);
        }
        Node::If(arms, else_block) => {
            key.push_str("if;");
            key.push_str(&arms.len().to_string());
            key.push(';');
            for arm in arms {
                push_node_key(ast, resolution, arm.cond, key);
                push_node_key(ast, resolution, arm.body, key);
            }
            if let Some(else_block) = else_block {
                key.push_str("else;");
                push_node_key(ast, resolution, *else_block, key);
            } else {
                key.push_str("else:none;");
            }
        }
        Node::Fornum(name, start, end, step, body) => {
            key.push_str("fornum;");
            push_text(key, ast.strings.get(*name));
            push_node_key(ast, resolution, *start, key);
            push_node_key(ast, resolution, *end, key);
            if let Some(step) = step {
                push_node_key(ast, resolution, *step, key);
            } else {
                key.push_str("step:none;");
            }
            push_node_key(ast, resolution, *body, key);
        }
        Node::Forin(names, expressions, body) => {
            key.push_str("forin;");
            for name in names {
                push_text(key, ast.strings.get(*name));
            }
            for expression in expressions {
                push_node_key(ast, resolution, *expression, key);
            }
            push_node_key(ast, resolution, *body, key);
        }
        Node::Funcstat(target, function) => {
            key.push_str("funcstat;");
            push_node_key(ast, resolution, *target, key);
            push_node_key(ast, resolution, *function, key);
        }
        Node::Localfunc(name, function) => {
            key.push_str("localfunc;");
            push_text(key, ast.strings.get(*name));
            push_node_key(ast, resolution, *function, key);
        }
        Node::Local(names, expressions) => {
            key.push_str("local;");
            for name in names {
                push_text(key, ast.strings.get(*name));
            }
            for expression in expressions {
                push_node_key(ast, resolution, *expression, key);
            }
        }
        Node::Return(expressions) => {
            key.push_str("return;");
            for expression in expressions {
                push_node_key(ast, resolution, *expression, key);
            }
        }
        Node::Callstat(expression) => {
            key.push_str("callstat;");
            push_node_key(ast, resolution, *expression, key);
        }
        Node::Assign(targets, expressions) => {
            key.push_str("assign;");
            for target in targets {
                push_node_key(ast, resolution, *target, key);
            }
            for expression in expressions {
                push_node_key(ast, resolution, *expression, key);
            }
        }
    }
}

pub(crate) fn expression_key(ast: &Ast, resolution: &Resolution, node: NodeId) -> String {
    let mut key = String::new();
    push_node_key(ast, resolution, node, &mut key);
    key
}

/// Fx-style byte-stream hasher with an internal staging buffer, so the
/// result depends only on the byte sequence, never on how the writes were
/// chunked. Deterministic across runs and processes. This is only ever a
/// pre-filter: bucket hits are verified against the full key.
struct KeyHasher {
    state: u64,
    buf: [u8; 8],
    len: usize,
}

impl KeyHasher {
    const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

    fn new() -> Self {
        Self {
            state: 0,
            buf: [0; 8],
            len: 0,
        }
    }

    fn add(&mut self, value: u64) {
        self.state = (self.state.rotate_left(5) ^ value).wrapping_mul(Self::SEED);
    }

    fn write(&mut self, mut bytes: &[u8]) {
        if self.len > 0 {
            let take = (8 - self.len).min(bytes.len());
            self.buf[self.len..self.len + take].copy_from_slice(&bytes[..take]);
            self.len += take;
            bytes = &bytes[take..];
            if self.len < 8 {
                return;
            }
            self.add(u64::from_le_bytes(self.buf));
            self.len = 0;
        }
        while bytes.len() >= 8 {
            let mut chunk = [0u8; 8];
            chunk.copy_from_slice(&bytes[..8]);
            self.add(u64::from_le_bytes(chunk));
            bytes = &bytes[8..];
        }
        if !bytes.is_empty() {
            self.buf[..bytes.len()].copy_from_slice(bytes);
            self.len = bytes.len();
        }
    }

    /// Decimal ASCII, byte-identical to the `to_string()` the key builder
    /// appends for lengths, counts, and binding ids.
    fn write_count(&mut self, value: usize) {
        let mut buf = [0u8; 20];
        let mut n = value as u64;
        let mut i = buf.len();
        loop {
            i -= 1;
            buf[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        self.write(&buf[i..]);
    }

    /// Same encoding as `push_text`: `<len>:<text>;`.
    fn write_text(&mut self, text: &str) {
        self.write_count(text.len());
        self.write(b":");
        self.write(text.as_bytes());
        self.write(b";");
    }

    fn finish(mut self) -> u64 {
        if self.len > 0 {
            let mut tail = [0u8; 8];
            tail[..self.len].copy_from_slice(&self.buf[..self.len]);
            let mixed =
                u64::from_le_bytes(tail) ^ (self.len as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
            self.add(mixed);
            self.len = 0;
        }
        self.state
    }
}

/// Mirror of `push_node_key` that feeds the identical byte stream to a
/// `KeyHasher` instead of appending it to a `String`. Kept arm-by-arm
/// parallel to `push_node_key`; the unit test
/// `expression_key_hash_matches_materialized_key_bytes` pins the two
/// walkers together over every node kind.
fn push_node_key_hash(ast: &Ast, resolution: &Resolution, node: NodeId, hasher: &mut KeyHasher) {
    match ast.node(node) {
        Node::Nil => hasher.write(b"nil;"),
        Node::Bool(value) => hasher.write(if *value { b"bool1;" } else { b"bool0;" }),
        Node::Vararg => hasher.write(b"vararg;"),
        Node::Num(value) => {
            hasher.write(b"num;");
            hasher.write_text(value);
        }
        Node::Str(value) => {
            hasher.write(b"str;");
            hasher.write_text(value);
        }
        Node::Name(symbol) => {
            hasher.write(b"name;");
            hasher.write_text(ast.strings.get(*symbol));
            match resolution.node_bid.get(node as usize).copied().flatten() {
                Some(bid) => {
                    hasher.write(b"bid;");
                    hasher.write_count(bid as usize);
                    hasher.write(b";");
                }
                None => hasher.write(b"bid:none;"),
            }
        }
        Node::Paren(expression) => {
            hasher.write(b"paren;");
            push_node_key_hash(ast, resolution, *expression, hasher);
        }
        Node::Un(operator, expression) => {
            hasher.write(b"un;");
            hasher.write_text(operator);
            push_node_key_hash(ast, resolution, *expression, hasher);
        }
        Node::Bin(operator, left, right) => {
            hasher.write(b"bin;");
            hasher.write_text(operator);
            push_node_key_hash(ast, resolution, *left, hasher);
            push_node_key_hash(ast, resolution, *right, hasher);
        }
        Node::Index(object, index, dot) => {
            hasher.write(if *dot { b"index1;" } else { b"index0;" });
            push_node_key_hash(ast, resolution, *object, hasher);
            push_node_key_hash(ast, resolution, *index, hasher);
        }
        Node::Methodname(object, name) => {
            hasher.write(b"methodname;");
            push_node_key_hash(ast, resolution, *object, hasher);
            hasher.write_text(ast.strings.get(*name));
        }
        Node::Call(function, arguments, method) => {
            hasher.write(b"call;");
            push_node_key_hash(ast, resolution, *function, hasher);
            match method {
                Some(method) => {
                    hasher.write(b"method;");
                    hasher.write_text(ast.strings.get(*method));
                }
                None => hasher.write(b"method:none;"),
            }
            hasher.write(b"args;");
            hasher.write_count(arguments.len());
            hasher.write(b";");
            for argument in arguments {
                push_node_key_hash(ast, resolution, *argument, hasher);
            }
        }
        Node::Table(fields) => {
            hasher.write(b"table;");
            hasher.write_count(fields.len());
            hasher.write(b";");
            for field in fields {
                match field {
                    TableField::Arr(value) => {
                        hasher.write(b"arr;");
                        push_node_key_hash(ast, resolution, *value, hasher);
                    }
                    TableField::Name(name, value) => {
                        hasher.write(b"field-name;");
                        hasher.write_text(ast.strings.get(*name));
                        push_node_key_hash(ast, resolution, *value, hasher);
                    }
                    TableField::KVar(field_key, value) => {
                        hasher.write(b"field-key;");
                        push_node_key_hash(ast, resolution, *field_key, hasher);
                        push_node_key_hash(ast, resolution, *value, hasher);
                    }
                }
            }
        }
        Node::Function(parameters, variadic, body) => {
            hasher.write(if *variadic {
                b"function-vararg;"
            } else {
                b"function;"
            });
            hasher.write_count(parameters.len());
            hasher.write(b";");
            for parameter in parameters {
                hasher.write_text(ast.strings.get(*parameter));
            }
            push_node_key_hash(ast, resolution, *body, hasher);
        }
        Node::Block(statements) => {
            hasher.write(b"block;");
            hasher.write_count(statements.len());
            hasher.write(b";");
            for statement in statements {
                push_node_key_hash(ast, resolution, *statement, hasher);
            }
        }
        Node::Break => hasher.write(b"break;"),
        Node::Goto(name) => {
            hasher.write(b"goto;");
            hasher.write_text(ast.strings.get(*name));
        }
        Node::Label(name) => {
            hasher.write(b"label;");
            hasher.write_text(ast.strings.get(*name));
        }
        Node::Do(body) => {
            hasher.write(b"do;");
            push_node_key_hash(ast, resolution, *body, hasher);
        }
        Node::While(condition, body) => {
            hasher.write(b"while;");
            push_node_key_hash(ast, resolution, *condition, hasher);
            push_node_key_hash(ast, resolution, *body, hasher);
        }
        Node::Repeat(body, condition) => {
            hasher.write(b"repeat;");
            push_node_key_hash(ast, resolution, *body, hasher);
            push_node_key_hash(ast, resolution, *condition, hasher);
        }
        Node::If(arms, else_block) => {
            hasher.write(b"if;");
            hasher.write_count(arms.len());
            hasher.write(b";");
            for arm in arms {
                push_node_key_hash(ast, resolution, arm.cond, hasher);
                push_node_key_hash(ast, resolution, arm.body, hasher);
            }
            if let Some(else_block) = else_block {
                hasher.write(b"else;");
                push_node_key_hash(ast, resolution, *else_block, hasher);
            } else {
                hasher.write(b"else:none;");
            }
        }
        Node::Fornum(name, start, end, step, body) => {
            hasher.write(b"fornum;");
            hasher.write_text(ast.strings.get(*name));
            push_node_key_hash(ast, resolution, *start, hasher);
            push_node_key_hash(ast, resolution, *end, hasher);
            if let Some(step) = step {
                push_node_key_hash(ast, resolution, *step, hasher);
            } else {
                hasher.write(b"step:none;");
            }
            push_node_key_hash(ast, resolution, *body, hasher);
        }
        Node::Forin(names, expressions, body) => {
            hasher.write(b"forin;");
            for name in names {
                hasher.write_text(ast.strings.get(*name));
            }
            for expression in expressions {
                push_node_key_hash(ast, resolution, *expression, hasher);
            }
            push_node_key_hash(ast, resolution, *body, hasher);
        }
        Node::Funcstat(target, function) => {
            hasher.write(b"funcstat;");
            push_node_key_hash(ast, resolution, *target, hasher);
            push_node_key_hash(ast, resolution, *function, hasher);
        }
        Node::Localfunc(name, function) => {
            hasher.write(b"localfunc;");
            hasher.write_text(ast.strings.get(*name));
            push_node_key_hash(ast, resolution, *function, hasher);
        }
        Node::Local(names, expressions) => {
            hasher.write(b"local;");
            for name in names {
                hasher.write_text(ast.strings.get(*name));
            }
            for expression in expressions {
                push_node_key_hash(ast, resolution, *expression, hasher);
            }
        }
        Node::Return(expressions) => {
            hasher.write(b"return;");
            for expression in expressions {
                push_node_key_hash(ast, resolution, *expression, hasher);
            }
        }
        Node::Callstat(expression) => {
            hasher.write(b"callstat;");
            push_node_key_hash(ast, resolution, *expression, hasher);
        }
        Node::Assign(targets, expressions) => {
            hasher.write(b"assign;");
            for target in targets {
                push_node_key_hash(ast, resolution, *target, hasher);
            }
            for expression in expressions {
                push_node_key_hash(ast, resolution, *expression, hasher);
            }
        }
    }
}

/// Deterministic structural hash of the exact byte stream
/// `expression_key` would produce for `node`, computed without
/// materializing the string. Equal keys always hash equal, so indexing
/// by this hash never misses a candidate the string key would find;
/// the converse does not hold, so a bucket hit must still be verified
/// against the full key before it is treated as a match.
pub(crate) fn expression_key_hash(ast: &Ast, resolution: &Resolution, node: NodeId) -> u64 {
    let mut hasher = KeyHasher::new();
    push_node_key_hash(ast, resolution, node, &mut hasher);
    hasher.finish()
}

/// Hash of a materialized key's bytes through the same `KeyHasher`
/// `expression_key_hash` streams into. Test support only: agreement of
/// the two on every node pins the hash walker to the key walker.
#[cfg(test)]
fn key_bytes_hash(key: &str) -> u64 {
    let mut hasher = KeyHasher::new();
    hasher.write(key.as_bytes());
    hasher.finish()
}

/// Key for one member-chain step. String keys decode to their string
/// value, so `t.x` and `t["x"]` — which parse to the same key node —
/// agree, and differently-escaped spellings of one string also agree.
/// Numeric keys normalize by value (`1`, `1.0`, and `0x1` are the same
/// table key). Any other key expression falls back to the binding-aware
/// expression key.
pub(crate) fn chain_step_key(ast: &Ast, resolution: &Resolution, key: NodeId) -> String {
    match ast.node(key) {
        Node::Str(raw) => match storm_lua_analysis::effects::decode_lua_string(raw) {
            Some(decoded) => format!("s:{decoded}"),
            None => format!("s:{raw}"),
        },
        Node::Num(raw) => {
            let text = raw.as_ref();
            if let Some(integer) = storm_lua_syntax::numeric::integer_literal_value(text) {
                return format!("i:{integer}");
            }
            let value = storm_lua_syntax::numeric::num_val(text);
            if value.is_finite() && value.fract() == 0.0 && value.abs() < 9.007_199_254_740_992e15 {
                format!("i:{}", value as i64)
            } else {
                format!("f:{}", storm_lua_syntax::numeric::short_num(value))
            }
        }
        _ => format!("e:{}", expression_key(ast, resolution, key)),
    }
}

/// Decompose a member chain into its root bindings and step keys in
/// root-to-leaf order. Parentheses are transparent; an `and`/`or` base
/// contributes several roots and requires both sides to share the same
/// steps. Anything else (calls, tables, mismatched branches) is not a
/// plain member chain and yields `None`.
pub(crate) fn decompose_chain(
    ast: &Ast,
    resolution: &Resolution,
    node: NodeId,
) -> Option<(Vec<BindingId>, Vec<String>)> {
    match ast.node(node) {
        Node::Name(_) => resolution
            .node_bid
            .get(node as usize)
            .copied()
            .flatten()
            .map(|bid| (vec![bid], Vec::new())),
        Node::Paren(inner) => decompose_chain(ast, resolution, *inner),
        Node::Index(object, key, _) => {
            let (roots, mut steps) = decompose_chain(ast, resolution, *object)?;
            steps.push(chain_step_key(ast, resolution, *key));
            Some((roots, steps))
        }
        Node::Bin(operator, left, right) if matches!(operator.as_str(), "and" | "or") => {
            let (mut roots, steps) = decompose_chain(ast, resolution, *left)?;
            let (right_roots, right_steps) = decompose_chain(ast, resolution, *right)?;
            if steps != right_steps {
                return None;
            }
            roots.extend(right_roots);
            Some((roots, steps))
        }
        _ => None,
    }
}

/// Binding-blind syntactic key: `push_node_key` run with an empty resolution,
/// so every `Name` records `bid:none`. Bid-aware key equality implies syntactic
/// key equality (the bid segments are the only resolution-dependent parts), so
/// a syntactic mismatch proves that no candidate can match: skipping the whole
/// pass on a syntactic miss preserves the winner exactly. A syntactic hit only
/// means "may match", so the original path still runs unchanged.
fn syntactic_key(ast: &Ast, node: NodeId) -> String {
    let blank = Resolution {
        bindings: Vec::new(),
        scopes: Vec::new(),
        globals: Vec::new(),
        node_scope_id: Vec::new(),
        node_bid: Vec::new(),
        node_bids: Vec::new(),
        node_write: Vec::new(),
        binding_write_counts: Vec::new(),
    };
    expression_key(ast, &blank, node)
}

/// Necessary condition for `reuse_immutable_values` to fire: some top-level
/// carrier-candidate expression (a non-atom right-hand side of a top-level
/// `Local`/`Assign`) must be syntactically equal to some expression inside a
/// function body. Carrier roots are never atoms, tables, or functions (atoms
/// are rejected by `safe_carrier_expression`, tables/functions by the
/// fresh-reference check), so those node kinds are excluded from both sides
/// without weakening the condition.
fn reuse_gate_possible(ast: &Ast, root: NodeId) -> bool {
    let Node::Block(statements) = ast.node(root) else {
        return false;
    };
    let mut candidates: std::collections::HashSet<String> = std::collections::HashSet::new();
    for statement in statements {
        match ast.node(*statement) {
            Node::Local(_, expressions) => {
                for expression in expressions {
                    if !is_atom(ast.node(*expression)) {
                        candidates.insert(syntactic_key(ast, *expression));
                    }
                }
            }
            Node::Assign(targets, expressions) if targets.len() == expressions.len() => {
                for (target, expression) in targets.iter().copied().zip(expressions.iter().copied())
                {
                    if matches!(ast.node(target), Node::Name(_)) && !is_atom(ast.node(expression)) {
                        candidates.insert(syntactic_key(ast, expression));
                    }
                }
            }
            _ => {}
        }
    }
    if candidates.is_empty() {
        return false;
    }
    fn visit(
        ast: &Ast,
        node: NodeId,
        in_function: bool,
        candidates: &std::collections::HashSet<String>,
        hit: &mut bool,
    ) {
        if *hit {
            return;
        }
        if in_function
            && is_expression_type(ast.node(node))
            && !is_atom(ast.node(node))
            && !matches!(ast.node(node), Node::Function(..) | Node::Table(_))
            && candidates.contains(&syntactic_key(ast, node))
        {
            *hit = true;
            return;
        }
        let nested_in_function = in_function || matches!(ast.node(node), Node::Function(..));
        storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
            visit(ast, child, nested_in_function, candidates, hit);
        });
    }
    let mut hit = false;
    visit(ast, root, false, &candidates, &mut hit);
    hit
}

/// Position of every node relative to the root block: which top-level
/// statement contains it, and whether a function boundary intervenes.
/// A dependency write is only provably ordered before a carrier when it
/// sits in an earlier top-level statement outside any function body: a
/// write inside a function may execute at any later call time, and a
/// write in a later (or the same) statement has not happened yet when
/// the carrier is defined. Replacement sites can only reference the
/// carrier binding from within its scope, which starts at the carrier
/// statement, so every site executes after the carrier statement and
/// therefore after all such early writes.
struct RootPositions {
    statement: Vec<Option<usize>>,
    in_function: Vec<bool>,
}

fn root_positions(ast: &Ast, resolution: &Resolution, root: NodeId) -> RootPositions {
    let mut positions = RootPositions {
        statement: vec![None; resolution.node_bid.len()],
        in_function: vec![false; resolution.node_bid.len()],
    };
    let Node::Block(statements) = ast.node(root) else {
        return positions;
    };
    fn visit(
        ast: &Ast,
        positions: &mut RootPositions,
        node: NodeId,
        index: usize,
        in_function: bool,
    ) {
        if let Some(slot) = positions.statement.get_mut(node as usize) {
            *slot = Some(index);
        }
        if let Some(slot) = positions.in_function.get_mut(node as usize) {
            *slot = in_function;
        }
        let nested = in_function || matches!(ast.node(node), Node::Function(..));
        storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
            visit(ast, positions, child, index, nested);
        });
    }
    for (index, statement) in statements.iter().enumerate() {
        visit(ast, &mut positions, *statement, index, false);
    }
    positions
}

/// Whether the root block transfers control non-linearly outside function
/// bodies. With a `goto`/`label` present, top-level statement order no
/// longer proves that an earlier statement's write ran before the
/// carrier statement, so single-write dependencies are not accepted.
fn root_has_flow_transfer(ast: &Ast, root: NodeId) -> bool {
    fn visit(ast: &Ast, node: NodeId, in_function: bool, found: &mut bool) {
        if *found {
            return;
        }
        if !in_function && matches!(ast.node(node), Node::Goto(_) | Node::Label(_)) {
            *found = true;
            return;
        }
        let nested = in_function || matches!(ast.node(node), Node::Function(..));
        storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
            visit(ast, child, nested, found);
        });
    }
    let mut found = false;
    visit(ast, root, false, &mut found);
    found
}

fn dependencies_are_single_write(
    analyzer: &EffectAnalyzer<'_>,
    resolution: &Resolution,
    positions: &RootPositions,
    flow_transfer: bool,
    statement_index: usize,
    expression: NodeId,
) -> bool {
    analyzer
        .effects_for_expr(expression)
        .reads
        .iter()
        .all(|bid| {
            let count = resolution
                .binding_write_counts
                .get(*bid as usize)
                .copied()
                .unwrap_or(0);
            if count == 0 {
                return true;
            }
            if count > 1 || flow_transfer {
                return false;
            }
            // Exactly one write: it must already have executed when the
            // carrier statement runs, i.e. it sits in an earlier top-level
            // statement and is not deferred inside a function body.
            resolution
                .node_write
                .iter()
                .enumerate()
                .all(|(node, is_write)| {
                    !*is_write
                        || resolution.node_bid.get(node).copied().flatten() != Some(*bid)
                        || (positions
                            .statement
                            .get(node)
                            .copied()
                            .flatten()
                            .is_some_and(|index| index < statement_index)
                            && !positions.in_function.get(node).copied().unwrap_or(true))
                })
        })
}

fn safe_carrier_expression(
    ast: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    resolution: &Resolution,
    positions: &RootPositions,
    flow_transfer: bool,
    statement_index: usize,
    expression: NodeId,
) -> bool {
    let effect = analyzer.effects_for_expr(expression);
    !effect.calls
        && !effect.ordered
        && effect.writes.is_empty()
        && effect.stable
        && dependencies_are_single_write(
            analyzer,
            resolution,
            positions,
            flow_transfer,
            statement_index,
            expression,
        )
        && !is_atom(ast.node(expression))
        && !contains_fresh_reference(ast, expression)
}

fn collect_carriers(
    ast: &Ast,
    root: NodeId,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
) -> Vec<Carrier> {
    let Node::Block(statements) = ast.node(root) else {
        return Vec::new();
    };
    let positions = root_positions(ast, resolution, root);
    let flow_transfer = root_has_flow_transfer(ast, root);
    let mut carriers = Vec::new();
    for (statement_index, statement) in statements.iter().enumerate() {
        match ast.node(*statement) {
            Node::Local(_, expressions) => {
                let bids = resolution
                    .node_bids
                    .get(*statement as usize)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                if bids.len() != expressions.len() {
                    continue;
                }
                for (bid, expression) in bids.iter().copied().zip(expressions.iter().copied()) {
                    if resolution
                        .binding_write_counts
                        .get(bid as usize)
                        .copied()
                        .unwrap_or(0)
                        == 0
                        && safe_carrier_expression(
                            ast,
                            analyzer,
                            resolution,
                            &positions,
                            flow_transfer,
                            statement_index,
                            expression,
                        )
                    {
                        carriers.push(Carrier {
                            bid,
                            key: expression_key(ast, resolution, expression),
                            expression,
                            root_index: None,
                        });
                    }
                }
            }
            Node::Assign(targets, expressions) if targets.len() == expressions.len() => {
                for (target, expression) in targets.iter().copied().zip(expressions.iter().copied())
                {
                    if !matches!(ast.node(target), Node::Name(_)) {
                        continue;
                    }
                    let Some(bid) = resolution.node_bid.get(target as usize).copied().flatten()
                    else {
                        continue;
                    };
                    let binding = &resolution.bindings[bid as usize];
                    if binding.scope == 0
                        && resolution
                            .binding_write_counts
                            .get(bid as usize)
                            .copied()
                            .unwrap_or(0)
                            == 1
                        && safe_carrier_expression(
                            ast,
                            analyzer,
                            resolution,
                            &positions,
                            flow_transfer,
                            statement_index,
                            expression,
                        )
                    {
                        carriers.push(Carrier {
                            bid,
                            key: expression_key(ast, resolution, expression),
                            expression,
                            root_index: Some(statement_index),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    carriers
}

/// Time-invariance guard for member reads inside carrier expressions.
///
/// `safe_carrier_expression` judges a carrier by its binding-level effects:
/// an `Index` read records only the base binding's read and stays `stable`,
/// so a write to the object's fields (`t.x = 99`) is invisible to it. A
/// binding never being re-bound does not make the values read through it
/// constant, yet the replacement sites are function bodies that may run at
/// any later time. A member read is therefore reusable only when its value
/// provably cannot change after the carrier statement:
///
/// * the chain roots at a builtin API global that the chunk never mutates
///   (`mutated_builtin_roots`, which also covers alias writes and escapes
///   to unknown code) and never rebinds, or
/// * the chain roots at a local table built by a table constructor whose
///   binding is never re-bound, never written through any member target,
///   and never used as a whole value anywhere (no aliasing / escape).
///
/// Even for a closed root, a read that passes through an escaped chain
/// prefix (an intermediate table such as `t.x` that itself flowed into a
/// value position) is rejected: the escaped reference can be written
/// through under a different root binding, which the root-level scan
/// cannot attribute back to this root.
///
/// Anything else (user globals, locals not built by a constructor,
/// function bindings, roots that do not end at a plain name) is rejected.
struct MemberReadGuard {
    mutated_builtins: std::collections::HashSet<String>,
    closed_tables: std::collections::HashSet<BindingId>,
    escaped: EscapedChains,
}

/// How a name occurrence is used, for the closed-table scan below.
#[derive(Clone, Copy, PartialEq)]
enum UseCtx {
    /// Ordinary value position: the binding's value itself flows somewhere.
    Value,
    /// Object position of a member chain whose top is read.
    ChainRead,
    /// Object position of a member chain whose top is a write target.
    ChainWrite,
    /// Object position of a method name: the base is passed as `self`
    /// (an escape) and may be written through.
    MethodBase,
}

/// Leaf binding ids a member chain may read through: parentheses, nested
/// member accesses, and `and`/`or` value flow (either operand's value may
/// be the chain's base). Anything else yields no leaf and is treated as
/// unprovable by the caller.
fn chain_root_bids(ast: &Ast, resolution: &Resolution, node: NodeId, out: &mut Vec<BindingId>) {
    match ast.node(node) {
        Node::Name(_) => {
            if let Some(bid) = resolution.node_bid.get(node as usize).copied().flatten() {
                out.push(bid);
            }
        }
        Node::Paren(inner) | Node::Index(inner, _, _) | Node::Methodname(inner, _) => {
            chain_root_bids(ast, resolution, *inner, out)
        }
        Node::Bin(operator, left, right) if matches!(operator.as_str(), "and" | "or") => {
            chain_root_bids(ast, resolution, *left, out);
            chain_root_bids(ast, resolution, *right, out);
        }
        _ => {}
    }
}

/// Member chains whose value escapes the closed-table scan: when a chain
/// prefix such as `t.x` flows into a value position (a local, an
/// argument, a table field, a return), later writes through that escaped
/// reference are rooted at a different binding that this scan attributes
/// elsewhere, so reads passing through the escaped prefix can go stale
/// even though the root binding itself is never written through directly.
#[derive(Default)]
struct EscapedChains {
    /// Root binding -> escaped full chains (their step lists).
    by_root: std::collections::HashMap<BindingId, std::collections::HashSet<Vec<String>>>,
    /// Roots with an escape whose chain could not be decomposed; any
    /// member read through them is unprovable.
    whole_root: std::collections::HashSet<BindingId>,
}

fn record_chain_escape(
    ast: &Ast,
    resolution: &Resolution,
    node: NodeId,
    escaped: &mut EscapedChains,
) {
    match decompose_chain(ast, resolution, node) {
        Some((roots, steps)) => {
            if steps.is_empty() {
                return;
            }
            for root in roots {
                escaped
                    .by_root
                    .entry(root)
                    .or_default()
                    .insert(steps.clone());
            }
        }
        None => {
            let mut roots = Vec::new();
            chain_root_bids(ast, resolution, node, &mut roots);
            for root in roots {
                escaped.whole_root.insert(root);
            }
        }
    }
}

fn scan_member_uses(
    ast: &Ast,
    resolution: &Resolution,
    node: NodeId,
    ctx: UseCtx,
    write_roots: &mut std::collections::HashSet<BindingId>,
    value_uses: &mut std::collections::HashSet<BindingId>,
    escaped: &mut EscapedChains,
) {
    match ast.node(node) {
        Node::Name(_) => {
            if let Some(bid) = resolution.node_bid.get(node as usize).copied().flatten() {
                match ctx {
                    UseCtx::Value => {
                        value_uses.insert(bid);
                    }
                    UseCtx::ChainWrite => {
                        write_roots.insert(bid);
                    }
                    UseCtx::MethodBase => {
                        write_roots.insert(bid);
                        value_uses.insert(bid);
                    }
                    UseCtx::ChainRead => {}
                }
            }
        }
        Node::Paren(inner) => scan_member_uses(
            ast,
            resolution,
            *inner,
            ctx,
            write_roots,
            value_uses,
            escaped,
        ),
        Node::Bin(operator, left, right) if matches!(operator.as_str(), "and" | "or") => {
            scan_member_uses(
                ast,
                resolution,
                *left,
                ctx,
                write_roots,
                value_uses,
                escaped,
            );
            scan_member_uses(
                ast,
                resolution,
                *right,
                ctx,
                write_roots,
                value_uses,
                escaped,
            );
        }
        Node::Index(object, key, _) => {
            // In a value or method-base position this chain's value itself
            // flows onward, so the prefix ending at this node escapes.
            if matches!(ctx, UseCtx::Value | UseCtx::MethodBase) {
                record_chain_escape(ast, resolution, node, escaped);
            }
            let object_ctx = match ctx {
                UseCtx::Value => UseCtx::ChainRead,
                other => other,
            };
            scan_member_uses(
                ast,
                resolution,
                *object,
                object_ctx,
                write_roots,
                value_uses,
                escaped,
            );
            scan_member_uses(
                ast,
                resolution,
                *key,
                UseCtx::Value,
                write_roots,
                value_uses,
                escaped,
            );
        }
        Node::Methodname(object, _) => {
            scan_member_uses(
                ast,
                resolution,
                *object,
                UseCtx::MethodBase,
                write_roots,
                value_uses,
                escaped,
            );
        }
        Node::Assign(targets, expressions) => {
            for target in targets {
                // A plain name target is a rebinding, tracked by the
                // resolution's write counts; only member targets write
                // through a base binding.
                if !matches!(ast.node(*target), Node::Name(_)) {
                    scan_member_uses(
                        ast,
                        resolution,
                        *target,
                        UseCtx::ChainWrite,
                        write_roots,
                        value_uses,
                        escaped,
                    );
                }
            }
            for expression in expressions {
                scan_member_uses(
                    ast,
                    resolution,
                    *expression,
                    UseCtx::Value,
                    write_roots,
                    value_uses,
                    escaped,
                );
            }
        }
        Node::Funcstat(target, function) => {
            if !matches!(ast.node(*target), Node::Name(_)) {
                scan_member_uses(
                    ast,
                    resolution,
                    *target,
                    UseCtx::ChainWrite,
                    write_roots,
                    value_uses,
                    escaped,
                );
            }
            scan_member_uses(
                ast,
                resolution,
                *function,
                UseCtx::Value,
                write_roots,
                value_uses,
                escaped,
            );
        }
        _ => {
            storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
                scan_member_uses(
                    ast,
                    resolution,
                    child,
                    UseCtx::Value,
                    write_roots,
                    value_uses,
                    escaped,
                );
            });
        }
    }
}

impl MemberReadGuard {
    fn analyze(ast: &Ast, resolution: &Resolution, root: NodeId) -> Self {
        let mutated_builtins =
            storm_lua_analysis::effects::mutated_builtin_roots(ast, resolution, root);
        let mut write_roots = std::collections::HashSet::new();
        let mut value_uses = std::collections::HashSet::new();
        let mut escaped = EscapedChains::default();
        scan_member_uses(
            ast,
            resolution,
            root,
            UseCtx::Value,
            &mut write_roots,
            &mut value_uses,
            &mut escaped,
        );
        let mut closed_tables = std::collections::HashSet::new();
        for (bid, binding) in resolution.bindings.iter().enumerate() {
            let bid = bid as BindingId;
            if binding.kind != BindingKind::Local
                || resolution
                    .binding_write_counts
                    .get(bid as usize)
                    .copied()
                    .unwrap_or(0)
                    != 0
                || write_roots.contains(&bid)
                || value_uses.contains(&bid)
            {
                continue;
            }
            // The defining statement must initialize this binding with a
            // table constructor.
            let Some(decl) = binding.decl_node else {
                continue;
            };
            let Node::Local(_, expressions) = ast.node(decl) else {
                continue;
            };
            let Some(position) = resolution
                .node_bids
                .get(decl as usize)
                .and_then(|bids| bids.iter().position(|candidate| *candidate == bid))
            else {
                continue;
            };
            if expressions
                .get(position)
                .is_some_and(|expression| matches!(ast.node(*expression), Node::Table(_)))
            {
                closed_tables.insert(bid);
            }
        }
        Self {
            mutated_builtins,
            closed_tables,
            escaped,
        }
    }

    fn root_is_time_invariant(&self, ast: &Ast, resolution: &Resolution, bid: BindingId) -> bool {
        if resolution
            .binding_write_counts
            .get(bid as usize)
            .copied()
            .unwrap_or(0)
            != 0
        {
            return false;
        }
        let binding = &resolution.bindings[bid as usize];
        match binding.kind {
            BindingKind::Global => {
                let name = ast.strings.get(binding.name);
                storm_lua_analysis::resolver::api_roots(name)
                    && !self.mutated_builtins.contains(name)
            }
            BindingKind::Local => self.closed_tables.contains(&bid),
            _ => false,
        }
    }

    /// Every member read inside `expression` must be time-invariant.
    fn expression_is_time_invariant(
        &self,
        ast: &Ast,
        resolution: &Resolution,
        expression: NodeId,
    ) -> bool {
        let mut ok = true;
        storm_lua_syntax::ast_utils::walk(ast, expression, &mut |node| {
            if !ok {
                return;
            }
            if matches!(ast.node(node), Node::Index(..)) {
                let mut roots = Vec::new();
                chain_root_bids(ast, resolution, node, &mut roots);
                if roots.is_empty()
                    || !roots
                        .iter()
                        .all(|bid| self.root_is_time_invariant(ast, resolution, *bid))
                {
                    ok = false;
                    return;
                }
                // A read that passes through an escaped prefix can go
                // stale: the escaped reference may be written through
                // under a different root binding after the carrier was
                // defined. Reading the escaped value itself is fine (its
                // identity cannot change), so only proper prefixes block.
                if let Some((chain_roots, steps)) = decompose_chain(ast, resolution, node) {
                    for root in &chain_roots {
                        if self.escaped.whole_root.contains(root) {
                            ok = false;
                            return;
                        }
                        if let Some(escaped) = self.escaped.by_root.get(root) {
                            for len in 1..steps.len() {
                                if escaped.contains(&steps[..len]) {
                                    ok = false;
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        });
        ok
    }
}

fn expression_contains_index(ast: &Ast, expression: NodeId) -> bool {
    let mut found = false;
    storm_lua_syntax::ast_utils::walk(ast, expression, &mut |node| {
        if matches!(ast.node(node), Node::Index(..)) {
            found = true;
        }
    });
    found
}

/// Tick-scoped host-state observation (`input.*`) reachable from an
/// expression, used to vet carrier expressions.
///
/// Carriers are bound by chunk-level statements, which execute exactly once
/// when the chunk loads, while every replacement site sits inside a function
/// body that may run at any later time. `input.*` values are certified
/// stable only within a single tick, so a carrier whose evaluation observes
/// `input.*` would substitute the load-time snapshot for the fresh read at
/// every later call, across tick boundaries (candidate S8). `property.*`
/// values are certified stable for the whole execution and stay admissible,
/// as do deterministic expressions.
///
/// The observation may be direct — a call the effects analyzer resolves to
/// `input.*`, including through proven single-write aliases — or transitive,
/// through a call to a chunk function whose own body observes `input.*`.
/// Such a callee carries a pure summary under the pass's `stable_reads`
/// analyzer (host reads keep `stable` there by design), so the
/// binding-level gate in `safe_carrier_expression` admits it unaided; the
/// observing set is therefore computed here by a fixpoint over the chunk's
/// function-literal bindings.
struct TickScopedReads {
    observing: std::collections::HashSet<BindingId>,
}

/// Whether the subtree rooted at `node` evaluates a tick-scoped host read,
/// without descending into nested function bodies (their reads happen when
/// the nested function runs, not when the enclosing code evaluates). A call
/// to a function in `observing` counts, as does using one as a generic-for
/// iterator (the loop calls it synchronously).
fn subtree_observes_tick_scoped(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    observing: &std::collections::HashSet<BindingId>,
) -> bool {
    if matches!(ast.node(node), Node::Function(..)) {
        return false;
    }
    let mut found = false;
    match ast.node(node) {
        Node::Call(callee, _, _) => {
            if let Some(reference) = analyzer.resolve_builtin_reference(*callee) {
                found = reference.starts_with("input.");
            } else if matches!(ast.node(*callee), Node::Name(_)) {
                found = resolution
                    .node_bid
                    .get(*callee as usize)
                    .copied()
                    .flatten()
                    .is_some_and(|bid| observing.contains(&bid));
            }
        }
        Node::Forin(_, iterators, _) => {
            found = iterators.iter().any(|iterator| {
                matches!(ast.node(*iterator), Node::Name(_))
                    && resolution
                        .node_bid
                        .get(*iterator as usize)
                        .copied()
                        .flatten()
                        .is_some_and(|bid| observing.contains(&bid))
            });
        }
        _ => {}
    }
    if found {
        return true;
    }
    let mut observed = false;
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        if !observed {
            observed = subtree_observes_tick_scoped(ast, resolution, analyzer, child, observing);
        }
    });
    observed
}

impl TickScopedReads {
    fn analyze(
        ast: &Ast,
        resolution: &Resolution,
        analyzer: &EffectAnalyzer<'_>,
        root: NodeId,
    ) -> Self {
        // Enumerate the bindings a carrier could call by name: bindings
        // whose defining expression is a function literal.
        let mut bodies: Vec<(BindingId, NodeId)> = Vec::new();
        let mut nodes = Vec::new();
        storm_lua_syntax::ast_utils::walk(ast, root, &mut |node| nodes.push(node));
        for node in nodes {
            match ast.node(node) {
                Node::Local(_, expressions) => {
                    let bids = resolution
                        .node_bids
                        .get(node as usize)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    for (bid, expression) in bids.iter().copied().zip(expressions.iter().copied()) {
                        if let Node::Function(_, _, body) = ast.node(expression) {
                            bodies.push((bid, *body));
                        }
                    }
                }
                Node::Localfunc(_, function) => {
                    let bid = resolution
                        .node_bids
                        .get(node as usize)
                        .and_then(|bids| bids.first().copied())
                        .or_else(|| resolution.node_bid.get(node as usize).copied().flatten());
                    if let (Some(bid), Node::Function(_, _, body)) = (bid, ast.node(*function)) {
                        bodies.push((bid, *body));
                    }
                }
                Node::Funcstat(target, function) => {
                    if matches!(ast.node(*target), Node::Name(_)) {
                        let bid = resolution.node_bid.get(*target as usize).copied().flatten();
                        if let (Some(bid), Node::Function(_, _, body)) = (bid, ast.node(*function))
                        {
                            bodies.push((bid, *body));
                        }
                    }
                }
                Node::Assign(targets, expressions) if targets.len() == expressions.len() => {
                    for (target, expression) in
                        targets.iter().copied().zip(expressions.iter().copied())
                    {
                        if !matches!(ast.node(target), Node::Name(_)) {
                            continue;
                        }
                        let bid = resolution.node_bid.get(target as usize).copied().flatten();
                        if let (Some(bid), Node::Function(_, _, body)) = (bid, ast.node(expression))
                        {
                            bodies.push((bid, *body));
                        }
                    }
                }
                _ => {}
            }
        }
        let mut observing = std::collections::HashSet::new();
        loop {
            let mut grew = false;
            for (bid, body) in &bodies {
                if !observing.contains(bid)
                    && subtree_observes_tick_scoped(ast, resolution, analyzer, *body, &observing)
                {
                    observing.insert(*bid);
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        Self { observing }
    }

    fn expression_observes(
        &self,
        ast: &Ast,
        resolution: &Resolution,
        analyzer: &EffectAnalyzer<'_>,
        expression: NodeId,
    ) -> bool {
        subtree_observes_tick_scoped(ast, resolution, analyzer, expression, &self.observing)
    }
}

/// Identity of a function that replacement sites and load-time calls can
/// refer to: a name-bound function by its binding, or a field-bound
/// function (`function T.m()`, `T.m = function ...`) by its dotted chain.
#[derive(Clone, PartialEq, Eq, Hash)]
enum FnKey {
    Named(BindingId),
    Chain(String),
}

/// Function references found in a statement or function-body exterior
/// (function literals themselves are pruned: defining a function does
/// not execute it). `dynamic` records a call whose callee cannot be
/// resolved to a known chunk function or builtin/API reference; such a
/// call may invoke any function already bound at that point.
#[derive(Default, Clone)]
struct FnRefs {
    keys: std::collections::HashSet<FnKey>,
    dynamic: bool,
}

/// Candidate S9: a global carrier is bound by one root statement, but a
/// replacement site sits inside a function body. If that function can
/// execute at load BEFORE the binding statement runs — through a direct
/// or indirect load-time call — the site reads the carrier global while
/// it is still nil instead of evaluating the expression. The guard
/// computes, per carrier binding index, the set of functions that may
/// run during the load prefix up to and including the binding statement,
/// so the replacer can suppress sites inside them. Reachability is
/// over-approximated: a function counts as reachable when it is
/// referenced (called, or passed as a value) from the prefix or from a
/// reachable function's body; a dynamic call makes every function bound
/// up to the carrier statement reachable.
struct LoadOrderGuard {
    /// Function literal node -> its site scope, for name- and field-bound
    /// functions (unrecorded literals are unnamed; see SiteScope).
    fn_scope: std::collections::HashMap<NodeId, SiteScope>,
    named_root: std::collections::HashMap<BindingId, usize>,
    chain_root: std::collections::HashMap<String, usize>,
    stmt_refs: Vec<FnRefs>,
    body_refs: std::collections::HashMap<FnKey, FnRefs>,
}

/// Dotted chain key for a member target or callee (`T`, `T.m`,
/// `T.m.n`), or `None` when any step is computed.
fn chain_key(ast: &Ast, node: NodeId) -> Option<String> {
    match ast.node(node) {
        Node::Name(symbol) => Some(ast.strings.get(*symbol).to_string()),
        Node::Paren(inner) => chain_key(ast, *inner),
        Node::Index(object, key, _) => {
            let Node::Str(field) = ast.node(*key) else {
                return None;
            };
            Some(format!("{}.{}", chain_key(ast, *object)?, field.as_ref()))
        }
        Node::Methodname(object, name) => Some(format!(
            "{}.{}",
            chain_key(ast, *object)?,
            ast.strings.get(*name)
        )),
        _ => None,
    }
}

fn unwrap_paren(ast: &Ast, mut node: NodeId) -> NodeId {
    while let Node::Paren(inner) = ast.node(node) {
        node = *inner;
    }
    node
}

#[allow(clippy::too_many_arguments)]
fn classify_callee(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    named: &std::collections::HashSet<BindingId>,
    chains: &std::collections::HashSet<String>,
    callee: NodeId,
    refs: &mut FnRefs,
) {
    let callee = unwrap_paren(ast, callee);
    match ast.node(callee) {
        Node::Name(_) => {
            if let Some(bid) = resolution.node_bid.get(callee as usize).copied().flatten() {
                if named.contains(&bid) {
                    refs.keys.insert(FnKey::Named(bid));
                } else if analyzer.resolve_builtin_reference(callee).is_none() {
                    // A local/parameter of unknown value (e.g. an alias
                    // for a chunk function): the call is dynamic.
                    refs.dynamic = true;
                }
            }
            // An unresolved global callee is not a chunk function.
        }
        Node::Index(..) | Node::Methodname(..) => {
            if let Some(key) = chain_key(ast, callee) {
                if chains.contains(&key) {
                    refs.keys.insert(FnKey::Chain(key));
                    return;
                }
            }
            if analyzer.resolve_builtin_reference(callee).is_none() {
                refs.dynamic = true;
            }
        }
        Node::Function(..) => {}
        _ => {
            refs.dynamic = true;
        }
    }
}

/// Collect the function references in the exterior of `node` (a root
/// statement or a function body). Function literals are pruned, except
/// an immediately-invoked literal, whose body executes inline and is
/// scanned as part of the exterior.
#[allow(clippy::too_many_arguments)]
fn scan_refs(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    named: &std::collections::HashSet<BindingId>,
    chains: &std::collections::HashSet<String>,
    node: NodeId,
    refs: &mut FnRefs,
) {
    match ast.node(node) {
        Node::Function(..) => return,
        // A binding target's own name is a declaration, not a
        // reference: only the target's subexpressions (the base chain
        // and key, which are evaluated) and the assigned values scan.
        Node::Assign(targets, expressions) => {
            for target in targets {
                storm_lua_analysis::resolver::for_each_child(ast, *target, &mut |child| {
                    scan_refs(ast, resolution, analyzer, named, chains, child, refs);
                });
            }
            for expression in expressions {
                scan_refs(ast, resolution, analyzer, named, chains, *expression, refs);
            }
            return;
        }
        Node::Funcstat(target, _) => {
            storm_lua_analysis::resolver::for_each_child(ast, *target, &mut |child| {
                scan_refs(ast, resolution, analyzer, named, chains, child, refs);
            });
            return;
        }
        Node::Call(callee, _, _) => {
            if let Node::Name(symbol) = ast.node(unwrap_paren(ast, *callee)) {
                if ast.strings.get(*symbol) == "setmetatable" {
                    // Installing metamethods lets later operations in the
                    // prefix invoke chunk functions implicitly.
                    refs.dynamic = true;
                }
            }
            if let Node::Function(_, _, body) = ast.node(unwrap_paren(ast, *callee)) {
                scan_refs(ast, resolution, analyzer, named, chains, *body, refs);
            } else {
                classify_callee(ast, resolution, analyzer, named, chains, *callee, refs);
            }
        }
        Node::Forin(_, iterators, _) => {
            for iterator in iterators {
                if !matches!(ast.node(*iterator), Node::Call(..)) {
                    classify_callee(ast, resolution, analyzer, named, chains, *iterator, refs);
                }
            }
        }
        Node::Name(_) => {
            if let Some(bid) = resolution.node_bid.get(node as usize).copied().flatten() {
                if named.contains(&bid) {
                    refs.keys.insert(FnKey::Named(bid));
                }
            }
        }
        _ => {}
    }
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        scan_refs(ast, resolution, analyzer, named, chains, child, refs);
    });
}

impl LoadOrderGuard {
    fn analyze(
        ast: &Ast,
        resolution: &Resolution,
        analyzer: &EffectAnalyzer<'_>,
        root: NodeId,
    ) -> Self {
        let mut fn_scope: std::collections::HashMap<NodeId, SiteScope> =
            std::collections::HashMap::new();
        let mut named_root: std::collections::HashMap<BindingId, usize> =
            std::collections::HashMap::new();
        let mut chain_root: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut bodies: Vec<(FnKey, NodeId)> = Vec::new();

        #[allow(clippy::too_many_arguments)]
        fn record(
            fn_scope: &mut std::collections::HashMap<NodeId, SiteScope>,
            named_root: &mut std::collections::HashMap<BindingId, usize>,
            chain_root: &mut std::collections::HashMap<String, usize>,
            bodies: &mut Vec<(FnKey, NodeId)>,
            function: NodeId,
            key: FnKey,
            body: NodeId,
            root_index: usize,
        ) {
            match &key {
                FnKey::Named(bid) => {
                    fn_scope.insert(function, SiteScope::Named(*bid));
                    named_root
                        .entry(*bid)
                        .and_modify(|idx| *idx = (*idx).min(root_index))
                        .or_insert(root_index);
                }
                FnKey::Chain(name) => {
                    fn_scope.insert(function, SiteScope::Chain(name.clone()));
                    chain_root
                        .entry(name.clone())
                        .and_modify(|idx| *idx = (*idx).min(root_index))
                        .or_insert(root_index);
                }
            }
            bodies.push((key, body));
        }

        #[allow(clippy::too_many_arguments)]
        fn walk_bindings(
            ast: &Ast,
            resolution: &Resolution,
            node: NodeId,
            root_index: usize,
            fn_scope: &mut std::collections::HashMap<NodeId, SiteScope>,
            named_root: &mut std::collections::HashMap<BindingId, usize>,
            chain_root: &mut std::collections::HashMap<String, usize>,
            bodies: &mut Vec<(FnKey, NodeId)>,
        ) {
            match ast.node(node) {
                Node::Local(_, expressions) => {
                    let bids = resolution
                        .node_bids
                        .get(node as usize)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    for (bid, expression) in bids.iter().copied().zip(expressions.iter().copied()) {
                        if let Node::Function(_, _, body) = ast.node(expression) {
                            record(
                                fn_scope,
                                named_root,
                                chain_root,
                                bodies,
                                expression,
                                FnKey::Named(bid),
                                *body,
                                root_index,
                            );
                        }
                    }
                }
                Node::Localfunc(_, function) => {
                    let bid = resolution
                        .node_bids
                        .get(node as usize)
                        .and_then(|bids| bids.first().copied())
                        .or_else(|| resolution.node_bid.get(node as usize).copied().flatten());
                    if let (Some(bid), Node::Function(_, _, body)) = (bid, ast.node(*function)) {
                        record(
                            fn_scope,
                            named_root,
                            chain_root,
                            bodies,
                            *function,
                            FnKey::Named(bid),
                            *body,
                            root_index,
                        );
                    }
                }
                Node::Funcstat(target, function) => {
                    if let Node::Function(_, _, body) = ast.node(*function) {
                        let key = if matches!(ast.node(*target), Node::Name(_)) {
                            resolution
                                .node_bid
                                .get(*target as usize)
                                .copied()
                                .flatten()
                                .map(FnKey::Named)
                        } else {
                            chain_key(ast, *target).map(FnKey::Chain)
                        };
                        if let Some(key) = key {
                            record(
                                fn_scope, named_root, chain_root, bodies, *function, key, *body,
                                root_index,
                            );
                        }
                    }
                }
                Node::Assign(targets, expressions) if targets.len() == expressions.len() => {
                    for (target, expression) in
                        targets.iter().copied().zip(expressions.iter().copied())
                    {
                        if let Node::Function(_, _, body) = ast.node(expression) {
                            let key = if matches!(ast.node(target), Node::Name(_)) {
                                resolution
                                    .node_bid
                                    .get(target as usize)
                                    .copied()
                                    .flatten()
                                    .map(FnKey::Named)
                            } else {
                                chain_key(ast, target).map(FnKey::Chain)
                            };
                            if let Some(key) = key {
                                record(
                                    fn_scope, named_root, chain_root, bodies, expression, key,
                                    *body, root_index,
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
            storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
                walk_bindings(
                    ast, resolution, child, root_index, fn_scope, named_root, chain_root, bodies,
                );
            });
        }

        let mut stmt_nodes: Vec<NodeId> = Vec::new();
        if let Node::Block(statements) = ast.node(root) {
            stmt_nodes = statements.clone();
        }
        for (index, statement) in stmt_nodes.iter().enumerate() {
            walk_bindings(
                ast,
                resolution,
                *statement,
                index,
                &mut fn_scope,
                &mut named_root,
                &mut chain_root,
                &mut bodies,
            );
        }

        let named: std::collections::HashSet<BindingId> = named_root.keys().copied().collect();
        let chains: std::collections::HashSet<String> = chain_root.keys().cloned().collect();
        let stmt_refs = stmt_nodes
            .iter()
            .map(|statement| {
                let mut refs = FnRefs::default();
                scan_refs(
                    ast, resolution, analyzer, &named, &chains, *statement, &mut refs,
                );
                refs
            })
            .collect();
        let mut body_refs: std::collections::HashMap<FnKey, FnRefs> =
            std::collections::HashMap::new();
        for (key, body) in &bodies {
            let entry = body_refs.entry(key.clone()).or_default();
            scan_refs(ast, resolution, analyzer, &named, &chains, *body, entry);
        }
        Self {
            fn_scope,
            named_root,
            chain_root,
            stmt_refs,
            body_refs,
        }
    }

    /// The site filter for a global carrier bound by root statement
    /// `carrier_index`: functions that may execute during the load
    /// prefix up to and including that statement.
    fn filter_for(&self, carrier_index: usize) -> SiteFilter<'_> {
        let mut blocked: std::collections::HashSet<FnKey> = std::collections::HashSet::new();
        let mut queue: Vec<FnKey> = Vec::new();
        let mut dynamic = false;
        let mut expanded = false;
        for refs in self.stmt_refs.iter().take(carrier_index + 1) {
            dynamic |= refs.dynamic;
            for key in &refs.keys {
                if blocked.insert(key.clone()) {
                    queue.push(key.clone());
                }
            }
        }
        loop {
            if dynamic && !expanded {
                expanded = true;
                for (bid, index) in &self.named_root {
                    if *index <= carrier_index && blocked.insert(FnKey::Named(*bid)) {
                        queue.push(FnKey::Named(*bid));
                    }
                }
                for (name, index) in &self.chain_root {
                    if *index <= carrier_index && blocked.insert(FnKey::Chain(name.clone())) {
                        queue.push(FnKey::Chain(name.clone()));
                    }
                }
            }
            let Some(key) = queue.pop() else {
                if dynamic && !expanded {
                    continue;
                }
                break;
            };
            if let Some(refs) = self.body_refs.get(&key) {
                dynamic |= refs.dynamic;
                for next in &refs.keys {
                    if blocked.insert(next.clone()) {
                        queue.push(next.clone());
                    }
                }
            }
        }
        SiteFilter {
            fn_scope: &self.fn_scope,
            blocked_keys: blocked,
            carrier_index,
        }
    }
}

/// Where a replacement site sits relative to function boundaries, for
/// the load-order guard. Only the outermost enclosing function matters:
/// a nested function can run only when its outermost function runs (or
/// after escaping from it, which likewise requires the outer function
/// to have run first).
#[derive(Clone, PartialEq)]
enum SiteScope {
    Root,
    Named(BindingId),
    Chain(String),
    /// The outermost function has no resolvable identity (a literal
    /// passed as a value, or a function bound through a computed
    /// target); it is identified by its containing root statement.
    Unnamed(usize),
}

struct SiteFilter<'a> {
    fn_scope: &'a std::collections::HashMap<NodeId, SiteScope>,
    blocked_keys: std::collections::HashSet<FnKey>,
    carrier_index: usize,
}

impl SiteFilter<'_> {
    fn allows(&self, scope: &SiteScope) -> bool {
        match scope {
            SiteScope::Root => false,
            SiteScope::Named(bid) => !self.blocked_keys.contains(&FnKey::Named(*bid)),
            SiteScope::Chain(name) => !self.blocked_keys.contains(&FnKey::Chain(name.clone())),
            // An unnamed function whose containing statement is part of
            // the load prefix may execute during it (immediately
            // invoked, or stored and reached dynamically).
            SiteScope::Unnamed(root_index) => *root_index > self.carrier_index,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_replacements(
    ast: &Ast,
    resolution: &Resolution,
    node: NodeId,
    scope: &SiteScope,
    root_stmt: usize,
    filter: Option<&SiteFilter<'_>>,
    key: &str,
    output: &mut Vec<NodeId>,
) {
    // An assignment or function-statement target must never itself be
    // replaced: the target node can be syntactically equal to the carrier
    // expression (a member chain such as `t.x`), and rewriting it would
    // redirect the write onto the carrier binding. The target's
    // subexpressions (the base chain and the key) are ordinary value
    // positions — replacing a carrier read inside them evaluates the same
    // value at the same point — so only the target node itself is
    // skipped, by recursing into its children directly.
    match ast.node(node) {
        Node::Assign(targets, expressions) => {
            for target in targets {
                storm_lua_analysis::resolver::for_each_child(ast, *target, &mut |child| {
                    collect_replacements(
                        ast, resolution, child, scope, root_stmt, filter, key, output,
                    );
                });
            }
            for expression in expressions {
                collect_replacements(
                    ast,
                    resolution,
                    *expression,
                    scope,
                    root_stmt,
                    filter,
                    key,
                    output,
                );
            }
            return;
        }
        Node::Funcstat(target, function) => {
            storm_lua_analysis::resolver::for_each_child(ast, *target, &mut |child| {
                collect_replacements(
                    ast, resolution, child, scope, root_stmt, filter, key, output,
                );
            });
            collect_replacements(
                ast, resolution, *function, scope, root_stmt, filter, key, output,
            );
            return;
        }
        _ => {}
    }
    let in_function = !matches!(scope, SiteScope::Root);
    if in_function
        && is_expression_type(ast.node(node))
        && expression_key(ast, resolution, node) == key
        && filter.is_none_or(|site_filter| site_filter.allows(scope))
    {
        output.push(node);
        return;
    }
    // Entering the outermost function fixes the site's scope: the
    // binding identity recorded by the load-order guard, or the
    // containing root statement for an unnamed literal.
    let entered;
    let child_scope = if !in_function && matches!(ast.node(node), Node::Function(..)) {
        entered = match filter.and_then(|site_filter| site_filter.fn_scope.get(&node)) {
            Some(found) => found.clone(),
            None => SiteScope::Unnamed(root_stmt),
        };
        &entered
    } else {
        scope
    };
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        collect_replacements(
            ast,
            resolution,
            child,
            child_scope,
            root_stmt,
            filter,
            key,
            output,
        );
    });
}

fn candidate_for_carrier(
    source: &Ast,
    root: NodeId,
    resolution: &Resolution,
    carrier: &Carrier,
    filter: Option<&SiteFilter<'_>>,
) -> Option<(Ast, usize)> {
    let mut replacements = Vec::new();
    match source.node(root) {
        Node::Block(statements) => {
            for (index, statement) in statements.iter().enumerate() {
                collect_replacements(
                    source,
                    resolution,
                    *statement,
                    &SiteScope::Root,
                    index,
                    filter,
                    &carrier.key,
                    &mut replacements,
                );
            }
        }
        _ => {
            collect_replacements(
                source,
                resolution,
                root,
                &SiteScope::Root,
                0,
                filter,
                &carrier.key,
                &mut replacements,
            );
        }
    }
    if replacements.is_empty() {
        return None;
    }
    let mut candidate = clone_ast(source);
    let carrier_symbol = resolution.bindings[carrier.bid as usize].name;
    for replacement in &replacements {
        candidate.nodes.rewrite(
            *replacement,
            Node::Name(carrier_symbol),
            "immutable-global-expression-reuse",
        );
        super::origins::derive(
            &mut candidate,
            *replacement,
            source,
            &[*replacement],
            "immutable-global-expression-reuse",
        );
        candidate.nodes.relate_from(
            *replacement,
            &source.nodes,
            carrier.expression,
            "immutable-value-definition",
        );
    }
    // Resolve at the insertion sites, where a local with the same spelling can
    // shadow the carrier. Binding IDs can be renumbered by eliminated globals;
    // compare the declaration identity rather than raw IDs across resolutions.
    let resolved = resolve(&candidate, root);
    let expected = resolution.binding(carrier.bid);
    if replacements.iter().any(|node| {
        resolved.node_bid[*node as usize].is_none_or(|bid| {
            let actual = resolved.binding(bid);
            actual.name != expected.name
                || actual.kind != expected.kind
                || actual.scope != expected.scope
                || actual.decl_node != expected.decl_node
        })
    }) {
        return None;
    }
    Some((candidate, replacements.len()))
}

pub fn reuse_immutable_values_with_options(
    ast: &mut Ast,
    root: NodeId,
    max_rounds: usize,
) -> PassResult {
    // Cheap necessary-condition gate: when no top-level expression can repeat
    // inside a function body, the pass provably fires nothing, so skip the
    // clone/resolve/analyzer/measure round entirely. The skipped round would
    // have returned saved=0 with the AST unchanged, which this return matches.
    if !reuse_gate_possible(ast, root) {
        return PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["reused=0".to_string()]),
        };
    }
    let original_size = measured(ast, root);
    let mut reused = 0usize;

    for _ in 0..max_rounds {
        let source = clone_ast(ast);
        let resolution = resolve(&source, root);
        let analyzer = EffectAnalyzer::new(&source, &resolution, root, true);
        let carriers = collect_carriers(&source, root, &resolution, &analyzer);
        // A carrier is bound once, when the chunk loads, but replacement
        // sites run at arbitrary later times: drop carriers whose
        // evaluation observes tick-scoped host state (`input.*`), whose
        // certified stability does not reach across tick boundaries
        // (see TickScopedReads).
        let carriers = if carriers.is_empty() {
            carriers
        } else {
            let tick_scoped = TickScopedReads::analyze(&source, &resolution, &analyzer, root);
            carriers
                .into_iter()
                .filter(|carrier| {
                    !tick_scoped.expression_observes(
                        &source,
                        &resolution,
                        &analyzer,
                        carrier.expression,
                    )
                })
                .collect()
        };
        // Member reads need a stronger guarantee than the binding-level
        // effects provide (see MemberReadGuard): drop carriers whose
        // expression reads members through a base that may change value.
        let carriers = if carriers
            .iter()
            .any(|carrier| expression_contains_index(&source, carrier.expression))
        {
            let guard = MemberReadGuard::analyze(&source, &resolution, root);
            carriers
                .into_iter()
                .filter(|carrier| {
                    !expression_contains_index(&source, carrier.expression)
                        || guard.expression_is_time_invariant(
                            &source,
                            &resolution,
                            carrier.expression,
                        )
                })
                .collect()
        } else {
            carriers
        };
        // Load order (candidate S9): a global carrier is bound by one
        // root statement, but a function holding a replacement site may
        // execute at load before that statement runs, reading the
        // carrier while it is still nil. Suppress sites inside any
        // function the guard proves load-reachable (see LoadOrderGuard).
        let load_order = if carriers.iter().any(|carrier| carrier.root_index.is_some()) {
            Some(LoadOrderGuard::analyze(
                &source,
                &resolution,
                &analyzer,
                root,
            ))
        } else {
            None
        };
        let baseline = measured(&source, root);
        let mut best: Option<(Ast, usize, usize)> = None;

        for carrier in carriers {
            let filter = match (&load_order, carrier.root_index) {
                (Some(guard), Some(index)) => Some(guard.filter_for(index)),
                _ => None,
            };
            let Some((candidate, replacements)) =
                candidate_for_carrier(&source, root, &resolution, &carrier, filter.as_ref())
            else {
                continue;
            };
            let size = measured(&candidate, root);
            if size < baseline && best.as_ref().is_none_or(|entry| size < entry.1) {
                best = Some((candidate, size, replacements));
            }
        }

        let Some((candidate, _, replacements)) = best else {
            break;
        };
        *ast = candidate;
        reused += replacements;
    }

    PassResult {
        root,
        saved: Some(original_size.saturating_sub(measured(ast, root)) as u64),
        details: Some(vec![format!("reused={reused}")]),
    }
}

pub fn reuse_immutable_values(ast: &mut Ast, root: NodeId) -> PassResult {
    reuse_immutable_values_with_options(ast, root, 16)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = reuse_immutable_values(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn expression_key_hash_matches_materialized_key_bytes() {
        // The hash walker must emit exactly the byte stream the key
        // walker appends, for every node kind; otherwise a candidate the
        // string key would find could land in a different bucket and a
        // reuse would be silently missed (an output change).
        let sources = [
            "local t={1,2.5,\"s\",true,nil,x=1,[2]=\"k\",[u]=6} local u=-t[1]+t.x*(t[2]-1)^2 local v=t:m(1,2) local w=t.m(t,u) if u>0 and not v then u=1 elseif u<0 then u=2 else u=3 end while u>10 do u=u-1 end repeat u=u+1 until u>3 for i=1,10,2 do u=u+i end for j=1,3 do u=u+j end for k2,v2 in pairs(t) do u=u+1 end do local z=u end",
            "local function f(a1,b1) return a1+b1,a1 end local function g(...) return ... end function h.q(a1) return a1 end for i=1,3 do if i==2 then break end end local m1,m2=f(1,2),g(3) m1=m2 f(1,2) local s=\"a\"..\"b\"==\"ab\" or false local n=#t2",
            "goto done local x=1 ::done::",
            "local a=0x10 local b=1e3 local c=\"a\\nb\" local d='q' local e=[[zz]] local f=a+b local g=f//2 local h=f<<1 local i=f&3 local j=~f local k=f~=g local l=not a",
        ];
        for source in sources {
            let (ast, root) = parse_source(source).expect("parse");
            let resolution = resolve(&ast, root);
            for id in 0..ast.nodes.len() {
                let node = id as NodeId;
                let key = expression_key(&ast, &resolution, node);
                assert_eq!(
                    expression_key_hash(&ast, &resolution, node),
                    key_bytes_hash(&key),
                    "hash/key stream mismatch at node {node} of {source}"
                );
            }
        }
    }

    #[test]
    fn expression_key_hash_agrees_on_equal_keys() {
        // Repeated subexpressions must share a hash (the lookup side of
        // the contract), including when the same binding is read through
        // different spellings of one member step.
        let source = "local t={x=3} local a=t.x+t[\"x\"]+math.max(1,2) local b=t.x+math.max(1,2)";
        let (ast, root) = parse_source(source).expect("parse");
        let resolution = resolve(&ast, root);
        let mut by_key: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for id in 0..ast.nodes.len() {
            let node = id as NodeId;
            let key = expression_key(&ast, &resolution, node);
            let hash = expression_key_hash(&ast, &resolution, node);
            if let Some(previous) = by_key.insert(key.clone(), hash) {
                assert_eq!(previous, hash, "equal keys hashed differently: {key}");
            }
        }
    }

    #[test]
    fn reuses_stable_top_level_expression_in_function() {
        let source = "local tau=math.pi*2 function onTick()output.setNumber(1,tau)output.setNumber(2,math.pi*2)output.setNumber(3,math.pi*2)end";
        let result = output(source);
        assert_eq!(
            result,
            "local tau=math.pi*2 function onTick()output.setNumber(1,tau)output.setNumber(2,tau)output.setNumber(3,tau)end"
        );
    }

    #[test]
    fn preserves_fresh_table_identity() {
        let source = "local template={x=1} function make()return {x=1}end function onTick()local a=make()local b=make()output.setBool(1,a~=b)end";
        assert_eq!(
            output(source),
            "local template={x=1}function make()return{x=1}end function onTick()local a,b=make(),make()output.setBool(1,a~=b)end"
        );
    }

    #[test]
    fn respects_shadowed_bindings() {
        let source =
            "a=1 b=2 carrier=a+b function onTick()local a=3 local b=4 output.setNumber(1,a+b)end";
        assert_eq!(
            output(source),
            "a=1 b=2 carrier=a+b function onTick()local a,b=3,4 output.setNumber(1,a+b)end"
        );
    }

    #[test]
    fn gate_skips_when_carrier_never_repeats() {
        // The carrier `math.pi*2` never repeats inside a function body, so the
        // syntactic gate skips the pass and the code is unchanged.
        let source = "local tau=math.pi*2 function onTick()output.setNumber(1,1)end";
        assert_eq!(output(source), source);
    }

    #[test]
    fn does_not_reuse_member_read_across_member_write() {
        // The write `t.x=99` executes after the function is defined but
        // before it is called, so the read inside the body must not be
        // replaced by the earlier carrier value.
        let source = "local t={x=3}local a,f=t.x,function()return t.x end t.x=99 return a+f()";
        assert_eq!(output(source), source);
    }

    #[test]
    fn reuses_closed_table_member_read() {
        // No write or escape anywhere: the table is closed, so the member
        // read is time-invariant and reuse still fires.
        let source = "local t={x=3} local a=t.x local f=function() return t.x end return a+f()";
        assert_eq!(
            output(source),
            "local t={x=3}local a=t.x local f=function()return a end return a+f()"
        );
    }

    #[test]
    fn does_not_reuse_member_read_when_table_escapes() {
        let source = "local t={x=3}local a,f=t.x,function()return t.x end sink(t)return a+f()";
        assert_eq!(output(source), source);
    }

    #[test]
    fn does_not_reuse_member_read_across_write_inside_function() {
        let source = "local t={x=3}local a,f,g=t.x,function()return t.x end,function()t.x=99 end g()return a+f()";
        assert_eq!(output(source), source);
    }

    #[test]
    fn does_not_reuse_member_read_across_alias_write() {
        let source = "local t={x=3}local u,a,f=t,t.x,function()return t.x end u.x=99 return a+f()";
        assert_eq!(output(source), source);
    }

    #[test]
    fn does_not_reuse_member_read_of_nonconstructor_local() {
        let source = "local t=make()local a,f=t.x,function()return t.x end return a+f()";
        assert_eq!(output(source), source);
    }

    #[test]
    fn does_not_reuse_builtin_member_read_when_root_mutated() {
        let source =
            "local a=math.pi*2 math.pi=3 local f=function()return math.pi*2 end return a+f()";
        assert_eq!(output(source), source);
    }

    #[test]
    fn does_not_reuse_member_read_when_method_defined_on_table() {
        let source = "local t={x=3}local a=t.x function t:m()return 1 end local f=function()return t.x end return a+f()";
        assert_eq!(output(source), source);
    }

    #[test]
    fn ignores_atomic_carriers() {
        let source = "local one=1 function onTick()output.setNumber(1,1)end";
        assert_eq!(output(source), source);
    }

    #[test]
    fn does_not_reuse_when_dependency_written_after_carrier() {
        // The single write to `x` sits in a later top-level statement, so
        // the expression's value inside the function (called after the
        // write) differs from the carrier's definition-time value.
        let source = "local x=1 local a=x+1 x=2 local f=function()return x+1 end return a+f()";
        assert_eq!(output(source), source);
    }

    #[test]
    fn does_not_reuse_when_dependency_written_inside_function() {
        // The single write to `x` is deferred inside a function body and
        // can execute after the carrier statement.
        let source = "local x=1 local a=x+1 local f=function()return x+1 end local g=function()x=2 end g()return f()";
        assert_eq!(
            output(source),
            "local x=1 local a,f,g=x+1,function()return x+1 end,function()x=2 end g()return f()"
        );
    }

    #[test]
    fn does_not_reuse_when_dependency_written_by_carrier_statement() {
        // The write to `y` is a target of the carrier statement itself and
        // executes after the right-hand side was evaluated.
        let source = "local y=1 local a,y=y+1,2 local f=function()return y+1 end return a+f()";
        assert_eq!(output(source), source);
    }

    #[test]
    fn reuses_when_dependency_written_before_carrier() {
        // The single write to `x` completes in an earlier top-level
        // statement, so the expression's value is fixed from the carrier
        // statement onward.
        let source = "local x=1 x=2 local a=x+1 local f=function()return x+1 end return f()";
        assert_eq!(
            output(source),
            "local x=1 x=2 local a=x+1 local f=function()return a end return f()"
        );
    }

    #[test]
    fn does_not_reuse_member_read_through_escaped_prefix() {
        // `t.x` escapes into `inner`; the write through `inner` is rooted
        // at a different binding, so the root scan cannot see it, yet the
        // read of `t.x.y` inside the function would go stale.
        let source = "local t={x={y=3}} local inner=t.x local a=t.x.y local f=function()return t.x.y end inner.y=99 return a+f()";
        // The stale value carrier `a` must not be used; reading through
        // the escaped alias `inner` itself (or not replacing at all) is
        // sound because the alias permanently denotes the same table.
        let result = output(source);
        assert!(result.contains("return inner.y") || result.contains("return t.x.y"));
    }

    #[test]
    fn does_not_reuse_member_read_through_argument_escaped_prefix() {
        // `t.x` escapes as a call argument; the callee writes through its
        // parameter, which the root scan attributes to the parameter.
        let source = "local t={x={y=3}} local a=t.x.y local f=function()return t.x.y end local g=function(v)v.y=99 end g(t.x)return a+f()";
        assert!(output(source).contains("return t.x.y"));
    }

    #[test]
    fn reuses_member_read_of_escaped_value_itself() {
        // Reading the escaped value itself stays sound: member writes
        // cannot change which table `t.x` denotes, only its fields.
        let source = "local t={x={y=3}} local inner=t.x local a=t.x local f=function()return t.x end return f()";
        let result = output(source);
        assert!(result.contains("return a end") || result.contains("return inner end"));
    }

    #[test]
    fn replacements_never_include_assignment_targets() {
        let source = "local t={x=1} local a=t.x local f=function()t.x=99 return t.x end return f()";
        let (ast, root) = parse_source(source).expect("parse");
        let resolution = resolve(&ast, root);
        let Node::Block(statements) = ast.node(root) else {
            panic!("root block");
        };
        let Node::Local(_, expressions) = ast.node(statements[1]) else {
            panic!("carrier declaration");
        };
        let key = expression_key(&ast, &resolution, expressions[0]);
        let mut replacements = Vec::new();
        collect_replacements(
            &ast,
            &resolution,
            root,
            &SiteScope::Root,
            0,
            None,
            &key,
            &mut replacements,
        );
        // Only the read in the `return` may be replaced, never the
        // assignment target `t.x=99`.
        assert_eq!(replacements.len(), 1);
    }

    #[test]
    fn replacements_never_include_funcstat_targets() {
        let source = "local t={x=1} local a=t.x local f=function()return t.x end function t.x()return 1 end return f()";
        let (ast, root) = parse_source(source).expect("parse");
        let resolution = resolve(&ast, root);
        let Node::Block(statements) = ast.node(root) else {
            panic!("root block");
        };
        let Node::Local(_, expressions) = ast.node(statements[1]) else {
            panic!("carrier declaration");
        };
        let key = expression_key(&ast, &resolution, expressions[0]);
        let mut replacements = Vec::new();
        collect_replacements(
            &ast,
            &resolution,
            root,
            &SiteScope::Root,
            0,
            None,
            &key,
            &mut replacements,
        );
        // Only the read inside `f` may be replaced, never the function
        // statement's target `t.x`.
        assert_eq!(replacements.len(), 1);
    }

    #[test]
    fn does_not_reuse_input_read_across_tick_boundary() {
        // Candidate S8: the carrier is bound once at chunk load, but the
        // replacement site runs on every tick, where the certified
        // per-tick stability of `input.*` no longer covers the snapshot.
        let source =
            "local a=input.getNumber(1) function onTick()output.setNumber(1,input.getNumber(1))end";
        assert!(output(source).contains("output.setNumber(1,input.getNumber(1))"));
    }

    #[test]
    fn does_not_reuse_input_read_through_global_carrier() {
        // The w6 cycle-74 minimal shape (s8-min): a compound carrier
        // anchored on a chunk-level global.
        let source =
            "G1=input.getNumber(3)+5 function onTick()output.setNumber(2,input.getNumber(3)+5)end";
        assert!(output(source).contains("output.setNumber(2,input.getNumber(3)+5)"));
    }

    #[test]
    fn does_not_reuse_compound_input_read() {
        let source = "local a=math.floor(input.getNumber(3)) function f()return math.floor(input.getNumber(3))end";
        assert!(output(source).contains("return math.floor(input.getNumber(3))"));
    }

    #[test]
    fn does_not_reuse_input_getbool() {
        let source =
            "local a=input.getBool(2) function onTick()output.setBool(1,input.getBool(2))end";
        assert!(output(source).contains("output.setBool(1,input.getBool(2))"));
    }

    #[test]
    fn does_not_reuse_input_read_through_chunk_function() {
        // The callee's summary is pure under the pass's stable_reads
        // analyzer, so the observation is transitive and needs the
        // fixpoint, not just the direct-call check.
        let source = "local function rd()return input.getNumber(3)end local a=rd() function onTick()output.setNumber(1,rd())end";
        assert!(output(source).contains("output.setNumber(1,rd())"));
    }

    #[test]
    fn does_not_reuse_input_read_through_two_chunk_functions() {
        let source = "local function rd()return input.getNumber(3)end local function rd2()return rd()end local a=rd2() function onTick()output.setNumber(1,rd2())end";
        assert!(output(source).contains("output.setNumber(1,rd2())"));
    }

    #[test]
    fn does_not_reuse_global_carrier_before_binding_via_load_call() {
        // Candidate S9 (w6 loadorder-min): f is called at load before
        // the global carrier G is bound; substituting inside f would
        // read G while it is still nil.
        let source = "local t={n=40} function f()return t.n+2 end output.setNumber(1,f()) G=t.n+2 output.setNumber(2,G)";
        assert!(output(source).contains("return t.n+2"));
    }

    #[test]
    fn does_not_reuse_global_carrier_before_binding_via_indirect_call() {
        // The load-time call reaches f through the helper h.
        let source = "local t={n=40} function f()return t.n+2 end function h()return f() end output.setNumber(1,h()) G=t.n+2";
        assert!(output(source).contains("return t.n+2"));
    }

    #[test]
    fn does_not_reuse_global_carrier_inside_load_iife() {
        // An immediately-invoked literal executes during the prefix,
        // before the carrier statement runs.
        let source = "local t={n=40} output.setNumber(1,(function()return t.n+2 end)()) G=t.n+2";
        assert!(output(source).contains("return t.n+2"));
    }

    #[test]
    fn reuses_global_carrier_when_call_follows_binding() {
        // Control: the only load-time call happens after the binding
        // statement, so the substitution is load-safe.
        let source = "local t={n=40} function f()return t.n+2 end G=t.n+2 output.setNumber(1,f())";
        assert!(output(source).contains("return G"));
    }

    #[test]
    fn reuses_global_carrier_when_call_is_tick_only() {
        // Control: f is called only from onTick, which runs after the
        // whole chunk (and therefore the binding) has executed.
        let source = "local t={n=40} function f()return t.n+2 end G=t.n+2 function onTick()output.setNumber(1,f())end";
        assert!(output(source).contains("return G"));
    }

    #[test]
    fn reuses_property_read_across_tick_boundary() {
        // `property.*` is certified stable for the whole execution, so
        // the load-time snapshot stays valid at any later call.
        let source = "local a=property.getNumber(1) function onTick()output.setNumber(1,property.getNumber(1))end";
        assert_eq!(
            output(source),
            "local a=property.getNumber(1)function onTick()output.setNumber(1,a)end"
        );
    }

    #[test]
    fn reuses_property_read_through_chunk_function() {
        let source = "local function rd()return property.getNumber(3)end local a=rd() function onTick()output.setNumber(1,rd())end";
        assert_eq!(
            output(source),
            "local function rd()return property.getNumber(3)end local a=rd()function onTick()output.setNumber(1,a)end"
        );
    }

    #[test]
    fn does_not_reuse_single_write_dependency_across_goto() {
        // A `goto` in the root block breaks the statement-order argument,
        // so even a textually earlier write no longer proves the
        // dependency value is fixed at the carrier statement.
        let source = "local x=1 x=2 goto done local a=x+1 ::done:: local f=function()return x+1 end return f()";
        assert!(output(source).contains("return x+1"));
    }
}
