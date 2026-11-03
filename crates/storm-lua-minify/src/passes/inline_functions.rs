//! Function-specialization and inlining passes (`passes/inline-functions.ts`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::pass::PassResult;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId, TableField};
use storm_lua_syntax::numeric::{num_val, short_num};
use storm_lua_syntax::size::{measure_expr, measure_size, measure_stmt};

#[derive(Clone)]
struct FunctionInfo {
    bid: BindingId,
    function: NodeId,
    declaration: NodeId,
    parameters: Vec<SymbolId>,
    parameter_bids: Vec<BindingId>,
    body: NodeId,
    variadic: bool,
}

/// Call nodes that appear as statements: in the AST a call statement is a
/// `Call` wrapped in `Callstat`. Lua allows only calls as expression
/// statements, so replacing such a call with a non-call expression would
/// print a bare expression statement that does not reparse. Inlining a
/// statement call is therefore allowed only when the replacement is itself
/// a call.
fn statement_call_nodes(source: &Ast, root: NodeId) -> HashSet<NodeId> {
    fn walk(source: &Ast, node: NodeId, out: &mut HashSet<NodeId>) {
        if let Node::Callstat(inner) = source.node(node) {
            out.insert(*inner);
        }
        let mut children = Vec::new();
        storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| {
            children.push(child)
        });
        for child in children {
            walk(source, child, out);
        }
    }
    let mut out = HashSet::new();
    walk(source, root, &mut out);
    out
}

/// Builtin calls the literal fold can evaluate from a resolved callee
/// identity alone: the `math.*` members in `literal_folding::eval_math`
/// (also evaluated by the builtin branch in `rewrite_literal_calls`
/// below) and the `string.format` zero-pad pattern. A fragment-level
/// fold -- a cloned body folded in a scratch arena or in a synthetic
/// block -- resolves these against the fragment only, where chunk-level
/// redefinitions of the builtin root (a Funcstat or assignment writing
/// `math.max`, a write through an alias of `math`, a shadowing local
/// named `math`) are invisible. A candidate body may therefore only be
/// fragment-folded when the enclosing chunk's analysis resolves every
/// such callee in the body to the same builtin; otherwise the fold
/// could bake in the builtin's value where the program redefined it
/// (candidate H, w9 cycle 63).
pub(crate) const FRAGMENT_EVALUABLE_BUILTINS: &[&str] = &[
    "math.abs",
    "math.atan",
    "math.cos",
    "math.floor",
    "math.max",
    "math.min",
    "math.sin",
    "math.sqrt",
    "math.tan",
    "string.format",
];

/// The dotted path a callee is spelled with (`math.max`, a bare name),
/// mirroring how `EffectAnalyzer::resolve_builtin_reference` reads
/// Index chains: string keys only, no parenthesis unwrapping.
pub(crate) fn spelled_builtin_path(ast: &Ast, mut node: NodeId) -> Option<String> {
    let mut parts = Vec::new();
    loop {
        match ast.node(node) {
            Node::Name(symbol) => {
                parts.push(ast.strings.get(*symbol).to_string());
                parts.reverse();
                return Some(parts.join("."));
            }
            Node::Index(object, key, _) => {
                let Node::Str(raw) = ast.node(*key) else {
                    return None;
                };
                parts.push(spelled_member(raw)?);
                node = *object;
            }
            _ => return None,
        }
    }
}

pub(crate) fn spelled_member(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[0], b'\'' | b'"') && bytes.last() == Some(&bytes[0]) {
        let inner = &raw[1..raw.len() - 1];
        if !inner.as_bytes().contains(&b'\\') {
            return Some(inner.to_string());
        }
    }
    storm_lua_syntax::numeric::decode_lua_string(raw)
}

/// Whether fragment-folding the candidate body `expression` cannot
/// evaluate a builtin call the enclosing chunk redefined or shadowed.
/// Shared with `omit_arguments`, whose trial-body normalization folds a
/// function body in a fragment arena the same way (candidate I,
/// w9 cycle 64):
/// every evaluable builtin callee spelled in the body must resolve to
/// that same builtin under the chunk-level `analyzer`.
pub(crate) fn fragment_fold_builtin_safe(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    expression: NodeId,
) -> bool {
    let mut safe = true;
    storm_lua_syntax::ast_utils::walk(source, expression, &mut |node| {
        if !safe {
            return;
        }
        let Node::Call(function, _, _) = source.node(node) else {
            return;
        };
        let Some(path) = spelled_builtin_path(source, *function) else {
            return;
        };
        if FRAGMENT_EVALUABLE_BUILTINS.contains(&path.as_str())
            && analyzer.resolve_builtin_reference(*function).as_deref() != Some(path.as_str())
        {
            safe = false;
        }
    });
    safe
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn function_infos(ast: &Ast, res: &Resolution, root: NodeId) -> Vec<FunctionInfo> {
    let mut output = Vec::new();
    for (bid, binding) in res.bindings.iter().enumerate().skip(1) {
        let (Some(function), Some(declaration)) = (binding.function_node, binding.decl_node) else {
            continue;
        };
        let Node::Function(parameters, variadic, body) = ast.node(function) else {
            continue;
        };
        output.push(FunctionInfo {
            bid: bid as BindingId,
            function,
            declaration,
            parameters: parameters.clone(),
            parameter_bids: res
                .node_bids
                .get(function as usize)
                .cloned()
                .unwrap_or_default(),
            body: *body,
            variadic: *variadic,
        });
    }
    // The parent index is a pure function of (ast, root), which do not
    // change while the checks run: build it once (lazily, on the first
    // candidate that passes the write-count gate) and share it across all
    // candidates instead of rebuilding it per candidate.
    let mut parents: Option<ParentIndex> = None;
    output.retain(|info| declaration_execution_guaranteed(ast, res, root, info, &mut parents));
    output
}

/// Parent links used by the execution-guarantee check below: for every
/// statement, the block that directly contains it; for every block, the
/// node (statement or function) that directly contains it. Both maps are
/// dense vectors indexed by NodeId (arena ids are contiguous), so the
/// index is built and queried without hashing; the contents are exactly
/// the key/value pairs the previous HashMap representation held.
struct ParentIndex {
    stmt_block: Vec<Option<NodeId>>,
    block_owner: Vec<Option<NodeId>>,
}

impl ParentIndex {
    fn stmt_block(&self, node: NodeId) -> Option<NodeId> {
        self.stmt_block.get(node as usize).copied().flatten()
    }

    fn block_owner(&self, node: NodeId) -> Option<NodeId> {
        self.block_owner.get(node as usize).copied().flatten()
    }
}

fn build_parent_index(ast: &Ast, root: NodeId) -> ParentIndex {
    fn walk(ast: &Ast, node: NodeId, index: &mut ParentIndex) {
        if let Node::Block(statements) = ast.node(node) {
            for &statement in statements {
                index.stmt_block[statement as usize] = Some(node);
            }
        }
        let mut children = Vec::new();
        storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| children.push(child));
        for child in children {
            if matches!(ast.node(child), Node::Block(..)) {
                index.block_owner[child as usize] = Some(node);
            }
            walk(ast, child, index);
        }
    }
    let mut index = ParentIndex {
        stmt_block: vec![None; ast.nodes.len()],
        block_owner: vec![None; ast.nodes.len()],
    };
    walk(ast, root, &mut index);
    index
}

fn subtree_has_jump(ast: &Ast, node: NodeId) -> bool {
    match ast.node(node) {
        Node::Goto(_) | Node::Label(_) | Node::Break => true,
        Node::Function(..) => false,
        _ => {
            let mut found = false;
            storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
                if subtree_has_jump(ast, child) {
                    found = true;
                }
            });
            found
        }
    }
}

/// Region scan for the execution-guarantee check: walks code that may
/// execute before a definition, looking for a read of the target
/// binding. Function bodies are boundaries; a body joins the scanned
/// region only when a read of its binding appears inside an
/// already-scanned region (the expansion queue), which models a call
/// that can actually happen before the definition executes.
struct GuaranteeScan<'a> {
    ast: &'a Ast,
    res: &'a Resolution,
    bid: BindingId,
    known: HashMap<BindingId, NodeId>,
    visited: HashSet<NodeId>,
    queue: Vec<NodeId>,
}

impl GuaranteeScan<'_> {
    fn scan(&mut self, node: NodeId) -> bool {
        match self.ast.node(node) {
            Node::Function(..) => false,
            Node::Name(_) => {
                let Some(bid) = self.res.node_bid.get(node as usize).copied().flatten() else {
                    return false;
                };
                if self.res.node_write[node as usize] {
                    return false;
                }
                if bid == self.bid {
                    return true;
                }
                if let Some(&function) = self.known.get(&bid) {
                    if self.visited.insert(function) {
                        if let Node::Function(_, _, body) = self.ast.node(function) {
                            self.queue.push(*body);
                        }
                    }
                }
                false
            }
            Node::Funcstat(target, function) => {
                if matches!(self.ast.node(*target), Node::Name(_)) {
                    if let Some(bid) = self.res.node_bid.get(*target as usize).copied().flatten() {
                        self.known.insert(bid, *function);
                    }
                }
                self.scan_children(node)
            }
            Node::Localfunc(_, function) => {
                if let Some(bid) = self.res.node_bid.get(node as usize).copied().flatten() {
                    self.known.insert(bid, *function);
                }
                self.scan_children(node)
            }
            Node::Local(_, expressions) => {
                if let Some(bids) = self.res.node_bids.get(node as usize) {
                    for (bid, expression) in bids.iter().zip(expressions.iter()) {
                        if matches!(self.ast.node(*expression), Node::Function(..)) {
                            self.known.insert(*bid, *expression);
                        }
                    }
                }
                self.scan_children(node)
            }
            Node::Assign(targets, expressions) => {
                for (target, expression) in targets.iter().zip(expressions.iter()) {
                    if matches!(self.ast.node(*target), Node::Name(_))
                        && matches!(self.ast.node(*expression), Node::Function(..))
                    {
                        if let Some(bid) =
                            self.res.node_bid.get(*target as usize).copied().flatten()
                        {
                            self.known.insert(bid, *expression);
                        }
                    }
                }
                self.scan_children(node)
            }
            _ => self.scan_children(node),
        }
    }

    fn scan_children(&mut self, node: NodeId) -> bool {
        let mut children = Vec::new();
        storm_lua_analysis::resolver::for_each_child(self.ast, node, &mut |child| {
            children.push(child)
        });
        for child in children {
            if self.scan(child) {
                return true;
            }
        }
        false
    }
}

/// A recorded Funcstat/Localfunc definition may be treated as the
/// binding's value (calls replaced by the body, declaration removed)
/// only when the definition is guaranteed to have executed before any
/// read of the binding can observe a different value (w6 cycle 47,
/// candidate A: definitions inside a conditional branch, a loop body
/// or another function were folded anyway, erasing the original's
/// nil-call error). The guarantee has four parts: the definition is
/// the binding's only write; its block reaches the binding's scope
/// block (the frame) through `do` blocks only; the frame contains no
/// goto/label/break outside function bodies; and no code that may
/// execute before the definition reads the binding.
fn declaration_execution_guaranteed(
    ast: &Ast,
    res: &Resolution,
    root: NodeId,
    info: &FunctionInfo,
    parents: &mut Option<ParentIndex>,
) -> bool {
    let binding = res.binding(info.bid);
    // A Funcstat counts as one write, a Localfunc as none; any further
    // write means some reads observe a different value than the body.
    let expected_writes = match binding.kind {
        storm_lua_analysis::resolver::BindingKind::Localfunc => 0,
        _ => 1,
    };
    if res
        .binding_write_counts
        .get(info.bid as usize)
        .copied()
        .unwrap_or(0)
        != expected_writes
    {
        return false;
    }
    let parents = parents.get_or_insert_with(|| build_parent_index(ast, root));
    let mut chain = vec![info.declaration];
    let Some(mut block) = parents.stmt_block(info.declaration) else {
        return false;
    };
    let frame = loop {
        if res.node_scope_id.get(block as usize).copied().flatten() == Some(binding.scope) {
            break block;
        }
        let Some(owner) = parents.block_owner(block) else {
            return false;
        };
        if !matches!(ast.node(owner), Node::Do(inner) if *inner == block) {
            return false;
        }
        let Some(owner_block) = parents.stmt_block(owner) else {
            return false;
        };
        chain.push(owner);
        block = owner_block;
    };
    if subtree_has_jump(ast, frame) {
        return false;
    }
    // Regions that may execute before the definition, in order: the
    // frame block's statements before the chain statement, then inside
    // each `do` on the chain the statements before the next one.
    let mut regions: Vec<NodeId> = Vec::new();
    let mut current = frame;
    for &statement in chain.iter().rev() {
        if let Node::Block(statements) = ast.node(current) {
            for &s in statements {
                if s == statement {
                    break;
                }
                regions.push(s);
            }
        }
        if statement != info.declaration {
            let Node::Do(inner) = ast.node(statement) else {
                return false;
            };
            current = *inner;
        }
    }
    let mut scan = GuaranteeScan {
        ast,
        res,
        bid: info.bid,
        known: HashMap::new(),
        visited: HashSet::new(),
        queue: regions,
    };
    let mut cursor = 0;
    while cursor < scan.queue.len() {
        let region = scan.queue[cursor];
        cursor += 1;
        if scan.scan(region) {
            return false;
        }
    }
    true
}

fn info_map(infos: &[FunctionInfo]) -> HashMap<BindingId, FunctionInfo> {
    infos.iter().cloned().map(|info| (info.bid, info)).collect()
}

fn direct_function_bid(ast: &Ast, res: &Resolution, node: NodeId) -> Option<BindingId> {
    let Node::Call(function, _, _) = ast.node(node) else {
        return None;
    };
    if !matches!(ast.node(*function), Node::Name(_)) {
        return None;
    }
    res.node_bid.get(*function as usize).copied().flatten()
}

fn walk_nodes(ast: &Ast, root: NodeId) -> Vec<NodeId> {
    let mut output = Vec::new();
    storm_lua_analysis::effects::walk(ast, root, &mut output);
    output
}

fn function_reference_sets(
    ast: &Ast,
    root: NodeId,
    res: &Resolution,
    functions: &HashMap<BindingId, FunctionInfo>,
) -> (HashMap<BindingId, usize>, HashSet<BindingId>) {
    let mut call_counts = HashMap::<BindingId, usize>::new();
    let mut allowed_name_nodes = HashSet::<NodeId>::new();

    for node in walk_nodes(ast, root) {
        match ast.node(node) {
            Node::Call(function, _, _) if matches!(ast.node(*function), Node::Name(_)) => {
                if let Some(bid) = res.node_bid.get(*function as usize).copied().flatten() {
                    if functions.contains_key(&bid) {
                        *call_counts.entry(bid).or_default() += 1;
                        allowed_name_nodes.insert(*function);
                    }
                }
            }
            Node::Funcstat(target, _) if matches!(ast.node(*target), Node::Name(_)) => {
                if let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() {
                    if functions.contains_key(&bid) {
                        allowed_name_nodes.insert(*target);
                    }
                }
            }
            _ => {}
        }
    }

    let mut non_call_refs = HashSet::<BindingId>::new();
    for node in walk_nodes(ast, root) {
        if !matches!(ast.node(node), Node::Name(_)) || allowed_name_nodes.contains(&node) {
            continue;
        }
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            if functions.contains_key(&bid) {
                non_call_refs.insert(bid);
            }
        }
    }
    (call_counts, non_call_refs)
}

fn is_literal(ast: &Ast, node: NodeId) -> bool {
    matches!(
        ast.node(node),
        Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
    )
}

fn literal_nodes_same(ast: &Ast, a: Option<NodeId>, b: Option<NodeId>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => storm_lua_analysis::effects::ast_same(ast, a, b),
        (None, Some(b)) | (Some(b), None) => matches!(ast.node(b), Node::Nil),
    }
}

