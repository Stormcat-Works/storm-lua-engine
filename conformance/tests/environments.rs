//! Environment, external-name and reflection regressions use real compiler/runtime implementations.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::{cell::Cell, collections::BTreeMap, rc::Rc};
use storm_lua_analysis::project::LuaProject;
use storm_lua_analysis::{analyze, AnalyzeOptions};
use storm_lua_build::{build, minify, ApiCompileOptions, ApiProjectCompileOptions};
use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_spec::{
    environment::EnvironmentProfile,
    io::CompositeSignal,
    property::{PropertyBag, PropertyValue},
};
use storm_lua_vm::{
    bindings::HostBindings,
    runner::{ErrorKind, Vm},
    value::LuaValue,
    ExecutionLimits,
};

fn project(source: &str) -> LuaProject {
    LuaProject {
        entry: "main".into(),
        modules: BTreeMap::from([("main".into(), source.into())]),
        ambient: Default::default(),
    }
}
fn configured(profile: EnvironmentProfile) -> MicrocontrollerConfig {
    MicrocontrollerConfig {
        environment: profile,
        ..Default::default()
    }
}
#[test]
fn game_globals_are_absent_and_debug_has_exactly_log() {
    let mut vm = Microcontroller::new(Default::default()).unwrap();
    vm.load(
        br#"function onTick()
      local count=0 local valid=true
      for k,v in pairs(debug) do count=count+1 valid=valid and k=='log' and type(v)=='function' end
      output.setBool(1,count==1 and valid)
      output.setBool(2,pcall==nil and xpcall==nil and error==nil and assert==nil and print==nil)
      debug.log('game log')
    end"#,
        "=game",
    )
    .unwrap();
    vm.tick(&Default::default()).unwrap();
    assert!(vm.output().booleans[0] && vm.output().booleans[1]);
    assert_eq!(vm.drain_logs(), vec![b"game log".to_vec()]);
    assert_eq!(
        vm.enable_dev_logs().unwrap_err().kind,
        ErrorKind::InvalidArgument
    );
    let mut addon = storm_lua_addon::Addon::new(Default::default()).unwrap();
    addon.load(b"g_savedata={absent=pcall==nil and error==nil and print==nil, members=0};for k in pairs(debug)do g_savedata.members=g_savedata.members+1 end;debug.log('addon log')", "=addon").unwrap();
    addon.start().unwrap();
    let LuaValue::Table(entries) = addon.savedata().unwrap() else {
        panic!("expected table")
    };
    assert!(entries.contains(&(LuaValue::Bytes(b"absent".to_vec()), LuaValue::Bool(true))));
    assert!(entries.contains(&(LuaValue::Bytes(b"members".to_vec()), LuaValue::Integer(1))));
}
#[test]
fn builtin_rejections_match_single_source_projects_and_editing_diagnostics() {
    for name in ["pcall", "xpcall", "error", "assert", "print", "unpack"] {
        let source = format!("function onTick(){name}()end");
        let lint = analyze(&project(&source), &AnalyzeOptions::default());
        assert!(
            lint.diagnostics
                .iter()
                .any(|d| d.code == "sw-unavailable-global"
                    && d.severity == storm_lua_analysis::Severity::Warning),
            "{name}"
        );
        for target in [None, Some(8192), Some(1)] {
            let options = ApiCompileOptions {
                target_size: target,
                ..Default::default()
            };
            let result = minify(&source, &options);
            assert!(!result.ok && result.code.is_none(), "{name}: {result:?}");
            assert!(result
                .diagnostics
                .iter()
                .any(|d| d.code == "sw-unavailable-global"));
            for compress in [false, true] {
                let result = build(
                    &project(&source),
                    &ApiProjectCompileOptions {
                        metadata_tag: None,
                        compile: options.clone(),
                        minify: Some(compress),
                    },
                );
                assert!(!result.ok && result.code.is_none(), "{name}: {result:?}");
            }
        }
    }
}
#[test]
fn nil_probes_and_local_shadowing_are_not_forbidden_names() {
    for source in [
        "function onTick()output.setBool(1,pcall==nil and type(error)=='nil' and debug.getinfo==nil)end",
        "local function pcall(x)return x end function onTick()output.setNumber(1,pcall(7))end",
        "function onTick()debug.log('works')end",
    ] {
        let result=minify(source,&Default::default());assert!(result.ok,"{result:?}");
        let mut a=Microcontroller::new(Default::default()).unwrap();let mut b=Microcontroller::new(Default::default()).unwrap();
        a.load(source.as_bytes(),"=a").unwrap();b.load(result.code.as_ref().unwrap().as_bytes(),"=b").unwrap();
        a.tick(&Default::default()).unwrap();b.tick(&Default::default()).unwrap();
        assert_eq!(a.output(),b.output());assert_eq!(a.drain_logs(),b.drain_logs());
    }
}
#[test]
fn extended_protected_calls_keep_names_and_behavior_under_every_search_route() {
    let source="function onTick() local ok,v=pcall(function()return 7 end);assert(ok);output.setNumber(1,v);print(v) end";
    for target in [None, Some(1), Some(8192)] {
        let options = ApiCompileOptions {
            environment: EnvironmentProfile::Extended,
            target_size: target,
            ..Default::default()
        };
        let result = minify(source, &options);
        assert!(result.ok, "{result:?}");
        assert_eq!(result.search.as_ref().unwrap().mode, "lexical");
        assert!(result
            .diagnostics
            .iter()
            .any(|d| d.code == "conservative-minification"));
        assert!(result.code.as_ref().unwrap().contains("pcall"));
        let mut vm = Microcontroller::new(configured(EnvironmentProfile::Extended)).unwrap();
        vm.load(result.code.as_ref().unwrap().as_bytes(), "=extended")
            .unwrap();
        vm.tick(&Default::default()).unwrap();
        assert_eq!(vm.output().numbers[0], 7.0);
        assert_eq!(vm.drain_logs(), vec![b"7".to_vec()]);
        let options = options.to_core().unwrap();
        let (context, jobs) = storm_lua_minify::search::prepare_search(source, &options).unwrap();
        let batches = jobs
            .iter()
            .map(|job| {
                storm_lua_minify::search::decode_and_evaluate_candidate_job(
                    &storm_lua_minify::search::encode_candidate_job(job).unwrap(),
                )
                .unwrap()
            })
            .collect();
        let selected = storm_lua_minify::search::select_best(context, batches).unwrap();
        assert_eq!(result.code.as_ref().unwrap(), &selected.code);
    }
}
#[test]
fn dynamic_environment_values_and_functions_preserve_state_draws_and_literals() {
    let sources = [
        r#"caption='HELLO' other='WORLD' function onDraw() local name=property.getText('name') screen.drawText(0,0,_ENV[name]) end"#,
        r#"function named()return 7 end function onTick()local name=property.getText('name')output.setNumber(1,_ENV[name]())end"#,
        r#"counter=0 local env=_ENV function onTick()local key='counter' env[key]=env[key]+1 output.setNumber(1,counter)end"#,
        r#"local e=_ENV local _ENV={out=output, n=4} function e.onTick()out.setNumber(1,n)end"#,
        r#"local e=_ENV function onTick()output.setBool(1,math.type(9007199254740993)=='integer');debug.log(1.0, [=[hello]=], e==_ENV)end"#,
    ];
    for source in sources {
        let options = ApiCompileOptions {
            target_size: Some(8192),
            zero_cost_newlines: Some(false),
            ..Default::default()
        };
        let result = minify(source, &options);
        assert!(result.ok, "{result:?}");
        assert_eq!(result.search.as_ref().unwrap().mode, "lexical");
        let output = result.code.unwrap();
        assert!(output.len() <= source.len());
        let mut properties = PropertyBag::default();
        properties.insert(
            b"name".to_vec(),
            PropertyValue::Text(if source.contains("function named") {
                b"named".to_vec()
            } else {
                b"caption".to_vec()
            }),
        );
        let config = MicrocontrollerConfig {
            properties,
            ..Default::default()
        };
        let mut a = Microcontroller::new(config.clone()).unwrap();
        let mut b = Microcontroller::new(config).unwrap();
        a.load(source.as_bytes(), "=original").unwrap();
        b.load(output.as_bytes(), "=compact").unwrap();
        for _ in 0..4 {
            a.tick(&Default::default()).unwrap();
            b.tick(&Default::default()).unwrap();
            assert_eq!(a.output(), b.output());
            for (w, h) in [(32, 32), (64, 32)] {
                a.draw(w, h).unwrap();
                b.draw(w, h).unwrap();
                assert_eq!(&*a.commands(), &*b.commands());
            }
            assert_eq!(a.drain_logs(), b.drain_logs());
        }
    }
}
#[test]
fn unassigned_external_globals_are_not_renamed_or_captured_by_short_names() {
    let source =
        "function onTick()local quiteLongLocal=3;capture(externalValue+quiteLongLocal+a)end";
    let result = minify(source, &Default::default());
    assert!(result.ok, "{result:?}");
    let observed = Rc::new(Cell::new(0.0));
    let clone = Rc::clone(&observed);
    let mut vm = Vm::new(ExecutionLimits::default()).unwrap();
    vm.configure(|lua, env| {
        env.set(
            "capture",
            lua.create_function(move |_, v: f64| {
                clone.set(v);
                Ok(())
            })?,
        )?;
        env.set("externalValue", 7)?;
        env.set("a", 2)
    })
    .unwrap();
    vm.execute(result.code.unwrap().as_bytes(), "=external")
        .unwrap();
    vm.call("onTick").unwrap();
    assert_eq!(observed.get(), 12.0);
}
#[test]
fn host_overrides_are_explicit_preserved_on_reset_and_respected_by_compilation() {
    let bindings = HostBindings {
        values: BTreeMap::from([
            ("host.value".into(), LuaValue::Integer(11)),
            ("pcall".into(), LuaValue::Nil),
        ]),
        functions: BTreeMap::from([(
            "math.abs".into(),
            Rc::new(|_: &[LuaValue]| Ok(vec![LuaValue::Integer(99)]))
                as storm_lua_vm::value::HostFunction,
        )]),
    };
    assert!(Microcontroller::new(MicrocontrollerConfig {
        bindings: bindings.clone(),
        ..Default::default()
    })
    .is_err());
    let source="function onTick()output.setNumber(1,math.abs(-3)+host.value);output.setBool(1,pcall==nil)end";
    let result = minify(
        source,
        &ApiCompileOptions {
            environment: EnvironmentProfile::Extended,
            host_bindings: vec!["math.abs".into(), "host.value".into(), "pcall".into()],
            ..Default::default()
        },
    );
    assert!(result.ok);
    let mut vm = Microcontroller::new(MicrocontrollerConfig {
        environment: EnvironmentProfile::Extended,
        bindings,
        ..Default::default()
    })
    .unwrap();
    vm.load(result.code.unwrap().as_bytes(), "=override")
        .unwrap();
    for _ in 0..2 {
        vm.tick(&CompositeSignal::default()).unwrap();
        assert_eq!(vm.output().numbers[0], 110.0);
        assert!(vm.output().booleans[0]);
        vm.reset().unwrap();
    }
}
#[test]
fn debugger_does_not_require_script_visible_standard_debug_or_protected_calls() {
    let mut vm = Microcontroller::new(Default::default()).unwrap();
    vm.set_breakpoints(vec![("=game-debug".into(), 3)]).unwrap();
    vm.load(
        b"local n=0\nfunction onTick()\n n=n+1\n output.setNumber(1,n)\nend",
        "=game-debug",
    )
    .unwrap();
    assert_eq!(
        vm.tick(&Default::default()).unwrap(),
        storm_lua_vm::runner::RunOutcome::Suspended
    );
    assert!(!vm.stack().unwrap().is_empty());
    assert_eq!(
        vm.evaluate_watch(
            0,
            "pcall==nil and debug.getinfo==nil and type(debug.log)=='function'"
        )
        .unwrap(),
        storm_lua_vm::debug::DebugValue::Bool(true)
    );
    vm.resume(storm_lua_vm::runner::StepMode::Continue).unwrap();
    assert_eq!(vm.output().numbers[0], 1.0);
}

