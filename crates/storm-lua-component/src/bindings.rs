//! Component Mod の名前空間の組み込み。`matrix` は VM 共有モジュール、
//! `debug.log` は VM の logging が担い、`component` / `parser` テーブルを
//! 提供されたドメインの分だけここで登録します。
//!
//! 引数意味論は実ゲーム v1.15.26 の静的解析で確定した内容に従います:
//! - 引数の個数が合わない呼び出しは全関数で Lua エラー（`Wrong number of args`）。
//! - 型は無言で変換され、整数・数値は変換失敗で 0、bool は false になります。
//! - table 必須の引数（matrix / composite）だけは型不一致が Lua エラーです。
//! - `debug.log` は arity がエラー、型は無言（number/string 以外を捨てる）の
//!   関数単位の例外です。VM 共有の log は arity を検査しないため、
//!   Component ではラッパで検査を足します（他 profile の挙動は変えません）。
//! - phase 制限は実ゲームに確認されていないため、ゲートは入れません。
use super::host::{
    ComponentProviders, ElectricSlots, EnvironmentQuery, FluidContents, FluidSlots, Heater,
    LogicSlots, ParseHandlers, Physics, Render, Sfx, TorqueSlots,
};
use std::rc::Rc;
use storm_lua_spec::io::CompositeSignal;
use storm_lua_vm::backend::{
    self, BackendError, BackendResult, Function, Lua, MultiValue, Table, Value, Variadic,
};

/// 実ゲームの arity エラー。呼び出した Lua コード側の通常エラーとして扱うため、
/// ErrorKind::Lua になる RuntimeError で送出します。
fn wrong_args() -> BackendError {
    BackendError::RuntimeError("Wrong number of args".into())
}

fn arity(args: &[Value], expected: usize) -> BackendResult<()> {
    if args.len() != expected {
        return Err(wrong_args());
    }
    Ok(())
}

/// `lua_tointegerx` 相当。変換できない値は 0。
fn integer(lua: &Lua, value: &Value) -> i64 {
    lua.coerce_integer(value.clone())
        .ok()
        .flatten()
        .unwrap_or(0)
}

/// `lua_tonumberx` 相当。変換できない値は 0。
fn number(lua: &Lua, value: &Value) -> f64 {
    lua.coerce_number(value.clone())
        .ok()
        .flatten()
        .unwrap_or(0.0)
}

/// `lua_toboolean` 相当。false と nil だけが偽。
fn boolean(value: &Value) -> bool {
    !matches!(value, Value::Nil | Value::Boolean(false))
}

/// `lua_tolstring` 相当（数値は文字列化）。文字列化できない値は空として扱います。
fn string_bytes(lua: &Lua, value: &Value) -> Vec<u8> {
    lua.coerce_string(value.clone())
        .ok()
        .flatten()
        .map(|text| text.as_bytes().to_vec())
        .unwrap_or_default()
}

fn integers<const N: usize>(lua: &Lua, args: &[Value], start: usize) -> [i64; N] {
    let mut out = [0_i64; N];
    for (offset, value) in out.iter_mut().enumerate() {
        *value = integer(lua, &args[start + offset]);
    }
    out
}

fn numbers<const N: usize>(lua: &Lua, args: &[Value], start: usize) -> [f64; N] {
    let mut out = [0.0_f64; N];
    for (offset, value) in out.iter_mut().enumerate() {
        *value = number(lua, &args[start + offset]);
    }
    out
}

/// `lua_table_to_composite` 相当。table でない場合は実ゲームと同じ文面で
/// Lua エラーになります。table の形は `number_values` / `bool_values`
/// サブテーブル（キー 1〜32）で、`bool_values` の読みは実ゲームで確定、
/// `number_values` は対称形の想定です（U3）。欠けた要素は 0 / false です。
fn composite_from_table(value: &Value) -> BackendResult<CompositeSignal> {
    let Value::Table(table) = value else {
        return Err(BackendError::RuntimeError(
            "lua_table_to_composite: invalid table".into(),
        ));
    };
    let mut signal = CompositeSignal::default();
    if let Value::Table(numbers_table) = table.raw_get::<Value>("number_values")? {
        for (index, slot) in signal.numbers.iter_mut().enumerate() {
            let element = numbers_table.raw_get::<Value>(index + 1)?;
            if let Value::Number(n) = element {
                *slot = n as f32;
            } else if let Value::Integer(n) = element {
                *slot = n as f32;
            }
        }
    }
    if let Value::Table(bools_table) = table.raw_get::<Value>("bool_values")? {
        for (index, slot) in signal.booleans.iter_mut().enumerate() {
            let element = bools_table.raw_get::<Value>(index + 1)?;
            *slot = boolean(&element);
        }
    }
    Ok(signal)
}