// Track source binding identities through expression copies and substitutions.
// A single re-resolution at the pass boundary rejects relocation across a
// private module scope or into a caller's same-spelling local. Declaration
// NodeIds, unlike numeric BindingIds, remain stable for unmoved free storage.
type ReferenceOrigins = HashMap<NodeId, BindingId>;
fn reference_origins(res: &Resolution) -> ReferenceOrigins {
    res.node_bid
        .iter()
        .enumerate()
        .filter_map(|(n, b)| b.map(|b| (n as NodeId, b)))
        .collect()
}
fn install_expression(
    ast: &mut Ast,
    node: NodeId,
    replacement: NodeId,
    origins: &mut ReferenceOrigins,
) {
    ast.nodes.relate_within_role(
        replacement,
        node,
        "function-inline-site",
        storm_lua_syntax::explanation::RelationRole::CallSite,
    );
    ast.nodes.add_inline_context(replacement, replacement, node);
    let source_origin = ast.nodes.capture_origin(replacement);
    ast.nodes[node as usize] = ast.node(replacement).clone();
    ast.nodes
        .finish_rewrite(node, source_origin, "function-inline-expression");
    if let Some(bid) = origins.get(&replacement).copied() {
        origins.insert(node, bid);
    } else {
        origins.remove(&node);
    }
}
fn expression_bindings_preserved(
    ast: &Ast,
    root: NodeId,
    old: &Resolution,
    origins: &ReferenceOrigins,
) -> bool {
    let new = resolve(ast, root);
    let mut valid = true;
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |node| {
        if !matches!(ast.node(node), Node::Name(_)) {
            return;
        }
        let (Some(&before), Some(after)) = (origins.get(&node), new.node_bid[node as usize]) else {
            valid = false;
            return;
        };
        let (before, after) = (old.binding(before), new.binding(after));
        valid &= before.name == after.name
            && before.kind == after.kind
            && before.decl_node == after.decl_node;
    });
    valid
}

fn clone_local_subtree(ast: &mut Ast, node: NodeId, origins: &mut ReferenceOrigins) -> NodeId {
    let source_origin = ast.nodes.capture_origin(node);
    let original = ast.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_local_subtree(ast, child, origins)
    });
    let copied = ast.push(mapped);
    ast.nodes
        .finish_rewrite(copied, source_origin, "function-inline-argument-copy");
    if let Some(bid) = origins.get(&node).copied() {
        origins.insert(copied, bid);
    }
    copied
}

fn clone_subtree_with_replacements(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    replacements: &HashMap<BindingId, NodeId>,
    origins: &mut ReferenceOrigins,
) -> NodeId {
    if matches!(source.node(node), Node::Name(_)) {
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            if let Some(replacement) = replacements.get(&bid) {
                let copied = clone_local_subtree(target, *replacement, origins);
                target.nodes.relate_from_role(
                    copied,
                    &source.nodes,
                    node,
                    "function-parameter-substitution",
                    storm_lua_syntax::explanation::RelationRole::ParameterUse,
                );
                return copied;
            }
        }
    }
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_subtree_with_replacements(target, source, res, child, replacements, origins)
    });
    let copied = target.push(mapped);
    // Pure clone: only child IDs may differ (a replaced parameter subtree
    // shifts later siblings), so share the source origin slot instead of
    // recording a rewrite reason per node. The substitution branch above
    // keeps `relate_from_role`: a substituted node is not a clone of the
    // parameter name, so it must not take this path.
    target
        .nodes
        .derive_from_clone(copied, &source.nodes, node, "function-body-copy");
    if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
        origins.insert(copied, bid);
    }
    copied
}

/// Binding ids used as the root name of a `function a.b()` /
/// `function a:b()` definition target anywhere below `node`.  A definition
/// target must stay rooted at a name: substituting any other expression
/// there prints as `function 0.b()`, which no longer parses (candidate K).
fn funcstat_target_root_bids(ast: &Ast, res: &Resolution, node: NodeId) -> HashSet<BindingId> {
    let mut out = HashSet::new();
    for id in walk_nodes(ast, node) {
        let Node::Funcstat(target, _) = ast.node(id) else {
            continue;
        };
        let mut current = *target;
        loop {
            match ast.node(current) {
                Node::Index(object, _, _) => current = *object,
                Node::Methodname(object, _) => current = *object,
                _ => break,
            }
        }
        if matches!(ast.node(current), Node::Name(_)) {
            if let Some(bid) = res.node_bid.get(current as usize).copied().flatten() {
                out.insert(bid);
            }
        }
    }
    out
}

/// True when substituting any binding in `replacements` would rewrite the
/// root of a definition target below `node`.  Callers must decline the
/// substitution (specialization / fold / inline) in that case: the binding
/// being replaced does not exist at the clone site, so keeping the name
/// would silently rebind it (candidate K).
fn subtree_has_replaced_funcstat_root(
    ast: &Ast,
    res: &Resolution,
    node: NodeId,
    replacements: &HashMap<BindingId, NodeId>,
) -> bool {
    funcstat_target_root_bids(ast, res, node)
        .iter()
        .any(|bid| replacements.contains_key(bid))
}

fn count_param_uses(ast: &Ast, res: &Resolution, expression: NodeId, bid: BindingId) -> usize {
    walk_nodes(ast, expression)
        .into_iter()
        .filter(|node| {
            matches!(ast.node(*node), Node::Name(_))
                && res.node_bid.get(*node as usize).copied().flatten() == Some(bid)
        })
        .count()
}

fn remove_statement_from_blocks(ast: &mut Ast, statement: NodeId) {
    ast.nodes
        .retain_block_statements(|id| id != statement, "inline-declaration-removal");
}

pub fn specialize_constant_arguments(
    ast: &mut Ast,
    root: NodeId,
    fold_numeric: bool,
) -> PassResult {
    let before = measure_size(ast, root);
    let source = clone_ast(ast);
    let res = resolve(&source, root);
    let infos = function_infos(&source, &res, root);
    let functions = info_map(&infos);
    // This pass mirrors the TS scanner exactly: only a direct call callee is
    // exempt from non-call references. A global Funcstat declaration target
    // is therefore ineligible, while a Localfunc declaration can specialize.
    let mut direct_call_names = HashSet::<NodeId>::new();
    for node in walk_nodes(&source, root) {
        if let Node::Call(function, _, _) = source.node(node) {
            if matches!(source.node(*function), Node::Name(_)) {
                if let Some(bid) = res.node_bid.get(*function as usize).copied().flatten() {
                    if functions.contains_key(&bid) {
                        direct_call_names.insert(*function);
                    }
                }
            }
        }
    }
    let mut non_call_refs = HashSet::<BindingId>::new();
    for node in walk_nodes(&source, root) {
        if !matches!(source.node(node), Node::Name(_)) || direct_call_names.contains(&node) {
            continue;
        }
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            if functions.contains_key(&bid) {
                non_call_refs.insert(bid);
            }
        }
    }

    let mut calls = HashMap::<BindingId, Vec<NodeId>>::new();
    for node in walk_nodes(&source, root) {
        if let Some(bid) = direct_function_bid(&source, &res, node) {
            if functions.contains_key(&bid) {
                calls.entry(bid).or_default().push(node);
            }
        }
    }

    #[derive(Clone)]
    struct Spec {
        remove: Vec<usize>,
        values: HashMap<BindingId, Option<NodeId>>,
    }
    let mut specs = HashMap::<BindingId, Spec>::new();
    for info in &infos {
        let Some(function_calls) = calls.get(&info.bid) else {
            continue;
        };
        if function_calls.is_empty() || non_call_refs.contains(&info.bid) {
            continue;
        }
        let mut remove = Vec::new();
        let mut values = HashMap::new();
        // A parameter used as the root of a definition target
        // (`function p.b()`) cannot be specialized: the target must stay
        // rooted at the parameter name, and removing the parameter would
        // leave the root resolving to a global (candidate K).
        let funcstat_roots = funcstat_target_root_bids(&source, &res, info.body);
        for index in 0..info.parameters.len() {
            if info
                .parameter_bids
                .get(index)
                .is_some_and(|bid| funcstat_roots.contains(bid))
            {
                continue;
            }
            // A parameter that is assigned anywhere in the body cannot be
            // specialized safely: later reads observe the assigned value,
            // not the call-site constant.  Keep both the parameter and its
            // argument for this conservative case.
            if info
                .parameter_bids
                .get(index)
                .and_then(|bid| res.binding_write_counts.get(*bid as usize))
                .is_some_and(|count| *count > 0)
            {
                continue;
            }
            let first = match source.node(function_calls[0]) {
                Node::Call(_, arguments, _) => arguments.get(index).copied(),
                _ => None,
            };
            if first.is_some_and(|node| !is_literal(&source, node)) {
                continue;
            }
            if first.is_none() {
                // Missing argument is Lua nil and therefore a literal
                // candidate, unless the call's final argument expands (a
                // call or `...`); that case is rejected in `same` below.
            }
            let same = function_calls.iter().all(|call| match source.node(*call) {
                Node::Call(_, arguments, _) => {
                    // A trailing call or `...` argument expands to fill
                    // parameter slots beyond the written argument list, so
                    // a slot past the written list is NOT nil for such a
                    // call. Treating it as nil specialized the second and
                    // third parameters of `take3(pair())` to nil even though
                    // the call supplies three values. Only Call and Vararg
                    // expand; a parenthesized call (Paren) is adjusted to
                    // one value and stays a genuine nil slot.
                    if index >= arguments.len() {
                        if let Some(last) = arguments.last() {
                            if matches!(source.node(*last), Node::Call(..) | Node::Vararg) {
                                return false;
                            }
                        }
                    }
                    literal_nodes_same(&source, first, arguments.get(index).copied())
                }
                _ => false,
            });
            if same {
                remove.push(index);
                if let Some(param_bid) = info.parameter_bids.get(index) {
                    values.insert(*param_bid, first);
                }
            }
        }
        if !remove.is_empty() {
            specs.insert(info.bid, Spec { remove, values });
        }
    }

    if specs.is_empty() {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }

    let mut target = clone_ast(&source);
    for node in walk_nodes(&source, root) {
        let Some(bid) = direct_function_bid(&source, &res, node) else {
            continue;
        };
        let Some(spec) = specs.get(&bid) else {
            continue;
        };
        let Node::Call(function, arguments, method) = target.node(node).clone() else {
            continue;
        };
        let filtered = arguments
            .into_iter()
            .enumerate()
            .filter_map(|(index, argument)| (!spec.remove.contains(&index)).then_some(argument))
            .collect();
        target.nodes.rewrite(
            node,
            Node::Call(function, filtered, method),
            "constant-argument-specialization",
        );
    }

    for info in &infos {
        let Some(spec) = specs.get(&info.bid) else {
            continue;
        };
        // Replace parameter reads with their common literal value.
        for node in walk_nodes(&source, info.body) {
            if !matches!(source.node(node), Node::Name(_)) {
                continue;
            }
            let Some(bid) = res.node_bid.get(node as usize).copied().flatten() else {
                continue;
            };
            let Some(value) = spec.values.get(&bid) else {
                continue;
            };
            // Parameter bindings are mutable locals.  Replacing an lvalue
            // (`x = ...`) with a literal produces invalid Lua and also erases
            // the function's state transition.  Only substitute read sites.
            if res.node_write.get(node as usize).copied().unwrap_or(false) {
                continue;
            }
            target.nodes[node as usize] = match value {
                Some(value) => source.node(*value).clone(),
                None => Node::Nil,
            };
            if let Some(_value) = value {
                let inputs = info
                    .parameter_bids
                    .iter()
                    .position(|b| *b == bid)
                    .map(|index| {
                        calls[&info.bid]
                            .iter()
                            .filter_map(|&call| match source.node(call) {
                                Node::Call(_, args, _) => args.get(index).copied(),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                super::origins::derive(
                    &mut target,
                    node,
                    &source,
                    &inputs,
                    "specialized-argument-sites",
                );
            } else {
                target
                    .nodes
                    .derive_from(node, &source.nodes, node, "specialized-implicit-nil");
                for &call in &calls[&info.bid] {
                    target.nodes.relate_from_role(
                        node,
                        &source.nodes,
                        call,
                        "specialized-omitted-arguments",
                        storm_lua_syntax::explanation::RelationRole::CallSite,
                    );
                }
            }
            target.nodes.relate_from_role(
                node,
                &source.nodes,
                node,
                "specialized-parameter-read",
                storm_lua_syntax::explanation::RelationRole::ParameterUse,
            );
            if target.nodes.tracks_origins() {
                let Some(index) = info.parameter_bids.iter().position(|b| *b == bid) else {
                    unreachable!("accepted specialization has no parameter")
                };
                for &call in &calls[&info.bid] {
                    if let Node::Call(_, args, _) = source.node(call) {
                        if let Some(&argument) = args.get(index) {
                            target.nodes.relate_from_role(
                                node,
                                &source.nodes,
                                argument,
                                "specialized-actual-argument",
                                storm_lua_syntax::explanation::RelationRole::Argument,
                            );
                        }
                    }
                }
                let literal = match target.node(node) {
                    Node::Num(v) => format!("number:{v}"),
                    Node::Str(v) => format!("string-literal-bytes:{}", v.len()),
                    Node::Bool(v) => format!("boolean:{v}"),
                    Node::Nil => "nil".into(),
                    _ => unreachable!("specialization selected a literal"),
                };
                target.nodes.explain(
                    node,
                    storm_lua_syntax::explanation::OptimizationReason::decision(
                        "constant-argument-specialization",
                        "same-literal-at-every-analyzed-call-and-unmodified-parameter",
                        [
                            ("parameterIndex", index.to_string()),
                            ("analyzedCallSites", calls[&info.bid].len().to_string()),
                            ("value", literal),
                            ("removedParameter", spec.remove.contains(&index).to_string()),
                        ],
                    ),
                );
            }
        }
        let filtered_parameters = info
            .parameters
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(index, parameter)| (!spec.remove.contains(&index)).then_some(parameter))
            .collect();
        target.nodes.rewrite(
            info.function,
            Node::Function(filtered_parameters, info.variadic, info.body),
            "constant-parameter-removal",
        );
        for (new_index, old_index) in (0..info.parameters.len())
            .filter(|i| !spec.remove.contains(i))
            .enumerate()
        {
            target.nodes.copy_name_from(
                info.function,
                storm_lua_syntax::NameSite::Parameter(new_index as u32),
                &source.nodes,
                info.function,
                storm_lua_syntax::NameSite::Parameter(old_index as u32),
            );
        }
    }

    // Numeric folding is separately gated by the caller. Exact mode passes
    // `false` here so specialization cannot bypass the exact-numeric contract.
    if fold_numeric {
        super::literal_folding::constant_fold(&mut target, root, true);
    }
    let after = measure_size(&target, root);
    if after < before {
        *ast = target;
        PassResult {
            root,
            saved: Some((before - after) as u64),
            details: None,
        }
    } else {
        PassResult {
            root,
            saved: Some(0),
            details: None,
        }
    }
}

fn numeric_literal_value(ast: &Ast, node: NodeId) -> Option<f64> {
    match ast.node(node) {
        Node::Num(value) => Some(num_val(value)),
        Node::Un(operator, expression) if operator == "-" => match ast.node(*expression) {
            Node::Num(value) => Some(-num_val(value)),
            _ => None,
        },
        _ => None,
    }
}

fn fold_inlined_expression(ast: &mut Ast, expression: NodeId, aggressive: bool) -> NodeId {
    let return_node = ast.push(Node::Return(vec![expression]));
    let block = ast.push(Node::Block(vec![return_node]));
    let (folded, _) = super::literal_folding::fold_expressions(
        ast,
        block,
        aggressive,
        super::literal_folding::NumericTolerance::default(),
    );
    *ast = folded;
    match ast.node(return_node) {
        Node::Return(expressions) if expressions.len() == 1 => expressions[0],
        _ => expression,
    }
}

/// Like `fold_inlined_expression`, but reuses the caller's shared fold
/// facts when given. `None` keeps the previous per-fold behaviour (used
/// for untracked scratch folds, where no evaluation evidence is built).
fn fold_inlined_expression_with_shared(
    ast: &mut Ast,
    expression: NodeId,
    aggressive: bool,
    shared: Option<&super::literal_folding::FoldSharedFacts>,
) -> NodeId {
    let return_node = ast.push(Node::Return(vec![expression]));
    let block = ast.push(Node::Block(vec![return_node]));
    let (folded, _) = match shared {
        Some(shared) => super::literal_folding::fold_expressions_with_shared(
            ast,
            block,
            aggressive,
            super::literal_folding::NumericTolerance::default(),
            true,
            shared,
        ),
        None => super::literal_folding::fold_expressions(
            ast,
            block,
            aggressive,
            super::literal_folding::NumericTolerance::default(),
        ),
    };
    *ast = folded;
    match ast.node(return_node) {
        Node::Return(expressions) if expressions.len() == 1 => expressions[0],
        _ => expression,
    }
}

/// An empty scratch arena: no nodes and no origin table. `inherit_ast`
/// alone keeps an (empty) origin table, which makes the fold build
/// evaluation evidence in tracked mode only; `clone_without_origins`
/// alone keeps the whole source arena, whose size the fold's resolve
/// pays per call site. Building the arena directly gives empty and
/// untracked without the intermediate: the old composition allocated
/// an empty origin-slot table from `inherit_ast` (one allocation per
/// call site when the source tracks origins) only for
/// `clone_without_origins` to discard it. The resulting scratch arena
/// is identical either way: no nodes, the source's strings, no origins.
fn empty_scratch_ast(source: &Ast) -> Ast {
    Ast {
        nodes: storm_lua_syntax::node_arena::NodeArena::default(),
        strings: source.strings.clone(),
    }
}

fn clone_scratch_values(dst: &mut Ast, src: &Ast, node: NodeId) -> NodeId {
    let original = src.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_scratch_values(dst, src, child)
    });
    dst.push(mapped)
}

