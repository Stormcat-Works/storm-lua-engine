//! Uniform field-name alpha conversion for a closed whole-program key space.
//!
//! Unlike scalar replacement this never removes a table, moves an allocation,
//! or substitutes its contents. Aliasing, mutation and reference identity are
//! retained. The proof is deliberately program-wide rather than a speculative
//! per-object points-to analysis: one injective mapping is used at every static
//! key site, and names which occur as ordinary string values are never changed.
//!
//! Eligibility excludes foreign tables escaping their API namespace, reflection,
//! observable key enumeration and unknown external globals. Dynamic keys are
//! allowed only when numeric, or when every possible string originates in the
//! preserved literal vocabulary. String-producing APIs/concatenation invalidate
//! that latter proof. A plain `for k,v in pairs(src) do dst[k]=v end` copy is
//! supported because its fixed destination and distinct keys make order and key
//! spelling unobservable. Other pairs/next uses are rejected.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::pass::PassResult;
use storm_lua_analysis::resolver::{resolve, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, TableField};
use storm_lua_syntax::numeric::{decode_lua_string, quote_lua};
use storm_lua_syntax::size::measure_size;

fn namespace(name: &str) -> bool {
    matches!(
        name,
        "math" | "input" | "output" | "screen" | "property" | "string" | "table" | "map"
    )
}

// This is an allow-list of key-insensitive host operations, NOT a purity list.
// Side effects stay in their original positions; only field labels change.
fn supported_builtin(name: &str) -> bool {
    matches!(
        name,
        "type"
            | "tonumber"
            | "tostring"
            | "select"
            | "ipairs"
            | "input.getNumber"
            | "input.getBool"
            | "output.setNumber"
            | "output.setBool"
            | "property.getNumber"
            | "property.getBool"
            | "property.getText"
            | "screen.getWidth"
            | "screen.getHeight"
            | "screen.setColor"
            | "screen.drawClear"
            | "screen.drawText"
            | "screen.drawTextBox"
            | "screen.drawLine"
            | "screen.drawRect"
            | "screen.drawRectF"
            | "screen.drawCircle"
            | "screen.drawCircleF"
            | "screen.drawTriangle"
            | "screen.drawTriangleF"
            | "screen.drawMap"
            | "screen.setMapColorOcean"
            | "screen.setMapColorShallows"
            | "screen.setMapColorLand"
            | "screen.setMapColorGrass"
            | "screen.setMapColorSand"
            | "screen.setMapColorSnow"
            | "map.mapToScreen"
            | "map.screenToMap"
            | "math.abs"
            | "math.acos"
            | "math.asin"
            | "math.atan"
            | "math.ceil"
            | "math.cos"
            | "math.deg"
            | "math.exp"
            | "math.floor"
            | "math.fmod"
            | "math.huge"
            | "math.log"
            | "math.max"
            | "math.maxinteger"
            | "math.min"
            | "math.mininteger"
            | "math.modf"
            | "math.pi"
            | "math.rad"
            | "math.random"
            | "math.randomseed"
            | "math.sin"
            | "math.sqrt"
            | "math.tan"
            | "math.tointeger"
            | "math.type"
            | "math.ult"
            | "math.clamp"
            | "string.byte"
            | "string.char"
            | "string.find"
            | "string.format"
            | "string.gmatch"
            | "string.gsub"
            | "string.len"
            | "string.lower"
            | "string.match"
            | "string.pack"
            | "string.packsize"
            | "string.rep"
            | "string.reverse"
            | "string.sub"
            | "string.unpack"
            | "string.upper"
            | "table.concat"
            | "table.insert"
            | "table.move"
            | "table.pack"
            | "table.remove"
            | "table.sort"
            | "table.unpack"
    )
}

fn string_producer(name: &str) -> bool {
    name == "tostring"
        || name == "property.getText"
        || name == "table.concat"
        || (name.starts_with("string.")
            && !matches!(name, "string.byte" | "string.len" | "string.packsize"))
}

