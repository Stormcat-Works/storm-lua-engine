//! 一時停止の予約、入口をまたぐステップ、実行中の一時停止、ブレークポイントを置ける行。
use std::{
    error::Error,
    num::NonZeroU64,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc,
    },
};
use storm_lua_addon::{Addon, AddonConfig};
use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_spec::io::CompositeSignal;
use storm_lua_vm::{
    logging::LogLocation,
    runner::{ErrorKind, LuaErrorStage, PausePoll, RunOutcome, StepMode, PAUSE_POLL_INTERVAL},
};

type TestResult = Result<(), Box<dyn Error>>;

const SCRIPT: &[u8] = b"count = 0
function onTick()
  count = count + 1
  output.setNumber(1, count)
end
function onDraw()
  screen.drawClear()
end";

fn vehicle(source: &[u8]) -> Result<Microcontroller, Box<dyn Error>> {
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(source, "@main.lua")?;
    Ok(vm)
}
fn top(vm: &Microcontroller) -> Result<(String, i32), Box<dyn Error>> {
    let frame = vm.stack()?.into_iter().next().ok_or("empty stack")?;
    Ok((frame.source, frame.line))
}
fn at(line: i32) -> (String, i32) {
    ("@main.lua".into(), line)
}
fn signal() -> CompositeSignal {
    CompositeSignal::default()
}

#[test]
fn pause_requested_while_idle_stops_on_the_first_line_of_the_next_entry() -> TestResult {
    let mut vm = vehicle(SCRIPT)?;
    assert_eq!(vm.tick(&signal())?, RunOutcome::Completed);
    vm.request_pause()?;
    assert_eq!(vm.tick(&signal())?, RunOutcome::Suspended);
    assert_eq!(top(&vm)?, at(3));
    // 予約は1回の停止で消える。
    assert_eq!(vm.resume(StepMode::Continue)?, RunOutcome::Completed);
    assert_eq!(vm.tick(&signal())?, RunOutcome::Completed);
    assert_eq!(vm.output().numbers[0], 3.0);
    Ok(())
}

#[test]
fn pause_reservation_survives_a_missing_callback_and_can_be_cancelled() -> TestResult {
    let mut vm = vehicle(b"function onTick()\n  output.setNumber(1, 1)\nend")?;
    vm.request_pause()?;
    assert_eq!(vm.draw(32, 32)?, RunOutcome::Missing);
    assert_eq!(vm.tick(&signal())?, RunOutcome::Suspended);
    assert_eq!(top(&vm)?, at(2));
    assert_eq!(vm.resume(StepMode::Continue)?, RunOutcome::Completed);
    vm.request_pause()?;
    vm.cancel_pause();
    assert_eq!(vm.tick(&signal())?, RunOutcome::Completed);
    Ok(())
}

#[test]
fn pause_requested_while_suspended_stops_on_the_next_line_after_continue() -> TestResult {
    let mut vm = vehicle(SCRIPT)?;
    vm.set_breakpoints(vec![("@main.lua".into(), 3)])?;
    assert_eq!(vm.tick(&signal())?, RunOutcome::Suspended);
    vm.set_breakpoints(Vec::new())?;
    vm.request_pause()?;
    assert_eq!(vm.resume(StepMode::Continue)?, RunOutcome::Suspended);
    assert_eq!(top(&vm)?, at(4));
    assert_eq!(vm.resume(StepMode::Continue)?, RunOutcome::Completed);
    Ok(())
}

#[test]
fn pause_reservation_does_not_change_execution_without_the_request() -> TestResult {
    let mut vm = vehicle(SCRIPT)?;
    for expected in 1..=3 {
        assert_eq!(vm.tick(&signal())?, RunOutcome::Completed);
        assert_eq!(vm.draw(32, 32)?, RunOutcome::Completed);
        assert_eq!(vm.output().numbers[0], expected as f32);
    }
    Ok(())
}

#[test]
fn step_past_the_end_of_an_entry_is_dropped_unless_carried() -> TestResult {
    for mode in [StepMode::Over, StepMode::Into, StepMode::Out] {
        let mut vm = vehicle(SCRIPT)?;
        vm.set_breakpoints(vec![("@main.lua".into(), 4)])?;
        assert_eq!(vm.tick(&signal())?, RunOutcome::Suspended);
        vm.set_breakpoints(Vec::new())?;
        // 最後の行（5行目のend）まで進めてから、入口の終わりを越える。
        assert_eq!(vm.resume(StepMode::Over)?, RunOutcome::Suspended);
        assert_eq!(top(&vm)?, at(5));
        // 従来どおり、指定がなければステップは入口の終わりで消える。
        assert_eq!(vm.resume(mode)?, RunOutcome::Completed);
        assert_eq!(vm.draw(32, 32)?, RunOutcome::Completed);
        assert_eq!(vm.tick(&signal())?, RunOutcome::Completed);
    }
    Ok(())
}

