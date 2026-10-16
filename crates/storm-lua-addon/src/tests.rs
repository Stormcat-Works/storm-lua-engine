use super::*;
use std::error::Error;

fn field(addon: &Addon, name: &str) -> Result<Option<LuaValue>, Box<dyn Error>> {
    let LuaValue::Table(entries) = addon.savedata()? else {
        return Err("savedata table expected".into());
    };
    Ok(entries
        .into_iter()
        .find_map(|(key, value)| (key == LuaValue::text(name)).then_some(value)))
}

#[test]
fn host_can_resolve_http_without_invoking_an_origin_callback() -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(Default::default())?;
    addon.load(
        br#"
        function onCreate() server.httpGet(27180, '/request') end
        function httpReply() g_savedata.replied = true end
    "#,
        "=host-http",
    )?;
    addon.start()?;
    let request = addon.drain_http_requests().pop().ok_or("missing request")?;
    // Rejected transport data must leave the token available to the host.
    assert!(addon
        .resolve_http_reply(request.token, &vec![0; 1024 * 1024])
        .is_err());
    let resolved = addon.resolve_http_reply(request.token, b"response")?;
    assert_eq!(resolved, request);
    assert_eq!(field(&addon, "replied")?, None);
    assert!(addon
        .resolve_http_reply(request.token, b"duplicate")
        .is_err());
    addon.dispatch_http_reply(u32::from(resolved.port), &resolved.request, b"response")?;
    assert_eq!(field(&addon, "replied")?, Some(LuaValue::Bool(true)));
    Ok(())
}

#[test]
fn tick_lua_error_retains_globals_and_delivers_events_and_destroy() -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(Default::default())?;
    addon.load(
        br#"
        g_savedata.count = 0
        function onTick() g_savedata.count = g_savedata.count + 1; server.missing() end
        function onPlayerJoin() g_savedata.joined = true end
        function onDestroy() g_savedata.destroyed = true end
    "#,
        "=tick-error",
    )?;
    addon.start()?;
    assert_eq!(
        addon.tick(1).err().ok_or("expected VM failure")?.kind,
        ErrorKind::Lua
    );
    assert!(!addon.is_failed());
    assert_eq!(addon.tick(1)?, RunOutcome::Missing);
    assert_eq!(field(&addon, "count")?, Some(LuaValue::Integer(1)));
    addon.dispatch("onPlayerJoin", &[])?;
    addon.destroy()?;
    assert_eq!(field(&addon, "joined")?, Some(LuaValue::Bool(true)));
    assert_eq!(field(&addon, "destroyed")?, Some(LuaValue::Bool(true)));
    Ok(())
}

#[test]
fn source_lua_error_still_restores_savedata_and_calls_create() -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(AddonConfig {
        is_world_create: false,
        savedata: Some(LuaValue::record([("count", LuaValue::Integer(41))])),
        ..Default::default()
    })?;
    assert_eq!(
        addon
            .load(
                br#"
        function onCreate(new) g_savedata.count = g_savedata.count + 1; g_savedata.new = new end
        g_savedata.count = 999
        server.missing()
    "#,
                "=source-error"
            )
            .err()
            .ok_or("expected VM failure")?
            .kind,
        ErrorKind::Lua
    );
    assert_eq!(field(&addon, "count")?, Some(LuaValue::Integer(41)));
    addon.start()?;
    assert_eq!(field(&addon, "count")?, Some(LuaValue::Integer(42)));
    assert_eq!(field(&addon, "new")?, Some(LuaValue::Bool(false)));
    assert_eq!(addon.tick(1)?, RunOutcome::Missing);
    Ok(())
}

