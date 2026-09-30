//! Available-expression reuse (`passes/postprocess.ts`).
//!
//! Straight-line expressions can be replaced by a shorter reaching carrier
//! when the expression is stable and none of its dependencies have been
//! invalidated. Semantic identity includes resolved BindingIds and expressions
//! that allocate fresh table/function references are never shared.

use std::collections::HashMap;

use crate::pass::PassResult;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, BindingId, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId, TableField};
use storm_lua_syntax::size::measure_expr;

use super::immutable_values::{contains_fresh_reference, expression_key};

#[derive(Clone)]
struct AvailableExpression {
    carrier: SymbolId,
    carrier_bid: BindingId,
    definition: NodeId,
    reads: Vec<BindingId>,
}

fn is_atom(node: &Node) -> bool {
    matches!(
        node,
        Node::Name(_) | Node::Num(_) | Node::Str(_) | Node::Bool(_) | Node::Nil
    )
}

fn clone_source(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn api_call_preserves_lua_values(
    source: &Ast,
    analyzer: &EffectAnalyzer<'_>,
    statement: NodeId,
) -> bool {
    let Node::Callstat(expression) = source.node(statement) else {
        return false;
    };
    let Node::Call(function, _, _) = source.node(*expression) else {
        return false;
    };
    let Some(builtin) = analyzer.resolve_builtin_reference(*function) else {
        return false;
    };
    builtin.starts_with("output.")
        || (builtin.starts_with("screen.")
            && builtin != "screen.getWidth"
            && builtin != "screen.getHeight")
}

fn contains_mutable_index_read(source: &Ast, analyzer: &EffectAnalyzer<'_>, node: NodeId) -> bool {
    if matches!(source.node(node), Node::Index(..))
        && analyzer.resolve_builtin_reference(node).is_none()
    {
        return true;
    }
    let mut found = false;
    storm_lua_analysis::resolver::for_each_child(source, node, &mut |child| {
        if !found && contains_mutable_index_read(source, analyzer, child) {
            found = true;
        }
    });
    found
}

struct Transformer<'a> {
    source: &'a Ast,
    resolution: &'a Resolution,
    analyzer: EffectAnalyzer<'a>,
    output: Ast,
    reused: usize,
}

impl<'a> Transformer<'a> {
    fn new(source: &'a Ast, root: NodeId, resolution: &'a Resolution) -> Self {
        Self {
            source,
            resolution,
            analyzer: EffectAnalyzer::new(source, resolution, root, true),
            output: clone_source(source),
            reused: 0,
        }
    }

