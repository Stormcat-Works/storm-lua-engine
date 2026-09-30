//! Uniform table fill scalarization (`passes/uniform-tables.ts`).

use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::numeric::num_val;

#[derive(Clone)]
struct Fill {
    table_bid: BindingId,
    value: NodeId,
    loop_node: NodeId,
    target_index: NodeId,
    lower: f64,
    upper: f64,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn walk(ast: &Ast, root: NodeId) -> Vec<NodeId> {
    let mut nodes = Vec::new();
    storm_lua_analysis::effects::walk(ast, root, &mut nodes);
    nodes
}

fn tick_invariant(
    ast: &Ast,
    res: &Resolution,
    expression: NodeId,
    loop_bid: BindingId,
    analyzer: &EffectAnalyzer<'_>,
) -> bool {
    for node in walk(ast, expression) {
        if matches!(ast.node(node), Node::Name(_))
            && res.node_bid.get(node as usize).copied().flatten() == Some(loop_bid)
        {
            return false;
        }
        match ast.node(node) {
            Node::Call(function, _, _) => {
                let builtin = analyzer.resolve_builtin_reference(*function);
                if !matches!(
                    builtin.as_deref(),
                    Some("input.getNumber" | "property.getNumber")
                ) {
                    return false;
                }
            }
            Node::Function(..) | Node::Table(..) | Node::Vararg | Node::Nil | Node::Bool(_) => {
                return false;
            }
            Node::Un(operator, _) if operator == "not" => return false,
            Node::Bin(operator, _, _)
                if matches!(
                    operator.as_str(),
                    "and" | "or" | "==" | "~=" | "<" | "<=" | ">" | ">="
                ) =>
            {
                return false;
            }
            _ => {}
        }
    }
    true
}

fn uniform_fill(
    ast: &Ast,
    res: &Resolution,
    statement: NodeId,
    analyzer: &EffectAnalyzer<'_>,
) -> Option<Fill> {
    let Node::Fornum(_, start, end, step, body) = ast.node(statement) else {
        return None;
    };
    let loop_bid = res.node_bid.get(statement as usize).copied().flatten()?;
    let Node::Num(start_text) = ast.node(*start) else {
        return None;
    };
    let Node::Num(end_text) = ast.node(*end) else {
        return None;
    };
    if step.is_some_and(|step| !matches!(ast.node(step), Node::Num(_))) {
        return None;
    }
    let Node::Block(body_statements) = ast.node(*body) else {
        return None;
    };
    if body_statements.len() != 1 {
        return None;
    }
    let Node::Assign(targets, expressions) = ast.node(body_statements[0]) else {
        return None;
    };
    if targets.len() != 1 || expressions.len() != 1 {
        return None;
    }
    let target = targets[0];
    let Node::Index(object, key, _) = ast.node(target) else {
        return None;
    };
    let Node::Name(_) = ast.node(*object) else {
        return None;
    };
    let table_bid = res.node_bid.get(*object as usize).copied().flatten()?;
    if !matches!(ast.node(*key), Node::Name(_))
        || res.node_bid.get(*key as usize).copied().flatten() != Some(loop_bid)
        || !tick_invariant(ast, res, expressions[0], loop_bid, analyzer)
    {
        return None;
    }
    let start = num_val(start_text);
    let end = num_val(end_text);
    let step = step
        .map(|step| match ast.node(step) {
            Node::Num(value) => num_val(value),
            _ => unreachable!(),
        })
        .unwrap_or(1.0);
    if !start.is_finite()
        || !end.is_finite()
        || !step.is_finite()
        || start.fract() != 0.0
        || end.fract() != 0.0
        || step != 1.0
        || start > end
    {
        return None;
    }
    Some(Fill {
        table_bid,
        value: expressions[0],
        loop_node: statement,
        target_index: target,
        lower: start,
        upper: end,
    })
}

fn loop_ranges(ast: &Ast, res: &Resolution, root: NodeId) -> HashMap<BindingId, (f64, f64)> {
    let mut ranges = HashMap::new();
    for node in walk(ast, root) {
        let Node::Fornum(_, start, end, _, _) = ast.node(node) else {
            continue;
        };
        let (Node::Num(start), Node::Num(end)) = (ast.node(*start), ast.node(*end)) else {
            continue;
        };
        let lower = num_val(start);
        let upper = num_val(end);
        if lower.is_finite() && upper.is_finite() && lower <= upper {
            if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
                ranges.insert(bid, (lower, upper));
            }
        }
    }
    ranges
}

