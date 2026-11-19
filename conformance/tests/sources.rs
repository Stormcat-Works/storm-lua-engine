//! Actual Lua execution for host-supplied modules and multi-chunk replay.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use storm_lua_addon::{Addon, AddonConfig};
use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_spec::{environment::EnvironmentProfile, io::CompositeSignal};
use storm_lua_vm::{
    runner::{ErrorKind, RunOutcome, StepMode, VmError},
    source::{RequireLoader, SourceChunk},
    value::LuaValue,
};

fn loader(entries: &[(&str, &str, &str)], calls: Arc<Mutex<Vec<String>>>) -> RequireLoader {
    let sources: BTreeMap<_, _> = entries
        .iter()
        .map(|(key, name, source)| {
            (
                key.to_string(),
                SourceChunk {
                    source: source.as_bytes().to_vec(),
                    name: name.to_string(),
                },
            )
        })
        .collect();
    RequireLoader::new(move |name| {
        calls
            .lock()
            .map_err(|_| VmError::new(ErrorKind::Host, "load log is poisoned"))?
            .push(name.to_owned());
        sources
            .get(name)
            .cloned()
            .ok_or_else(|| VmError::new(ErrorKind::Host, format!("module not found: {name}")))
    })
}
fn configured(loader: RequireLoader) -> MicrocontrollerConfig {
    MicrocontrollerConfig {
        environment: EnvironmentProfile::Extended,
        require_loader: Some(loader),
        ..Default::default()
    }
}
#[test]
fn include_once_shares_globals_but_not_locals_and_discards_returns(
) -> Result<(), Box<dyn std::error::Error>> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sources=loader(&[("lib", "@lib/init.lua", "loads=(loads or 0)+1 local private=4 function helper()return shared+private end return 7,nil,9")],Arc::clone(&calls));
    let mut vm = Microcontroller::new(configured(sources))?;
    vm.load(b"shared=10; local r=require('lib'); require('lib'); function onTick()output.setNumber(1,helper());output.setNumber(2,loads);output.setBool(1,r==nil and private==nil);output.setNumber(3,select('#',require('lib')))end", "@main.lua")?;
    vm.tick(&CompositeSignal::default())?;
    assert_eq!(&vm.output().numbers[..3], &[14.0, 1.0, 0.0]);
    assert!(vm.output().booleans[0]);
    assert_eq!(&*calls.lock().unwrap(), &["lib"]);
    vm.reset()?;
    vm.tick(&Default::default())?;
    assert_eq!(vm.output().numbers[0], 14.0);
    assert_eq!(&*calls.lock().unwrap(), &["lib", "lib"]);
    Ok(())
}
#[test]
fn nested_cycles_mark_before_execution_and_cache_logical_names(
) -> Result<(), Box<dyn std::error::Error>> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sources = loader(
        &[
            (
                "a",
                "@a.lua",
                "trace=trace..'a';require('b');trace=trace..'A'",
            ),
            (
                "b",
                "@b.lua",
                "trace=trace..'b';require('a');trace=trace..'B'",
            ),
            ("alias", "@a.lua", "trace=trace..'x'"),
        ],
        Arc::clone(&calls),
    );
    let mut vm = Microcontroller::new(configured(sources))?;
    vm.load(
        b"trace='';require('a');require('alias');require('b');debug.log(trace)",
        "@main.lua",
    )?;
    assert_eq!(vm.drain_logs(), vec![b"abBAx".to_vec()]);
    assert_eq!(&*calls.lock().unwrap(), &["a", "b", "alias"]);
    Ok(())
}
#[test]
fn required_chunk_breakpoint_resumes_inside_same_coroutine(
) -> Result<(), Box<dyn std::error::Error>> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut vm = Microcontroller::new(configured(loader(
        &[(
            "lib",
            "@lib/helper.lua",
            "local captured=4\nshared=captured+3\nfunction helper()return shared end",
        )],
        Arc::clone(&calls),
    )))?;
    vm.set_breakpoints(vec![("@lib/helper.lua".into(), 2)])?;
    assert_eq!(
        vm.load(
            b"require('lib')\nfunction onTick()output.setNumber(1,helper())end",
            "@main.lua"
        )?,
        RunOutcome::Suspended
    );
    let stack = vm.stack()?;
    assert_eq!(stack[0].source, "@lib/helper.lua");
    assert_eq!(stack[0].line, 2);
    assert!(stack.iter().any(|f| f.source == "@main.lua"));
    assert_eq!(
        vm.load(b"x=1", "@extra.lua").expect_err("busy").kind,
        ErrorKind::Busy
    );
    vm.set_breakpoints(vec![])?;
    assert_eq!(vm.resume(StepMode::Into)?, RunOutcome::Suspended);
    assert_eq!(vm.resume(StepMode::Continue)?, RunOutcome::Completed);
    vm.tick(&Default::default())?;
    assert_eq!(vm.output().numbers[0], 7.0);
    assert_eq!(&*calls.lock().unwrap(), &["lib"]);
    vm.reset()?;
    vm.tick(&Default::default())?;
    assert_eq!(vm.output().numbers[0], 7.0);
    assert_eq!(&*calls.lock().unwrap(), &["lib", "lib"]);
    Ok(())
}
#[test]
fn source_failures_are_explicit_and_only_started_chunks_remain_cached(
) -> Result<(), Box<dyn std::error::Error>> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sources = loader(
        &[
            (
                "runtime",
                "@runtime.lua",
                "runs=(runs or 0)+1;error('failure')",
            ),
            ("syntax", "@syntax.lua", "local ="),
        ],
        Arc::clone(&calls),
    );
    let mut vm = Microcontroller::new(configured(sources))?;
    vm.load(b"local a=pcall(require,'missing');local b=pcall(require,'missing');local c=pcall(require,'syntax');local d=pcall(require,'syntax');local e,msg=pcall(require,'runtime');local f=pcall(require,'runtime');assert(not a and not b and not c and not d and not e and f);assert(runs==1);assert(msg:find('runtime.lua'));debug.log(runs)","@main.lua")?;
    assert_eq!(
        &*calls.lock().unwrap(),
        &["missing", "missing", "syntax", "syntax", "runtime"]
    );
    assert_eq!(vm.drain_logs(), vec![b"1".to_vec()]);
    Ok(())
}
#[test]
fn loader_is_opt_in_and_cannot_conflict_with_host_bindings(
) -> Result<(), Box<dyn std::error::Error>> {
    let l = loader(&[], Arc::default());
    assert!(Microcontroller::new(MicrocontrollerConfig {
        require_loader: Some(l.clone()),
        ..Default::default()
    })
    .is_err());
    let mut c = configured(l.clone());
    c.bindings.values.insert("require".into(), LuaValue::Nil);
    assert!(Microcontroller::new(c).is_err());
    let mut a = AddonConfig {
        environment: EnvironmentProfile::Extended,
        require_loader: Some(l),
        ..Default::default()
    };
    a.bindings
        .functions
        .insert("require.foo".into(), Arc::new(|_| Ok(vec![])));
    assert!(Addon::new(a).is_err());
    let mut plain = Microcontroller::new(MicrocontrollerConfig {
        environment: EnvironmentProfile::Extended,
        ..Default::default()
    })?;
    plain.load(
        b"assert(require==nil and load==nil and loadfile==nil and package==nil)",
        "@plain.lua",
    )?;
    Ok(())
}
#[test]
fn nested_require_never_resets_instruction_budget_or_escapes_memory_limits(
) -> Result<(), Box<dyn std::error::Error>> {
    let l = RequireLoader::new(|name| {
        Ok(SourceChunk {
            name: format!("@{name}.lua"),
            source: b"local i=0;while i<20000 do i=i+1 end;require('next')".to_vec(),
        })
    });
    let mut c = configured(l);
    c.limits.instruction_budget = Some(std::num::NonZeroU64::MIN.saturating_add(1999));
    let mut vm = Microcontroller::new(c)?;
    assert_eq!(
        vm.load(b"while true do pcall(require,'loop')end", "@main.lua")
            .expect_err("bounded")
            .kind,
        ErrorKind::Limit
    );
    assert!(vm.is_failed());
    let huge = RequireLoader::new(|_| {
        Ok(SourceChunk {
            name: "@huge.lua".into(),
            source: vec![b' '; 1024 * 1024 + 1],
        })
    });
    let mut vm = Microcontroller::new(configured(huge))?;
    assert_eq!(
        vm.load(b"require('huge')", "@main.lua")
            .expect_err("size")
            .kind,
        ErrorKind::Limit
    );
    let allocation = loader(
        &[(
            "alloc",
            "@alloc.lua",
            "local t={}for i=1,10000 do t[i]=string.rep('x',1024)end",
        )],
        Arc::default(),
    );
    let mut c = configured(allocation);
    c.limits.lua_memory_bytes = Some(std::num::NonZeroUsize::MIN.saturating_add(256 * 1024 - 1));
    let mut vm = Microcontroller::new(c)?;
    assert_eq!(
        vm.load(b"require('alloc')", "@main.lua")
            .expect_err("heap")
            .kind,
        ErrorKind::Limit
    );
    Ok(())
}
#[test]
fn invalid_module_names_and_bytecode_do_not_turn_into_empty_sources(
) -> Result<(), Box<dyn std::error::Error>> {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let l = RequireLoader::new(move |_| {
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(SourceChunk {
            name: "@binary.lua".into(),
            source: b"\x1bLua\0\0".to_vec(),
        })
    });
    let mut vm = Microcontroller::new(configured(l))?;
    vm.load(b"for _,v in ipairs({true,1,{},'',string.char(0)})do assert(not pcall(require,v))end;assert(not pcall(require,'binary'))","@main.lua")?;
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    Ok(())
}
#[test]
fn loader_cache_is_per_vm_not_per_source_provider() -> Result<(), Box<dyn std::error::Error>> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let c = configured(loader(&[("a", "@a.lua", "value=7")], Arc::clone(&calls)));
    for _ in 0..2 {
        let mut vm = Microcontroller::new(c.clone())?;
        vm.load(b"require('a');require('a')", "@main.lua")?;
    }
    assert_eq!(&*calls.lock().unwrap(), &["a", "a"]);
    Ok(())
}
#[test]
fn vehicle_replays_prefix_entry_suffix_with_independent_locals(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(
        b"shared=10;local private=4;function helper()return shared+private end",
        "@prefix.lua",
    )?;
    vm.load(b"local private=3;function onTick()output.setNumber(1,helper()+private)end;function onDraw()screen.drawText(0,0,'hello')end","@main.lua")?;
    vm.load(b"shared=20", "@suffix.lua")?;
    for _ in 0..3 {
        vm.tick(&Default::default())?;
        assert_eq!(vm.output().numbers[0], 27.0);
        assert_eq!(vm.draw(32, 32)?, RunOutcome::Completed);
        assert!(!vm.commands().is_empty());
        vm.reset()?;
    }
    Ok(())
}
#[test]
fn failed_or_abandoned_load_does_not_enter_reset_program() -> Result<(), Box<dyn std::error::Error>>
{
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(
        b"v=7;function onTick()output.setNumber(1,v)end",
        "@good.lua",
    )?;
    assert!(vm.load(b"local =", "@syntax.lua").is_err());
    vm.load(b"v=8;missing()", "@runtime.lua")
        .expect_err("runtime failure");
    vm.reset()?;
    vm.tick(&Default::default())?;
    assert_eq!(vm.output().numbers[0], 7.0);
    vm.set_breakpoints(vec![("@pending.lua".into(), 2)])?;
    assert_eq!(
        vm.load(b"v=99\nv=100", "@pending.lua")?,
        RunOutcome::Suspended
    );
    vm.reset()?;
    vm.tick(&Default::default())?;
    assert_eq!(vm.output().numbers[0], 7.0);
    Ok(())
}
#[test]
fn completed_suspended_load_is_replayed_and_failed_replay_keeps_old_vm(
) -> Result<(), Box<dyn std::error::Error>> {
    let fail = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&fail);
    let mut c = MicrocontrollerConfig {
        environment: EnvironmentProfile::Extended,
        ..Default::default()
    };
    c.bindings.functions.insert(
        "host.check".into(),
        Arc::new(move |_| {
            if flag.load(Ordering::Relaxed) {
                Err(VmError::new(ErrorKind::Host, "replay failed"))
            } else {
                Ok(vec![])
            }
        }),
    );
    let mut vm = Microcontroller::new(c)?;
    vm.load(
        b"v=1;function onTick()output.setNumber(1,v)end",
        "@prefix.lua",
    )?;
    vm.set_breakpoints(vec![("@suffix.lua".into(), 2)])?;
    assert_eq!(
        vm.load(b"host.check()\nv=7", "@suffix.lua")?,
        RunOutcome::Suspended
    );
    vm.set_breakpoints(vec![])?;
    vm.resume(StepMode::Continue)?;
    fail.store(true, Ordering::Relaxed);
    assert!(vm.reset().is_err());
    vm.tick(&Default::default())?;
    assert_eq!(vm.output().numbers[0], 7.0);
    fail.store(false, Ordering::Relaxed);
    vm.reset()?;
    vm.tick(&Default::default())?;
    assert_eq!(vm.output().numbers[0], 7.0);
    Ok(())
}
#[test]
fn load_history_is_bounded_before_source_executes() -> Result<(), Box<dyn std::error::Error>> {
    // 保持履歴の上限は、ホストがソース長制限を有効化した場合に適用されます（J1）。
    let mut config = MicrocontrollerConfig::default();
    config.limits.max_source_bytes = Some(std::num::NonZeroUsize::new(1024).ok_or("nonzero")?);
    let mut vm = Microcontroller::new(config)?;
    for _ in 0..128 {
        vm.load(b"counter=(counter or 0)+1", "@same.lua")?;
    }
    assert_eq!(
        vm.load(b"counter=999", "@too-many.lua")
            .expect_err("bounded")
            .kind,
        ErrorKind::Limit
    );
    assert!(!vm.is_failed());
    vm.reset()?;
    Ok(())
}
#[test]
fn addon_keeps_single_entry_lifecycle_with_named_requires_and_reload(
) -> Result<(), Box<dyn std::error::Error>> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sources = loader(
        &[(
            "prefix",
            "@prefix.lua",
            "local n=7;function helper()return n end",
        )],
        Arc::clone(&calls),
    );
    let c = AddonConfig {
        environment: EnvironmentProfile::Extended,
        require_loader: Some(sources),
        ..Default::default()
    };
    let mut addon = Addon::new(c)?;
    addon.set_breakpoints(vec![("@prefix.lua".into(), 1)])?;
    assert_eq!(addon.load(b"require('prefix');g_savedata={count=0};function onCreate(new)g_savedata.new=new;g_savedata.value=helper()end;function onTick()g_savedata.count=g_savedata.count+1 end","@main.lua")?,RunOutcome::Suspended);
    assert_eq!(addon.stack()?[0].source, "@prefix.lua");
    addon.set_breakpoints(vec![])?;
    addon.resume(StepMode::Continue)?;
    assert_eq!(
        addon
            .load(b"suffix=1", "@suffix.lua")
            .expect_err("initial-only")
            .kind,
        ErrorKind::InvalidArgument
    );
    addon.start()?;
    addon.tick(1)?;
    let saved = addon.savedata()?;
    addon.reload(saved)?;
    addon.start()?;
    let LuaValue::Table(entries) = addon.savedata()? else {
        unreachable!()
    };
    assert!(entries
        .iter()
        .any(|(k, v)| k == &LuaValue::Bytes(b"count".to_vec()) && v == &LuaValue::Integer(1)));
    assert!(entries
        .iter()
        .any(|(k, v)| k == &LuaValue::Bytes(b"new".to_vec()) && v == &LuaValue::Bool(false)));
    assert_eq!(&*calls.lock().unwrap(), &["prefix", "prefix"]);
    Ok(())
}