#[test]
fn protected_error_messages_keep_source_lines_after_lexical_minification() {
    let source = "-- heading\n\nfunction onTick()\n  local ok, message = pcall(function()\n    error('observed error')\n  end)\n  debug.log(message)\nend\n";
    let result = minify(
        source,
        &ApiCompileOptions {
            environment: EnvironmentProfile::Extended,
            ..Default::default()
        },
    );
    assert!(result.ok, "{result:?}");
    let mut a = Microcontroller::new(configured(EnvironmentProfile::Extended)).unwrap();
    let mut b = Microcontroller::new(configured(EnvironmentProfile::Extended)).unwrap();
    a.load(source.as_bytes(), "=same-name").unwrap();
    b.load(result.code.unwrap().as_bytes(), "=same-name")
        .unwrap();
    a.tick(&Default::default()).unwrap();
    b.tick(&Default::default()).unwrap();
    let logs = a.drain_logs();
    assert!(String::from_utf8_lossy(&logs[0]).contains(":5:"));
    assert_eq!(logs, b.drain_logs());
}
#[test]
fn http_callback_is_kept_as_an_external_entry_point() {
    let source="reply=0 function httpReply(port,request,response)reply=tonumber(response)end function onTick()output.setNumber(1,reply)async.httpGet(8080,'/test')end";
    let result = minify(source, &Default::default());
    assert!(result.ok, "{result:?}");
    assert!(result.code.as_ref().unwrap().contains("httpReply"));
    let mut vm = Microcontroller::new(Default::default()).unwrap();
    vm.load(result.code.unwrap().as_bytes(), "=http").unwrap();
    vm.tick(&Default::default()).unwrap();
    let requests = vm.drain_http_requests();
    assert_eq!(requests.len(), 1);
    vm.http_reply(requests[0].token, b"7").unwrap();
    vm.tick(&Default::default()).unwrap();
    assert_eq!(vm.output().numbers[0], 7.0);
}
