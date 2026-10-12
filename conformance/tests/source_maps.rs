//! Non-minified source locations checked through an actual game-profile VM.
//! Synthetic modules only: this contract has no dependency on a private corpus.
use std::{collections::BTreeMap, error::Error};
use storm_lua_analysis::LuaProject;
use storm_lua_build::{build, ApiProjectCompileOptions};
use storm_lua_microcontroller::Microcontroller;
use storm_lua_spec::io::CompositeSignal;
use storm_lua_vm::runner::{RunOutcome, StepMode};

#[test]
fn original_library_and_caller_locations_survive_bundle_debugging() -> Result<(), Box<dyn Error>> {
    let project = LuaProject {
        entry: "main".into(),
        modules: BTreeMap::from([
            ("main".into(), "local m=require(\"lib.math\")\nfunction onTick()\n output.setNumber(1,m.twice(input.getNumber(1)))\n output.setBool(1,pcall==nil and print==nil and require==nil)\nend\n".into()),
            ("lib.math".into(), "local M={}\nfunction M.twice(x)\n local result=x*2\n return result\nend\nreturn M\n".into()),
        ]),
        ambient: BTreeMap::new(),
    };
    let artifact = build(
        &project,
        &ApiProjectCompileOptions {
            metadata_tag: None,
            minify: Some(false),
            ..Default::default()
        },
    );
    assert!(artifact.ok, "{:?}", artifact.diagnostics);
    let code = artifact.code.ok_or("missing built source")?;
    let map =
        sourcemap::SourceMap::from_slice(artifact.map.ok_or("missing source map")?.as_bytes())?;
    let lines: Vec<_> = map
        .tokens()
        .filter(|token| token.get_source() == Some("lib/math.lua") && token.get_src_line() == 2)
        .map(|token| ("@bundle.lua".to_owned(), (token.get_dst_line() + 1) as i32))
        .collect();
    assert!(!lines.is_empty());
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(code.as_bytes(), "@bundle.lua")?;
    vm.set_breakpoints(lines)?;
    let mut input = CompositeSignal::default();
    input.numbers[0] = 3.0;
    assert_eq!(vm.tick(&input)?, RunOutcome::Suspended);
    let stack = vm.stack()?;
    let top = stack.first().ok_or("missing stopped frame")?;
    assert_eq!(top.source, "@bundle.lua");
    let location = map
        .lookup_token(u32::try_from(top.line)? - 1, 0)
        .ok_or("missing source origin")?;
    assert_eq!(location.get_source(), Some("lib/math.lua"));
    assert_eq!(location.get_src_line(), 2);
    assert!(stack.iter().any(|frame| frame.line > 0
        && map
            .lookup_token(frame.line as u32 - 1, 0)
            .is_some_and(
                |token| token.get_source() == Some("main.lua") && token.get_src_line() == 2
            )));
    vm.set_breakpoints(Vec::new())?;
    assert_eq!(vm.resume(StepMode::Over)?, RunOutcome::Suspended);
    let stepped = vm.stack()?;
    let token = map
        .lookup_token(
            u32::try_from(stepped.first().ok_or("missing stepped frame")?.line)? - 1,
            0,
        )
        .ok_or("missing step origin")?;
    assert_eq!(token.get_source(), Some("lib/math.lua"));
    assert_eq!(token.get_src_line(), 3);
    assert_eq!(vm.resume(StepMode::Continue)?, RunOutcome::Completed);
    assert_eq!(vm.output().numbers[0], 6.0);
    assert!(vm.output().booleans[0]);
    vm.reset()?;
    input.numbers[0] = 4.0;
    assert_eq!(vm.tick(&input)?, RunOutcome::Completed);
    assert_eq!(vm.output().numbers[0], 8.0);
    assert!(build(&project, &ApiProjectCompileOptions::default())
        .map
        .is_none());
    Ok(())
}

#[test]
fn runtime_error_points_into_the_original_returned_function() -> Result<(), Box<dyn Error>> {
    let project = LuaProject {
        entry: "main".into(),
        modules: BTreeMap::from([
            (
                "main".into(),
                "local f=require(\"lib.bad\")\nfunction onTick()f()end".into(),
            ),
            (
                "lib.bad".into(),
                "return function()\n local missing\n return missing.value\nend".into(),
            ),
        ]),
        ambient: BTreeMap::new(),
    };
    let result = build(
        &project,
        &ApiProjectCompileOptions {
            metadata_tag: None,
            minify: Some(false),
            ..Default::default()
        },
    );
    assert!(result.ok, "{:?}", result.diagnostics);
    let map = sourcemap::SourceMap::from_slice(result.map.ok_or("missing map")?.as_bytes())?;
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(
        result.code.ok_or("missing source")?.as_bytes(),
        "@bundle.lua",
    )?;
    let error = match vm.tick(&Default::default()) {
        Err(error) => error,
        Ok(_) => return Err("the real Lua nil access did not fail".into()),
    };
    let text = error.to_string();
    let line: u32 = text
        .split("bundle.lua:")
        .nth(1)
        .ok_or("missing error source")?
        .split(':')
        .next()
        .ok_or("missing error line")?
        .parse()?;
    let token = map
        .lookup_token(line - 1, 0)
        .ok_or("missing error origin")?;
    assert_eq!(token.get_source(), Some("lib/bad.lua"));
    assert_eq!(token.get_src_line(), 2);
    Ok(())
}
