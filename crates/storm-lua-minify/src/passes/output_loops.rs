//! Output sequence loop synthesis (`passes/output-loops.ts`).
//!
//! Six or more consecutive `output.setNumber` or `output.setBool` calls for
//! channels 1..N can be represented as a table plus numeric-for loop when each
//! value is movable (or reads only explicitly trusted tables). The TypeScript
//! pass uses normalized one-letter names for its profitability check; this
//! implementation reproduces that metric exactly.

use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, TableField};
use storm_lua_syntax::numeric::num_val;
use storm_lua_syntax::print::Printer;
use storm_lua_syntax::size::measure_size;

const OUTPUT_CALLS: &[&str] = &["output.setNumber", "output.setBool"];

#[derive(Clone)]
struct OutputCall {
    expression: NodeId,
    function: NodeId,
    args: Vec<NodeId>,
    builtin: String,
}

fn output_call(ast: &Ast, analyzer: &EffectAnalyzer<'_>, statement: NodeId) -> Option<OutputCall> {
    let Node::Callstat(expression) = ast.node(statement) else {
        return None;
    };
    let Node::Call(function, args, _) = ast.node(*expression) else {
        return None;
    };
    if args.len() != 2 {
        return None;
    }
    let builtin = analyzer.resolve_builtin_reference(*function)?;
    if !OUTPUT_CALLS.contains(&builtin.as_str()) {
        return None;
    }
    Some(OutputCall {
        expression: *expression,
        function: *function,
        args: args.clone(),
        builtin,
    })
}

fn statement_size(ast: &Ast, statement: NodeId) -> usize {
    Printer::new(ast, false).stat_public(statement).len()
}

fn trusted_pure(
    source: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    expression: NodeId,
    trusted_tables: &HashSet<String>,
) -> bool {
    if trusted_tables.is_empty() {
        return false;
    }
    let effect = analyzer.effects_for_expr(expression);
    !effect.calls
        && !effect.ordered
        && !effect.may_throw
        && effect.writes.is_empty()
        && effect.reads.iter().all(|bid| {
            let binding = res.binding(*bid);
            !matches!(binding.kind, BindingKind::Global)
                || trusted_tables.contains(source.strings.get(binding.name))
        })
}

fn normalized_original_cost(ast: &mut Ast, calls: &[OutputCall]) -> usize {
    calls
        .iter()
        .map(|call| {
            let function = ast.name("f");
            let expression = ast.call(function, call.args.clone(), None);
            let statement = ast.callstat(expression);
            statement_size(ast, statement)
        })
        .sum()
}

fn normalized_candidate_cost(ast: &mut Ast, values: &[NodeId]) -> usize {
    let table = ast.table(values.iter().copied().map(TableField::Arr).collect());
    let target = ast.name("a");
    let assignment = ast.assign(vec![target], vec![table]);

    let loop_index_for_channel = ast.name("b");
    let table_name = ast.name("a");
    let loop_index_for_key = ast.name("b");
    let lookup = ast.index(table_name, loop_index_for_key, false);
    let function = ast.name("f");
    let call = ast.call(function, vec![loop_index_for_channel, lookup], None);
    let call_statement = ast.callstat(call);
    let body = ast.block(vec![call_statement]);
    let start = ast.num("1");
    let end = ast.num(&values.len().to_string());
    let loop_statement = ast.fornum("b", start, end, None, body);
    statement_size(ast, assignment) + statement_size(ast, loop_statement)
}