#[test]
fn non_function_callbacks_are_missing_and_false_results_are_ignored() -> Result<(), Box<dyn Error>>
{
    let mut addon = Addon::new(Default::default())?;
    addon.load(
        b"onCreate = false; onTick = 42; onDestroy = {}; function onPlayerJoin() return false end",
        "=non-function",
    )?;
    assert_eq!(addon.start()?, RunOutcome::Missing);
    assert_eq!(addon.tick(1)?, RunOutcome::Missing);
    assert_eq!(addon.dispatch("onPlayerJoin", &[])?, RunOutcome::Completed);
    assert_eq!(addon.destroy()?, RunOutcome::Missing);
    assert!(!addon.is_failed());
    Ok(())
}

#[test]
fn game_addon_base_globals_do_not_include_select() -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(Default::default())?;
    addon.load(
        b"g_savedata.has_select = select ~= nil; g_savedata.has_type = type(type) == 'function'",
        "=base",
    )?;
    assert_eq!(field(&addon, "has_select")?, Some(LuaValue::Bool(false)));
    assert_eq!(field(&addon, "has_type")?, Some(LuaValue::Bool(true)));
    let mut addon = Addon::new(AddonConfig {
        environment: storm_lua_spec::environment::EnvironmentProfile::Extended,
        ..Default::default()
    })?;
    addon.load(
        b"g_savedata.has_select = type(select) == 'function'",
        "=extended-base",
    )?;
    assert_eq!(field(&addon, "has_select")?, Some(LuaValue::Bool(true)));
    Ok(())
}

#[test]
fn resource_limits_and_host_failures_remain_terminal() -> Result<(), Box<dyn Error>> {
    let mut config = AddonConfig::default();
    config.limits.instruction_budget =
        std::num::NonZeroU64::new(1000).ok_or("nonzero test budget")?;
    let mut addon = Addon::new(config)?;
    addon.load(
        b"function onTick() while true do end end; function onDestroy() end",
        "=limit",
    )?;
    addon.start()?;
    assert_eq!(
        addon.tick(1).err().ok_or("expected VM failure")?.kind,
        ErrorKind::Limit
    );
    assert!(addon.is_failed());
    assert_eq!(
        addon.destroy().err().ok_or("expected VM failure")?.kind,
        ErrorKind::Failed
    );

    let handler: HostFunction =
        Rc::new(|_| Err(VmError::new(ErrorKind::Host, "transport failure")));
    let mut addon = Addon::new(AddonConfig {
        server: BTreeMap::from([("query".into(), handler)]),
        ..Default::default()
    })?;
    addon.load(b"function onCreate() server.query() end", "=host-failure")?;
    assert_eq!(
        addon.start().err().ok_or("expected VM failure")?.kind,
        ErrorKind::Host
    );
    assert!(addon.is_failed());
    Ok(())
}

#[test]
fn ordinary_vm_still_fails_closed_after_lua_error() -> Result<(), Box<dyn Error>> {
    let mut vm = Vm::new(Default::default())?;
    assert_eq!(
        vm.execute(b"missing()", "=vehicle-policy")
            .err()
            .ok_or("expected VM failure")?
            .kind,
        ErrorKind::Lua
    );
    assert!(vm.is_failed());
    assert_eq!(
        vm.call("onTick").err().ok_or("expected VM failure")?.kind,
        ErrorKind::Failed
    );
    Ok(())
}

#[test]
fn create_and_event_lua_errors_allow_later_callbacks() -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(Default::default())?;
    addon.load(
        br#"
        function onCreate() server.missing() end
        function onPlayerJoin() server.missing() end
        function onChatMessage() g_savedata.chat = true end
        function onDestroy() g_savedata.destroyed = true end
    "#,
        "=callback-error",
    )?;
    assert_eq!(
        addon.start().err().ok_or("expected VM failure")?.kind,
        ErrorKind::Lua
    );
    assert_eq!(
        addon
            .dispatch("onPlayerJoin", &[])
            .err()
            .ok_or("expected VM failure")?
            .kind,
        ErrorKind::Lua
    );
    assert_eq!(addon.tick(1)?, RunOutcome::Missing);
    addon.dispatch("onChatMessage", &[])?;
    addon.destroy()?;
    assert_eq!(field(&addon, "chat")?, Some(LuaValue::Bool(true)));
    assert_eq!(field(&addon, "destroyed")?, Some(LuaValue::Bool(true)));
    Ok(())
}