fn index_range(
    ast: &Ast,
    res: &Resolution,
    key: NodeId,
    ranges: &HashMap<BindingId, (f64, f64)>,
) -> Option<(f64, f64)> {
    match ast.node(key) {
        Node::Num(value) => {
            let value = num_val(value);
            (value.is_finite() && value.fract() == 0.0).then_some((value, value))
        }
        Node::Name(_) => res
            .node_bid
            .get(key as usize)
            .copied()
            .flatten()
            .and_then(|bid| ranges.get(&bid).copied()),
        _ => None,
    }
}

fn can_scalar_read(
    ast: &Ast,
    res: &Resolution,
    index: NodeId,
    fill: &Fill,
    ranges: &HashMap<BindingId, (f64, f64)>,
) -> bool {
    let Node::Index(_, key, _) = ast.node(index) else {
        return false;
    };
    index_range(ast, res, *key, ranges).is_some_and(|_| true) && {
        // Every known range is expressible: outside=>nil, inside=>scalar, overlap=>guard.
        let _ = fill;
        true
    }
}

fn build_scalar_read(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    index: NodeId,
    fill: &Fill,
    ranges: &HashMap<BindingId, (f64, f64)>,
) -> Option<NodeId> {
    let Node::Index(object, key, _) = source.node(index) else {
        return None;
    };
    let (lower, upper) = index_range(source, res, *key, ranges)?;
    if upper < fill.lower || lower > fill.upper {
        let nil = target.push(Node::Nil);
        target
            .nodes
            .derive_from(nil, &source.nodes, index, "uniform-fill-outside-range");
        target
            .nodes
            .relate_from(nil, &source.nodes, fill.loop_node, "uniform-fill-range");
        return Some(nil);
    }
    let Node::Name(symbol) = source.node(*object) else {
        return None;
    };
    let scalar = target.push(Node::Name(*symbol));
    target
        .nodes
        .derive_from(scalar, &source.nodes, index, "uniform-fill-read");
    target.nodes.copy_name_from(
        scalar,
        storm_lua_syntax::NameSite::Reference,
        &source.nodes,
        *object,
        storm_lua_syntax::NameSite::Reference,
    );
    target
        .nodes
        .relate_from(scalar, &source.nodes, fill.value, "uniform-fill-value");
    if lower >= fill.lower && upper <= fill.upper {
        return Some(scalar);
    }
    let Node::Fornum(_, original_start, original_end, _, _) = source.node(fill.loop_node) else {
        unreachable!()
    };
    let mut conditions = Vec::new();
    if lower < fill.lower {
        let lower_node = target.push(Node::Num(fill.lower.to_string().into()));
        target.nodes.derive_from(
            lower_node,
            &source.nodes,
            *original_start,
            "uniform-fill-bound",
        );
        let check = target.push(Node::Bin(">=".to_string(), *key, lower_node));
        target
            .nodes
            .derive_from(check, &source.nodes, index, "uniform-fill-range-check");
        conditions.push(check);
    }
    if upper > fill.upper {
        let upper_node = target.push(Node::Num(fill.upper.to_string().into()));
        target.nodes.derive_from(
            upper_node,
            &source.nodes,
            *original_end,
            "uniform-fill-bound",
        );
        let check = target.push(Node::Bin("<=".to_string(), *key, upper_node));
        target
            .nodes
            .derive_from(check, &source.nodes, index, "uniform-fill-range-check");
        conditions.push(check);
    }
    let mut guarded = scalar;
    for condition in conditions.into_iter().rev() {
        guarded = target.push(Node::Bin("and".to_string(), condition, guarded));
        target
            .nodes
            .derive_from(guarded, &source.nodes, index, "uniform-fill-guard");
    }
    let nil = target.push(Node::Nil);
    target
        .nodes
        .derive_from(nil, &source.nodes, index, "uniform-fill-outside-range");
    let result = target.push(Node::Bin("or".to_string(), guarded, nil));
    target
        .nodes
        .derive_from(result, &source.nodes, index, "uniform-fill-guard");
    Some(result)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChildRole {
    Other,
    IndexObject,
}

#[allow(clippy::too_many_arguments)]
fn scan_invalid(
    ast: &Ast,
    res: &Resolution,
    node: NodeId,
    parent: Option<NodeId>,
    role: ChildRole,
    candidates: &HashMap<BindingId, Fill>,
    initializers: &HashMap<BindingId, NodeId>,
    ranges: &HashMap<BindingId, (f64, f64)>,
    invalid: &mut HashSet<BindingId>,
) {
    if let Node::Name(_) = ast.node(node) {
        if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
            if let Some(fill) = candidates.get(&bid) {
                let allowed = parent == initializers.get(&bid).copied()
                    || parent == Some(fill.target_index)
                    || role == ChildRole::IndexObject;
                if !allowed {
                    invalid.insert(bid);
                }
            }
        }
    }
    if let Node::Index(object, _, _) = ast.node(node) {
        if let Node::Name(_) = ast.node(*object) {
            if let Some(bid) = res.node_bid.get(*object as usize).copied().flatten() {
                if let Some(fill) = candidates.get(&bid) {
                    let write = res.node_write.get(node as usize).copied().unwrap_or(false);
                    if write && node != fill.target_index {
                        invalid.insert(bid);
                    }
                    if !write && !can_scalar_read(ast, res, node, fill, ranges) {
                        invalid.insert(bid);
                    }
                }
            }
        }
    }
    match ast.node(node) {
        Node::Index(object, key, _) => {
            scan_invalid(
                ast,
                res,
                *object,
                Some(node),
                ChildRole::IndexObject,
                candidates,
                initializers,
                ranges,
                invalid,
            );
            scan_invalid(
                ast,
                res,
                *key,
                Some(node),
                ChildRole::Other,
                candidates,
                initializers,
                ranges,
                invalid,
            );
        }
        _ => {
            let mut children = Vec::new();
            storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
                children.push(child)
            });
            for child in children {
                scan_invalid(
                    ast,
                    res,
                    child,
                    Some(node),
                    ChildRole::Other,
                    candidates,
                    initializers,
                    ranges,
                    invalid,
                );
            }
        }
    }
}