fn build_candidate(
    ast: &mut Ast,
    source: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    calls: &[OutputCall],
    trusted_tables: &HashSet<String>,
    sequence: &mut usize,
) -> Option<Vec<NodeId>> {
    if calls.len() < 6 {
        return None;
    }
    for (index, call) in calls.iter().enumerate() {
        let Node::Num(channel) = source.node(call.args[0]) else {
            return None;
        };
        if num_val(channel) != (index + 1) as f64 {
            return None;
        }
        let value = call.args[1];
        if !analyzer.is_movable(value)
            && !trusted_pure(source, res, analyzer, value, trusted_tables)
        {
            return None;
        }
    }
    let values = calls.iter().map(|call| call.args[1]).collect::<Vec<_>>();
    let original_cost = normalized_original_cost(ast, calls);
    let candidate_cost = normalized_candidate_cost(ast, &values);
    if candidate_cost + 3 >= original_cost {
        return None;
    }

    let table_symbol = ast
        .strings
        .fresh(&format!("__stormmin_output_values_{}", *sequence));
    let index_symbol = ast
        .strings
        .fresh(&format!("__stormmin_output_index_{}", *sequence));
    let table_name = ast.strings.get(table_symbol).to_string();
    let index_name = ast.strings.get(index_symbol).to_string();
    let table = ast.table(values.into_iter().map(TableField::Arr).collect());
    ast.nodes.mark_synthetic(table, "output-loop-value-storage");
    let target = ast.name(&table_name);
    let assignment = ast.assign(vec![target], vec![table]);

    let channel = ast.name(&index_name);
    let table_ref = ast.name(&table_name);
    let key = ast.name(&index_name);
    let lookup = ast.index(table_ref, key, false);
    let call = ast.call(calls[0].function, vec![channel, lookup], None);
    let statement = ast.callstat(call);
    let body = ast.block(vec![statement]);
    let start = ast.num("1");
    let end = ast.num(&calls.len().to_string());
    let loop_statement = ast.fornum(&index_name, start, end, None, body);
    if ast.nodes.tracks_origins() {
        for id in [
            target,
            assignment,
            table_ref,
            key,
            body,
            start,
            end,
            loop_statement,
        ] {
            ast.nodes.mark_synthetic(id, "output-loop-control");
        }
        let channels = calls.iter().map(|c| c.args[0]).collect::<Vec<_>>();
        super::origins::derive(
            ast,
            channel,
            source,
            &channels,
            "output-loop-channel-series",
        );
        let originals = calls.iter().map(|c| c.args[1]).collect::<Vec<_>>();
        super::origins::derive(
            ast,
            lookup,
            source,
            &originals,
            "output-loop-value-selection",
        );
        let sites = calls.iter().map(|c| c.expression).collect::<Vec<_>>();
        super::origins::derive(ast, call, source, &sites, "output-loop-replay");
        super::origins::derive(ast, statement, source, &sites, "output-loop-replay");
    }
    *sequence += 1;
    Some(vec![assignment, loop_statement])
}

/// Conservative active-local maximum of the enclosing function for
/// every block in the chunk, keyed by block node. The chunk itself is
/// the outermost function (zero parameters); method definitions add
/// their implicit `self` parameter. The synthesized numeric-for loop
/// needs four control locals (variable + limit/step/state) at the
/// sequence's block, so synthesis is skipped when the enclosing
/// function is already at the VM's simultaneous-local limit.
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

/// Locals a synthesized numeric-for loop adds at its block: the loop
/// variable plus the three hidden control values.
const FORNUM_LOCALS: usize = 4;

#[allow(clippy::too_many_arguments)]
fn transform_node(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    trusted_tables: &HashSet<String>,
    budgets: &HashMap<NodeId, usize>,
    sequence: &mut usize,
    synthesized: &mut usize,
) -> NodeId {
    if matches!(source.node(node), Node::Block(_)) {
        return transform_block(
            target,
            source,
            res,
            analyzer,
            node,
            trusted_tables,
            budgets,
            sequence,
            synthesized,
        );
    }
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        transform_node(
            target,
            source,
            res,
            analyzer,
            child,
            trusted_tables,
            budgets,
            sequence,
            synthesized,
        )
    });
    target
        .nodes
        .rewrite(node, mapped, "output-sequence-loop-synthesis");
    node
}

