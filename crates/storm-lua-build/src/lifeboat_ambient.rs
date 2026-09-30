//! Application libraries used by a LifeBoat-style bundle; not a replacement Lua environment.
use crate::link::LinkedRange;
use std::collections::{BTreeMap, BTreeSet};
use storm_lua_analysis::resolver::{resolve, BindingKind};
use storm_lua_analysis::{AmbientMember, LuaProject};
use storm_lua_syntax::source_tools::string_bytes;
use storm_lua_syntax::{parse_source_with_positions, Node};

pub(crate) fn append(
    project: &LuaProject,
    sources: &BTreeMap<String, String>,
    output: &mut String,
    ranges: &mut Vec<LinkedRange>,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut used: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for source in sources.values() {
        let (ast, root, _) = parse_source_with_positions(source).map_err(|e| e.to_string())?;
        let resolution = resolve(&ast, root);
        for node in &ast.nodes {
            let Node::Index(object, key, _) = node else {
                continue;
            };
            let Node::Name(name) = ast.node(*object) else {
                continue;
            };
            let namespace = ast.strings.get(*name);
            let Some(ambient) = project.ambient.get(namespace) else {
                continue;
            };
            if resolution.node_bid[*object as usize]
                .is_some_and(|bid| resolution.binding(bid).kind != BindingKind::Global)
            {
                continue;
            }
            let Node::Str(raw) = ast.node(*key) else {
                return Err(format!(
                    "dynamic ambient lookup in {namespace} is not exportable"
                ));
            };
            let member = String::from_utf8(string_bytes(raw)?)
                .map_err(|_| "ambient member name must be UTF-8")?;
            match ambient.members.get(&member) {
                Some(AmbientMember::Module { .. }) => {
                    used.entry(namespace.into()).or_default().insert(member);
                }
                Some(AmbientMember::EnvironmentOnly) => {
                    return Err(format!(
                    "development-only API {namespace}.{member} remains outside a removed section"
                ))
                }
                None => return Err(format!("unknown ambient member {namespace}.{member}")),
            }
        }
    }
    let mut injected = BTreeMap::new();
    for (namespace, members) in used {
        output.push_str(&format!("local {namespace}={{}}\n"));
        let mut names = vec![];
        // A library function may reference a namespace peer. Preserve all real library members.
        for (name, member) in &project.ambient[&namespace].members {
            let AmbientMember::Module { source } = member else {
                continue;
            };
            output.push_str(&format!(
                "{namespace}[{}]=(function()\n",
                crate::lifeboat::quote(name)
            ));
            let line = output.bytes().filter(|b| *b == b'\n').count() as u32 + 1;
            let output_start_byte = output.len();
            output.push_str(source);
            let output_end_byte = output.len();
            if !source.ends_with('\n') {
                output.push('\n');
            }
            let count = source.lines().count() as u32;
            if count > 0 {
                ranges.push(LinkedRange {
                    output_start_line: line,
                    output_end_line: line + count - 1,
                    module: format!("{namespace}.{name}"),
                    source_start_line: 1,
                    output_start_byte,
                    output_end_byte,
                    source_start_byte: 0,
                });
            }
            output.push_str("end)()\n");
            names.push(name.clone());
        }
        if !members.is_empty() {
            injected.insert(namespace, names);
        }
    }
    Ok(injected)
}
