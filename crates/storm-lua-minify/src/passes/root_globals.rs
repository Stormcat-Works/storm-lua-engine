//! Root-local globalization (`passes/scope-and-api.ts::globalizeRootLocals`).
//!
//! Only top-level locals with a complete non-empty initializer list are
//! converted to global assignments. Top-level local functions become ordinary
//! function statements. Partial or uninitialized locals are preserved.

use crate::pass::PassResult;
use std::collections::HashMap;
use storm_lua_syntax::ast::{Ast, Node, NodeId, TableField};
use storm_lua_syntax::size::measure_size;

pub fn globalize_root_locals(ast: &mut Ast, root: NodeId) -> PassResult {
    let original_size = measure_size(ast, root);
    let Node::Block(statements) = ast.node(root).clone() else {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    };
    // Root locals are lexical bindings.  Converting two declarations with the
    // same spelling to global assignments merges distinct BindingIds and
    // changes callback state (the second declaration would reset the first).
    // Function statements share the same global namespace and are included in
    // the collision check as well.
    let mut root_name_counts = HashMap::<String, usize>::new();
    for statement in &statements {
        match ast.node(*statement) {
            Node::Local(names, _) => {
                for name in names {
                    *root_name_counts
                        .entry(ast.strings.get(*name).to_string())
                        .or_default() += 1;
                }
            }
            Node::Localfunc(name, _) => {
                *root_name_counts
                    .entry(ast.strings.get(*name).to_string())
                    .or_default() += 1;
            }
            Node::Funcstat(target, _) => {
                if let Node::Name(name) = ast.node(*target) {
                    *root_name_counts
                        .entry(ast.strings.get(*name).to_string())
                        .or_default() += 1;
                }
            }
            _ => {}
        }
    }
    let resolution = storm_lua_analysis::resolver::resolve(ast, root);
    let function_writes = function_write_bids(ast, &resolution, root);
    let analyzer = storm_lua_analysis::effects::EffectAnalyzer::new(ast, &resolution, root, true);
    let mut timing_excluded =
        member_base_timing_exclusions(ast, &resolution, root, &function_writes);
    timing_excluded.extend(deferred_read_timing_exclusions(
        ast,
        &resolution,
        &analyzer,
        root,
        &function_writes,
    ));
    for (name, _) in &resolution.globals {
        *root_name_counts
            .entry(ast.strings.get(*name).to_string())
            .or_default() += 1;
    }
    for name in ast.strings.iter() {
        if storm_lua_analysis::resolver::reserved(name)
            || storm_lua_analysis::resolver::api_roots(name)
        {
            root_name_counts.insert(name.to_string(), 2);
        }
    }
    // One shared code string for every rewrite in this pass: the recorded
    // reasons are identical, and a wide root block would otherwise pay one
    // fresh code string per rewritten statement (in the origin, the reason,
    // and any fallback detachment reason).
    let code: std::sync::Arc<str> = std::sync::Arc::from("root-local-globalization");
    let mut output = Vec::with_capacity(statements.len());
    for statement in statements {
        let timing_excluded_here = resolution
            .node_bids
            .get(statement as usize)
            .is_some_and(|bids| bids.iter().any(|bid| timing_excluded.contains(bid)))
            || resolution
                .node_bid
                .get(statement as usize)
                .copied()
                .flatten()
                .is_some_and(|bid| timing_excluded.contains(&bid));
        match ast.node(statement).clone() {
            Node::Local(names, expressions)
                if !timing_excluded_here
                    && !expressions.is_empty()
                    && expressions.len() == names.len()
                    && names.iter().all(|name| {
                        root_name_counts.get(ast.strings.get(*name)).copied() == Some(1)
                    }) =>
            {
                let targets = names
                    .into_iter()
                    .enumerate()
                    .map(|(index, symbol)| {
                        let name = ast.strings.get(symbol).to_string();
                        let target = ast.name(&name);
                        inherit_synthetic_binding(ast, target, statement);
                        ast.nodes.copy_name_within(
                            target,
                            storm_lua_syntax::NameSite::Reference,
                            statement,
                            storm_lua_syntax::NameSite::Binding(index as u32),
                        );
                        target
                    })
                    .collect::<Vec<_>>();
                // Rewrite the statement slot in place instead of appending a
                // fresh assignment node and finishing it from a captured
                // origin: the slot's origin is taken (not cloned) by
                // `rewrite`, and the old local id is retained, so the final
                // block rewrite no longer records the locals as detached.
                // The derived origin, reason, and name transfers are the
                // same as the previous capture + finish path.
                ast.nodes
                    .rewrite_with_code(statement, Node::Assign(targets, expressions), &code);
                output.push(statement);
            }
            Node::Localfunc(name, function)
                if !timing_excluded_here
                    && root_name_counts.get(ast.strings.get(name)).copied() == Some(1) =>
            {
                let name = ast.strings.get(name).to_string();
                let target = ast.name(&name);
                inherit_synthetic_binding(ast, target, statement);
                ast.nodes.copy_name_within(
                    target,
                    storm_lua_syntax::NameSite::Reference,
                    statement,
                    storm_lua_syntax::NameSite::Binding(0),
                );
                ast.nodes
                    .rewrite_with_code(statement, Node::Funcstat(target, function), &code);
                output.push(statement);
            }
            _ => output.push(statement),
        }
    }
    ast.nodes
        .rewrite_with_code(root, Node::Block(output), &code);
    PassResult {
        root,
        saved: Some(original_size.saturating_sub(measure_size(ast, root)) as u64),
        details: None,
    }
}

