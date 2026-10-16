//! Synchronous event order, ownership and shared-budget contracts.
#![allow(clippy::unwrap_used, clippy::panic)]
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use storm_lua_addon::{Addon, AddonConfig, SynchronousEvents};
use storm_lua_vm::{
    runner::{ErrorKind, RunOutcome, VmError},
    value::{HostFunction, LuaValue},
};

#[test]
fn nested_events_observe_committed_owner_and_post_callback_snapshots() {
    let targets = Rc::new(RefCell::new(Vec::<SynchronousEvents>::new()));
    let owner = Rc::new(Cell::new(0_i64));
    let snapshots = Rc::new(RefCell::new(Vec::new()));
    let trace = Rc::new(RefCell::new(Vec::new()));
    let spawn: HostFunction = {
        let targets = targets.clone();
        let owner = owner.clone();
        let snapshots = snapshots.clone();
        Rc::new(move |args| {
            let [LuaValue::Integer(value)] = args else {
                panic!("integer argument expected")
            };
            let accepted = *value > owner.get();
            if accepted {
                owner.set(*value);
                let recipients = targets.borrow().clone();
                for target in recipients {
                    if target.is_live() {
                        target.dispatch("onTsunami", args)?;
                    }
                }
                snapshots.borrow_mut().push(owner.get());
            }
            Ok(vec![LuaValue::Bool(accepted)])
        })
    };
    let record: HostFunction = {
        let trace = trace.clone();
        Rc::new(move |args| {
            trace.borrow_mut().push(args.to_vec());
            Ok(vec![])
        })
    };
    let mut config = AddonConfig::default();
    config.server.insert("spawn".into(), spawn);
    config.server.insert("record".into(), record);
    let mut a = Addon::new(config.clone()).unwrap();
    let mut b = Addon::new(config).unwrap();
    targets
        .borrow_mut()
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
    assert_eq!(*trace.borrow(), expected);
    assert_eq!(*snapshots.borrow(), [2, 2]);
    assert_eq!(owner.get(), 2);
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
    let target = Rc::new(RefCell::new(None::<SynchronousEvents>));
    let owner = Rc::new(Cell::new(false));
    let failure = Rc::new(RefCell::new(None));
    let spawn: HostFunction = {
        let target = target.clone();
        let owner = owner.clone();
        let failure = failure.clone();
        Rc::new(move |_| {
            owner.set(true);
            *failure.borrow_mut() = target
                .borrow()
                .as_ref()
                .unwrap()
                .dispatch("onTsunami", &[])
                .err();
            Ok(vec![LuaValue::Bool(true)])
        })
    };
    let mut config = AddonConfig::default();
    config.server.insert("spawn".into(), spawn);
    config.server.insert(
        "fail".into(),
        Rc::new(|_| Err(VmError::new(ErrorKind::Host, "expected callback failure"))),
    );
    let observations = Rc::new(RefCell::new(Vec::new()));
    let record = observations.clone();
    config.server.insert(
        "record".into(),
        Rc::new(move |args| {
            *record.borrow_mut() = args.to_vec();
            Ok(vec![])
        }),
    );
    let mut addon = Addon::new(config).unwrap();
    let handle = addon.synchronous_events();
    *target.borrow_mut() = Some(handle.clone());
    addon.load(b"function onTsunami() g_savedata.changed=true;server.fail() end function onCreate() g_savedata.accepted=server.spawn();g_savedata.after=true;server.record(g_savedata.changed,g_savedata.accepted,g_savedata.after) end","fail").unwrap();
    assert_eq!(addon.start().unwrap(), RunOutcome::Completed);
    assert!(owner.get());
    assert!(failure.borrow().is_some());
    assert!(!handle.is_live());
    assert_eq!(*observations.borrow(), vec![LuaValue::Bool(true); 3]);
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
    let target = Rc::new(RefCell::new(None::<SynchronousEvents>));
    let calls = Rc::new(Cell::new(0));
    let handler: HostFunction = {
        let target = target.clone();
        let calls = calls.clone();
        Rc::new(move |args| {
            calls.set(calls.get() + 1);
            let handle = target.borrow().as_ref().unwrap().clone();
            let _callback_result = handle.dispatch("onTsunami", args);
            Ok(vec![LuaValue::Bool(true)])
        })
    };
    let mut config = AddonConfig::default();
    config.limits.instruction_budget = std::num::NonZeroU64::new(60).unwrap();
    config.server.insert("spawn".into(), handler);
    let mut addon = Addon::new(config).unwrap();
    *target.borrow_mut() = Some(addon.synchronous_events());
    addon.load(b"function onTsunami(n) if n<20 then server.spawn(n+1) end end function onCreate() server.spawn(1) end","budget").unwrap();
    assert_eq!(addon.start().unwrap_err().kind, ErrorKind::Limit);
    assert!(
        calls.get() > 1 && calls.get() < 20,
        "nested callbacks reset the instruction budget: {}",
        calls.get()
    );
    assert!(!target.borrow().as_ref().unwrap().is_live());
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
        let target = Rc::new(RefCell::new(None::<SynchronousEvents>));
        let handler: HostFunction = {
            let target = target.clone();
            Rc::new(move |args| {
                let handle = target.borrow().as_ref().unwrap().clone();
                let _callback_result = handle.dispatch("onTsunami", args);
                Ok(vec![])
            })
        };
        let mut config = AddonConfig::default();
        config.limits.instruction_budget = std::num::NonZeroU64::new(60).unwrap();
        config.server.insert("spawn".into(), handler);
        let mut addon = Addon::new(config).unwrap();
        *target.borrow_mut() = Some(addon.synchronous_events());
        addon.load(source.as_bytes(), "restore-budget").unwrap();
        assert_eq!(addon.start().unwrap_err().kind, ErrorKind::Limit);
    }
}

