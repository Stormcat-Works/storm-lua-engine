//! Named callbacks and optional controls use the standard Vehicle state and phases.
use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_spec::{
    environment::EnvironmentProfile, io::CompositeSignal, property::PropertyValue,
};
use storm_lua_vm::{
    runner::{ErrorKind, RunOutcome, StepMode},
    value::LuaValue,
};

fn config() -> MicrocontrollerConfig {
    MicrocontrollerConfig {
        environment: EnvironmentProfile::Extended,
        control_namespace: Some("harness".into()),
        ..Default::default()
    }
}
#[test]
fn controls_change_standard_properties_in_the_same_chunk_and_rebind_on_reset(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut vm = Microcontroller::new(config())?;
    vm.load(b"harness.setProperty('gain',16777217);harness.setProperty('raw',string.char(0,255));local gain=property.getNumber('gain');function read() output.setNumber(1,gain-16777216);debug.log(property.getText('raw')) end", "@controls.lua")?;
    for _ in 0..2 {
        assert_eq!(
            vm.call_tick("read", &[], &Default::default())?,
            RunOutcome::Completed
        );
        assert_eq!(vm.output().numbers[0], 1.0);
        assert_eq!(vm.drain_logs(), vec![vec![0, 255]]);
        assert_eq!(
            vm.properties().get(b"gain"),
            Some(&PropertyValue::Number(16777217.0))
        );
        vm.reset()?;
    }
    vm.load(b"harness.setProperty('gain',nil)", "@remove.lua")?;
    assert!(vm.properties().get(b"gain").is_none());
    Ok(())
}
#[test]
fn input_controls_quantize_at_io_and_named_callbacks_do_not_replace_on_tick(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut vm = Microcontroller::new(config())?;
    vm.load(b"function custom(n) harness.setInputNumber(1,n);harness.setInputBool(1,true);output.setNumber(1,input.getNumber(1));output.setBool(1,input.getBool(1)) end function onTick()output.setNumber(2,7)end function paint(n) output.setNumber(2,99);screen.drawRectF(0,0,n,1);debug.log(screen.getWidth(),screen.getHeight(),input.getNumber(1))end", "@phase.lua")?;
    let input = CompositeSignal::default();
    vm.call_tick("custom", &[LuaValue::Number(16777217.0)], &input)?;
    assert_eq!(vm.input().numbers[0], 16777216.0);
    assert_eq!(vm.output().numbers[0], 16777216.0);
    assert!(vm.output().booleans[0]);
    vm.tick(&input)?;
    assert_eq!(vm.output().numbers[1], 7.0);
    vm.call_draw("paint", &[LuaValue::Integer(2)], 64, 32)?;
    assert!(!vm.commands().is_empty());
    assert_eq!(vm.output().numbers[1], 7.0);
    assert_eq!(vm.drain_logs(), vec![b"64\t32\t0.0".to_vec()]);
    assert_eq!(vm.call_tick("missing", &[], &input)?, RunOutcome::Missing);
    Ok(())
}
#[test]
fn game_and_conflicting_control_namespaces_are_rejected_without_replacing_builtins() {
    assert!(Microcontroller::new(MicrocontrollerConfig {
        control_namespace: Some("harness".into()),
        ..Default::default()
    })
    .is_err());
    for name in [
        "input", "screen", "property", "print", "_ENV", "_G", "a.b", "", "bad name",
    ] {
        let mut c = config();
        c.control_namespace = Some(name.into());
        assert!(Microcontroller::new(c).is_err(), "{name}");
    }
    let mut c = config();
    c.bindings
        .values
        .insert("harness.setProperty".into(), LuaValue::Nil);
    assert!(Microcontroller::new(c).is_err());
    let mut c = config();
    c.bindings.values.insert("harness".into(), LuaValue::Nil);
    assert!(Microcontroller::new(c).is_err());
}
#[test]
fn named_draw_suspension_preserves_single_invocation_and_real_source_line(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(b"count=0\nfunction preview()\n count=count+1\n screen.drawRectF(0,0,1,1)\n screen.drawRectF(2,0,1,1)\nend","@preview.lua")?;
    vm.set_breakpoints(vec![("@preview.lua".into(), 5)])?;
    assert_eq!(vm.call_draw("preview", &[], 32, 32)?, RunOutcome::Suspended);
    assert_eq!(vm.stack()?[0].line, 5);
    assert_eq!(vm.commands().len(), 1);
    vm.set_breakpoints(vec![])?;
    assert_eq!(vm.resume(StepMode::Continue)?, RunOutcome::Completed);
    assert_eq!(vm.commands().len(), 2);
    Ok(())
}
#[test]
fn controls_have_explicit_argument_and_rust_owned_memory_budgets(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut vm = Microcontroller::new(config())?;
    vm.load(b"assert(not pcall(harness.setInputNumber,0,1));assert(not pcall(harness.setInputNumber,1.5,1));assert(not pcall(harness.setInputBool,1,0));assert(not pcall(harness.setProperty,'bad',{}));harness.setProperty('good',true)","@invalid.lua")?;
    assert_eq!(
        vm.properties().get(b"good"),
        Some(&PropertyValue::Bool(true))
    );
    let result = vm.load(
        b"for i=1,4097 do harness.setProperty(tostring(i),true)end",
        "@bounded.lua",
    );
    assert!(matches!(result,Err(error) if error.kind==ErrorKind::Limit));
    assert_eq!(vm.properties().len(), 4096);
    Ok(())
}

#[test]
fn runtime_analysis_keeps_syntax_and_environment_checks_without_static_require_rules() {
    use std::collections::BTreeMap;
    use storm_lua_analysis::{analyze, AnalyzeMode, AnalyzeOptions, LuaProject};
    let project = LuaProject {
        entry: "main".into(),
        modules: BTreeMap::from([
            (
                "main".into(),
                "function onTick()require(property.getText('file'))end".into(),
            ),
            ("lib".into(), "function helper()print('ok')end".into()),
        ]),
        ambient: Default::default(),
    };
    let options = AnalyzeOptions {
        mode: AnalyzeMode::Runtime,
        environment: EnvironmentProfile::Extended,
        host_bindings: vec!["require".into()],
        ..Default::default()
    };
    let run = analyze(&project, &options);
    assert!(run.ok);
    assert!(!run
        .diagnostics
        .iter()
        .any(|d| d.severity == storm_lua_analysis::Severity::Error));
    assert!(analyze(&project, &Default::default())
        .diagnostics
        .iter()
        .any(|d| d.code == "require-not-top-level"));
    let mut invalid = project.clone();
    invalid.modules.insert("main".into(), "local =".into());
    assert!(analyze(&invalid, &options)
        .diagnostics
        .iter()
        .any(|d| d.code == "syntax-error"));
    let game = analyze(
        &project,
        &AnalyzeOptions {
            mode: AnalyzeMode::Runtime,
            ..Default::default()
        },
    );
    assert!(game
        .diagnostics
        .iter()
        .any(|d| d.code == "sw-unavailable-global"));
}
