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
