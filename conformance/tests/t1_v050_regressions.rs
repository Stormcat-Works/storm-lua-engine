//! Track 1 (v0.5.0 semantics × 65 minify passes, Component/matrix, debugger/timeout) regressions.
use std::{
    collections::BTreeMap,
    error::Error,
    num::{NonZeroU64, NonZeroUsize},
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};
use storm_lua_addon::{Addon, AddonConfig};
use storm_lua_component::{Component, ComponentConfig, ComponentStatus};
use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_minify::{compile_code, CompileMode, CompileOptions, NumericMode};
use storm_lua_spec::{draw::DrawCommand, io::CompositeSignal, screen::Rgba8};
use storm_lua_vm::{
    runner::{ErrorKind, LuaErrorStage, RunOutcome, StepMode},
    value::HostFunction,
    ExecutionLimits,
};

type TestResult = Result<(), Box<dyn Error>>;

fn minify_default(source: &str) -> Result<String, Box<dyn Error>> {
    Ok(compile_code(
        source,
        &CompileOptions {
            mode: CompileMode::Smallest,
            numeric_mode: NumericMode::Exact,
            ..Default::default()
        },
    )?
    .code)
}

fn load_vehicle(source: &str) -> Result<Microcontroller, Box<dyn Error>> {
    let mut vm = Microcontroller::new(MicrocontrollerConfig::default())?;
    vm.load(source.as_bytes(), "@main.lua")?;
    Ok(vm)
}

#[test]
fn bug_t1_01_effect_analyzer_preserves_wrong_phase_errors_across_minify() -> TestResult {
    // Case 1: `dead_local_elimination` must not drop an unused `input.getNumber(1)` inside `onDraw`
    // because in v0.5.0 (E4) it raises `attempting to use input function outside onTick`.
    let src_draw_input =
        "function onDraw()\n  local unused = input.getNumber(1)\n  screen.drawClear()\nend";
    let min_draw_input = minify_default(src_draw_input)?;
    let mut raw_vm = load_vehicle(src_draw_input)?;
    let mut min_vm = load_vehicle(&min_draw_input)?;
    let raw_err = raw_vm.draw(32, 32).err().ok_or("raw must fail in onDraw")?;
    let min_err = min_vm
        .draw(32, 32)
        .err()
        .ok_or_else(|| format!("minified erased mandatory phase error: {min_draw_input}"))?;
    assert_eq!(raw_err.kind, ErrorKind::Lua);
    assert_eq!(min_err.kind, ErrorKind::Lua);
    assert!(min_err
        .message
        .contains("attempting to use input function outside onTick"));
    assert_eq!(raw_vm.error_status(), 1);
    assert_eq!(min_vm.error_status(), 1);

    // Case 2: `single_use_forwarding` / `sink_single_use_locals` must not sink a wrong-phase
    // `input.getNumber(1)` past an expression (`y + 1` where `y` is nil) that raises a different error!
    let src_sink = "function onDraw()\n  local x = input.getNumber(1)\n  screen.drawText(0, 0, (y + 1) + x)\nend";
    let min_sink = minify_default(src_sink)?;
    let mut raw_vm = load_vehicle(src_sink)?;
    let mut min_vm = load_vehicle(&min_sink)?;
    let raw_sink_err = raw_vm.draw(32, 32).err().ok_or("raw must fail")?;
    let min_sink_err = min_vm.draw(32, 32).err().ok_or("min must fail")?;
    assert!(raw_sink_err
        .message
        .contains("attempting to use input function outside onTick"));
    assert!(
        min_sink_err
            .message
            .contains("attempting to use input function outside onTick"),
        "wrong-phase input.getNumber(1) sunk past nil arithmetic `{min_sink}`: got {}",
        min_sink_err.message
    );
    Ok(())
}

