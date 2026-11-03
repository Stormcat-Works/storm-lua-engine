//! non-escaping ルートテーブル名前空間への unread フィールド nil ストア除去
//! （TS `passes/tables.ts` の `removeWriteOnlyNilTableFields`）。

use crate::pass::PassResult;
use storm_lua_analysis::resolver::{resolve, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::numeric::decode_lua_string;

pub fn remove_write_only_nil_table_fields(ast: &mut Ast, root: NodeId) -> PassResult {
    let res = resolve(ast, root);
    let (new_ast, new_root, removed) = impl_remove(ast, &res, root);
    ast.nodes = new_ast.nodes;
    PassResult {
        root: new_root,
        saved: Some(removed as u64),
        details: None,
    }
}

/// 親コンテキスト（TS の parent + parentKey の要約）。
#[derive(Clone, Copy, PartialEq)]
enum Ctx {
    Root,     // 文のルート（親無し）
    AssignVs, // assign.vs
    AssignEs, // assign.es
    LocalEs,  // local.es
    IndexObj, // index.obj
    IndexKey, // index.key
    Other,    // それ以外
}

fn is_name(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(id), Node::Name(_))
}
fn bid_of(res: &Resolution, id: NodeId) -> Option<u32> {
    res.node_bid.get(id as usize).copied().flatten()
}

fn impl_remove(ast: &Ast, res: &Resolution, root: NodeId) -> (Ast, NodeId, usize) {
    let Node::Block(ss) = &ast.nodes[root as usize] else {
        let mut new_ast = storm_lua_syntax::ast_utils::inherit_ast(ast);
        new_ast.nodes = ast.nodes.clone();
        return (new_ast, root, 0);
    };
    // roots: bid → 初期化 statement index（initializer 文）
    let mut roots: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for (index, &sid) in ss.iter().enumerate() {
        match ast.node(sid) {
            Node::Local(ns, es) => {
                for (at, _nid) in ns.iter().enumerate() {
                    if let Some(&col) = es.get(at) {
                        if matches!(ast.node(col), Node::Table(_)) {
                            if let Some(bid) = res
                                .node_bids
                                .get(sid as usize)
                                .and_then(|bs| bs.get(at))
                                .copied()
                            {
                                roots.insert(bid, index);
                            }
                        }
                    }
                }
            }
            Node::Assign(vs, es)
                if vs.len() == 1
                    && is_name(ast, vs[0])
                    && es.len() == 1
                    && matches!(ast.node(es[0]), Node::Table(_)) =>
            {
                if let Some(bid) = bid_of(res, vs[0]) {
                    roots.insert(bid, index);
                }
            }
            _ => {}
        }
    }
    if roots.is_empty() {
        let mut new_ast = storm_lua_syntax::ast_utils::inherit_ast(ast);
        new_ast.nodes = ast.nodes.clone();
        return (new_ast, root, 0);
    }

    let mut bad: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut reads: std::collections::HashMap<u32, std::collections::HashSet<Vec<u8>>> =
        std::collections::HashMap::new();
    for &bid in roots.keys() {
        reads.insert(bid, std::collections::HashSet::new());
    }

    // scan(node, ctx, top): TS scan(node, parent, parentKey, topStatement)
    // TS の走査コンテキストを明示的に対応付けるため、引数は個別に保持する。
    #[allow(clippy::too_many_arguments)]
    fn scan(
        ast: &Ast,
        res: &Resolution,
        id: NodeId,
        ctx: Ctx,
        top: usize,
        roots: &std::collections::HashMap<u32, usize>,
        bad: &mut std::collections::HashSet<u32>,
        reads: &mut std::collections::HashMap<u32, std::collections::HashSet<Vec<u8>>>,
    ) {
        let node = ast.node(id).clone();
        match &node {
            Node::Index(obj, key, _) => {
                if is_name(ast, *obj) {
                    if let Some(obid) = bid_of(res, *obj) {
                        if roots.contains_key(&obid) {
                            // key が文字列でなければ bad
                            if !matches!(ast.node(*key), Node::Str(_)) {
                                bad.insert(obid);
                            } else if !matches!(ctx, Ctx::AssignVs) {
                                if let Node::Str(s) = ast.node(*key) {
                                    #[expect(
                                        clippy::unwrap_used,
                                        reason = "The reads map is initialized for every root binding before scanning; root membership was checked above"
                                    )]
                                    reads.get_mut(&obid).unwrap().insert(
                                        storm_lua_syntax::source_tools::string_bytes(s).unwrap(),
                                    );
                                }
                            }
                            // key のみ訪問（parent=index, parentKey=key）
                            scan(ast, res, *key, Ctx::IndexKey, top, roots, bad, reads);
                            return;
                        }
                    }
                }
            }
            Node::Name(_) => {
                if let Some(b) = bid_of(res, id) {
                    if roots.contains_key(&b) {
                        let initializer = roots[&b];
                        let allowed = matches!(ctx, Ctx::IndexObj)
                            || (top == initializer
                                && matches!(ctx, Ctx::LocalEs | Ctx::AssignEs | Ctx::AssignVs));
                        if !allowed {
                            bad.insert(b);
                        }
                    }
                }
            }
            _ => {}
        }
        // 子を keyed で訪問（TS Object.entries と同順）
        let children = {
            let mut out = Vec::new();
            storm_lua_syntax::ast_utils::for_each_child_key(ast, id, &mut |k, c| out.push((k, c)));
            out
        };
        for (k, cid) in children {
            let cctx = match (k, &node) {
                ("obj", Node::Index(..)) => Ctx::IndexObj,
                ("vs", Node::Assign(..)) => Ctx::AssignVs,
                ("es", Node::Assign(..)) => Ctx::AssignEs,
                ("es", Node::Local(..)) => Ctx::LocalEs,
                _ => Ctx::Other,
            };
            scan(ast, res, cid, cctx, top, roots, bad, reads);
        }
    }

    for (index, &sid) in ss.iter().enumerate() {
        scan(
            ast,
            res,
            sid,
            Ctx::Root,
            index,
            &roots,
            &mut bad,
            &mut reads,
        );
    }

    // writeOnlyNilTarget
    #[expect(
        clippy::unwrap_used,
        reason = "Every root binding has a reads entry and the closure checks root membership before lookup"
    )]
    let write_only_target = |target: NodeId| -> bool {
        let Node::Index(obj, key, _) = ast.node(target) else {
            return false;
        };
        if !is_name(ast, *obj) {
            return false;
        }
        let Some(bid) = bid_of(res, *obj) else {
            return false;
        };
        if !roots.contains_key(&bid) || bad.contains(&bid) {
            return false;
        }
        let Node::Str(s) = ast.node(*key) else {
            return false;
        };
        !reads
            .get(&bid)
            .unwrap()
            .contains(&storm_lua_syntax::source_tools::string_bytes(s).unwrap())
    };

    // transform: block 文は assign=nil のフィールドを落とす
    fn transform(
        new_ast: &mut Ast,
        ast: &Ast,
        id: NodeId,
        write_only: &dyn Fn(NodeId) -> bool,
        removed: &mut usize,
    ) -> NodeId {
        let node = ast.node(id).clone();
        if let Node::Block(ss) = &node {
            let mut out = Vec::new();
            for &sid in ss {
                let stmt = ast.node(sid).clone();
                let is_nil_assign = match &stmt {
                    Node::Assign(vs, es) => {
                        let _ = vs;
                        es.len() == 1 && matches!(ast.node(es[0]), Node::Nil)
                    }
                    _ => false,
                };
                if !is_nil_assign {
                    out.push(transform(new_ast, ast, sid, write_only, removed));
                    continue;
                }
                let types = match &stmt {
                    Node::Assign(vs, _) => vs.clone(),
                    _ => unreachable!(),
                };
                let kept: Vec<NodeId> = types
                    .into_iter()
                    .filter(|t| {
                        let drop = write_only(*t);
                        if drop {
                            *removed += 1;
                        }
                        !drop
                    })
                    .collect();
                if kept.is_empty() {
                    continue;
                }
                let new_vs: Vec<NodeId> = kept
                    .iter()
                    .map(|&t| transform(new_ast, ast, t, write_only, removed))
                    .collect();
                let nil_id = new_ast.push(Node::Nil);
                let Node::Assign(_, values) = &stmt else {
                    unreachable!()
                };
                new_ast.nodes.derive_from(
                    nil_id,
                    &ast.nodes,
                    values[0],
                    "write-only-field-nil-copy",
                );
                let assign = new_ast.push(Node::Assign(new_vs, vec![nil_id]));
                new_ast
                    .nodes
                    .derive_from(assign, &ast.nodes, sid, "write-only-field-cleanup");
                out.push(assign);
            }
            new_ast
                .nodes
                .rewrite(id, Node::Block(out), "write-only-field-cleanup");
            id
        } else {
            let mut rewriter = |c: NodeId| transform(new_ast, ast, c, write_only, removed);
            let (new_node, _) = storm_lua_syntax::ast_utils::map_children(&node, &mut rewriter);
            new_ast
                .nodes
                .rewrite(id, new_node, "write-only-field-cleanup");
            id
        }
    }

    let mut new_ast = storm_lua_syntax::ast_utils::inherit_ast(ast);
    new_ast.nodes = ast.nodes.clone();
    let mut removed = 0usize;
    let new_root = transform(&mut new_ast, ast, root, &write_only_target, &mut removed);
    (new_ast, new_root, removed)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod write_only_field_tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    #[test]
    fn global_table_initializer_target_is_not_escape() {
        let source = "onoff={} onoff.ch1=nil onoff.ch2=nil";
        let (mut ast, root) = parse_source(source).expect("parse");
        let out = remove_write_only_nil_table_fields(&mut ast, root);
        assert_eq!(Printer::new(&ast, false).output(out.root), "onoff={}");
    }
}

