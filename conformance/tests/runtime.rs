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
fn single_property_updates_leave_other_entries_and_compare_labels_as_bytes(
) -> Result<(), Box<dyn Error>> {
    let mut vm = Microcontroller::new(MicrocontrollerConfig::default())?;
    let mut initial = PropertyBag::default();
    initial.insert(b"keep".to_vec(), PropertyValue::Number(16777217.0));
    initial.insert(vec![0xff, 0x00], PropertyValue::Bool(true));
    vm.set_properties(initial)?;
    // 1件の設定は、load前にもtickの間にも受け付ける。f64は縮めない。
    vm.set_property(b"n", PropertyValue::Number(0.1))?;
    vm.load(b"local cached=property.getNumber('n');function onTick()output.setNumber(1,cached*10);output.setNumber(2,property.getNumber('n')*10);output.setNumber(3,property.getNumber('keep')-16777216);output.setBool(1,property.getBool(string.char(255,0)));output.setBool(2,property.getBool('N'))end","=single")?;
    vm.set_property(b"n", PropertyValue::Number(0.2))?;
    // 大文字小文字の違うラベルは別の項目。型の違う値でも、同じラベルなら置き換える。
    vm.set_property(b"N", PropertyValue::Bool(true))?;
    vm.set_property(&[0xff, 0x00], PropertyValue::Text(vec![0, 255]))?;
    vm.tick(&CompositeSignal::default())?;
    assert_eq!(&vm.output().numbers[..3], &[1.0, 2.0, 1.0]);
    assert_eq!(&vm.output().booleans[..2], &[false, true]);
    assert_eq!(vm.properties().len(), 4);
    assert_eq!(
        vm.properties().get(&[0xff, 0x00]),
        Some(&PropertyValue::Text(vec![0, 255]))
    );
    // 空のテキストの設定と削除は別の操作。ない項目の削除はfalseを返す。
    vm.set_property(b"n", PropertyValue::Text(Vec::new()))?;
    assert_eq!(
        vm.properties().get(b"n"),
        Some(&PropertyValue::Text(Vec::new()))
    );
    assert!(vm.remove_property(b"n")?);
    assert!(!vm.remove_property(b"n")?);
    assert!(vm.properties().get(b"n").is_none());
    assert_eq!(vm.properties().len(), 3);
    // resetは1件ずつ変えた現在の値を引き継ぐ。
    vm.reset()?;
    vm.tick(&CompositeSignal::default())?;
    assert_eq!(&vm.output().numbers[..3], &[0.0, 0.0, 1.0]);
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
        config.limits.instruction_budget = Some(std::num::NonZeroU64::new(3000).ok_or("budget")?);
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
fn time_limit_matches_the_game_and_does_not_fail_the_vm() -> Result<(), Box<dyn Error>> {
    // 実ゲームの時間制限は1001msで、超過は通常のLuaエラーとして報告され、VMは失敗状態になりません。
    let mut vm = loaded(
        "n=0\nfunction onTick()\n  n=n+1\n  if n==1 then while true do end end\n  output.setNumber(1,n)\nend",
    )?;
    let error = vm
        .tick(&CompositeSignal::default())
        .err()
        .ok_or("loop was accepted")?;
    assert_eq!(error.kind, ErrorKind::Lua, "{error}");
    assert!(
        error
            .message
            .contains("max execution time exceeded (1000ms). stopped at line 4"),
        "{}",
        error.message
    );
    assert!(!vm.is_failed());
    // 次のtickは通常どおり実行できます。
    vm.tick(&CompositeSignal::default())?;
    assert_eq!(vm.output().numbers[0], 2.0);
    Ok(())
}
#[test]
fn time_limit_cannot_be_caught_forever_by_pcall() -> Result<(), Box<dyn Error>> {
    let mut vm = Microcontroller::new(MicrocontrollerConfig {
        environment: storm_lua_spec::environment::EnvironmentProfile::Extended,
        ..Default::default()
    })?;
    vm.load(
        b"function onTick() while true do pcall(function() while true do end end) end end",
        "=timeout-pcall",
    )?;
    let error = vm
        .tick(&CompositeSignal::default())
        .err()
        .ok_or("loop was accepted")?;
    assert_eq!(error.kind, ErrorKind::Lua, "{error}");
    assert!(
        error.message.contains("max execution time exceeded (1000ms)"),
        "{}",
        error.message
    );
    assert!(!vm.is_failed());
    Ok(())
}
#[test]
fn custom_time_limit_is_host_configurable() -> Result<(), Box<dyn Error>> {
    let mut config = MicrocontrollerConfig::default();
    config.limits.time_limit = Some(std::time::Duration::from_millis(50));
    let mut vm = Microcontroller::new(config)?;
    vm.load(b"function onTick() while true do end end", "=timeout-custom")?;
    let error = vm
        .tick(&CompositeSignal::default())
        .err()
        .ok_or("loop was accepted")?;
    assert_eq!(error.kind, ErrorKind::Lua, "{error}");
    assert!(
        error
            .message
            .contains("max execution time exceeded (50ms). stopped at line 1"),
        "{}",
        error.message
    );
    Ok(())
}
#[test]
fn instruction_memory_and_source_limits_are_off_by_default() -> Result<(), Box<dyn Error>> {
    // 実ゲームにハード制限はないため、既定では旧既定値（100万命令・8MiB・1MiB）を超えても動作します。
    let mut vm = Microcontroller::new(MicrocontrollerConfig::default())?;
    let mut source = String::from("--");
    source.push_str(&"x".repeat(1100 * 1024));
    source.push_str("\nfunction onTick() local s=string.rep('y',9*1024*1024) output.setNumber(1,#s) end");
    vm.load(source.as_bytes(), "=unbounded")?;
    vm.tick(&CompositeSignal::default())?;
    assert_eq!(vm.output().numbers[0], 9.0 * 1024.0 * 1024.0);
    Ok(())
}
#[test]
fn source_limit_applies_only_when_the_host_enables_it() -> Result<(), Box<dyn Error>> {
    let mut config = MicrocontrollerConfig::default();
    config.limits.max_source_bytes = Some(std::num::NonZeroUsize::new(64).ok_or("nonzero")?);
    let mut vm = Microcontroller::new(config)?;
    let error = vm
        .load(
            b"function onTick() output.setNumber(1, 1) end -- padding padding padding",
            "=limited-source",
        )
        .err()
        .ok_or("oversized source was accepted")?;
    assert_eq!(error.kind, ErrorKind::Limit, "{error}");
    Ok(())
}
#[test]
fn memory_budget_and_syntax_failure_are_explicit() -> Result<(), Box<dyn Error>> {
    let limits = ExecutionLimits {
        lua_memory_bytes: Some(std::num::NonZeroUsize::new(100_000).ok_or("memory")?),
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