fn identifier(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && bytes.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

pub(crate) fn builtin_reference(
    ast: &Ast,
    res: &Resolution,
    aliases: &HashMap<BindingId, String>,
    node: NodeId,
) -> Option<String> {
    match ast.node(node) {
        Node::Name(_) => res.node_bid[node as usize].and_then(|bid| aliases.get(&bid).cloned()),
        Node::Paren(inner) => builtin_reference(ast, res, aliases, *inner),
        Node::Index(object, key, _) => {
            let Node::Str(raw) = ast.node(*key) else {
                return None;
            };
            Some(format!(
                "{}.{}",
                builtin_reference(ast, res, aliases, *object)?,
                decode_lua_string(raw)
            ))
        }
        _ => None,
    }
}

// This pass needs aliases but not function effect summaries. Collect both local
// initializers and single assignments, which avoids a full effect-analysis run
// for every final search layout.
pub(crate) fn api_aliases(ast: &Ast, res: &Resolution, root: NodeId) -> HashMap<BindingId, String> {
    let mut aliases = HashMap::new();
    for (bid, binding) in res.bindings.iter().enumerate().skip(1) {
        let name = ast.strings.get(binding.name);
        if binding.kind == BindingKind::Global
            && res.binding_write_counts[bid] == 0
            && storm_lua_analysis::resolver::api_roots(name)
            && name != "self"
        {
            aliases.insert(bid as BindingId, name.to_string());
        }
    }
    let mut definitions = Vec::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |node| match ast.node(node) {
        Node::Local(_, values) => {
            for (&bid, &value) in res.node_bids[node as usize].iter().zip(values) {
                if res.binding_write_counts[bid as usize] == 0 {
                    definitions.push((bid, value));
                }
            }
        }
        Node::Assign(targets, values) => {
            for (&target, &value) in targets.iter().zip(values) {
                if matches!(ast.node(target), Node::Name(_)) {
                    if let Some(bid) = res.node_bid[target as usize] {
                        let binding = res.binding(bid);
                        // An assignment is the only definition only for a global
                        // or an uninitialized local. A parameter, local function,
                        // or initialized local already has another value, which
                        // may execute and replace itself with this API function.
                        let uninitialized = binding.kind == BindingKind::Global
                            || binding.kind == BindingKind::Local
                                && binding.decl_node.is_some_and(|n| {
                                    matches!(ast.node(n), Node::Local(_, values) if values.is_empty())
                                });
                        if uninitialized && res.binding_write_counts[bid as usize] == 1 {
                            definitions.push((bid, value));
                        }
                    }
                }
            }
        }
        _ => {}
    });
    loop {
        let mut changed = false;
        for &(bid, value) in &definitions {
            if aliases.contains_key(&bid) {
                continue;
            }
            if let Some(name) = builtin_reference(ast, res, &aliases, value) {
                aliases.insert(bid, name);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    aliases
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Context {
    Value,
    Key,
    ApiKey,
    IndexObject,
    Alias,
    Write,
    CopyIterator,
}

struct Scan<'a> {
    ast: &'a Ast,
    res: &'a Resolution,
    aliases: HashMap<BindingId, String>,
    counts: BTreeMap<String, usize>,
    preserved: HashSet<String>,
    vocabulary: HashSet<String>,
    numeric_for: HashSet<BindingId>,
    dynamic_keys: bool,
    produces_strings: bool,
    implicit_self_depth: usize,
}

impl Scan<'_> {
    fn builtin_reference(&self, node: NodeId) -> Option<String> {
        builtin_reference(self.ast, self.res, &self.aliases, node)
    }

    fn key(&mut self, key: String) {
        self.vocabulary.insert(key.clone());
        *self.counts.entry(key).or_default() += 1;
    }

    fn non_string_key(&self, node: NodeId) -> bool {
        match self.ast.node(node) {
            Node::Num(_) | Node::Bool(_) | Node::Nil | Node::Table(_) | Node::Function(..) => true,
            Node::Paren(x) => self.non_string_key(*x),
            Node::Name(_) => {
                self.res.node_bid[node as usize].is_some_and(|b| self.numeric_for.contains(&b))
            }
            Node::Un(_, _) => true, // No user metatables in an eligible program.
            Node::Bin(op, a, b) if op == "and" || op == "or" => {
                self.non_string_key(*a) && self.non_string_key(*b)
            }
            Node::Bin(op, _, _) => op != "..",
            Node::Call(f, _, None) => self.builtin_reference(*f).is_some_and(|name| {
                name == "tonumber"
                    || name == "input.getNumber"
                    || name == "input.getBool"
                    || name == "property.getNumber"
                    || name == "property.getBool"
                    || (name.starts_with("math.") && name != "math.type")
            }),
            _ => false,
        }
    }

    fn alias_context(&self, target: Option<BindingId>, expression: NodeId) -> Context {
        match (
            target.and_then(|b| self.aliases.get(&b)),
            self.builtin_reference(expression),
        ) {
            (Some(a), Some(b)) if a == &b && namespace(a) => Context::Alias,
            _ => Context::Value,
        }
    }

    fn builtin(&mut self, name: &str, context: Context) -> Result<(), &'static str> {
        if namespace(name) {
            if !matches!(
                context,
                Context::IndexObject | Context::Alias | Context::Write
            ) {
                return Err("foreign namespace escapes");
            }
        } else if name == "pairs" {
            if context != Context::CopyIterator {
                return Err("observable key enumeration");
            }
        } else if !supported_builtin(name) {
            return Err("unsupported host operation");
        }
        if let Some((_, key)) = name.rsplit_once('.') {
            self.preserved.insert(key.to_string());
        }
        self.produces_strings |= string_producer(name);
        Ok(())
    }

    // The loop variables must be used ONLY as dst[k]=v, where dst is a fixed
    // binding, not an expression whose value can change as fields are copied.
    fn copy_loop(&self, node: NodeId) -> Option<(NodeId, NodeId, NodeId)> {
        let Node::Forin(names, iterators, body) = self.ast.node(node) else {
            return None;
        };
        if names.len() != 2 || iterators.len() != 1 {
            return None;
        }
        let Node::Call(f, args, None) = self.ast.node(iterators[0]) else {
            return None;
        };
        if args.len() != 1 || self.builtin_reference(*f).as_deref() != Some("pairs") {
            return None;
        }
        let Node::Block(stmts) = self.ast.node(*body) else {
            return None;
        };
        if stmts.len() != 1 {
            return None;
        }
        let Node::Assign(targets, values) = self.ast.node(stmts[0]) else {
            return None;
        };
        if targets.len() != 1 || values.len() != 1 {
            return None;
        }
        let Node::Index(dst, key, _) = self.ast.node(targets[0]) else {
            return None;
        };
        if !matches!(self.ast.node(*dst), Node::Name(_))
            || !matches!(self.ast.node(*key), Node::Name(_))
            || !matches!(self.ast.node(values[0]), Node::Name(_))
        {
            return None;
        }
        let bids = &self.res.node_bids[node as usize];
        if bids.len() != 2
            || self.res.node_bid[*key as usize] != Some(bids[0])
            || self.res.node_bid[values[0] as usize] != Some(bids[1])
            || self.res.node_bid[*dst as usize].is_none_or(|b| bids.contains(&b))
        {
            return None;
        }
        Some((*f, args[0], *dst))
    }

    fn visit(
        &mut self,
        node: NodeId,
        context: Context,
        function_depth: usize,
    ) -> Result<(), &'static str> {
        match self.ast.node(node) {
            Node::Name(sym) => {
                let spelling = self.ast.strings.get(*sym);
                if matches!(
                    spelling,
                    "_ENV"
                        | "_G"
                        | "debug"
                        | "setmetatable"
                        | "getmetatable"
                        | "rawget"
                        | "rawset"
                        | "load"
                        | "loadfile"
                        | "loadstring"
                        | "dofile"
                        | "httpReply"
                ) {
                    return Err("reflection or foreign callback");
                }
                if let Some(name) = self.builtin_reference(node) {
                    self.builtin(&name, context)?;
                } else if let Some(bid) = self.res.node_bid[node as usize] {
                    let b = self.res.binding(bid);
                    // The parser represents a colon declaration with a
                    // Methodname target; its implicit self is not a global
                    // object supplied by the host.
                    if b.kind == BindingKind::Global
                        && !(spelling == "self" && self.implicit_self_depth > 0)
                        && (self.res.binding_write_counts[bid as usize] == 0
                            || storm_lua_analysis::resolver::api_roots(spelling))
                    {
                        return Err("unknown or mutated external global");
                    }
                }
            }
            Node::Str(raw) => {
                let text = decode_lua_string(raw);
                // A string value exposes string-library functions through its
                // implicit metatable, including via s[key] and an aliased
                // method. Account for those without relying on a known API
                // root at the eventual call site. Bytecode exposes field names.
                if text == "dump" {
                    return Err("bytecode reflection");
                }
                let method = format!("string.{text}");
                if context != Context::ApiKey && supported_builtin(&method) {
                    self.produces_strings |= string_producer(&method);
                }
                if matches!(context, Context::Key | Context::ApiKey) {
                    self.key(text);
                } else {
                    self.vocabulary.insert(text.clone());
                    self.preserved.insert(text);
                }
            }
            Node::Index(object, key, _) => {
                if let Some(name) = self.builtin_reference(*object) {
                    if !namespace(&name)
                        || !matches!(self.ast.node(*key), Node::Str(_))
                        || context == Context::Write
                    {
                        return Err("dynamic or mutated foreign namespace");
                    }
                    let full = self
                        .builtin_reference(node)
                        .ok_or("unknown builtin field")?;
                    self.builtin(&full, Context::Value)?;
                }
                self.visit(*object, Context::IndexObject, function_depth)?;
                if !matches!(self.ast.node(*key), Node::Str(_)) && !self.non_string_key(*key) {
                    self.dynamic_keys = true;
                }
                let key_context = if self.builtin_reference(*object).is_some() {
                    Context::ApiKey
                } else {
                    Context::Key
                };
                self.visit(*key, key_context, function_depth)?;
            }
            Node::Table(fields) => {
                for field in fields {
                    match field {
                        TableField::Name(key, value) => {
                            self.key(self.ast.strings.get(*key).to_string());
                            self.visit(*value, Context::Value, function_depth)?;
                        }
                        TableField::KVar(key, value) => {
                            if !matches!(self.ast.node(*key), Node::Str(_))
                                && !self.non_string_key(*key)
                            {
                                self.dynamic_keys = true;
                            }
                            self.visit(*key, Context::Key, function_depth)?;
                            self.visit(*value, Context::Value, function_depth)?;
                        }
                        TableField::Arr(value) => {
                            self.visit(*value, Context::Value, function_depth)?
                        }
                    }
                }
            }
            Node::Local(_, values) => {
                let bids = &self.res.node_bids[node as usize];
                for (i, value) in values.iter().enumerate() {
                    self.visit(
                        *value,
                        self.alias_context(bids.get(i).copied(), *value),
                        function_depth,
                    )?;
                }
            }
            Node::Assign(targets, values) => {
                for target in targets {
                    self.visit(*target, Context::Write, function_depth)?;
                }
                for (i, value) in values.iter().enumerate() {
                    let bid = targets.get(i).and_then(|t| self.res.node_bid[*t as usize]);
                    self.visit(*value, self.alias_context(bid, *value), function_depth)?;
                }
            }
            Node::Funcstat(target, function) => {
                self.visit(*target, Context::Write, function_depth)?;
                let method = matches!(self.ast.node(*target), Node::Methodname(..));
                self.implicit_self_depth += usize::from(method);
                let result = self.visit(*function, Context::Value, function_depth);
                self.implicit_self_depth -= usize::from(method);
                result?;
            }
            Node::Function(_, _, body) => self.visit(*body, Context::Value, function_depth + 1)?,
            Node::Return(values) if function_depth == 0 && !values.is_empty() => {
                return Err("chunk result escapes")
            }
            Node::Forin(..) if self.copy_loop(node).is_some() => {
                #[expect(
                    clippy::unwrap_used,
                    reason = "The match guard just validated the same immutable node with copy_loop; no mutation occurs between the two queries"
                )]
                let (f, source, destination) = self.copy_loop(node).unwrap();
                self.visit(f, Context::CopyIterator, function_depth)?;
                self.visit(source, Context::Value, function_depth)?;
                self.visit(destination, Context::Value, function_depth)?;
            }
            Node::Call(receiver, args, Some(method)) => {
                // Strings also have methods supplied by the host string library.
                let builtin = format!("string.{method}");
                if method == "dump" {
                    return Err("bytecode reflection");
                }
                if supported_builtin(&builtin) {
                    self.preserved.insert(method.clone());
                    self.produces_strings |= string_producer(&builtin);
                }
                self.key(method.clone());
                self.visit(*receiver, Context::Value, function_depth)?;
                for arg in args {
                    self.visit(*arg, Context::Value, function_depth)?;
                }
            }
            Node::Methodname(object, key) => {
                self.key(self.ast.strings.get(*key).to_string());
                self.visit(*object, Context::Value, function_depth)?;
            }
            _ => {
                if let Node::Bin(op, _, _) = self.ast.node(node) {
                    self.produces_strings |= op == "..";
                }
                let mut children = Vec::new();
                storm_lua_analysis::resolver::for_each_child(self.ast, node, &mut |child| {
                    children.push(child)
                });
                for child in children {
                    self.visit(child, Context::Value, function_depth)?;
                }
            }
        }
        Ok(())
    }
}

