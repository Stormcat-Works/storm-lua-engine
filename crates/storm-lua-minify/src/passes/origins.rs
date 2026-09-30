//! Provenance operations shared by transformations with explicit operands.
use storm_lua_syntax::{Ast, NodeId};

// This checks actual input subexpressions, not an enclosing fallback anchor.
// A name-only slot is an explicit identifier attribution, not a guessed origin.
pub(super) fn known(ast: &Ast, node: NodeId) -> bool {
    let mut known = true;
    storm_lua_syntax::ast_utils::walk(ast, node, &mut |id| {
        known &= ast.nodes.origin(id).is_some()
            || (matches!(ast.node(id), storm_lua_syntax::Node::Name(_))
                && ast
                    .nodes
                    .name_origin(id, storm_lua_syntax::NameSite::Reference)
                    .is_some());
    });
    known
}

pub(super) fn derive(
    target: &mut Ast,
    result: NodeId,
    source: &Ast,
    inputs: &[NodeId],
    reason: &str,
) {
    if !target.nodes.tracks_origins() {
        return;
    }
    if inputs.is_empty() || inputs.iter().any(|&n| !known(source, n)) {
        let value = target.node(result).clone();
        target.nodes[result as usize] = value;
        return;
    }
    target
        .nodes
        .derive_from(result, &source.nodes, inputs[0], reason);
    for &input in &inputs[1..] {
        target
            .nodes
            .relate_from(result, &source.nodes, input, reason);
    }
}

pub(super) fn within(target: &mut Ast, result: NodeId, inputs: &[NodeId], reason: &str) {
    if !target.nodes.tracks_origins() {
        return;
    }
    if inputs.is_empty() || inputs.iter().any(|&n| !known(target, n)) {
        let value = target.node(result).clone();
        target.nodes[result as usize] = value;
        return;
    }
    let snapshot = target.nodes.capture_origin(inputs[0]);
    target.nodes.finish_rewrite(result, snapshot, reason);
    for &input in &inputs[1..] {
        target.nodes.relate_within(result, input, reason);
    }
}