#[test]
fn scoped_errors_retain_later_callbacks_until_the_host_call_returns() {
    let mut config = AddonConfig::default();
    config.server.insert(
        "fail".into(),
        Rc::new(|_| Err(VmError::new(ErrorKind::Host, "expected"))),
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
    config.limits.instruction_budget = std::num::NonZeroU64::new(60).unwrap();
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
    let target = Rc::new(RefCell::new(None::<SynchronousEvents>));
    let mut config = AddonConfig::default();
    let callbacks = target.clone();
    config.server.insert(
        "spawn".into(),
        Rc::new(move |_| {
            let events = callbacks.borrow().as_ref().unwrap().clone();
            let _callback_error = events.dispatch("onTsunami", &[]);
            Ok(vec![])
        }),
    );
    let mut addon = Addon::new(config).unwrap();
    addon.load(b"function onTsunami() error('expected') end\nfunction onTick()\n server.spawn()\n g_savedata.after=true\nend", "=debug-nested").unwrap();
    addon.start().unwrap();
    let pass = addon.begin_synchronous_event_pass().unwrap();
    *target.borrow_mut() = Some(pass.events());
    addon
        .set_breakpoints(vec![("=debug-nested".into(), 4)])
        .unwrap();
    assert_eq!(addon.tick(1).unwrap(), RunOutcome::Suspended);
    drop(pass);
    assert!(!target.borrow().as_ref().unwrap().is_live());
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
    let owner = Rc::new(RefCell::new(None::<Addon>));
    let target = Rc::new(RefCell::new(None::<SynchronousEvents>));
    let mut config = AddonConfig::default();
    let weak_owner = Rc::downgrade(&owner);
    let events = target.clone();
    config.server.insert(
        "probe".into(),
        Rc::new(move |_| {
            let shared = weak_owner.upgrade().unwrap();
            let mut guard = shared.borrow_mut();
            let addon = guard.as_mut().unwrap();
            assert_eq!(addon.tick(1).unwrap_err().kind, ErrorKind::Busy);
            assert_eq!(
                addon.dispatch("onWhirlpool", &[]).unwrap_err().kind,
                ErrorKind::Busy
            );
            assert_eq!(addon.destroy().unwrap_err().kind, ErrorKind::Busy);
            assert_eq!(addon.savedata().unwrap_err().kind, ErrorKind::Busy);
            let target = events.borrow().as_ref().unwrap().clone();
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
    *target.borrow_mut() = Some(events.clone());
    *owner.borrow_mut() = Some(addon);
    assert_eq!(
        events.dispatch("onTsunami", &[]).unwrap(),
        RunOutcome::Completed
    );
    assert!(events.is_live());
    assert!(owner.borrow().as_ref().unwrap().savedata().is_ok());
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
