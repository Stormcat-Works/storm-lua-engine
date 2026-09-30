//! Ordered screen button outlining (`passes/screen-buttons.ts`).

use std::collections::HashMap;

use crate::pass::PassResult;
use crate::scope_rename::scope_rename_fast;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId};
use storm_lua_syntax::numeric::num_val;
use storm_lua_syntax::size::measure_size;

#[derive(Clone)]
struct Site {
    block: NodeId,
    start: usize,
    condition: NodeId,
    action_statement: NodeId,
    draw_statement: NodeId,
    x: NodeId,
    y: NodeId,
    label: NodeId,
    value: NodeId,
    hit: NodeId,
    active: NodeId,
    pulse: NodeId,
    target: NodeId,
    on: NodeId,
    off: NodeId,
    draw_fn: NodeId,
    offset: NodeId,
    key: String,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn renamed_size(ast: &Ast, root: NodeId) -> usize {
    let renamed = scope_rename_fast(ast, root);
    measure_size(&renamed.ast, renamed.root)
}

fn expr_key(ast: &Ast, res: &Resolution, node: NodeId) -> String {
    super::immutable_values::expression_key(ast, res, node)
}

fn action(ast: &Ast, res: &Resolution, statement: NodeId) -> Option<(NodeId, NodeId, NodeId)> {
    match ast.node(statement) {
        Node::If(arms, None) if arms.len() == 1 => {
            let Node::Block(statements) = ast.node(arms[0].body) else {
                return None;
            };
            if statements.len() != 1 {
                return None;
            }
            let Node::Assign(targets, values) = ast.node(statements[0]) else {
                return None;
            };
            if targets.len() == 1
                && values.len() == 1
                && matches!(ast.node(targets[0]), Node::Name(_))
            {
                Some((arms[0].cond, targets[0], values[0]))
            } else {
                None
            }
        }
        Node::Assign(targets, values)
            if targets.len() == 1
                && values.len() == 1
                && matches!(ast.node(targets[0]), Node::Name(_)) =>
        {
            let target_bid = res.node_bid.get(targets[0] as usize).copied().flatten()?;
            let Node::Bin(op, left, right) = ast.node(values[0]) else {
                return None;
            };
            if op != "or"
                || !matches!(ast.node(*right), Node::Name(_))
                || res.node_bid.get(*right as usize).copied().flatten() != Some(target_bid)
            {
                return None;
            }
            let Node::Bin(and_op, pulse, value) = ast.node(*left) else {
                return None;
            };
            (and_op == "and").then_some((*pulse, targets[0], *value))
        }
        _ => None,
    }
}

fn collect_sites(
    ast: &Ast,
    root: NodeId,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
) -> Vec<Site> {
    let mut sites = Vec::new();
    fn inspect(
        ast: &Ast,
        node: NodeId,
        res: &Resolution,
        analyzer: &EffectAnalyzer<'_>,
        sites: &mut Vec<Site>,
    ) {
        if let Node::Block(statements) = ast.node(node) {
            for start in 0..statements.len().saturating_sub(1) {
                let conditional = statements[start];
                let trailing = statements[start + 1];
                let Node::If(arms, Some(else_block)) = ast.node(conditional) else {
                    continue;
                };
                if arms.len() != 1 {
                    continue;
                }
                let Node::Block(no_statements) = ast.node(*else_block) else {
                    continue;
                };
                if no_statements.len() != 1 {
                    continue;
                }
                let Node::Callstat(trailing_call) = ast.node(trailing) else {
                    continue;
                };
                let Node::Call(draw_fn, draw_args, _) = ast.node(*trailing_call) else {
                    continue;
                };
                if draw_args.len() != 3 {
                    continue;
                }
                let Node::Bin(cond_op, hit_call, active) = ast.node(arms[0].cond) else {
                    continue;
                };
                if cond_op != "and" {
                    continue;
                }
                let Node::Call(hit_fn, hit_args, _) = ast.node(*hit_call) else {
                    continue;
                };
                if hit_args.len() != 4 {
                    continue;
                }
                let Node::Block(yes) = ast.node(arms[0].body) else {
                    continue;
                };
                if yes.len() != 2 {
                    continue;
                }
                let Some((pulse, target, value)) = action(ast, res, yes[0]) else {
                    continue;
                };
                let Node::Callstat(on_call) = ast.node(yes[1]) else {
                    continue;
                };
                let Node::Callstat(off_call) = ast.node(no_statements[0]) else {
                    continue;
                };
                let (Node::Call(_, _, _), Node::Call(_, _, _)) =
                    (ast.node(*on_call), ast.node(*off_call))
                else {
                    continue;
                };
                let x = hit_args[0];
                let y = hit_args[2];
                if expr_key(ast, res, y) != expr_key(ast, res, draw_args[1]) {
                    continue;
                }
                let Node::Bin(draw_op, draw_left, offset) = ast.node(draw_args[0]) else {
                    continue;
                };
                if draw_op != "+"
                    || expr_key(ast, res, *draw_left) != expr_key(ast, res, x)
                    || !matches!(ast.node(*offset), Node::Num(_))
                {
                    continue;
                }
                let effect = analyzer.effects_for_expr(value);
                if effect.calls || effect.ordered || !effect.writes.is_empty() {
                    continue;
                }
                let target_bid: Option<BindingId> =
                    res.node_bid.get(target as usize).copied().flatten();
                let offset_value = match ast.node(*offset) {
                    Node::Num(v) => num_val(v),
                    _ => unreachable!(),
                };
                let key = format!(
                    "{}|{}|{}|{}|{}|{:?}|{}|{}|{}|{}",
                    expr_key(ast, res, *hit_fn),
                    expr_key(ast, res, hit_args[1]),
                    expr_key(ast, res, hit_args[3]),
                    expr_key(ast, res, *active),
                    expr_key(ast, res, pulse),
                    target_bid,
                    expr_key(ast, res, *on_call),
                    expr_key(ast, res, *off_call),
                    expr_key(ast, res, *draw_fn),
                    offset_value
                );
                sites.push(Site {
                    block: node,
                    start,
                    condition: arms[0].cond,
                    action_statement: yes[0],
                    draw_statement: trailing,
                    x,
                    y,
                    label: draw_args[2],
                    value,
                    hit: *hit_call,
                    active: *active,
                    pulse,
                    target,
                    on: *on_call,
                    off: *off_call,
                    draw_fn: *draw_fn,
                    offset: *offset,
                    key,
                });
            }
        }
        let mut children = Vec::new();
        storm_lua_analysis::resolver::for_each_child(ast, node, &mut |c| children.push(c));
        for child in children {
            inspect(ast, child, res, analyzer, sites);
        }
    }
    inspect(ast, root, res, analyzer, &mut sites);
    sites
}

#[allow(clippy::too_many_arguments)]
fn replace_xy(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    node: NodeId,
    x_key: &str,
    y_key: &str,
    x_symbol: SymbolId,
    y_symbol: SymbolId,
) -> NodeId {
    let key = expr_key(source, res, node);
    if key == x_key {
        let result = target.push(Node::Name(x_symbol));
        target
            .nodes
            .derive_from(result, &source.nodes, node, "button-x-parameter");
        return result;
    }
    if key == y_key {
        let result = target.push(Node::Name(y_symbol));
        target
            .nodes
            .derive_from(result, &source.nodes, node, "button-y-parameter");
        return result;
    }
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        replace_xy(target, source, res, child, x_key, y_key, x_symbol, y_symbol)
    });
    let result = target.push(mapped);
    target
        .nodes
        .derive_from(result, &source.nodes, node, "button-expression-copy");
    result
}