fn rewrite_node(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    candidates: &HashMap<BindingId, Fill>,
    initializers: &HashMap<BindingId, NodeId>,
    ranges: &HashMap<BindingId, (f64, f64)>,
) -> Option<NodeId> {
    if let Some(fill) = candidates.values().find(|fill| fill.loop_node == node) {
        let Node::Index(object, _, _) = source.node(fill.target_index) else {
            unreachable!()
        };
        let Node::Name(symbol) = source.node(*object) else {
            unreachable!()
        };
        let target_name = target.push(Node::Name(*symbol));
        target.nodes.derive_from(
            target_name,
            &source.nodes,
            *object,
            "uniform-fill-scalar-storage",
        );
        let value = rewrite_node(
            target,
            source,
            res,
            fill.value,
            candidates,
            initializers,
            ranges,
        )?;
        target.nodes.rewrite(
            node,
            Node::Assign(vec![target_name], vec![value]),
            "uniform-fill-assignment",
        );
        return Some(node);
    }
    if let Node::Local(names, expressions) = source.node(node).clone() {
        let bids = res
            .node_bids
            .get(node as usize)
            .cloned()
            .unwrap_or_default();
        let mut new_expressions = expressions;
        for index in (0..new_expressions.len()).rev() {
            if bids
                .get(index)
                .is_some_and(|bid| candidates.contains_key(bid))
                && matches!(source.node(new_expressions[index]), Node::Table(fields) if fields.is_empty())
            {
                new_expressions.remove(index);
            }
        }
        // Other expression children are still recursively rewritten.
        let mut rewritten = Vec::new();
        for expression in new_expressions {
            if let Some(expression) = rewrite_node(
                target,
                source,
                res,
                expression,
                candidates,
                initializers,
                ranges,
            ) {
                rewritten.push(expression);
            }
        }
        target
            .nodes
            .rewrite(node, Node::Local(names, rewritten), "uniform-fill-storage");
        return Some(node);
    }
    if let Node::Assign(targets, expressions) = source.node(node) {
        if targets.len() == 1
            && expressions.len() == 1
            && matches!(source.node(targets[0]), Node::Name(_))
        {
            if let Some(bid) = res.node_bid.get(targets[0] as usize).copied().flatten() {
                if candidates.contains_key(&bid)
                    && initializers.get(&bid).copied() == Some(node)
                    && matches!(source.node(expressions[0]), Node::Table(fields) if fields.is_empty())
                {
                    return None;
                }
            }
        }
    }
    if let Node::Index(object, _, _) = source.node(node) {
        if let Node::Name(_) = source.node(*object) {
            if let Some(bid) = res.node_bid.get(*object as usize).copied().flatten() {
                if let Some(fill) = candidates.get(&bid) {
                    let read = build_scalar_read(target, source, res, node, fill, ranges)?;
                    let value = target.node(read).clone();
                    target.nodes[node as usize] = value;
                    let origin = target.nodes.capture_origin(read);
                    target
                        .nodes
                        .finish_rewrite(node, origin, "uniform-fill-read");
                    return Some(node);
                }
            }
        }
    }
    let original = source.node(node).clone();
    if let Node::Block(statements) = original {
        let mut out = Vec::new();
        for statement in statements {
            if let Some(statement) = rewrite_node(
                target,
                source,
                res,
                statement,
                candidates,
                initializers,
                ranges,
            ) {
                out.push(statement);
            }
        }
        target
            .nodes
            .rewrite(node, Node::Block(out), "uniform-fill-scalarization");
    } else {
        let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
            rewrite_node(target, source, res, child, candidates, initializers, ranges)
                .unwrap_or(child)
        });
        target
            .nodes
            .rewrite(node, mapped, "uniform-fill-scalarization");
    }
    Some(node)
}

