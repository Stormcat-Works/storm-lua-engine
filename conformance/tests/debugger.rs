//! ホストデバッガのセマンティクス、ハンドルの生存期間、およびバジェット構成。
use std::error::Error;
use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_spec::{
    io::CompositeSignal,
    property::{PropertyBag, PropertyValue},
};
use storm_lua_vm::{
    debug::{DebugHandle, DebugValue},
    runner::{ErrorKind, RunOutcome, StepMode},
};

fn paused() -> Result<Microcontroller, Box<dyn Error>> {
    paused_in(storm_lua_spec::environment::EnvironmentProfile::Game)
}
fn paused_in(
    environment: storm_lua_spec::environment::EnvironmentProfile,
) -> Result<Microcontroller, Box<dyn Error>> {
    let mut vm = Microcontroller::new(MicrocontrollerConfig {
        environment,
        ..Default::default()
    })?;
    vm.load(
        b"function onTick()
 local x=9223372036854775807
 local t={a=1,b=2}
 local nothing=nil
 output.setNumber(1,t.a)
end",
        "=debug",
    )?;
    vm.set_breakpoints(vec![("=debug".into(), 5)])?;
    assert_eq!(vm.tick(&CompositeSignal::default())?, RunOutcome::Suspended);
    Ok(vm)
}
#[test]
fn inspect_continue_and_stale_handles() -> Result<(), Box<dyn Error>> {
    let mut vm = paused()?;
    let stack = vm.stack()?;
    assert_eq!(stack[0].source, "=debug");
    assert_eq!(stack[0].line, 5);
    let locals = vm.locals(0)?;
    assert!(locals
        .iter()
        .any(|v| v.name == b"x" && v.value == DebugValue::Integer(i64::MAX)));
    let handle = locals
        .iter()
        .find_map(|v| {
            if v.name == b"t" {
                if let DebugValue::Table(h) = v.value {
                    return Some(h);
                }
            }
            None
        })
        .ok_or("no table")?;
    assert_eq!(vm.expand_table(handle, 0, 16)?.len(), 2);
    assert!(vm.set_properties(PropertyBag::default()).is_err());
    // 中断中は1件の設定と削除もBusyで拒否し、プロパティを変えない。
    for error in [
        vm.set_property(b"a", PropertyValue::Bool(true)).err(),
        vm.remove_property(b"a").err(),
    ] {
        assert_eq!(
            error.ok_or("accepted while suspended")?.kind,
            ErrorKind::Busy
        );
    }
    assert!(vm.properties().is_empty());
    assert!(vm.tick(&CompositeSignal::default()).is_err());
    vm.set_breakpoints(Vec::new())?;
    assert_eq!(vm.resume(StepMode::Continue)?, RunOutcome::Completed);
    assert_eq!(vm.output().numbers[0], 1.0);
    assert!(vm.expand_table(handle, 0, 16).is_err());
    Ok(())
}
#[test]
fn watch_preserves_i64_nil_and_bytes_and_restores_execution_hook() -> Result<(), Box<dyn Error>> {
    let mut vm = paused()?;
    assert_eq!(vm.evaluate_watch(0, "x")?, DebugValue::Integer(i64::MAX));
    assert_eq!(vm.evaluate_watch(0, "nothing")?, DebugValue::Nil);
    assert_eq!(
        vm.evaluate_watch(0, "string.char(0,255)")?,
        DebugValue::Bytes(vec![0, 255])
    );
    assert_eq!(
        vm.evaluate_watch(0, "(function()t.a=7;return t.a end)()")?,
        DebugValue::Integer(7)
    );
    vm.set_breakpoints(Vec::new())?;
    assert_eq!(vm.resume(StepMode::Continue)?, RunOutcome::Completed);
    assert_eq!(vm.output().numbers[0], 7.0);
    Ok(())
}
#[test]
fn nil_local_shadows_global_in_watch() -> Result<(), Box<dyn Error>> {
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(
        b"nothing=123
function onTick()
 local nothing=nil
 output.setNumber(1,0)
end",
        "=shadow",
    )?;
    vm.set_breakpoints(vec![("=shadow".into(), 4)])?;
    vm.tick(&CompositeSignal::default())?;
    assert_eq!(vm.evaluate_watch(0, "nothing")?, DebugValue::Nil);
    Ok(())
}
#[test]
fn runaway_watch_is_bounded_and_main_continuation_survives() -> Result<(), Box<dyn Error>> {
    let mut vm = paused_in(storm_lua_spec::environment::EnvironmentProfile::Extended)?;
    for expression in ["(function()while true do end end)()","(function()table.sort({1,2},function()while true do pcall(function()while true do end end)end end)end)()"]{
        assert_eq!(vm.evaluate_watch(0,expression).err().ok_or("watch did not fail")?.kind,ErrorKind::Limit);
    }
    vm.set_breakpoints(Vec::new())?;
    assert_eq!(vm.resume(StepMode::Continue)?, RunOutcome::Completed);
    Ok(())
}
#[test]
fn step_over_skips_callee_and_out_returns_to_caller() -> Result<(), Box<dyn Error>> {
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(
        b"function f()
 local x=1
 return x
end
function onTick()
 local v=f()
 output.setNumber(1,v)
end",
        "=steps",
    )?;
    vm.set_breakpoints(vec![("=steps".into(), 6)])?;
    vm.tick(&CompositeSignal::default())?;
    vm.set_breakpoints(Vec::new())?;
    assert_eq!(vm.resume(StepMode::Over)?, RunOutcome::Suspended);
    assert_eq!(vm.stack()?[0].line, 7);
    vm.resume(StepMode::Continue)?;
    vm.set_breakpoints(vec![("=steps".into(), 2)])?;
    vm.tick(&CompositeSignal::default())?;
    vm.set_breakpoints(Vec::new())?;
    assert_eq!(vm.resume(StepMode::Out)?, RunOutcome::Suspended);
    assert_eq!(vm.stack()?[0].line, 7);
    Ok(())
}
#[test]
fn draw_resume_does_not_duplicate_the_prefix() -> Result<(), Box<dyn Error>> {
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(
        b"function onDraw()
 screen.setColor(255,0,0,128)
 screen.drawRectF(0,0,2,2)
 screen.drawRectF(0,0,2,2)
end",
        "=draw",
    )?;
    vm.set_breakpoints(vec![("=draw".into(), 4)])?;
    vm.draw(32, 32)?;
    assert_eq!(vm.commands().len(), 2);
    vm.set_breakpoints(Vec::new())?;
    vm.resume(StepMode::Continue)?;
    assert_eq!(vm.commands().len(), 3);
    Ok(())
}
#[test]
fn debug_enabled_does_not_disable_instruction_limits() -> Result<(), Box<dyn Error>> {
    let mut config = MicrocontrollerConfig::default();
    config.limits.instruction_budget = Some(std::num::NonZeroU64::new(3000).ok_or("budget")?);
    let mut vm = Microcontroller::new(config)?;
    vm.load(
        b"function onTick()
 local n=0
 while true do n=n+1 end
end",
        "=budget-debug",
    )?;
    vm.set_breakpoints(vec![("=budget-debug".into(), 3)])?;
    assert_eq!(vm.tick(&CompositeSignal::default())?, RunOutcome::Suspended);
    vm.set_breakpoints(Vec::new())?;
    assert_eq!(
        vm.resume(StepMode::Continue)
            .err()
            .ok_or("budget missing")?
            .kind,
        ErrorKind::Limit
    );
    Ok(())
}
#[test]
fn handles_are_scoped_to_vm_and_reset() -> Result<(), Box<dyn Error>> {
    let mut a = paused()?;
    let mut b = paused()?;
    let handle = match a.evaluate_watch(0, "t")? {
        DebugValue::Table(h) => h,
        _ => return Err("table missing".into()),
    };
    assert!(b.expand_table(handle, 0, 1).is_err());
    a.reset()?;
    assert!(a.expand_table(handle, 0, 1).is_err());
    assert!(b
        .expand_table(
            DebugHandle {
                vm_id: 0,
                pause_epoch: 0,
                slot: 0
            },
            0,
            1
        )
        .is_err());
    Ok(())
}