#[test]
fn bug_t1_02_licm_does_not_hoist_wrong_phase_host_reads_out_of_zero_trip_loops() -> TestResult {
    // Under target_size compilation (`target_speed`), `hoist_loop_invariant_host_reads` runs.
    // A 0-trip loop inside `onDraw` containing `input.getNumber` (or inside `onTick` containing
    // `screen.getWidth`) must not hoist the wrong-phase call into the preheader.
    let src = "n = 0\nfunction onDraw()\n  local s = 0\n  for i = 1, n do\n    s = s + input.getNumber(1) + input.getNumber(2)\n  end\n  screen.drawText(0, 0, tostring(s))\nend\nfunction onTick()\n  local s = 0\n  for i = 1, n do\n    s = s + screen.getWidth() + screen.getHeight()\n  end\n  output.setNumber(1, s)\nend";
    let minified = compile_code(
        src,
        &CompileOptions {
            mode: CompileMode::Smallest,
            numeric_mode: NumericMode::Exact,
            target_size: Some(4096),
            ..Default::default()
        },
    )?
    .code;

    let mut raw_vm = load_vehicle(src)?;
    let mut min_vm = load_vehicle(&minified)?;
    assert_eq!(raw_vm.draw(32, 32)?, RunOutcome::Completed);
    assert_eq!(
        min_vm.draw(32, 32)?,
        RunOutcome::Completed,
        "LICM hoisted input.getNumber into onDraw preheader: {minified}"
    );
    assert_eq!(
        raw_vm.tick(&CompositeSignal::default())?,
        RunOutcome::Completed
    );
    assert_eq!(
        min_vm.tick(&CompositeSignal::default())?,
        RunOutcome::Completed,
        "LICM hoisted screen.getWidth into onTick preheader: {minified}"
    );
    assert_eq!(min_vm.error_status(), 0);
    Ok(())
}

#[test]
fn bug_t1_03_output_stores_preserves_earlier_wrong_phase_error_before_global_write() -> TestResult {
    // Outside `onTick` (here in `onDraw`), the first `output.setNumber(1, 5)` immediately raises
    // `attempting to use output function outside onTick` on line 2 and aborts before `g = 99` or
    // line 4 (`output.setNumber(1, 9)`). Deleting line 2's store both executes `g = 99` and shifts
    // the source-mapped error site from line 2 to line 4!
    let src =
        "function onDraw()\n  output.setNumber(1, 5)\n  g = 99\n  output.setNumber(1, 9)\nend";
    let built = storm_lua_build::minify(
        src,
        &storm_lua_build::ApiCompileOptions {
            source_map: Some(true),
            zero_cost_newlines: Some(true),
            ..Default::default()
        },
    );
    let map_json = built.map.as_deref().ok_or("missing source map")?;
    let code = built.code.as_deref().ok_or("missing code")?;
    let sm = sourcemap::SourceMap::from_slice(map_json.as_bytes())?;

    let mut raw_vm = load_vehicle(src)?;
    let mut min_vm = load_vehicle(code)?;
    let raw_err = raw_vm.draw(32, 32).err().ok_or("raw must fail")?;
    let min_err = min_vm.draw(32, 32).err().ok_or("min must fail")?;
    let raw_line = raw_err
        .trace
        .as_ref()
        .and_then(|t| t.location.as_ref())
        .map(|l| l.line)
        .ok_or("missing raw location")?;
    let gen_line = min_err
        .trace
        .as_ref()
        .and_then(|t| t.location.as_ref())
        .map(|l| l.line)
        .ok_or("missing min location")?;
    // Map generated 1-based line back to original 1-based line.
    let mapped_line = (0..200_u32)
        .filter_map(|col| sm.lookup_token(gen_line.saturating_sub(1), col))
        .find(|tok| tok.get_src_line() > 0)
        .map(|tok| tok.get_src_line() + 1)
        .unwrap_or(0);
    assert_eq!(raw_line, 2);
    assert_eq!(
        mapped_line, 2,
        "output_stores deleted first throwing output.setNumber in onDraw (code={:?})",
        built.code
    );
    Ok(())
}

