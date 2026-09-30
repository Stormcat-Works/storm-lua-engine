//! Else-default hoisting (`passes/default-hoisting.ts`).

use std::collections::HashSet;

use crate::pass::PassResult;
use crate::scope_rename::scope_rename_fast;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::resolve;
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::size::measure_size;

#[derive(Clone, Copy)]
struct Site {
    block: NodeId,
    index: usize,
    statement: NodeId,
}

fn clone_ast(source: &Ast) -> Ast {
    let mut ast = storm_lua_syntax::ast_utils::inherit_ast(source);
    ast.nodes = source.nodes.clone();
    ast
}

fn measured(ast: &Ast, root: NodeId) -> usize {
    let renamed = scope_rename_fast(ast, root);
    measure_size(&renamed.ast, renamed.root)
}

fn is_control_barrier(node: &Node) -> bool {
    matches!(
        node,
        Node::If(..)
            | Node::While(..)
            | Node::Repeat(..)
            | Node::Fornum(..)
            | Node::Forin(..)
            | Node::Return(..)
            | Node::Break
            | Node::Goto(_)
    )
}

fn valid_site(
    ast: &Ast,
    res: &storm_lua_analysis::resolver::Resolution,
    analyzer: &EffectAnalyzer<'_>,
    statement: NodeId,
) -> bool {
    let Node::If(arms, Some(else_block)) = ast.node(statement) else {
        return false;
    };
    if arms.len() != 1 {
        return false;
    }
    let Node::Block(default_statements) = ast.node(*else_block) else {
        return false;
    };
    if default_statements.is_empty() {
        return false;
    }
    let mut targets = HashSet::new();
    for default_statement in default_statements {
        let Node::Assign(vs, es) = ast.node(*default_statement) else {
            return false;
        };
        if vs
            .iter()
            .any(|target| !matches!(ast.node(*target), Node::Name(_)))
        {
            return false;
        }
        for target in vs {
            if let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() {
                targets.insert(bid);
            }
        }
        for expression in es {
            let effect = analyzer.effects_for_expr(*expression);
            if effect.calls
                || effect.ordered
                || effect.may_throw
                || !effect.writes.is_empty()
                || !effect.reads.is_empty()
            {
                return false;
            }
        }
    }
    if targets.is_empty() {
        return false;
    }
    let condition_effect = analyzer.effects_for_expr(arms[0].cond);
    if condition_effect.calls
        || condition_effect.ordered
        || condition_effect.may_throw
        || !condition_effect.writes.is_empty()
        || targets
            .iter()
            .any(|bid| condition_effect.reads.contains(bid))
    {
        return false;
    }

    let Node::Block(true_statements) = ast.node(arms[0].body) else {
        return false;
    };
    let mut remaining = targets;
    for item in true_statements {
        if remaining.is_empty() {
            break;
        }
        let effect = analyzer.effects_for_statement(*item);
        if effect.calls
            || effect.ordered
            || effect.may_throw
            || remaining.iter().any(|bid| effect.reads.contains(bid))
        {
            return false;
        }
        let mut direct = HashSet::new();
        if let Node::Assign(vs, _) = ast.node(*item) {
            for target in vs {
                if matches!(ast.node(*target), Node::Name(_)) {
                    if let Some(bid) = res.node_bid.get(*target as usize).copied().flatten() {
                        direct.insert(bid);
                    }
                }
            }
        }
        if remaining
            .iter()
            .any(|bid| effect.writes.contains(bid) && !direct.contains(bid))
        {
            return false;
        }
        for bid in direct {
            remaining.remove(&bid);
        }
        if !matches!(ast.node(*item), Node::Assign(..))
            && !remaining.is_empty()
            && is_control_barrier(ast.node(*item))
        {
            return false;
        }
    }
    remaining.is_empty()
}