#[test]
fn carried_step_stops_on_the_first_line_of_the_next_entry() -> TestResult {
    for mode in [StepMode::Over, StepMode::Into, StepMode::Out] {
        let mut vm = vehicle(SCRIPT)?;
        vm.set_breakpoints(vec![("@main.lua".into(), 4)])?;
        assert_eq!(vm.tick(&signal())?, RunOutcome::Suspended);
        vm.set_breakpoints(Vec::new())?;
        assert_eq!(vm.resume(StepMode::Over)?, RunOutcome::Suspended);
        assert_eq!(top(&vm)?, at(5));
        assert_eq!(vm.resume_with(mode, true)?, RunOutcome::Completed);
        // 次に実行する入口がdrawなら、onDrawの最初の行で止まる。
        assert_eq!(vm.draw(32, 32)?, RunOutcome::Suspended);
        assert_eq!(top(&vm)?, at(7));
        // そこからさらに持ち越すと、次のtickのonTickの最初の行で止まる。
        assert_eq!(vm.resume_with(StepMode::Out, true)?, RunOutcome::Completed);
        assert_eq!(vm.tick(&signal())?, RunOutcome::Suspended);
        assert_eq!(top(&vm)?, at(3));
        assert_eq!(vm.resume(StepMode::Continue)?, RunOutcome::Completed);
        assert_eq!(vm.tick(&signal())?, RunOutcome::Completed);
    }
    Ok(())
}

#[test]
fn carried_continue_and_a_step_that_stops_inside_the_entry_reserve_nothing() -> TestResult {
    let mut vm = vehicle(SCRIPT)?;
    vm.set_breakpoints(vec![("@main.lua".into(), 3)])?;
    assert_eq!(vm.tick(&signal())?, RunOutcome::Suspended);
    vm.set_breakpoints(Vec::new())?;
    // 同じ入口の中で止まるステップは予約を作らない。
    assert_eq!(vm.resume_with(StepMode::Over, true)?, RunOutcome::Suspended);
    assert_eq!(top(&vm)?, at(4));
    assert_eq!(
        vm.resume_with(StepMode::Continue, true)?,
        RunOutcome::Completed
    );
    assert_eq!(vm.tick(&signal())?, RunOutcome::Completed);
    Ok(())
}

/// pollを登録したVM、止めるかどうかのフラグ、pollが呼ばれた回数。
type Polled = (Microcontroller, Arc<AtomicBool>, Arc<AtomicU32>);
fn polled(source: &[u8], budget: u64) -> Result<Polled, Box<dyn Error>> {
    let mut config = MicrocontrollerConfig::default();
    config.limits.instruction_budget = Some(NonZeroU64::new(budget).ok_or("zero budget")?);
    let mut vm = Microcontroller::new(config)?;
    vm.load(source, "@main.lua")?;
    let (flag, calls) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU32::new(0)),
    );
    let (pause, counter) = (Arc::clone(&flag), Arc::clone(&calls));
    let poll: PausePoll = Arc::new(move || {
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(pause.load(Ordering::Relaxed))
    });
    vm.set_pause_poll(Some(poll))?;
    Ok((vm, flag, calls))
}

const LOOP: &[u8] = b"n = 0
function onTick()
  while true do
    n = n + 1
    m = n
  end
end";

#[test]
fn pause_poll_stops_a_running_loop_and_the_vm_stays_inspectable() -> TestResult {
    let (mut vm, flag, calls) = polled(LOOP, 1_000_000)?;
    flag.store(true, Ordering::Relaxed);
    assert_eq!(vm.tick(&signal())?, RunOutcome::Suspended);
    // 最初の問い合わせで止まる。上限エラーにはならず、VMは検査できる。
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert!(!vm.is_failed());
    let (source, line) = top(&vm)?;
    assert_eq!(source, "@main.lua");
    assert!((3..=6).contains(&line), "{line}");
    assert!(matches!(
        vm.evaluate_watch(0, "n")?,
        storm_lua_vm::debug::DebugValue::Integer(n) if n > 0
    ));
    // 行フックなしで始まった実行でも、止まった後のステップは次の行で止まる。
    flag.store(false, Ordering::Relaxed);
    assert_eq!(vm.resume(StepMode::Over)?, RunOutcome::Suspended);
    let (_, next) = top(&vm)?;
    assert!((3..=6).contains(&next) && next != line, "{line} -> {next}");
    // 後から置いたブレークポイントも働く。
    vm.set_breakpoints(vec![("@main.lua".into(), 4)])?;
    assert_eq!(vm.resume(StepMode::Continue)?, RunOutcome::Suspended);
    assert_eq!(top(&vm)?, at(4));
    Ok(())
}