/// Root locals that must not be globalized because the conversion would
/// change when a member-assignment target's base is located.
///
/// A member target whose base is a local is located through its register
/// at store time, and one whose base is an upvalue is located through
/// its cell at store time (SETTABUP), so a rebind that happens while the
/// right-hand side runs still lands in the new table. A global base is
/// materialized before the right-hand side runs, so the same store
/// lands in the old table. (An earlier version of this analysis assumed
/// an upvalue base is materialized like a global; the w9 cycle-66
/// campaign disproved that: with the base swapped to a number before
/// the call, the upvalue form stores into the rebound table while the
/// globalized form errors indexing the number.) Globalizing is
/// therefore unsafe for a root local that is used as a
/// member-assignment base, at chunk level or inside a function,
/// whenever the binding can be re-bound in that window: by a write
/// inside a function body (the right-hand side may call it, including
/// an immediately invoked function in the right-hand side itself), or
/// by an earlier name target of the same multi-target assignment
/// statement.
/// Bindings written anywhere inside a function body. A root local in
/// this set can be re-bound while chunk-level code runs, whenever a
/// call reaches one of those functions.
fn function_write_bids(
    ast: &Ast,
    resolution: &storm_lua_analysis::resolver::Resolution,
    root: NodeId,
) -> std::collections::HashSet<storm_lua_analysis::resolver::BindingId> {
    fn scan_function_writes(
        ast: &Ast,
        resolution: &storm_lua_analysis::resolver::Resolution,
        node: NodeId,
        in_function: bool,
        out: &mut std::collections::HashSet<storm_lua_analysis::resolver::BindingId>,
    ) {
        let in_function = in_function || matches!(ast.node(node), Node::Function(..));
        if in_function
            && resolution
                .node_write
                .get(node as usize)
                .copied()
                .unwrap_or(false)
        {
            if let Some(bid) = resolution.node_bid.get(node as usize).copied().flatten() {
                out.insert(bid);
            }
        }
        storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
            scan_function_writes(ast, resolution, child, in_function, out);
        });
    }
    let mut out = std::collections::HashSet::new();
    scan_function_writes(ast, resolution, root, false, &mut out);
    out
}