/// Clone a function-body expression into a scratch arena, replacing
/// parameter names with the caller's current argument values. The caller
/// guarantees those arguments are literals in `target`.
fn clone_scratch_body(
    dst: &mut Ast,
    source: &Ast,
    target: &Ast,
    res: &Resolution,
    node: NodeId,
    replacements: &HashMap<BindingId, NodeId>,
) -> NodeId {
    if let Node::Name(_) = source.node(node) {
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            if let Some(argument) = replacements.get(&bid) {
                return dst.push(target.node(*argument).clone());
            }
        }
    }
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_scratch_body(dst, source, target, res, child, replacements)
    });
    dst.push(mapped)
}

/// Fold a subtree in its own scratch arena; return its value when it folds
/// to a single atom, and `None` otherwise.
fn scratch_fold_atom(source: &Ast, node: NodeId, aggressive: bool) -> Option<Node> {
    let mut mini = empty_scratch_ast(source);
    let cloned = clone_scratch_values(&mut mini, source, node);
    let folded = fold_inlined_expression(&mut mini, cloned, aggressive);
    let value = mini.node(folded).clone();
    match value {
        Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil => Some(value),
        _ => None,
    }
}

enum ScratchFoldOutcome {
    Installed,
    Rejected,
    Fallback,
}

/// Build a scratch-folded call's final atom directly, without
/// replaying the root fold in the tiny tracked arena.
///
/// The scratch fold above already proved the folded value, and the root
/// fold over already-folded atoms is deterministic: for a Bin/Un root
/// whose operands are atoms, the fold takes its literal-evaluation path
/// exactly when the operator evaluates, the aggressive literal node is
/// accepted, the folded value matches the scratch fold, and (for Bin)
/// the folded atom is no larger than the root. Those conditions are
/// re-checked here from the operand values, and any failure falls back
/// to the tracked fold path, so this never installs an origin the fold
/// would not have produced. The recorded origin is the fold's own: the
/// folded construct's shared slot, the constant-folding rewrite reason,
/// the evaluation decision reason, and the same disposition, token, and
/// name filtering (see `NodeArena::record_folded_atom_origin`). The
/// scratch probe nodes below live in the untracked scratch arena, so
/// they cost the same in both modes and never move the tracked diff.
#[allow(clippy::too_many_arguments)]
fn direct_folded_atom(
    scratch: &mut Ast,
    tiny: &mut Ast,
    source: &Ast,
    expression: NodeId,
    operand_values: &[Node],
    simplified_value: &Node,
    simplified: NodeId,
    aggressive: bool,
    shared: &super::literal_folding::FoldSharedFacts,
    fold_code: &Arc<str>,
) -> Option<NodeId> {
    let (op, is_bin) = match source.node(expression).clone() {
        Node::Bin(op, _, _) => (op, true),
        Node::Un(op, _) => (op, false),
        _ => return None,
    };
    if (is_bin && operand_values.len() != 2) || (!is_bin && operand_values.len() != 1) {
        return None;
    }
    let mut probe_ids = Vec::new();
    for value in operand_values {
        probe_ids.push(scratch.push(value.clone()));
    }
    let mut literals = Vec::new();
    for id in &probe_ids {
        literals.push(super::literal_folding::literal_value(scratch, *id)?);
    }
    let tolerance = super::literal_folding::NumericTolerance::default();
    let decision = if is_bin {
        let evaluated = super::literal_folding::eval_binary(&op, &literals[0], &literals[1])?;
        let candidate_id = super::literal_folding::aggressive_literal_node(
            scratch,
            evaluated.clone(),
            aggressive,
            tolerance,
        )?;
        let candidate = scratch.node(candidate_id).clone();
        if &candidate != simplified_value {
            return None;
        }
        let probe_bin = scratch.push(Node::Bin(op.clone(), probe_ids[0], probe_ids[1]));
        let before_size = measure_expr(scratch, probe_bin);
        let after_size = measure_expr(scratch, simplified);
        if after_size > before_size {
            return None;
        }
        let emitted = super::literal_folding::literal_value(scratch, candidate_id)
            .map(|value| shared.literal_fact_shared(&value))
            .unwrap_or_else(super::literal_folding::nonliteral_fact);
        storm_lua_syntax::explanation::OptimizationReason::decision_shared(
            "constant-folding",
            "literal-operands-evaluated-and-size-nonincreasing",
            [
                (
                    "operator",
                    storm_lua_syntax::explanation::DecisionFactValue::Borrowed(op.as_str()),
                ),
                (
                    "left",
                    storm_lua_syntax::explanation::DecisionFactValue::Shared(
                        shared.literal_fact_shared(&literals[0]),
                    ),
                ),
                (
                    "right",
                    storm_lua_syntax::explanation::DecisionFactValue::Shared(
                        shared.literal_fact_shared(&literals[1]),
                    ),
                ),
                (
                    "evaluated",
                    storm_lua_syntax::explanation::DecisionFactValue::Shared(
                        shared.literal_fact_shared(&evaluated),
                    ),
                ),
                (
                    "emitted",
                    storm_lua_syntax::explanation::DecisionFactValue::Shared(emitted),
                ),
                (
                    "expressionSizeBefore",
                    storm_lua_syntax::explanation::DecisionFactValue::Shared(
                        shared.size_fact_shared(before_size),
                    ),
                ),
                (
                    "expressionSizeAfter",
                    storm_lua_syntax::explanation::DecisionFactValue::Shared(
                        shared.size_fact_shared(after_size),
                    ),
                ),
                (
                    "aggressive",
                    storm_lua_syntax::explanation::DecisionFactValue::Shared(Arc::clone(
                        &shared.aggressive,
                    )),
                ),
                (
                    "absToleranceBits",
                    storm_lua_syntax::explanation::DecisionFactValue::Shared(Arc::clone(
                        &shared.abs_bits,
                    )),
                ),
                (
                    "relToleranceBits",
                    storm_lua_syntax::explanation::DecisionFactValue::Shared(Arc::clone(
                        &shared.rel_bits,
                    )),
                ),
            ],
        )
    } else {
        let evaluated = super::literal_folding::eval_unary(&op, &literals[0])?;
        let candidate_id = super::literal_folding::aggressive_literal_node(
            scratch,
            evaluated.clone(),
            aggressive,
            tolerance,
        )?;
        let candidate = scratch.node(candidate_id).clone();
        if &candidate != simplified_value {
            return None;
        }
        let emitted = super::literal_folding::literal_value(scratch, candidate_id)
            .map(|value| shared.literal_fact_shared(&value))
            .unwrap_or_else(super::literal_folding::nonliteral_fact);
        storm_lua_syntax::explanation::OptimizationReason::decision_shared(
            "constant-folding",
            "literal-unary-evaluation",
            [
                (
                    "operator",
                    storm_lua_syntax::explanation::DecisionFactValue::Borrowed(op.as_str()),
                ),
                (
                    "operand",
                    storm_lua_syntax::explanation::DecisionFactValue::Shared(
                        shared.literal_fact_shared(&literals[0]),
                    ),
                ),
                (
                    "evaluated",
                    storm_lua_syntax::explanation::DecisionFactValue::Shared(
                        shared.literal_fact_shared(&evaluated),
                    ),
                ),
                (
                    "emitted",
                    storm_lua_syntax::explanation::DecisionFactValue::Shared(emitted),
                ),
                (
                    "aggressive",
                    storm_lua_syntax::explanation::DecisionFactValue::Shared(Arc::clone(
                        &shared.aggressive,
                    )),
                ),
                (
                    "absToleranceBits",
                    storm_lua_syntax::explanation::DecisionFactValue::Shared(Arc::clone(
                        &shared.abs_bits,
                    )),
                ),
                (
                    "relToleranceBits",
                    storm_lua_syntax::explanation::DecisionFactValue::Shared(Arc::clone(
                        &shared.rel_bits,
                    )),
                ),
            ],
        )
    };
    let previous = if is_bin {
        Node::Bin(op, 0, 0)
    } else {
        Node::Un(op, 0)
    };
    let atom_t = tiny.push(simplified_value.clone());
    if !tiny.nodes.record_folded_atom_origin(
        atom_t,
        &source.nodes,
        expression,
        &previous,
        fold_code,
        Some(decision),
    ) {
        return None;
    }
    Some(atom_t)
}

/// Per-call scratch evaluation for literal user-function calls.
///
/// The current path clones the function body into the tracked target arena
/// and folds it there, so every intermediate node pays capture / finish /
/// reason allocations even though only the final atom survives: the
/// installed call-site node takes the folded root's origin, and the
/// intermediate nodes' origins are unreachable from the result. This path
/// instead clones the body (values only) into a scratch arena, folds it
/// there, and — only when the fold yields a single atom — rebuilds just
/// that atom's origin in a tiny tracked arena `t` and transfers it to the
/// call site.
///
/// Equivalence argument (atom results only):
/// - The scratch fold sees exactly the same values as the tracked fold
///   (body values from `source`, argument values from `target` after the
///   child recursion), so its value and its accept/reject decision
///   (measure comparison against the untouched call site) match.
/// - The final atom's origin in the tracked flow is the folded root's
///   origin: the body-copy origin of the body expression, plus the root
///   fold's constant-folding rewrite reason and its evaluation evidence.
///   Intermediate nodes' origins are not reachable from the atom. The
///   tiny arena replays only the root fold — its root has the same
///   body-copy origin, its operands are the scratch-folded atoms, so the
///   root evaluation (operator, operand values, sizes, tolerance) is
///   identical — and then applies the same inline-site relation and
///   inline context that `install_expression` applies, before the origin
///   is transferred with a shared-slot derive.
/// - Anything outside this shape falls back to the tracked path below:
///   non-atom fold results, non-Bin/Un/atom body roots (Paren, Call,
///   Table, ...), non-atom folded operands, a tiny-arena fold whose value
///   differs from the scratch fold, and an atom root whose text the fold
///   canonicalizes (its rewrite reason belongs to the tracked fold).
///   The builtin branch is untouched by this path.
#[allow(clippy::too_many_arguments)]
fn try_install_scratch_folded_call(
    source: &Ast,
    target: &mut Ast,
    res: &Resolution,
    node: NodeId,
    call_value: Node,
    expression: NodeId,
    replacements: &HashMap<BindingId, NodeId>,
    aggressive: bool,
    origins: &mut ReferenceOrigins,
    shared: &super::literal_folding::FoldSharedFacts,
    tiny: &mut Ast,
    inline_site_code: &Arc<str>,
    fold_code: &Arc<str>,
) -> ScratchFoldOutcome {
    if subtree_has_replaced_funcstat_root(source, res, expression, replacements) {
        return ScratchFoldOutcome::Fallback;
    }
    // Classify the body root from `source`. Only Bin / Un / atom roots are
    // eligible; a parameter name counts as the argument's atom.
    enum Root {
        AtomLiteral,
        AtomParam(NodeId),
        Bin,
        Un,
    }
    let root = match source.node(expression).clone() {
        Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil => Root::AtomLiteral,
        Node::Bin(..) => Root::Bin,
        Node::Un(..) => Root::Un,
        Node::Name(_) => {
            let Some(bid) = res.node_bid.get(expression as usize).copied().flatten() else {
                return ScratchFoldOutcome::Fallback;
            };
            let Some(argument) = replacements.get(&bid).copied() else {
                return ScratchFoldOutcome::Fallback;
            };
            Root::AtomParam(argument)
        }
        _ => return ScratchFoldOutcome::Fallback,
    };

    // Scratch-clone the body (values only, parameters substituted) and
    // fold the immediate operands first, so the tiny arena below can replay
    // just the root fold over already-folded atoms.
    let mut scratch = empty_scratch_ast(source);
    let inlined = clone_scratch_body(&mut scratch, source, target, res, expression, replacements);
    // The pre-fold atom value, used only to detect text canonicalization
    // of an atom root (which must stay on the tracked path).
    let pre_fold_atom = match scratch.node(inlined).clone() {
        Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil => {
            Some(scratch.node(inlined).clone())
        }
        _ => None,
    };
    let mut operand_values: Vec<Node> = Vec::new();
    match scratch.node(inlined).clone() {
        Node::Bin(_, left, right) => {
            let Some(left_value) = scratch_fold_atom(&scratch, left, aggressive) else {
                return ScratchFoldOutcome::Fallback;
            };
            let Some(right_value) = scratch_fold_atom(&scratch, right, aggressive) else {
                return ScratchFoldOutcome::Fallback;
            };
            operand_values.push(left_value);
            operand_values.push(right_value);
        }
        Node::Un(_, operand) => {
            let Some(value) = scratch_fold_atom(&scratch, operand, aggressive) else {
                return ScratchFoldOutcome::Fallback;
            };
            operand_values.push(value);
        }
        _ => {}
    }

    let simplified = fold_inlined_expression(&mut scratch, inlined, aggressive);
    let simplified_value = scratch.node(simplified).clone();
    if !matches!(
        simplified_value,
        Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
    ) {
        return ScratchFoldOutcome::Fallback;
    }
    if let Some(pre) = pre_fold_atom {
        if pre != simplified_value {
            // The fold canonicalized the atom's text; its rewrite reason
            // belongs to the tracked fold, so stay on the tracked path.
            return ScratchFoldOutcome::Fallback;
        }
    }
    if measure_expr(&scratch, simplified) >= measure_expr(target, node) {
        return ScratchFoldOutcome::Rejected;
    }

    // Tiny tracked arena, reused across calls in this pass: the call
    // site (for its origin contribution), the folded root, and nothing
    // else reachable. Resetting discards every node and origin of the
    // previous call, so each call observes an arena indistinguishable
    // from a fresh `inherit_ast` arena except for capacities.
    tiny.nodes.reset_for_reuse();
    let t = tiny;
    let tmp = t.push(call_value);
    t.nodes
        .derive_from(tmp, &target.nodes, node, "function-inline-site");
    let final_id = match root {
        Root::AtomLiteral => {
            let atom_t = t.push(simplified_value.clone());
            t.nodes
                .derive_from_clone(atom_t, &source.nodes, expression, "function-body-copy");
            atom_t
        }
        Root::AtomParam(argument) => {
            let atom_t = t.push(simplified_value.clone());
            t.nodes
                .derive_from(atom_t, &target.nodes, argument, "function-body-copy");
            atom_t
        }
        Root::Bin | Root::Un => {
            // The tiny-arena fold replay is replaced by a direct
            // construction of the folded atom's origin: the scratch
            // fold above already proved the value, and the root fold
            // over already-folded atoms is deterministic, so replaying
            // it only re-pays the fold's origin bookkeeping. Any shape
            // the direct path cannot prove falls back to the tracked
            // path below, which recomputes the origin with the fold.
            match direct_folded_atom(
                &mut scratch,
                &mut *t,
                source,
                expression,
                &operand_values,
                &simplified_value,
                simplified,
                aggressive,
                shared,
                fold_code,
            ) {
                Some(atom_t) => atom_t,
                None => return ScratchFoldOutcome::Fallback,
            }
        }
    };
    t.nodes.relate_within_role_code(
        final_id,
        tmp,
        inline_site_code,
        storm_lua_syntax::explanation::RelationRole::CallSite,
    );
    t.nodes.add_inline_context(final_id, final_id, tmp);
    let final_value = t.node(final_id).clone();
    target.nodes[node as usize] = final_value;
    target
        .nodes
        .derive_from(node, &t.nodes, final_id, "function-inline-expression");
    origins.remove(&node);
    ScratchFoldOutcome::Installed
}