#[test]
fn source_syntax_error_still_restores_checkpoint() -> Result<(), Box<dyn Error>> {
    let snapshot = LuaValue::record([("count", LuaValue::Integer(7))]);
    let mut addon = Addon::new(AddonConfig {
        is_world_create: false,
        savedata: Some(snapshot.clone()),
        ..Default::default()
    })?;
    assert_eq!(
        addon
            .load(b"function broken(", "=syntax-error")
            .err()
            .ok_or("expected VM failure")?
            .kind,
        ErrorKind::Lua
    );
    assert_eq!(addon.savedata()?, snapshot);
    assert_eq!(addon.start()?, RunOutcome::Missing);
    assert_eq!(addon.tick(1)?, RunOutcome::Missing);
    assert_eq!(addon.destroy()?, RunOutcome::Missing);
    Ok(())
}

#[test]
fn borrowed_host_phase_supports_cached_alias_and_synchronous_callback() -> Result<(), Box<dyn Error>>
{
    use crate::ScopedServerFunction;
    let mut addon = Addon::new(AddonConfig {
        scoped_server: vec!["spawn".into(), "read".into()],
        ..Default::default()
    })?;
    let target = addon.callback_target();
    let world = RefCell::new(0i64);
    let spawn = |_: &[LuaValue]| {
        *world.borrow_mut() += 1;
        target.dispatch("onVehicleSpawn", &[LuaValue::Integer(42)])?;
        Ok(vec![])
    };
    let read = |_: &[LuaValue]| Ok(vec![LuaValue::Integer(*world.borrow())]);
    let functions: [(&str, ScopedServerFunction<'_>); 2] = [("spawn", &spawn), ("read", &read)];
    target.with_server_functions(&functions, || {
        addon.load(br#"
            local spawn, read = server.spawn, server.read
            g_savedata.order = ''
            function onVehicleSpawn(id)
                if id ~= 42 or read() ~= 1 then server.testFailure() end
                g_savedata.order = g_savedata.order .. 'V'
            end
            function onCreate()
                g_savedata.order = g_savedata.order .. 'C'
                spawn()
                g_savedata.order = g_savedata.order .. 'R'
            end
            function onTick() if read() ~= 1 then server.testFailure() end; g_savedata.order = g_savedata.order .. 'T' end
        "#, "phase")?;
        // Native environment components spawn before onCreate.
        target.dispatch("onVehicleSpawn", &[LuaValue::Integer(42)])?;
        Ok(())
    }).err().ok_or("expected error")?; // read() == 0 is a real script error, but start remains permitted.
    target.with_server_functions(&functions, || {
        addon.start()?;
        Ok(())
    })?;
    target.with_server_functions(&functions, || {
        addon.tick(1)?;
        Ok(())
    })?;
    // The pre-create ordinary error disabled ticks, without disabling onCreate or nested callbacks.
    assert_eq!(
        addon.savedata()?,
        LuaValue::Table(vec![(
            LuaValue::Bytes(b"order".to_vec()),
            LuaValue::Bytes(b"CVR".to_vec())
        )])
    );
    Ok(())
}

#[test]
fn nested_callback_cannot_reset_instruction_budget() -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(AddonConfig {
        scoped_server: vec!["again".into()],
        ..Default::default()
    })?;
    let target = addon.callback_target();
    let again = |_: &[LuaValue]| {
        target.dispatch("onVehicleSpawn", &[])?;
        Ok(vec![])
    };
    target.with_server_functions(&[("again", &again)], || {
        addon.load(b"function onVehicleSpawn() while true do end end; function onCreate() server.again() end", "budget")?;
        let error = addon.start().err().ok_or_else(|| invalid("expected error"))?;
        assert_eq!(error.kind, ErrorKind::Limit);
        assert!(addon.is_failed());
        Ok(())
    })?;
    Ok(())
}

#[test]
fn replaced_addon_invalidates_detached_target() -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(Default::default())?;
    addon.load(b"function onVehicleSpawn() end", "reload")?;
    let old = addon.callback_target();
    addon.reload(LuaValue::Table(vec![]))?;
    assert_eq!(
        old.dispatch("onVehicleSpawn", &[])
            .err()
            .ok_or("expected error")?
            .kind,
        ErrorKind::Failed
    );
    Ok(())
}

#[test]
fn cached_host_alias_uses_each_new_phase_and_restores_outer_phase() -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(AddonConfig {
        scoped_server: vec!["read".into()],
        ..Default::default()
    })?;
    let target = addon.callback_target();
    let first = |_: &[LuaValue]| Ok(vec![LuaValue::Integer(1)]);
    let second = |_: &[LuaValue]| Ok(vec![LuaValue::Integer(2)]);
    target.with_server_functions(&[("read", &first)], || {
        addon.load(b"local read = server.read; function onCreate() g_savedata.value = read() end; function onVehicleSpawn() g_savedata.value = read() end; function onTick() g_savedata.value = read() end", "alias")?;
        addon.start()?;
        target.with_server_functions(&[("read", &second)], || { target.dispatch("onVehicleSpawn", &[])?; Ok(()) })?;
        target.dispatch("onVehicleSpawn", &[])?;
        assert_eq!(field(&addon, "value").map_err(|e| invalid(&e.to_string()))?, Some(LuaValue::Integer(1)));
        let inner: Result<(), VmError> = target.with_server_functions(&[("read", &second)], || Err(invalid("inner host error")));
        assert!(inner.is_err());
        target.dispatch("onVehicleSpawn", &[])?;
        Ok(())
    })?;
    assert_eq!(field(&addon, "value")?, Some(LuaValue::Integer(1)));
    target.with_server_functions(&[("read", &first)], || {
        addon.tick(1)?;
        Ok(())
    })?;
    assert_eq!(field(&addon, "value")?, Some(LuaValue::Integer(1)));
    assert_eq!(
        target
            .dispatch("onVehicleSpawn", &[])
            .err()
            .ok_or("expected closed phase error")?
            .kind,
        ErrorKind::Lua
    );
    addon.reload_with(LuaValue::Table(vec![]), |next, source, name| {
        let next_target = next.callback_target();
        next_target.with_server_functions(&[("read", &second)], || next.load(source, name))
    })?;
    assert_eq!(
        target
            .dispatch("onVehicleSpawn", &[])
            .err()
            .ok_or("expected invalidated target")?
            .kind,
        ErrorKind::Failed
    );
    let fresh = addon.callback_target();
    fresh.with_server_functions(&[("read", &second)], || {
        addon.start()?;
        Ok(())
    })?;
    assert_eq!(field(&addon, "value")?, Some(LuaValue::Integer(2)));
    Ok(())
}

