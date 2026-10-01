//! Destructive radix helper synthesis/reuse (`passes/radix-helpers.ts`).

use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use crate::scope_rename::measure_renamed_size;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId};
use storm_lua_syntax::size::{measure_expr, measure_stmt};

#[derive(Clone)]
struct Site {
    block: NodeId,
    start: usize,
    target_bid: BindingId,
    target_name: SymbolId,
    remainder_bid: BindingId,
    remainder_name: SymbolId,
    divisor: NodeId,
    estimate: isize,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn renamed_size(ast: &Ast, root: NodeId) -> usize {
    measure_renamed_size(ast, root)
}

fn same_expr(ast: &Ast, res: &Resolution, a: NodeId, b: NodeId) -> bool {
    super::immutable_values::expression_key(ast, res, a)
        == super::immutable_values::expression_key(ast, res, b)
}

fn pair_at(
    ast: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    block: NodeId,
    start: usize,
) -> Option<Site> {
    let Node::Block(statements) = ast.node(block) else {
        return None;
    };
    let remainder_stmt = *statements.get(start)?;
    let quotient_stmt = *statements.get(start + 1)?;
    let (
        Node::Assign(remainder_targets, remainder_values),
        Node::Assign(quotient_targets, quotient_values),
    ) = (ast.node(remainder_stmt), ast.node(quotient_stmt))
    else {
        return None;
    };
    if remainder_targets.len() != 1
        || remainder_values.len() != 1
        || quotient_targets.len() != 1
        || quotient_values.len() != 1
    {
        return None;
    }
    let (remainder_target, quotient_target) = (remainder_targets[0], quotient_targets[0]);
    if !matches!(ast.node(remainder_target), Node::Name(_))
        || !matches!(ast.node(quotient_target), Node::Name(_))
    {
        return None;
    }
    let remainder_bid = res
        .node_bid
        .get(remainder_target as usize)
        .copied()
        .flatten()?;
    let target_bid = res
        .node_bid
        .get(quotient_target as usize)
        .copied()
        .flatten()?;
    if remainder_bid == target_bid {
        return None;
    }
    let (Node::Bin(rem_op, rem_left, rem_div), Node::Bin(quo_op, quo_left, quo_div)) =
        (ast.node(remainder_values[0]), ast.node(quotient_values[0]))
    else {
        return None;
    };
    if rem_op != "%"
        || quo_op != "//"
        || !matches!(ast.node(*rem_left), Node::Name(_))
        || !matches!(ast.node(*quo_left), Node::Name(_))
    {
        return None;
    }
    if res.node_bid.get(*rem_left as usize).copied().flatten() != Some(target_bid)
        || res.node_bid.get(*quo_left as usize).copied().flatten() != Some(target_bid)
        || !same_expr(ast, res, *rem_div, *quo_div)
    {
        return None;
    }
    let effect = analyzer.effects_for_expr(*rem_div);
    if effect.calls
        || effect.ordered
        || !effect.writes.is_empty()
        || effect.reads.contains(&remainder_bid)
        || effect.reads.contains(&target_bid)
    {
        return None;
    }
    let target = &res.bindings[target_bid as usize];
    let remainder = &res.bindings[remainder_bid as usize];
    if target.kind != BindingKind::Global
        || remainder.kind != BindingKind::Global
        || target.fixed
        || remainder.fixed
    {
        return None;
    }
    let old = measure_stmt(ast, remainder_stmt) + measure_stmt(ast, quotient_stmt);
    let estimate = old as isize - measure_expr(ast, *rem_div) as isize - 3;
    Some(Site {
        block,
        start,
        target_bid,
        target_name: target.name,
        remainder_bid,
        remainder_name: remainder.name,
        divisor: *rem_div,
        estimate,
    })
}

fn collect_sites(
    ast: &Ast,
    root: NodeId,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
) -> Vec<Vec<Site>> {
    let mut groups: Vec<Vec<Site>> = Vec::new();
    let mut group_map: HashMap<(BindingId, BindingId), usize> = HashMap::new();
    fn visit(
        ast: &Ast,
        node: NodeId,
        res: &Resolution,
        analyzer: &EffectAnalyzer<'_>,
        inside: bool,
        groups: &mut Vec<Vec<Site>>,
        group_map: &mut HashMap<(BindingId, BindingId), usize>,
    ) {
        if let Node::Function(_, _, body) = ast.node(node) {
            visit(ast, *body, res, analyzer, true, groups, group_map);
            return;
        }
        if inside {
            if let Node::Block(statements) = ast.node(node) {
                let mut i = 0;
                while i + 1 < statements.len() {
                    if let Some(site) = pair_at(ast, res, analyzer, node, i) {
                        let key = (site.target_bid, site.remainder_bid);
                        if let Some(&index) = group_map.get(&key) {
                            groups[index].push(site);
                        } else {
                            group_map.insert(key, groups.len());
                            groups.push(vec![site]);
                        }
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
        }
        let mut children = Vec::new();
        storm_lua_analysis::resolver::for_each_child(ast, node, &mut |c| children.push(c));
        for child in children {
            visit(ast, child, res, analyzer, inside, groups, group_map);
        }
    }
    visit(ast, root, res, analyzer, false, &mut groups, &mut group_map);
    groups
}

fn used_names(ast: &Ast, root: NodeId) -> HashSet<String> {
    let mut nodes = Vec::new();
    storm_lua_analysis::effects::walk(ast, root, &mut nodes);
    nodes
        .into_iter()
        .filter_map(|n| match ast.node(n) {
            Node::Name(s) => Some(ast.strings.get(*s).to_string()),
            _ => None,
        })
        .collect()
}

pub fn synthesize_destructive_radix_helpers(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    max_rounds: usize,
) -> PassResult {
    if !aggressive || !matches!(ast.node(root), Node::Block(_)) {
        return PassResult {
            root,
            saved: None,
            details: Some(vec!["synthesized=0;considered=0".into()]),
        };
    }
    let mut synthesized = 0;
    let mut considered = 0;
    for _ in 0..max_rounds {
        let source = clone_ast(ast);
        let res = resolve(&source, root);
        let analyzer = EffectAnalyzer::new(&source, &res, root, true);
        let groups = collect_sites(&source, root, &res, &analyzer);
        let baseline = renamed_size(&source, root);
        let names = used_names(&source, root);
        let mut best: Option<(Ast, usize)> = None;
        for sites in groups.into_iter().filter(|g| g.len() >= 3).take(16) {
            let mut ranked = sites;
            ranked.sort_by_key(|s| std::cmp::Reverse(s.estimate));
            for count in 3..=ranked.len() {
                considered += 1;
                let selected = ranked.iter().take(count).cloned().collect::<Vec<_>>();
                let first = &ranked[0];
                let mut serial = 0;
                let (helper, param) = loop {
                    let h = format!("__stormmin_radix_{serial}");
                    let p = format!("__stormmin_radix_base_{serial}");
                    if !names.contains(&h) && !names.contains(&p) {
                        break (h, p);
                    }
                    serial += 1;
                };
                let mut candidate = clone_ast(&source);
                let helper_sym = candidate.strings.intern(&helper);
                let param_sym = candidate.strings.intern(&param);
                let mut by_block: HashMap<NodeId, Vec<&Site>> = HashMap::new();
                for site in &selected {
                    by_block.entry(site.block).or_default().push(site);
                }
                for (block, sites) in by_block {
                    let Node::Block(statements) = source.node(block).clone() else {
                        continue;
                    };
                    let mut out = Vec::new();
                    let mut i = 0;
                    while i < statements.len() {
                        if let Some(site) = sites.iter().find(|s| s.start == i) {
                            let fnn = candidate.push(Node::Name(helper_sym));
                            let call = candidate.push(Node::Call(fnn, vec![site.divisor], None));
                            candidate
                                .nodes
                                .mark_synthetic(fnn, "radix-helper-reference");
                            super::origins::derive(
                                &mut candidate,
                                call,
                                &source,
                                &[statements[i], statements[i + 1]],
                                "radix-helper-invocation",
                            );
                            let stmt = candidate.push(Node::Callstat(call));
                            super::origins::derive(
                                &mut candidate,
                                stmt,
                                &source,
                                &[statements[i], statements[i + 1]],
                                "radix-helper-invocation",
                            );
                            out.push(stmt);
                            i += 2;
                        } else {
                            out.push(statements[i]);
                            i += 1;
                        }
                    }
                    candidate
                        .nodes
                        .rewrite(block, Node::Block(out), "radix-helper-extraction");
                }
                let rem_target = candidate.push(Node::Name(first.remainder_name));
                let target_read1 = candidate.push(Node::Name(first.target_name));
                let p1 = candidate.push(Node::Name(param_sym));
                let rem_expr = candidate.push(Node::Bin("%".into(), target_read1, p1));
                let s1 = candidate.push(Node::Assign(vec![rem_target], vec![rem_expr]));
                let target_write = candidate.push(Node::Name(first.target_name));
                let target_read2 = candidate.push(Node::Name(first.target_name));
                let p2 = candidate.push(Node::Name(param_sym));
                let quo = candidate.push(Node::Bin("//".into(), target_read2, p2));
                let s2 = candidate.push(Node::Assign(vec![target_write], vec![quo]));
                let body = candidate.push(Node::Block(vec![s1, s2]));
                let fun = candidate.push(Node::Function(vec![param_sym], false, body));
                let target = candidate.push(Node::Name(helper_sym));
                let def = candidate.push(Node::Funcstat(target, fun));
                if candidate.nodes.tracks_origins() {
                    let mut rem_sites = Vec::new();
                    let mut quo_sites = Vec::new();
                    let mut rem_targets = Vec::new();
                    let mut quo_targets = Vec::new();
                    let mut rem_reads = Vec::new();
                    let mut quo_reads = Vec::new();
                    let mut rem_values = Vec::new();
                    let mut quo_values = Vec::new();
                    let mut divisors = Vec::new();
                    for site in &selected {
                        let Node::Block(ss) = source.node(site.block) else {
                            unreachable!()
                        };
                        let (rs, qs) = (ss[site.start], ss[site.start + 1]);
                        let (Node::Assign(rv, re), Node::Assign(qv, qe)) =
                            (source.node(rs), source.node(qs))
                        else {
                            unreachable!()
                        };
                        let (Node::Bin(_, rl, rr), Node::Bin(_, ql, qr)) =
                            (source.node(re[0]), source.node(qe[0]))
                        else {
                            unreachable!()
                        };
                        rem_sites.push(rs);
                        quo_sites.push(qs);
                        rem_targets.push(rv[0]);
                        quo_targets.push(qv[0]);
                        rem_reads.push(*rl);
                        quo_reads.push(*ql);
                        rem_values.push(re[0]);
                        quo_values.push(qe[0]);
                        divisors.extend([*rr, *qr]);
                    }
                    for (id, inputs) in [
                        (rem_target, &rem_targets),
                        (target_write, &quo_targets),
                        (target_read1, &rem_reads),
                        (target_read2, &quo_reads),
                        (rem_expr, &rem_values),
                        (quo, &quo_values),
                        (s1, &rem_sites),
                        (s2, &quo_sites),
                        (p1, &divisors),
                        (p2, &divisors),
                    ] {
                        super::origins::derive(
                            &mut candidate,
                            id,
                            &source,
                            inputs,
                            "shared-radix-operation",
                        );
                    }
                    for id in [body, fun, target, def] {
                        candidate.nodes.mark_synthetic(id, "radix-helper-control");
                    }
                }
                let Node::Block(mut top) = candidate.node(root).clone() else {
                    unreachable!()
                };
                top.insert(0, def);
                candidate
                    .nodes
                    .rewrite(root, Node::Block(top), "radix-helper-insertion");
                let size = renamed_size(&candidate, root);
                if size < baseline && best.as_ref().is_none_or(|(_, b)| size < *b) {
                    best = Some((candidate, size));
                }
            }
        }
        let Some((candidate, _)) = best else { break };
        *ast = candidate;
        synthesized += 1;
    }
    PassResult {
        root,
        saved: None,
        details: Some(vec![format!(
            "synthesized={synthesized};considered={considered}"
        )]),
    }
}

#[derive(Clone)]
struct Helper {
    name: SymbolId,
    target_bid: BindingId,
    remainder_bid: BindingId,
}
fn find_helper(ast: &Ast, root: NodeId, res: &Resolution) -> Option<Helper> {
    let Node::Block(statements) = ast.node(root) else {
        return None;
    };
    for statement in statements {
        let Node::Funcstat(target, function) = ast.node(*statement) else {
            continue;
        };
        let Node::Name(name) = ast.node(*target) else {
            continue;
        };
        let Node::Function(_, _, body) = ast.node(*function) else {
            continue;
        };
        let param_bid = *res.node_bids.get(*function as usize)?.first()?;
        let Node::Block(ss) = ast.node(*body) else {
            continue;
        };
        if ss.len() != 2 {
            continue;
        }
        let (Node::Assign(rv, re), Node::Assign(qv, qe)) = (ast.node(ss[0]), ast.node(ss[1]))
        else {
            continue;
        };
        if rv.len() != 1 || re.len() != 1 || qv.len() != 1 || qe.len() != 1 {
            continue;
        }
        let (Node::Name(_), Node::Name(_)) = (ast.node(rv[0]), ast.node(qv[0])) else {
            continue;
        };
        let remainder_bid = res.node_bid.get(rv[0] as usize).copied().flatten()?;
        let target_bid = res.node_bid.get(qv[0] as usize).copied().flatten()?;
        let (Node::Bin(ro, rl, rr), Node::Bin(qo, ql, qr)) = (ast.node(re[0]), ast.node(qe[0]))
        else {
            continue;
        };
        if ro != "%"
            || qo != "//"
            || !matches!(ast.node(*rl), Node::Name(_))
            || !matches!(ast.node(*ql), Node::Name(_))
            || !matches!(ast.node(*rr), Node::Name(_))
            || !matches!(ast.node(*qr), Node::Name(_))
        {
            continue;
        }
        if res.node_bid.get(*rr as usize).copied().flatten() != Some(param_bid)
            || res.node_bid.get(*qr as usize).copied().flatten() != Some(param_bid)
            || res.node_bid.get(*rl as usize).copied().flatten() != Some(target_bid)
            || res.node_bid.get(*ql as usize).copied().flatten() != Some(target_bid)
        {
            continue;
        }
        if res.bindings[target_bid as usize].kind == BindingKind::Global
            && res.bindings[remainder_bid as usize].kind == BindingKind::Global
        {
            return Some(Helper {
                name: *name,
                target_bid,
                remainder_bid,
            });
        }
    }
    None
}

fn direct_write(ast: &Ast, res: &Resolution, statement: NodeId, bid: BindingId) -> bool {
    matches!(ast.node(statement),Node::Assign(targets,_) if targets.iter().any(|t|matches!(ast.node(*t),Node::Name(_))&&res.node_bid.get(*t as usize).copied().flatten()==Some(bid)))
}
fn contains_call(ast: &Ast, node: NodeId) -> bool {
    let mut nodes = Vec::new();
    storm_lua_analysis::effects::walk(ast, node, &mut nodes);
    nodes
        .into_iter()
        .any(|n| matches!(ast.node(n), Node::Call(..)))
}
fn scan_replacements(
    ast: &Ast,
    res: &Resolution,
    node: NodeId,
    target: BindingId,
    divisor: NodeId,
    repl: &mut Vec<NodeId>,
    valid: &mut bool,
) {
    if !*valid {
        return;
    }
    if let Node::Bin(op, left, right) = ast.node(node) {
        if op == "//"
            && matches!(ast.node(*left), Node::Name(_))
            && res.node_bid.get(*left as usize).copied().flatten() == Some(target)
            && same_expr(ast, res, *right, divisor)
        {
            repl.push(node);
            return;
        }
    }
    if matches!(ast.node(node), Node::Name(_))
        && res.node_bid.get(node as usize).copied().flatten() == Some(target)
        && !res.node_write.get(node as usize).copied().unwrap_or(false)
    {
        *valid = false;
        return;
    }
    let mut children = Vec::new();
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |c| children.push(c));
    for c in children {
        scan_replacements(ast, res, c, target, divisor, repl, valid);
    }
}

pub fn reuse_terminal_radix_quotients(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    max_rounds: usize,
) -> PassResult {
    if !aggressive {
        return PassResult {
            root,
            saved: None,
            details: Some(vec!["reused=0;considered=0".into()]),
        };
    }
    let mut reused = 0;
    let mut considered = 0;
    for _ in 0..max_rounds {
        let source = clone_ast(ast);
        let res = resolve(&source, root);
        let Some(helper) = find_helper(&source, root, &res) else {
            break;
        };
        let baseline = renamed_size(&source, root);
        let mut best: Option<(Ast, usize)> = None;
        #[allow(clippy::too_many_arguments)]
        fn visit(
            ast: &Ast,
            node: NodeId,
            res: &Resolution,
            helper: &Helper,
            baseline: usize,
            root: NodeId,
            best: &mut Option<(Ast, usize)>,
            considered: &mut usize,
        ) {
            if let Node::Function(_, _, body) = ast.node(node) {
                visit(ast, *body, res, helper, baseline, root, best, considered);
                return;
            }
            if let Node::Block(statements) = ast.node(node) {
                for start in 0..statements.len() {
                    let statement = statements[start];
                    let Node::Assign(targets, values) = ast.node(statement) else {
                        continue;
                    };
                    if targets.len() != 1
                        || values.len() != 1
                        || !matches!(ast.node(targets[0]), Node::Name(_))
                        || res.node_bid.get(targets[0] as usize).copied().flatten()
                            != Some(helper.remainder_bid)
                    {
                        continue;
                    }
                    let Node::Bin(op, left, divisor) = ast.node(values[0]) else {
                        continue;
                    };
                    if op != "%"
                        || !matches!(ast.node(*left), Node::Name(_))
                        || res.node_bid.get(*left as usize).copied().flatten()
                            != Some(helper.target_bid)
                    {
                        continue;
                    }
                    let mut replacements = Vec::new();
                    let mut boundary = None;
                    let mut valid = true;
                    for (idx, later) in statements.iter().copied().enumerate().skip(start + 1) {
                        if direct_write(ast, res, later, helper.target_bid) {
                            boundary = Some(idx);
                            break;
                        }
                        if contains_call(ast, later) {
                            valid = false;
                            break;
                        }
                        scan_replacements(
                            ast,
                            res,
                            later,
                            helper.target_bid,
                            *divisor,
                            &mut replacements,
                            &mut valid,
                        );
                        if !valid {
                            break;
                        }
                    }
                    if boundary.is_none() {
                        continue;
                    }
                    if !valid || replacements.is_empty() {
                        continue;
                    }
                    *considered += 1;
                    let mut candidate = clone_ast(ast);
                    let target_name = res.bindings[helper.target_bid as usize].name;
                    for r in replacements {
                        candidate
                            .nodes
                            .rewrite(r, Node::Name(target_name), "terminal-radix-reuse");
                    }
                    let fnn = candidate.push(Node::Name(helper.name));
                    let call = candidate.push(Node::Call(fnn, vec![*divisor], None));
                    candidate
                        .nodes
                        .mark_synthetic(fnn, "terminal-radix-helper-reference");
                    candidate
                        .nodes
                        .derive_from(call, &ast.nodes, values[0], "terminal-radix-call");
                    candidate.nodes.rewrite(
                        statement,
                        Node::Callstat(call),
                        "terminal-radix-reuse",
                    );
                    let size = renamed_size(&candidate, root);
                    if size < baseline && best.as_ref().is_none_or(|(_, b)| size < *b) {
                        *best = Some((candidate, size));
                    }
                }
            }
            let mut children = Vec::new();
            storm_lua_analysis::resolver::for_each_child(ast, node, &mut |c| children.push(c));
            for c in children {
                visit(ast, c, res, helper, baseline, root, best, considered);
            }
        }
        visit(
            &source,
            root,
            &res,
            &helper,
            baseline,
            root,
            &mut best,
            &mut considered,
        );
        let Some((candidate, _)) = best else { break };
        *ast = candidate;
        reused += 1;
    }
    PassResult {
        root,
        saved: None,
        details: Some(vec![format!("reused={reused};considered={considered}")]),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::{parse_source, Printer};
    #[test]
    fn synthesizes_three_pairs() {
        let src="function onTick()p=input.getNumber(1)r=p%100001 p=p//100001 r=p%1251 p=p//1251 r=p%60001 p=p//60001 r=p%15001 p=p//15001 r=p%100001 p=p//100001 output.setNumber(1,p+r)end";
        let (mut ast, root) = parse_source(src).unwrap();
        let r = synthesize_destructive_radix_helpers(&mut ast, root, true, 4);
        assert!(r.details.unwrap()[0].contains("synthesized=1"));
        let _ = Printer::new(&ast, false).output(root);
    }
}