#[allow(clippy::too_many_arguments)]
fn rewrite_literal_calls(
    source: &Ast,
    target: &mut Ast,
    res: &Resolution,
    statement_calls: &HashSet<NodeId>,
    analyzer: &EffectAnalyzer<'_>,
    timing: &ReadTimingIndex,
    functions: &HashMap<BindingId, FunctionInfo>,
    candidates: &HashMap<BindingId, NodeId>,
    node: NodeId,
    aggressive: bool,
    origins: &mut ReferenceOrigins,
    folded: &mut usize,
    shared: &super::literal_folding::FoldSharedFacts,
    tiny: &mut Ast,
    inline_site_code: &Arc<str>,
    fold_code: &Arc<str>,
) {
    let mut children = Vec::new();
    storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| children.push(child));
    for child in children {
        rewrite_literal_calls(
            source,
            target,
            res,
            statement_calls,
            analyzer,
            timing,
            functions,
            candidates,
            child,
            aggressive,
            origins,
            folded,
            shared,
            tiny,
            inline_site_code,
            fold_code,
        );
    }

    let Node::Call(function, arguments, method) = target.node(node).clone() else {
        return;
    };
    let is_statement_call = statement_calls.contains(&node);
    if !is_statement_call
        && !arguments.is_empty()
        && arguments
            .iter()
            .all(|arg| numeric_literal_value(target, *arg).is_some())
    {
        if let Some(builtin) = analyzer.resolve_builtin_reference(match source.node(node) {
            Node::Call(function, _, _) => *function,
            _ => function,
        }) {
            let values = arguments
                .iter()
                .filter_map(|argument| numeric_literal_value(target, *argument))
                .collect::<Vec<_>>();
            let value = match builtin.as_str() {
                "math.abs" if values.len() == 1 => Some(values[0].abs()),
                "math.floor" if values.len() == 1 => Some(values[0].floor()),
                "math.min" if !values.is_empty() => {
                    Some(values.iter().copied().fold(f64::INFINITY, f64::min))
                }
                "math.max" if !values.is_empty() => {
                    Some(values.iter().copied().fold(f64::NEG_INFINITY, f64::max))
                }
                _ => None,
            };
            if let Some(value) = value.filter(|value| value.is_finite()) {
                let candidate = target.push(Node::Num(short_num(value).into()));
                if measure_expr(target, candidate) < measure_expr(target, node) {
                    target.nodes.rewrite(
                        node,
                        target.node(candidate).clone(),
                        "literal-call-folding",
                    );
                    super::origins::derive(target, node, source, &[node], "literal-call-folding");
                    *folded += 1;
                    return;
                }
            }
        }
    }

    let Some(bid) = (match source.node(node) {
        Node::Call(source_function, _, _)
            if matches!(source.node(*source_function), Node::Name(_)) =>
        {
            res.node_bid
                .get(*source_function as usize)
                .copied()
                .flatten()
        }
        _ => None,
    }) else {
        return;
    };
    let (Some(info), Some(expression)) = (functions.get(&bid), candidates.get(&bid)) else {
        return;
    };
    // Read-timing gate (T02), as in `rewrite_expression_helpers`.
    if !inlined_body_timing_safe(
        source,
        res,
        analyzer,
        *expression,
        &info.parameter_bids,
        timing.enclosing[node as usize],
        timing,
    ) {
        return;
    }
    if arguments.len() != info.parameters.len()
        || !arguments
            .iter()
            .all(|argument| is_literal(target, *argument))
    {
        return;
    }
    if !fragment_fold_builtin_safe(source, analyzer, *expression) {
        return;
    }
    let replacements = info
        .parameter_bids
        .iter()
        .copied()
        .zip(arguments.iter().copied())
        .collect::<HashMap<_, _>>();
    if subtree_has_replaced_funcstat_root(source, res, *expression, &replacements) {
        return;
    }
    if is_statement_call {
        let inlined = clone_subtree_with_replacements(
            target,
            source,
            res,
            *expression,
            &replacements,
            origins,
        );
        let simplified =
            fold_inlined_expression_with_shared(target, inlined, aggressive, Some(shared));
        if matches!(target.node(simplified), Node::Call(..))
            && measure_expr(target, simplified) < measure_expr(target, node)
        {
            install_expression(target, node, simplified, origins);
            *folded += 1;
        }
        return;
    }
    match try_install_scratch_folded_call(
        source,
        target,
        res,
        node,
        Node::Call(function, arguments.clone(), method),
        *expression,
        &replacements,
        aggressive,
        origins,
        shared,
        tiny,
        inline_site_code,
        fold_code,
    ) {
        ScratchFoldOutcome::Installed => {
            *folded += 1;
        }
        ScratchFoldOutcome::Rejected => {
            target.nodes.rewrite(
                node,
                Node::Call(function, arguments, method),
                "literal-call-rejected-candidate",
            );
        }
        ScratchFoldOutcome::Fallback => {
            if subtree_has_replaced_funcstat_root(source, res, *expression, &replacements) {
                target.nodes.rewrite(
                    node,
                    Node::Call(function, arguments, method),
                    "literal-call-rejected-candidate",
                );
                return;
            }
            let inlined = clone_subtree_with_replacements(
                target,
                source,
                res,
                *expression,
                &replacements,
                origins,
            );
            let simplified =
                fold_inlined_expression_with_shared(target, inlined, aggressive, Some(shared));
            if measure_expr(target, simplified) < measure_expr(target, node) {
                install_expression(target, node, simplified, origins);
                *folded += 1;
            } else {
                target.nodes.rewrite(
                    node,
                    Node::Call(function, arguments, method),
                    "literal-call-rejected-candidate",
                );
            }
        }
    }
}

pub fn fold_literal_function_calls(ast: &mut Ast, root: NodeId, aggressive: bool) -> PassResult {
    let before = measure_size(ast, root);
    if ast.strings.contains("_ENV") {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let source = clone_ast(ast);
    let res = resolve(&source, root);
    let infos = function_infos(&source, &res, root);
    let functions = info_map(&infos);
    let analyzer = EffectAnalyzer::new(&source, &res, root, true);
    let timing = read_timing_index(&source, &res, root);
    let mut candidates = HashMap::<BindingId, NodeId>::new();
    for info in &infos {
        if info.variadic {
            continue;
        }
        let Node::Block(statements) = source.node(info.body) else {
            continue;
        };
        if statements.len() != 1 {
            continue;
        }
        let Node::Return(expressions) = source.node(statements[0]) else {
            continue;
        };
        if expressions.len() == 1 {
            candidates.insert(info.bid, expressions[0]);
        }
    }

    let mut target = clone_ast(&source);
    let mut origins = reference_origins(&res);
    let mut folded = 0usize;
    // One shared fact table for every literal-call fold in this pass:
    // all of them run with the same aggressive flag and the default
    // tolerance, so the recorded fact strings are identical and only
    // the per-fold formatting allocations are shared.
    let shared = super::literal_folding::FoldSharedFacts::new(
        aggressive,
        super::literal_folding::NumericTolerance::default(),
    );
    // One shared transformation string for every literal-call relate in
    // this pass: the recorded code is identical, only its allocation is shared.
    let inline_site_code: Arc<str> = Arc::from("function-inline-site");
    // One shared transformation string for every literal-call fold
    // rewrite in this pass, matching the fold's own shared code.
    let fold_code: Arc<str> = Arc::from("constant-folding");
    // One tiny tracked arena for every literal-call install in this
    // pass, reset per call (see `try_install_scratch_folded_call`).
    let mut tiny = storm_lua_syntax::ast_utils::inherit_ast(&source);
    rewrite_literal_calls(
        &source,
        &mut target,
        &res,
        &statement_call_nodes(&source, root),
        &analyzer,
        &timing,
        &functions,
        &candidates,
        root,
        aggressive,
        &mut origins,
        &mut folded,
        &shared,
        &mut tiny,
        &inline_site_code,
        &fold_code,
    );
    let mut after = measure_size(&target, root);
    if after <= before && expression_bindings_preserved(&target, root, &res, &origins) {
        *ast = target;
    } else {
        after = before;
        folded = 0;
    }
    PassResult {
        root,
        saved: Some(before.saturating_sub(after) as u64),
        details: Some(vec![format!("folded={folded}")]),
    }
}

#[allow(clippy::too_many_arguments)]
/// Read-timing index for the expression-helper inlining gate (T02):
/// for every node, the innermost function enclosing it (the chunk
/// root stands for the chunk itself), and for every non-global
/// binding, the function that declares it. A free read in a helper
/// body is an upvalue read there; substituted at a call site inside
/// the declaring function it becomes a plain local read of that
/// frame, whose timing differs in the deferred positions.
struct ReadTimingIndex {
    enclosing: Vec<NodeId>,
    declaring: HashMap<BindingId, NodeId>,
}

fn read_timing_index(source: &Ast, res: &Resolution, root: NodeId) -> ReadTimingIndex {
    fn walk(source: &Ast, node: NodeId, current: NodeId, enclosing: &mut Vec<NodeId>) {
        enclosing[node as usize] = current;
        let inner = if matches!(source.node(node), Node::Function(..)) {
            node
        } else {
            current
        };
        let mut children = Vec::new();
        storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| {
            children.push(child)
        });
        for child in children {
            walk(source, child, inner, enclosing);
        }
    }
    let mut enclosing = vec![root; source.nodes.len()];
    walk(source, root, root, &mut enclosing);
    let mut declaring = HashMap::new();
    for (bid, binding) in res.bindings.iter().enumerate() {
        if binding.kind == BindingKind::Global {
            continue;
        }
        if let Some(decl) = binding.decl_node {
            // A parameter is declared by its function node itself;
            // every other binding is declared in the function that
            // encloses its declaration statement.
            let function = if binding.kind == BindingKind::Param {
                decl
            } else {
                enclosing[decl as usize]
            };
            declaring.insert(bid as BindingId, function);
        }
    }
    ReadTimingIndex {
        enclosing,
        declaring,
    }
}

fn inline_body_contains_write(
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    bid: BindingId,
) -> bool {
    if matches!(source.node(node), Node::Function(..)) {
        return false;
    }
    if matches!(source.node(node), Node::Name(_))
        && res.node_bid.get(node as usize).copied().flatten() == Some(bid)
        && res.node_write.get(node as usize).copied().unwrap_or(false)
    {
        return true;
    }
    let mut children = Vec::new();
    storm_lua_syntax::ast_utils::for_each_child_key(source, node, &mut |_, child| {
        children.push(child)
    });
    children
        .iter()
        .any(|&child| inline_body_contains_write(source, res, child, bid))
}

fn inline_body_contains_call(source: &Ast, node: NodeId) -> bool {
    if matches!(source.node(node), Node::Function(..)) {
        return false;
    }
    if matches!(source.node(node), Node::Call(..)) {
        return true;
    }
    let mut found = false;
    storm_lua_syntax::ast_utils::for_each_child_key(source, node, &mut |_, child| {
        found |= inline_body_contains_call(source, child);
    });
    found
}

/// Whether evaluating `call` may write `bid`: transitive over the
/// static call graph, with a call through an unresolved callee (a
/// parameter, an unidentified name, a member or call result)
/// counting as a writer. Mirrors the forwarding pass's
/// `call_may_write_target` (T03).
fn inline_call_may_write(
    source: &Ast,
    res: &Resolution,
    effects: &EffectAnalyzer<'_>,
    call: NodeId,
    bid: BindingId,
) -> bool {
    let mut visited = HashSet::new();
    inline_call_may_write_inner(source, res, effects, call, bid, &mut visited)
}

fn inline_call_may_write_inner(
    source: &Ast,
    res: &Resolution,
    effects: &EffectAnalyzer<'_>,
    call: NodeId,
    bid: BindingId,
    visited: &mut HashSet<BindingId>,
) -> bool {
    if effects.effects_for_expr(call).writes.contains(&bid) {
        return true;
    }
    let Node::Call(function, _, _) = source.node(call) else {
        return true;
    };
    if effects.resolve_builtin_reference(*function).is_some() {
        return false;
    }
    match source.node(*function) {
        Node::Function(_, _, body) => {
            inline_body_contains_write(source, res, *body, bid)
                || inline_body_contains_call(source, *body)
        }
        Node::Name(_) => {
            if let Some(callee_bid) = res.node_bid.get(*function as usize).copied().flatten() {
                if let Some(function_node) = res.bindings[callee_bid as usize].function_node {
                    if let Node::Function(_, _, body) = source.node(function_node) {
                        if !visited.insert(callee_bid) {
                            return false;
                        }
                        return inline_body_may_write(source, res, effects, *body, bid, visited);
                    }
                    return false;
                }
            }
            true
        }
        _ => true,
    }
}

fn inline_body_may_write(
    source: &Ast,
    res: &Resolution,
    effects: &EffectAnalyzer<'_>,
    node: NodeId,
    bid: BindingId,
    visited: &mut HashSet<BindingId>,
) -> bool {
    if matches!(source.node(node), Node::Function(..)) {
        return false;
    }
    if matches!(source.node(node), Node::Name(_))
        && res.node_bid.get(node as usize).copied().flatten() == Some(bid)
        && res.node_write.get(node as usize).copied().unwrap_or(false)
    {
        return true;
    }
    if matches!(source.node(node), Node::Call(..))
        && inline_call_may_write_inner(source, res, effects, node, bid, visited)
    {
        return true;
    }
    let mut children = Vec::new();
    storm_lua_syntax::ast_utils::for_each_child_key(source, node, &mut |_, child| {
        children.push(child)
    });
    children
        .iter()
        .any(|&child| inline_body_may_write(source, res, effects, child, bid, visited))
}

/// Whether the subtree `node` holds a call that may write `bid`,
/// without crossing a nested function boundary.
fn inline_window_may_write(
    source: &Ast,
    res: &Resolution,
    effects: &EffectAnalyzer<'_>,
    node: NodeId,
    bid: BindingId,
) -> bool {
    if matches!(source.node(node), Node::Function(..)) {
        return false;
    }
    if matches!(source.node(node), Node::Call(..))
        && inline_call_may_write(source, res, effects, node, bid)
    {
        return true;
    }
    let mut children = Vec::new();
    storm_lua_syntax::ast_utils::for_each_child_key(source, node, &mut |_, child| {
        children.push(child)
    });
    children
        .iter()
        .any(|&child| inline_window_may_write(source, res, effects, child, bid))
}

