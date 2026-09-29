//! Integration of LB directive processing, source inspection and the real game-profile VM.
use std::collections::BTreeMap;
use storm_lua_analysis::{AmbientMember, AmbientNamespace, LuaProject};
use storm_lua_build::public_api::{
    compile_lifeboat, ApiCompileMode, ApiCompileOptions, ApiProjectCompileOptions,
};
use storm_lua_microcontroller::Microcontroller;
fn project(modules: &[(&str, &str)]) -> LuaProject {
    LuaProject {
        entry: "main".into(),
        modules: modules
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
        ambient: BTreeMap::new(),
    }
}
fn run(project: LuaProject, minify: bool) -> Result<[f32; 3], Box<dyn std::error::Error>> {
    let result = compile_lifeboat(
        &project,
        &ApiProjectCompileOptions {
            minify: Some(minify),
            compile: ApiCompileOptions {
                mode: Some(ApiCompileMode::Safe),
                ..Default::default()
            },
        },
    );
    assert!(result.ok, "{:?} {:?}", result.error, result.diagnostics);
    assert_eq!(result.map.is_some(), !minify);
    let code = result.code.ok_or("no game artifact")?;
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(code.as_bytes(), "@built.lua")?;
    vm.tick(&Default::default())?;
    let values = vm.output().numbers;
    Ok([values[0], values[1], values[2]])
}
#[test]
fn include_once_returns_nil_and_has_private_locals_even_with_cycles(
) -> Result<(), Box<dyn std::error::Error>> {
    let p=project(&[("main","local private=9\nlocal value=require('util')\nrequire('util')\nfunction onTick()output.setNumber(1,count);output.setNumber(2,private);output.setNumber(3,value==nil and 1 or 0)end"),
        ("util","local private=3\ncount=(count or 0)+1\nrequire('main')\nreturn 42")]);
    for minify in [false, true] {
        assert_eq!(run(p.clone(), minify)?, [1.0, 9.0, 1.0]);
    }
    Ok(())
}
#[test]
fn simulator_and_managed_sections_are_removed_without_minify(
) -> Result<(), Box<dyn std::error::Error>> {
    let p=project(&[("main","---@section __LB_SIMULATOR_ONLY__\nprint('development')\nrequire('unavailable-dev-tool')\n---@endsection\n-- >>> storm-lua-runner:simio (generated)\ndo sim.setProperty('x',3)end\n-- <<< storm-lua-runner:simio\nfunction onTick()output.setNumber(1,2)end")]);
    for minify in [false, true] {
        assert_eq!(run(p.clone(), minify)?[0], 2.0);
    }
    Ok(())
}
#[test]
fn ordinary_sections_and_pattern_counts_do_not_count_strings_or_comments(
) -> Result<(), Box<dyn std::error::Error>> {
    let p=project(&[("main","---@section unused\nfunction unused()missing()end\n---@endsection\n---@section PATTERN used%.%w+ 1 chosen\nused={value=4}\n---@endsection chosen\nlocal text='unused' -- unused\nfunction onTick()output.setNumber(1,used.value)end")]);
    assert_eq!(run(p, false)?[0], 4.0);
    Ok(())
}
#[test]
fn declaration_library_is_injected_but_environment_only_members_fail(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut p = project(&[(
        "main",
        "function onTick()output.setNumber(1,sim.clamp(20,0,7))end",
    )]);
    p.ambient.insert(
        "sim".into(),
        AmbientNamespace {
            members: BTreeMap::from([
                (
                    "clamp".into(),
                    AmbientMember::Module {
                        source: "return function(x,a,b)return math.min(b,math.max(a,x))end".into(),
                    },
                ),
                ("setProperty".into(), AmbientMember::EnvironmentOnly),
            ]),
        },
    );
    assert_eq!(run(p.clone(), false)?[0], 7.0);
    p.modules
        .insert("main".into(), "sim.setProperty('bad',1)".into());
    assert!(!compile_lifeboat(&p, &Default::default()).ok);
    Ok(())
}
#[test]
fn malformed_directives_missing_modules_and_dynamic_game_requires_are_explicit_errors() {
    for source in [
        "---@section x\nfunction f()end",
        "---@section x 1 a\nfoo()\n---@endsection b",
        "require('missing')",
        "function onTick()require(property.getText('module'))end",
    ] {
        let result = compile_lifeboat(
            &project(&[("main", source)]),
            &ApiProjectCompileOptions {
                minify: Some(false),
                ..Default::default()
            },
        );
        assert!(!result.ok, "accepted {source}");
        assert!(result.code.is_none());
    }
}