    fn rewrite_expression(
        &mut self,
        node: NodeId,
        available: &mut HashMap<String, AvailableExpression>,
    ) {
        if !is_atom(self.source.node(node)) {
            let key = expression_key(self.source, self.resolution, node);
            if let Some(entry) = available.get(&key) {
                let carrier_len = self.source.strings.get(entry.carrier).len();
                if carrier_len < measure_expr(self.source, node) {
                    self.output.nodes.rewrite(
                        node,
                        Node::Name(entry.carrier),
                        "available-expression-use",
                    );
                    super::origins::derive(
                        &mut self.output,
                        node,
                        self.source,
                        &[node],
                        "available-expression-use",
                    );
                    self.output.nodes.relate_from_role(
                        node,
                        &self.source.nodes,
                        entry.definition,
                        "available-expression-definition",
                        storm_lua_syntax::explanation::RelationRole::Definition,
                    );
                    self.reused += 1;
                    return;
                }
            }
        }

        match self.source.node(node).clone() {
            Node::Function(_, _, body) => self.transform_block(body),
            Node::Block(_) => self.transform_block(node),
            Node::Do(body) => self.transform_block(body),
            Node::While(condition, body) => {
                // A condition is reevaluated after every iteration.  Values
                // available before the loop cannot stand in for it when the
                // body may mutate one of its dependencies.
                available.clear();
                self.rewrite_expression(condition, available);
                self.transform_block(body);
            }
            Node::Repeat(body, condition) => {
                self.transform_block(body);
                available.clear();
                self.rewrite_expression(condition, available);
            }
            Node::If(arms, else_block) => {
                for arm in arms {
                    self.rewrite_expression(arm.cond, available);
                    self.transform_block(arm.body);
                }
                if let Some(body) = else_block {
                    self.transform_block(body);
                }
            }
            Node::Fornum(_, start, end, step, body) => {
                available.clear();
                self.rewrite_expression(start, available);
                self.rewrite_expression(end, available);
                if let Some(step) = step {
                    self.rewrite_expression(step, available);
                }
                self.transform_block(body);
            }
            Node::Forin(_, expressions, body) => {
                available.clear();
                for expression in expressions {
                    self.rewrite_expression(expression, available);
                }
                self.transform_block(body);
            }
            Node::Funcstat(target, function) => {
                self.rewrite_expression(target, available);
                self.rewrite_expression(function, available);
            }
            Node::Localfunc(_, function) => self.rewrite_expression(function, available),
            Node::Local(_, expressions) | Node::Return(expressions) => {
                for expression in expressions {
                    self.rewrite_expression(expression, available);
                }
            }
            Node::Callstat(expression) => self.rewrite_expression(expression, available),
            Node::Assign(targets, expressions) => {
                for target in targets {
                    self.rewrite_expression(target, available);
                }
                for expression in expressions {
                    self.rewrite_expression(expression, available);
                }
            }
            Node::Table(fields) => {
                for field in fields {
                    match field {
                        TableField::Arr(value) | TableField::Name(_, value) => {
                            self.rewrite_expression(value, available)
                        }
                        TableField::KVar(key, value) => {
                            self.rewrite_expression(key, available);
                            self.rewrite_expression(value, available);
                        }
                    }
                }
            }
            Node::Un(_, expression) | Node::Paren(expression) => {
                self.rewrite_expression(expression, available)
            }
            Node::Bin(_, left, right) | Node::Index(left, right, _) => {
                self.rewrite_expression(left, available);
                self.rewrite_expression(right, available);
            }
            Node::Call(function, arguments, _) => {
                self.rewrite_expression(function, available);
                for argument in arguments {
                    self.rewrite_expression(argument, available);
                }
            }
            Node::Methodname(object, _) => self.rewrite_expression(object, available),
            Node::Break
            | Node::Goto(_)
            | Node::Label(_)
            | Node::Nil
            | Node::Bool(_)
            | Node::Vararg
            | Node::Num(_)
            | Node::Str(_)
            | Node::Name(_) => {}
        }
    }

    fn transform_statement(
        &mut self,
        statement: NodeId,
        available: &mut HashMap<String, AvailableExpression>,
    ) {
        match self.source.node(statement).clone() {
            // Assignment LHS is not rewritten by the TS implementation.
            Node::Assign(_, expressions) => {
                for expression in expressions {
                    self.rewrite_expression(expression, available);
                }
            }
            Node::Function(_, _, body) => self.transform_block(body),
            Node::Block(_) => self.transform_block(statement),
            Node::Do(body) => self.transform_block(body),
            Node::While(condition, body) => {
                available.clear();
                self.rewrite_expression(condition, available);
                self.transform_block(body);
            }
            Node::Repeat(body, condition) => {
                self.transform_block(body);
                available.clear();
                self.rewrite_expression(condition, available);
            }
            Node::If(arms, else_block) => {
                for arm in arms {
                    self.rewrite_expression(arm.cond, available);
                    self.transform_block(arm.body);
                }
                if let Some(body) = else_block {
                    self.transform_block(body);
                }
            }
            Node::Fornum(_, start, end, step, body) => {
                available.clear();
                self.rewrite_expression(start, available);
                self.rewrite_expression(end, available);
                if let Some(step) = step {
                    self.rewrite_expression(step, available);
                }
                self.transform_block(body);
            }
            Node::Forin(_, expressions, body) => {
                available.clear();
                for expression in expressions {
                    self.rewrite_expression(expression, available);
                }
                self.transform_block(body);
            }
            Node::Funcstat(target, function) => {
                self.rewrite_expression(target, available);
                self.rewrite_expression(function, available);
            }
            Node::Localfunc(_, function) => self.rewrite_expression(function, available),
            Node::Local(_, expressions) | Node::Return(expressions) => {
                for expression in expressions {
                    self.rewrite_expression(expression, available);
                }
            }
            Node::Callstat(expression) => self.rewrite_expression(expression, available),
            Node::Break | Node::Goto(_) | Node::Label(_) => {}
            // Expression variants only occur recursively.
            _ => self.rewrite_expression(statement, available),
        }
    }