fn member_base_timing_exclusions(
    ast: &Ast,
    resolution: &storm_lua_analysis::resolver::Resolution,
    root: NodeId,
    function_write_bids: &std::collections::HashSet<storm_lua_analysis::resolver::BindingId>,
) -> std::collections::HashSet<storm_lua_analysis::resolver::BindingId> {
    use storm_lua_analysis::resolver::BindingId;
    fn chain_root_bid(
        ast: &Ast,
        resolution: &storm_lua_analysis::resolver::Resolution,
        mut node: NodeId,
    ) -> Option<BindingId> {
        loop {
            match ast.node(node) {
                Node::Name(_) => return resolution.node_bid.get(node as usize).copied().flatten(),
                Node::Index(inner, _, _) | Node::Methodname(inner, _) | Node::Paren(inner) => {
                    node = *inner;
                }
                _ => return None,
            }
        }
    }
    let mut member_base_bids = std::collections::HashSet::new();
    let mut same_statement_rebinds = std::collections::HashSet::new();
    fn scan_member_bases(
        ast: &Ast,
        resolution: &storm_lua_analysis::resolver::Resolution,
        node: NodeId,
        member_base_bids: &mut std::collections::HashSet<storm_lua_analysis::resolver::BindingId>,
        same_statement_rebinds: &mut std::collections::HashSet<
            storm_lua_analysis::resolver::BindingId,
        >,
    ) {
        if let Node::Assign(targets, _) = ast.node(node) {
            let member_roots = targets
                .iter()
                .map(|target| {
                    if matches!(ast.node(*target), Node::Name(_)) {
                        None
                    } else {
                        chain_root_bid(ast, resolution, *target)
                    }
                })
                .collect::<Vec<_>>();
            for (index, root_bid) in member_roots.iter().enumerate() {
                if let Some(bid) = root_bid {
                    member_base_bids.insert(*bid);
                    // An earlier name target of the same statement that
                    // rebinds this base runs before this target's store.
                    for earlier in &targets[..index] {
                        if matches!(ast.node(*earlier), Node::Name(_))
                            && resolution
                                .node_bid
                                .get(*earlier as usize)
                                .copied()
                                .flatten()
                                == Some(*bid)
                        {
                            same_statement_rebinds.insert(*bid);
                        }
                    }
                }
            }
        }
        storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
            scan_member_bases(
                ast,
                resolution,
                child,
                member_base_bids,
                same_statement_rebinds,
            );
        });
    }
    scan_member_bases(
        ast,
        resolution,
        root,
        &mut member_base_bids,
        &mut same_statement_rebinds,
    );
    member_base_bids
        .into_iter()
        .filter(|bid| function_write_bids.contains(bid) || same_statement_rebinds.contains(bid))
        .collect()
}

