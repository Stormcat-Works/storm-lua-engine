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
    let mut reads: std::collections::HashMap<u32, std::collections::HashSet<String>> =
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
        reads: &mut std::collections::HashMap<u32, std::collections::HashSet<String>>,
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
                                    reads.get_mut(&obid).unwrap().insert(decode_lua_string(s));
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
        !reads.get(&bid).unwrap().contains(&decode_lua_string(s))
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
    use storm_lua_syntax::parser::parse_source;
    use storm_lua_syntax::print::Printer;

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

use crate::scope_rename::scope_rename_fast;
use storm_lua_syntax::ast::{SymbolId, TableField};
use storm_lua_syntax::numeric::num_val;
use storm_lua_syntax::size::measure_size;

fn clone_table_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn renamed_table_size(ast: &Ast, root: NodeId) -> usize {
    let renamed = scope_rename_fast(ast, root);
    measure_size(&renamed.ast, renamed.root)
}

#[derive(Clone, Debug, PartialEq)]
enum StaticTableKey {
    Str(String),
    Num(f64),
}

fn static_table_key(ast: &Ast, node: NodeId) -> Option<StaticTableKey> {
    match ast.node(node) {
        Node::Str(value) => Some(StaticTableKey::Str(decode_lua_string(value))),
        Node::Num(value) => Some(StaticTableKey::Num(num_val(value))),
        _ => None,
    }
}

fn table_literal_pure(ast: &Ast, node: NodeId) -> bool {
    match ast.node(node) {
        Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil => true,
        Node::Un(_, expression) => table_literal_pure(ast, *expression),
        Node::Bin(_, left, right) => {
            table_literal_pure(ast, *left) && table_literal_pure(ast, *right)
        }
        Node::Table(fields) => fields.iter().all(|field| match field {
            TableField::Arr(value) | TableField::Name(_, value) => table_literal_pure(ast, *value),
            TableField::KVar(key, value) => {
                table_literal_pure(ast, *key) && table_literal_pure(ast, *value)
            }
        }),
        _ => false,
    }
}