#[test]
fn bug_t1_04_wrong_arity_get_number_is_nil_not_truthy_or_droppable() -> TestResult {
    // In v0.5.0 (E1), `input.getNumber(1, 2)` and `property.getNumber()` return 0 values (`nil`).
    // `conditionals` must not lower `if c then x = input.getNumber(1, 2) else x = 99 end` to
    // `x = c and input.getNumber(1, 2) or 99` (which evaluates to 99 when c is true!).
    let src_cond = "c = true\nfunction onTick()\n  local x\n  if c then\n    x = input.getNumber(1, 2)\n  else\n    x = 99\n  end\n  output.setBool(1, x == nil)\nend";
    let min_cond = minify_default(src_cond)?;
    let mut raw_vm = load_vehicle(src_cond)?;
    let mut min_vm = load_vehicle(&min_cond)?;
    raw_vm.tick(&CompositeSignal::default())?;
    min_vm.tick(&CompositeSignal::default())?;
    assert!(raw_vm.output().booleans[0]);
    assert!(
        min_vm.output().booleans[0],
        "conditionals treated wrong-arity input.getNumber(1,2) as truthy: {min_cond}"
    );

    // `global_stores` must not drop unread `dead_global = input.getNumber(side())` or wrong-phase
    // `dead_global = input.getNumber(1)` at top-level.
    let src_global = "ran = 0\nlocal function side()\n  ran = 1\n  return 1\nend\nfunction onTick()\n  dead_global = input.getNumber(side())\n  output.setNumber(1, ran)\nend";
    let min_global = minify_default(src_global)?;
    let mut raw_vm = load_vehicle(src_global)?;
    let mut min_vm = load_vehicle(&min_global)?;
    raw_vm.tick(&CompositeSignal::default())?;
    min_vm.tick(&CompositeSignal::default())?;
    assert_eq!(raw_vm.output().numbers[0], 1.0);
    assert_eq!(
        min_vm.output().numbers[0],
        1.0,
        "global_stores dropped call with side-effecting argument: {min_global}"
    );

    // Float numeric-for counter `i = 0.5` throws on bitwise `~i` (`number has no integer representation`),
    // so `global_stores` must not treat `dead_global = ~i` as total/droppable.
    let src_float_for = "function onTick()\n  for i = 0.5, 2.5 do\n    dead_global = ~i\n  end\n  output.setNumber(1, 7)\nend";
    let min_float_for = minify_default(src_float_for)?;
    let mut raw_ff = load_vehicle(src_float_for)?;
    let mut min_ff = load_vehicle(&min_float_for)?;
    assert!(raw_ff.tick(&CompositeSignal::default()).is_err());
    assert!(
        min_ff.tick(&CompositeSignal::default()).is_err(),
        "global_stores dropped throwing bitwise op on float fornum counter: {min_float_for}"
    );
    Ok(())
}

