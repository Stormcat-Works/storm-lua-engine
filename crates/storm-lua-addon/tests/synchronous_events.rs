//! Synchronous event order, ownership and shared-budget contracts.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::sync::{
    atomic::{AtomicI64, AtomicUsize, Ordering},
    Arc, Mutex,
};
use storm_lua_addon::{Addon, AddonConfig, SynchronousEvents};
use storm_lua_vm::{
    runner::{ErrorKind, RunOutcome, VmError},
    value::{HostFunction, LuaValue},
};

#[test]
fn nested_events_observe_committed_owner_and_post_callback_snapshots() {
    let targets = Arc::new(Mutex::new(Vec::<SynchronousEvents>::new()));
    let owner = Arc::new(AtomicI64::new(0_i64));
    let snapshots = Arc::new(Mutex::new(Vec::new()));
    let spawn: HostFunction = {
        let targets = targets.clone();
        let owner = owner.clone();
        let snapshots = snapshots.clone();
        Arc::new(move |args| {
            let [LuaValue::Integer(value)] = args else {
                panic!("integer argument expected")
            };
            let accepted = *value > owner.load(Ordering::Relaxed);
            if accepted {
                owner.store(*value, Ordering::Relaxed);
                let recipients = targets
                    .lock()
                    .map_err(|_| VmError::new(ErrorKind::Host, "target list is poisoned"))?
                    .clone();
                for target in recipients {
                    if target.is_live() {
                        target.dispatch("onTsunami", args)?;
                    }
                }
                snapshots
                    .lock()
                    .map_err(|_| VmError::new(ErrorKind::Host, "snapshots are poisoned"))?
                    .push(owner.load(Ordering::Relaxed));
            }
            Ok(vec![LuaValue::Bool(accepted)])
        })
    };
    let trace = Arc::new(Mutex::new(Vec::new()));
    let record: HostFunction = {
        let trace = trace.clone();
        Arc::new(move |args| {
            trace
                .lock()
                .map_err(|_| VmError::new(ErrorKind::Host, "trace is poisoned"))?
                .push(args.to_vec());
            Ok(vec![])
        })
    };
    let mut config = AddonConfig::default();
    config.server.insert("spawn".into(), spawn);
    config.server.insert("record".into(), record);
    let mut a = Addon::new(config.clone()).unwrap();
    let mut b = Addon::new(config).unwrap();
    targets
        .lock()
        .unwrap()
        .extend([a.synchronous_events(), b.synchronous_events()]);
    a.load(br#"function onTsunami(m) server.record(1,m); if m==1 then g_savedata.inner=server.spawn(2) end end
function onCreate() g_savedata.outer=server.spawn(1); server.record(3,99) end"#,"a").unwrap();
    b.load(b"function onTsunami(m) server.record(2,m) end", "b")
        .unwrap();
    assert_eq!(a.start().unwrap(), RunOutcome::Completed);
    let expected = vec![(1, 1), (1, 2), (2, 2), (2, 1), (3, 99)]
        .into_iter()
        .map(|(a, b)| vec![LuaValue::Integer(a), LuaValue::Integer(b)])
        .collect::<Vec<_>>();
    assert_eq!(*trace.lock().unwrap(), expected);
    assert_eq!(*snapshots.lock().unwrap(), [2, 2]);
    assert_eq!(owner.load(Ordering::Relaxed), 2);
    let saved = a.savedata().unwrap();
    let LuaValue::Table(saved) = saved else {
        panic!("savedata table expected")
    };
    assert!(saved
        .iter()
        .any(|(k, v)| k == &LuaValue::Bytes(b"inner".to_vec()) && v == &LuaValue::Bool(true)));
    assert!(saved
        .iter()
        .any(|(k, v)| k == &LuaValue::Bytes(b"outer".to_vec()) && v == &LuaValue::Bool(true)));
}

#[test]
fn failed_event_does_not_rollback_host_admission_or_abort_outer_host_call() {
    let target = Arc::new(Mutex::new(None::<SynchronousEvents>));
    let owner = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let failure = Arc::new(Mutex::new(None));
    let spawn: HostFunction = {
        let target = target.clone();
        let owner = owner.clone();
        let failure = failure.clone();
        Arc::new(move |_| {
            owner.store(true, Ordering::Relaxed);
            let handle = target
                .lock()
                .map_err(|_| VmError::new(ErrorKind::Host, "target is poisoned"))?
                .as_ref()
                .cloned()
                .ok_or_else(|| VmError::new(ErrorKind::Host, "target is not installed"))?;
            *failure
                .lock()
                .map_err(|_| VmError::new(ErrorKind::Host, "failure slot is poisoned"))? =
                handle.dispatch("onTsunami", &[]).err();
            Ok(vec![LuaValue::Bool(true)])
        })
    };
    let mut config = AddonConfig::default();
    config.server.insert("spawn".into(), spawn);
    config.server.insert(
        "fail".into(),
        Arc::new(|_| Err(VmError::new(ErrorKind::Host, "expected callback failure"))),
    );
    let observations = Arc::new(Mutex::new(Vec::new()));
    let record = observations.clone();
    config.server.insert(
        "record".into(),
        Arc::new(move |args| {
            *record
                .lock()
                .map_err(|_| VmError::new(ErrorKind::Host, "observations are poisoned"))? =
                args.to_vec();
            Ok(vec![])
        }),
    );
    let mut addon = Addon::new(config).unwrap();
    let handle = addon.synchronous_events();
    *target.lock().unwrap() = Some(handle.clone());
    addon.load(b"function onTsunami() g_savedata.changed=true;server.fail() end function onCreate() g_savedata.accepted=server.spawn();g_savedata.after=true;server.record(g_savedata.changed,g_savedata.accepted,g_savedata.after) end","fail").unwrap();
    assert_eq!(addon.start().unwrap(), RunOutcome::Completed);
    assert!(owner.load(Ordering::Relaxed));
    assert!(failure.lock().unwrap().is_some());
    assert!(!handle.is_live());
    assert_eq!(*observations.lock().unwrap(), vec![LuaValue::Bool(true); 3]);
    assert!(addon.tick(1).is_err());
}

#[test]
fn synchronous_events_reject_destroyed_and_reloaded_owners() {
    let mut addon = Addon::new(AddonConfig::default()).unwrap();
    addon
        .load(
            b"function onTsunami() g_savedata.called=true end",
            "lifetime",
        )
        .unwrap();
    addon.start().unwrap();
    let old = addon.synchronous_events();
    addon.reload(addon.savedata().unwrap()).unwrap();
    assert!(!old.is_live());
    assert!(old.dispatch("onTsunami", &[]).is_err());
    addon.start().unwrap();
    let live = addon.synchronous_events();
    addon.destroy().unwrap();
    assert!(!live.is_live());
    assert!(live.dispatch("onTsunami", &[]).is_err());
}

#[test]
fn short_nested_callbacks_share_one_instruction_budget() {
    let target = Arc::new(Mutex::new(None::<SynchronousEvents>));
    let calls = Arc::new(AtomicUsize::new(0));
    let handler: HostFunction = {
        let target = target.clone();
        let calls = calls.clone();
        Arc::new(move |args| {
            calls.fetch_add(1, Ordering::Relaxed);
            let handle = target
                .lock()
                .map_err(|_| VmError::new(ErrorKind::Host, "target is poisoned"))?
                .as_ref()
                .cloned()
                .ok_or_else(|| VmError::new(ErrorKind::Host, "target is not installed"))?;
            let _callback_result = handle.dispatch("onTsunami", args);
            Ok(vec![LuaValue::Bool(true)])
        })
    };
    let mut config = AddonConfig::default();
    config.limits.instruction_budget = Some(std::num::NonZeroU64::new(60).unwrap());
    config.server.insert("spawn".into(), handler);
    let mut addon = Addon::new(config).unwrap();
    *target.lock().unwrap() = Some(addon.synchronous_events());
    addon.load(b"function onTsunami(n) if n<20 then server.spawn(n+1) end end function onCreate() server.spawn(1) end","budget").unwrap();
    assert_eq!(addon.start().unwrap_err().kind, ErrorKind::Limit);
    let attempts = calls.load(Ordering::Relaxed);
    assert!(
        attempts > 1 && attempts < 20,
        "nested callbacks reset the instruction budget: {attempts}"
    );
    assert!(!target.lock().unwrap().as_ref().unwrap().is_live());
}

#[cfg(feature = "debug")]
#[test]
fn suspended_main_callback_rejects_overlap_without_failing_the_script() {
    use storm_lua_vm::runner::StepMode;
    let mut addon = Addon::new(AddonConfig::default()).unwrap();
    addon.load(b"function onTick()\n g_savedata.ticked=true\nend\nfunction onTsunami() g_savedata.wave=true end", "=paused-sync").unwrap();
    addon.start().unwrap();
    let handle = addon.synchronous_events();
    addon
        .set_breakpoints(vec![("=paused-sync".into(), 2)])
        .unwrap();
    assert_eq!(addon.tick(1).unwrap(), RunOutcome::Suspended);
    assert_eq!(
        handle.dispatch("onTsunami", &[]).unwrap_err().kind,
        ErrorKind::Busy
    );
    assert!(handle.is_live());
    addon.set_breakpoints(vec![]).unwrap();
    assert_eq!(
        addon.resume(StepMode::Continue).unwrap(),
        RunOutcome::Completed
    );
    assert_eq!(
        handle.dispatch("onTsunami", &[]).unwrap(),
        RunOutcome::Completed
    );
}

#[test]
fn nested_return_restores_each_callers_instruction_hook() {
    for source in [
        "function onTsunami(n) end function onCreate() server.spawn(1); for i=1,10000 do g_savedata.n=i end end",
        "function onTsunami(n) if n==1 then server.spawn(2); for i=1,10000 do g_savedata.n=i end end end function onCreate() server.spawn(1) end",
    ] {
        let target = Arc::new(Mutex::new(None::<SynchronousEvents>));
        let handler: HostFunction = {
            let target = target.clone();
            Arc::new(move |args| {
                let handle = target
                    .lock()
                    .map_err(|_| VmError::new(ErrorKind::Host, "target is poisoned"))?
                    .clone()
                    .ok_or_else(|| VmError::new(ErrorKind::Host, "target is not installed"))?;
                let _callback_result = handle.dispatch("onTsunami", args);
                Ok(vec![])
            })
        };
        let mut config = AddonConfig::default();
        config.limits.instruction_budget = Some(std::num::NonZeroU64::new(60).unwrap());
        config.server.insert("spawn".into(), handler);
        let mut addon = Addon::new(config).unwrap();
        *target.lock().unwrap() = Some(addon.synchronous_events());
        addon.load(source.as_bytes(), "restore-budget").unwrap();
        assert_eq!(addon.start().unwrap_err().kind, ErrorKind::Limit);
    }
}

#[test]
fn scoped_errors_retain_later_callbacks_until_the_host_call_returns() {
    let mut config = AddonConfig::default();
    config.server.insert(
        "fail".into(),
        Arc::new(|_| Err(VmError::new(ErrorKind::Host, "expected"))),
    );
    let mut addon = Addon::new(config).unwrap();
    addon
        .load(
            b"function onTsunami() g_savedata.calls=(g_savedata.calls or 0)+1;server.fail() end",
            "scoped",
        )
        .unwrap();
    addon.start().unwrap();
    let pass = addon.begin_synchronous_event_pass().unwrap();
    let events = pass.events();
    assert!(addon.begin_synchronous_event_pass().is_err());
    for _ in 0..2 {
        assert_eq!(
            events.dispatch("onTsunami", &[]).unwrap_err().kind,
            ErrorKind::Host
        );
        assert!(events.is_live());
    }
    let LuaValue::Table(saved) = addon.savedata().unwrap() else {
        panic!("table expected")
    };
    assert!(saved
        .iter()
        .any(|(k, v)| k == &LuaValue::Bytes(b"calls".to_vec()) && v == &LuaValue::Integer(2)));
    drop(pass);
    assert!(!events.is_live());
    assert!(events.dispatch("onTsunami", &[]).is_err());
    assert!(addon.tick(1).is_err());
}

#[test]
fn scoped_idle_callbacks_share_budget_and_limits_are_immediately_terminal() {
    let mut config = AddonConfig::default();
    config.limits.instruction_budget = Some(std::num::NonZeroU64::new(60).unwrap());
    let mut addon = Addon::new(config).unwrap();
    addon
        .load(
            b"function onTsunami() g_savedata.calls=(g_savedata.calls or 0)+1 end",
            "scoped-budget",
        )
        .unwrap();
    addon.start().unwrap();
    let pass = addon.begin_synchronous_event_pass().unwrap();
    let events = pass.events();
    let mut limited = false;
    for _ in 0..30 {
        if let Err(error) = events.dispatch("onTsunami", &[]) {
            assert_eq!(error.kind, ErrorKind::Limit);
            limited = true;
            break;
        }
    }
    assert!(limited);
    assert!(!events.is_live());
    assert!(events.dispatch("onTsunami", &[]).is_err());
}

#[test]
fn nonfunction_addon_callbacks_are_missing() {
    let mut addon = Addon::new(AddonConfig::default()).unwrap();
    addon
        .load(b"onTsunami=false; onWhirlpool=42", "nonfunction")
        .unwrap();
    addon.start().unwrap();
    let events = addon.synchronous_events();
    for name in ["onTsunami", "onWhirlpool"] {
        assert_eq!(events.dispatch(name, &[]).unwrap(), RunOutcome::Missing);
    }
    assert!(events.is_live());
}

#[cfg(feature = "debug")]
#[test]
fn nested_lua_error_restores_debugger_and_preserves_non_tick_callbacks() {
    use storm_lua_vm::runner::StepMode;
    let target = Arc::new(Mutex::new(None::<SynchronousEvents>));
    let mut config = AddonConfig::default();
    let callbacks = target.clone();
    config.server.insert(
        "spawn".into(),
        Arc::new(move |_| {
            let events = callbacks
                .lock()
                .map_err(|_| VmError::new(ErrorKind::Host, "target is poisoned"))?
                .as_ref()
                .cloned()
                .ok_or_else(|| VmError::new(ErrorKind::Host, "target is not installed"))?;
            let _callback_error = events.dispatch("onTsunami", &[]);
            Ok(vec![])
        }),
    );
    let mut addon = Addon::new(config).unwrap();
    addon.load(b"function onTsunami() error('expected') end\nfunction onTick()\n server.spawn()\n g_savedata.after=true\nend", "=debug-nested").unwrap();
    addon.start().unwrap();
    let pass = addon.begin_synchronous_event_pass().unwrap();
    *target.lock().unwrap() = Some(pass.events());
    addon
        .set_breakpoints(vec![("=debug-nested".into(), 4)])
        .unwrap();
    assert_eq!(addon.tick(1).unwrap(), RunOutcome::Suspended);
    drop(pass);
    assert!(!target.lock().unwrap().as_ref().unwrap().is_live());
    assert!(!addon.is_failed());
    assert_eq!(
        addon.resume(StepMode::Continue).unwrap(),
        RunOutcome::Completed
    );
    assert_eq!(addon.tick(1).unwrap(), RunOutcome::Missing);
    assert_eq!(
        addon.dispatch("onWhirlpool", &[]).unwrap(),
        RunOutcome::Missing
    );
    assert!(addon.synchronous_events().is_live());
}

#[test]
fn synchronous_callbacks_reject_regular_vm_reentry_without_resetting_the_budget() {
    // Regular (non-synchronous) re-entry into the owning Addon must report Busy
    // while the synchronous callback is active, without resetting its budget.
    let owner: Arc<Mutex<Option<Addon>>> = Arc::new(Mutex::new(None));
    let target = Arc::new(Mutex::new(None::<SynchronousEvents>));
    let mut config = AddonConfig::default();
    let weak_owner = Arc::downgrade(&owner);
    let events = target.clone();
    config.server.insert(
        "probe".into(),
        Arc::new(move |_| {
            let shared = weak_owner
                .upgrade()
                .ok_or_else(|| VmError::new(ErrorKind::Host, "owner was dropped"))?;
            let mut guard = shared
                .lock()
                .map_err(|_| VmError::new(ErrorKind::Host, "owner is poisoned"))?;
            let addon = guard
                .as_mut()
                .ok_or_else(|| VmError::new(ErrorKind::Host, "owner is not installed"))?;
            let busy = |result: Result<RunOutcome, VmError>| {
                assert_eq!(result.unwrap_err().kind, ErrorKind::Busy);
            };
            busy(addon.tick(1));
            busy(addon.dispatch("onWhirlpool", &[]));
            busy(addon.destroy());
            assert_eq!(
                addon.savedata().unwrap_err().kind,
                ErrorKind::Busy,
                "savedata must reject a busy owner"
            );
            let target = events
                .lock()
                .map_err(|_| VmError::new(ErrorKind::Host, "target is poisoned"))?
                .as_ref()
                .cloned()
                .ok_or_else(|| VmError::new(ErrorKind::Host, "target is not installed"))?;
            assert_eq!(
                target.dispatch("onWhirlpool", &[]).unwrap(),
                RunOutcome::Completed
            );
            Ok(vec![])
        }),
    );
    let mut addon = Addon::new(config).unwrap();
    addon.load(b"function onTsunami() server.probe() end function onWhirlpool() g_savedata.nested=true end", "regular-reentry").unwrap();
    addon.start().unwrap();
    let events = addon.synchronous_events();
    *target.lock().unwrap() = Some(events.clone());
    *owner.lock().unwrap() = Some(addon);
    assert_eq!(
        events.dispatch("onTsunami", &[]).unwrap(),
        RunOutcome::Completed
    );
    assert!(events.is_live());
    assert!(owner.lock().unwrap().as_ref().unwrap().savedata().is_ok());
}

#[test]
fn ordinary_synchronous_lua_error_stops_ticks_and_retains_event_delivery() {
    for scoped in [false, true] {
        let mut addon = Addon::new(AddonConfig::default()).unwrap();
        addon.load(b"function onTsunami() g_savedata.failed=true; math.abs(nil) end function onWhirlpool() g_savedata.later=true end function onTick() g_savedata.ticked=true end", "lua-error").unwrap();
        addon.start().unwrap();
        let pass = scoped.then(|| addon.begin_synchronous_event_pass().unwrap());
        let events = pass
            .as_ref()
            .map_or_else(|| addon.synchronous_events(), |pass| pass.events());
        assert_eq!(
            events.dispatch("onTsunami", &[]).unwrap_err().kind,
            ErrorKind::Lua
        );
        assert!(events.is_live());
        assert_eq!(
            events.dispatch("onWhirlpool", &[]).unwrap(),
            RunOutcome::Completed
        );
        drop(pass);
        assert!(!addon.is_failed());
        assert_eq!(addon.tick(1).unwrap(), RunOutcome::Missing);
        assert_eq!(
            addon.dispatch("onWhirlpool", &[]).unwrap(),
            RunOutcome::Completed
        );
        let LuaValue::Table(saved) = addon.savedata().unwrap() else {
            panic!("table expected")
        };
        for key in ["failed", "later"] {
            assert!(saved
                .iter()
                .any(|(k, v)| k == &LuaValue::Bytes(key.as_bytes().to_vec())
                    && v == &LuaValue::Bool(true)));
        }
        assert!(!saved
            .iter()
            .any(|(k, _)| k == &LuaValue::Bytes(b"ticked".to_vec())));
    }
}

#[test]
fn synchronous_pass_and_nested_callbacks_count_every_instruction_above_64_budget() {
    use std::num::NonZeroU64;
    use storm_lua_vm::ExecutionLimits;

    // Bypass A: Idle SynchronousEventPass with budget > 64 across many short dispatches.
    let cfg_a = AddonConfig {
        limits: ExecutionLimits {
            instruction_budget: Some(NonZeroU64::new(500).unwrap()),
            ..ExecutionLimits::default()
        },
        ..AddonConfig::default()
    };
    let mut addon_a = Addon::new(cfg_a).unwrap();
    addon_a
        .load(
            b"function onTsunami() g_savedata[1] = (g_savedata[1] or 0) + 1 end",
            "=pass-budget-500",
        )
        .unwrap();
    addon_a.start().unwrap();
    let pass = addon_a.begin_synchronous_event_pass().unwrap();
    let events = pass.events();
    let mut hit_limit = false;
    for _ in 0..200 {
        match events.dispatch("onTsunami", &[]) {
            Ok(RunOutcome::Completed) => {}
            Err(err) => {
                assert_eq!(err.kind, ErrorKind::Limit);
                hit_limit = true;
                break;
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }
    assert!(
        hit_limit,
        "200 dispatches must exhaust instruction_budget=500"
    );
    drop(pass);
    assert!(addon_a.is_failed());

    // Bypass B: Nested SynchronousCallbacks from Lua after ~1,000-instruction inner loops.
    let target = Arc::new(Mutex::new(None::<SynchronousEvents>));
    let target_cb = target.clone();
    let mut cfg_b = AddonConfig {
        limits: ExecutionLimits {
            instruction_budget: Some(NonZeroU64::new(5000).unwrap()),
            ..ExecutionLimits::default()
        },
        ..AddonConfig::default()
    };
    cfg_b.server.insert(
        "ping".into(),
        Arc::new(move |_| {
            let ev = target_cb
                .lock()
                .map_err(|_| VmError::new(ErrorKind::Host, "poisoned"))?
                .as_ref()
                .cloned()
                .ok_or_else(|| VmError::new(ErrorKind::Host, "missing"))?;
            ev.dispatch("onTsunami", &[])?;
            Ok(vec![])
        }),
    );
    let mut addon_b = Addon::new(cfg_b).unwrap();
    *target.lock().unwrap() = Some(addon_b.synchronous_events());
    addon_b
        .load(
            br#"
            function onTsunami()
                g_savedata[1] = (g_savedata[1] or 0) + 1
            end
            function onTick()
                for outer = 1, 20 do
                    local s = 0
                    for i = 1, 250 do
                        s = s + i
                    end
                    server.ping()
                end
            end
            "#,
            "=nested-budget-5000",
        )
        .unwrap();
    addon_b.start().unwrap();
    let err = addon_b
        .tick(1)
        .expect_err("20 * ~1000 instructions must exceed instruction_budget=5000");
    assert_eq!(err.kind, ErrorKind::Limit);
    assert!(addon_b.is_failed());
}