// ---------------------------------------------------------------------------
// Phase 4c table scalarization/flattening passes
// ---------------------------------------------------------------------------

use crate::scope_rename::measure_renamed_size;
use storm_lua_syntax::ast::{SymbolId, TableField};
use storm_lua_syntax::numeric::num_val;
use storm_lua_syntax::size::measure_size;

fn clone_table_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn renamed_table_size(ast: &Ast, root: NodeId) -> usize {
    measure_renamed_size(ast, root)
}

#[derive(Clone, Debug, PartialEq)]
enum StaticTableKey {
    Str(Vec<u8>),
    Int(i64),
    Num(f64),
}

fn static_table_key(ast: &Ast, node: NodeId) -> Option<StaticTableKey> {
    match ast.node(node) {
        Node::Str(value) => storm_lua_syntax::source_tools::string_bytes(value)
            .ok()
            .map(StaticTableKey::Str),
        Node::Num(value) => {
            if let Some(integer) = storm_lua_syntax::numeric::integer_literal_value(value) {
                return Some(StaticTableKey::Int(integer));
            }
            let value = num_val(value);
            if value.fract() == 0.0
                && (-9223372036854775808.0..9223372036854775808.0).contains(&value)
            {
                Some(StaticTableKey::Int(value as i64))
            } else if !value.is_nan() {
                Some(StaticTableKey::Num(value))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// A constructor entry only evaluates without raising when its key is
/// neither nil nor NaN: inserting such a key raises "table index is
/// nil" / "table index is NaN". A key is provably valid when it has a
/// static key form (string / integer / non-NaN number) or is a boolean
/// literal. Any other expression — a name, a compound expression whose
/// value is not a static key — may be nil or NaN at runtime (e.g. a
/// local bound to `0 / 0`), so a table containing it does not evaluate
/// purely and must not be flattened away.
fn key_is_valid_table_key(ast: &Ast, key: NodeId) -> bool {
    static_table_key(ast, key).is_some() || matches!(ast.node(key), Node::Bool(_))
}

fn table_literal_pure(ast: &Ast, node: NodeId) -> bool {
    match ast.node(node) {
        Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil => true,
        // A compound expression over literals can still raise when
        // the constructor evaluates it: arithmetic or concatenation
        // on nil, integer division or modulo by zero, arithmetic on
        // a non-numeric string, length of nil, comparing nil with a
        // number, and so on. Any compound value the constant folder
        // could prove finite has already been materialized into a
        // literal before this pass runs in the pipeline, so a Bin/Un
        // value that survives to here is one whose evaluation is not
        // provably safe: the table is not a pure literal.
        Node::Un(..) | Node::Bin(..) => false,
        Node::Table(fields) => {
            // Lua flushes array entries with SETLIST in batches; textual order
            // does not determine the winner of mixed array/index duplicates.
            let array_len = fields
                .iter()
                .filter(|field| matches!(field, TableField::Arr(_)))
                .count() as i64;
            fields.iter().all(|field| match field {
                TableField::Arr(value) | TableField::Name(_, value) => table_literal_pure(ast, *value),
                TableField::KVar(key, value) => {
                    key_is_valid_table_key(ast, *key)
                        && !matches!(static_table_key(ast, *key), Some(StaticTableKey::Int(index)) if (1..=array_len).contains(&index))
                        && table_literal_pure(ast, *key) && table_literal_pure(ast, *value)
                }
            })
        }
        _ => false,
    }
}

// Read-only lookup: an absent entry must not allocate a Nil in a scratch
// arena (which would clone the entire COW AST on every miss).
fn table_get(ast: &Ast, table: NodeId, key: &StaticTableKey) -> Option<NodeId> {
    let Node::Table(fields) = ast.node(table) else {
        return None;
    };
    let mut array_index = 1i64;
    let mut found = None;
    for field in fields {
        match field {
            TableField::Arr(value) => {
                if matches!(key, StaticTableKey::Int(value_key) if *value_key == array_index) {
                    found = Some(*value);
                }
                array_index += 1;
            }
            TableField::Name(name, value) => {
                if matches!(key, StaticTableKey::Str(value_key) if value_key == ast.strings.get(*name).as_bytes())
                {
                    found = Some(*value);
                }
            }
            TableField::KVar(field_key, value) => {
                if static_table_key(ast, *field_key).as_ref() == Some(key) {
                    found = Some(*value);
                }
            }
        }
    }
    found
}

fn static_index_chain(
    ast: &Ast,
    res: &Resolution,
    mut node: NodeId,
) -> Option<(u32, Vec<StaticTableKey>)> {
    let mut keys = Vec::new();
    loop {
        match ast.node(node) {
            Node::Index(object, key, _) => {
                keys.push(static_table_key(ast, *key)?);
                node = *object;
            }
            Node::Name(_) if !keys.is_empty() => {
                let bid = bid_of(res, node)?;
                keys.reverse();
                return Some((bid, keys));
            }
            _ => return None,
        }
    }
}

fn index_root_info(ast: &Ast, res: &Resolution, mut node: NodeId) -> Option<(u32, bool)> {
    let mut saw = false;
    let mut all_static = true;
    while let Node::Index(object, key, _) = ast.node(node) {
        saw = true;
        if static_table_key(ast, *key).is_none() {
            all_static = false;
        }
        node = *object;
    }
    if saw && matches!(ast.node(node), Node::Name(_)) {
        Some((bid_of(res, node)?, all_static))
    } else {
        None
    }
}

#[derive(Clone)]
struct ImmutableTableInit {
    statement_index: usize,
    table: NodeId,
    kind_local: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum TableScanCtx {
    Other,
    IndexObj,
    AssignTarget,
}

/// Whether `rewrite_static_table_chains` can replace a read of this
/// chain: the walk must end at a scalar literal (copied into the read
/// site) or miss at the final key (replaced by nil). A table/function
/// value is substitutable only as the object of an enclosing index,
/// whose own chain is checked separately. Every other outcome -- a
/// compound pure value such as `0 / 0` (whose non-finite result the
/// constant folder refuses to materialize, so it survives as an
/// expression), or a missing intermediate key -- leaves the read in
/// place, so removing the binding would leave the read dangling
/// against a nil global of the same name.
fn static_chain_substitutable(
    ast: &Ast,
    table: NodeId,
    keys: &[StaticTableKey],
    parent_ctx: TableScanCtx,
) -> bool {
    let mut current = table;
    for (index, key) in keys.iter().enumerate() {
        if !matches!(ast.node(current), Node::Table(_)) {
            return false;
        }
        match table_get(ast, current, key) {
            Some(value) => current = value,
            None => return index + 1 == keys.len(),
        }
    }
    match ast.node(current) {
        Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil => true,
        Node::Table(_) | Node::Function(..) => parent_ctx == TableScanCtx::IndexObj,
        _ => false,
    }
}

fn scan_immutable_table_uses(
    ast: &Ast,
    res: &Resolution,
    node: NodeId,
    parent_ctx: TableScanCtx,
    top_statement: usize,
    init: &std::collections::HashMap<u32, ImmutableTableInit>,
    bad: &mut std::collections::HashSet<u32>,
) {
    if let Some((bid, all_static)) = index_root_info(ast, res, node) {
        if init.contains_key(&bid) && !all_static {
            bad.insert(bid);
        }
    }
    if let Node::Name(_) = ast.node(node) {
        if let Some(bid) = bid_of(res, node) {
            if let Some(info) = init.get(&bid) {
                // A global init only takes effect when its statement
                // executes. A use that textually precedes the init (or
                // sits in the init statement's own right-hand side,
                // which evaluates before the assignment) still sees the
                // previous value on first execution, so folding it to
                // the init literal changes meaning. Locals cannot be
                // referenced before their declaration under the same bid,
                // so only globals need this order guard; uses inside
                // functions defined before the init are conservatively
                // rejected too (they may run before the init).
                let allowed = (parent_ctx == TableScanCtx::IndexObj
                    && (info.kind_local || top_statement > info.statement_index))
                    || (top_statement == info.statement_index
                        && !info.kind_local
                        && parent_ctx == TableScanCtx::AssignTarget);
                if !allowed {
                    bad.insert(bid);
                }
            }
        }
    }
    if let Some((bid, _)) = static_index_chain(ast, res, node) {
        if init.contains_key(&bid) && parent_ctx == TableScanCtx::AssignTarget {
            bad.insert(bid);
        }
    }
    if let Some((bid, keys)) = static_index_chain(ast, res, node) {
        if let Some(info) = init.get(&bid) {
            if !static_chain_substitutable(ast, info.table, &keys, parent_ctx) {
                bad.insert(bid);
            }
        }
    }

    match ast.node(node) {
        Node::Assign(targets, expressions) => {
            for target in targets {
                scan_immutable_table_uses(
                    ast,
                    res,
                    *target,
                    TableScanCtx::AssignTarget,
                    top_statement,
                    init,
                    bad,
                );
            }
            for expression in expressions {
                scan_immutable_table_uses(
                    ast,
                    res,
                    *expression,
                    TableScanCtx::Other,
                    top_statement,
                    init,
                    bad,
                );
            }
        }
        Node::Funcstat(target, function) => {
            scan_immutable_table_uses(
                ast,
                res,
                *target,
                TableScanCtx::AssignTarget,
                top_statement,
                init,
                bad,
            );
            scan_immutable_table_uses(
                ast,
                res,
                *function,
                TableScanCtx::Other,
                top_statement,
                init,
                bad,
            );
        }
        Node::Methodname(object, _) => {
            // function t:method() writes through t even though the member is a symbol.
            scan_immutable_table_uses(
                ast,
                res,
                *object,
                TableScanCtx::AssignTarget,
                top_statement,
                init,
                bad,
            );
        }
        Node::Index(object, key, _) => {
            scan_immutable_table_uses(
                ast,
                res,
                *object,
                TableScanCtx::IndexObj,
                top_statement,
                init,
                bad,
            );
            scan_immutable_table_uses(
                ast,
                res,
                *key,
                TableScanCtx::Other,
                top_statement,
                init,
                bad,
            );
        }
        _ => {
            let mut children = Vec::new();
            storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
                children.push(child)
            });
            for child in children {
                scan_immutable_table_uses(
                    ast,
                    res,
                    child,
                    TableScanCtx::Other,
                    top_statement,
                    init,
                    bad,
                );
            }
        }
    }
}

fn rewrite_static_table_chains(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    candidates: &std::collections::HashSet<u32>,
    init: &std::collections::HashMap<u32, ImmutableTableInit>,
) {
    if let Some((bid, keys)) = static_index_chain(source, res, node) {
        if candidates.contains(&bid) {
            let mut current = init[&bid].table;
            let mut ok = true;
            for (index, key) in keys.iter().enumerate() {
                if !matches!(source.node(current), Node::Table(_)) {
                    ok = false;
                    break;
                }
                if let Some(value) = table_get(source, current, key) {
                    current = value;
                } else {
                    if index + 1 != keys.len() {
                        // Missing intermediate values still raise an indexing error.
                        ok = false;
                        break;
                    }
                    target
                        .nodes
                        .rewrite(node, Node::Nil, "immutable-table-missing-key");
                    target.nodes.relate_from(
                        node,
                        &source.nodes,
                        init[&bid].table,
                        "immutable-table-missing-key",
                    );
                    return;
                }
            }
            if ok {
                // Do not duplicate fresh reference values: two replacements
                // of a nested table would become two distinct Lua tables and
                // change `t.item == t.item`. Scalar leaves are safe to copy.
                if matches!(
                    source.node(current),
                    Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
                ) {
                    target.nodes[node as usize] = source.node(current).clone();
                    target.nodes.derive_from(
                        node,
                        &source.nodes,
                        current,
                        "immutable-table-element",
                    );
                    target
                        .nodes
                        .relate_from(node, &source.nodes, node, "immutable-table-access");
                    return;
                }
            }
        }
    }
    let original = source.node(node).clone();
    let mut children = Vec::new();
    storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| children.push(child));
    for child in children {
        rewrite_static_table_chains(target, source, res, child, candidates, init);
    }
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| child);
    // Preserve already rewritten child slots and their source occurrences.
    target
        .nodes
        .rewrite(node, mapped, "immutable-table-flattening");
}

/// Syntactic pre-filter for immutable-table flattening: a block can only
/// contribute an `init` entry when one of its direct statements binds a table
/// literal (a `local` with matching arity, or an assignment pairing a name
/// target with a table literal). Blocks without such a statement always break
/// out of the round loop with an empty `init`, so cloning and resolving them
/// is pure overhead -- including one tracked-only allocation per block for
/// the cloned origin table. The condition is necessary, not sufficient
/// (purity and binding ids may still leave `init` empty), so skipping a block
/// here can never change the outcome.
fn block_has_table_binding(ast: &Ast, root: NodeId) -> bool {
    let Node::Block(statements) = ast.node(root) else {
        return false;
    };
    statements
        .iter()
        .copied()
        .any(|statement| match ast.node(statement) {
            Node::Local(names, expressions) => {
                names.len() == expressions.len()
                    && expressions
                        .iter()
                        .any(|expression| matches!(ast.node(*expression), Node::Table(_)))
            }
            Node::Assign(targets, expressions) => {
                targets.len() == expressions.len()
                    && targets
                        .iter()
                        .zip(expressions.iter())
                        .any(|(target, expression)| {
                            matches!(ast.node(*target), Node::Name(_))
                                && matches!(ast.node(*expression), Node::Table(_))
                        })
            }
            _ => false,
        })
}

fn flatten_immutable_tables_in_block(ast: &mut Ast, root: NodeId, max_rounds: usize) {
    for _ in 0..max_rounds {
        if !block_has_table_binding(ast, root) {
            break;
        }
        let source = clone_table_ast(ast);
        let res = resolve(&source, root);
        let Node::Block(statements) = source.node(root).clone() else {
            return;
        };
        let mut init = std::collections::HashMap::<u32, ImmutableTableInit>::new();
        for (statement_index, statement) in statements.iter().copied().enumerate() {
            match source.node(statement) {
                Node::Local(names, expressions) if names.len() == expressions.len() => {
                    let bids = res
                        .node_bids
                        .get(statement as usize)
                        .cloned()
                        .unwrap_or_default();
                    for (part, bid) in bids.into_iter().enumerate() {
                        if let Some(expression) = expressions.get(part).copied() {
                            if matches!(source.node(expression), Node::Table(_))
                                && table_literal_pure(&source, expression)
                            {
                                init.insert(
                                    bid,
                                    ImmutableTableInit {
                                        statement_index,
                                        table: expression,
                                        kind_local: true,
                                    },
                                );
                            }
                        }
                    }
                }
                // Globals are never candidates: a global binding can be read
                // or written from any block (other functions, outer blocks,
                // later ticks), which this per-block scan cannot see, so
                // neither folding its uses nor removing its binding is sound.
                // (A global assigned in one function and read in another was
                // miscompiled: the defining block removed the binding while
                // the reading block still referenced it.)
                Node::Assign(..) => {}
                _ => {}
            }
        }
        if init.is_empty() {
            break;
        }
        let mut bad = std::collections::HashSet::new();
        for (index, statement) in statements.iter().copied().enumerate() {
            scan_immutable_table_uses(
                &source,
                &res,
                statement,
                TableScanCtx::Other,
                index,
                &init,
                &mut bad,
            );
        }
        // `self` is never a candidate: occurrences of the name inside a
        // method body resolve to this binding but denote the implicit
        // receiver parameter, so folding those uses to the table's
        // literal fields would substitute this table's values for the
        // receiver's (candidate AA, w9 cycle 68).
        let candidates = init
            .keys()
            .copied()
            .filter(|bid| {
                !bad.contains(bid) && source.strings.get(res.bindings[*bid as usize].name) != "self"
            })
            .collect::<std::collections::HashSet<_>>();
        if candidates.is_empty() {
            break;
        }

        let old_len = renamed_table_size(&source, root);
        let mut trial = clone_table_ast(&source);
        rewrite_static_table_chains(&mut trial, &source, &res, root, &candidates, &init);
        let mut out = Vec::new();
        for statement in statements {
            match source.node(statement).clone() {
                Node::Local(names, expressions) if names.len() == expressions.len() => {
                    let bids = res
                        .node_bids
                        .get(statement as usize)
                        .cloned()
                        .unwrap_or_default();
                    let keep = bids
                        .iter()
                        .enumerate()
                        .filter_map(|(part, bid)| (!candidates.contains(bid)).then_some(part))
                        .collect::<Vec<_>>();
                    if !keep.is_empty() {
                        trial.nodes.rewrite(
                            statement,
                            Node::Local(
                                keep.iter().map(|part| names[*part]).collect(),
                                keep.iter().map(|part| expressions[*part]).collect(),
                            ),
                            "immutable-table-binding-removal",
                        );
                        for (new_index, &old_index) in keep.iter().enumerate() {
                            trial.nodes.copy_name_from(
                                statement,
                                storm_lua_syntax::NameSite::Binding(new_index as u32),
                                &source.nodes,
                                statement,
                                storm_lua_syntax::NameSite::Binding(old_index as u32),
                            );
                        }
                        out.push(statement);
                    }
                }
                Node::Assign(targets, expressions) if targets.len() == expressions.len() => {
                    let keep = targets
                        .iter()
                        .enumerate()
                        .filter_map(|(part, target)| {
                            let remove = matches!(source.node(*target), Node::Name(_))
                                && bid_of(&res, *target)
                                    .is_some_and(|bid| candidates.contains(&bid));
                            (!remove).then_some(part)
                        })
                        .collect::<Vec<_>>();
                    if !keep.is_empty() {
                        trial.nodes.rewrite(
                            statement,
                            Node::Assign(
                                keep.iter().map(|part| targets[*part]).collect(),
                                keep.iter().map(|part| expressions[*part]).collect(),
                            ),
                            "immutable-table-assignment-removal",
                        );
                        out.push(statement);
                    }
                }
                _ => out.push(statement),
            }
        }
        trial
            .nodes
            .rewrite(root, Node::Block(out), "table-scalarization");
        let new_len = renamed_table_size(&trial, root);
        if new_len >= old_len {
            break;
        }
        *ast = trial;
    }
}

fn process_immutable_blocks(ast: &mut Ast, node: NodeId, max_rounds: usize) {
    let original = ast.node(node).clone();
    let mut children = Vec::new();
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| children.push(child));
    for child in children {
        process_immutable_blocks(ast, child, max_rounds);
    }
    if matches!(original, Node::Block(_)) {
        flatten_immutable_tables_in_block(ast, node, max_rounds);
    }
}

