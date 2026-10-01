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
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, TableField};

#[derive(Clone)]
struct Carrier {
    bid: BindingId,
    key: String,
    expression: NodeId,
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
                    push_text(key, method);
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

fn dependencies_are_single_write(
    analyzer: &EffectAnalyzer<'_>,
    resolution: &Resolution,
    expression: NodeId,
) -> bool {
    analyzer
        .effects_for_expr(expression)
        .reads
        .iter()
        .all(|bid| {
            resolution
                .binding_write_counts
                .get(*bid as usize)
                .copied()
                .unwrap_or(0)
                <= 1
        })
}

fn safe_carrier_expression(
    ast: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    resolution: &Resolution,
    expression: NodeId,
) -> bool {
    let effect = analyzer.effects_for_expr(expression);
    !effect.calls
        && !effect.ordered
        && effect.writes.is_empty()
        && effect.stable
        && dependencies_are_single_write(analyzer, resolution, expression)
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
    let mut carriers = Vec::new();
    for statement in statements {
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
                        && safe_carrier_expression(ast, analyzer, resolution, expression)
                    {
                        carriers.push(Carrier {
                            bid,
                            key: expression_key(ast, resolution, expression),
                            expression,
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
                        && safe_carrier_expression(ast, analyzer, resolution, expression)
                    {
                        carriers.push(Carrier {
                            bid,
                            key: expression_key(ast, resolution, expression),
                            expression,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    carriers
}

fn collect_replacements(
    ast: &Ast,
    resolution: &Resolution,
    node: NodeId,
    in_function: bool,
    key: &str,
    output: &mut Vec<NodeId>,
) {
    if in_function
        && is_expression_type(ast.node(node))
        && expression_key(ast, resolution, node) == key
    {
        output.push(node);
        return;
    }
    let nested_in_function = in_function || matches!(ast.node(node), Node::Function(..));
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        collect_replacements(ast, resolution, child, nested_in_function, key, output);
    });
}

fn candidate_for_carrier(
    source: &Ast,
    root: NodeId,
    resolution: &Resolution,
    carrier: &Carrier,
) -> Option<(Ast, usize)> {
    let mut replacements = Vec::new();
    collect_replacements(
        source,
        resolution,
        root,
        false,
        &carrier.key,
        &mut replacements,
    );
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
    Some((candidate, replacements.len()))
}

pub fn reuse_immutable_values_with_options(
    ast: &mut Ast,
    root: NodeId,
    max_rounds: usize,
) -> PassResult {
    let original_size = measured(ast, root);
    let mut reused = 0usize;

    for _ in 0..max_rounds {
        let source = clone_ast(ast);
        let resolution = resolve(&source, root);
        let analyzer = EffectAnalyzer::new(&source, &resolution, root, true);
        let carriers = collect_carriers(&source, root, &resolution, &analyzer);
        let baseline = measured(&source, root);
        let mut best: Option<(Ast, usize, usize)> = None;

        for carrier in carriers {
            let Some((candidate, replacements)) =
                candidate_for_carrier(&source, root, &resolution, &carrier)
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
            "local template={x=1}function make()return{x=1}end function onTick()local a=make()local b=make()output.setBool(1,a~=b)end"
        );
    }

    #[test]
    fn respects_shadowed_bindings() {
        let source =
            "a=1 b=2 carrier=a+b function onTick()local a=3 local b=4 output.setNumber(1,a+b)end";
        assert_eq!(output(source), source);
    }

    #[test]
    fn ignores_atomic_carriers() {
        let source = "local one=1 function onTick()output.setNumber(1,1)end";
        assert_eq!(output(source), source);
    }
}
