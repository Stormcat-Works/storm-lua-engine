//! LifeBoat-style include-once game builds and explicit comment directives.
//! Language syntax comes from the shared lexer/parser. No script is executed here.
use crate::link::{LinkResult, LinkedRange};
use std::collections::{BTreeMap, BTreeSet};
use storm_lua_analysis::resolver::{resolve, BindingKind};
use storm_lua_analysis::{Diagnostic, LuaProject};
use storm_lua_syntax::source_tools::string_bytes;
use storm_lua_syntax::{parse_source_with_positions, Ast, Lexer, Node, NodeId, TokenKind};

const MAX_SOURCE: usize = 2 * 1024 * 1024;
const MAX_MODULES: usize = 2048;
const MANAGED_START: &str = "-- >>> storm-lua-runner:simio";
const MANAGED_END: &str = "-- <<< storm-lua-runner:simio";
#[derive(Clone)]
struct Section {
    start: usize,
    end: usize,
    pattern: String,
    instances: usize,
    simulator: bool,
}
fn blank(bytes: &mut [u8]) {
    for b in bytes {
        if *b != b'\n' && *b != b'\r' {
            *b = b' ';
        }
    }
}
fn escape_pattern(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        if "^$()%.[]*+-?".contains(c) {
            out.push('%');
        }
        out.push(c);
    }
    out
}
fn sections(source: &str) -> Result<Vec<Section>, String> {
    let mut lexer = Lexer::new(source).with_comment_capture();
    lexer.all().map_err(|e| e.to_string())?;
    let mut stack: Vec<(usize, String, usize, String, bool)> = vec![];
    let mut result = vec![];
    let mut managed = None;
    for comment in lexer.comments() {
        if comment.long {
            continue;
        }
        let line = comment.text.trim_end_matches('\r');
        if line.starts_with(MANAGED_START) {
            if managed.is_some() {
                return Err("nested Sim I/O managed blocks are not valid".into());
            }
            managed = Some(comment.start);
            continue;
        }
        if line == MANAGED_END {
            let start = managed.take().ok_or("unmatched Sim I/O block end")?;
            result.push(Section {
                start,
                end: comment.end,
                pattern: String::new(),
                instances: 1,
                simulator: true,
            });
            continue;
        }
        if let Some(args) = line
            .strip_prefix("---@section")
            .filter(|tail| tail.starts_with(char::is_whitespace))
        {
            let mut args = args.split_whitespace();
            let mut exact = true;
            let first = args.next().ok_or("section identifier is missing")?;
            let identifier = match first {
                "EXACT" => args.next(),
                "PATTERN" => {
                    exact = false;
                    args.next()
                }
                _ => Some(first),
            }
            .ok_or("section identifier is missing")?;
            let mut instances = 1;
            let mut name = "";
            if let Some(next) = args.next() {
                if let Ok(value) = next.parse::<usize>() {
                    instances = value;
                    name = args.next().unwrap_or("");
                } else {
                    name = next;
                }
            }
            if args.next().is_some() {
                return Err("too many section arguments".into());
            }
            if instances > 1_000_000 {
                return Err("section instance threshold exceeds limit".into());
            }
            let pattern = if exact {
                format!("[^%a%d_]{}[^%a%d_]", escape_pattern(identifier))
            } else {
                identifier.to_owned()
            };
            stack.push((
                comment.start,
                pattern,
                instances,
                name.into(),
                identifier == "__LB_SIMULATOR_ONLY__",
            ));
            if stack.len() > 128 {
                return Err("section nesting exceeds 128".into());
            }
        } else if let Some(tail) = line
            .strip_prefix("---@endsection")
            .filter(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace))
        {
            let name = tail.trim();
            let (start, pattern, instances, expected, simulator) =
                stack.pop().ok_or("unmatched endsection")?;
            if name != expected {
                return Err(format!(
                    "endsection name {name:?} does not match {expected:?}"
                ));
            }
            result.push(Section {
                start,
                end: comment.end,
                pattern,
                instances,
                simulator,
            });
        }
    }
    if managed.is_some() {
        return Err("unterminated Sim I/O managed block".into());
    }
    if !stack.is_empty() {
        return Err("unterminated LB section".into());
    }
    result.sort_by_key(|s| s.start);
    Ok(result)
}
/// Remove only development directives while preserving UTF-8 byte offsets and every newline.
/// It is also useful when an IDE needs the game-side static view of a file without optimization.
pub fn strip_development(source: &str) -> Result<String, String> {
    if source.len() > MAX_SOURCE {
        return Err("source exceeds 2 MiB build limit".into());
    }
    let mut out = source.as_bytes().to_vec();
    for section in sections(source)? {
        if section.simulator {
            blank(&mut out[section.start..section.end]);
        }
    }
    String::from_utf8(out).map_err(|e| e.to_string())
}
fn countable(source: &str) -> Result<Vec<u8>, String> {
    let mut bytes = source.as_bytes().to_vec();
    let mut lexer = Lexer::new(source).with_comment_capture();
    loop {
        let token = lexer.next().map_err(|e| e.to_string())?;
        if token.k == TokenKind::Eof {
            break;
        }
        if token.k == TokenKind::Str {
            blank(&mut bytes[token.p..lexer.pos()]);
        }
    }
    for comment in lexer.comments() {
        blank(&mut bytes[comment.start..comment.end]);
    }
    Ok(bytes)
}
fn strip_sections(source: &str) -> Result<String, String> {
    let mut source = source.to_owned();
    let mut passes = 0;
    loop {
        let items = sections(&source)?;
        if items.is_empty() {
            return Ok(source);
        }
        if items.len() > 4096 {
            return Err("too many LB sections".into());
        }
        let searchable = countable(&source)?;
        let mut removable = None;
        for section in &items {
            if section.simulator {
                removable = Some(section);
                break;
            }
            // Count each side separately, matching LB's outside-section contract.
            let count = crate::lua_pattern::count(&searchable[..section.start], &section.pattern)?
                + crate::lua_pattern::count(&searchable[section.end..], &section.pattern)?;
            if count < section.instances {
                removable = Some(section);
                break;
            }
        }
        let mut bytes = source.into_bytes();
        if let Some(section) = removable {
            blank(&mut bytes[section.start..section.end]);
        } else {
            let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
            let mut lexer = Lexer::new(text).with_comment_capture();
            lexer.all().map_err(|e| e.to_string())?;
            let spans: Vec<_> = lexer
                .comments()
                .iter()
                .filter(|c| {
                    !c.long
                        && (c.text.starts_with("---@section")
                            || c.text.starts_with("---@endsection"))
                })
                .map(|c| (c.start, c.end))
                .collect();
            for (start, end) in spans {
                blank(&mut bytes[start..end]);
            }
            return String::from_utf8(bytes).map_err(|e| e.to_string());
        }
        source = String::from_utf8(bytes).map_err(|e| e.to_string())?;
        passes += 1;
        if passes > 4096 {
            return Err("LB section work limit exceeded".into());
        }
    }
}
fn direct_name(ast: &Ast, id: NodeId, name: &str) -> bool {
    matches!(ast.node(id),Node::Name(symbol) if ast.strings.get(*symbol)==name)
}
fn dependencies(source: &str) -> Result<Vec<String>, String> {
    let (ast, root, _) = parse_source_with_positions(source).map_err(|e| e.to_string())?;
    let resolution = resolve(&ast, root);
    let mut result = vec![];
    let mut direct = BTreeSet::new();
    for node in &ast.nodes {
        let Node::Call(f, args, method) = node else {
            continue;
        };
        if method.is_some() || !direct_name(&ast, *f, "require") {
            continue;
        }
        if resolution.node_bid[*f as usize]
            .is_some_and(|bid| resolution.binding(bid).kind != BindingKind::Global)
        {
            continue;
        }
        direct.insert(*f);
        if args.len() != 1 {
            return Err("LB game build requires one literal module name".into());
        }
        let Node::Str(raw) = ast.node(args[0]) else {
            return Err("Dynamic require is supported in development, but cannot be exported to a self-contained game script".into());
        };
        result
            .push(String::from_utf8(string_bytes(raw)?).map_err(|_| "require name must be UTF-8")?);
    }
    for (id, node) in ast.nodes.iter().enumerate() {
        if matches!(node,Node::Name(n) if ast.strings.get(*n)=="require")
            && resolution.node_bid[id]
                .is_some_and(|bid| resolution.binding(bid).kind == BindingKind::Global)
            && !direct.contains(&(id as NodeId))
        {
            return Err("An aliased or reflected require cannot be resolved by the game build; use direct literal includes".into());
        }
    }
    if resolution.globals.iter().any(|(name, bid)| {
        ast.strings.get(*name) == "require"
            && resolution
                .binding_write_counts
                .get(*bid as usize)
                .copied()
                .unwrap_or(0)
                > 0
    }) {
        return Err("LB game build cannot replace the include loader".into());
    }
    Ok(result)
}
fn key_for(project: &LuaProject, name: &str) -> Option<String> {
    if project.modules.contains_key(name) {
        return Some(name.to_owned());
    }
    let init = format!("{name}.init");
    project.modules.contains_key(&init).then_some(init)
}
pub(crate) fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for b in value.bytes() {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            32..=126 => out.push(b as char),
            _ => out.push_str(&format!("\\{b:03}")),
        }
    }
    out.push('"');
    out
}
struct Builder<'a> {
    project: &'a LuaProject,
    sources: BTreeMap<String, String>,
    aliases: BTreeMap<String, String>,
    used: Vec<String>,
    seen: BTreeSet<String>,
}
impl Builder<'_> {
    fn visit(&mut self, key: &str) -> Result<(), String> {
        if !self.seen.insert(key.to_owned()) {
            return Ok(());
        }
        if self.seen.len() > MAX_MODULES {
            return Err("too many LB source modules".into());
        }
        let source = self
            .project
            .modules
            .get(key)
            .ok_or_else(|| format!("module not found: {key}"))?;
        let prepared = strip_development(source)?;
        self.used.push(key.into());
        for name in dependencies(&prepared)? {
            if matches!(name.as_str(), "table" | "math" | "string") {
                continue;
            }
            let resolved = key_for(self.project, &name)
                .ok_or_else(|| format!("module not found: {name} (from {key})"))?;
            self.aliases.insert(name, resolved.clone());
            self.visit(&resolved)?;
        }
        self.sources.insert(key.into(), prepared);
        Ok(())
    }
}
/// Build an include-once program. Each source has private locals; return values are discarded.
/// Non-minified line correspondence is expressed using the existing LinkResult ranges.
pub fn link_lifeboat(project: &LuaProject) -> LinkResult {
    let failure = |error: String| LinkResult {
        diagnostics: vec![Diagnostic::error("lifeboat-build", error)],
        linked_source: None,
        used_modules: vec![],
        ranges: vec![],
        injected_ambient: BTreeMap::new(),
    };
    let run = || -> Result<LinkResult, String> {
        if project.modules.len() > MAX_MODULES
            || project.modules.values().map(String::len).sum::<usize>() > 8 * MAX_SOURCE
        {
            return Err("project exceeds LB build resources".into());
        }
        let mut builder = Builder {
            project,
            sources: BTreeMap::new(),
            aliases: BTreeMap::new(),
            used: vec![],
            seen: BTreeSet::new(),
        };
        builder.visit(&project.entry)?;
        let mut prefix = "__lb".to_string();
        while project.modules.values().any(|s| s.contains(&prefix)) {
            prefix.push('_');
        }
        let loaders = format!("{prefix}loaders");
        let loaded = format!("{prefix}loaded");
        let aliases = format!("{prefix}aliases");
        let mut output=format!("local {loaders},{loaded},{aliases}={{}},{{}},{{}}\nlocal function require(name)\n if {loaded}[name] then return end\n {loaded}[name]=true\n local fn={loaders}[{aliases}[name] or name]\n if fn then fn() end\nend\n");
        let mut ranges = vec![];
        let injected =
            crate::lifeboat_ambient::append(project, &builder.sources, &mut output, &mut ranges)?;
        for (name, key) in &builder.aliases {
            output.push_str(&format!("{aliases}[{}]={}\n", quote(name), quote(key)));
        }
        for key in &builder.used {
            let source = &builder.sources[key];
            output.push_str(&format!("{loaders}[{}]=function()\n", quote(key)));
            let line = output.bytes().filter(|b| *b == b'\n').count() as u32 + 1;
            output.push_str(source);
            if !source.ends_with('\n') {
                output.push('\n');
            }
            let count = source.lines().count() as u32;
            if count > 0 {
                ranges.push(LinkedRange {
                    output_start_line: line,
                    output_end_line: line + count - 1,
                    module: key.clone(),
                    source_start_line: 1,
                });
            }
            output.push_str("end\n");
        }
        output.push_str(&format!("require({})\n", quote(&project.entry)));
        let code = strip_sections(&output)?;
        Ok(LinkResult {
            diagnostics: vec![],
            linked_source: Some(code),
            used_modules: builder.used,
            ranges,
            injected_ambient: injected,
        })
    };
    run().unwrap_or_else(failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_real_directives_are_removed_and_lines_preserved() {
        let source="local text='---@section __LB_SIMULATOR_ONLY__'\n---@section __LB_SIMULATOR_ONLY__\nprint('DEV')\n---@endsection\nfunction onTick()end";
        let stripped = strip_development(source).unwrap_or_default();
        assert!(stripped.contains("local text='---@section"));
        assert!(!stripped.contains("print('DEV')"));
        assert_eq!(stripped.lines().count(), source.lines().count());
        assert!(strip_development("---@section X\nfoo()\n").is_err());
    }
    #[test]
    fn named_nested_and_pattern_sections_reach_a_fixed_point() {
        let source="---@section unused\nfunction unused() called() end\n---@endsection\n---@section EXACT called 1 c\nfunction called()end\n---@endsection c\n---@section PATTERN used%.%w+ 1 keep\nused={}\n---@endsection keep\nused.run()";
        let result = strip_sections(source).unwrap_or_default();
        assert!(!result.contains("function unused"));
        assert!(!result.contains("function called"));
        assert!(result.contains("used={}"));
        assert!(!result.contains("---@section"));
    }
    #[test]
    fn dependency_cycles_and_conditional_includes_are_runtime_correct_shapes() {
        let project = LuaProject {
            entry: "main".into(),
            modules: BTreeMap::from([
                ("main".into(), "if true then require('lib')end".into()),
                ("lib".into(), "require('main')\nx=3\nreturn 99".into()),
            ]),
            ambient: BTreeMap::new(),
        };
        let result = link_lifeboat(&project);
        assert!(result.linked_source.is_some(), "{:?}", result.diagnostics);
        assert_eq!(result.used_modules, vec!["main", "lib"]);
    }
}