#[allow(clippy::too_many_arguments)]
fn transform_block(
    target: &mut Ast,
    source: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    block: NodeId,
    trusted_tables: &HashSet<String>,
    budgets: &HashMap<NodeId, usize>,
    sequence: &mut usize,
    synthesized: &mut usize,
) -> NodeId {
    let Node::Block(statements) = source.node(block).clone() else {
        unreachable!("transform_block requires block")
    };
    let nested = statements
        .into_iter()
        .map(|statement| {
            transform_node(
                target,
                source,
                res,
                analyzer,
                statement,
                trusted_tables,
                budgets,
                sequence,
                synthesized,
            )
        })
        .collect::<Vec<_>>();
    let mut output = Vec::with_capacity(nested.len());
    let mut index = 0usize;
    while index < nested.len() {
        let Some(first) = output_call(source, analyzer, nested[index]) else {
            output.push(nested[index]);
            index += 1;
            continue;
        };
        let mut calls = Vec::new();
        let mut end = index;
        while end < nested.len() {
            let Some(call) = output_call(source, analyzer, nested[end]) else {
                break;
            };
            if call.builtin != first.builtin {
                break;
            }
            calls.push(call);
            end += 1;
        }
        let over_budget = budgets.get(&block).copied().unwrap_or(0) + FORNUM_LOCALS
            > super::single_member_cache::MAX_LOCALS;
        if let Some(candidate) = if over_budget {
            None
        } else {
            build_candidate(
                target,
                source,
                res,
                analyzer,
                &calls,
                trusted_tables,
                sequence,
            )
        } {
            output.extend(candidate);
            *synthesized += 1;
        } else {
            output.extend_from_slice(&nested[index..end]);
        }
        index = end;
    }
    target
        .nodes
        .rewrite(block, Node::Block(output), "output-sequence-loop-synthesis");
    block
}

pub fn synthesize_output_loops_with_trusted_tables(
    ast: &mut Ast,
    root: NodeId,
    trusted_tables: HashSet<String>,
) -> PassResult {
    let before = measure_size(ast, root);
    let mut source = storm_lua_syntax::ast_utils::inherit_ast(ast);
    source.nodes = ast.nodes.clone();
    let res = resolve(&source, root);
    let analyzer = EffectAnalyzer::new(&source, &res, root, true);
    let budgets = block_local_budgets(&source, root);
    let mut sequence = 0usize;
    let mut synthesized = 0usize;
    let out = transform_block(
        ast,
        &source,
        &res,
        &analyzer,
        root,
        &trusted_tables,
        &budgets,
        &mut sequence,
        &mut synthesized,
    );
    PassResult {
        root: out,
        saved: Some(before.saturating_sub(measure_size(ast, out)) as u64),
        details: if synthesized == 0 {
            None
        } else {
            Some(vec![format!("synthesized={synthesized}")])
        },
    }
}

pub fn synthesize_output_loops(ast: &mut Ast, root: NodeId) -> PassResult {
    synthesize_output_loops_with_trusted_tables(ast, root, HashSet::new())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::{parse_source, Printer};

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = synthesize_output_loops(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn synthesizes_six_numeric_outputs() {
        let source = "function onTick()output.setNumber(1,123456789012345)output.setNumber(2,123456789012345)output.setNumber(3,123456789012345)output.setNumber(4,123456789012345)output.setNumber(5,123456789012345)output.setNumber(6,123456789012345)output.setNumber(7,123456789012345)output.setNumber(8,123456789012345)end";
        let out = output(source);
        assert!(
            out.contains("for __stormmin_output_index_0=1,8 do"),
            "{out}"
        );
        assert!(out.contains("__stormmin_output_values_0"), "{out}");
    }

    #[test]
    fn requires_channels_starting_at_one() {
        let source = "output.setNumber(2,11)output.setNumber(3,22)output.setNumber(4,33)output.setNumber(5,44)output.setNumber(6,55)output.setNumber(7,66)";
        assert!(!output(source).contains("for __stormmin_output_index"));
    }

    #[test]
    fn rejects_effectful_values() {
        let source = "output.setNumber(1,f())output.setNumber(2,2)output.setNumber(3,3)output.setNumber(4,4)output.setNumber(5,5)output.setNumber(6,6)";
        assert!(!output(source).contains("for __stormmin_output_index"));
    }
    #[test]
    fn skips_synthesis_when_function_is_at_the_local_limit() {
        // 197 parameters + the synthesized loop's four control locals
        // would exceed the VM's 200 simultaneous-local limit.
        let params = (1..=197)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let calls = (1..=8)
            .map(|i| format!("output.setNumber({i},p{i})"))
            .collect::<String>();
        let source = format!("local function big({params}){calls}return p1 end");
        assert!(!output(&source).contains("__stormmin_output_index"));
    }

    #[test]
    fn synthesizes_below_the_local_limit() {
        let params = (1..=196)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let calls = (1..=8)
            .map(|i| format!("output.setNumber({i},p{i})"))
            .collect::<String>();
        let source = format!("local function big({params}){calls}return p1 end");
        assert!(output(&source).contains("__stormmin_output_index"));
    }
}