#[cfg(feature = "debug")]
#[test]
fn suspended_dispatch_is_busy_but_watch_can_deliver_synchronous_event() -> Result<(), Box<dyn Error>>
{
    use storm_lua_vm::runner::StepMode;
    let mut addon = Addon::new(AddonConfig {
        scoped_server: vec!["trigger".into()],
        ..Default::default()
    })?;
    addon.load(b"function onVehicleSpawn()\n g_savedata.callback = true\nend\nfunction onTick()\n local x = 1\n g_savedata.tick = x\nend", "=watch-reentry")?;
    addon.start()?;
    addon.set_breakpoints(vec![("=watch-reentry".into(), 5)])?;
    assert_eq!(addon.tick(1)?, RunOutcome::Suspended);
    let target = addon.callback_target();
    assert_eq!(
        target
            .dispatch("onVehicleSpawn", &[])
            .err()
            .ok_or("expected Busy")?
            .kind,
        ErrorKind::Busy
    );
    assert!(!addon.is_failed());
    let trigger = |_: &[LuaValue]| {
        target.dispatch("onVehicleSpawn", &[])?;
        Ok(vec![])
    };
    addon.set_breakpoints(vec![])?;
    target.with_server_functions(&[("trigger", &trigger)], || {
        addon.evaluate_watch(0, "server.trigger()")?;
        Ok(())
    })?;
    assert!(addon.is_suspended());
    assert_eq!(addon.resume(StepMode::Continue)?, RunOutcome::Completed);
    assert_eq!(field(&addon, "callback")?, Some(LuaValue::Bool(true)));
    assert_eq!(field(&addon, "tick")?, Some(LuaValue::Integer(1)));
    Ok(())
}