/// Whether substituting the helper body `expression` at a call site
/// inside `site_function` preserves the read timing of its free
/// reads (T02). The deferred positions are the ones the root-globals
/// classification established (binary operands, index bases,
/// computed constructor keys): a local read there is consumed after
/// its sibling subtree runs, while the upvalue read it replaces is
/// materialized at its own evaluation point. Only reads of bindings
/// declared in the call site's own function change class; parameters
/// are substituted, and every other free read stays an upvalue read
/// (or a global read) in both frames.
fn inlined_body_timing_safe(
    source: &Ast,
    res: &Resolution,
    effects: &EffectAnalyzer<'_>,
    expression: NodeId,
    parameters: &[BindingId],
    site_function: NodeId,
    timing: &ReadTimingIndex,
) -> bool {
    fn strip_parens(source: &Ast, mut node: NodeId) -> NodeId {
        while let Node::Paren(inner) = source.node(node) {
            node = *inner;
        }
        node
    }
    fn risk_read(
        source: &Ast,
        res: &Resolution,
        node: NodeId,
        parameters: &[BindingId],
        site_function: NodeId,
        timing: &ReadTimingIndex,
    ) -> Option<BindingId> {
        let node = strip_parens(source, node);
        if !matches!(source.node(node), Node::Name(_))
            || res.node_write.get(node as usize).copied().unwrap_or(false)
        {
            return None;
        }
        let bid = res.node_bid.get(node as usize).copied().flatten()?;
        if parameters.contains(&bid) {
            return None;
        }
        (timing.declaring.get(&bid) == Some(&site_function)).then_some(bid)
    }
    #[allow(clippy::too_many_arguments)]
    fn check(
        source: &Ast,
        res: &Resolution,
        effects: &EffectAnalyzer<'_>,
        read: NodeId,
        window: NodeId,
        parameters: &[BindingId],
        site_function: NodeId,
        timing: &ReadTimingIndex,
    ) -> bool {
        match risk_read(source, res, read, parameters, site_function, timing) {
            Some(bid) => !inline_window_may_write(source, res, effects, window, bid),
            None => true,
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn scan(
        source: &Ast,
        res: &Resolution,
        effects: &EffectAnalyzer<'_>,
        node: NodeId,
        parameters: &[BindingId],
        site_function: NodeId,
        timing: &ReadTimingIndex,
    ) -> bool {
        if matches!(source.node(node), Node::Function(..)) {
            return true;
        }
        let mut safe = match source.node(node) {
            Node::Bin(op, left, right) if !matches!(op.as_str(), "and" | "or") => {
                let (left, right) = (*left, *right);
                check(
                    source,
                    res,
                    effects,
                    left,
                    right,
                    parameters,
                    site_function,
                    timing,
                ) && check(
                    source,
                    res,
                    effects,
                    right,
                    left,
                    parameters,
                    site_function,
                    timing,
                )
            }
            Node::Index(base, key, _) => {
                let (base, key) = (*base, *key);
                check(
                    source,
                    res,
                    effects,
                    base,
                    key,
                    parameters,
                    site_function,
                    timing,
                )
            }
            Node::Table(fields) => fields.iter().all(|field| match field {
                TableField::KVar(key, value) => {
                    let (key, value) = (*key, *value);
                    check(
                        source,
                        res,
                        effects,
                        key,
                        value,
                        parameters,
                        site_function,
                        timing,
                    )
                }
                _ => true,
            }),
            _ => true,
        };
        let mut children = Vec::new();
        storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| {
            children.push(child)
        });
        for child in children {
            safe &= scan(
                source,
                res,
                effects,
                child,
                parameters,
                site_function,
                timing,
            );
        }
        safe
    }
    scan(
        source,
        res,
        effects,
        expression,
        parameters,
        site_function,
        timing,
    )
}

#[allow(clippy::too_many_arguments)]
fn rewrite_expression_helpers(
    source: &Ast,
    target: &mut Ast,
    res: &Resolution,
    statement_calls: &HashSet<NodeId>,
    effects: &EffectAnalyzer<'_>,
    timing: &ReadTimingIndex,
    functions: &HashMap<BindingId, FunctionInfo>,
    candidates: &HashMap<BindingId, NodeId>,
    call_counts: &HashMap<BindingId, usize>,
    non_call_refs: &HashSet<BindingId>,
    node: NodeId,
    aggressive: bool,
    whole_program: bool,
    origins: &mut ReferenceOrigins,
    inlined_counts: &mut HashMap<BindingId, usize>,
) {
    let mut children = Vec::new();
    storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| children.push(child));
    for child in children {
        rewrite_expression_helpers(
            source,
            target,
            res,
            statement_calls,
            effects,
            timing,
            functions,
            candidates,
            call_counts,
            non_call_refs,
            child,
            aggressive,
            whole_program,
            origins,
            inlined_counts,
        );
    }

    let Some(bid) = direct_function_bid(source, res, node) else {
        return;
    };
    let (Some(info), Some(expression)) = (functions.get(&bid), candidates.get(&bid)) else {
        return;
    };
    // Read-timing gate (T02): the body's free reads are upvalue
    // reads in the helper; at a call site inside the function that
    // declares the binding they become local reads of that frame,
    // which are consumed later in the deferred positions. When such
    // a read's window holds a call that may rebind the binding, the
    // substituted form observes a different value, so this call
    // site keeps the call.
    if !inlined_body_timing_safe(
        source,
        res,
        effects,
        *expression,
        &info.parameter_bids,
        timing.enclosing[node as usize],
        timing,
    ) {
        return;
    }
    let Node::Call(_, arguments, _) = target.node(node).clone() else {
        return;
    };
    if arguments.len() != info.parameters.len() {
        return;
    }
    if arguments.len() > 1
        && arguments
            .iter()
            .any(|argument| !is_literal(target, *argument))
    {
        // Multiple dynamic arguments can be observed in a different order
        // after substitution (the helper body may use b before a).
        return;
    }
    // Inlining substitutes arguments at their parameter's use sites.  Lua
    // evaluates call arguments left-to-right, so `return b-a` with two
    // effectful arguments would otherwise become `arg2-arg1`.  Pure/movable
    // arguments are safe; retain the call when an effectful argument would be
    // observed in a different order.
    let parameter_order = walk_nodes(source, *expression)
        .into_iter()
        .filter_map(|id| {
            let bid = res.node_bid.get(id as usize).copied().flatten()?;
            info.parameter_bids
                .iter()
                .position(|candidate| *candidate == bid)
        })
        .collect::<Vec<_>>();
    let preserves_order = parameter_order.windows(2).all(|pair| pair[0] <= pair[1]);
    if !preserves_order
        && arguments
            .iter()
            .any(|argument| !is_literal(target, *argument))
    {
        return;
    }
    let mut replacements = HashMap::new();
    for (index, parameter_bid) in info.parameter_bids.iter().copied().enumerate() {
        let uses = count_param_uses(source, res, *expression, parameter_bid);
        let source_argument = match source.node(node) {
            Node::Call(_, source_arguments, _) => source_arguments[index],
            _ => return,
        };
        if uses > 1 && !effects.is_movable(source_argument) {
            return;
        }
        if !aggressive && uses == 0 && !effects.is_droppable(source_argument) {
            return;
        }
        replacements.insert(parameter_bid, arguments[index]);
    }
    if subtree_has_replaced_funcstat_root(source, res, *expression, &replacements) {
        return;
    }
    let inlined =
        clone_subtree_with_replacements(target, source, res, *expression, &replacements, origins);
    let old_len = measure_expr(target, node);
    let new_len = measure_expr(target, inlined);
    let count = call_counts.get(&bid).copied().unwrap_or(1);
    let decl_cost = measure_stmt(source, info.declaration);
    let profitable = new_len < old_len
        || (whole_program
            && !non_call_refs.contains(&bid)
            && new_len * count < old_len * count + decl_cost);
    if profitable {
        // A statement call may only become another call; anything else
        // would print as a bare expression statement and fail to reparse.
        if statement_calls.contains(&node) && !matches!(target.node(inlined), Node::Call(..)) {
            return;
        }
        install_expression(target, node, inlined, origins);
        *inlined_counts.entry(bid).or_default() += 1;
    }
}