fn find_sites(
    ast: &Ast,
    res: &storm_lua_analysis::resolver::Resolution,
    analyzer: &EffectAnalyzer<'_>,
    root: NodeId,
) -> Vec<Site> {
    let mut sites = Vec::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |id| {
        let Node::Block(statements) = ast.node(id) else {
            return;
        };
        for (index, statement) in statements.iter().enumerate() {
            if valid_site(ast, res, analyzer, *statement) {
                sites.push(Site {
                    block: id,
                    index,
                    statement: *statement,
                });
            }
        }
    });
    sites
}

fn apply_site(ast: &mut Ast, site: Site) -> Result<(), &'static str> {
    let Node::If(arms, Some(else_block)) = ast.node(site.statement).clone() else {
        return Err("validated site changed");
    };
    let Node::Block(defaults) = ast.node(else_block).clone() else {
        return Err("validated else block changed");
    };
    ast.nodes.rewrite(
        site.statement,
        Node::If(arms, None),
        "else-default-hoisting",
    );
    let Node::Block(statements) = ast.node(site.block).clone() else {
        return Err("validated containing block changed");
    };
    let mut output = Vec::with_capacity(statements.len() + defaults.len());
    output.extend_from_slice(&statements[..site.index]);
    output.extend(defaults);
    output.push(site.statement);
    output.extend_from_slice(&statements[site.index + 1..]);
    ast.nodes
        .rewrite(site.block, Node::Block(output), "else-default-hoisting");
    Ok(())
}

pub fn hoist_else_defaults_with_options(
    ast: &mut Ast,
    root: NodeId,
    aggressive: bool,
    max_rounds: usize,
) -> PassResult {
    if !aggressive {
        return PassResult {
            root,
            saved: Some(0),
            details: None,
        };
    }
    let original = measured(ast, root);
    let mut hoisted = 0usize;
    let mut considered = 0usize;
    for _ in 0..max_rounds {
        let source = clone_ast(ast);
        let res = resolve(&source, root);
        let analyzer = EffectAnalyzer::new(&source, &res, root, true);
        let sites = find_sites(&source, &res, &analyzer, root);
        let baseline = measured(&source, root);
        let mut best: Option<(Ast, usize)> = None;
        for site in sites {
            considered += 1;
            let mut candidate = clone_ast(&source);
            if apply_site(&mut candidate, site).is_err() {
                continue;
            }
            let size = measured(&candidate, root);
            if size < baseline && best.as_ref().is_none_or(|entry| size < entry.1) {
                best = Some((candidate, size));
            }
        }
        let Some((candidate, _)) = best else {
            break;
        };
        *ast = candidate;
        hoisted += 1;
    }
    PassResult {
        root,
        saved: Some(original.saturating_sub(measured(ast, root)) as u64),
        details: if hoisted == 0 && considered == 0 {
            None
        } else {
            Some(vec![
                format!("hoisted={hoisted}"),
                format!("considered={considered}"),
            ])
        },
    }
}

pub fn hoist_else_defaults(ast: &mut Ast, root: NodeId) -> PassResult {
    hoist_else_defaults_with_options(ast, root, true, 8)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    fn output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = hoist_else_defaults(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn hoists_constant_defaults_before_definite_writes() {
        let source = "x=0 y=0 function onTick()if input.getBool(1)then x=input.getNumber(1)y=input.getNumber(2)else x,y=0,0 end output.setNumber(1,x)output.setNumber(2,y)end";
        let out = output(source);
        assert!(out.contains("x,y=0,0 if input.getBool(1)then"), "{out}");
    }

    #[test]
    fn rejects_read_before_definite_write() {
        let source = "x=0 y=0 function onTick()if input.getBool(1)then y=x x=1 else x=0 end output.setNumber(1,y)end";
        let out = output(source);
        assert!(out.contains("else x=0 end"), "{out}");
    }

    #[test]
    fn rejects_throwing_condition() {
        let source = "a=nil if a[1]then x=1 else x=0 end";
        let out = output(source);
        assert!(out.contains("else x=0 end"), "{out}");
    }
}