#[test]
fn borrowed_predicates_preserve_upvalues_passthrough_and_strict_boolean(
) -> Result<(), Box<dyn Error>> {
    use storm_lua_vm::host::{HostCall, HostReturn};
    let mut addon = Addon::new(AddonConfig {
        scoped_server: vec!["choose".into()],
        ..Default::default()
    })?;
    let target = addon.callback_target();
    let choose = |call: HostCall<'_>| {
        if !call.is_function(0)? {
            return Ok(HostReturn::Argument(0));
        }
        let mut selected = 0;
        for value in 1..=3 {
            if call.predicate(0, &[LuaValue::Integer(value)])? {
                selected += 1;
            }
        }
        Ok(HostReturn::Values(vec![LuaValue::Integer(selected)]))
    };
    target.with_server_calls(&[], &[("choose", &choose)], || {
        addon.load(br#"
            function onCreate()
                local count = 0
                g_savedata.selected = server.choose(function(value)
                    count = count + 1
                    if value == 1 then return 1 end
                    if value == 2 then math.abs(nil) end
                    return true
                end)
                g_savedata.count = count
                g_savedata.cycle_rejected = server.choose(function() local t = {}; t.self=t; return t end)
                local t = {}; t.self=t
                g_savedata.identity = server.choose(t) == t
                g_savedata.false_value = server.choose(false)
            end
        "#, "=borrowed-predicate")?;
        addon.start()
    })?;
    assert_eq!(field(&addon, "selected")?, Some(LuaValue::Integer(1)));
    assert_eq!(field(&addon, "count")?, Some(LuaValue::Integer(3)));
    assert_eq!(field(&addon, "cycle_rejected")?, Some(LuaValue::Integer(0)));
    assert_eq!(field(&addon, "identity")?, Some(LuaValue::Bool(true)));
    assert_eq!(field(&addon, "false_value")?, Some(LuaValue::Bool(false)));
    Ok(())
}

#[test]
fn borrowed_predicates_share_budget_and_propagate_caught_exhaustion() -> Result<(), Box<dyn Error>>
{
    use storm_lua_vm::host::{HostCall, HostReturn};
    let mut config = AddonConfig {
        scoped_server: vec!["choose".into()],
        ..Default::default()
    };
    config.limits.instruction_budget = std::num::NonZeroU64::new(2000).ok_or("budget")?;
    let mut addon = Addon::new(config)?;
    let target = addon.callback_target();
    let attempts = std::cell::Cell::new(0);
    let choose = |call: HostCall<'_>| {
        for _ in 0..10000 {
            attempts.set(attempts.get() + 1);
            call.predicate(0, &[])?;
        }
        Ok(HostReturn::Values(Vec::new()))
    };
    let result = target.with_server_calls(&[], &[("choose", &choose)], || {
        addon.load(
            br#"
            function onCreate()
                server.choose(function()
                    local total = 0
                    for i = 1, 50 do total = total + i end
                    return true
                end)
                g_savedata.finished = true
            end
        "#,
            "=predicate-budget",
        )?;
        addon.start()
    });
    assert_eq!(result.err().ok_or("expected limit")?.kind, ErrorKind::Limit);
    assert!(attempts.get() > 1 && attempts.get() < 10000);
    assert!(addon.is_failed());
    Ok(())
}