fn table_get(ast: &mut Ast, table: NodeId, key: &StaticTableKey) -> NodeId {
    let Node::Table(fields) = ast.node(table).clone() else {
        return ast.push(Node::Nil);
    };
    let mut array_index = 1f64;
    for field in fields {
        match field {
            TableField::Arr(value) => {
                if matches!(key, StaticTableKey::Num(value_key) if *value_key == array_index) {
                    return value;
                }
                array_index += 1.0;
            }
            TableField::Name(name, value) => {
                if matches!(key, StaticTableKey::Str(value_key) if value_key == ast.strings.get(name))
                {
                    return value;
                }
            }
            TableField::KVar(field_key, value) => {
                if static_table_key(ast, field_key).as_ref() == Some(key) {
                    return value;
                }
            }
        }
    }
    ast.push(Node::Nil)
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

fn static_table_chain_value(ast: &Ast, table: NodeId, keys: &[StaticTableKey]) -> Option<NodeId> {
    let mut current = table;
    for key in keys {
        let Node::Table(fields) = ast.node(current) else {
            return None;
        };
        let mut array_index = 1.0;
        let mut next = None;
        for field in fields {
            match field {
                TableField::Arr(value) => {
                    if matches!(key, StaticTableKey::Num(expected) if *expected == array_index) {
                        next = Some(*value);
                        break;
                    }
                    array_index += 1.0;
                }
                TableField::Name(name, value) => {
                    if matches!(key, StaticTableKey::Str(expected) if expected == ast.strings.get(*name))
                    {
                        next = Some(*value);
                        break;
                    }
                }
                TableField::KVar(field_key, value) => {
                    if static_table_key(ast, *field_key).as_ref() == Some(key) {
                        next = Some(*value);
                        break;
                    }
                }
            }
        }
        current = next?;
    }
    Some(current)
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
                let allowed = parent_ctx == TableScanCtx::IndexObj
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
            if static_table_chain_value(ast, info.table, &keys).is_some_and(|value| {
                parent_ctx != TableScanCtx::IndexObj
                    && matches!(ast.node(value), Node::Table(_) | Node::Function(..))
            }) {
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
            for key in &keys {
                if !matches!(source.node(current), Node::Table(_)) {
                    ok = false;
                    break;
                }
                let mut scratch = clone_table_ast(source);
                current = table_get(&mut scratch, current, key);
                if current as usize >= source.nodes.len() {
                    // missing key -> nil allocated in scratch
                    target.nodes[node as usize] = Node::Nil;
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
    // Preserve already rewritten child slots; parent NodeIds still refer to them.
    target.nodes[node as usize] = mapped;
}

fn flatten_immutable_tables_in_block(ast: &mut Ast, root: NodeId, max_rounds: usize) {
    for _ in 0..max_rounds {
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
                Node::Assign(targets, expressions) if targets.len() == expressions.len() => {
                    for (target, expression) in
                        targets.iter().copied().zip(expressions.iter().copied())
                    {
                        if matches!(source.node(target), Node::Name(_))
                            && matches!(source.node(expression), Node::Table(_))
                            && table_literal_pure(&source, expression)
                        {
                            if let Some(bid) = bid_of(&res, target) {
                                init.insert(
                                    bid,
                                    ImmutableTableInit {
                                        statement_index,
                                        table: expression,
                                        kind_local: false,
                                    },
                                );
                            }
                        }
                    }
                }
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
        let candidates = init
            .keys()
            .copied()
            .filter(|bid| !bad.contains(bid))
            .collect::<std::collections::HashSet<_>>();
        if candidates.is_empty() {
            break;
        }

        let old_len = renamed_table_size(&source, root);
        let mut trial = clone_table_ast(&source);
        for node in 0..source.nodes.len() as NodeId {
            rewrite_static_table_chains(&mut trial, &source, &res, node, &candidates, &init);
        }
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
                        trial.nodes[statement as usize] = Node::Local(
                            keep.iter().map(|part| names[*part]).collect(),
                            keep.iter().map(|part| expressions[*part]).collect(),
                        );
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
                        trial.nodes[statement as usize] = Node::Assign(
                            keep.iter().map(|part| targets[*part]).collect(),
                            keep.iter().map(|part| expressions[*part]).collect(),
                        );
                        out.push(statement);
                    }
                }
                _ => out.push(statement),
            }
        }
        trial.nodes[root as usize] = Node::Block(out);
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
                    if let Node::Str(value) = ast.node(*key) {
                        let value = decode_lua_string(value);
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
        let candidates = init
            .keys()
            .copied()
            .filter(|bid| !bad.contains(bid) && !fields[bid].is_empty())
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
                let symbol = trial.strings.intern(&name);
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
                        let field = decode_lua_string(value);
                        if candidates.contains(&bid) {
                            if let Some(symbol) = field_names.get(&(bid, field)).copied() {
                                trial.nodes[node as usize] = Node::Name(symbol);
                            }
                        }
                    }
                }
            }
        }
        let mut extra = std::collections::HashMap::<usize, Vec<(SymbolId, NodeId)>>::new();
        for bid in &candidates {
            let info = &init[bid];
            let Node::Table(table_fields) = source.node(info.table) else {
                continue;
            };
            let mut values = std::collections::HashMap::<String, NodeId>::new();
            for field in table_fields {
                match field {
                    TableField::Name(name, value) => {
                        values.insert(source.strings.get(*name).to_string(), *value);
                    }
                    TableField::KVar(key, value) => {
                        if let Node::Str(key) = source.node(*key) {
                            values.insert(decode_lua_string(key), *value);
                        }
                    }
                    TableField::Arr(_) => {}
                }
            }
            for field in &fields[bid] {
                if let Some(value) = values.get(field).copied() {
                    extra
                        .entry(info.statement_index)
                        .or_default()
                        .push((field_names[&(*bid, field.clone())], value));
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
                        trial.nodes[statement as usize] = Node::Local(
                            keep.iter().map(|part| names[*part]).collect(),
                            keep.iter()
                                .filter_map(|part| expressions.get(*part).copied())
                                .collect(),
                        );
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
                        .map(|(symbol, _)| trial.push(Node::Name(*symbol)))
                        .collect::<Vec<_>>();
                    let expressions = values.iter().map(|(_, value)| *value).collect();
                    let assignment = trial.push(Node::Assign(targets, expressions));
                    out.push(assignment);
                }
            }
        }
        trial.nodes[root as usize] = Node::Block(out);
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
    // Nested function bodies are excluded just like the TS implementation.
    if matches!(ast.node(node), Node::Function(..)) {
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
                    target.nodes[node as usize] = Node::Name(*symbol);
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

                let mut trial = clone_table_ast(&source);
                let mut replacements = Vec::<(StaticTableKey, SymbolId)>::new();
                for (index, key) in keys.iter().cloned().enumerate() {
                    let symbol = trial
                        .strings
                        .intern(&format!("__tp{}_{}_{}", info.bid, parameter_index, index));
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
                        expanded.push(table_get(&mut trial, table, key));
                    }
                    expanded.extend(arguments[parameter_index + 1..].iter().copied());
                    trial.nodes[*call as usize] = Node::Call(function, expanded, method);
                }
                // Rewrite the function parameter list and indexed uses.
                let mut parameters = info.parameters.clone();
                parameters.splice(
                    parameter_index..=parameter_index,
                    replacements.iter().map(|(_, symbol)| *symbol),
                );
                trial.nodes[info.function as usize] =
                    Node::Function(parameters, info.variadic, info.body);
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
    use storm_lua_syntax::parser::parse_source;
    use storm_lua_syntax::print::Printer;

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
}
