//! Immutable carrier synthesis (`passes/immutable-synthesis.ts`).
//!
//! Repeated stable unary/binary expressions inside functions that depend only
//! on single-write top-level numeric globals can be evaluated once into a
//! synthetic global. Candidate prefixes are measured after scope renaming and
//! only the smallest candidate is committed in each of up to eight rounds.

use std::collections::{HashMap, HashSet};

use crate::pass::PassResult;
use crate::scope_rename::measure_renamed_size;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::print::Printer;

#[derive(Clone)]
struct Group {
    expression: NodeId,
    occurrences: Vec<NodeId>,
    declaration: usize,
    estimate: isize,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn clone_subtree(target: &mut Ast, source: &Ast, node: NodeId) -> NodeId {
    let original = source.node(node).clone();
    let (mapped, _) = storm_lua_syntax::ast_utils::map_children(&original, &mut |child| {
        clone_subtree(target, source, child)
    });
    let copied = target.push(mapped);
    target
        .nodes
        .derive_from(copied, &source.nodes, node, "immutable-carrier-copy");
    copied
}

fn measured(ast: &Ast, root: NodeId) -> usize {
    measure_renamed_size(ast, root)
}

fn expression_key(ast: &Ast, node: NodeId) -> String {
    Printer::new(ast, false).expr_public(node)
}

#[allow(clippy::too_many_arguments)]
fn inspect(
    ast: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    numeric_initializers: &HashMap<BindingId, usize>,
    node: NodeId,
    inside_function: bool,
    groups: &mut Vec<Group>,
    group_indexes: &mut HashMap<String, usize>,
) {
    if inside_function && matches!(ast.node(node), Node::Bin(..) | Node::Un(..)) {
        let key = expression_key(ast, node);
        if key.len() >= 3 {
            let effect = analyzer.effects_for_expr(node);
            if !effect.calls
                && !effect.ordered
                && effect.writes.is_empty()
                && effect.stable
                && !effect.reads.is_empty()
                && effect
                    .reads
                    .iter()
                    .all(|bid| numeric_initializers.contains_key(bid))
            {
                let declaration = effect
                    .reads
                    .iter()
                    .filter_map(|bid| numeric_initializers.get(bid))
                    .copied()
                    .max()
                    .unwrap_or(0);
                let index = if let Some(index) = group_indexes.get(&key).copied() {
                    index
                } else {
                    let index = groups.len();
                    group_indexes.insert(key.clone(), index);
                    groups.push(Group {
                        expression: node,
                        occurrences: Vec::new(),
                        declaration,
                        estimate: 0,
                    });
                    index
                };
                let group = &mut groups[index];
                group.occurrences.push(node);
                group.estimate = group.occurrences.len() as isize * (key.len() as isize - 1)
                    - key.len() as isize
                    - 2;
            }
        }
    }
    if let Node::Function(_, _, body) = ast.node(node) {
        inspect(
            ast,
            analyzer,
            numeric_initializers,
            *body,
            true,
            groups,
            group_indexes,
        );
        return;
    }
    storm_lua_analysis::resolver::for_each_child(ast, node, &mut |child| {
        inspect(
            ast,
            analyzer,
            numeric_initializers,
            child,
            inside_function,
            groups,
            group_indexes,
        );
    });
}

fn candidate_for_group(
    source: &Ast,
    root: NodeId,
    group: &Group,
    count: usize,
    carrier: &str,
) -> Ast {
    let mut candidate = clone_ast(source);
    let expression = clone_subtree(&mut candidate, source, group.expression);
    let carrier_symbol = candidate.strings.intern(carrier);
    for occurrence in group.occurrences.iter().take(count) {
        candidate.nodes.rewrite(
            *occurrence,
            Node::Name(carrier_symbol),
            "immutable-carrier-use",
        );
        super::origins::derive(
            &mut candidate,
            *occurrence,
            source,
            &[*occurrence],
            "immutable-carrier-use",
        );
        candidate.nodes.relate_from_role(
            *occurrence,
            &source.nodes,
            group.expression,
            "immutable-carrier-definition",
            storm_lua_syntax::explanation::RelationRole::Definition,
        );
    }
    let target = candidate.name(carrier);
    candidate
        .nodes
        .mark_synthetic(target, "immutable-carrier-storage");
    let assignment = candidate.assign(vec![target], vec![expression]);
    candidate
        .nodes
        .mark_synthetic(assignment, "immutable-carrier-storage");
    if let Node::Block(statements) = candidate.node(root).clone() {
        let mut statements = statements;
        let insert_at = (group.declaration + 1).min(statements.len());
        statements.insert(insert_at, assignment);
        candidate
            .nodes
            .rewrite(root, Node::Block(statements), "immutable-carrier-insertion");
    }
    candidate
}

pub fn synthesize_immutable_carriers_with_options(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    max_rounds: usize,
) -> PassResult {
    if !aggressive || !matches!(ast.node(root), Node::Block(_)) {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let original_size = measured(ast, root);
    let mut synthesized = 0usize;
    let mut considered = 0usize;

    for _ in 0..max_rounds {
        let source = clone_ast(ast);
        let res = resolve(&source, root);
        let analyzer = EffectAnalyzer::new(&source, &res, root, true);
        let mut writes = vec![0u32; res.bindings.len()];
        let mut used_names = HashSet::new();
        storm_lua_syntax::ast_utils::walk(&source, root, &mut |node| {
            if let Node::Name(symbol) = source.node(node) {
                used_names.insert(source.strings.get(*symbol).to_string());
                if res.node_write.get(node as usize).copied().unwrap_or(false) {
                    if let Some(bid) = res.node_bid.get(node as usize).copied().flatten() {
                        writes[bid as usize] += 1;
                    }
                }
            }
        });
        let mut numeric_initializers = HashMap::new();
        let Node::Block(statements) = source.node(root) else {
            break;
        };
        for (statement_index, statement) in statements.iter().enumerate() {
            let Node::Assign(targets, expressions) = source.node(*statement) else {
                continue;
            };
            for index in 0..targets.len() {
                let Some(target) = targets.get(index) else {
                    continue;
                };
                let Some(expression) = expressions.get(index) else {
                    continue;
                };
                if !matches!(source.node(*target), Node::Name(_))
                    || !matches!(source.node(*expression), Node::Num(_))
                {
                    continue;
                }
                let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() else {
                    continue;
                };
                if writes.get(bid as usize).copied().unwrap_or(0) == 1 {
                    numeric_initializers.insert(bid, statement_index);
                }
            }
        }

        let mut groups = Vec::new();
        let mut group_indexes = HashMap::new();
        inspect(
            &source,
            &analyzer,
            &numeric_initializers,
            root,
            false,
            &mut groups,
            &mut group_indexes,
        );
        groups.retain(|group| group.occurrences.len() >= 2);
        groups.sort_by_key(|group| std::cmp::Reverse(group.estimate));
        groups.truncate(32);

        let baseline = measured(&source, root);
        let mut best: Option<(Ast, usize)> = None;
        for group in groups {
            for count in 2..=group.occurrences.len() {
                considered += 1;
                let mut serial = 0usize;
                let mut carrier = format!("__stormmin_immutable_{serial}");
                while used_names.contains(&carrier) {
                    serial += 1;
                    carrier = format!("__stormmin_immutable_{serial}");
                }
                let candidate = candidate_for_group(&source, root, &group, count, &carrier);
                let size = measured(&candidate, root);
                if size < baseline && best.as_ref().is_none_or(|entry| size < entry.1) {
                    best = Some((candidate, size));
                }
            }
        }
        let Some((candidate, _)) = best else {
            break;
        };
        *ast = candidate;
        synthesized += 1;
    }

    PassResult {
        root,
        saved: Some(original_size.saturating_sub(measured(ast, root)) as u64),
        details: if synthesized == 0 && considered == 0 {
            None
        } else {
            Some(vec![
                format!("synthesized={synthesized}"),
                format!("considered={considered}"),
            ])
        },
    }
}

pub fn synthesize_immutable_carriers(ast: &mut Ast, root: NodeId) -> PassResult {
    synthesize_immutable_carriers_with_options(ast, root, true, 8)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::{parse_source, Printer};

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = synthesize_immutable_carriers(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn synthesizes_repeated_cross_function_expression() {
        let source = "a=123456 b=789012 function f0()return a+b end function f1()return a+b end function f2()return a+b end function f3()return a+b end";
        let out = output(source);
        assert!(out.contains("__stormmin_immutable_0=a+b"), "{out}");
        assert_eq!(out.matches("a+b").count(), 1, "{out}");
    }

    #[test]
    fn requires_single_write_numeric_dependencies() {
        let source = "a=1 a=2 function f()return a+3 end function g()return a+3 end function h()return a+3 end";
        assert!(!output(source).contains("__stormmin_immutable"));
    }

    #[test]
    fn ignores_top_level_occurrences() {
        let source = "a=1 b=2 x=a+b function f()return a+b end";
        assert!(!output(source).contains("__stormmin_immutable"));
    }
}
