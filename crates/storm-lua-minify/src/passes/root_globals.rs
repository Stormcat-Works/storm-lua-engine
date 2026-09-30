//! Root-local globalization (`passes/scope-and-api.ts::globalizeRootLocals`).
//!
//! Only top-level locals with a complete non-empty initializer list are
//! converted to global assignments. Top-level local functions become ordinary
//! function statements. Partial or uninitialized locals are preserved.

use crate::pass::PassResult;
use std::collections::HashMap;
use storm_lua_syntax::ast::{Ast, Node, NodeId};
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
    let mut output = Vec::with_capacity(statements.len());
    for statement in statements {
        match ast.node(statement).clone() {
            Node::Local(names, expressions)
                if !expressions.is_empty()
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
                let origin = ast.nodes.capture_origin(statement);
                let assignment = ast.assign(targets, expressions);
                ast.nodes
                    .finish_rewrite(assignment, origin, "root-local-globalization");
                output.push(assignment);
            }
            Node::Localfunc(name, function)
                if root_name_counts.get(ast.strings.get(name)).copied() == Some(1) =>
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
                let origin = ast.nodes.capture_origin(statement);
                let declaration = ast.funcstat(target, function);
                ast.nodes
                    .finish_rewrite(declaration, origin, "root-local-globalization");
                output.push(declaration);
            }
            _ => output.push(statement),
        }
    }
    ast.nodes
        .rewrite(root, Node::Block(output), "root-local-globalization");
    PassResult {
        root,
        saved: Some(original_size.saturating_sub(measure_size(ast, root)) as u64),
        details: None,
    }
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
    use storm_lua_syntax::parser::parse_source;
    use storm_lua_syntax::print::Printer;

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
    fn preserves_nested_locals() {
        assert_eq!(
            output("function onTick()local x=1 end"),
            "function onTick()local x=1 end"
        );
    }
}