pub fn inline_expression_functions(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    whole_program: bool,
) -> PassResult {
    let before = measure_size(ast, root);
    if ast.strings.contains("_ENV") {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let source = clone_ast(ast);
    let res = resolve(&source, root);
    let infos = function_infos(&source, &res, root);
    let functions = info_map(&infos);
    let effects = EffectAnalyzer::new(&source, &res, root, true);
    let timing = read_timing_index(&source, &res, root);
    let (call_counts, non_call_refs) = function_reference_sets(&source, root, &res, &functions);
    let mut candidates = HashMap::<BindingId, NodeId>::new();
    for info in &infos {
        if info.variadic {
            continue;
        }
        let Node::Block(statements) = source.node(info.body) else {
            continue;
        };
        if statements.len() == 1 {
            if let Node::Return(expressions) = source.node(statements[0]) {
                if expressions.len() == 1 {
                    candidates.insert(info.bid, expressions[0]);
                }
            }
        }
    }

    // Trial bodies are built for every call site but only reach the output
    // when the whole pass is profitable, so the trial runs on an
    // origin-free clone: writing origins for discarded trial nodes was
    // the pass's entire tracking delta on inputs where it does not fire
    // (inline.lua: 12,275 allocs, all from trial argument copies). Only a
    // profitable trial is replayed on the tracked clone, so committed
    // output keeps exactly today's origins; the rewrite itself is
    // origin-independent (values, resolution, effects and sizes only).
    // A closure (not a helper function) so the shared trial/replay block
    // does not trip the too-many-arguments lint: it borrows the pass
    // inputs and takes only the arena being built.
    let build = |target: &mut Ast| -> ReferenceOrigins {
        let mut origins = reference_origins(&res);
        let mut inlined_counts = HashMap::<BindingId, usize>::new();
        rewrite_expression_helpers(
            &source,
            target,
            &res,
            &statement_call_nodes(&source, root),
            &effects,
            &timing,
            &functions,
            &candidates,
            &call_counts,
            &non_call_refs,
            root,
            aggressive,
            whole_program,
            &mut origins,
            &mut inlined_counts,
        );
        if whole_program {
            for info in &infos {
                if non_call_refs.contains(&info.bid) {
                    continue;
                }
                let calls = call_counts.get(&info.bid).copied().unwrap_or(0);
                let inlined = inlined_counts.get(&info.bid).copied().unwrap_or(0);
                if calls > 0 && calls == inlined {
                    remove_statement_from_blocks(target, info.declaration);
                }
            }
        }
        origins
    };
    let mut trial = storm_lua_syntax::ast_utils::clone_without_origins(&source);
    let trial_origins = build(&mut trial);
    let after = measure_size(&trial, root);
    if after < before && expression_bindings_preserved(&trial, root, &res, &trial_origins) {
        if ast.nodes.tracks_origins() {
            let mut target = clone_ast(&source);
            let _ = build(&mut target);
            *ast = target;
        } else {
            *ast = trial;
        }
        PassResult {
            root,
            saved: Some((before - after) as u64),
            details: None,
        }
    } else {
        PassResult {
            root,
            saved: Some(0),
            details: None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn output(source: &str, which: u8) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        match which {
            0 => specialize_constant_arguments(&mut ast, root, true),
            1 => fold_literal_function_calls(&mut ast, root, true),
            _ => inline_expression_functions(&mut ast, root, true, true),
        };
        Printer::new(&ast, false).output(root)
    }

    #[test]
    fn parent_index_dense_matches_map_contents() {
        // The dense Vec representation must hold exactly the key/value
        // pairs the previous HashMap representation built, for every
        // node id in the arena (mapped or not).
        let (ast, root) = parse_source(
            "do a=1 do b=2 end end function f() c=3 if a then d=4 else e=5 end end while a do f() end",
        )
        .expect("parse");
        let mut expected_stmt: HashMap<NodeId, NodeId> = HashMap::new();
        let mut expected_owner: HashMap<NodeId, NodeId> = HashMap::new();
        fn walk(
            ast: &Ast,
            node: NodeId,
            stmt: &mut HashMap<NodeId, NodeId>,
            owner: &mut HashMap<NodeId, NodeId>,
        ) {
            if let Node::Block(statements) = ast.node(node) {
                for &statement in statements {
                    stmt.insert(statement, node);
                }
            }
            let mut children = Vec::new();
            storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
                children.push(child)
            });
            for child in children {
                if matches!(ast.node(child), Node::Block(..)) {
                    owner.insert(child, node);
                }
                walk(ast, child, stmt, owner);
            }
        }
        walk(&ast, root, &mut expected_stmt, &mut expected_owner);
        assert!(!expected_stmt.is_empty() && !expected_owner.is_empty());
        let index = build_parent_index(&ast, root);
        for id in 0..ast.nodes.len() as NodeId {
            assert_eq!(
                index.stmt_block(id),
                expected_stmt.get(&id).copied(),
                "stmt {id}"
            );
            assert_eq!(
                index.block_owner(id),
                expected_owner.get(&id).copied(),
                "owner {id}"
            );
        }
        // The root block itself is owned by no node.
        assert_eq!(index.block_owner(root), None);
    }

    #[test]
    fn specializes_common_literal_argument() {
        let out = output("local function f(x,y)return x+y end a=f(1,2)b=f(3,2)", 0);
        assert!(out.contains("function f(x)"), "{out}");
        assert!(out.contains("f(1)"), "{out}");
    }

    #[test]
    fn specialization_keeps_multivalue_final_argument_slots() {
        // Regression: `take3(pair())` supplies three values, so the
        // unwritten second and third parameters are NOT nil. Before the
        // fix the pass specialized them to nil and the call lost values.
        // Long parameter names make the (buggy) specialization shrink the
        // code in isolation, so the pass adopts it and this test fails
        // without the guard; with short names the size gate alone would
        // mask the bug at pass level.
        let out = output(
            "local function pair() return 7,8,9 end local function take3(aa,bbbb,cccc) return aa,bbbb,cccc end x=take3(pair())",
            0,
        );
        assert!(out.contains("take3(pair())"), "{out}");
        assert!(out.contains("take3(aa,bbbb,cccc)"), "{out}");
        assert!(!out.contains("nil"), "{out}");
    }

    #[test]
    fn specialization_keeps_prefixed_multivalue_final_argument_slots() {
        let out = output(
            "local function pair() return 7,8,9 end local function take4(a,b,c,d) return a,b,c,d end x=take4(0,pair())",
            0,
        );
        // The leading literal 0 IS legitimately specialized (parameter a
        // removed, call keeps pair()); only the expansion-covered slots
        // b/c/d must survive, with no nil substitution.
        assert!(out.contains("take4(pair())"), "{out}");
        assert!(out.contains("return 0,b,c,d"), "{out}");
        assert!(!out.contains("nil"), "{out}");
    }

    #[test]
    fn specialization_keeps_vararg_final_argument_slots() {
        let out = output(
            "local function take3(aa,bbbb,cccc) return aa,bbbb,cccc end local function wrap(...) return take3(...) end",
            0,
        );
        assert!(out.contains("take3(...)"), "{out}");
        assert!(out.contains("take3(aa,bbbb,cccc)"), "{out}");
        assert!(!out.contains("nil"), "{out}");
    }

    #[test]
    fn specialization_still_specializes_genuine_missing_nil() {
        let out = output("local function f(x,y) return x,y end a=f(1) b=f(1)", 0);
        // Both parameters specialize here: x is the common literal 1
        // and y is genuinely missing (nil) at every call.
        assert!(out.contains("function f()"), "{out}");
        assert!(out.contains("return 1,nil"), "{out}");
        assert!(out.contains("a=f()"), "{out}");
    }

    #[test]
    fn specialization_does_not_bypass_numeric_folding_gate() {
        let source = "local function f(x,d)return x/d end a=f(.10000000149011612,6)b=f(.20000000298023224,6)";
        let (mut ast, root) = parse_source(source).expect("parse");
        specialize_constant_arguments(&mut ast, root, false);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains(".10000000149011612"), "{out}");
    }

    #[test]
    fn specialization_keeps_parameter_used_as_funcstat_target_root() {
        // Candidate K (w9 cycle 66): replacing the parameter would print
        // `function 35.b()` and removing it would rebind the root to a
        // global, so the parameter (and its argument) must be kept.
        let out = output(
            "local function f(a, b) function b.b() return a + 1 end end f(2, 35)",
            0,
        );
        assert!(out.contains("function f(b)"), "{out}");
        assert!(out.contains("function b.b()"), "{out}");
        assert!(out.contains("f(35)"), "{out}");
    }

    #[test]
    fn specialization_keeps_parameter_used_as_nested_funcstat_target_root() {
        // Candidate K (w9 cycle 66): the same protection inside a nested
        // function expression, where the parameter is an upvalue.
        let out = output(
            "local function f(b) return function() function b.b() return 1 end return 7 end end g=f(0)",
            0,
        );
        assert!(out.contains("function f(b)"), "{out}");
        assert!(out.contains("function b.b()"), "{out}");
    }

    #[test]
    fn literal_call_fold_keeps_call_when_body_funcstat_roots_at_parameter() {
        // Candidate K (w9 cycle 66): folding the call clones the returned
        // function expression; its inner definition target is rooted at
        // the parameter, which does not exist at the clone site, so the
        // fold must be declined.
        let out = output(
            "local function f(b) return function() function b.b() return 1 end return 7 end end g=f(0)",
            1,
        );
        assert!(out.contains("function b.b()"), "{out}");
        assert!(!out.contains("function 0.b()"), "{out}");
    }

    #[test]
    fn folds_literal_user_call() {
        let out = output("function f(x)return x+1 end a=f(2)", 1);
        assert!(out.contains("a=3"), "{out}");
    }

    #[test]
    fn redefined_builtin_member_is_not_fragment_folded() {
        // Candidate H (w9 cycle 63): the body is folded in a scratch
        // fragment whose analysis cannot see the chunk-level Funcstat
        // redefining math.max, so the call must stay unfolded.
        let out = output(
            "local function g() return math.max(1, 2) end function math.max(x, y) return 99 end a=g()",
            1,
        );
        assert!(out.contains("math.max(1,2)"), "{out}");
        assert!(!out.contains("a=2"), "{out}");
    }

    #[test]
    fn builtin_redefined_before_definition_is_not_fragment_folded() {
        let out = output(
            "function math.max(x, y) return 99 end local function g() return math.max(1, 2) end a=g()",
            1,
        );
        assert!(out.contains("math.max(1,2)"), "{out}");
        assert!(!out.contains("a=2"), "{out}");
    }

    #[test]
    fn builtin_member_assignment_is_not_fragment_folded() {
        let out = output(
            "local function g() return math.floor(1.5) end math.floor=function(x) return 99 end a=g()",
            1,
        );
        assert!(out.contains("math.floor"), "{out}");
        assert!(!out.contains("a=1"), "{out}");
    }

    #[test]
    fn builtin_write_through_alias_is_not_fragment_folded() {
        let out = output(
            "local m=math local function g() return math.max(1, 2) end m.max=function(x, y) return 99 end a=g()",
            1,
        );
        assert!(out.contains("math.max(1,2)"), "{out}");
        assert!(!out.contains("a=2"), "{out}");
    }

    #[test]
    fn shadowed_builtin_root_is_not_fragment_folded() {
        let out = output(
            "local math={max=function(a, b) return 99 end} local function g() return math.max(1, 2) end a=g()",
            1,
        );
        assert!(out.contains("math.max(1,2)"), "{out}");
        assert!(!out.contains("a=2"), "{out}");
    }

    #[test]
    fn redefined_string_format_is_not_fragment_folded() {
        let out = output(
            "local function g() return string.format(\"%03d\", 5) end function string.format(s, n) return \"x\" end a=g()",
            1,
        );
        assert!(out.contains("string.format"), "{out}");
        assert!(!out.contains("005"), "{out}");
    }

    #[test]
    fn nested_builtin_call_in_body_is_not_fragment_folded() {
        let out = output(
            "local function g() return math.max(1, 2)+1 end function math.max(x, y) return 99 end a=g()",
            1,
        );
        assert!(out.contains("math.max(1,2)"), "{out}");
        assert!(!out.contains("a=3"), "{out}");
    }

    #[test]
    fn unmodified_builtin_still_fragment_folds() {
        let out = output("local function g() return math.max(1, 2) end a=g()", 1);
        assert!(out.contains("a=2"), "{out}");
    }

    #[test]
    fn conditional_definition_is_not_folded() {
        // w6 cycle 47 candidate A: the definition sits in a branch that
        // may not run, so the original's call can hit nil; folding the
        // call to the body would erase that error.
        let out = output(
            "if input.getBool(1) then function H() return 1 end end a=H()",
            1,
        );
        assert!(out.contains("H()"), "{out}");
        assert!(!out.contains("a=1"), "{out}");
    }

    #[test]
    fn loop_body_definition_is_not_folded() {
        let out = output("while false do function H() return 1 end end a=H()", 1);
        assert!(out.contains("H()"), "{out}");
        assert!(!out.contains("a=1"), "{out}");
    }

    #[test]
    fn definition_inside_function_is_not_folded() {
        let out = output("function outer() function H() return 1 end end a=H()", 1);
        assert!(out.contains("H()"), "{out}");
        assert!(!out.contains("a=1"), "{out}");
    }

    #[test]
    fn reassigned_function_is_not_folded() {
        // The recorded body is not the value later reads observe.
        let out = output(
            "function H() return 1 end H=function() return 2 end a=H()",
            1,
        );
        assert!(out.contains("H()"), "{out}");
        assert!(!out.contains("a=1"), "{out}");
    }

    #[test]
    fn call_between_two_definitions_is_not_folded() {
        let out = output(
            "function H() return 1 end a=H() function H() return 2 end",
            1,
        );
        assert!(out.contains("H()"), "{out}");
        assert!(!out.contains("a=1"), "{out}");
        assert!(!out.contains("a=2"), "{out}");
    }

    #[test]
    fn load_time_call_before_definition_is_not_folded() {
        let out = output("a=H() function H() return 1 end", 1);
        assert!(out.contains("H()"), "{out}");
        assert!(!out.contains("a=1"), "{out}");
    }

    #[test]
    fn call_via_earlier_function_is_not_folded() {
        // G runs during load, before H's definition executes.
        let out = output(
            "function G() return H() end a=G() function H() return 1 end",
            1,
        );
        assert!(out.contains("H()"), "{out}");
        assert!(!out.contains("a=1"), "{out}");
    }

    #[test]
    fn callback_call_to_later_definition_still_folds() {
        // Host callbacks run only after the chunk finishes loading, so
        // a definition after the callback is already in place.
        let out = output(
            "function onTick() output.setNumber(3,H()) end function H() return 1 end",
            1,
        );
        assert!(out.contains("setNumber(3,1)"), "{out}");
    }

    #[test]
    fn do_block_definition_still_folds() {
        let out = output("do function H() return 1 end end a=H()", 1);
        assert!(out.contains("a=1"), "{out}");
    }

    #[test]
    fn branch_local_function_still_folds() {
        // A Localfunc's uses are confined to its own block, so a
        // definition inside a branch stays sound for them.
        let out = output("if c then local function f() return 1 end a=f() end", 1);
        assert!(out.contains("a=1"), "{out}");
    }

    #[test]
    fn statement_call_is_not_folded_to_bare_expression() {
        // A call used as a statement may only become another call: folding
        // f(0.3) with f(z) = return z into a bare `.3` statement does not
        // reparse, so the whole optimization used to fail with no surviving
        // candidate.
        let out = output(
            "local function f(z) return z end function onTick() f(0.3) end",
            1,
        );
        assert!(out.contains("f(.3)"), "{out}");
        assert!(parse_source(&out).is_ok(), "{out}");
    }

    #[test]
    fn statement_call_is_not_inlined_to_bare_expression() {
        let out = output(
            "local function f(z) return z end function onTick() f(0.3) end",
            2,
        );
        assert!(out.contains("f(.3)"), "{out}");
        assert!(parse_source(&out).is_ok(), "{out}");
    }

    #[test]
    fn statement_call_is_not_inlined_by_one_use_helper() {
        let (mut ast, root) =
            parse_source("local function f(z) return z end function onTick() f(0.3) end")
                .expect("parse");
        inline_one_use_expression_helpers(&mut ast, root, true);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("f(.3)"), "{out}");
        assert!(parse_source(&out).is_ok(), "{out}");
    }

    #[test]
    fn statement_call_to_call_bodied_helper_stays_a_call() {
        let out = output(
            "local function g(x) return x end local function f(z) return g(z) end function onTick() f(0.3) end",
            1,
        );
        assert!(out.contains("f(.3)"), "{out}");
        assert!(parse_source(&out).is_ok(), "{out}");
    }

    #[test]
    fn folds_nested_literal_user_call_via_scratch() {
        let out = output(
            "local function f(a,b,c,d) return (a*2+b*3)*(c*4+d*5) end r=f(1,2,3,4)",
            1,
        );
        assert!(out.contains("r=256"), "{out}");
    }

    #[test]
    fn folds_literal_user_call_via_scratch_unary() {
        let out = output("local function f(x) return -x*2 end r=f(3)", 1);
        assert!(out.contains("r=-6"), "{out}");
    }

    #[test]
    fn empty_scratch_ast_is_empty_untracked_and_shares_strings() {
        let (ast, _root) =
            parse_source("local function f(x) return -x*2 end r=f(3)").expect("parse");
        assert!(ast.nodes.tracks_origins());
        let scratch = empty_scratch_ast(&ast);
        assert_eq!(scratch.nodes.len(), 0);
        assert!(!scratch.nodes.tracks_origins());
        assert!(scratch.strings.same_symbols(&ast.strings));
    }

    #[test]
    fn fuses_one_use_tail_function() {
        let (mut ast, root) = parse_source(
            "local function f(x)local y=x+1 return y*2 end function onTick()local z=f(input.getNumber(1))output.setNumber(1,z)end",
        )
        .expect("parse");
        inline_one_use_statement_and_tail_functions(&mut ast, root, 32);
        let out = Printer::new(&ast, false).output(root);
        assert!(!out.contains("function f"), "{out}");
    }

    #[test]
    fn fusion_does_not_splice_locals_between_goto_and_label() {
        // Candidate N (w9 cycle 66): the callee takes a parameter, so
        // the splice would declare the parameter binding between the
        // goto and its label; Lua rejects a jump into the scope of a
        // local, so the fusion must be declined here.
        let (mut ast, root) = parse_source(
            "local function F39(a, b) return a end local function F41() goto LB1 L46 = F39(1, 2) ::LB1:: return L46 end return F41",
        )
        .expect("parse");
        inline_one_use_statement_and_tail_functions(&mut ast, root, 32);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("function F39"), "{out}");
        assert!(out.contains("goto LB1"), "{out}");
    }

    #[test]
    fn tail_assign_fusion_skips_global_base_rebound_by_body() {
        // The member target's base is a global, so it is located before
        // the call runs; splicing the body first would store into the
        // replacement table instead of the original one.
        let (mut ast, root) =
            parse_source("t={} function f()t={} return 1 end t.x=f()").expect("parse");
        inline_one_use_statement_and_tail_functions(&mut ast, root, 32);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("function f"), "{out}");
    }

    #[test]
    fn tail_assign_fusion_keeps_local_base_rebound_by_body() {
        // A bare local base is re-read from its register at store time,
        // so the body-first order matches source order exactly.
        let (mut ast, root) =
            parse_source("local t={} local function f()t={} return 1 end t.x=f()").expect("parse");
        inline_one_use_statement_and_tail_functions(&mut ast, root, 32);
        let out = Printer::new(&ast, false).output(root);
        assert!(!out.contains("function f"), "{out}");
    }

    #[test]
    fn tail_assign_fusion_skips_chained_base_replaced_by_body() {
        // The base chain `t.x` is materialized before the call; the
        // body's member write replaces it.
        let (mut ast, root) =
            parse_source("t={x={}} function f()t.x={} return 1 end t.x.y=f()").expect("parse");
        inline_one_use_statement_and_tail_functions(&mut ast, root, 32);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("function f"), "{out}");
    }

    #[test]
    fn tail_assign_fusion_skips_param_member_write_with_materialized_base() {
        // The body writes through its parameter, which aliases the
        // caller's `G.x`; the materialized base could be the table the
        // write replaces.
        let (mut ast, root) =
            parse_source("G={x={}} local function f(u)u.x={} return 1 end G.x.y=f(G.x)")
                .expect("parse");
        inline_one_use_statement_and_tail_functions(&mut ast, root, 32);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("function f"), "{out}");
    }

    #[test]
    fn tail_assign_fusion_applies_when_body_writes_nothing() {
        let (mut ast, root) =
            parse_source("t={x={}} function f()return 1 end t.x.y=f()").expect("parse");
        inline_one_use_statement_and_tail_functions(&mut ast, root, 32);
        let out = Printer::new(&ast, false).output(root);
        assert!(!out.contains("function f"), "{out}");
    }

    #[test]
    fn does_not_inline_when_body_read_timing_changes() {
        // T02: `run` reads the chunk local `x` as an upvalue, which
        // PUC materializes before `f()` runs (original returns 4).
        // Inlined into the chunk, the same read becomes a local
        // binary operand, consumed after `f()` rebinds `x` (returns
        // 5). The call site must keep the call.
        let out = output(
            "local x=1 local function f() x=2 return 3 end local function run() return x+f() end return run()",
            2,
        );
        assert!(out.contains("function run()"), "{out}");
        // When the sibling call rebinds a different binding, the
        // timing difference is unobservable and inlining proceeds.
        let out = output(
            "local x=1 local z=0 local function f() z=2 return 3 end local function run() return x+f() end return run()",
            2,
        );
        assert!(!out.contains("function run()"), "{out}");
        // A call site inside another function keeps the read an
        // upvalue read in both frames, so inlining proceeds there:
        // the helper is still removed when its only call site is
        // inside `onTick` (the body read stays an upvalue read).
        let out = output(
            "local x=1 local function f() x=2 return 3 end local function run() return x+f() end function onTick() return run() end return x",
            2,
        );
        assert!(!out.contains("function run()"), "{out}");
    }

    #[test]
    fn preserves_function_definition_for_non_call_reference() {
        let out = output(
            "local function f(x)return x+1 end g=f function onTick()output.setNumber(1,f(1))output.setNumber(2,g(2))end",
            2,
        );
        assert!(out.contains("function f"), "{out}");
        assert!(out.contains("g=f"), "{out}");
    }

    #[test]
    fn tracked_trial_replay_matches_untracked_output() {
        let source =
            "local function f(x)return x+1 end function onTick()output.setNumber(1,f(2))end";
        let (mut tracked, root) = parse_source(source).expect("parse");
        inline_expression_functions(&mut tracked, root, false, true);
        let mut printer = Printer::new(&tracked, false);
        let tracked_out = printer.output(root);
        let (mut plain, plain_root) =
            storm_lua_syntax::parser::parse_source(source).expect("plain parse");
        inline_expression_functions(&mut plain, plain_root, false, true);
        let plain_out = storm_lua_syntax::Printer::new(&plain, false).output(plain_root);
        assert_eq!(tracked_out, plain_out);
        assert!(tracked_out.contains("2+1"), "{tracked_out}");
        assert!(!tracked_out.contains("function f"), "{tracked_out}");
    }
    #[test]
    fn fusion_skips_when_caller_cannot_absorb_callee_locals() {
        // Caller at 195 active locals; the callee peak is 6 (3
        // parameters + 3 body locals), so splicing would reach 201.
        let params = (1..=195)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "local function f(a,b,c)local u=a+b local v=b+c local w=u+v+a+c output.setNumber(7,w) output.setNumber(8,u+v) end function onTick({params})f(input.getNumber(1),input.getNumber(2),input.getNumber(3))end"
        );
        let (mut ast, root) = parse_source(&source).expect("parse");
        inline_one_use_statement_and_tail_functions(&mut ast, root, 32);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("function f"), "{out}");
    }

    #[test]
    fn fusion_applies_when_caller_has_local_headroom() {
        let params = (1..=194)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "local function f(a,b,c)local u=a+b local v=b+c local w=u+v+a+c output.setNumber(7,w) output.setNumber(8,u+v) end function onTick({params})f(input.getNumber(1),input.getNumber(2),input.getNumber(3))end"
        );
        let (mut ast, root) = parse_source(&source).expect("parse");
        inline_one_use_statement_and_tail_functions(&mut ast, root, 32);
        let out = Printer::new(&ast, false).output(root);
        assert!(!out.contains("function f"), "{out}");
    }
}

fn scope_descendant(res: &Resolution, mut scope: u32, ancestor: u32) -> bool {
    loop {
        if scope == ancestor {
            return true;
        }
        let Some(parent) = res
            .scopes
            .get(scope as usize)
            .and_then(|scope| scope.parent)
        else {
            return false;
        };
        scope = parent;
    }
}

fn alpha_clone_node(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    names: &HashMap<BindingId, SymbolId>,
    free_references: &mut Vec<(NodeId, BindingId)>,
) -> NodeId {
    if let Node::Name(symbol) = source.node(node) {
        let symbol = res
            .node_bid
            .get(node as usize)
            .copied()
            .flatten()
            .and_then(|bid| names.get(&bid).copied())
            .unwrap_or(*symbol);
        let copied = target.push(Node::Name(symbol));
        target
            .nodes
            .finish_rename(copied, source.nodes.capture_origin(node));
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            if !names.contains_key(&bid) {
                free_references.push((copied, bid));
            }
        }
        return copied;
    }
    let original = source.node(node).clone();
    let (mut mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        alpha_clone_node(target, source, res, child, names, free_references)
    });
    match &mut mapped {
        Node::Local(local_names, _) | Node::Forin(local_names, _, _) => {
            if let Some(bids) = res.node_bids.get(node as usize) {
                for (index, bid) in bids.iter().enumerate() {
                    if let (Some(slot), Some(symbol)) =
                        (local_names.get_mut(index), names.get(bid).copied())
                    {
                        *slot = symbol;
                    }
                }
            }
        }
        Node::Function(parameters, _, _) => {
            if let Some(bids) = res.node_bids.get(node as usize) {
                for (index, bid) in bids.iter().enumerate() {
                    if let (Some(slot), Some(symbol)) =
                        (parameters.get_mut(index), names.get(bid).copied())
                    {
                        *slot = symbol;
                    }
                }
            }
        }
        Node::Localfunc(name, _) | Node::Fornum(name, ..) => {
            if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
                if let Some(symbol) = names.get(&bid).copied() {
                    *name = symbol;
                }
            }
        }
        _ => {}
    }
    let copied = target.push(mapped);
    // Alpha conversion changes binding spellings but never reorders the slots.
    target
        .nodes
        .finish_rename(copied, source.nodes.capture_origin(node));
    copied
}

#[derive(Clone, Copy)]
enum FusionKind {
    Statement,
    TailLocal,
    TailAssign,
}

#[derive(Clone, Copy)]
struct FusionSite {
    block: NodeId,
    index: usize,
    statement: NodeId,
    call: NodeId,
    kind: FusionKind,
}

fn direct_call_matches(ast: &Ast, res: &Resolution, call: NodeId, bid: BindingId) -> bool {
    let Node::Call(function, _, method) = ast.node(call) else {
        return false;
    };
    method.is_none()
        && matches!(ast.node(*function), Node::Name(_))
        && res.node_bid.get(*function as usize).copied().flatten() == Some(bid)
}

fn find_fusion_site_node(
    ast: &Ast,
    res: &Resolution,
    node: NodeId,
    bid: BindingId,
    statement_mode: bool,
    tail_mode: bool,
) -> Option<FusionSite> {
    if matches!(ast.node(node), Node::Block(_)) {
        return find_fusion_site_block(ast, res, node, bid, statement_mode, tail_mode);
    }
    let mut result = None;
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        if result.is_none() {
            result = find_fusion_site_node(ast, res, child, bid, statement_mode, tail_mode);
        }
    });
    result
}