/// [`composite_from_table`] と対の Lua 表現を構築します。
fn composite_to_table(lua: &Lua, signal: &CompositeSignal) -> BackendResult<Table> {
    let table = lua.create_table()?;
    let numbers_table = lua.create_table()?;
    for (index, value) in signal.numbers.iter().enumerate() {
        numbers_table.raw_set(index + 1, f64::from(*value))?;
    }
    let bools_table = lua.create_table()?;
    for (index, value) in signal.booleans.iter().enumerate() {
        bools_table.raw_set(index + 1, *value)?;
    }
    table.raw_set("number_values", numbers_table)?;
    table.raw_set("bool_values", bools_table)?;
    Ok(table)
}

/// Component の `debug.log` は arity 不一致が Lua エラーです（U1）。
/// VM 共有の log 実装（型ゲートは同じ）を包み、個数の検査だけを足します。
fn wrap_debug_log(lua: &Lua, env: &Table) -> BackendResult<()> {
    let debug: Table = env.raw_get("debug")?;
    let original: Function = debug.raw_get("log")?;
    let wrapper = lua.create_function(move |_, args: Variadic<Value>| {
        arity(&args, 1)?;
        original.call::<()>(args[0].clone())?;
        Ok(())
    })?;
    debug.raw_set("log", wrapper)?;
    Ok(())
}

pub(super) fn install(lua: &Lua, env: &Table, providers: &ComponentProviders) -> BackendResult<()> {
    storm_lua_vm::matrix::install_component(lua, env)?;
    wrap_debug_log(lua, env)?;
    let component = lua.create_table()?;
    if let Some(provider) = &providers.logic {
        install_logic(lua, &component, Rc::clone(provider))?;
    }
    if let Some(provider) = &providers.torque {
        install_torque(lua, &component, Rc::clone(provider))?;
    }
    if let Some(provider) = &providers.fluid {
        install_fluid(lua, &component, Rc::clone(provider))?;
    }
    if let Some(provider) = &providers.electric {
        install_electric(lua, &component, Rc::clone(provider))?;
    }
    if let Some(provider) = &providers.sfx {
        install_sfx(lua, &component, Rc::clone(provider))?;
    }
    if let Some(provider) = &providers.fluid_contents {
        install_fluid_contents(lua, &component, Rc::clone(provider))?;
    }
    if let Some(provider) = &providers.heater {
        install_heater(lua, &component, Rc::clone(provider))?;
    }
    if let Some(provider) = &providers.render {
        install_render(lua, &component, Rc::clone(provider))?;
    }
    if let Some(provider) = &providers.physics {
        install_physics(lua, &component, Rc::clone(provider))?;
    }
    if let Some(provider) = &providers.environment {
        install_environment(lua, &component, Rc::clone(provider))?;
    }
    env.raw_set("component", component)?;
    let parser = lua.create_table()?;
    if let Some(provider) = &providers.parse {
        install_parser(lua, &parser, Rc::clone(provider))?;
    }
    env.raw_set("parser", parser)?;
    Ok(())
}

fn install_logic(lua: &Lua, table: &Table, provider: Rc<dyn LogicSlots>) -> BackendResult<()> {
    let p = Rc::clone(&provider);
    table.raw_set(
        "getInputLogicSlotBool",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            p.get_input_bool(integer(lua, &args[0]))
                .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "getInputLogicSlotFloat",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            p.get_input_float(integer(lua, &args[0]))
                .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "getInputLogicSlotComposite",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            let signal = p
                .get_input_composite(integer(lua, &args[0]))
                .map_err(BackendError::external)?;
            composite_to_table(lua, &signal)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "setOutputLogicSlotBool",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 2)?;
            p.set_output_bool(integer(lua, &args[0]), boolean(&args[1]))
                .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "setOutputLogicSlotFloat",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 2)?;
            p.set_output_float(integer(lua, &args[0]), number(lua, &args[1]))
                .map_err(BackendError::external)
        })?,
    )?;
    table.raw_set(
        "setOutputLogicSlotComposite",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 2)?;
            let signal = composite_from_table(&args[1])?;
            provider
                .set_output_composite(integer(lua, &args[0]), signal)
                .map_err(BackendError::external)
        })?,
    )?;
    Ok(())
}