pub fn outline_screen_buttons(ast: &mut Ast, root: NodeId, aggressive: bool) -> PassResult {
    if !aggressive || !matches!(ast.node(root), Node::Block(_)) {
        return PassResult {
            root,
            saved: None,
            details: Some(vec!["outlined=0;considered=0".into()]),
        };
    }
    let source = clone_ast(ast);
    let res = resolve(&source, root);
    let analyzer = EffectAnalyzer::new(&source, &res, root, true);
    let sites = collect_sites(&source, root, &res, &analyzer);
    let mut groups: HashMap<String, Vec<Site>> = HashMap::new();
    let mut order = Vec::new();
    for site in sites {
        if !groups.contains_key(&site.key) {
            order.push(site.key.clone());
        }
        groups.entry(site.key.clone()).or_default().push(site);
    }
    let baseline = renamed_size(&source, root);
    let mut best: Option<(Ast, usize, usize)> = None;
    let mut considered = 0;
    for key in order {
        let group = &groups[&key];
        if group.len() < 3 {
            continue;
        }
        for count in 3..=group.len() {
            considered += 1;
            let chosen = &group[..count];
            let first = &group[0];
            let mut candidate = clone_ast(&source);
            let helper = candidate.strings.intern("__stormmin_screen_button");
            let xs = candidate.strings.intern("__stormmin_button_x");
            let ys = candidate.strings.intern("__stormmin_button_y");
            let label = candidate.strings.intern("__stormmin_button_label");
            let value = candidate.strings.intern("__stormmin_button_value");
            let active_name = candidate.strings.intern("__stormmin_button_active");
            let x_key = expr_key(&source, &res, first.x);
            let y_key = expr_key(&source, &res, first.y);
            let hit = replace_xy(
                &mut candidate,
                &source,
                &res,
                first.hit,
                &x_key,
                &y_key,
                xs,
                ys,
            );
            let active_read = candidate.push(Node::Name(active_name));
            let active_assign_target = candidate.push(Node::Name(active_name));
            let active_expr = candidate.push(Node::Bin("and".into(), hit, first.active));
            let s_active =
                candidate.push(Node::Assign(vec![active_assign_target], vec![active_expr]));
            let pulse_cond = candidate.push(Node::Bin("and".into(), active_read, first.pulse));
            let target = first.target;
            let value_read = candidate.push(Node::Name(value));
            let assign = candidate.push(Node::Assign(vec![target], vec![value_read]));
            let pulse_body = candidate.push(Node::Block(vec![assign]));
            let pulse_if = candidate.push(Node::If(
                vec![storm_lua_syntax::ast::IfArm {
                    cond: pulse_cond,
                    body: pulse_body,
                }],
                None,
            ));
            let active_cond = candidate.push(Node::Name(active_name));
            let on_stat = candidate.push(Node::Callstat(first.on));
            let off_stat = candidate.push(Node::Callstat(first.off));
            let on_body = candidate.push(Node::Block(vec![on_stat]));
            let off_body = candidate.push(Node::Block(vec![off_stat]));
            let color_if = candidate.push(Node::If(
                vec![storm_lua_syntax::ast::IfArm {
                    cond: active_cond,
                    body: on_body,
                }],
                Some(off_body),
            ));
            let x_read = candidate.push(Node::Name(xs));
            let offset = first.offset;
            let draw_x = candidate.push(Node::Bin("+".into(), x_read, offset));
            let y_read = candidate.push(Node::Name(ys));
            let label_read = candidate.push(Node::Name(label));
            let draw_call = candidate.push(Node::Call(
                first.draw_fn,
                vec![draw_x, y_read, label_read],
                None,
            ));
            let draw_stat = candidate.push(Node::Callstat(draw_call));
            let body = candidate.push(Node::Block(vec![s_active, pulse_if, color_if, draw_stat]));
            let fun = candidate.push(Node::Function(vec![xs, ys, label, value], false, body));
            let helper_target = candidate.push(Node::Name(helper));
            let definition = candidate.push(Node::Funcstat(helper_target, fun));
            if candidate.nodes.tracks_origins() {
                for id in [active_assign_target, body, fun, helper_target, definition] {
                    candidate
                        .nodes
                        .mark_synthetic(id, "button-helper-storage-control");
                }
                let conditions = chosen.iter().map(|s| s.condition).collect::<Vec<_>>();
                let actions = chosen
                    .iter()
                    .map(|s| s.action_statement)
                    .collect::<Vec<_>>();
                let draws = chosen.iter().map(|s| s.draw_statement).collect::<Vec<_>>();
                let xsources = chosen.iter().map(|s| s.x).collect::<Vec<_>>();
                let ysources = chosen.iter().map(|s| s.y).collect::<Vec<_>>();
                let labels = chosen.iter().map(|s| s.label).collect::<Vec<_>>();
                let values = chosen.iter().map(|s| s.value).collect::<Vec<_>>();
                let ons = chosen.iter().map(|s| s.on).collect::<Vec<_>>();
                let offs = chosen.iter().map(|s| s.off).collect::<Vec<_>>();
                for (id, inputs) in [
                    (active_read, &conditions),
                    (active_expr, &conditions),
                    (s_active, &conditions),
                    (active_cond, &conditions),
                    (pulse_cond, &actions),
                    (value_read, &values),
                    (assign, &actions),
                    (pulse_body, &actions),
                    (pulse_if, &actions),
                    (color_if, &conditions),
                    (on_stat, &ons),
                    (on_body, &ons),
                    (off_stat, &offs),
                    (off_body, &offs),
                    (x_read, &xsources),
                    (draw_x, &draws),
                    (y_read, &ysources),
                    (label_read, &labels),
                    (draw_call, &draws),
                    (draw_stat, &draws),
                ] {
                    super::origins::derive(
                        &mut candidate,
                        id,
                        &source,
                        inputs,
                        "screen-button-outlining",
                    );
                }
            }
            let mut by_block: HashMap<NodeId, Vec<&Site>> = HashMap::new();
            for site in chosen {
                by_block.entry(site.block).or_default().push(site);
            }
            for (block, block_sites) in by_block {
                let Node::Block(statements) = source.node(block).clone() else {
                    continue;
                };
                let mut out = Vec::new();
                let mut i = 0;
                while i < statements.len() {
                    if let Some(site) = block_sites.iter().find(|s| s.start == i) {
                        let fnn = candidate.push(Node::Name(helper));
                        let call = candidate.push(Node::Call(
                            fnn,
                            vec![site.x, site.y, site.label, site.value],
                            None,
                        ));
                        candidate
                            .nodes
                            .mark_synthetic(fnn, "screen-button-helper-reference");
                        super::origins::derive(
                            &mut candidate,
                            call,
                            &source,
                            &[statements[i], statements[i + 1]],
                            "screen-button-invocation",
                        );
                        let stmt = candidate.push(Node::Callstat(call));
                        super::origins::derive(
                            &mut candidate,
                            stmt,
                            &source,
                            &[statements[i], statements[i + 1]],
                            "screen-button-invocation",
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
                    .rewrite(block, Node::Block(out), "screen-button-extraction");
            }
            let Node::Block(mut top) = candidate.node(root).clone() else {
                unreachable!()
            };
            top.insert(0, definition);
            candidate
                .nodes
                .rewrite(root, Node::Block(top), "screen-button-helper-insertion");
            let size = renamed_size(&candidate, root);
            if size < baseline && best.as_ref().is_none_or(|(_, b, _)| size < *b) {
                best = Some((candidate, size, count));
            }
        }
    }
    if let Some((candidate, _, count)) = best {
        *ast = candidate;
        PassResult {
            root,
            saved: None,
            details: Some(vec![format!("outlined={count};considered={considered}")]),
        }
    } else {
        PassResult {
            root,
            saved: None,
            details: Some(vec![format!("outlined=0;considered={considered}")]),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::{parse_source, Printer};
    #[test]
    fn outlines_three_buttons() {
        let src = r#"value=0 active=false pulse=false
function hit(x,w,y,h)return input.getBool(1)end
function onColor()screen.setColor(180,180,200)end
function offColor()screen.setColor(35,30,30)end
function onTick()
 active=input.getBool(2)
 pulse=input.getBool(3)
 output.setNumber(1,value)
end
function onDraw()
 if hit(1,5,2,5)and active then
  if pulse then value=value+1 end
  onColor()
 else offColor() end
 screen.drawText(1+1,2,"A")
 if hit(7,5,8,5)and active then
  if pulse then value=value+2 end
  onColor()
 else offColor() end
 screen.drawText(7+1,8,"B")
 if hit(13,5,14,5)and active then
  if pulse then value=value+3 end
  onColor()
 else offColor() end
 screen.drawText(13+1,14,"C")
end
"#;
        let (mut ast, root) = parse_source(src).unwrap();
        let r = outline_screen_buttons(&mut ast, root, true);
        assert!(r.details.unwrap()[0].contains("outlined=3"));
        let _ = Printer::new(&ast, false).output(root);
    }
}