fn plan(ast: &Ast, root: NodeId) -> Result<BTreeMap<String, String>, &'static str> {
    let res = resolve(ast, root);
    let mut numeric_for = HashSet::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |node| {
        if matches!(ast.node(node), Node::Fornum(..)) {
            if let Some(bid) = res.node_bid[node as usize] {
                if res.binding_write_counts[bid as usize] == 0 {
                    numeric_for.insert(bid);
                }
            }
        }
    });
    let mut scan = Scan {
        ast,
        res: &res,
        aliases: api_aliases(ast, &res, root),
        counts: BTreeMap::new(),
        preserved: HashSet::new(),
        vocabulary: HashSet::new(),
        numeric_for,
        dynamic_keys: false,
        produces_strings: false,
        implicit_self_depth: 0,
    };
    // Implicit strings from type/math.type and the table.pack count field.
    for name in [
        "nil", "boolean", "number", "string", "function", "userdata", "thread", "table", "integer",
        "float", "n",
    ] {
        scan.preserved.insert(name.to_string());
        scan.vocabulary.insert(name.to_string());
    }
    // Strings have a host __index table: both s:sub(...) and s.sub keep names.
    for key in [
        "byte", "char", "dump", "find", "format", "gmatch", "gsub", "len", "lower", "match",
        "pack", "packsize", "rep", "reverse", "sub", "unpack", "upper",
    ] {
        scan.preserved.insert(key.to_string());
        scan.vocabulary.insert(key.to_string());
    }
    scan.visit(root, Context::Value, 0)?;
    if scan.dynamic_keys && scan.produces_strings {
        return Err("unbounded dynamic string keys");
    }
    let mut keys = scan
        .counts
        .into_iter()
        .filter(|(key, _)| {
            key.len() > 1
                && identifier(key)
                && !key.starts_with("__")
                && !scan.preserved.contains(key)
        })
        .collect::<Vec<_>>();
    keys.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut mapping = BTreeMap::new();
    let alphabet = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_";
    let mut serial = 0usize;
    for (key, _) in keys {
        let short = loop {
            let mut n = serial;
            serial += 1;
            let mut name = String::new();
            loop {
                name.push(alphabet[n % alphabet.len()] as char);
                if n < alphabet.len() {
                    break;
                }
                n = n / alphabet.len() - 1;
            }
            if !storm_lua_analysis::resolver::reserved(&name) && !scan.vocabulary.contains(&name) {
                break name;
            }
        };
        if short.len() < key.len() {
            scan.vocabulary.insert(short.clone());
            mapping.insert(key, short);
        }
    }
    Ok(mapping)
}

