//! Small numeric proof domain for algebraic rewrites. Unknown is deliberately
//! distinct from a numeric value; this does not infer a type from Lua spelling.
use std::collections::HashMap;
use storm_lua_analysis::{
    effects::EffectAnalyzer,
    resolver::{BindingId, Resolution},
};
use storm_lua_syntax::numeric::{integer_literal_value, num_val};
use storm_lua_syntax::{Ast, Node, NodeId};

#[derive(Clone, Copy)]
pub(super) enum Number {
    Integer(i64),
    Float,
}

pub(super) struct NumericProof<'a> {
    ast: &'a Ast,
    resolution: &'a Resolution,
    analyzer: &'a EffectAnalyzer<'a>,
    initializers: HashMap<BindingId, NodeId>,
}
impl<'a> NumericProof<'a> {
    pub(super) fn new(
        ast: &'a Ast,
        resolution: &'a Resolution,
        analyzer: &'a EffectAnalyzer<'a>,
        root: NodeId,
    ) -> Self {
        let mut initializers = HashMap::new();
        storm_lua_syntax::ast_utils::walk(ast, root, &mut |node| {
            if let Node::Local(_, expressions) = ast.node(node) {
                for (bid, expression) in resolution.node_bids[node as usize].iter().zip(expressions)
                {
                    if resolution.binding_write_counts[*bid as usize] == 0 {
                        initializers.insert(*bid, *expression);
                    }
                }
            }
        });
        if let Node::Block(statements) = ast.node(root) {
            for statement in statements {
                if let Node::Assign(targets, expressions) = ast.node(*statement) {
                    for (target, expression) in targets.iter().zip(expressions) {
                        if matches!(ast.node(*target), Node::Name(_)) {
                            if let Some(bid) = resolution.node_bid[*target as usize] {
                                if resolution.binding_write_counts[bid as usize] == 1 {
                                    initializers.insert(bid, *expression);
                                }
                            }
                        }
                    }
                }
            }
        }
        Self {
            ast,
            resolution,
            analyzer,
            initializers,
        }
    }
    pub(super) fn value(&self, node: NodeId) -> Option<Number> {
        self.value_at(node, 0)
    }
    fn value_at(&self, node: NodeId, depth: usize) -> Option<Number> {
        if depth > 32 {
            return None;
        }
        let recurse = |node| self.value_at(node, depth + 1);
        match self.ast.node(node) {
            Node::Num(raw) => integer_literal_value(raw)
                .map(Number::Integer)
                .or_else(|| num_val(raw).is_finite().then_some(Number::Float)),
            Node::Paren(inner) => recurse(*inner),
            Node::Name(_) => recurse(
                *self
                    .initializers
                    .get(&self.resolution.node_bid[node as usize]?)?,
            ),
            Node::Index(..)
                if self.analyzer.resolve_builtin_reference(node).as_deref() == Some("math.pi") =>
            {
                Some(Number::Float)
            }
            Node::Call(function, _, None)
                if matches!(
                    self.analyzer
                        .resolve_builtin_reference(*function)
                        .as_deref(),
                    Some("input.getNumber" | "property.getNumber")
                ) =>
            {
                Some(Number::Float)
            }
            Node::Un(op, inner) if op == "-" => match recurse(*inner)? {
                Number::Integer(v) => v.checked_neg().map(Number::Integer),
                Number::Float => Some(Number::Float),
            },
            Node::Bin(op, left, right) if matches!(op.as_str(), "+" | "-" | "*" | "/" | "^") => {
                let (a, b) = (recurse(*left)?, recurse(*right)?);
                if matches!(op.as_str(), "/" | "^")
                    || matches!(a, Number::Float)
                    || matches!(b, Number::Float)
                {
                    return Some(Number::Float);
                }
                let (Number::Integer(a), Number::Integer(b)) = (a, b) else {
                    return None;
                };
                match op.as_str() {
                    "+" => a.checked_add(b),
                    "-" => a.checked_sub(b),
                    "*" => a.checked_mul(b),
                    _ => None,
                }
                .map(Number::Integer)
            }
            _ => None,
        }
    }
}
