//! エラーの位置は、失敗したコルーチンのスタックから読む。構文エラーの行だけはメッセージから読む。
use std::num::NonZeroU64;
use storm_lua_addon::{Addon, AddonConfig};
use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_spec::environment::EnvironmentProfile;
use storm_lua_vm::{
    logging::LogLocation,
    runner::{ErrorKind, ErrorTrace, LuaErrorStage, VmError},
    source::{RequireLoader, SourceChunk},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Luaがメッセージの中で先頭を省略する長さ（60バイト超）のチャンク名。
const LONG: &str = "@a/path/that/goes/on/and/on/for/more/than/sixty/characters/main.lua";

fn location(chunk: &str, line: u32) -> Option<LogLocation> {
    Some(LogLocation {
        chunk: chunk.into(),
        line,
    })
}
fn trace(error: &VmError) -> Result<&ErrorTrace, Box<dyn std::error::Error>> {
    Ok(error.trace.as_deref().ok_or("error has no trace")?)
}
fn frames(trace: &ErrorTrace) -> Vec<(u32, &str, i32, Option<&str>)> {
    trace
        .stack
        .iter()
        .map(|frame| {
            (
                frame.level,
                frame.source.as_str(),
                frame.line,
                frame.function_name.as_deref(),
            )
        })
        .collect()
}

#[test]
fn runtime_error_keeps_the_full_chunk_name_and_call_stack() -> TestResult {
    assert!(LONG.len() > 60);
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(
        b"local function inner()\n  local t = nil\n  return t.x\nend\nfunction outer()\n  return inner() + 1\nend\nfunction onTick()\n  outer()\nend",
        LONG,
    )?;
    let error = vm.tick(&Default::default()).err().ok_or("nil index")?;
    assert_eq!(error.kind, ErrorKind::Lua);
    // メッセージはLuaの省略形のまま。位置情報は省略されない。
    assert!(error.message.contains("...") && !error.message.contains(&LONG[1..]));
    let trace = trace(&error)?;
    assert_eq!(trace.stage, Some(LuaErrorStage::Runtime));
    assert_eq!(trace.location, location(LONG, 3));
    assert_eq!(
        frames(trace),
        vec![
            (0, LONG, 3, Some("inner")),
            (1, LONG, 6, Some("outer")),
            (2, LONG, 9, None),
        ]
    );
    // 通常のLuaエラーではVMは失敗状態になりません（E9）。ステータスへ記録されます。
    assert!(!vm.is_failed());
    assert_eq!(vm.error_status(), 2);
    Ok(())
}

#[test]
fn error_raised_by_a_builtin_or_host_function_points_at_the_lua_caller() -> TestResult {
    for (body, name) in [
        ("error('boom')", "error"),
        ("input.getNumber(1)", "getNumber"),
    ] {
        // errorはextended環境だけにある。
        let mut vm = Microcontroller::new(MicrocontrollerConfig {
            environment: EnvironmentProfile::Extended,
            ..Default::default()
        })?;
        vm.load(
            format!("function onDraw()\n  local x = 1\n  {body}\nend").as_bytes(),
            "@main.lua",
        )?;
        let error = vm.draw(32, 32).err().ok_or("raised")?;
        let trace = trace(&error)?;
        assert_eq!(trace.stage, Some(LuaErrorStage::Runtime));
        assert_eq!(trace.location, location("@main.lua", 3));
        assert_eq!(
            frames(trace),
            vec![(0, "=[C]", -1, Some(name)), (1, "@main.lua", 3, None)]
        );
    }
    Ok(())
}

#[test]
fn message_level_does_not_move_the_raise_location() -> TestResult {
    let mut vm = Microcontroller::new(MicrocontrollerConfig {
        environment: EnvironmentProfile::Extended,
        ..Default::default()
    })?;
    vm.load(
        b"local function check()\n  error('bad', 2)\nend\nfunction onTick()\n  check()\nend",
        "@main.lua",
    )?;
    let error = vm.tick(&Default::default()).err().ok_or("raised")?;
    // メッセージは呼び出し側の5行目を指すが、送出したのは2行目。
    assert!(error.message.contains("main.lua:5: bad"));
    assert_eq!(trace(&error)?.location, location("@main.lua", 2));
    Ok(())
}

#[test]
fn syntax_error_reports_stage_chunk_and_line_without_a_stack() -> TestResult {
    for name in [LONG, "@main.lua", "=literal", "plain chunk text"] {
        let mut vm = Microcontroller::new(Default::default())?;
        let error = vm
            .load(b"local a = 1\nlocal b = = 2\n", name)
            .err()
            .ok_or("syntax")?;
        assert_eq!(error.kind, ErrorKind::Lua);
        let trace = trace(&error)?;
        assert_eq!(trace.stage, Some(LuaErrorStage::Syntax));
        assert_eq!(trace.location, location(name, 2), "{name}");
        assert!(trace.stack.is_empty());
    }
    Ok(())
}

#[test]
fn top_level_runtime_error_is_not_a_syntax_error() -> TestResult {
    let mut vm = Microcontroller::new(Default::default())?;
    let error = vm
        .load(b"local t\nlocal x = t.field", "@main.lua")
        .err()
        .ok_or("runtime")?;
    let trace = trace(&error)?;
    assert_eq!(trace.stage, Some(LuaErrorStage::Runtime));
    assert_eq!(trace.location, location("@main.lua", 2));
    Ok(())
}

#[test]
fn instruction_limit_reports_where_the_script_was_running() -> TestResult {
    for environment in [EnvironmentProfile::Game, EnvironmentProfile::Extended] {
        let mut config = MicrocontrollerConfig {
            environment,
            ..Default::default()
        };
        config.limits.instruction_budget = Some(NonZeroU64::MIN.saturating_add(9_999));
        let mut vm = Microcontroller::new(config)?;
        vm.load(
            b"local function spin()\n  while true do\n  end\nend\nfunction onTick()\n  spin()\nend",
            "@loop.lua",
        )?;
        let error = vm.tick(&Default::default()).err().ok_or("limit")?;
        assert_eq!(error.kind, ErrorKind::Limit);
        let trace = trace(&error)?;
        assert_eq!(trace.stage, None);
        let location = trace.location.as_ref().ok_or("no location")?;
        assert_eq!(location.chunk, "@loop.lua");
        assert!((2..=3).contains(&location.line), "{location:?}");
        assert_eq!(
            frames(trace),
            vec![
                (0, "@loop.lua", location.line as i32, Some("spin")),
                (1, "@loop.lua", 6, None),
            ]
        );
    }
    Ok(())
}

#[test]
fn required_chunk_syntax_error_names_the_required_chunk() -> TestResult {
    let mut vm = Microcontroller::new(MicrocontrollerConfig {
        environment: EnvironmentProfile::Extended,
        require_loader: Some(RequireLoader::new(|_| {
            Ok(SourceChunk {
                name: "@lib/broken.lua".into(),
                source: b"local ok = 1\n\nlocal bad = = 2".to_vec(),
            })
        })),
        ..Default::default()
    })?;
    let error = vm
        .load(b"local x = 1\nrequire('broken')", "@main.lua")
        .err()
        .ok_or("syntax in required chunk")?;
    let trace = trace(&error)?;
    assert_eq!(trace.stage, Some(LuaErrorStage::Syntax));
    assert_eq!(trace.location, location("@lib/broken.lua", 3));
    // 呼び出し履歴はrequireを呼んだ側を示す。
    assert!(trace
        .stack
        .iter()
        .any(|frame| frame.source == "@main.lua" && frame.line == 2));
    Ok(())
}

#[test]
fn non_utf8_function_name_is_unknown_and_keeps_the_location() -> TestResult {
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(
        b"local t = {}\nt['\\255'] = function()\n  local n = nil\n  return n.x\nend\nfunction onTick()\n  t['\\255']()\nend",
        "@main.lua",
    )?;
    let error = vm.tick(&Default::default()).err().ok_or("nil index")?;
    let trace = trace(&error)?;
    assert_eq!(trace.location, location("@main.lua", 4));
    assert_eq!(
        frames(trace),
        vec![(0, "@main.lua", 4, None), (1, "@main.lua", 7, None)]
    );
    Ok(())
}

#[test]
fn memory_limit_reports_where_the_script_was_running() -> TestResult {
    // メモリ上限はホストが明示した場合だけ効きます（J1）。
    let mut config = MicrocontrollerConfig::default();
    config.limits.lua_memory_bytes =
        Some(std::num::NonZeroUsize::new(8 * 1024 * 1024).ok_or("memory")?);
    let mut vm = Microcontroller::new(config)?;
    vm.load(
        b"function onTick()\n  local t = {}\n  local s = 'x'\n  while true do\n    s = s .. s\n    t[#t + 1] = s\n  end\nend",
        "@main.lua",
    )?;
    let error = vm.tick(&Default::default()).err().ok_or("memory")?;
    assert_eq!(error.kind, ErrorKind::Limit);
    let trace = trace(&error)?;
    assert_eq!(trace.stage, None);
    let location = trace.location.as_ref().ok_or("no location")?;
    assert_eq!(location.chunk, "@main.lua");
    assert!((4..=7).contains(&location.line), "{location:?}");
    Ok(())
}

#[test]
fn host_argument_errors_have_no_trace() -> TestResult {
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(b"function onDraw() end", "@main.lua")?;
    let error = vm.draw(0, 0).err().ok_or("invalid size")?;
    assert_eq!(error.kind, ErrorKind::InvalidArgument);
    assert_eq!(error.trace, None);
    Ok(())
}

#[test]
fn addon_callback_error_has_a_trace_and_the_vm_survives() -> TestResult {
    let mut addon = Addon::new(AddonConfig::default())?;
    addon.load(
        b"function onTick(ticks)\n  local t = nil\n  t.x = ticks\nend",
        "@addon.lua",
    )?;
    addon.start()?;
    let error = addon.tick(1).err().ok_or("nil index")?;
    assert_eq!(error.kind, ErrorKind::Lua);
    let trace = trace(&error)?;
    assert_eq!(trace.stage, Some(LuaErrorStage::Runtime));
    assert_eq!(trace.location, location("@addon.lua", 3));
    assert!(!addon.is_failed());
    Ok(())
}