pub fn flatten_immutable_tables(ast: &mut Ast, root: NodeId, max_rounds: usize) -> PassResult {
    let original = measure_size(ast, root);
    process_immutable_blocks(ast, root, max_rounds);
    PassResult {
        root,
        saved: Some(original.saturating_sub(measure_size(ast, root)) as u64),
        details: None,
    }
}

#[derive(Clone)]
struct ClosedNamespaceInit {
    statement_index: usize,
    kind_local: bool,
    table: NodeId,
}

#[allow(clippy::too_many_arguments)]
fn scan_closed_namespace(
    ast: &Ast,
    res: &Resolution,
    node: NodeId,
    parent_ctx: TableScanCtx,
    top_statement: usize,
    init: &std::collections::HashMap<u32, ClosedNamespaceInit>,
    fields: &mut std::collections::HashMap<u32, Vec<String>>,
    bad: &mut std::collections::HashSet<u32>,
) {
    if let Node::Index(object, key, _) = ast.node(node) {
        if matches!(ast.node(*object), Node::Name(_)) {
            if let Some(bid) = bid_of(res, *object) {
                if init.contains_key(&bid) {
                    // The initializer may not have run yet when this access
                    // executes: an access in an earlier top-level statement
                    // runs before the initializer, and a function defined
                    // earlier may be called before it too. Rewriting such an
                    // access to a scalar would turn a nil-table error (or an
                    // older value) into a nil scalar read, so reject the
                    // candidate.
                    if let Some(info) = init.get(&bid) {
                        if top_statement < info.statement_index {
                            bad.insert(bid);
                        }
                    }
                    if let Node::Str(value) = ast.node(*key) {
                        let Some(value) = decode_lua_string(value) else {
                            bad.insert(bid);
                            return;
                        };
                        let list = fields.entry(bid).or_default();
                        if !list.contains(&value) {
                            list.push(value);
                        }
                    } else {
                        bad.insert(bid);
                    }
                    scan_closed_namespace(
                        ast,
                        res,
                        *key,
                        TableScanCtx::Other,
                        top_statement,
                        init,
                        fields,
                        bad,
                    );
                    return;
                }
            }
        }
    }
    if let Node::Name(_) = ast.node(node) {
        if let Some(bid) = bid_of(res, node) {
            if let Some(info) = init.get(&bid) {
                let allowed = parent_ctx == TableScanCtx::IndexObj
                    || (top_statement == info.statement_index && !info.kind_local);
                if !allowed {
                    bad.insert(bid);
                }
            }
        }
    }
    match ast.node(node) {
        Node::Assign(targets, expressions) => {
            for target in targets {
                scan_closed_namespace(
                    ast,
                    res,
                    *target,
                    TableScanCtx::AssignTarget,
                    top_statement,
                    init,
                    fields,
                    bad,
                );
            }
            for expression in expressions {
                scan_closed_namespace(
                    ast,
                    res,
                    *expression,
                    TableScanCtx::Other,
                    top_statement,
                    init,
                    fields,
                    bad,
                );
            }
        }
        Node::Index(object, key, _) => {
            scan_closed_namespace(
                ast,
                res,
                *object,
                TableScanCtx::IndexObj,
                top_statement,
                init,
                fields,
                bad,
            );
            scan_closed_namespace(
                ast,
                res,
                *key,
                TableScanCtx::Other,
                top_statement,
                init,
                fields,
                bad,
            );
        }
        _ => {
            let mut children = Vec::new();
            storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
                children.push(child)
            });
            for child in children {
                scan_closed_namespace(
                    ast,
                    res,
                    child,
                    TableScanCtx::Other,
                    top_statement,
                    init,
                    fields,
                    bad,
                );
            }
        }
    }
}