/// Root locals that must not be globalized because the conversion would
/// change when an expression reads them (Q01, generalized by R01 and
/// completed by S01 in round 308).
///
/// Globalizing a root local turns its reads into `_ENV` lookups, which
/// PUC materializes into a register at the point the read subexpression
/// is discharged. A same-function local is instead read *in place*: its
/// register (or upvalue cell) is consumed by the instruction that uses
/// the value, which can run after sibling subexpressions have been
/// evaluated. A read is therefore timing-sensitive exactly when its
/// consuming instruction runs after a sibling subtree that may rebind
/// the local:
///
/// - binary operator operands (either side): the operator instruction
///   runs after both operand subtrees (Q01; parentheses do not change
///   this, R01). `and`/`or` have no operator instruction and are
///   excluded. Concatenation is kept under the same rule for uniformity
///   even though PUC copies its operands.
/// - an index expression's base: the table register is consumed by the
///   indexing instruction after the key subtree runs (S01).
/// - a table constructor's computed key: the key register is consumed
///   by the field store after the value subtree runs (S01).
/// - an assignment target's index key: the key register is consumed by
///   the store instruction, which runs after every right-hand side and
///   every sibling target subexpression has been evaluated (T01).
///   Round 308's battery had classified this position as copied at
///   discharge, but that measurement ran the parenthesis-free shape
///   through the full pipeline, where downstream passes happened to
///   fold the globalized form back to the original result; measured
///   directly in PUC, a local target key is read at store time with or
///   without parentheses, while a global key is materialized before
///   the right-hand side runs.
///
/// Every other read position copies the value into a fresh register at
/// its own evaluation point in PUC, exactly where a global lookup would
/// be materialized, so read timing is unobservable there (measured in
/// round 308): index keys, constructor field values (record and list),
/// call and method callees and arguments, return values, numeric-for
/// bounds, generic-for expressions, and unary operands (consumed by
/// their own instruction immediately). Reads inside nested functions
/// are upvalue reads, which PUC materializes eagerly just like global
/// reads, so the scan does not cross function boundaries.
///
/// Positions are classified on the parenthesis-stripped shape:
/// parentheses never change a position's read timing in PUC, so every
/// classification point (the position matchers and the read matcher)
/// strips them through the one `strip_parens` helper instead of
/// treating parenthesized variants as separate shapes. The round-308
/// battery had classified from parenthesis-free shapes only, which is
/// how the parenthesized target key slipped the classification (T01).
fn deferred_read_timing_exclusions(
    ast: &Ast,
    resolution: &storm_lua_analysis::resolver::Resolution,
    analyzer: &storm_lua_analysis::effects::EffectAnalyzer<'_>,
    root: NodeId,
    function_write_bids: &std::collections::HashSet<storm_lua_analysis::resolver::BindingId>,
) -> std::collections::HashSet<storm_lua_analysis::resolver::BindingId> {
    use storm_lua_analysis::resolver::BindingId;

    fn node_bid(
        resolution: &storm_lua_analysis::resolver::Resolution,
        node: NodeId,
    ) -> Option<BindingId> {
        resolution.node_bid.get(node as usize).copied().flatten()
    }

    /// A write of `bid` inside `node`, not crossing a nested function
    /// boundary (mirrors the forwarding pass's body scan).
    fn body_contains_write(
        ast: &Ast,
        resolution: &storm_lua_analysis::resolver::Resolution,
        node: NodeId,
        bid: BindingId,
    ) -> bool {
        if matches!(ast.node(node), Node::Function(..)) {
            return false;
        }
        if matches!(ast.node(node), Node::Name(_))
            && node_bid(resolution, node) == Some(bid)
            && resolution
                .node_write
                .get(node as usize)
                .copied()
                .unwrap_or(false)
        {
            return true;
        }
        let mut children = Vec::new();
        storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
            children.push(child)
        });
        children
            .iter()
            .any(|&child| body_contains_write(ast, resolution, child, bid))
    }

    fn body_contains_call(ast: &Ast, node: NodeId) -> bool {
        if matches!(ast.node(node), Node::Function(..)) {
            return false;
        }
        if matches!(ast.node(node), Node::Call(..)) {
            return true;
        }
        let mut found = false;
        storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
            found |= body_contains_call(ast, child);
        });
        found
    }

    /// Whether evaluating `call` may rebind `bid` (mirrors the
    /// forwarding pass's `call_may_write_target`, including its
    /// transitivity over the static call graph).
    fn call_may_write(
        ast: &Ast,
        resolution: &storm_lua_analysis::resolver::Resolution,
        analyzer: &storm_lua_analysis::effects::EffectAnalyzer<'_>,
        call: NodeId,
        bid: BindingId,
    ) -> bool {
        let mut visited = std::collections::HashSet::new();
        call_may_write_inner(ast, resolution, analyzer, call, bid, &mut visited)
    }

    fn call_may_write_inner(
        ast: &Ast,
        resolution: &storm_lua_analysis::resolver::Resolution,
        analyzer: &storm_lua_analysis::effects::EffectAnalyzer<'_>,
        call: NodeId,
        bid: BindingId,
        visited: &mut std::collections::HashSet<BindingId>,
    ) -> bool {
        if analyzer.effects_for_expr(call).writes.contains(&bid) {
            return true;
        }
        let Node::Call(function, _, _) = ast.node(call) else {
            return true;
        };
        if analyzer.resolve_builtin_reference(*function).is_some() {
            return false;
        }
        match ast.node(*function) {
            Node::Function(_, _, body) => {
                body_contains_write(ast, resolution, *body, bid) || body_contains_call(ast, *body)
            }
            Node::Name(_) => {
                if let Some(callee_bid) = node_bid(resolution, *function) {
                    if let Some(function_node) =
                        resolution.bindings[callee_bid as usize].function_node
                    {
                        if let Node::Function(_, _, body) = ast.node(function_node) {
                            // Transitive over the static call graph,
                            // as in the forwarding pass (T03): a call
                            // through an unresolved callee inside the
                            // body may invoke a rebinding function.
                            if !visited.insert(callee_bid) {
                                return false;
                            }
                            return body_may_write(ast, resolution, analyzer, *body, bid, visited);
                        }
                        return false;
                    }
                }
                true
            }
            _ => true,
        }
    }

    /// The transitive body scan behind `call_may_write_inner`: a body
    /// may rebind `bid` when it writes it directly, or when any call
    /// it makes may rebind it (not crossing a nested function
    /// boundary, as in `body_contains_write`).
    fn body_may_write(
        ast: &Ast,
        resolution: &storm_lua_analysis::resolver::Resolution,
        analyzer: &storm_lua_analysis::effects::EffectAnalyzer<'_>,
        node: NodeId,
        bid: BindingId,
        visited: &mut std::collections::HashSet<BindingId>,
    ) -> bool {
        if matches!(ast.node(node), Node::Function(..)) {
            return false;
        }
        if matches!(ast.node(node), Node::Name(_))
            && node_bid(resolution, node) == Some(bid)
            && resolution
                .node_write
                .get(node as usize)
                .copied()
                .unwrap_or(false)
        {
            return true;
        }
        if matches!(ast.node(node), Node::Call(..))
            && call_may_write_inner(ast, resolution, analyzer, node, bid, visited)
        {
            return true;
        }
        let mut children = Vec::new();
        storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
            children.push(child)
        });
        children
            .iter()
            .any(|&child| body_may_write(ast, resolution, analyzer, child, bid, visited))
    }

    fn subtree_has_writer_call(
        ast: &Ast,
        resolution: &storm_lua_analysis::resolver::Resolution,
        analyzer: &storm_lua_analysis::effects::EffectAnalyzer<'_>,
        node: NodeId,
        bid: BindingId,
    ) -> bool {
        if matches!(ast.node(node), Node::Function(..)) {
            return false;
        }
        if matches!(ast.node(node), Node::Call(..))
            && call_may_write(ast, resolution, analyzer, node, bid)
        {
            return true;
        }
        let mut found = false;
        storm_lua_syntax::ast_utils::for_each_child_key(ast, node, &mut |_, child| {
            found |= subtree_has_writer_call(ast, resolution, analyzer, child, bid);
        });
        found
    }

    /// Parentheses stripped from `node`: classification always works
    /// on the stripped shape, because parentheses never change a
    /// position's read timing in PUC (R01, T01).
    fn strip_parens(ast: &Ast, mut node: NodeId) -> NodeId {
        while let Node::Paren(inner) = ast.node(node) {
            node = *inner;
        }
        node
    }

    /// The binding read by a bare local reference at `node` (through
    /// any parentheses), when the reference is a read of a
    /// function-written binding.
    fn bare_read_bid(
        ast: &Ast,
        resolution: &storm_lua_analysis::resolver::Resolution,
        node: NodeId,
        function_write_bids: &std::collections::HashSet<BindingId>,
    ) -> Option<BindingId> {
        // 括弧は何重でも剥がす: `(x)` は PUC でも素のレジスタ参照の
        // ままで、消費命令の実行時点に読まれる（R01）。
        let node = strip_parens(ast, node);
        if !matches!(ast.node(node), Node::Name(_))
            || resolution
                .node_write
                .get(node as usize)
                .copied()
                .unwrap_or(false)
        {
            return None;
        }
        node_bid(resolution, node).filter(|bid| function_write_bids.contains(bid))
    }

    /// Exclude the binding read at position `read` when the sibling
    /// subtree `window` — evaluated after `read`'s discharge point but
    /// before its consuming instruction — may rebind it.
    #[allow(clippy::too_many_arguments)]
    fn check_position(
        ast: &Ast,
        resolution: &storm_lua_analysis::resolver::Resolution,
        analyzer: &storm_lua_analysis::effects::EffectAnalyzer<'_>,
        read: NodeId,
        window: NodeId,
        function_write_bids: &std::collections::HashSet<BindingId>,
        excluded: &mut std::collections::HashSet<BindingId>,
    ) {
        if let Some(bid) = bare_read_bid(ast, resolution, read, function_write_bids) {
            if subtree_has_writer_call(ast, resolution, analyzer, window, bid) {
                excluded.insert(bid);
            }
        }
    }

    fn scan(
        ast: &Ast,
        resolution: &storm_lua_analysis::resolver::Resolution,
        analyzer: &storm_lua_analysis::effects::EffectAnalyzer<'_>,
        node: NodeId,
        function_write_bids: &std::collections::HashSet<BindingId>,
        excluded: &mut std::collections::HashSet<BindingId>,
    ) {
        if matches!(ast.node(node), Node::Function(..)) {
            return;
        }
        match ast.node(node) {
            Node::Bin(op, left, right) if !matches!(op.as_str(), "and" | "or") => {
                let (left, right) = (*left, *right);
                check_position(
                    ast,
                    resolution,
                    analyzer,
                    left,
                    right,
                    function_write_bids,
                    excluded,
                );
                check_position(
                    ast,
                    resolution,
                    analyzer,
                    right,
                    left,
                    function_write_bids,
                    excluded,
                );
            }
            Node::Index(base, key, _) => {
                let (base, key) = (*base, *key);
                check_position(
                    ast,
                    resolution,
                    analyzer,
                    base,
                    key,
                    function_write_bids,
                    excluded,
                );
            }
            Node::Table(fields) => {
                for field in fields {
                    if let TableField::KVar(key, value) = field {
                        let (key, value) = (*key, *value);
                        check_position(
                            ast,
                            resolution,
                            analyzer,
                            key,
                            value,
                            function_write_bids,
                            excluded,
                        );
                    }
                }
            }
            Node::Assign(targets, expressions) => {
                // An assignment target's index key is read by the store
                // instruction, after every right-hand side and every
                // sibling target subexpression has run (T01). The
                // window is therefore the whole statement: any
                // right-hand side, and the base/key subexpressions of
                // every target (a sibling target's key may itself hold
                // the rebinding call). The target and the key are
                // matched on their parenthesis-stripped shapes.
                let (targets, expressions) = (targets.clone(), expressions.clone());
                for target in &targets {
                    let stripped = strip_parens(ast, *target);
                    if let Node::Index(_, key, _) = ast.node(stripped) {
                        let key = *key;
                        if let Some(bid) = bare_read_bid(ast, resolution, key, function_write_bids)
                        {
                            let in_rhs = expressions.iter().any(|expr| {
                                subtree_has_writer_call(ast, resolution, analyzer, *expr, bid)
                            });
                            let in_targets = targets.iter().any(|other| {
                                subtree_has_writer_call(ast, resolution, analyzer, *other, bid)
                            });
                            if in_rhs || in_targets {
                                excluded.insert(bid);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
            scan(
                ast,
                resolution,
                analyzer,
                child,
                function_write_bids,
                excluded,
            );
        });
    }

    let mut excluded = std::collections::HashSet::new();
    if let Node::Block(statements) = ast.node(root) {
        for statement in statements.clone() {
            scan(
                ast,
                resolution,
                analyzer,
                statement,
                function_write_bids,
                &mut excluded,
            );
        }
    }
    excluded
}

// A compiler-created local has no original identifier slot. Globalizing it
// retains that explicit generated origin, not a fabricated source declaration.
fn inherit_synthetic_binding(ast: &mut Ast, target: NodeId, statement: NodeId) {
    if ast
        .nodes
        .origin(statement)
        .is_some_and(|o| o.kind == storm_lua_syntax::provenance::OriginKind::Synthetic)
    {
        let snapshot = ast.nodes.capture_origin(statement);
        ast.nodes
            .finish_rewrite(target, snapshot, "root-generated-binding-globalization");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = globalize_root_locals(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn globalizes_fully_initialized_root_locals() {
        assert_eq!(output("local a,b=1,2"), "a,b=1,2");
    }

    #[test]
    fn globalizes_root_local_functions() {
        assert_eq!(
            output("local function helper(x)return x end"),
            "function helper(x)return x end"
        );
    }

    #[test]
    fn preserves_partial_or_uninitialized_locals() {
        assert_eq!(output("local a,b=1\nlocal c"), "local a,b=1 local c");
    }

    #[test]
    fn globalizes_mixed_root_statements_in_place() {
        // Converted statements keep their slot (and its derived origin);
        // unconverted statements keep theirs. `output` runs the
        // origin-auditing printer, so every surviving byte must keep a
        // known origin through the in-place rewrite path.
        assert_eq!(
            output("local a=1 local function helper(x) return x end local b\nlocal c=2"),
            "a=1 function helper(x)return x end local b c=2"
        );
    }

    #[test]
    fn preserves_nested_locals() {
        assert_eq!(
            output("function onTick()local x=1 end"),
            "function onTick()local x=1 end"
        );
    }

    #[test]
    fn keeps_member_base_local_rebound_inside_function() {
        // `t` is the base of a member assignment whose right-hand side
        // can rebind it (the callee writes it as an upvalue). As a local
        // the store reads the register and lands in the new table; as a
        // global the base would be materialized first and the store
        // would land in the old one.
        let out = output("local t={} local function f()t={} return 1 end t.x=f() return t.x");
        assert!(out.contains("local t="), "{out}");
    }

    #[test]
    fn keeps_member_base_local_rebound_by_same_statement() {
        // The earlier name target of the same assignment rebinds the
        // base before the member target's store runs.
        let out = output("local T={} T,T.x={},1 return T.x");
        assert!(out.contains("local T="), "{out}");
    }

    #[test]
    fn globalizes_member_base_never_rebound() {
        // No rebind anywhere: which table the base denotes never
        // changes, so the location timing is unobservable.
        assert_eq!(
            output("local t={} t.x=1 return t.x"),
            "t={}t.x=1 return t.x"
        );
    }

    #[test]
    fn keeps_member_base_local_rebound_inside_function_bodies() {
        // Inside a function the base is an upvalue; SETTABUP reads the
        // cell at store time, so a rebind by the right-hand side call
        // still lands in the new table -- unlike a global base, which is
        // materialized before the call. (This corrects the earlier
        // assumption that the two are equivalent; w9 cycle 66,
        // candidate L.)
        let out = output(
            "local t={} local function f()t={} return 1 end local function g()t.x=f() end return g",
        );
        assert!(out.contains("local t="), "{out}");
    }

    #[test]
    fn keeps_local_read_as_binary_operand_rebound_by_sibling_call() {
        // Q01: a direct local operand of a binary operator is read by
        // the operator instruction, after the sibling call has run and
        // rebound it (PUC gives 1 + 3 with x read as 2 -> 5 in the
        // original; a global operand is materialized before the call
        // and would give 4). The local must stay a local.
        let out = output("local x=1 local function f() x=2 return 3 end return x+f()");
        assert!(out.contains("local x="), "{out}");
        // The same shape inside a call argument (the Codex variant).
        let out = output("local x=1 local function f() x=2 return 3 end output.setNumber(1,x+f())");
        assert!(out.contains("local x="), "{out}");
    }

    #[test]
    fn keeps_parenthesized_local_read_as_binary_operand() {
        // R01: parentheses do not materialize a local read in PUC —
        // `(x) + f()` still reads x when the operator executes, after
        // f() rebound it (original 5, globalized form 4). The Q01 fix
        // exempted this shape; the run6 review disproved the exemption.
        let out = output("local x=1 local function f() x=2 return 3 end return (x)+f()");
        assert!(out.contains("local x="), "{out}");
        // Double parentheses and the right-hand operand behave the same.
        let out = output("local x=1 local function f() x=2 return 3 end return f()+((x))");
        assert!(out.contains("local x="), "{out}");
    }

    #[test]
    fn globalizes_binary_operand_when_sibling_call_cannot_rebind() {
        // The sibling call never writes the operand binding, so the
        // read timing is unobservable and globalization proceeds.
        let out = output("local x=1 local function f() return 3 end return x+f()");
        assert!(!out.contains("local x="), "{out}");
        // A parenthesized operand converts too when the sibling call
        // cannot rebind (parentheses do not change the read timing).
        let out = output("local x=1 local function f() return 3 end return (x)+f()");
        assert!(!out.contains("local x="), "{out}");
    }

    #[test]
    fn keeps_local_read_as_index_base_rebound_by_key_call() {
        // S01: an index expression's base register is consumed by the
        // indexing instruction after the key subtree runs, so a key
        // call that rebinds the base changes what the index reads
        // (original 2, globalized form 1). Parentheses do not change
        // this. The local must stay a local.
        let out = output("local t={x=1} local function f() t={x=2} return \"x\" end return t[f()]");
        assert!(out.contains("local t="), "{out}");
        let out =
            output("local t={x=1} local function f() t={x=2} return \"x\" end return ((t))[f()]");
        assert!(out.contains("local t="), "{out}");
        // The same position reached through a nested index and through
        // a local initializer.
        let out = output(
            "local t={u={x=1}} local function f() t={u={x=2}} return \"u\" end return t[f()].x",
        );
        assert!(out.contains("local t="), "{out}");
        let out = output(
            "local t={x=1} local function f() t={x=2} return \"x\" end local r=t[f()] return r",
        );
        assert!(out.contains("local t="), "{out}");
    }

    #[test]
    fn keeps_local_read_as_constructor_key_rebound_by_value_call() {
        // S01: a computed constructor key register is consumed by the
        // field store after the value subtree runs (original
        // {[2]=7}, globalized form {[1]=7}). Parentheses do not
        // change this either.
        let out = output("local k=1 local function f() k=2 return 7 end return {[k]=f()}");
        assert!(out.contains("local k="), "{out}");
        let out = output("local k=1 local function f() k=2 return 7 end return {[(k)]=f()}");
        assert!(out.contains("local k="), "{out}");
    }

    #[test]
    fn globalizes_reads_in_positions_copied_at_discharge() {
        // Round 308 classification of read positions: PUC copies the
        // value into a fresh register at these positions, exactly
        // where a global lookup would be materialized, so a sibling
        // call that rebinds the local afterwards is unobservable and
        // globalization proceeds. (The deferred positions — binary
        // operands, index bases, computed constructor keys — are
        // covered by the keeps_* tests above.)
        // Constructor field values (record and list).
        let out =
            output("local x=1 local function f() x=2 return 9 end local t={a=x, b=f()} return t.a");
        assert!(!out.contains("local x="), "{out}");
        let out =
            output("local x=1 local function f() x=2 return 9 end local t={x, f()} return t[1]");
        assert!(!out.contains("local x="), "{out}");
        // Call arguments.
        let out = output(
            "local x=1 local function f() x=2 return 0 end local function g(a,b) return a end return g(x, f())",
        );
        assert!(!out.contains("local x="), "{out}");
        // Return values.
        let out = output("local x=1 local function f() x=2 return 0 end return x, f()");
        assert!(!out.contains("local x="), "{out}");
        // Parenthesized variants of the copied positions classify
        // identically (positions are matched on the stripped shape).
        let out = output(
            "local x=1 local function f() x=2 return 9 end local t={a=(x), b=f()} return t.a",
        );
        assert!(!out.contains("local x="), "{out}");
        let out = output(
            "local x=1 local function f() x=2 return 0 end local function g(a,b) return a end return g((x), f())",
        );
        assert!(!out.contains("local x="), "{out}");
        let out = output("local x=1 local function f() x=2 return 0 end return (x), f()");
        assert!(!out.contains("local x="), "{out}");
        // Numeric-for bounds.
        let out = output(
            "local x=1 local function f() x=50 return 3 end local s=0 for i=x, f() do s=s+1 end return s",
        );
        assert!(!out.contains("local x="), "{out}");
    }

    #[test]
    fn keeps_local_read_as_assignment_target_key_rebound_by_rhs_call() {
        // T01: an assignment target's index key is read by the store
        // instruction, after the right-hand side has run. A global key
        // is materialized before the right-hand side, so globalizing
        // the key binding changes the destination slot (original
        // t[2]=3, globalized form t[1]=3). Parentheses change nothing:
        // the classification works on the stripped shape, so the bare,
        // parenthesized, and doubly parenthesized keys are all kept.
        let out = output(
            "local k=1 local t={} local function f() k=2 return 3 end t[k]=f() return t[1],t[2]",
        );
        assert!(out.contains("local k="), "{out}");
        let out = output(
            "local k=1 local t={} local function f() k=2 return 3 end t[(k)]=f() return t[1],t[2]",
        );
        assert!(out.contains("local k="), "{out}");
        let out = output(
            "local k=1 local t={} local function f() k=2 return 3 end t[((k))]=f() return t[1],t[2]",
        );
        assert!(out.contains("local k="), "{out}");
        // The rebinding call may also sit in a sibling target's key:
        // every target subexpression is evaluated before any store.
        let out = output(
            "local k=1 local t={} local function g() k=2 return 1 end t[k],t[g()]=1,1 return t[2]",
        );
        assert!(out.contains("local k="), "{out}");
        // With no rebinding call in the statement, the key's read
        // timing is unobservable and globalization proceeds.
        let out =
            output("local k=1 local t={} local function f() return 3 end t[(k)]=f() return t[1]");
        assert!(!out.contains("local k="), "{out}");
    }

    #[test]
    fn keeps_member_base_local_when_swap_can_make_it_a_number() {
        // Candidate L (w9 cycle 66): with the base globalized, the
        // second onTick would materialize the swapped-in number and
        // error indexing it; as an upvalue the store reads the cell
        // after the callee rebinds it to a table.
        let out = output(
            "local t={x=4} local c=0 local function f()t={x=100} return 7 end function onTick() t.x=f() c,t=t,c end",
        );
        assert!(out.contains("local t="), "{out}");
    }
}
