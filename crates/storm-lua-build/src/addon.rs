//! Non-optimizing Addon builds using the shared include-once linker.
//!
//! No Vehicle phase rules, output limit, property specialization, or minifier
//! are applied. Server/world compatibility remains the host's responsibility.
use crate::metadata::prepare;
use crate::{lifeboat, public_api::ApiProjectCompileResult, source_map::generate_source_map};
use serde::{Deserialize, Serialize};
use storm_lua_analysis::{Diagnostic, LuaProject, Severity};
use storm_lua_spec::environment::EnvironmentProfile;
use storm_lua_syntax::parse_source_with_positions;

/// Explicitly non-minifying options. Unknown fields are rejected.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddonBuildOptions {
    /// Script-visible profile, independent of the Addon runtime model.
    #[serde(default)]
    pub environment: EnvironmentProfile,
    /// Optional exact first-line marker inside a leading long comment.
    /// Consumers may remove declarative development metadata without executing it.
    pub metadata_tag: Option<String>,
}

fn failure(diagnostics: Vec<Diagnostic>) -> ApiProjectCompileResult {
    ApiProjectCompileResult {
        diagnostics,
        ..Default::default()
    }
}
/// Link literal include-once dependencies to one readable Addon script.
/// Return values are discarded as specified by the Engine development loader.
/// Success means syntactically valid linked Lua, not complete game-world emulation.
pub fn build_addon(project: &LuaProject, options: &AddonBuildOptions) -> ApiProjectCompileResult {
    // Check resources before cloning or parsing potentially untrusted input.
    if project.modules.len() > 2048
        || project.modules.values().map(String::len).sum::<usize>() > 16 * 1024 * 1024
    {
        return failure(vec![Diagnostic::error(
            "addon-build-limit",
            "Addon project exceeds build resources",
        )]);
    }
    let prepared = match prepare(project, options.metadata_tag.as_deref()) {
        Ok(value) => value,
        Err((name, error)) => {
            return failure(vec![
                Diagnostic::error("addon-build", error).with_module(name)
            ])
        }
    };
    let mut link = lifeboat::link_lifeboat(&prepared.project);
    if let Err(error) = prepared.exclude_removed_ranges(&mut link) {
        return failure(vec![Diagnostic::error("addon-build", error)]);
    }
    let Some(code) = link.linked_source.as_deref() else {
        return failure(link.diagnostics);
    };
    let (ast, root, positions) = match parse_source_with_positions(code) {
        Ok(value) => value,
        Err(error) => return failure(vec![Diagnostic::error("syntax-error", error.to_string())]),
    };
    let mut diagnostics = storm_lua_analysis::environment_checks::addon_diagnostics(
        &ast,
        root,
        Some(&positions),
        options.environment,
    );
    for diagnostic in &mut diagnostics {
        if let Some(position) = diagnostic.range {
            if let Some((module, line)) =
                crate::link::lookup_source_line(&link.ranges, position.line)
            {
                diagnostic.module = Some(module.into());
                diagnostic.range = Some(storm_lua_analysis::Range { line, ..position });
            } else {
                diagnostic.range = None;
            }
        }
    }
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return failure(diagnostics);
    }
    // Keep original source text in the map; metadata blanking preserved byte and line positions.
    let map = generate_source_map(project, &link);
    ApiProjectCompileResult {
        ok: true,
        code: Some(code.into()),
        map,
        used_modules: link.used_modules,
        injected_ambient: link.injected_ambient,
        diagnostics,
        original: Some(
            project
                .modules
                .values()
                .map(|s| s.encode_utf16().count())
                .sum(),
        ),
        size: Some(code.encode_utf16().count()),
        ..Default::default()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    fn project(main: &str) -> LuaProject {
        LuaProject {
            entry: "main".into(),
            modules: BTreeMap::from([
                ("main".into(), main.into()),
                (
                    "lib".into(),
                    "local private=3\nHelper={value=private}\nreturn 99".into(),
                ),
            ]),
            ambient: BTreeMap::new(),
        }
    }
    #[test]
    fn addon_globals_and_callbacks_are_not_vehicle_restricted() {
        let p=project("require('lib')\ng_savedata={}\nfunction onCreate() g_savedata.x=Helper.value end\nfunction onTick(dt) server.announce('hello',tostring(dt)) end");
        let out = build_addon(&p, &Default::default());
        assert!(out.ok, "{:?}", out.diagnostics);
        assert!(out.map.is_some());
        assert!(out.search.is_none());
    }
    #[test]
    fn vehicle_size_and_optimizer_are_not_applied() {
        let mut p = project("function onTick(dt) g_savedata.dt=dt end");
        p.modules
            .get_mut("main")
            .unwrap()
            .push_str(&format!("\n--{}\n", "x".repeat(10_000)));
        let out = build_addon(&p, &Default::default());
        assert!(out.ok);
        assert!(out.size.unwrap() > 8192);
        assert!(out.passes.is_empty());
    }
    #[test]
    fn metadata_and_development_includes_do_not_escape() {
        let p=project("--[==[storm-lua-studio\n{\"mode\":\"addon\",\"scene\":\"private_scene\"}\n]==]\n---@section __LB_SIMULATOR_ONLY__\nrequire('not_packaged')\n---@endsection\nfunction onTick(dt) end");
        let out = build_addon(
            &p,
            &AddonBuildOptions {
                metadata_tag: Some("storm-lua-studio".into()),
                ..Default::default()
            },
        );
        assert!(out.ok, "{:?}", out.diagnostics);
        let code = out.code.unwrap();
        assert!(!code.contains("private_scene"));
        assert!(!code.contains("not_packaged"));
        assert!(out.map.unwrap().contains("private_scene"));
    }
    #[test]
    fn dynamic_dependencies_and_bad_syntax_fail() {
        assert!(!build_addon(&project("require(name)"), &Default::default()).ok);
        assert!(!build_addon(&project("function onTick("), &Default::default()).ok);
    }
    #[test]
    fn game_environment_is_not_silently_extended() {
        assert!(!build_addon(&project("pcall(function()end)"), &Default::default()).ok);
        assert!(
            build_addon(
                &project("pcall(function()end)"),
                &AddonBuildOptions {
                    environment: EnvironmentProfile::Extended,
                    ..Default::default()
                }
            )
            .ok
        );
    }
    #[test]
    fn metadata_tag_does_not_strip_a_string_or_unrelated_comment() {
        let source = "local x=[==[storm-lua-studio\nhello\n]==]\n";
        assert_eq!(
            prepare(&project(source), Some("storm-lua-studio"))
                .unwrap()
                .project
                .modules["main"],
            source
        );
        assert!(serde_json::from_str::<AddonBuildOptions>("{\"minify\":true}").is_err());
    }

    #[test]
    fn addon_select_availability_preserves_shadowing_and_explicit_probes() {
        assert!(!build_addon(&project("select(1,2)"), &Default::default()).ok);
        for source in [
            "local select=function(...)return 1 end select(1,2)",
            "function select(...)return 1 end select(1,2)",
            "local absent=select==nil",
            "local kind=type(select)",
        ] {
            assert!(
                build_addon(&project(source), &Default::default()).ok,
                "{source}"
            );
        }
        assert!(
            build_addon(
                &project("select(1,2)"),
                &AddonBuildOptions {
                    environment: EnvironmentProfile::Extended,
                    ..Default::default()
                }
            )
            .ok
        );
    }
}