fn sanitize_field(field: &str) -> String {
    field
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn namespace_table_values_safe(ast: &Ast, table: NodeId) -> bool {
    let Node::Table(fields) = ast.node(table) else {
        return false;
    };
    fields.iter().all(|field| {
        let value = match field {
            TableField::Arr(value) | TableField::Name(_, value) => *value,
            TableField::KVar(_, value) => *value,
        };
        // The namespace itself is removed; each selected value is evaluated
        // once in a synthesized scalar assignment.  Nested table/function
        // values must stay in the original table to preserve fresh identity.
        !super::immutable_values::contains_fresh_reference(ast, value)
    })
}

fn collect_chunk_labels(
    ast: &Ast,
    node: NodeId,
    statement_index: usize,
    labels: &mut std::collections::HashMap<storm_lua_syntax::ast::SymbolId, usize>,
) {
    if let Node::Label(name) = ast.node(node) {
        labels.entry(*name).or_insert(statement_index);
    }
    if matches!(ast.node(node), Node::Function(..)) {
        return;
    }
    let mut children = Vec::new();
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        children.push(child);
    });
    for child in children {
        collect_chunk_labels(ast, child, statement_index, labels);
    }
}

fn chunk_prefix_skips_initializer(
    ast: &Ast,
    node: NodeId,
    init_index: usize,
    labels: &std::collections::HashMap<storm_lua_syntax::ast::SymbolId, usize>,
) -> bool {
    match ast.node(node) {
        Node::Return(_) => return true,
        Node::Goto(name) => {
            if let Some(label_index) = labels.get(name) {
                if *label_index > init_index {
                    return true;
                }
            }
        }
        Node::Function(..) => return false,
        _ => {}
    }
    let mut children = Vec::new();
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        children.push(child);
    });
    children
        .iter()
        .any(|child| chunk_prefix_skips_initializer(ast, *child, init_index, labels))
}