fn install_torque(lua: &Lua, table: &Table, provider: Rc<dyn TorqueSlots>) -> BackendResult<()> {
    let p = Rc::clone(&provider);
    table.raw_set(
        "slotTorqueIsConnected",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            p.is_connected(integer(lua, &args[0]))
                .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "slotTorqueApplyMomentum",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 3)?;
            p.apply_momentum(
                integer(lua, &args[0]),
                number(lua, &args[1]),
                number(lua, &args[2]),
            )
            .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "slotTorqueCreateBridge",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 2)?;
            p.create_bridge(integer(lua, &args[0]), integer(lua, &args[1]))
                .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "slotTorqueDestroyBridge",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            p.destroy_bridge(integer(lua, &args[0]))
                .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "slotTorqueSetBridgeFactor",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 2)?;
            p.set_bridge_factor(integer(lua, &args[0]), number(lua, &args[1]))
                .map_err(BackendError::external)
        })?,
    )?;
    table.raw_set(
        "slotTorqueSetBridgeRatio",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 2)?;
            provider
                .set_bridge_ratio(integer(lua, &args[0]), number(lua, &args[1]))
                .map_err(BackendError::external)
        })?,
    )?;
    Ok(())
}

fn install_fluid(lua: &Lua, table: &Table, provider: Rc<dyn FluidSlots>) -> BackendResult<()> {
    let p = Rc::clone(&provider);
    table.raw_set(
        "slotFluidResolveFlow",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 8)?;
            p.resolve_flow(
                integer(lua, &args[0]),
                integer(lua, &args[1]),
                number(lua, &args[2]),
                number(lua, &args[3]),
                integers(lua, &args, 4),
            )
            .map_err(BackendError::external)
        })?,
    )?;
    table.raw_set(
        "slotFluidResolveFlowToSlot",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 7)?;
            provider
                .resolve_flow_to_slot(
                    integer(lua, &args[0]),
                    integer(lua, &args[1]),
                    number(lua, &args[2]),
                    number(lua, &args[3]),
                    integers(lua, &args, 4),
                )
                .map_err(BackendError::external)
        })?,
    )?;
    Ok(())
}

fn install_electric(
    lua: &Lua,
    table: &Table,
    provider: Rc<dyn ElectricSlots>,
) -> BackendResult<()> {
    let p = Rc::clone(&provider);
    table.raw_set(
        "slotElectricIsConnected",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            p.is_connected(integer(lua, &args[0]))
                .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "slotElectricAddCharge",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 2)?;
            p.add_charge(integer(lua, &args[0]), number(lua, &args[1]))
                .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "slotElectricRemoveCharge",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 2)?;
            p.remove_charge(integer(lua, &args[0]), number(lua, &args[1]))
                .map_err(BackendError::external)
        })?,
    )?;
    table.raw_set(
        "slotElectricGetChargeFactor",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            provider
                .get_charge_factor(integer(lua, &args[0]))
                .map_err(BackendError::external)
        })?,
    )?;
    Ok(())
}

fn install_sfx(lua: &Lua, table: &Table, provider: Rc<dyn Sfx>) -> BackendResult<()> {
    let p = Rc::clone(&provider);
    table.raw_set(
        "sfxPlayOnce",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 10)?;
            p.play_once(integers(lua, &args, 0), numbers(lua, &args, 2))
                .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "sfxPlayLoop",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 10)?;
            p.play_loop(integers(lua, &args, 0), numbers(lua, &args, 2))
                .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "sfxUpdate",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 8)?;
            p.update(integer(lua, &args[0]), numbers(lua, &args, 1))
                .map_err(BackendError::external)
        })?,
    )?;
    table.raw_set(
        "sfxStop",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            provider
                .stop(integer(lua, &args[0]))
                .map_err(BackendError::external)
        })?,
    )?;
    Ok(())
}

