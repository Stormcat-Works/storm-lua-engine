//! Shared environment diagnostics. Reflection is valid Lua, not permission to change its meaning.
use crate::{
    diagnostic::{codes, Diagnostic, Range, Severity},
    resolver::{resolve, BindingKind},
};
use std::collections::HashSet;
use storm_lua_spec::environment::EnvironmentProfile;
use storm_lua_syntax::{
    ast::{Ast, Node, NodeId},
    ast_utils::walk,
    parser::NodePositions,
};

/// Why only exact-token compaction is valid for this input/environment.
pub fn lexical_reason(
    ast: &Ast,
    environment: EnvironmentProfile,
    host_bindings: &[String],
) -> Option<&'static str> {
    if ast.strings.contains("_ENV") {
        Some("explicit _ENV access or rebinding observes global names and values")
    } else if !host_bindings.is_empty() {
        Some("host bindings can replace builtin behavior")
    } else if environment == EnvironmentProfile::Extended {
        Some("extended Lua error handling and host semantics require token-preserving compilation")
    } else {
        None
    }
}

/// Diagnose missing builtin dependencies, preserving local shadowing and explicit nil/type probes.
#[allow(clippy::too_many_arguments)]
pub fn diagnostics(
    ast: &Ast,
    root: NodeId,
    positions: Option<&NodePositions>,
    environment: EnvironmentProfile,
    host_bindings: &[String],
    written_globals: &HashSet<String>,
    severity: Severity,
    module: Option<&str>,
) -> Vec<Diagnostic> {
    diagnostics_for(
        ast,
        root,
        positions,
        environment,
        host_bindings,
        written_globals,
        severity,
        module,
        EnvironmentProfile::is_unavailable,
    )
}

/// Addon base-function diagnostics, with the same shadowing and probe rules.
pub fn addon_diagnostics(
    ast: &Ast,
    root: NodeId,
    positions: Option<&NodePositions>,
    environment: EnvironmentProfile,
) -> Vec<Diagnostic> {
    diagnostics_for(
        ast,
        root,
        positions,
        environment,
        &[],
        &HashSet::new(),
        Severity::Error,
        None,
        EnvironmentProfile::is_addon_unavailable,
    )
}

#[allow(clippy::too_many_arguments)]
fn diagnostics_for(
    ast: &Ast,
    root: NodeId,
    positions: Option<&NodePositions>,
    environment: EnvironmentProfile,
    host_bindings: &[String],
    written_globals: &HashSet<String>,
    severity: Severity,
    module: Option<&str>,
    unavailable: fn(EnvironmentProfile, &str) -> bool,
) -> Vec<Diagnostic> {
    // The selected lexical path preserves explicit environment rebinding. Static global lookup
    // cannot assume that the ambient game environment remains visible in such a chunk.
    if ast.strings.contains("_ENV") {
        return Vec::new();
    }
    let resolution = resolve(ast, root);
    let mut probes = HashSet::new();
    let unwrap = |mut id: NodeId| {
        while let Node::Paren(inner) = ast.node(id) {
            id = *inner;
        }
        id
    };
    walk(ast, root, &mut |id| match ast.node(id) {
        Node::Bin(op, a, b) if op == "==" || op == "~=" => {
            if matches!(ast.node(unwrap(*a)), Node::Nil) {
                probes.insert(unwrap(*b));
            }
            if matches!(ast.node(unwrap(*b)), Node::Nil) {
                probes.insert(unwrap(*a));
            }
        }
        Node::Call(f, args, None) if args.len() == 1 => {
            if matches!(ast.node(unwrap(*f)), Node::Name(n) if ast.strings.get(*n) == "type") {
                let bid = resolution.node_bid[unwrap(*f) as usize];
                if bid.is_some_and(|b| {
                    resolution.binding(b).kind == BindingKind::Global
                        && resolution.binding_write_counts[b as usize] == 0
                }) {
                    probes.insert(unwrap(args[0]));
                }
            }
        }
        _ => {}
    });
    let external_roots: HashSet<_> = host_bindings
        .iter()
        .filter_map(|p| p.split('.').next())
        .collect();
    let mut result = Vec::new();
    walk(ast, root, &mut |id| {
        let Some(bid) = resolution.node_bid.get(id as usize).copied().flatten() else {
            return;
        };
        let binding = resolution.binding(bid);
        if !matches!(ast.node(id), Node::Name(_))
            || binding.kind != BindingKind::Global
            || resolution.node_write[id as usize]
        {
            return;
        }
        let name = ast.strings.get(binding.name);
        if written_globals.contains(name)
            || resolution.binding_write_counts[bid as usize] != 0
            || external_roots.contains(name)
            || probes.contains(&id)
        {
            return;
        }
        if unavailable(environment, name) {
            result.push(Diagnostic {
                code: codes::SW_UNAVAILABLE_GLOBAL, severity,
                message: format!("{name} is absent from the {} environment; use an explicit extended environment or define your own binding.", environment.as_str()),
                module: module.map(str::to_owned),
                range: positions.and_then(|p| p.get(id)).map(|(line,col)| Range::point(line,col)),
            });
        }
    });
    // debug is a real logging-only table, not the Lua standard debug library.
    walk(ast, root, &mut |id| {
        let Node::Index(object, key, _) = ast.node(id) else {
            return;
        };
        let (Node::Name(name), Node::Str(key)) = (ast.node(*object), ast.node(*key)) else {
            return;
        };
        if ast.strings.get(*name) != "debug"
            || probes.contains(&id)
            || written_globals.contains("debug")
            || external_roots.contains("debug")
        {
            return;
        }
        let Some(bid) = resolution.node_bid[*object as usize] else {
            return;
        };
        if resolution.binding(bid).kind != BindingKind::Global
            || resolution.binding_write_counts[bid as usize] != 0
        {
            return;
        }
        let field = storm_lua_syntax::numeric::decode_lua_string(key);
        if field != "log" {
            result.push(Diagnostic { code: codes::SW_UNAVAILABLE_GLOBAL, severity,
                message: format!("debug.{field} is absent; scripts can use debug.log only. Host debugging uses the SDK debugger API."),
                module: module.map(str::to_owned),
                range: positions.and_then(|p| p.get(id)).map(|(line,col)| Range::point(line,col)),
            });
        }
    });
    result
}

/// Check the optimization entry before any property folding, renaming or early target return.
pub fn validate(
    ast: &Ast,
    root: NodeId,
    environment: EnvironmentProfile,
    host_bindings: &[String],
) -> Result<(), String> {
    let issues = diagnostics(
        ast,
        root,
        None,
        environment,
        host_bindings,
        &HashSet::new(),
        Severity::Error,
        None,
    );
    if let Some(issue) = issues.first() {
        return Err(format!("{}: {}", issue.code, issue.message));
    }
    Ok(())
}