    fn transform_block(&mut self, block: NodeId) {
        let Node::Block(statements) = self.source.node(block).clone() else {
            return;
        };
        let mut available = HashMap::<String, AvailableExpression>::new();

        for statement in statements {
            self.transform_statement(statement, &mut available);
            let effect = self.analyzer.effects_for_statement(statement);
            let unknown_call = effect.calls
                && !api_call_preserves_lua_values(self.source, &self.analyzer, statement);
            available.retain(|_, entry| {
                !unknown_call
                    && !effect.writes.contains(&entry.carrier_bid)
                    && !entry.reads.iter().any(|bid| effect.writes.contains(bid))
            });

            match self.source.node(statement) {
                Node::Assign(targets, expressions) => {
                    for (part, target) in targets.iter().copied().enumerate() {
                        if part >= expressions.len() {
                            break;
                        }
                        let Node::Name(symbol) = self.source.node(target) else {
                            continue;
                        };
                        let Some(bid) = self
                            .resolution
                            .node_bid
                            .get(target as usize)
                            .copied()
                            .flatten()
                        else {
                            continue;
                        };
                        self.add_candidate(
                            bid,
                            *symbol,
                            expressions[part],
                            expressions[part],
                            &effect.writes,
                            &mut available,
                        );
                    }
                }
                Node::Local(names, expressions) if names.len() == 1 && expressions.len() == 1 => {
                    let bids = self
                        .resolution
                        .node_bids
                        .get(statement as usize)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    if bids.len() == 1 {
                        self.add_candidate(
                            bids[0],
                            names[0],
                            expressions[0],
                            expressions[0],
                            &effect.writes,
                            &mut available,
                        );
                    }
                }
                _ => {}
            }

            if matches!(
                self.source.node(statement),
                Node::If(..)
                    | Node::While(..)
                    | Node::Repeat(..)
                    | Node::Fornum(..)
                    | Node::Forin(..)
                    | Node::Do(..)
            ) {
                available.clear();
            }
        }
    }

    fn add_candidate(
        &mut self,
        bid: BindingId,
        symbol: SymbolId,
        source_expression: NodeId,
        output_expression: NodeId,
        statement_writes: &[BindingId],
        available: &mut HashMap<String, AvailableExpression>,
    ) {
        let expression_effect = self.analyzer.effects_for_expr(source_expression);
        if expression_effect.calls
            || expression_effect.ordered
            || !expression_effect.writes.is_empty()
            || !expression_effect.stable
            || contains_fresh_reference(self.source, source_expression)
            || contains_mutable_index_read(self.source, &self.analyzer, source_expression)
            || expression_effect
                .reads
                .iter()
                .any(|read| statement_writes.contains(read))
            || is_atom(self.output.node(output_expression))
        {
            return;
        }
        available.insert(
            expression_key(self.source, self.resolution, source_expression),
            AvailableExpression {
                carrier: symbol,
                carrier_bid: bid,
                definition: source_expression,
                reads: expression_effect.reads,
            },
        );
    }
}

pub fn reuse_available_expressions(ast: &mut Ast, root: NodeId) -> PassResult {
    let source = clone_source(ast);
    let resolution = resolve(&source, root);
    let mut transformer = Transformer::new(&source, root, &resolution);
    transformer.transform_block(root);
    let reused = transformer.reused;
    *ast = transformer.output;
    PassResult {
        root,
        saved: None,
        details: Some(vec![format!("reused={reused}")]),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn run(source: &str) -> (String, usize) {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = reuse_available_expressions(&mut ast, root);
        let reused = result
            .details
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.strip_prefix("reused="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        (Printer::new(&ast, false).output(root), reused)
    }

    #[test]
    fn reuses_reaching_expression() {
        let source = "function onTick()x=math.abs(input.getNumber(1))y=math.abs(input.getNumber(1))output.setNumber(1,x+y)end";
        let (out, reused) = run(source);
        assert!(reused > 0, "{out}");
        assert!(out.contains("y=x"), "{out}");
    }

    #[test]
    fn rejects_mutable_table_field_reads() {
        let source = "function onTick()t={value=7}local value=t.value t.value=9 output.setNumber(1,t.value)end";
        let (out, reused) = run(source);
        assert_eq!(reused, 0, "{out}");
        assert!(out.contains("output.setNumber(1,t.value)"), "{out}");
    }

    #[test]
    fn preserves_fresh_table_identity() {
        let source = "function onTick()local a={x=1}local b={x=1}a.x=2 output.setBool(1,a~=b)output.setNumber(1,b.x)end";
        let (out, reused) = run(source);
        assert_eq!(reused, 0, "{out}");
        assert!(out.contains("local b={x=1}"), "{out}");
    }
}
