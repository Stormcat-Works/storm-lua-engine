//! ランタイムのセマンティクスと敵対的スクリプトに対する制限。ゲームアセットは不要です。
#[path = "support/assertions.rs"]
mod assertion_support;
use std::error::Error;
use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_spec::{
    io::CompositeSignal,
    property::{PropertyBag, PropertyValue},
};
use storm_lua_vm::{
    runner::{ErrorKind, RunOutcome},
    ExecutionLimits,
};

fn loaded(source: &str) -> Result<Microcontroller, Box<dyn Error>> {
    let mut vm = Microcontroller::new(MicrocontrollerConfig::default())?;
    vm.load(source.as_bytes(), "=test")?;
    Ok(vm)
}
#[test]
fn top_level_properties_and_signal_boundaries_are_distinct() -> Result<(), Box<dyn Error>> {
    let mut properties = PropertyBag::default();
    properties.insert(b"Gain".to_vec(), PropertyValue::Number(16777217.0));
    let mut vm = Microcontroller::new(MicrocontrollerConfig {
        properties,
        ..Default::default()
    })?;
    vm.load(b"local p=property.getNumber('Gain');function onTick()output.setNumber(1,p-16777216.0);output.setNumber(2,input.getNumber(1)-16777216.0);output.setNumber(3,16777217.0)end","=precision")?;
    let mut input = CompositeSignal::default();
    input.numbers[0] = 16777217_f32;
    vm.tick(&input)?;
    assert_eq!(&vm.output().numbers[..3], &[1.0, 0.0, 16777216.0]);
    Ok(())
}
#[test]
fn output_retains_values_and_reset_restarts_lua() -> Result<(), Box<dyn Error>> {
    let mut vm=loaded("local n=0;function onTick()n=n+1;if n==1 then output.setNumber(1,42)end;output.setNumber(2,n)end")?;
    for _ in 0..3 {
        vm.tick(&CompositeSignal::default())?;
    }
    assert_eq!(&vm.output().numbers[..2], &[42.0, 3.0]);
    vm.reset()?;
    vm.tick(&CompositeSignal::default())?;
    assert_eq!(&vm.output().numbers[..2], &[42.0, 1.0]);
    Ok(())
}
#[test]
fn property_updates_do_not_rewrite_lua_locals_and_bytes_survive() -> Result<(), Box<dyn Error>> {
    let mut vm = Microcontroller::new(MicrocontrollerConfig::default())?;
    let mut p = PropertyBag::default();
    p.insert(b"x".to_vec(), PropertyValue::Text(vec![0, 255]));
    p.insert(b"n".to_vec(), PropertyValue::Number(2.0));
    vm.set_properties(p.clone())?;
    vm.load(b"local n=property.getNumber('n');debug.log(property.getText('x'));function onTick()output.setNumber(1,n);output.setNumber(2,property.getNumber('n'))end","=props")?;
    assert_eq!(vm.drain_logs(), vec![vec![0, 255]]);
    p.insert(b"n".to_vec(), PropertyValue::Number(3.0));
    vm.set_properties(p)?;
    vm.tick(&CompositeSignal::default())?;
    assert_eq!(&vm.output().numbers[..2], &[2.0, 3.0]);
    Ok(())
}
#[test]
fn drawing_calls_share_lua_state_but_not_composite_write_access() -> Result<(), Box<dyn Error>> {
    let mut vm=loaded("n=0;function onTick()output.setNumber(1,n)end;function onDraw()n=n+1;screen.drawRectF(n,0,screen.getWidth(),1)end")?;
    vm.draw(32, 32)?;
    let first = vm.commands().to_vec();
    vm.draw(64, 32)?;
    assert_ne!(vm.commands().as_ref(), first.as_slice());
    assert_eq!(vm.output().numbers[0], 0.0);
    vm.tick(&CompositeSignal::default())?;
    assert_eq!(vm.output().numbers[0], 2.0);
    Ok(())
}
#[test]
fn unsafe_libraries_and_string_dump_are_not_reachable() -> Result<(), Box<dyn Error>> {
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(&assertion_support::with_assertions(b"assert(os==nil and io==nil and debug.getinfo==nil and type(debug.log)=='function' and package==nil and require==nil and load==nil and _G==nil and coroutine==nil);assert(string.dump==nil and ('x').dump==nil)"),"=test")?;
    assert_eq!(vm.tick(&CompositeSignal::default())?, RunOutcome::Missing);
    Ok(())
}
#[test]
fn protected_calls_preserve_nil_results_and_errors() -> Result<(), Box<dyn Error>> {
    let mut vm = Microcontroller::new(MicrocontrollerConfig {
        environment: storm_lua_spec::environment::EnvironmentProfile::Extended,
        ..Default::default()
    })?;
    vm.load(b"local ok,a,b,c=pcall(function()return nil,2,nil end);assert(ok and a==nil and b==2 and c==nil);local yes,e=pcall(function()error('test')end);assert(not yes and type(e)=='string')","=protected")?;
    Ok(())
}
#[test]
fn instruction_limits_cannot_be_caught_forever() -> Result<(), Box<dyn Error>> {
    let loops = [
        "while true do end",
        "while true do pcall(function()while true do end end)end",
        "table.sort({1,2},function()while true do end end)",
        "table.sort({1,2},function()while true do pcall(function()while true do end end)end end)",
        "while true do xpcall(function()while true do end end,function()return 1 end)end",
    ];
    for source in loops {
        let mut config = MicrocontrollerConfig {
            environment: storm_lua_spec::environment::EnvironmentProfile::Extended,
            ..Default::default()
        };
        config.limits.instruction_budget = std::num::NonZeroU64::new(3000).ok_or("budget")?;
        let mut vm = Microcontroller::new(config)?;
        let error = vm
            .load(source.as_bytes(), "=budget")
            .err()
            .ok_or("loop was accepted")?;
        assert_eq!(error.kind, ErrorKind::Limit, "{source}: {error}");
        assert!(vm.is_failed());
    }
    Ok(())
}
#[test]
fn memory_budget_and_syntax_failure_are_explicit() -> Result<(), Box<dyn Error>> {
    let limits = ExecutionLimits {
        lua_memory_bytes: std::num::NonZeroUsize::new(100_000).ok_or("memory")?,
        ..Default::default()
    };
    let mut vm = Microcontroller::new(MicrocontrollerConfig {
        limits,
        ..Default::default()
    })?;
    assert_eq!(
        vm.load(b"local t={} while true do t[#t+1]={} end", "=memory")
            .err()
            .ok_or("memory limit missing")?
            .kind,
        ErrorKind::Limit
    );
    let mut syntax = Microcontroller::new(Default::default())?;
    assert!(syntax.load(b"local = ???", "=bad").is_err());
    Ok(())
}
#[test]
fn random_streams_are_independent() -> Result<(), Box<dyn Error>> {
    let source = "math.randomseed(9);function onTick()output.setNumber(1,math.random())end";
    let (mut a, mut b) = (loaded(source)?, loaded(source)?);
    a.tick(&CompositeSignal::default())?;
    let first = a.output().numbers[0];
    for _ in 0..10 {
        a.tick(&CompositeSignal::default())?;
    }
    b.tick(&CompositeSignal::default())?;
    assert_eq!(first, b.output().numbers[0]);
    Ok(())
}
#[test]
fn map_without_provider_is_an_error_not_an_ocean_fill() -> Result<(), Box<dyn Error>> {
    let mut vm = loaded("function onDraw()screen.drawMap(0,0,1)end")?;
    vm.draw(32, 32)?;
    let mut raster = storm_screen_raster::raster::ScreenRaster::new(32, 32)?;
    assert_eq!(
        vm.replay(&mut raster)
            .err()
            .ok_or("expected unsupported provider")?,
        storm_lua_spec::draw::ScreenError::MissingMapProvider
    );
    Ok(())
}