fn find_fusion_site_block(
    ast: &Ast,
    res: &Resolution,
    block: NodeId,
    bid: BindingId,
    statement_mode: bool,
    tail_mode: bool,
) -> Option<FusionSite> {
    let Node::Block(statements) = ast.node(block) else {
        return None;
    };
    for (index, statement) in statements.iter().copied().enumerate() {
        // mapBlocks() transforms nested blocks before testing the containing statement.
        let mut nested = None;
        storm_lua_analysis::resolver::for_each_child(ast, statement, &mut |child| {
            if nested.is_none() {
                nested = find_fusion_site_node(ast, res, child, bid, statement_mode, tail_mode);
            }
        });
        if nested.is_some() {
            return nested;
        }
        match ast.node(statement) {
            Node::Callstat(call) if statement_mode && direct_call_matches(ast, res, *call, bid) => {
                return Some(FusionSite {
                    block,
                    index,
                    statement,
                    call: *call,
                    kind: FusionKind::Statement,
                });
            }
            Node::Local(_, expressions)
                if tail_mode
                    && expressions.len() == 1
                    && direct_call_matches(ast, res, expressions[0], bid) =>
            {
                return Some(FusionSite {
                    block,
                    index,
                    statement,
                    call: expressions[0],
                    kind: FusionKind::TailLocal,
                });
            }
            Node::Assign(_, expressions)
                if tail_mode
                    && expressions.len() == 1
                    && direct_call_matches(ast, res, expressions[0], bid) =>
            {
                return Some(FusionSite {
                    block,
                    index,
                    statement,
                    call: expressions[0],
                    kind: FusionKind::TailAssign,
                });
            }
            _ => {}
        }
    }
    None
}

/// Enclosing function (node, body block) for every block in the chunk.
fn block_enclosing_functions(source: &Ast, root: NodeId) -> HashMap<NodeId, (NodeId, NodeId)> {
    fn walk(
        source: &Ast,
        node: NodeId,
        fn_node: NodeId,
        fn_body: NodeId,
        out: &mut HashMap<NodeId, (NodeId, NodeId)>,
    ) {
        if matches!(source.node(node), Node::Block(_)) {
            out.insert(node, (fn_node, fn_body));
        }
        match source.node(node).clone() {
            Node::Funcstat(target, function) => {
                walk(source, target, fn_node, fn_body, out);
                if let Node::Function(_, _, body) = source.node(function).clone() {
                    walk(source, body, function, body, out);
                } else {
                    walk(source, function, fn_node, fn_body, out);
                }
            }
            Node::Function(_, _, body) => walk(source, body, node, body, out),
            _ => {
                storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| {
                    walk(source, child, fn_node, fn_body, out);
                });
            }
        }
    }
    let mut out = HashMap::new();
    walk(source, root, root, root, &mut out);
    out
}

/// Whether a callee-body member write rooted at `bid` provably stays
/// inside storage the body itself created: the root must be a body-local
/// (not a parameter, not a loop variable) whose initializer is a table
/// constructor or scalar literal mentioning no binding from outside the
/// body and no call or member read.
fn body_private_root(
    source: &Ast,
    res: &Resolution,
    info: &FunctionInfo,
    body_nodes: &HashSet<NodeId>,
    bid: BindingId,
) -> bool {
    if info.parameter_bids.contains(&bid) {
        return false;
    }
    let binding = &res.bindings[bid as usize];
    if binding.kind != storm_lua_analysis::resolver::BindingKind::Local {
        return false;
    }
    let Some(decl) = binding.decl_node else {
        return false;
    };
    if !body_nodes.contains(&decl) {
        return false;
    }
    let Node::Local(_, expressions) = source.node(decl) else {
        return false;
    };
    let Some(position) = res
        .node_bids
        .get(decl as usize)
        .and_then(|bids| bids.iter().position(|candidate| *candidate == bid))
    else {
        return false;
    };
    let Some(initializer) = expressions.get(position) else {
        return false;
    };
    fn initializer_is_fresh(
        source: &Ast,
        res: &Resolution,
        body_nodes: &HashSet<NodeId>,
        node: NodeId,
    ) -> bool {
        match source.node(node) {
            Node::Nil | Node::Bool(_) | Node::Num(_) | Node::Str(_) => true,
            Node::Table(fields) => fields.iter().all(|field| match field {
                storm_lua_syntax::ast::TableField::Arr(value) => {
                    initializer_is_fresh(source, res, body_nodes, *value)
                }
                storm_lua_syntax::ast::TableField::Name(_, value) => {
                    initializer_is_fresh(source, res, body_nodes, *value)
                }
                storm_lua_syntax::ast::TableField::KVar(key, value) => {
                    initializer_is_fresh(source, res, body_nodes, *key)
                        && initializer_is_fresh(source, res, body_nodes, *value)
                }
            }),
            Node::Name(_) => res
                .node_bid
                .get(node as usize)
                .copied()
                .flatten()
                .is_some_and(|bid| {
                    res.bindings[bid as usize]
                        .decl_node
                        .is_some_and(|decl| body_nodes.contains(&decl))
                }),
            Node::Un(_, inner) | Node::Paren(inner) => {
                initializer_is_fresh(source, res, body_nodes, *inner)
            }
            Node::Bin(_, left, right) => {
                initializer_is_fresh(source, res, body_nodes, *left)
                    && initializer_is_fresh(source, res, body_nodes, *right)
            }
            _ => false,
        }
    }
    initializer_is_fresh(source, res, body_nodes, *initializer)
}