#[test]
fn borrowed_predicate_allocator_memory_error_propagates() -> Result<(), Box<dyn Error>> {
    use storm_lua_vm::host::{HostCall, HostReturn};
    let mut config = AddonConfig {
        scoped_server: vec!["choose".into()],
        ..Default::default()
    };
    config.limits.lua_memory_bytes =
        std::num::NonZeroUsize::new(512 * 1024).ok_or("memory limit")?;
    let mut addon = Addon::new(config)?;
    let target = addon.callback_target();
    let choose = |call: HostCall<'_>| {
        let accepted = call.predicate(0, &[])?;
        Ok(HostReturn::Values(vec![LuaValue::Bool(accepted)]))
    };
    let result = target.with_server_calls(&[], &[("choose", &choose)], || {
        addon.load(br#"function onCreate() server.choose(function() local t = {}; for i = 1, 1000000 do t[i] = i end; return true end) end"#, "=predicate-oom")?;
        addon.start()
    });
    assert_eq!(
        result.err().ok_or("expected memory limit")?.kind,
        ErrorKind::Limit
    );
    assert!(addon.is_failed());
    Ok(())
}

#[test]
fn borrowed_predicate_c_buffer_runtime_error_rejects_candidate() -> Result<(), Box<dyn Error>> {
    use storm_lua_vm::host::{HostCall, HostReturn};
    let mut config = AddonConfig {
        scoped_server: vec!["choose".into()],
        ..Default::default()
    };
    config.limits.lua_memory_bytes =
        std::num::NonZeroUsize::new(512 * 1024).ok_or("memory limit")?;
    let mut addon = Addon::new(config)?;
    let target = addon.callback_target();
    let choose = |call: HostCall<'_>| {
        Ok(HostReturn::Values(vec![LuaValue::Bool(
            call.predicate(0, &[])?,
        )]))
    };
    target.with_server_calls(&[], &[("choose", &choose)], || {
        // Lua 5.3 lauxlib resizebox calls luaL_error on buffer allocation failure,
        // yielding LUA_ERRRUN, unlike core allocator exhaustion's LUA_ERRMEM.
        addon.load(br#"function onCreate() g_savedata.accepted = server.choose(function() return string.rep("x", 1000000) end); g_savedata.finished = true end"#, "=predicate-buffer")?;
        addon.start()
    })?;
    assert_eq!(field(&addon, "accepted")?, Some(LuaValue::Bool(false)));
    assert_eq!(field(&addon, "finished")?, Some(LuaValue::Bool(true)));
    assert!(!addon.is_failed());
    Ok(())
}

#[test]
fn scoped_http_sink_owns_native_request_arguments_and_wide_reply_port() -> Result<(), Box<dyn Error>>
{
    use storm_lua_vm::host::{HostCall, HostReturn};
    let mut addon = Addon::new(AddonConfig {
        scoped_server: vec!["httpGet".into()],
        ..Default::default()
    })?;
    let requests = RefCell::new(Vec::new());
    let enqueue = |call: HostCall<'_>| {
        requests
            .borrow_mut()
            .push((call.number(0)?, call.string(1)?));
        Ok(HostReturn::Values(Vec::new()))
    };
    let target = addon.callback_target();
    target.with_server_calls(&[], &[("httpGet", &enqueue)], || {
        addon.load(
            br#"
            local get = server.httpGet
            function onCreate() get(' 0x10 ', '/before'..string.char(0)..'after') end
            function onTick() get(-38355.75, 123) end
            function httpReply(port, request, reply)
                g_savedata.port=port; g_savedata.kind=math.type(port)
                g_savedata.request=request; g_savedata.reply=reply
            end
        "#,
            "=shared-http",
        )?;
        addon.start()?;
        addon.tick(1)?;
        Ok(())
    })?;
    assert_eq!(
        requests.into_inner(),
        vec![
            (Some(16.0), Some(b"/before\0after".to_vec())),
            (Some(-38355.75), Some(b"123".to_vec())),
        ]
    );
    assert!(
        addon.drain_http_requests().is_empty(),
        "the SDK queue is not the host sink"
    );
    addon.dispatch_http_reply(0xffff_ffff, b"/request", b"raw\0bytes")?;
    assert_eq!(field(&addon, "port")?, Some(LuaValue::Integer(4294967295)));
    assert_eq!(field(&addon, "kind")?, Some(LuaValue::text("integer")));
    assert_eq!(
        field(&addon, "reply")?,
        Some(LuaValue::Bytes(b"raw\0bytes".to_vec()))
    );
    Ok(())
}