fn install_fluid_contents(
    lua: &Lua,
    table: &Table,
    provider: Rc<dyn FluidContents>,
) -> BackendResult<()> {
    let p = Rc::clone(&provider);
    table.raw_set(
        "fluidContentsTransferVolume",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 3)?;
            p.transfer_volume(
                integer(lua, &args[0]),
                integer(lua, &args[1]),
                number(lua, &args[2]),
            )
            .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "fluidContentsTransferVolumeExceptType",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 4)?;
            p.transfer_volume_except_type(
                integer(lua, &args[0]),
                integer(lua, &args[1]),
                number(lua, &args[2]),
                integer(lua, &args[3]),
            )
            .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "fluidContentsSetFluidTypeVolume",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 3)?;
            p.set_fluid_type_volume(
                integer(lua, &args[0]),
                integer(lua, &args[1]),
                number(lua, &args[2]),
            )
            .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "fluidContentsGetFluidTypeVolume",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 2)?;
            p.get_fluid_type_volume(integer(lua, &args[0]), integer(lua, &args[1]))
                .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "fluidContentsGetVolume",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            p.get_volume(integer(lua, &args[0]))
                .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "fluidContentsSetCapacity",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 2)?;
            p.set_capacity(integer(lua, &args[0]), number(lua, &args[1]))
                .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "fluidContentsGetCapacity",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            p.get_capacity(integer(lua, &args[0]))
                .map_err(BackendError::external)
        })?,
    )?;
    table.raw_set(
        "fluidContentsGetPressure",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            provider
                .get_pressure(integer(lua, &args[0]))
                .map_err(BackendError::external)
        })?,
    )?;
    Ok(())
}

fn install_heater(lua: &Lua, table: &Table, provider: Rc<dyn Heater>) -> BackendResult<()> {
    let p = Rc::clone(&provider);
    table.raw_set(
        "heaterSetSphere",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 5)?;
            p.set_sphere(
                numbers(lua, &args, 0),
                number(lua, &args[3]),
                number(lua, &args[4]),
            )
            .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "heaterSetOrientedBox",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 5)?;
            let matrix = storm_lua_vm::matrix::read_matrix(lua, &args[0])?;
            p.set_oriented_box(matrix, numbers(lua, &args, 1))
                .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "heaterSetTemperature",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            p.set_temperature(number(lua, &args[0]))
                .map_err(BackendError::external)
        })?,
    )?;
    table.raw_set(
        "heaterSetFactor",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 2)?;
            provider
                .set_factor(integer(lua, &args[0]), number(lua, &args[1]))
                .map_err(BackendError::external)
        })?,
    )?;
    Ok(())
}

fn install_render(lua: &Lua, table: &Table, provider: Rc<dyn Render>) -> BackendResult<()> {
    let p = Rc::clone(&provider);
    table.raw_set(
        "renderMesh0",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            let matrix = storm_lua_vm::matrix::read_matrix(lua, &args[0])?;
            p.render_mesh0(matrix).map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "renderMesh1",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            let matrix = storm_lua_vm::matrix::read_matrix(lua, &args[0])?;
            p.render_mesh1(matrix).map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "renderMesh2",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 1)?;
            let matrix = storm_lua_vm::matrix::read_matrix(lua, &args[0])?;
            p.render_mesh2(matrix).map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "renderLight",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 9)?;
            p.render_light(
                numbers(lua, &args, 0),
                [boolean(&args[7]), boolean(&args[8])],
            )
            .map_err(BackendError::external)
        })?,
    )?;
    table.raw_set(
        "renderLaser",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 3)?;
            let matrix = storm_lua_vm::matrix::read_matrix(lua, &args[0])?;
            provider
                .render_laser(matrix, number(lua, &args[1]), number(lua, &args[2]))
                .map_err(BackendError::external)
        })?,
    )?;
    Ok(())
}