pub fn scalarize_uniform_fill_tables(ast: &mut Ast, root: NodeId) -> PassResult {
    let source = clone_ast(ast);
    let res = resolve(&source, root);
    let analyzer = EffectAnalyzer::new(&source, &res, root, true);
    let ranges = loop_ranges(&source, &res, root);
    let mut initializers = HashMap::<BindingId, NodeId>::new();
    let mut fills = HashMap::<BindingId, Vec<Fill>>::new();
    for node in walk(&source, root) {
        match source.node(node) {
            Node::Local(names, expressions) if names.len() == 1 => {
                let bids = res
                    .node_bids
                    .get(node as usize)
                    .cloned()
                    .unwrap_or_default();
                for (index, bid) in bids.into_iter().enumerate() {
                    if expressions.get(index).is_some_and(|expression| matches!(source.node(*expression), Node::Table(fields) if fields.is_empty())) {
                        initializers.insert(bid, node);
                    }
                }
            }
            Node::Assign(targets, expressions)
                if targets.len() == 1
                    && expressions.len() == 1
                    && matches!(source.node(targets[0]), Node::Name(_))
                    && matches!(source.node(expressions[0]), Node::Table(fields) if fields.is_empty()) =>
            {
                if let Some(bid) = res.node_bid.get(targets[0] as usize).copied().flatten() {
                    initializers.insert(bid, node);
                }
            }
            _ => {}
        }
        if let Some(fill) = uniform_fill(&source, &res, node, &analyzer) {
            fills.entry(fill.table_bid).or_default().push(fill);
        }
    }
    let mut candidates = fills
        .into_iter()
        .filter_map(|(bid, entries)| {
            (entries.len() == 1 && initializers.contains_key(&bid))
                .then(|| (bid, entries[0].clone()))
        })
        .collect::<HashMap<_, _>>();
    if candidates.is_empty() {
        return PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["scalarized=0".to_string()]),
        };
    }
    let mut invalid = HashSet::new();
    scan_invalid(
        &source,
        &res,
        root,
        None,
        ChildRole::Other,
        &candidates,
        &initializers,
        &ranges,
        &mut invalid,
    );
    candidates.retain(|bid, _| !invalid.contains(bid));
    if candidates.is_empty() {
        return PassResult {
            root,
            saved: Some(0),
            details: Some(vec!["scalarized=0".to_string()]),
        };
    }
    let count = candidates.len();
    let mut target = clone_ast(&source);
    let _ = rewrite_node(
        &mut target,
        &source,
        &res,
        root,
        &candidates,
        &initializers,
        &ranges,
    );
    *ast = target;
    PassResult {
        root,
        saved: None,
        details: Some(vec![format!("scalarized={count}")]),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    #[test]
    fn scalarizes_uniform_fill_and_preserves_outside_nil() {
        let source = "local a={} function onTick()for i=0,5 do a[i]=input.getNumber(18)end output.setNumber(1,a[3])output.setBool(1,a[6]==nil)end";
        let (mut ast, root) = parse_source(source).unwrap();
        let result = scalarize_uniform_fill_tables(&mut ast, root);
        assert!(result.details.as_ref().unwrap()[0].contains("=1"));
        let out = Printer::new(&ast, false).output(root);
        assert!(!out.contains("a[3]"), "{out}");
    }
}