#[test]
fn sdk_http_settlement_is_independent_of_destroyed_vm() -> Result<(), Box<dyn Error>> {
    let mut addon = Addon::new(Default::default())?;
    addon.load(
        b"function onCreate() server.httpGet(27180,'/pending') end",
        "=settlement",
    )?;
    addon.start()?;
    let request = addon
        .drain_http_requests()
        .pop()
        .ok_or("pending request absent")?;
    addon.destroy()?;
    // The convenience callback path rejects before consuming the request.
    assert!(addon.http_reply(request.token, b"response").is_err());
    assert_eq!(
        addon.resolve_http_reply(request.token, b"response")?,
        request
    );
    assert!(addon
        .resolve_http_reply(request.token, b"duplicate")
        .is_err());
    Ok(())
}

#[test]
fn sdk_http_settlement_survives_terminal_instruction_failure() -> Result<(), Box<dyn Error>> {
    let mut config = AddonConfig::default();
    config.limits.instruction_budget =
        std::num::NonZeroU64::new(1000).ok_or("nonzero test budget")?;
    let mut addon = Addon::new(config)?;
    addon.load(
        b"function onCreate() server.httpGet(27180,'/pending') end; function onTick() while true do end end",
        "=failed-settlement",
    )?;
    addon.start()?;
    let request = addon
        .drain_http_requests()
        .pop()
        .ok_or("pending request absent")?;
    assert_eq!(
        addon
            .tick(1)
            .err()
            .ok_or("expected instruction limit")?
            .kind,
        ErrorKind::Limit
    );
    assert!(addon.is_failed());
    assert!(addon.http_reply(request.token, b"response").is_err());
    assert_eq!(
        addon.resolve_http_reply(request.token, b"response")?,
        request
    );
    assert!(addon
        .resolve_http_reply(request.token, b"duplicate")
        .is_err());
    Ok(())
}

#[test]
fn scoped_integer_coercion_retains_lua_integer_rules_without_owned_decoding(
) -> Result<(), Box<dyn Error>> {
    use storm_lua_vm::host::{HostCall, HostReturn};
    let mut addon = Addon::new(AddonConfig {
        scoped_server: vec!["integer".into()],
        ..Default::default()
    })?;
    let observed = RefCell::new(Vec::new());
    let coerce = |call: HostCall<'_>| {
        assert_eq!(call.len(), 1);
        assert!(call.integer(1).is_err(), "missing arguments are not nil");
        observed.borrow_mut().push(call.integer(0)?);
        Ok(HostReturn::Values(Vec::new()))
    };
    let target = addon.callback_target();
    target.with_server_calls(&[], &[("integer", &coerce)], || {
        addon.load(
            br#"function onCreate()
            local f=server.integer
            f(math.maxinteger); f(math.mininteger); f(7.0); f(-0.5)
            f(' 0x10 '); f('1e2'); f('7.5'); f('bad'); f(nil)
            f(true); f({}); f(function() end); f(0/0); f(2^63)
        end"#,
            "=integer-coercion",
        )?;
        addon.start()
    })?;
    assert_eq!(
        observed.into_inner(),
        vec![
            Some(i64::MAX),
            Some(i64::MIN),
            Some(7),
            None,
            Some(16),
            Some(100),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ]
    );
    Ok(())
}