#[test]
fn bug_t1_05_color_unpack_preserves_trailing_nil_alpha_and_shadowed_screen() -> TestResult {
    // `table.unpack({255, 0, 0, nil})` has `#t == 3` in Lua 5.3, passing 3 args -> alpha = 255.
    // Rewriting to `screen.setColor(255, 0, 0, nil)` passes 4 args with explicit nil -> alpha = 0 (E3)!
    let src = "local function color(c)\n  screen.setColor(table.unpack(c))\nend\nlocal red = {255, 0, 0, nil}\nfunction onDraw()\n  color(red)\n  screen.drawRectF(0, 0, 4, 4)\nend";
    let minified = minify_default(src)?;
    let mut raw_vm = load_vehicle(src)?;
    let mut min_vm = load_vehicle(&minified)?;
    raw_vm.draw(8, 8)?;
    min_vm.draw(8, 8)?;
    let extract_color = |vm: &Microcontroller| -> Option<Rgba8> {
        vm.commands().iter().find_map(|cmd| match cmd {
            DrawCommand::SetColor(rgba) => Some(*rgba),
            _ => None,
        })
    };
    assert_eq!(extract_color(&raw_vm), Some(Rgba8([255, 0, 0, 255])));
    assert_eq!(
        extract_color(&min_vm),
        Some(Rgba8([255, 0, 0, 255])),
        "color_unpack turned trailing nil into explicit 4th nil arg (alpha=0): {minified}"
    );

    // Also verify locally shadowed `screen` at the call site is not clobbered by synthesized `screen.setColor`.
    let src_shadow = "local function color(c)\n  screen.setColor(table.unpack(c))\nend\nfunction onDraw()\n  local screen = {}\n  color({10, 20, 30})\nend";
    let min_shadow = minify_default(src_shadow)?;
    let mut raw_vm = load_vehicle(src_shadow)?;
    let mut min_vm = load_vehicle(&min_shadow)?;
    assert_eq!(raw_vm.draw(8, 8)?, RunOutcome::Completed);
    assert_eq!(
        min_vm.draw(8, 8)?,
        RunOutcome::Completed,
        "color_unpack bound to locally shadowed `screen`: {min_shadow}"
    );
    assert_eq!(extract_color(&min_vm), Some(Rgba8([10, 20, 30, 255])));
    Ok(())
}

#[test]
fn bug_t1_06_screen_setcolor_respects_color_mutating_call_inside_draw_arguments() -> TestResult {
    let src = "local function label()\n  screen.setColor(0, 255, 0)\n  return 'hi'\nend\nfunction onDraw()\n  screen.setColor(255, 0, 0)\n  screen.drawText(0, 0, label())\n  screen.setColor(255, 0, 0)\n  screen.drawRectF(0, 0, 2, 2)\nend";
    let minified = minify_default(src)?;
    let mut raw_vm = load_vehicle(src)?;
    let mut min_vm = load_vehicle(&minified)?;
    raw_vm.draw(16, 16)?;
    min_vm.draw(16, 16)?;
    assert_eq!(
        &*min_vm.commands(),
        &*raw_vm.commands(),
        "screen_setcolor deleted needed setColor after color-mutating drawText arg: {minified}"
    );
    Ok(())
}

#[test]
fn bug_t1_07_matrix_errors_record_recoverable_lua_status_in_component_and_addon() -> TestResult {
    // Component: uncaught `matrix.multiply()` in `onTick` must record `TickFailed` (status 2)
    // with `ErrorKind::Lua` and `LuaErrorStage::Runtime`, keeping `render` & `remove_from_simulation` alive!
    let mut comp = Component::new(ComponentConfig::default())?;
    comp.load(
        b"rendered = 0\nfunction onTick()\n  matrix.multiply()\nend\nfunction onRender()\n  rendered = rendered + 1\nend",
        "@comp.lua",
    )?;
    comp.parse()?;
    comp.add_to_simulation()?;
    let err = comp.tick(1).err().ok_or("matrix.multiply() must error")?;
    assert_eq!(err.kind, ErrorKind::Lua, "got: {err:?}");
    assert_eq!(
        err.trace.as_ref().and_then(|t| t.stage),
        Some(LuaErrorStage::Runtime)
    );
    assert_eq!(comp.status(), ComponentStatus::TickFailed);
    assert!(!comp.is_failed());
    // Per Component spec, `onRender` is NOT gated by status 2 and must still run!
    assert_eq!(comp.render()?, RunOutcome::Completed);

    // Addon: uncaught `matrix.invert(nil)` in `onCustomCommand` must record status 5 (`ErrorKind::Lua`),
    // not poison `addon.is_failed()`.
    let mut addon = Addon::new(AddonConfig::default())?;
    addon.load(
        b"function onCustomCommand()\n  matrix.invert(nil)\nend",
        "@addon.lua",
    )?;
    addon.start()?;
    let addon_err = addon
        .dispatch("onCustomCommand", &[])
        .err()
        .ok_or("matrix.invert(nil) must error")?;
    assert_eq!(addon_err.kind, ErrorKind::Lua, "got: {addon_err:?}");
    assert!(!addon.is_failed());
    assert_eq!(addon.error_status(), 5);
    Ok(())
}