fn install_physics(lua: &Lua, table: &Table, provider: Rc<dyn Physics>) -> BackendResult<()> {
    let p = Rc::clone(&provider);
    table.raw_set(
        "physicsImpulse",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 7)?;
            p.impulse(
                numbers(lua, &args, 0),
                numbers(lua, &args, 3),
                boolean(&args[6]),
            )
            .map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "physicsRaycast",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 6)?;
            let values = p
                .raycast(numbers(lua, &args, 0), numbers(lua, &args, 3))
                .map_err(BackendError::external)?;
            backend::encode_values(lua, &values).map_err(BackendError::external)
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "physicsGetLinearVelocity",
        lua.create_function(move |_, args: Variadic<Value>| {
            arity(&args, 0)?;
            let v = p.get_linear_velocity().map_err(BackendError::external)?;
            Ok((v[0], v[1], v[2]))
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "physicsGetLinearVelocityAtPoint",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 3)?;
            let v = p
                .get_linear_velocity_at_point(numbers(lua, &args, 0))
                .map_err(BackendError::external)?;
            Ok((v[0], v[1], v[2]))
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "physicsGetAngularVelocity",
        lua.create_function(move |_, args: Variadic<Value>| {
            arity(&args, 0)?;
            let v = p.get_angular_velocity().map_err(BackendError::external)?;
            Ok((v[0], v[1], v[2]))
        })?,
    )?;
    table.raw_set(
        "spawnProjectile",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 8)?;
            provider
                .spawn_projectile(integers(lua, &args, 0), numbers(lua, &args, 2))
                .map_err(BackendError::external)
        })?,
    )?;
    Ok(())
}

fn install_environment(
    lua: &Lua,
    table: &Table,
    provider: Rc<dyn EnvironmentQuery>,
) -> BackendResult<()> {
    let p = Rc::clone(&provider);
    table.raw_set(
        "getSubmergenceFactor",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 4)?;
            p.get_submergence_factor(numbers(lua, &args, 0))
                .map_err(BackendError::external)
        })?,
    )?;
    table.raw_set(
        "getWindVelocity",
        lua.create_function(move |_, args: Variadic<Value>| {
            arity(&args, 0)?;
            let v = provider
                .get_wind_velocity()
                .map_err(BackendError::external)?;
            Ok((v[0], v[1], v[2]))
        })?,
    )?;
    Ok(())
}

/// parser は純粋変換ではなく、ホストの parse ハンドラ越しの定義データ
/// アクセスです（U2）。変換規則は実ゲームどおり:
/// key は無言で文字列化、parseBool の値は `lua_toboolean` 相当、
/// parseNumber の値は変換失敗で 0、parseString の値は文字列化。
fn install_parser(lua: &Lua, table: &Table, provider: Rc<dyn ParseHandlers>) -> BackendResult<()> {
    let p = Rc::clone(&provider);
    table.raw_set(
        "parseBool",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 2)?;
            let key = string_bytes(lua, &args[0]);
            match p
                .parse_bool(&key, boolean(&args[1]))
                .map_err(BackendError::external)?
            {
                None => Ok(MultiValue::from_vec(vec![Value::Boolean(false)])),
                Some((value, success)) => Ok(MultiValue::from_vec(vec![
                    Value::Boolean(value),
                    Value::Boolean(success),
                ])),
            }
        })?,
    )?;
    let p = Rc::clone(&provider);
    table.raw_set(
        "parseNumber",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 2)?;
            let key = string_bytes(lua, &args[0]);
            let value = number(lua, &args[1]) as f32;
            match p
                .parse_number(&key, value)
                .map_err(BackendError::external)?
            {
                None => Ok(MultiValue::from_vec(vec![Value::Boolean(false)])),
                Some((value, success)) => Ok(MultiValue::from_vec(vec![
                    Value::Number(f64::from(value)),
                    Value::Boolean(success),
                ])),
            }
        })?,
    )?;
    table.raw_set(
        "parseString",
        lua.create_function(move |lua, args: Variadic<Value>| {
            arity(&args, 2)?;
            let key = string_bytes(lua, &args[0]);
            let value = string_bytes(lua, &args[1]);
            match provider
                .parse_string(&key, &value)
                .map_err(BackendError::external)?
            {
                None => Ok(MultiValue::from_vec(vec![Value::Boolean(false)])),
                Some((value, success)) => Ok(MultiValue::from_vec(vec![
                    Value::String(lua.create_string(&value)?),
                    Value::Boolean(success),
                ])),
            }
        })?,
    )?;
    Ok(())
}