/// Shared conservative gate for transformations that cannot tolerate reflection.
pub(crate) fn has_closed_key_space(ast: &Ast, root: NodeId) -> bool {
    plan(ast, root).is_ok()
}

pub fn rename_closed_fields(ast: &mut Ast, root: NodeId) -> PassResult {
    let mapping = match plan(ast, root) {
        Ok(mapping) if !mapping.is_empty() => mapping,
        Ok(_) => {
            return PassResult {
                root,
                saved: Some(0),
                details: None,
            }
        }
        Err(reason) => {
            return PassResult {
                root,
                saved: Some(0),
                details: Some(vec![format!("skipped={reason}")]),
            }
        }
    };
    let before = measure_size(ast, root);
    let mut trial = ast.clone();
    let mut nodes = Vec::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |node| nodes.push(node));
    nodes.sort_unstable();
    nodes.dedup();
    for node in nodes {
        match ast.node(node) {
            Node::Index(object, key, _) => {
                if let Node::Str(raw) = ast.node(*key) {
                    if let Some(short) = mapping.get(&decode_lua_string(raw)) {
                        let renamed_key = trial.str(quote_lua(short));
                        trial
                            .nodes
                            .finish_rename(renamed_key, ast.nodes.capture_origin(*key));
                        trial.nodes.rewrite(
                            node,
                            Node::Index(*object, renamed_key, true),
                            "closed-field-access",
                        );
                    }
                }
            }
            Node::Table(fields) => {
                let renamed_fields = fields
                    .iter()
                    .map(|field| match field {
                        TableField::Name(key, value) => mapping
                            .get(ast.strings.get(*key))
                            .map(|short| TableField::Name(trial.strings.intern(short), *value))
                            .unwrap_or_else(|| field.clone()),
                        TableField::KVar(key, value) => {
                            if let Node::Str(raw) = ast.node(*key) {
                                if let Some(short) = mapping.get(&decode_lua_string(raw)) {
                                    return TableField::Name(trial.strings.intern(short), *value);
                                }
                            }
                            field.clone()
                        }
                        _ => field.clone(),
                    })
                    .collect();
                trial.nodes[node as usize] = Node::Table(renamed_fields);
                trial
                    .nodes
                    .finish_rename(node, ast.nodes.capture_origin(node));
                for (index, field) in fields.iter().enumerate() {
                    if let TableField::KVar(key, _) = field {
                        if matches!(trial.node(node), Node::Table(renamed) if matches!(renamed[index], TableField::Name(..)))
                        {
                            trial.nodes.copy_expression_to_name_from(
                                node,
                                storm_lua_syntax::NameSite::Field(index as u32),
                                &ast.nodes,
                                *key,
                            );
                        }
                    }
                }
            }
            Node::Call(object, args, Some(method)) => {
                if let Some(short) = mapping.get(method) {
                    trial.nodes[node as usize] =
                        Node::Call(*object, args.clone(), Some(short.clone()));
                    trial
                        .nodes
                        .finish_rename(node, ast.nodes.capture_origin(node));
                }
            }
            Node::Methodname(object, key) => {
                if let Some(short) = mapping.get(ast.strings.get(*key)) {
                    trial.nodes[node as usize] =
                        Node::Methodname(*object, trial.strings.intern(short));
                    trial
                        .nodes
                        .finish_rename(node, ast.nodes.capture_origin(node));
                }
            }
            _ => {}
        }
    }
    let after = measure_size(&trial, root);
    if after >= before {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    *ast = trial;
    PassResult {
        root,
        saved: Some((before - after) as u64),
        details: Some(vec![format!("fields={}", mapping.len())]),
    }
}