#[test]
fn pause_poll_is_called_once_per_interval_and_never_without_registration() -> TestResult {
    let budget = u64::from(PAUSE_POLL_INTERVAL) * 10 + 100;
    let (mut vm, _flag, calls) = polled(LOOP, budget)?;
    // 止めなければ、従来どおり命令数の上限で失敗する。
    assert_eq!(
        vm.tick(&signal()).err().ok_or("no limit")?.kind,
        ErrorKind::Limit
    );
    assert_eq!(calls.load(Ordering::Relaxed), 10);
    let (mut short, _flag, calls) = polled(SCRIPT, 1_000_000)?;
    assert_eq!(short.tick(&signal())?, RunOutcome::Completed);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
fn pause_poll_survives_reset_and_can_be_removed() -> TestResult {
    let (mut vm, flag, calls) = polled(LOOP, 1_000_000)?;
    flag.store(true, Ordering::Relaxed);
    assert_eq!(vm.tick(&signal())?, RunOutcome::Suspended);
    // 中断中は設定を変えられない。
    assert_eq!(
        vm.set_pause_poll(None).err().ok_or("accepted")?.kind,
        ErrorKind::Busy
    );
    vm.reset()?;
    assert_eq!(vm.tick(&signal())?, RunOutcome::Suspended);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    vm.reset()?;
    vm.set_pause_poll(None)?;
    assert_eq!(
        vm.tick(&signal()).err().ok_or("no limit")?.kind,
        ErrorKind::Limit
    );
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    Ok(())
}

#[test]
fn reset_replays_long_top_level_code_without_polling() -> TestResult {
    // トップレベルが問い合わせの間隔を超えて走り、pollは常に止めようとする。
    let source = b"sum = 0
for i = 1, 20000 do sum = sum + i end
function onTick()
  output.setNumber(1, 1)
end";
    let mut vm = vehicle(source)?;
    let calls = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&calls);
    let poll: PausePoll = Arc::new(move || {
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(true)
    });
    vm.set_pause_poll(Some(poll))?;
    vm.reset()?;
    assert!(!vm.is_suspended() && !vm.is_failed());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    // 再生後のVMは通常どおり入口を受け付け、pollも引き継いでいる。
    assert_eq!(vm.draw(32, 32)?, RunOutcome::Missing);
    let mut looping =
        vehicle(b"for i = 1, 20000 do x = i end\nfunction onTick() while true do end end")?;
    let poll: PausePoll = Arc::new(|| Ok(true));
    looping.set_pause_poll(Some(poll))?;
    looping.reset()?;
    assert!(!looping.is_suspended());
    assert_eq!(looping.tick(&signal())?, RunOutcome::Suspended);
    Ok(())
}

#[test]
fn pause_poll_inside_a_protected_call_stops_at_the_next_yieldable_instruction() -> TestResult {
    let mut config = MicrocontrollerConfig {
        environment: storm_lua_spec::environment::EnvironmentProfile::Extended,
        ..Default::default()
    };
    config.limits.instruction_budget = Some(NonZeroU64::new(1_000_000).ok_or("zero")?);
    let mut vm = Microcontroller::new(config)?;
    // table.sortの比較関数はCの関数をまたぐため、その中ではyieldできない。
    vm.load(
        b"done = 0
function onTick()
  table.sort({3, 2, 1}, function(a, b)
    for i = 1, 20000 do done = done + 1 end
    return a < b
  end)
  marker = done
  while true do end
end",
        "@main.lua",
    )?;
    let poll: PausePoll = Arc::new(|| Ok(true));
    vm.set_pause_poll(Some(poll))?;
    assert_eq!(vm.tick(&signal())?, RunOutcome::Suspended);
    // 比較関数の途中では止まらず、sortが終わった後の行で止まる。
    let (_, line) = top(&vm)?;
    assert!(line >= 7, "{line}");
    Ok(())
}

#[test]
fn pause_poll_failure_reaches_the_script_as_a_host_error() -> TestResult {
    let mut vm = vehicle(LOOP)?;
    let poll: PausePoll = Arc::new(|| {
        Err(storm_lua_vm::runner::VmError::new(
            ErrorKind::Host,
            "poll failed",
        ))
    });
    vm.set_pause_poll(Some(poll))?;
    let error = vm.tick(&signal()).err().ok_or("poll error was hidden")?;
    assert_eq!(error.kind, ErrorKind::Host);
    assert!(error.message.contains("poll failed"));
    assert!(vm.is_failed());
    Ok(())
}

const LINES: &str = "local total = 0