#[test]
fn bug_t1_08_architecture_policy_has_no_stale_external_dependencies() -> TestResult {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("missing workspace root")?;
    let arch_text = std::fs::read_to_string(repo_root.join("tools/architecture.json"))?;
    let policy: serde_json::Value = serde_json::from_str(&arch_text)?;
    let minify_ext = policy["storm-lua-minify"]["external"]
        .as_array()
        .ok_or("missing storm-lua-minify.external")?;
    assert!(
        !minify_ext.iter().any(|v| v.as_str() == Some("bincode")),
        "stale 'bincode' remains in tools/architecture.json: {minify_ext:?}"
    );
    let output = Command::new("python3")
        .arg(repo_root.join("tools/check_repository.py"))
        .current_dir(repo_root)
        .output()?;
    assert!(
        output.status.success(),
        "check_repository.py failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn bug_t1_11_wall_clock_deadline_pauses_while_suspended_and_survives_nested_timeout() -> TestResult
{
    // Part 1: Pausing at a debugger breakpoint longer than `time_limit` must NOT cause `resume()`
    // to immediately fail with `max execution time exceeded`.
    let mut vm = Microcontroller::new(MicrocontrollerConfig {
        limits: ExecutionLimits {
            instruction_budget: Some(NonZeroU64::new(2_000_000).ok_or("nonzero")?),
            time_limit: Some(Duration::from_millis(30)),
            ..ExecutionLimits::default()
        },
        ..Default::default()
    })?;
    vm.load(
        b"function onTick()\n  local s = 0\n  for i = 1, 1000 do s = s + i end\n  output.setNumber(1, s)\nend",
        "@main.lua",
    )?;
    vm.set_breakpoints(vec![("@main.lua".into(), 2)])?;
    assert_eq!(vm.tick(&CompositeSignal::default())?, RunOutcome::Suspended);
    std::thread::sleep(Duration::from_millis(45));
    vm.set_breakpoints(Vec::new())?;
    assert_eq!(
        vm.resume(StepMode::Continue)?,
        RunOutcome::Completed,
        "wall-clock deadline kept ticking while VM was suspended in debugger"
    );
    assert_eq!(vm.output().numbers[0], 500_500.0);

    // Part 2: When a nested synchronous Addon callback hits `time_limit` and the host recovers,
    // the outer entry's `deadline` must NOT be cleared to `None`.
    let events_slot: Arc<Mutex<Option<storm_lua_addon::SynchronousEvents>>> =
        Arc::new(Mutex::new(None));
    let slot_for_fn = Arc::clone(&events_slot);
    let trigger: HostFunction = Arc::new(move |_args| {
        if let Some(events) = slot_for_fn
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
        {
            // Nested callback times out; host observes the recoverable Lua timeout and returns.
            let _ = events.dispatch("onCustomCommand", &[]);
        }
        Ok(Vec::new())
    });
    let mut server = BTreeMap::new();
    server.insert("trigger".to_string(), trigger);
    let mut addon = Addon::new(AddonConfig {
        limits: ExecutionLimits {
            instruction_budget: Some(NonZeroU64::new(5_000_000).ok_or("nonzero")?),
            time_limit: Some(Duration::from_millis(20)),
            ..ExecutionLimits::default()
        },
        server,
        ..Default::default()
    })?;
    *events_slot
        .lock()
        .unwrap_or_else(|poison| poison.into_inner()) = Some(addon.synchronous_events());
    addon.load(
        b"function onCustomCommand()\n  while true do end\nend\nfunction onTick()\n  server.trigger()\n  while true do end\nend",
        "@addon.lua",
    )?;
    addon.start()?;
    let outer_err = addon.tick(1).err().ok_or("outer infinite loop must stop")?;
    assert_eq!(
        outer_err.kind,
        ErrorKind::Lua,
        "outer deadline was cleared by inner timeout, hit instruction Limit instead of Lua timeout: {outer_err:?}"
    );
    assert!(
        outer_err.message.contains("max execution time exceeded"),
        "got: {}",
        outer_err.message
    );
    assert!(!addon.is_failed());
    Ok(())
}

#[test]
fn bug_t1_12_breakable_lines_honours_execution_limits_max_source_bytes() -> TestResult {
    // 1.2 MiB valid Lua script: exceeds legacy hardcoded 1 MiB cap, allowed when `max_source_bytes` is `None`
    // (the v0.5.0 E16 default) or 2 MiB, and rejected with `ErrorKind::Limit` when `max_source_bytes` is 64 bytes.
    let mut big = String::with_capacity(1_200_100);
    big.push_str("--");
    big.push_str(&"a".repeat(1_150_000));
    big.push_str("\nfunction onTick()\n  output.setNumber(1, 1)\nend\n");

    let vm_unlimited = Microcontroller::new(MicrocontrollerConfig::default())?;
    let lines = vm_unlimited.breakable_lines(big.as_bytes(), "@big.lua")?;
    assert!(lines.contains(&3), "expected line 3 in {lines:?}");

    let vm_capped = Microcontroller::new(MicrocontrollerConfig {
        limits: ExecutionLimits {
            max_source_bytes: NonZeroUsize::new(64),
            ..ExecutionLimits::default()
        },
        ..Default::default()
    })?;
    let err = vm_capped
        .breakable_lines(
            b"function onTick()\n  local a = 1234567890123456789012345678901234567890\nend",
            "@capped.lua",
        )
        .err()
        .ok_or("expected max_source_bytes Limit error")?;
    assert_eq!(err.kind, ErrorKind::Limit);
    Ok(())
}

#[test]
fn bug_t1_04_bitwise_on_floats_and_channels_not_dropped_or_forwarded() -> TestResult {
    // 1) Unread global store with bitwise `~input.getNumber(1)` where channel 1 is `0.5`:
    let src_chan =
        "function onTick()\n  dead_global = ~input.getNumber(1)\n  output.setNumber(1, 7)\nend";
    let min_chan = minify_default(src_chan)?;
    let mut sig = CompositeSignal::default();
    sig.numbers[0] = 0.5;
    let mut raw_vm = load_vehicle(src_chan)?;
    let mut min_vm = load_vehicle(&min_chan)?;
    let raw_err = raw_vm.tick(&sig).err().ok_or("raw must fail on ~0.5")?;
    let min_err = min_vm
        .tick(&sig)
        .err()
        .ok_or_else(|| format!("global_stores dropped throwing ~input.getNumber(1): {min_chan}"))?;
    assert!(raw_err
        .message
        .contains("number has no integer representation"));
    assert!(min_err
        .message
        .contains("number has no integer representation"));
    assert_eq!(min_vm.output().numbers[0], 0.0);

    // 2) Unread global store with float literal `~0.5` and `~(1 / 2)`:
    for rhs in ["~0.5", "~(1 / 2)", "input.getNumber(1) & 1"] {
        let src =
            format!("function onTick()\n  dead_global = {rhs}\n  output.setNumber(1, 7)\nend");
        let min = minify_default(&src)?;
        let mut vm = load_vehicle(&min)?;
        assert!(
            vm.tick(&sig).is_err(),
            "global_stores dropped throwing `{rhs}` -> `{min}`"
        );
    }

    // 3) `single_use_forwarding` must not sink throwing `g = ~input.getNumber(1)` past an earlier error:
    let src_fwd = "flag = false\nfunction onTick()\n  g = ~input.getNumber(1)\n  if flag then output.setNumber(1, g) end\n  output.setNumber(2, 9)\nend";
    let min_fwd = minify_default(src_fwd)?;
    let mut fwd_vm = load_vehicle(&min_fwd)?;
    assert!(
        fwd_vm.tick(&sig).is_err(),
        "single_use_forwarding/global_stores erased throwing `~input.getNumber(1)`: {min_fwd}"
    );
    Ok(())
}

#[test]
fn bug_t1_13_available_expressions_does_not_hoist_wrong_phase_host_reads() -> TestResult {
    let src = "guard = false\nfunction onTick()\n  local a = guard and math.sin(screen.getWidth())\n  local b = guard and math.sin(screen.getWidth())\n  local c = guard and math.sin(screen.getWidth())\n  output.setBool(1, not a and not b and not c)\nend\nfunction onDraw()\n  local a = guard and math.sin(input.getNumber(1))\n  local b = guard and math.sin(input.getNumber(1))\n  local c = guard and math.sin(input.getNumber(1))\n  screen.drawClear()\nend";
    let min = minify_default(src)?;
    let mut raw_vm = load_vehicle(src)?;
    let mut min_vm = load_vehicle(&min)?;
    assert_eq!(
        raw_vm.tick(&CompositeSignal::default())?,
        RunOutcome::Completed
    );
    assert_eq!(
        min_vm.tick(&CompositeSignal::default())?,
        RunOutcome::Completed,
        "available_expressions hoisted screen.getWidth() out of short-circuit in onTick: {min}"
    );
    assert!(min_vm.output().booleans[0]);
    assert_eq!(raw_vm.draw(32, 32)?, RunOutcome::Completed);
    assert_eq!(
        min_vm.draw(32, 32)?,
        RunOutcome::Completed,
        "available_expressions hoisted input.getNumber(1) out of short-circuit in onDraw: {min}"
    );
    assert_eq!(min_vm.error_status(), 0);
    Ok(())
}

#[test]
fn bug_t1_01_host_on_entrypoints_not_certified_as_tick_only() -> TestResult {
    // A global `httpReply` Vehicle host entrypoint that happens to have an unreached call inside
    // `onTick` is still externally invoked by the host outside `onTick`, so `input.getNumber(1)`
    // inside `httpReply` must NOT be certified `TickOnly` and erased by `dead_local_elimination`!
    let src_entry = "guard = false\nfunction httpReply(port, req, res)\n  local unused = input.getNumber(1)\nend\nfunction onTick()\n  async.httpGet(80, \"/\")\n  if guard then httpReply(80, \"/\", \"\") end\nend";
    let min_entry = minify_default(src_entry)?;
    let mut raw_vm = load_vehicle(src_entry)?;
    let mut min_vm = load_vehicle(&min_entry)?;
    raw_vm.tick(&CompositeSignal::default())?;
    min_vm.tick(&CompositeSignal::default())?;
    let raw_tok = raw_vm
        .drain_http_requests()
        .pop()
        .ok_or("missing raw http req")?
        .token;
    let min_tok = min_vm
        .drain_http_requests()
        .pop()
        .ok_or("missing min http req")?
        .token;
    let raw_err = raw_vm
        .http_reply(raw_tok, b"ok")
        .err()
        .ok_or("raw httpReply must fail")?;
    let min_err = min_vm
        .http_reply(min_tok, b"ok")
        .err()
        .ok_or_else(|| format!("httpReply was misclassified as TickOnly: {min_entry}"))?;
    assert!(raw_err
        .message
        .contains("attempting to use input function outside onTick"));
    assert!(min_err
        .message
        .contains("attempting to use input function outside onTick"));
    assert_eq!(min_vm.error_status(), 5);
    Ok(())
}