pub fn scalarize_closed_namespaces(ast: &mut Ast, root: NodeId, max_rounds: usize) -> PassResult {
    let original_size = measure_size(ast, root);
    for _ in 0..max_rounds {
        let source = clone_table_ast(ast);
        let res = resolve(&source, root);
        let Node::Block(statements) = source.node(root).clone() else {
            break;
        };
        let mut init = std::collections::HashMap::<u32, ClosedNamespaceInit>::new();
        for (statement_index, statement) in statements.iter().copied().enumerate() {
            match source.node(statement) {
                Node::Local(_names, expressions) => {
                    let bids = res
                        .node_bids
                        .get(statement as usize)
                        .cloned()
                        .unwrap_or_default();
                    for (part, bid) in bids.into_iter().enumerate() {
                        if let Some(expression) = expressions.get(part).copied() {
                            if matches!(source.node(expression), Node::Table(_))
                                && table_literal_pure(&source, expression)
                                && namespace_table_values_safe(&source, expression)
                            {
                                init.insert(
                                    bid,
                                    ClosedNamespaceInit {
                                        statement_index,
                                        kind_local: true,
                                        table: expression,
                                    },
                                );
                            }
                        }
                    }
                }
                Node::Assign(targets, expressions)
                    if targets.len() == 1
                        && expressions.len() == 1
                        && matches!(source.node(targets[0]), Node::Name(_)) =>
                {
                    let mut table = expressions[0];
                    if let Node::Bin(operator, left, right) = source.node(table) {
                        if operator == "or"
                            && matches!(source.node(*left), Node::Name(_))
                            && bid_of(&res, *left) == bid_of(&res, targets[0])
                            && matches!(source.node(*right), Node::Table(_))
                        {
                            table = *right;
                        }
                    }
                    if matches!(source.node(table), Node::Table(_))
                        && table_literal_pure(&source, table)
                        && namespace_table_values_safe(&source, table)
                    {
                        #[expect(
                            clippy::unwrap_used,
                            reason = "This branch selected a resolved plain-name target before constructing its namespace initializer"
                        )]
                        let bid = bid_of(&res, targets[0]).unwrap();
                        init.insert(
                            bid,
                            ClosedNamespaceInit {
                                statement_index,
                                kind_local: false,
                                table,
                            },
                        );
                    }
                }
                _ => {}
            }
        }
        if init.is_empty() {
            break;
        }
        let mut fields = init
            .keys()
            .copied()
            .map(|bid| (bid, Vec::<String>::new()))
            .collect::<std::collections::HashMap<_, _>>();
        let mut bad = std::collections::HashSet::new();
        for (index, statement) in statements.iter().copied().enumerate() {
            scan_closed_namespace(
                &source,
                &res,
                statement,
                TableScanCtx::Other,
                index,
                &init,
                &mut fields,
                &mut bad,
            );
        }
        // A global initializer runs only if chunk execution reaches it. A
        // `return` in an earlier top-level statement, or a forward `goto`
        // jumping over it, leaves every field scalar unassigned while later
        // reads would still be rewritten, so reject such candidates.
        // Locals are exempt: jumping over a local declaration into its scope
        // is a parse error, and a later `return` cannot precede a local that
        // later code reads under the same binding.
        {
            let mut labels = std::collections::HashMap::new();
            for (index, statement) in statements.iter().copied().enumerate() {
                collect_chunk_labels(&source, statement, index, &mut labels);
            }
            for (bid, info) in init.iter() {
                if info.kind_local || bad.contains(bid) {
                    continue;
                }
                for (index, statement) in statements.iter().copied().enumerate() {
                    if index >= info.statement_index {
                        break;
                    }
                    if chunk_prefix_skips_initializer(
                        &source,
                        statement,
                        info.statement_index,
                        &labels,
                    ) {
                        bad.insert(*bid);
                        break;
                    }
                }
            }
        }
        // The rewrite below discards every other initializer of a
        // candidate binding: the part a `local` declaration gives it
        // and any assignment that is not the recorded initializer.
        // Discarding is sound only when the discarded expression's
        // evaluation is unobservable, so a candidate whose other
        // initializer is not a pure table literal is rejected
        // (candidate S, w9 cycle 66: a fused function body supplied
        // the pure initializer while the declaration still held a
        // constructor whose array part called a nil global, and the
        // call's error vanished with the declaration).
        for (index, statement) in statements.iter().copied().enumerate() {
            match source.node(statement) {
                Node::Local(_, expressions) => {
                    let bids = res
                        .node_bids
                        .get(statement as usize)
                        .cloned()
                        .unwrap_or_default();
                    for (part, bid) in bids.into_iter().enumerate() {
                        if init.contains_key(&bid) {
                            if let Some(expression) = expressions.get(part).copied() {
                                if !table_literal_pure(&source, expression) {
                                    bad.insert(bid);
                                }
                            }
                        }
                    }
                }
                Node::Assign(targets, expressions)
                    if targets.len() == 1
                        && expressions.len() == 1
                        && matches!(source.node(targets[0]), Node::Name(_)) =>
                {
                    if let Some(bid) = bid_of(&res, targets[0]) {
                        if let Some(info) = init.get(&bid) {
                            if info.statement_index != index
                                && !table_literal_pure(&source, expressions[0])
                            {
                                bad.insert(bid);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // `self` is never a candidate: the AST does not bind the implicit
        // method parameter, so occurrences of the name inside a method body
        // resolve to this binding even though they denote the receiver, not
        // the namespace table. Scalarizing would rewrite those reads and
        // writes to the namespace's field scalars (candidate AA, w9
        // cycle 68; the same phantom resolution as candidate X).
        let candidates = init
            .keys()
            .copied()
            .filter(|bid| {
                !bad.contains(bid)
                    && !fields[bid].is_empty()
                    && source.strings.get(res.bindings[*bid as usize].name) != "self"
            })
            .collect::<std::collections::HashSet<_>>();
        if candidates.is_empty() {
            break;
        }
        let mut trial = clone_table_ast(&source);
        let mut field_names = std::collections::HashMap::<(u32, String), SymbolId>::new();
        for bid in &candidates {
            for (field_index, field) in fields[bid].iter().enumerate() {
                // Sanitization is not injective (`a-b` and `a_b`), so retain a
                // deterministic ordinal to keep distinct members distinct.
                let name = format!("__sf{bid}_{field_index}_{}", sanitize_field(field));
                let symbol = trial.strings.fresh(&name);
                field_names.insert((*bid, field.clone()), symbol);
            }
        }
        // Replace table.field reads/writes with scalar synthetic names.
        for node in 0..source.nodes.len() as NodeId {
            if let Node::Index(object, key, _) = source.node(node) {
                if matches!(source.node(*object), Node::Name(_)) {
                    if let (Some(bid), Node::Str(value)) =
                        (bid_of(&res, *object), source.node(*key))
                    {
                        let Some(field) = decode_lua_string(value) else {
                            continue;
                        };
                        if candidates.contains(&bid) {
                            if let Some(symbol) = field_names.get(&(bid, field)).copied() {
                                trial.nodes.rewrite(
                                    node,
                                    Node::Name(symbol),
                                    "namespace-field-access",
                                );
                                trial.nodes.copy_name_from(
                                    node,
                                    storm_lua_syntax::NameSite::Reference,
                                    &source.nodes,
                                    *key,
                                    storm_lua_syntax::NameSite::Member,
                                );
                            }
                        }
                    }
                }
            }
        }
        let mut extra = std::collections::HashMap::<
            usize,
            Vec<(SymbolId, NodeId, NodeId, u32, Option<NodeId>)>,
        >::new();
        for bid in &candidates {
            let info = &init[bid];
            let Node::Table(table_fields) = source.node(info.table) else {
                continue;
            };
            let mut values =
                std::collections::HashMap::<String, (NodeId, u32, Option<NodeId>)>::new();
            for (field_index, field) in table_fields.iter().enumerate() {
                match field {
                    TableField::Name(name, value) => {
                        values.insert(
                            source.strings.get(*name).to_string(),
                            (*value, field_index as u32, None),
                        );
                    }
                    TableField::KVar(key, value) => {
                        if let Node::Str(raw) = source.node(*key) {
                            if let Some(key_text) = decode_lua_string(raw) {
                                values.insert(key_text, (*value, field_index as u32, Some(*key)));
                            }
                        }
                    }
                    TableField::Arr(_) => {}
                }
            }
            for field in &fields[bid] {
                if let Some((value, field_index, key)) = values.get(field).copied() {
                    extra.entry(info.statement_index).or_default().push((
                        field_names[&(*bid, field.clone())],
                        value,
                        info.table,
                        field_index,
                        key,
                    ));
                }
            }
        }
        let mut out = Vec::new();
        for (statement_index, statement) in statements.iter().copied().enumerate() {
            match source.node(statement).clone() {
                Node::Local(names, expressions) => {
                    let bids = res
                        .node_bids
                        .get(statement as usize)
                        .cloned()
                        .unwrap_or_default();
                    let keep = bids
                        .iter()
                        .enumerate()
                        .filter_map(|(part, bid)| (!candidates.contains(bid)).then_some(part))
                        .collect::<Vec<_>>();
                    if !keep.is_empty() {
                        trial.nodes.rewrite(
                            statement,
                            Node::Local(
                                keep.iter().map(|part| names[*part]).collect(),
                                keep.iter()
                                    .filter_map(|part| expressions.get(*part).copied())
                                    .collect(),
                            ),
                            "namespace-binding-removal",
                        );
                        for (new_index, &old_index) in keep.iter().enumerate() {
                            trial.nodes.copy_name_from(
                                statement,
                                storm_lua_syntax::NameSite::Binding(new_index as u32),
                                &source.nodes,
                                statement,
                                storm_lua_syntax::NameSite::Binding(old_index as u32),
                            );
                        }
                        out.push(statement);
                    }
                }
                Node::Assign(targets, _)
                    if targets.len() == 1
                        && matches!(source.node(targets[0]), Node::Name(_))
                        && bid_of(&res, targets[0])
                            .is_some_and(|bid| candidates.contains(&bid)) => {}
                _ => out.push(statement),
            }
            if let Some(values) = extra.get(&statement_index) {
                if !values.is_empty() {
                    let targets = values
                        .iter()
                        .map(|(symbol, _, table, field_index, key)| {
                            let target = trial.push(Node::Name(*symbol));
                            if let Some(key) = key {
                                trial.nodes.derive_from(
                                    target,
                                    &source.nodes,
                                    *key,
                                    "namespace-quoted-field-binding",
                                );
                            } else {
                                trial.nodes.copy_name_from(
                                    target,
                                    storm_lua_syntax::NameSite::Reference,
                                    &source.nodes,
                                    *table,
                                    storm_lua_syntax::NameSite::Field(*field_index),
                                );
                            }
                            target
                        })
                        .collect::<Vec<_>>();
                    let expressions = values.iter().map(|(_, value, ..)| *value).collect();
                    let assignment = trial.push(Node::Assign(targets, expressions));
                    trial.nodes.derive_from(
                        assignment,
                        &source.nodes,
                        statement,
                        "namespace-initializer",
                    );
                    out.push(assignment);
                }
            }
        }
        trial
            .nodes
            .rewrite(root, Node::Block(out), "table-scalarization");
        let old_len = renamed_table_size(&source, root);
        let new_len = renamed_table_size(&trial, root);
        if new_len >= old_len {
            break;
        }
        *ast = trial;
    }
    PassResult {
        root,
        saved: Some(original_size.saturating_sub(measure_size(ast, root)) as u64),
        details: None,
    }
}

#[derive(Clone)]
struct TableParamFunction {
    bid: u32,
    function: NodeId,
    parameters: Vec<SymbolId>,
    parameter_bids: Vec<u32>,
    body: NodeId,
    variadic: bool,
}

fn table_param_functions(ast: &Ast, res: &Resolution) -> Vec<TableParamFunction> {
    res.bindings
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(bid, binding)| {
            let function = binding.function_node?;
            binding.decl_node?;
            let Node::Function(parameters, variadic, body) = ast.node(function) else {
                return None;
            };
            Some(TableParamFunction {
                bid: bid as u32,
                function,
                parameters: parameters.clone(),
                parameter_bids: res
                    .node_bids
                    .get(function as usize)
                    .cloned()
                    .unwrap_or_default(),
                body: *body,
                variadic: *variadic,
            })
        })
        .collect()
}

fn table_param_call_and_bad_refs(
    ast: &Ast,
    root: NodeId,
    res: &Resolution,
    functions: &std::collections::HashMap<u32, TableParamFunction>,
) -> (
    std::collections::HashMap<u32, Vec<NodeId>>,
    std::collections::HashSet<u32>,
) {
    let mut calls = std::collections::HashMap::<u32, Vec<NodeId>>::new();
    let mut direct_names = std::collections::HashSet::<NodeId>::new();
    let mut definition_names = std::collections::HashSet::<NodeId>::new();
    let mut nodes = Vec::new();
    storm_lua_analysis::effects::walk(ast, root, &mut nodes);
    for node in &nodes {
        match ast.node(*node) {
            Node::Call(function, _, _) if matches!(ast.node(*function), Node::Name(_)) => {
                if let Some(bid) = bid_of(res, *function) {
                    if functions.contains_key(&bid) {
                        calls.entry(bid).or_default().push(*node);
                        direct_names.insert(*function);
                    }
                }
            }
            Node::Funcstat(target, _) if matches!(ast.node(*target), Node::Name(_)) => {
                if let Some(bid) = bid_of(res, *target) {
                    if functions.contains_key(&bid) {
                        definition_names.insert(*target);
                    }
                }
            }
            _ => {}
        }
    }
    let mut bad = std::collections::HashSet::new();
    for node in nodes {
        if !matches!(ast.node(node), Node::Name(_))
            || direct_names.contains(&node)
            || definition_names.contains(&node)
        {
            continue;
        }
        if let Some(bid) = bid_of(res, node) {
            if functions.contains_key(&bid) {
                bad.insert(bid);
            }
        }
    }
    (calls, bad)
}

fn inspect_table_parameter_uses(
    ast: &Ast,
    res: &Resolution,
    node: NodeId,
    parameter_bid: u32,
    keys: &mut Vec<StaticTableKey>,
    bad: &mut bool,
) {
    if *bad {
        return;
    }
    if matches!(ast.node(node), Node::Function(..)) {
        // The scalar replacement is not captured by nested closures. Decline
        // only parameters actually captured, including deeper descendants.
        storm_lua_syntax::ast_utils::walk(ast, node, &mut |child| {
            if matches!(ast.node(child), Node::Name(_)) && bid_of(res, child) == Some(parameter_bid)
            {
                *bad = true;
            }
        });
        return;
    }
    if let Node::Index(object, key, _) = ast.node(node) {
        if matches!(ast.node(*object), Node::Name(_)) && bid_of(res, *object) == Some(parameter_bid)
        {
            if let Some(static_key) = static_table_key(ast, *key) {
                if !keys.contains(&static_key) {
                    keys.push(static_key);
                }
                // The parameter Name itself is an allowed use; only inspect key children.
                inspect_table_parameter_uses(ast, res, *key, parameter_bid, keys, bad);
                return;
            }
        }
    }
    if matches!(ast.node(node), Node::Name(_)) && bid_of(res, node) == Some(parameter_bid) {
        *bad = true;
        return;
    }
    let mut children = Vec::new();
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| children.push(child));
    for child in children {
        inspect_table_parameter_uses(ast, res, child, parameter_bid, keys, bad);
    }
}

fn rewrite_table_parameter_body(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    parameter_bid: u32,
    replacements: &[(StaticTableKey, SymbolId)],
) {
    if matches!(source.node(node), Node::Function(..)) {
        return;
    }
    if let Node::Index(object, key, _) = source.node(node) {
        if matches!(source.node(*object), Node::Name(_))
            && bid_of(res, *object) == Some(parameter_bid)
        {
            if let Some(static_key) = static_table_key(source, *key) {
                if let Some((_, symbol)) = replacements.iter().find(|(key, _)| *key == static_key) {
                    target.nodes.rewrite(
                        node,
                        Node::Name(*symbol),
                        "table-parameter-scalarization",
                    );
                    if source
                        .nodes
                        .name_origin(*key, storm_lua_syntax::NameSite::Member)
                        .is_some()
                    {
                        target.nodes.copy_name_from(
                            node,
                            storm_lua_syntax::NameSite::Reference,
                            &source.nodes,
                            *key,
                            storm_lua_syntax::NameSite::Member,
                        );
                    } else {
                        target.nodes.copy_expression_to_name_from(
                            node,
                            storm_lua_syntax::NameSite::Reference,
                            &source.nodes,
                            *key,
                        );
                    }
                    return;
                }
            }
        }
    }
    let mut children = Vec::new();
    storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| children.push(child));
    for child in children {
        rewrite_table_parameter_body(target, source, res, child, parameter_bid, replacements);
    }
}

pub fn scalarize_table_literal_parameters(
    ast: &mut Ast,
    root: NodeId,
    max_rounds: usize,
) -> PassResult {
    let original_size = measure_size(ast, root);
    for _ in 0..max_rounds {
        let source = clone_table_ast(ast);
        let res = resolve(&source, root);
        let analyzer = storm_lua_analysis::effects::EffectAnalyzer::new(&source, &res, root, true);
        let infos = table_param_functions(&source, &res);
        let functions = infos
            .iter()
            .cloned()
            .map(|info| (info.bid, info))
            .collect::<std::collections::HashMap<_, _>>();
        let (calls, bad_refs) = table_param_call_and_bad_refs(&source, root, &res, &functions);
        let mut applied = false;

        for info in &infos {
            let Some(function_calls) = calls.get(&info.bid) else {
                continue;
            };
            if function_calls.is_empty() || bad_refs.contains(&info.bid) || info.variadic {
                continue;
            }
            for parameter_index in 0..info.parameters.len() {
                let all_tables_pure = function_calls.iter().all(|call| {
                    let Node::Call(_, arguments, _) = source.node(*call) else {
                        return false;
                    };
                    arguments
                        .get(parameter_index)
                        .copied()
                        .is_some_and(|argument| {
                            matches!(source.node(argument), Node::Table(_)) && {
                                let effect = analyzer.effects_for_expr(argument);
                                !effect.calls
                                    && !effect.ordered
                                    && effect.writes.is_empty()
                                    && !effect.may_throw
                            }
                        })
                });
                if !all_tables_pure {
                    continue;
                }
                let Some(parameter_bid) = info.parameter_bids.get(parameter_index).copied() else {
                    continue;
                };
                let mut keys = Vec::<StaticTableKey>::new();
                let mut bad = false;
                // Inspect the function body, while allowing the root function itself.
                let Node::Block(body_statements) = source.node(info.body) else {
                    continue;
                };
                for statement in body_statements {
                    inspect_table_parameter_uses(
                        &source,
                        &res,
                        *statement,
                        parameter_bid,
                        &mut keys,
                        &mut bad,
                    );
                }
                if bad || keys.is_empty() {
                    continue;
                }
                // Local budget: scalarization replaces this parameter
                // with one parameter per key, and parameters are
                // locals no later pass can globalize. The widened peak
                // is the function's whole-function simultaneous-local
                // maximum (parameters *and* body locals), minus the
                // removed parameter, plus one parameter per key. Skip
                // when that peak would exceed the VM's 200
                // simultaneous-local limit. Counting only the parameter
                // list undercounts functions whose bodies declare
                // locals: their scalarized form fails to load whenever
                // no later pass happens to globalize those locals.
                // (Method definitions are never candidates here: their
                // call sites are method calls, which this pass does not
                // rewrite, so no implicit self is involved.)
                if super::single_member_cache::max_active_locals(
                    &source,
                    info.parameters.len(),
                    info.body,
                ) - 1
                    + keys.len()
                    > super::single_member_cache::MAX_LOCALS
                {
                    continue;
                }

                let mut trial = clone_table_ast(&source);
                let mut replacements = Vec::<(StaticTableKey, SymbolId)>::new();
                for (index, key) in keys.iter().cloned().enumerate() {
                    let symbol = trial
                        .strings
                        .fresh(&format!("__tp{}_{}_{}", info.bid, parameter_index, index));
                    replacements.push((key, symbol));
                }
                // Rewrite every call of the candidate function.
                for call in function_calls {
                    let Node::Call(function, arguments, method) = trial.node(*call).clone() else {
                        continue;
                    };
                    let table = arguments[parameter_index];
                    let mut expanded = Vec::new();
                    expanded.extend(arguments[..parameter_index].iter().copied());
                    for key in &keys {
                        let start = trial.nodes.len();
                        let value = table_get(&trial, table, key).unwrap_or_else(|| trial.nil());
                        if value as usize >= start {
                            trial.nodes.derive_from(
                                value,
                                &source.nodes,
                                table,
                                "table-parameter-missing-key",
                            );
                            trial.nodes.relate_from(
                                value,
                                &source.nodes,
                                *call,
                                "table-parameter-argument",
                            );
                        }
                        expanded.push(value);
                    }
                    expanded.extend(arguments[parameter_index + 1..].iter().copied());
                    trial.nodes.rewrite(
                        *call,
                        Node::Call(function, expanded, method),
                        "table-argument-expansion",
                    );
                }
                // Rewrite the function parameter list and indexed uses.
                let mut parameters = info.parameters.clone();
                parameters.splice(
                    parameter_index..=parameter_index,
                    replacements.iter().map(|(_, symbol)| *symbol),
                );
                trial.nodes.rewrite(
                    info.function,
                    Node::Function(parameters, info.variadic, info.body),
                    "table-parameter-expansion",
                );
                if trial.nodes.tracks_origins() {
                    let new_len = info.parameters.len() - 1 + replacements.len();
                    for index in 0..new_len {
                        let old_index = if index < parameter_index {
                            index
                        } else if index < parameter_index + replacements.len() {
                            parameter_index
                        } else {
                            index + 1 - replacements.len()
                        };
                        trial.nodes.copy_name_from(
                            info.function,
                            storm_lua_syntax::NameSite::Parameter(index as u32),
                            &source.nodes,
                            info.function,
                            storm_lua_syntax::NameSite::Parameter(old_index as u32),
                        );
                    }
                }
                let Node::Block(body_statements) = source.node(info.body) else {
                    continue;
                };
                for statement in body_statements {
                    rewrite_table_parameter_body(
                        &mut trial,
                        &source,
                        &res,
                        *statement,
                        parameter_bid,
                        &replacements,
                    );
                }

                let old_len = renamed_table_size(&source, root);
                let new_len = renamed_table_size(&trial, root);
                if new_len < old_len {
                    *ast = trial;
                    applied = true;
                    break;
                }
            }
            if applied {
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
        details: None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod scalarization_tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    #[test]
    fn immutable_table_accesses_are_folded() {
        let (mut ast, root) = parse_source(
            "local t={x={y=12345},z=7}function onTick()output.setNumber(1,t.x.y+t.z)end",
        )
        .unwrap();
        flatten_immutable_tables(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(!out.contains("t.x"), "{out}");
        assert!(out.contains("12345"), "{out}");
    }

    #[test]
    fn immutable_table_with_nil_key_is_not_flattened() {
        // A nil constructor key raises "table index is nil" when the
        // constructor evaluates, so the table is not a pure literal:
        // flattening must not fold the reads or remove the binding.
        let (mut ast, root) =
            parse_source("local t={[nil]=1,b=2}function onTick()output.setBool(1,t.b==2)end")
                .unwrap();
        flatten_immutable_tables(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("[nil]"), "{out}");
        assert!(out.contains("t.b"), "{out}");
    }

    #[test]
    fn immutable_table_with_nan_key_is_not_flattened() {
        // Same for a NaN key ("table index is NaN"): `0 / 0` only
        // becomes NaN at runtime, so the constructor is effectful.
        let (mut ast, root) =
            parse_source("local t={[0/0]=1,b=2}function onTick()output.setBool(1,t.b==2)end")
                .unwrap();
        flatten_immutable_tables(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("[0/0]"), "{out}");
        assert!(out.contains("t.b"), "{out}");
    }

    #[test]
    fn immutable_table_with_name_key_is_not_flattened() {
        // A name key carries no proof here: its value may be nil or
        // NaN at runtime even when the reads are all static.
        let (mut ast, root) = parse_source(
            "local k=\"x\" local t={[k]=1,b=2}function onTick()output.setBool(1,t.b==2)end",
        )
        .unwrap();
        flatten_immutable_tables(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("[k]"), "{out}");
        assert!(out.contains("t.b"), "{out}");
    }

    #[test]
    fn immutable_table_with_bool_key_is_still_flattened() {
        // Boolean keys are always valid table keys, so the flattening
        // remains available for them (and for static string keys).
        let (mut ast, root) =
            parse_source("local t={[true]=1,b=2}function onTick()output.setBool(1,t.b==2)end")
                .unwrap();
        flatten_immutable_tables(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(!out.contains("t.b"), "{out}");
    }

    #[test]
    fn immutable_table_with_nan_value_read_is_not_flattened() {
        // A NaN *value* is legal, but it is not a scalar literal the
        // rewrite can copy into the read site (the constant folder
        // refuses to materialize non-finite results), so the read
        // cannot be substituted and the binding must stay.
        let (mut ast, root) = parse_source(
            "local t={a=0/0,b=2}function onTick()output.setBool(1,t.a~=t.a)output.setBool(2,t.b==2)end",
        )
        .unwrap();
        flatten_immutable_tables(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("t.a"), "{out}");
        assert!(out.contains("0/0"), "{out}");
    }

    #[test]
    fn immutable_table_with_inf_value_read_is_not_flattened() {
        // Same for an infinite value (`1 / 0`).
        let (mut ast, root) =
            parse_source("local t={a=1/0,b=2}function onTick()output.setBool(1,t.a>0)end").unwrap();
        flatten_immutable_tables(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("t.a"), "{out}");
        assert!(out.contains("1/0"), "{out}");
    }

    #[test]
    fn immutable_table_with_missing_intermediate_key_is_not_flattened() {
        // `t.inner.b` with no `inner` field cannot be substituted
        // (only a missing *final* key becomes nil), so the binding
        // must stay for the remaining reads.
        let (mut ast, root) = parse_source(
            "local t={b=2}function onTick()output.setBool(1,t.b==2)output.setBool(2,t.inner.b==2)end",
        )
        .unwrap();
        flatten_immutable_tables(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("t.inner.b"), "{out}");
        assert!(out.contains("t={"), "{out}");
    }

    #[test]
    fn immutable_table_with_nan_value_unread_is_not_flattened() {
        // A compound field value keeps the whole table out of the
        // flattening even when that field is never read: evaluating
        // the value is part of evaluating the constructor, and a
        // compound expression over literals can raise (see
        // immutable_table_with_raising_value_is_not_flattened). The
        // folder materializes provably finite compound values into
        // literals before this pass runs in the pipeline, so this
        // only costs the rare non-finite / unprovable shapes.
        let (mut ast, root) =
            parse_source("local t={a=0/0,b=2}function onTick()output.setBool(1,t.b==2)end")
                .unwrap();
        flatten_immutable_tables(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("t.b"), "{out}");
        assert!(out.contains("t={"), "{out}");
    }

    #[test]
    fn immutable_table_with_raising_value_is_not_flattened() {
        // `1 + nil` raises when the constructor evaluates, so the
        // constructor must not be dropped even though the field is
        // never read and the remaining reads are all scalar.
        let (mut ast, root) =
            parse_source("local t={a=1+nil,b=2}function onTick()output.setBool(1,t.b==2)end")
                .unwrap();
        flatten_immutable_tables(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("t.b"), "{out}");
        assert!(out.contains("t={"), "{out}");
    }

    #[test]
    fn immutable_table_with_int_div_zero_value_is_not_flattened() {
        // Integer division by zero raises as well.
        let (mut ast, root) =
            parse_source("local t={a=1//0,b=2}function onTick()output.setBool(1,t.b==2)end")
                .unwrap();
        flatten_immutable_tables(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("t.b"), "{out}");
        assert!(out.contains("t={"), "{out}");
    }

    #[test]
    fn immutable_table_global_use_before_init_is_not_folded() {
        let (mut ast, root) = parse_source(
            "g = {f = function() return 7 end} local done = false function onTick() if not done then output.setNumber(1, g.f()) g = {f = 5} done = true end end",
        )
        .unwrap();
        flatten_immutable_tables(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("g.f"), "{out}");
    }

    #[test]
    fn immutable_table_global_use_in_init_rhs_is_not_folded() {
        let (mut ast, root) =
            parse_source("g, x = {f = 5}, g.f function onTick() output.setNumber(1, x) end")
                .unwrap();
        flatten_immutable_tables(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("g.f"), "{out}");
    }

    #[test]
    fn immutable_table_global_use_after_init_is_not_folded() {
        let (mut ast, root) =
            parse_source("g = {v = 42} function onTick() output.setNumber(1, g.v) end").unwrap();
        flatten_immutable_tables(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("g.v"), "{out}");
        assert!(out.contains("g={"), "{out}");
    }

    #[test]
    fn immutable_table_global_binding_is_not_removed_across_functions() {
        let (mut ast, root) = parse_source(
            "function onTick() g = {v = 42} end function onDraw() output.setNumber(1, g.v) end",
        )
        .unwrap();
        flatten_immutable_tables(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("g={"), "{out}");
        assert!(out.contains("g.v"), "{out}");
    }

    #[test]
    fn closed_namespace_pre_init_read_is_not_scalarized() {
        let (mut ast, root) = parse_source(
            "output.setNumber(1, g.verylongfieldnameforscalar) g = {verylongfieldnameforscalar = 42, otherfieldnameforscalar = 7} function onTick() output.setNumber(2, g.verylongfieldnameforscalar) end",
        )
        .unwrap();
        scalarize_closed_namespaces(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("g.verylongfieldnameforscalar"), "{out}");
    }

    #[test]
    fn closed_namespace_impure_local_init_is_not_scalarized() {
        // Candidate S (w9 cycle 66): the pure assignment is the
        // recorded initializer, but the local declaration's own
        // initializer is a constructor with an effectful array
        // element. Scalarization discards the declaration part, so it
        // must refuse: the element's evaluation must survive.
        let (mut ast, root) =
            parse_source("local v = {F37()} v = {x = 100} v.x = 7 output.setNumber(1, v.x)")
                .unwrap();
        scalarize_closed_namespaces(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("F37()"), "{out}");
        assert!(out.contains("v.x"), "{out}");
    }

    #[test]
    fn closed_namespace_pre_init_call_is_not_scalarized() {
        let (mut ast, root) = parse_source(
            "function show() output.setNumber(1, g.verylongfieldnameforscalar) end show() g = {verylongfieldnameforscalar = 42, otherfieldnameforscalar = 7} function onTick() output.setNumber(2, g.verylongfieldnameforscalar) end",
        )
        .unwrap();
        scalarize_closed_namespaces(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("g.verylongfieldnameforscalar"), "{out}");
    }

    #[test]
    fn closed_namespace_goto_skipped_init_is_not_scalarized() {
        let (mut ast, root) = parse_source(
            "goto done g = {verylongfieldnameforscalar = 42, otherfieldnameforscalar = 7} ::done:: function onTick() output.setNumber(1, g.verylongfieldnameforscalar) end",
        )
        .unwrap();
        scalarize_closed_namespaces(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("g.verylongfieldnameforscalar"), "{out}");
    }

    #[test]
    fn closed_namespace_return_before_init_is_not_scalarized() {
        let (mut ast, root) = parse_source(
            "function onTick() output.setNumber(1, g.verylongfieldnameforscalar) end do return end g = {verylongfieldnameforscalar = 42, otherfieldnameforscalar = 7}",
        )
        .unwrap();
        scalarize_closed_namespaces(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("g.verylongfieldnameforscalar"), "{out}");
    }

    #[test]
    fn immutable_table_local_self_is_not_flattened_into_methods() {
        // Candidate AA (w9 cycle 68): the method body's `self.x`
        // resolves to this local but denotes the receiver; folding it
        // to the local's literal field would change the method's result.
        let (mut ast, root) = parse_source(
            "local self = {x = 111} local T = {x = 222} function T:m() return self.x end function onTick() output.setNumber(1, T:m()) end",
        )
        .unwrap();
        flatten_immutable_tables(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("self.x"), "{out}");
    }

    #[test]
    fn closed_namespace_global_self_is_not_scalarized() {
        // Candidate AA (w9 cycle 68): occurrences of `self` inside a
        // method body resolve to the global binding but denote the
        // receiver; scalarizing the global would substitute the
        // namespace's field values for the receiver's.
        let (mut ast, root) = parse_source(
            "self = {verylongfieldnameforscalar = 1000, otherfieldnameforscalar = 7} local T = {verylongfieldnameforscalar = 11} function T:m() return self.verylongfieldnameforscalar end function onTick() output.setNumber(1, T:m()) end",
        )
        .unwrap();
        scalarize_closed_namespaces(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("self.verylongfieldnameforscalar"), "{out}");
    }

    #[test]
    fn closed_namespace_local_self_is_not_scalarized() {
        // Candidate AA, local form: a chunk local named `self` is the
        // resolution target of method-body occurrences in its scope,
        // which still denote the receiver, not this table.
        let (mut ast, root) = parse_source(
            "local self = {verylongfieldnameforscalar = 111, otherfieldnameforscalar = 7} local T = {verylongfieldnameforscalar = 222} function T:m() return self.verylongfieldnameforscalar end function onTick() output.setNumber(1, T:m()) end",
        )
        .unwrap();
        scalarize_closed_namespaces(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("self.verylongfieldnameforscalar"), "{out}");
    }

    #[test]
    fn closed_namespace_global_post_init_still_scalarized() {
        let (mut ast, root) = parse_source(
            "g = {verylongfieldnameforscalar = 42, otherfieldnameforscalar = 7} function onTick() output.setNumber(1, g.verylongfieldnameforscalar) output.setNumber(2, g.verylongfieldnameforscalar) output.setNumber(3, g.otherfieldnameforscalar) end",
        )
        .unwrap();
        scalarize_closed_namespaces(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(!out.contains("g.verylongfieldnameforscalar"), "{out}");
    }

    #[test]
    fn closed_namespace_becomes_scalar_bindings() {
        let (mut ast, root) = parse_source(
            "local t={verylongfield=1,other=2}function onTick()t.verylongfield=t.verylongfield+1 output.setNumber(1,t.verylongfield+t.other)end",
        )
        .unwrap();
        scalarize_closed_namespaces(&mut ast, root, 8);
        let out = Printer::new(&ast, false).output(root);
        assert!(!out.contains("t.verylongfield"), "{out}");
    }

    #[test]
    fn table_parameter_scalarization_rejects_effectful_table_literals() {
        let source = "counter=0 function side()counter=counter+1 return 9 end local function f(t)return t.x end function onTick()local x=f({x=1,y=side()})output.setNumber(1,counter)end";
        let (mut ast, root) = parse_source(source).unwrap();
        scalarize_table_literal_parameters(&mut ast, root, 16);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("side()"), "{out}");
        assert!(out.contains("{x=1,y=side()}"), "{out}");
    }
    #[test]
    fn table_parameter_scalarization_respects_the_parameter_limit() {
        // 196 parameters + the table parameter widen to 201: skip.
        let params = (1..=196)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let args = (1..=196)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "local function f({params},t)return t.a+t.b+t.c+t.d+t.e+(p1 or 0) end return f({args},{{a=1,b=2,c=3,d=4,e=5}})"
        );
        let (mut ast, root) = parse_source(&source).unwrap();
        scalarize_table_literal_parameters(&mut ast, root, 16);
        let out = Printer::new(&ast, false).output(root);
        assert!(!out.contains("__tp"), "{out}");
    }

    #[test]
    fn table_parameter_scalarization_counts_body_locals() {
        // 194 parameters + the table parameter + 2 body locals: the
        // parameter count alone widens to 199, but the true peak is
        // (195 + 2) - 1 + 5 = 201, so scalarization must skip.
        let params = (1..=194)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let args = (1..=194)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "local function f({params},t) local alpha = p1 local beta = p2 return alpha + beta + t.a+t.b+t.c+t.d+t.e+(p3 or 0) end return f({args},{{a=1,b=2,c=3,d=4,e=5}})"
        );
        let (mut ast, root) = parse_source(&source).unwrap();
        scalarize_table_literal_parameters(&mut ast, root, 16);
        let out = Printer::new(&ast, false).output(root);
        assert!(!out.contains("__tp"), "{out}");
    }

    #[test]
    fn table_parameter_scalarization_with_body_locals_at_the_limit() {
        // 193 parameters + the table parameter + 2 body locals: the true
        // peak is (194 + 2) - 1 + 5 = 200, exactly at the limit, so
        // scalarization still applies.
        let params = (1..=193)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let args = (1..=193)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "local function f({params},t) local alpha = p1 local beta = p2 return alpha + beta + t.a+t.b+t.c+t.d+t.e+(p3 or 0) end return f({args},{{a=1,b=2,c=3,d=4,e=5}})"
        );
        let (mut ast, root) = parse_source(&source).unwrap();
        scalarize_table_literal_parameters(&mut ast, root, 16);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("__tp"), "{out}");
    }

    #[test]
    fn table_parameter_scalarization_below_the_parameter_limit() {
        // 195 parameters + the table parameter widen to exactly 200.
        let params = (1..=195)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let args = (1..=195)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "local function f({params},t)return t.a+t.b+t.c+t.d+t.e+(p1 or 0) end return f({args},{{a=1,b=2,c=3,d=4,e=5}})"
        );
        let (mut ast, root) = parse_source(&source).unwrap();
        scalarize_table_literal_parameters(&mut ast, root, 16);
        let out = Printer::new(&ast, false).output(root);
        assert!(out.contains("__tp"), "{out}");
    }
}