-- comment only
local function add(n)
  total = total + n

  return function()
    return total
  end
end
function onTick()
  local read = add(2)
  if read() > 100 then
    output.setNumber(2, 1)
  end
  output.setNumber(1, read())
end
";

#[test]
fn breakable_lines_match_where_breakpoints_actually_stop() -> TestResult {
    let line_count = LINES.lines().count() as u32;
    let reported = vehicle(b"")?.breakable_lines(LINES.as_bytes(), "@main.lua")?;
    assert!(reported.windows(2).all(|pair| pair[0] < pair[1]));
    // 空行、コメント行、local functionの開始行には命令がない。
    for blank in [2, 3, 4, 6] {
        assert!(!reported.contains(&blank), "{blank} in {reported:?}");
    }
    let mut stopped = Vec::new();
    for line in 1..=line_count {
        // 読み込み（トップレベル）とtickの両方で、その行に止まるかを実際に確かめる。
        let mut vm = Microcontroller::new(Default::default())?;
        vm.set_breakpoints(vec![("@main.lua".into(), line as i32)])?;
        let mut hit = false;
        let mut outcome = vm.load(LINES.as_bytes(), "@main.lua")?;
        for _ in 0..2 {
            while outcome == RunOutcome::Suspended {
                assert_eq!(top(&vm)?, at(line as i32));
                hit = true;
                outcome = vm.resume(StepMode::Continue)?;
            }
            outcome = vm.tick(&signal())?;
        }
        if hit {
            stopped.push(line);
        }
    }
    // 14行目は条件が成り立たず実行されないので、置けるが止まらない。
    assert!(reported.contains(&14) && !stopped.contains(&14));
    let reachable: Vec<u32> = reported
        .iter()
        .copied()
        .filter(|line| *line != 14)
        .collect();
    assert_eq!(stopped, reachable);
    Ok(())
}

#[test]
fn breakable_lines_do_not_execute_or_retain_the_source() -> TestResult {
    let mut vm = vehicle(b"function onTick() output.setNumber(1, 7) end")?;
    let lines = vm.breakable_lines(
        b"output.setNumber(1, 99)\nfunction onTick() end",
        "@other.lua",
    )?;
    assert_eq!(lines, vec![1, 2]);
    vm.tick(&signal())?;
    assert_eq!(vm.output().numbers[0], 7.0);
    vm.reset()?;
    vm.tick(&signal())?;
    assert_eq!(vm.output().numbers[0], 7.0);
    // 構文エラーは、読み込みのときと同じ位置情報を持つ。チャンク名は省略されない。
    const LONG: &str = "@a/path/that/goes/on/and/on/for/more/than/sixty/characters/bad.lua";
    for name in [LONG, "@bad.lua", "=literal"] {
        let error = vm
            .breakable_lines(b"local a = 1\nlocal = 1", name)
            .err()
            .ok_or("accepted")?;
        assert_eq!(error.kind, ErrorKind::Lua);
        let trace = error.trace.as_deref().ok_or("no trace")?;
        assert_eq!(trace.stage, Some(LuaErrorStage::Syntax));
        assert_eq!(
            trace.location,
            Some(LogLocation {
                chunk: name.into(),
                line: 2
            }),
            "{name}"
        );
        assert!(trace.stack.is_empty());
    }
    // 中断中でも問い合わせられる。
    vm.request_pause()?;
    assert_eq!(vm.tick(&signal())?, RunOutcome::Suspended);
    assert_eq!(vm.breakable_lines(b"x = 1", "@other.lua")?, vec![1]);
    Ok(())
}

#[test]
fn addon_callbacks_honour_pause_requests_and_carried_steps() -> TestResult {
    let mut addon = Addon::new(AddonConfig::default())?;
    addon.load(
        b"ticks = 0\nfunction onTick(n)\n  ticks = ticks + n\n  ticks = ticks + 0\nend",
        "@addon.lua",
    )?;
    addon.start()?;
    addon.request_pause()?;
    assert_eq!(addon.tick(1)?, RunOutcome::Suspended);
    assert_eq!(addon.stack()?[0].line, 3);
    assert_eq!(
        addon.resume_with(StepMode::Out, true)?,
        RunOutcome::Completed
    );
    assert_eq!(addon.tick(1)?, RunOutcome::Suspended);
    assert_eq!(addon.stack()?[0].line, 3);
    addon.cancel_pause();
    assert_eq!(addon.resume(StepMode::Continue)?, RunOutcome::Completed);
    assert_eq!(addon.tick(1)?, RunOutcome::Completed);
    assert_eq!(
        addon.breakable_lines(b"x = 1\n\ny = 2", "@x.lua")?,
        vec![1, 3]
    );
    Ok(())
}