/// Tail-assignment fusion splices the callee body *before* the assignment
/// statement that locates its targets. In source order a member target is
/// located before the call runs whenever its denotation is materialized
/// eagerly: a global or upvalue base is read into a temporary first, and
/// so is every intermediate value of a base chain with at least one
/// member step, as is a non-trivial key expression. Only a bare base (and
/// a bare key) bound to a register of the enclosing function is re-read
/// at store time and stays fusion-safe. This gate rejects the fusion
/// when anything the callee body writes could change what a materialized
/// target denotes: a write to a binding the target reads, a member write
/// through a prefix of a chain the target reads, or an unanalyzable
/// member write (through a parameter, a loop variable, or storage
/// derived from outside the body) while any part of the target is
/// materialized.
fn tail_assign_target_safe(
    source: &Ast,
    res: &Resolution,
    site: &FusionSite,
    info: &FunctionInfo,
    enclosing: &HashMap<NodeId, (NodeId, NodeId)>,
) -> bool {
    let Node::Assign(targets, _) = source.node(site.statement) else {
        return true;
    };
    if !targets
        .iter()
        .any(|target| matches!(source.node(*target), Node::Index(..)))
    {
        return true;
    }

    // Callee body writes, in the source resolution (the alpha clone only
    // renames the body's own locals; global and upvalue writes keep the
    // binding ids collected here).
    let body_node_list = walk_nodes(source, info.body);
    let body_nodes: HashSet<NodeId> = body_node_list.iter().copied().collect();
    let mut name_writes = HashSet::<BindingId>::new();
    let mut member_writes = Vec::<(BindingId, Vec<String>)>::new();
    let mut wild_member_write = false;
    let record_member_write = |ast: &Ast,
                               target: NodeId,
                               member_writes: &mut Vec<(BindingId, Vec<String>)>,
                               wild: &mut bool| {
        let decomposed = match ast.node(target) {
            Node::Methodname(object, name) => super::immutable_values::decompose_chain(
                ast, res, *object,
            )
            .map(|(roots, mut steps)| {
                steps.push(format!("s:{}", ast.strings.get(*name)));
                (roots, steps)
            }),
            _ => super::immutable_values::decompose_chain(ast, res, target),
        };
        match decomposed {
            Some((roots, steps)) if !steps.is_empty() => {
                for root in roots {
                    if info.parameter_bids.contains(&root) {
                        // Writes through a parameter can reach any table
                        // the caller passed in, including the target's.
                        *wild = true;
                    } else if body_private_root(ast, res, info, &body_nodes, root) {
                        // Writes into storage the body created cannot
                        // reach the caller's target denotation through
                        // this first step; deeper steps are not trusted.
                        if steps.len() > 1 {
                            *wild = true;
                        }
                    } else if res.bindings[root as usize]
                        .decl_node
                        .is_some_and(|decl| body_nodes.contains(&decl))
                    {
                        // Rooted at a parameter, loop variable, or a
                        // local derived from outside values: aliasing
                        // with the caller's data is unanalyzable here.
                        *wild = true;
                    } else {
                        member_writes.push((root, steps.clone()));
                    }
                }
            }
            _ => {
                // Not a plain chain: the write is contained only when
                // the chain bottoms out at a fresh table constructor.
                // A call result or an `and`/`or` base can alias caller
                // data, so those are wild.
                let mut base = target;
                loop {
                    match ast.node(base) {
                        Node::Index(inner, _, _)
                        | Node::Methodname(inner, _)
                        | Node::Paren(inner) => base = *inner,
                        Node::Table(_) => return,
                        _ => {
                            *wild = true;
                            return;
                        }
                    }
                }
            }
        }
    };
    for node in &body_node_list {
        if res.node_write.get(*node as usize).copied().unwrap_or(false) {
            if let Some(bid) = res.node_bid.get(*node as usize).copied().flatten() {
                name_writes.insert(bid);
            }
        }
        match source.node(*node) {
            Node::Assign(targets, _) => {
                for target in targets {
                    if !matches!(source.node(*target), Node::Name(_)) {
                        record_member_write(
                            source,
                            *target,
                            &mut member_writes,
                            &mut wild_member_write,
                        );
                    }
                }
            }
            Node::Funcstat(target, _) if !matches!(source.node(*target), Node::Name(_)) => {
                record_member_write(source, *target, &mut member_writes, &mut wild_member_write);
            }
            _ => {}
        }
    }

    // Enclosing function of the site, for the register (live) test.
    let Some(&(fn_node, fn_body)) = enclosing.get(&site.block) else {
        return false;
    };
    let fn_subtree: HashSet<NodeId> = walk_nodes(source, fn_body).into_iter().collect();
    let is_register = |bid: BindingId| -> bool {
        let binding = &res.bindings[bid as usize];
        matches!(
            binding.kind,
            storm_lua_analysis::resolver::BindingKind::Local
                | storm_lua_analysis::resolver::BindingKind::Param
                | storm_lua_analysis::resolver::BindingKind::Localfunc
                | storm_lua_analysis::resolver::BindingKind::For
        ) && (binding.decl_node == Some(fn_node)
            || binding
                .decl_node
                .is_some_and(|decl| fn_subtree.contains(&decl)))
    };
    let bare_register = |node: NodeId| -> bool {
        matches!(source.node(node), Node::Name(_))
            && res
                .node_bid
                .get(node as usize)
                .copied()
                .flatten()
                .is_some_and(is_register)
    };

    fn collect_deps(
        source: &Ast,
        res: &Resolution,
        expression: NodeId,
        dep_bids: &mut HashSet<BindingId>,
        dep_chains: &mut Vec<(BindingId, Vec<String>)>,
        dep_wild: &mut bool,
    ) {
        for node in walk_nodes(source, expression) {
            match source.node(node) {
                Node::Name(_) => {
                    if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
                        dep_bids.insert(bid);
                    }
                }
                Node::Index(..) => {
                    match super::immutable_values::decompose_chain(source, res, node) {
                        Some((roots, steps)) => {
                            if !steps.is_empty() {
                                for root in roots {
                                    dep_chains.push((root, steps.clone()));
                                }
                            }
                        }
                        None => {
                            // An undecomposable chain (call base,
                            // mismatched `and`/`or` base) is wild unless
                            // it bottoms out at a fresh table constructor,
                            // which no callee write can reach.
                            let mut base = node;
                            loop {
                                match source.node(base) {
                                    Node::Index(inner, _, _)
                                    | Node::Methodname(inner, _)
                                    | Node::Paren(inner) => base = *inner,
                                    Node::Table(_) => break,
                                    _ => {
                                        *dep_wild = true;
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    for target in targets {
        let Node::Index(object, key, _) = source.node(*target) else {
            continue;
        };
        let base_live = bare_register(*object);
        let key_live = matches!(
            source.node(*key),
            Node::Nil | Node::Bool(_) | Node::Num(_) | Node::Str(_)
        ) || bare_register(*key);
        if base_live && key_live {
            continue;
        }
        let mut dep_bids = HashSet::new();
        let mut dep_chains = Vec::new();
        let mut dep_wild = false;
        if !base_live {
            collect_deps(
                source,
                res,
                *object,
                &mut dep_bids,
                &mut dep_chains,
                &mut dep_wild,
            );
        }
        if !key_live {
            collect_deps(
                source,
                res,
                *key,
                &mut dep_bids,
                &mut dep_chains,
                &mut dep_wild,
            );
        }
        if name_writes.iter().any(|bid| dep_bids.contains(bid)) {
            return false;
        }
        if wild_member_write {
            return false;
        }
        if dep_wild && !member_writes.is_empty() {
            return false;
        }
        for (write_root, write_steps) in &member_writes {
            for (dep_root, dep_steps) in &dep_chains {
                if write_root == dep_root
                    && write_steps.len() <= dep_steps.len()
                    && write_steps
                        .iter()
                        .zip(dep_steps.iter())
                        .all(|(write, dep)| write == dep)
                {
                    return false;
                }
            }
        }
    }
    true
}

fn one_use_reference_sets(
    ast: &Ast,
    root: NodeId,
    res: &Resolution,
    functions: &HashMap<BindingId, FunctionInfo>,
) -> (HashMap<BindingId, usize>, HashSet<BindingId>) {
    let mut direct_names = HashSet::new();
    let mut definition_names = HashSet::new();
    let mut call_counts = HashMap::<BindingId, usize>::new();
    for node in walk_nodes(ast, root) {
        match ast.node(node) {
            Node::Call(function, _, method)
                if method.is_none() && matches!(ast.node(*function), Node::Name(_)) =>
            {
                if let Some(bid) = res.node_bid.get(*function as usize).copied().flatten() {
                    if functions.contains_key(&bid) {
                        direct_names.insert(*function);
                        *call_counts.entry(bid).or_default() += 1;
                    }
                }
            }
            Node::Funcstat(target, _) if matches!(ast.node(*target), Node::Name(_)) => {
                if let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() {
                    if functions.contains_key(&bid) {
                        definition_names.insert(*target);
                    }
                }
            }
            _ => {}
        }
    }
    let mut bad_refs = HashSet::new();
    for node in walk_nodes(ast, root) {
        if !matches!(ast.node(node), Node::Name(_))
            || direct_names.contains(&node)
            || definition_names.contains(&node)
        {
            continue;
        }
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            if functions.contains_key(&bid) {
                bad_refs.insert(bid);
            }
        }
    }
    (call_counts, bad_refs)
}

/// Conservative active-local maximum of the enclosing function for
/// every block in the chunk, keyed by block node. The chunk itself is
/// the outermost function (zero parameters); method definitions add
/// their implicit `self` parameter.
fn block_local_budgets(source: &Ast, root: NodeId) -> HashMap<NodeId, usize> {
    fn walk(
        source: &Ast,
        node: NodeId,
        fn_body: NodeId,
        fn_params: usize,
        memo: &mut HashMap<NodeId, usize>,
        budgets: &mut HashMap<NodeId, usize>,
    ) {
        if matches!(source.node(node), Node::Block(_)) {
            let max = *memo.entry(fn_body).or_insert_with(|| {
                super::single_member_cache::max_active_locals(source, fn_params, fn_body)
            });
            budgets.insert(node, max);
        }
        match source.node(node).clone() {
            Node::Funcstat(target, function) => {
                walk(source, target, fn_body, fn_params, memo, budgets);
                if let Node::Function(params, _, body) = source.node(function).clone() {
                    let extra = usize::from(matches!(source.node(target), Node::Methodname(..)));
                    walk(source, body, body, params.len() + extra, memo, budgets);
                } else {
                    walk(source, function, fn_body, fn_params, memo, budgets);
                }
            }
            Node::Function(params, _, body) => {
                walk(source, body, body, params.len(), memo, budgets);
            }
            _ => {
                let mut children = Vec::new();
                storm_lua_syntax::ast_utils::for_each_child(source, node, &mut |child| {
                    children.push(child);
                });
                for child in children {
                    walk(source, child, fn_body, fn_params, memo, budgets);
                }
            }
        }
    }
    let mut memo = HashMap::new();
    let mut budgets = HashMap::new();
    walk(source, root, root, 0, &mut memo, &mut budgets);
    budgets
}

/// True when splicing new local declarations at `site` could make an
/// existing goto illegal: a goto in the enclosing function, positioned
/// before the site statement, whose target label sits in the site block
/// after the site. The spliced declarations (the parameter binding and
/// any body locals) would stand between the goto and its label, and Lua
/// rejects a jump into the scope of a local at load time (candidate N).
fn fusion_site_jumps_into_new_locals(
    source: &Ast,
    enclosing: &HashMap<NodeId, (NodeId, NodeId)>,
    site: &FusionSite,
) -> bool {
    let Node::Block(statements) = source.node(site.block) else {
        return false;
    };
    let mut labels_after = HashSet::new();
    for statement in statements.iter().skip(site.index + 1) {
        if let Node::Label(symbol) = source.node(*statement) {
            labels_after.insert(*symbol);
        }
    }
    if labels_after.is_empty() {
        return false;
    }
    let body = enclosing
        .get(&site.block)
        .map(|(_, body)| *body)
        .unwrap_or(site.block);
    let order = walk_nodes(source, body);
    let Some(site_pos) = order.iter().position(|node| *node == site.statement) else {
        return false;
    };
    order[..site_pos]
        .iter()
        .any(|node| match source.node(*node) {
            Node::Goto(symbol) => labels_after.contains(symbol),
            _ => false,
        })
}

pub fn inline_one_use_statement_and_tail_functions(
    ast: &mut Ast,
    root: NodeId,
    max_rounds: usize,
) -> PassResult {
    let original_size = measure_size(ast, root);
    // Implicit global accesses use the lexical _ENV upvalue in Lua 5.3.
    // The resolver's global BindingId alone cannot prove that environment is
    // unchanged after crossing a function boundary.
    if ast.strings.contains("_ENV") {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let mut serial = 0usize;
    let mut details = Vec::new();

    for _ in 0..max_rounds {
        let source = clone_ast(ast);
        let res = resolve(&source, root);
        let budgets = block_local_budgets(&source, root);
        let enclosing = block_enclosing_functions(&source, root);
        let infos = function_infos(&source, &res, root);
        let functions = info_map(&infos);
        let (call_counts, bad_refs) = one_use_reference_sets(&source, root, &res, &functions);
        let mut applied = false;

        for info in &infos {
            if call_counts.get(&info.bid).copied().unwrap_or(0) != 1 || bad_refs.contains(&info.bid)
            {
                continue;
            }
            let binding_name = source.strings.get(res.binding(info.bid).name);
            if storm_lua_spec::environment::VEHICLE_CALLBACKS.contains(&binding_name)
                || info.variadic
            {
                continue;
            }
            let body_nodes = walk_nodes(&source, info.body);
            if body_nodes.iter().any(|node| {
                matches!(
                    source.node(*node),
                    Node::Goto(_) | Node::Label(_) | Node::Vararg
                )
            }) {
                continue;
            }
            let returns = body_nodes
                .iter()
                .copied()
                .filter(|node| matches!(source.node(*node), Node::Return(_)))
                .collect::<Vec<_>>();
            let Node::Block(body_statements) = source.node(info.body) else {
                continue;
            };
            let statement_mode = returns.is_empty();
            let tail_mode =
                returns.len() == 1 && body_statements.last().copied() == Some(returns[0]);
            if !statement_mode && !tail_mode {
                continue;
            }

            serial += 1;
            // Source-only rejection gates, hoisted ahead of the trial
            // clone: the fusion site search, the tail-assignment target
            // check and the goto-into-new-locals check read only the
            // round source (its AST, resolution and enclosing map),
            // never the trial arena, so evaluating them before the
            // clone cannot change their outcomes. `serial` is consumed
            // at the same point as before, so candidates that pass
            // still receive the same generated names.
            let Some(site) =
                find_fusion_site_block(&source, &res, root, info.bid, statement_mode, tail_mode)
            else {
                continue;
            };
            // A member target is located before the call runs in source
            // order unless it denotes a caller register; splicing the
            // body first would locate it after the body instead.
            if matches!(site.kind, FusionKind::TailAssign)
                && !tail_assign_target_safe(&source, &res, &site, info, &enclosing)
            {
                continue;
            }
            // The splice declares the parameter binding (and any body
            // locals) at the site; a goto jumping over that point to a
            // later label in the same block would jump into their scope
            // and fail to load (candidate N). The declaration test is
            // evaluated on the source body: the alpha clone renames
            // bindings but never changes the parameter count or which
            // statements are local declarations.
            let splice_declares_locals = !info.parameters.is_empty()
                || body_statements.iter().any(|statement| {
                    matches!(
                        source.node(*statement),
                        Node::Local(..) | Node::Localfunc(..)
                    )
                });
            if splice_declares_locals
                && fusion_site_jumps_into_new_locals(&source, &enclosing, &site)
            {
                continue;
            }
            let function_scope = res
                .node_scope_id
                .get(info.function as usize)
                .copied()
                .flatten()
                .unwrap_or(0);
            let mut trial = clone_ast(&source);
            let mut taken = source
                .strings
                .all_strings()
                .into_iter()
                .collect::<HashSet<_>>();
            let mut renamed = HashMap::<BindingId, SymbolId>::new();
            for (bid, binding) in res.bindings.iter().enumerate().skip(1) {
                if binding.kind != storm_lua_analysis::resolver::BindingKind::Global
                    && scope_descendant(&res, binding.scope, function_scope)
                {
                    let base = format!("__i{serial}_{bid}");
                    let mut name = base.clone();
                    let mut suffix = 0;
                    while !taken.insert(name.clone()) {
                        suffix += 1;
                        name = format!("{base}_{suffix}");
                    }
                    let symbol = trial.strings.intern(&name);
                    renamed.insert(bid as BindingId, symbol);
                }
            }
            let mut free_references = Vec::new();
            let first_alpha_node = trial.nodes.len();
            let alpha_function = alpha_clone_node(
                &mut trial,
                &source,
                &res,
                info.function,
                &renamed,
                &mut free_references,
            );
            let Node::Function(alpha_params, _, alpha_body) = trial.node(alpha_function).clone()
            else {
                continue;
            };
            let Node::Block(alpha_statements) = trial.node(alpha_body).clone() else {
                continue;
            };
            let alpha_tail = alpha_statements.last().copied();

            let Node::Call(_, call_args, _) = source.node(site.call).clone() else {
                continue;
            };
            // Safety fix mirrored from TS: a zero-parameter function cannot drop
            // evaluation of surplus arguments in tail mode (or statement mode).
            if alpha_params.is_empty() && !call_args.is_empty() {
                continue;
            }
            // Local budget: splicing the callee body into the caller
            // adds the callee's whole local peak (its parameters plus
            // its body locals) on top of the caller's active locals.
            // Skip the fusion when the sum could exceed the VM's 200
            // simultaneous-local limit; both sides use the shared
            // conservative whole-function maximum. (The tail form's
            // result local is the call statement's own declaration,
            // already counted in the caller's maximum.)
            let caller_max = budgets.get(&site.block).copied().unwrap_or(0);
            let callee_peak = super::single_member_cache::max_active_locals(
                &source,
                info.parameters.len(),
                info.body,
            );
            if caller_max + callee_peak > super::single_member_cache::MAX_LOCALS {
                continue;
            }

            if trial.nodes.tracks_origins() {
                for copied in first_alpha_node..trial.nodes.len() {
                    trial
                        .nodes
                        .add_inline_context(copied as NodeId, info.function, site.call);
                    trial.nodes.relate_from_role(
                        copied as NodeId,
                        &source.nodes,
                        site.call,
                        "one-use-function-call-site",
                        storm_lua_syntax::explanation::RelationRole::CallSite,
                    );
                }
            }
            let mut replacement = Vec::<NodeId>::new();
            if !alpha_params.is_empty() {
                let args = call_args;
                let local = trial.push(Node::Local(alpha_params.clone(), args));
                trial.nodes.derive_from(
                    local,
                    &source.nodes,
                    site.statement,
                    "inlined-parameter-binding",
                );
                for index in 0..alpha_params.len() {
                    trial.nodes.copy_name_from(
                        local,
                        storm_lua_syntax::NameSite::Binding(index as u32),
                        &source.nodes,
                        info.function,
                        storm_lua_syntax::NameSite::Parameter(index as u32),
                    );
                }
                replacement.push(local);
            }
            match site.kind {
                FusionKind::Statement => {
                    replacement.extend(alpha_statements.iter().copied());
                }
                FusionKind::TailLocal => {
                    replacement.extend(
                        alpha_statements[..alpha_statements.len().saturating_sub(1)]
                            .iter()
                            .copied(),
                    );
                    let Some(Node::Return(tail_values)) =
                        alpha_tail.map(|tail| trial.node(tail).clone())
                    else {
                        continue;
                    };
                    let Node::Local(names, _) = source.node(site.statement).clone() else {
                        continue;
                    };
                    let statement = trial.push(Node::Local(names, tail_values));
                    trial.nodes.derive_from(
                        statement,
                        &source.nodes,
                        site.statement,
                        "inlined-tail-local",
                    );
                    if let Some(tail) = alpha_tail {
                        trial
                            .nodes
                            .relate_within(statement, tail, "inlined-tail-return");
                    }
                    replacement.push(statement);
                }
                FusionKind::TailAssign => {
                    replacement.extend(
                        alpha_statements[..alpha_statements.len().saturating_sub(1)]
                            .iter()
                            .copied(),
                    );
                    let Some(Node::Return(tail_values)) =
                        alpha_tail.map(|tail| trial.node(tail).clone())
                    else {
                        continue;
                    };
                    let Node::Assign(targets, _) = source.node(site.statement).clone() else {
                        continue;
                    };
                    let statement = trial.push(Node::Assign(targets, tail_values));
                    trial.nodes.derive_from(
                        statement,
                        &source.nodes,
                        site.statement,
                        "inlined-tail-assignment",
                    );
                    if let Some(tail) = alpha_tail {
                        trial
                            .nodes
                            .relate_within(statement, tail, "inlined-tail-return");
                    }
                    replacement.push(statement);
                }
            }
            let Node::Block(mut block_statements) = trial.node(site.block).clone() else {
                continue;
            };
            block_statements.splice(site.index..=site.index, replacement);
            trial.nodes.rewrite(
                site.block,
                Node::Block(block_statements),
                "one-use-function-fusion",
            );
            remove_statement_from_blocks(&mut trial, info.declaration);

            // Alpha-renaming protects the callee's own locals, not its free
            // variables. Resolve the actual inserted references again: the
            // original declaration node + name + kind must still identify the
            // same captured storage (BindingIds are rebuilt after insertion).
            // This also catches caller-local shadowing of an original global.
            if !free_references.is_empty() {
                let relocated = resolve(&trial, root);
                if free_references.iter().any(|&(node, old_bid)| {
                    let Some(new_bid) = relocated.node_bid.get(node as usize).copied().flatten()
                    else {
                        return true;
                    };
                    let (old, new) = (res.binding(old_bid), relocated.binding(new_bid));
                    old.name != new.name || old.kind != new.kind || old.decl_node != new.decl_node
                }) {
                    continue;
                }
            }

            let old_len = measure_size(&source, root);
            let new_len = measure_size(&trial, root);
            let allowance = 4000.0f64.max(old_len as f64 * 0.4);
            if (new_len as f64) < old_len as f64 + allowance {
                *ast = trial;
                details.push(format!(
                    "function={binding_name};mode={};delta={}",
                    if statement_mode { "statement" } else { "tail" },
                    old_len as isize - new_len as isize
                ));
                applied = true;
                break;
            }
        }
        if !applied {
            break;
        }
    }

    PassResult {
        root,
        saved: Some(original_size.saturating_sub(measure_size(ast, root)) as u64),
        details: Some(details),
    }
}

/// Late reversal of a one-use expression helper (`postprocess.ts`).
pub fn inline_one_use_expression_helpers(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
) -> PassResult {
    let before = measure_size(ast, root);
    if ast.strings.contains("_ENV") {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let source = clone_ast(ast);
    let res = resolve(&source, root);
    let infos = function_infos(&source, &res, root);
    let functions = info_map(&infos);
    let effects = EffectAnalyzer::new(&source, &res, root, true);
    let timing = read_timing_index(&source, &res, root);
    let (call_counts, non_call_refs) = function_reference_sets(&source, root, &res, &functions);
    let mut returns = HashMap::<BindingId, NodeId>::new();
    for info in &infos {
        if info.variadic {
            continue;
        }
        let Node::Block(statements) = source.node(info.body) else {
            continue;
        };
        if statements.len() != 1 {
            continue;
        }
        if let Node::Return(expressions) = source.node(statements[0]) {
            if expressions.len() == 1 {
                returns.insert(info.bid, expressions[0]);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rewrite(
        source: &Ast,
        target: &mut Ast,
        res: &Resolution,
        statement_calls: &HashSet<NodeId>,
        effects: &EffectAnalyzer<'_>,
        timing: &ReadTimingIndex,
        functions: &HashMap<BindingId, FunctionInfo>,
        returns: &HashMap<BindingId, NodeId>,
        call_counts: &HashMap<BindingId, usize>,
        non_call_refs: &HashSet<BindingId>,
        node: NodeId,
        aggressive: bool,
        origins: &mut ReferenceOrigins,
        inlined: &mut HashSet<BindingId>,
    ) {
        let mut children = Vec::new();
        storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| {
            children.push(child)
        });
        for child in children {
            rewrite(
                source,
                target,
                res,
                statement_calls,
                effects,
                timing,
                functions,
                returns,
                call_counts,
                non_call_refs,
                child,
                aggressive,
                origins,
                inlined,
            );
        }
        let Some(bid) = direct_function_bid(source, res, node) else {
            return;
        };
        if call_counts.get(&bid).copied().unwrap_or(0) != 1 || non_call_refs.contains(&bid) {
            return;
        }
        let (Some(info), Some(return_expr)) = (functions.get(&bid), returns.get(&bid)) else {
            return;
        };
        let Node::Call(_, arguments, _) = target.node(node).clone() else {
            return;
        };
        if arguments.len() != info.parameters.len() {
            return;
        }
        if arguments.len() > 1
            && arguments
                .iter()
                .any(|argument| !is_literal(target, *argument))
        {
            // Keep left-to-right Lua argument evaluation when the helper body
            // uses parameters in a reordered expression.
            return;
        }
        // Read-timing gate (T02), as in `rewrite_expression_helpers`.
        if !inlined_body_timing_safe(
            source,
            res,
            effects,
            *return_expr,
            &info.parameter_bids,
            timing.enclosing[node as usize],
            timing,
        ) {
            return;
        }
        let mut replacements = HashMap::new();
        for (index, parameter_bid) in info.parameter_bids.iter().copied().enumerate() {
            let uses = count_param_uses(source, res, *return_expr, parameter_bid);
            let source_argument = match source.node(node) {
                Node::Call(_, source_arguments, _) => source_arguments[index],
                _ => return,
            };
            let duplicable = if effects.is_movable(source_argument) {
                true
            } else if aggressive {
                let effect = effects.effects_for_expr(source_argument);
                !effect.calls && !effect.ordered && effect.writes.is_empty() && effect.stable
            } else {
                false
            };
            if uses > 1 && !duplicable {
                return;
            }
            if uses == 0 && !effects.is_droppable(source_argument) {
                return;
            }
            replacements.insert(parameter_bid, arguments[index]);
        }
        if subtree_has_replaced_funcstat_root(source, res, *return_expr, &replacements) {
            return;
        }
        let replacement = clone_subtree_with_replacements(
            target,
            source,
            res,
            *return_expr,
            &replacements,
            origins,
        );
        // A statement call may only become another call; anything else
        // would print as a bare expression statement and fail to reparse.
        if statement_calls.contains(&node) && !matches!(target.node(replacement), Node::Call(..)) {
            return;
        }
        install_expression(target, node, replacement, origins);
        inlined.insert(bid);
    }

    let mut target = clone_ast(&source);
    let mut origins = reference_origins(&res);
    let mut inlined = HashSet::new();
    rewrite(
        &source,
        &mut target,
        &res,
        &statement_call_nodes(&source, root),
        &effects,
        &timing,
        &functions,
        &returns,
        &call_counts,
        &non_call_refs,
        root,
        aggressive,
        &mut origins,
        &mut inlined,
    );
    for bid in &inlined {
        if let Some(info) = functions.get(bid) {
            remove_statement_from_blocks(&mut target, info.declaration);
        }
    }
    let after = measure_size(&target, root);
    if after < before && expression_bindings_preserved(&target, root, &res, &origins) {
        *ast = target;
        PassResult {
            root,
            saved: Some((before - after) as u64),
            details: Some(vec![format!("inlined={}", inlined.len())]),
        }
    } else {
        PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["inlined=0".to_string()]),
        }
    }
}
