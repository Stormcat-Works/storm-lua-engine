//! Component Mod のホスト提供ドメインを同期JSホストサービスへ転送するブリッジ。
//!
//! Rust の trait 実装は変換済みの型付き引数を受け取るため、ここでは
//! Lua 引数の並びのまま `value_codec` の型付き形式へ直してホストへ送り、
//! 戻りを trait の型へ戻します。Composite と matrix は Lua テーブルと
//! 同じ形（`number_values` / `bool_values` サブテーブル、キー 1〜16）で運び、
//! 境界の両側で表現が一致するようにします。parser の方向は Rust 側の
//! パース状態が決めるもので、ここでは parse ハンドラとして応答するだけです。
use crate::{codec, host, services::host_error, value_codec};
use serde_json::{json, Value};
use std::sync::Arc;
use storm_lua_bridge::{BridgeError, Status};
use storm_lua_component::host::{
    ComponentProviders, ElectricSlots, EnvironmentQuery, FluidContents, FluidSlots, Heater,
    LogicSlots, ParseHandlers, Physics, Render, Sfx, TorqueSlots,
};
use storm_lua_spec::io::CompositeSignal;
use storm_lua_vm::{
    runner::{ErrorKind, VmError},
    value::LuaValue,
};

fn invalid(message: &str) -> BridgeError {
    BridgeError::new(Status::InvalidArgument, message)
}

fn bad_result(message: &str) -> VmError {
    VmError::new(ErrorKind::Host, message)
}

/// セッション設定で名前を指定されたドメインだけを構築します。
/// 未提供のドメインは登録されず、成功 stub で埋めることもありません。
pub(crate) fn providers(
    names: &[String],
    host_key: u32,
) -> Result<ComponentProviders, BridgeError> {
    if !names.is_empty() && host_key == 0 {
        return Err(invalid("missing component provider host"));
    }
    let mut result = ComponentProviders::default();
    for name in names {
        let bridge = HostBridge { key: host_key };
        match name.as_str() {
            "logic" => {
                if result.logic.replace(Arc::new(bridge)).is_some() {
                    return Err(invalid("duplicate component provider domain"));
                }
            }
            "torque" => {
                if result.torque.replace(Arc::new(bridge)).is_some() {
                    return Err(invalid("duplicate component provider domain"));
                }
            }
            "fluid" => {
                if result.fluid.replace(Arc::new(bridge)).is_some() {
                    return Err(invalid("duplicate component provider domain"));
                }
            }
            "electric" => {
                if result.electric.replace(Arc::new(bridge)).is_some() {
                    return Err(invalid("duplicate component provider domain"));
                }
            }
            "sfx" => {
                if result.sfx.replace(Arc::new(bridge)).is_some() {
                    return Err(invalid("duplicate component provider domain"));
                }
            }
            "fluidContents" => {
                if result.fluid_contents.replace(Arc::new(bridge)).is_some() {
                    return Err(invalid("duplicate component provider domain"));
                }
            }
            "heater" => {
                if result.heater.replace(Arc::new(bridge)).is_some() {
                    return Err(invalid("duplicate component provider domain"));
                }
            }
            "render" => {
                if result.render.replace(Arc::new(bridge)).is_some() {
                    return Err(invalid("duplicate component provider domain"));
                }
            }
            "physics" => {
                if result.physics.replace(Arc::new(bridge)).is_some() {
                    return Err(invalid("duplicate component provider domain"));
                }
            }
            "environment" => {
                if result.environment.replace(Arc::new(bridge)).is_some() {
                    return Err(invalid("duplicate component provider domain"));
                }
            }
            "parse" => {
                if result.parse.replace(Arc::new(bridge)).is_some() {
                    return Err(invalid("duplicate component provider domain"));
                }
            }
            _ => return Err(invalid("unknown component provider domain")),
        }
    }
    Ok(result)
}

#[derive(Clone, Copy)]
struct HostBridge {
    key: u32,
}

impl HostBridge {
    fn call(
        &self,
        domain: &str,
        name: &str,
        args: Vec<LuaValue>,
    ) -> Result<Vec<LuaValue>, VmError> {
        let request = json!({"kind":"component","domain":domain,"name":name,"args":args.iter().map(value_codec::encode).collect::<Vec<_>>()});
        let bytes = host::call(self.key, &request).map_err(host_error)?;
        let value = codec::parse(&bytes).map_err(host_error)?;
        value_codec::values(&value).map_err(host_error)
    }

    fn parse_call(
        &self,
        name: &str,
        key: &[u8],
        value: LuaValue,
    ) -> Result<Option<Vec<LuaValue>>, VmError> {
        let request = json!({"kind":"parse","name":name,"key":value_codec::encode(&LuaValue::Bytes(key.to_vec())),"value":value_codec::encode(&value)});
        let bytes = host::call(self.key, &request).map_err(host_error)?;
        let parsed = codec::parse(&bytes).map_err(host_error)?;
        // JSON null はパース期間外（Lua へは false が 1 個だけ返ります）。
        if parsed == Value::Null {
            return Ok(None);
        }
        let values = value_codec::values(&parsed).map_err(host_error)?;
        if values.len() != 2 {
            return Err(bad_result(
                "parse handler must return a value and a success flag",
            ));
        }
        Ok(Some(values))
    }
}

fn integer(value: i64) -> LuaValue {
    LuaValue::Integer(value)
}

fn number(value: f64) -> LuaValue {
    LuaValue::Number(value)
}

fn integers<const N: usize>(values: [i64; N]) -> Vec<LuaValue> {
    values.into_iter().map(integer).collect()
}

fn numbers<const N: usize>(values: [f64; N]) -> Vec<LuaValue> {
    values.into_iter().map(number).collect()
}

/// bindings の `composite_to_table` と同じ Lua 表現を作ります。
fn composite_value(signal: &CompositeSignal) -> LuaValue {
    let subtable = |entries: Vec<(LuaValue, LuaValue)>| LuaValue::Table(entries);
    let numbers = subtable(
        signal
            .numbers
            .iter()
            .enumerate()
            .map(|(index, value)| (integer(index as i64 + 1), number(f64::from(*value))))
            .collect(),
    );
    let booleans = subtable(
        signal
            .booleans
            .iter()
            .enumerate()
            .map(|(index, value)| (integer(index as i64 + 1), LuaValue::Bool(*value)))
            .collect(),
    );
    LuaValue::Table(vec![
        (LuaValue::Bytes(b"number_values".to_vec()), numbers),
        (LuaValue::Bytes(b"bool_values".to_vec()), booleans),
    ])
}

/// bindings の `composite_from_table` と同じ規則で読みます。
/// 欠けた要素は 0 / false、数値でない要素は無視されます。
fn composite_from_value(value: &LuaValue) -> Result<CompositeSignal, VmError> {
    fn subtable<'a>(
        entries: &'a [(LuaValue, LuaValue)],
        name: &[u8],
    ) -> Option<&'a [(LuaValue, LuaValue)]> {
        entries.iter().find_map(|(key, value)| match key {
            LuaValue::Bytes(bytes) if bytes == name => match value {
                LuaValue::Table(inner) => Some(inner.as_slice()),
                _ => None,
            },
            _ => None,
        })
    }
    fn element(table: &[(LuaValue, LuaValue)], index: usize) -> Option<&LuaValue> {
        table.iter().find_map(|(key, value)| match key {
            LuaValue::Integer(i) if *i == index as i64 + 1 => Some(value),
            _ => None,
        })
    }
    let LuaValue::Table(entries) = value else {
        return Err(bad_result("component host returned a non-table composite"));
    };
    let mut signal = CompositeSignal::default();
    if let Some(table) = subtable(entries, b"number_values") {
        for (index, slot) in signal.numbers.iter_mut().enumerate() {
            match element(table, index) {
                Some(LuaValue::Number(n)) => *slot = *n as f32,
                Some(LuaValue::Integer(n)) => *slot = *n as f32,
                _ => {}
            }
        }
    }
    if let Some(table) = subtable(entries, b"bool_values") {
        for (index, slot) in signal.booleans.iter_mut().enumerate() {
            *slot = !matches!(
                element(table, index),
                None | Some(LuaValue::Nil | LuaValue::Bool(false))
            );
        }
    }
    Ok(signal)
}

/// matrix テーブル（キー 1〜16 の数値）と同じ表現を作ります。
fn matrix_value(matrix: &[f64; 16]) -> LuaValue {
    LuaValue::Table(
        matrix
            .iter()
            .enumerate()
            .map(|(index, value)| (integer(index as i64 + 1), number(*value)))
            .collect(),
    )
}

fn take_bool(results: Vec<LuaValue>) -> Result<bool, VmError> {
    match results.as_slice() {
        [LuaValue::Bool(value)] => Ok(*value),
        _ => Err(bad_result("component host must return exactly one Boolean")),
    }
}

fn take_number(results: Vec<LuaValue>) -> Result<f64, VmError> {
    match results.as_slice() {
        [LuaValue::Number(value)] => Ok(*value),
        [LuaValue::Integer(value)] => Ok(*value as f64),
        _ => Err(bad_result("component host must return exactly one number")),
    }
}

fn take_vec3(results: Vec<LuaValue>) -> Result<[f64; 3], VmError> {
    let values: Result<Vec<f64>, VmError> = results
        .iter()
        .map(|value| match value {
            LuaValue::Number(n) => Ok(*n),
            LuaValue::Integer(n) => Ok(*n as f64),
            _ => Err(bad_result("component host must return only numbers")),
        })
        .collect();
    match values?.as_slice() {
        [x, y, z] => Ok([*x, *y, *z]),
        _ => Err(bad_result(
            "component host must return exactly three numbers",
        )),
    }
}

fn take_composite(results: Vec<LuaValue>) -> Result<CompositeSignal, VmError> {
    match results.as_slice() {
        [value] => composite_from_value(value),
        _ => Err(bad_result(
            "component host must return exactly one composite",
        )),
    }
}

fn take_void(_results: Vec<LuaValue>) -> Result<(), VmError> {
    // 戻り値は bindings 側で破棄されるため、余分な値は無視します。
    Ok(())
}

impl LogicSlots for HostBridge {
    fn get_input_bool(&self, slot: i64) -> Result<bool, VmError> {
        take_bool(self.call("logic", "getInputLogicSlotBool", vec![integer(slot)])?)
    }
    fn get_input_float(&self, slot: i64) -> Result<f64, VmError> {
        take_number(self.call("logic", "getInputLogicSlotFloat", vec![integer(slot)])?)
    }
    fn get_input_composite(&self, slot: i64) -> Result<CompositeSignal, VmError> {
        take_composite(self.call("logic", "getInputLogicSlotComposite", vec![integer(slot)])?)
    }
    fn set_output_bool(&self, slot: i64, value: bool) -> Result<(), VmError> {
        take_void(self.call(
            "logic",
            "setOutputLogicSlotBool",
            vec![integer(slot), LuaValue::Bool(value)],
        )?)
    }
    fn set_output_float(&self, slot: i64, value: f64) -> Result<(), VmError> {
        take_void(self.call(
            "logic",
            "setOutputLogicSlotFloat",
            vec![integer(slot), number(value)],
        )?)
    }
    fn set_output_composite(&self, slot: i64, value: CompositeSignal) -> Result<(), VmError> {
        take_void(self.call(
            "logic",
            "setOutputLogicSlotComposite",
            vec![integer(slot), composite_value(&value)],
        )?)
    }
}

impl TorqueSlots for HostBridge {
    fn is_connected(&self, slot: i64) -> Result<bool, VmError> {
        take_bool(self.call("torque", "slotTorqueIsConnected", vec![integer(slot)])?)
    }
    fn apply_momentum(&self, slot: i64, value: f64, dt: f64) -> Result<(), VmError> {
        take_void(self.call(
            "torque",
            "slotTorqueApplyMomentum",
            vec![integer(slot), number(value), number(dt)],
        )?)
    }
    fn create_bridge(&self, first: i64, second: i64) -> Result<(), VmError> {
        take_void(self.call(
            "torque",
            "slotTorqueCreateBridge",
            vec![integer(first), integer(second)],
        )?)
    }
    fn destroy_bridge(&self, slot: i64) -> Result<(), VmError> {
        take_void(self.call("torque", "slotTorqueDestroyBridge", vec![integer(slot)])?)
    }
    fn set_bridge_factor(&self, slot: i64, factor: f64) -> Result<(), VmError> {
        take_void(self.call(
            "torque",
            "slotTorqueSetBridgeFactor",
            vec![integer(slot), number(factor)],
        )?)
    }
    fn set_bridge_ratio(&self, slot: i64, ratio: f64) -> Result<(), VmError> {
        take_void(self.call(
            "torque",
            "slotTorqueSetBridgeRatio",
            vec![integer(slot), number(ratio)],
        )?)
    }
}

impl FluidSlots for HostBridge {
    fn resolve_flow(
        &self,
        first: i64,
        second: i64,
        x: f64,
        y: f64,
        rest: [i64; 4],
    ) -> Result<f64, VmError> {
        let mut args = vec![integer(first), integer(second), number(x), number(y)];
        args.extend(integers(rest));
        take_number(self.call("fluid", "slotFluidResolveFlow", args)?)
    }
    fn resolve_flow_to_slot(
        &self,
        first: i64,
        second: i64,
        x: f64,
        y: f64,
        rest: [i64; 3],
    ) -> Result<f64, VmError> {
        let mut args = vec![integer(first), integer(second), number(x), number(y)];
        args.extend(integers(rest));
        take_number(self.call("fluid", "slotFluidResolveFlowToSlot", args)?)
    }
}

impl ElectricSlots for HostBridge {
    fn is_connected(&self, slot: i64) -> Result<bool, VmError> {
        take_bool(self.call("electric", "slotElectricIsConnected", vec![integer(slot)])?)
    }
    fn add_charge(&self, slot: i64, charge: f64) -> Result<(), VmError> {
        take_void(self.call(
            "electric",
            "slotElectricAddCharge",
            vec![integer(slot), number(charge)],
        )?)
    }
    fn remove_charge(&self, slot: i64, charge: f64) -> Result<(), VmError> {
        take_void(self.call(
            "electric",
            "slotElectricRemoveCharge",
            vec![integer(slot), number(charge)],
        )?)
    }
    fn get_charge_factor(&self, slot: i64) -> Result<f64, VmError> {
        take_number(self.call(
            "electric",
            "slotElectricGetChargeFactor",
            vec![integer(slot)],
        )?)
    }
}

impl Sfx for HostBridge {
    fn play_once(&self, ints: [i64; 2], numbers_in: [f64; 8]) -> Result<(), VmError> {
        let mut args = integers(ints);
        args.extend(numbers(numbers_in));
        take_void(self.call("sfx", "sfxPlayOnce", args)?)
    }
    fn play_loop(&self, ints: [i64; 2], numbers_in: [f64; 8]) -> Result<(), VmError> {
        let mut args = integers(ints);
        args.extend(numbers(numbers_in));
        take_void(self.call("sfx", "sfxPlayLoop", args)?)
    }
    fn update(&self, id: i64, numbers_in: [f64; 7]) -> Result<(), VmError> {
        let mut args = vec![integer(id)];
        args.extend(numbers(numbers_in));
        take_void(self.call("sfx", "sfxUpdate", args)?)
    }
    fn stop(&self, id: i64) -> Result<(), VmError> {
        take_void(self.call("sfx", "sfxStop", vec![integer(id)])?)
    }
}

impl FluidContents for HostBridge {
    fn transfer_volume(&self, from: i64, to: i64, volume: f64) -> Result<(), VmError> {
        take_void(self.call(
            "fluidContents",
            "fluidContentsTransferVolume",
            vec![integer(from), integer(to), number(volume)],
        )?)
    }
    fn transfer_volume_except_type(
        &self,
        from: i64,
        to: i64,
        volume: f64,
        except_type: i64,
    ) -> Result<(), VmError> {
        take_void(self.call(
            "fluidContents",
            "fluidContentsTransferVolumeExceptType",
            vec![
                integer(from),
                integer(to),
                number(volume),
                integer(except_type),
            ],
        )?)
    }
    fn set_fluid_type_volume(
        &self,
        slot: i64,
        fluid_type: i64,
        volume: f64,
    ) -> Result<(), VmError> {
        take_void(self.call(
            "fluidContents",
            "fluidContentsSetFluidTypeVolume",
            vec![integer(slot), integer(fluid_type), number(volume)],
        )?)
    }
    fn get_fluid_type_volume(&self, slot: i64, fluid_type: i64) -> Result<f64, VmError> {
        take_number(self.call(
            "fluidContents",
            "fluidContentsGetFluidTypeVolume",
            vec![integer(slot), integer(fluid_type)],
        )?)
    }
    fn get_volume(&self, slot: i64) -> Result<f64, VmError> {
        take_number(self.call(
            "fluidContents",
            "fluidContentsGetVolume",
            vec![integer(slot)],
        )?)
    }
    fn set_capacity(&self, slot: i64, capacity: f64) -> Result<(), VmError> {
        take_void(self.call(
            "fluidContents",
            "fluidContentsSetCapacity",
            vec![integer(slot), number(capacity)],
        )?)
    }
    fn get_capacity(&self, slot: i64) -> Result<f64, VmError> {
        take_number(self.call(
            "fluidContents",
            "fluidContentsGetCapacity",
            vec![integer(slot)],
        )?)
    }
    fn get_pressure(&self, slot: i64) -> Result<f64, VmError> {
        take_number(self.call(
            "fluidContents",
            "fluidContentsGetPressure",
            vec![integer(slot)],
        )?)
    }
}

impl Heater for HostBridge {
    fn set_sphere(&self, center: [f64; 3], radius: f64, temperature: f64) -> Result<(), VmError> {
        let mut args = numbers(center);
        args.extend([number(radius), number(temperature)]);
        take_void(self.call("heater", "heaterSetSphere", args)?)
    }
    fn set_oriented_box(&self, matrix: [f64; 16], values: [f64; 4]) -> Result<(), VmError> {
        let mut args = vec![matrix_value(&matrix)];
        args.extend(numbers(values));
        take_void(self.call("heater", "heaterSetOrientedBox", args)?)
    }
    fn set_temperature(&self, temperature: f64) -> Result<(), VmError> {
        take_void(self.call("heater", "heaterSetTemperature", vec![number(temperature)])?)
    }
    fn set_factor(&self, index: i64, factor: f64) -> Result<(), VmError> {
        take_void(self.call(
            "heater",
            "heaterSetFactor",
            vec![integer(index), number(factor)],
        )?)
    }
}

impl Render for HostBridge {
    fn render_mesh0(&self, matrix: [f64; 16]) -> Result<(), VmError> {
        take_void(self.call("render", "renderMesh0", vec![matrix_value(&matrix)])?)
    }
    fn render_mesh1(&self, matrix: [f64; 16]) -> Result<(), VmError> {
        take_void(self.call("render", "renderMesh1", vec![matrix_value(&matrix)])?)
    }
    fn render_mesh2(&self, matrix: [f64; 16]) -> Result<(), VmError> {
        take_void(self.call("render", "renderMesh2", vec![matrix_value(&matrix)])?)
    }
    fn render_light(&self, numbers_in: [f64; 7], flags: [bool; 2]) -> Result<(), VmError> {
        let mut args = numbers(numbers_in);
        args.extend(flags.into_iter().map(LuaValue::Bool));
        take_void(self.call("render", "renderLight", args)?)
    }
    fn render_laser(&self, matrix: [f64; 16], first: f64, second: f64) -> Result<(), VmError> {
        take_void(self.call(
            "render",
            "renderLaser",
            vec![matrix_value(&matrix), number(first), number(second)],
        )?)
    }
}

impl Physics for HostBridge {
    fn impulse(&self, position: [f64; 3], force: [f64; 3], local: bool) -> Result<(), VmError> {
        let mut args = numbers(position);
        args.extend(numbers(force));
        args.push(LuaValue::Bool(local));
        take_void(self.call("physics", "physicsImpulse", args)?)
    }
    fn raycast(&self, origin: [f64; 3], direction: [f64; 3]) -> Result<Vec<LuaValue>, VmError> {
        let mut args = numbers(origin);
        args.extend(numbers(direction));
        // 返値の形は未確定のため、ホスト所有の値をそのまま Lua へ返します（U4）。
        self.call("physics", "physicsRaycast", args)
    }
    fn get_linear_velocity(&self) -> Result<[f64; 3], VmError> {
        take_vec3(self.call("physics", "physicsGetLinearVelocity", vec![])?)
    }
    fn get_linear_velocity_at_point(&self, point: [f64; 3]) -> Result<[f64; 3], VmError> {
        take_vec3(self.call("physics", "physicsGetLinearVelocityAtPoint", numbers(point))?)
    }
    fn get_angular_velocity(&self) -> Result<[f64; 3], VmError> {
        take_vec3(self.call("physics", "physicsGetAngularVelocity", vec![])?)
    }
    fn spawn_projectile(&self, ints: [i64; 2], numbers_in: [f64; 6]) -> Result<(), VmError> {
        let mut args = integers(ints);
        args.extend(numbers(numbers_in));
        take_void(self.call("physics", "spawnProjectile", args)?)
    }
}

impl EnvironmentQuery for HostBridge {
    fn get_submergence_factor(&self, args: [f64; 4]) -> Result<f64, VmError> {
        take_number(self.call("environment", "getSubmergenceFactor", numbers(args))?)
    }
    fn get_wind_velocity(&self) -> Result<[f64; 3], VmError> {
        take_vec3(self.call("environment", "getWindVelocity", vec![])?)
    }
}

impl ParseHandlers for HostBridge {
    fn parse_bool(&self, key: &[u8], value: bool) -> Result<Option<(bool, bool)>, VmError> {
        let Some(values) = self.parse_call("parseBool", key, LuaValue::Bool(value))? else {
            return Ok(None);
        };
        match values.as_slice() {
            [LuaValue::Bool(value), LuaValue::Bool(success)] => Ok(Some((*value, *success))),
            _ => Err(bad_result(
                "parseBool handler must return a Boolean and a success flag",
            )),
        }
    }
    fn parse_number(&self, key: &[u8], value: f32) -> Result<Option<(f32, bool)>, VmError> {
        let Some(values) = self.parse_call("parseNumber", key, number(f64::from(value)))? else {
            return Ok(None);
        };
        match values.as_slice() {
            [LuaValue::Number(value), LuaValue::Bool(success)] => {
                Ok(Some((*value as f32, *success)))
            }
            [LuaValue::Integer(value), LuaValue::Bool(success)] => {
                Ok(Some((*value as f32, *success)))
            }
            _ => Err(bad_result(
                "parseNumber handler must return a number and a success flag",
            )),
        }
    }
    fn parse_string(&self, key: &[u8], value: &[u8]) -> Result<Option<(Vec<u8>, bool)>, VmError> {
        let Some(values) = self.parse_call("parseString", key, LuaValue::Bytes(value.to_vec()))?
        else {
            return Ok(None);
        };
        match values.as_slice() {
            [LuaValue::Bytes(value), LuaValue::Bool(success)] => {
                Ok(Some((value.clone(), *success)))
            }
            _ => Err(bad_result(
                "parseString handler must return bytes and a success flag",
            )),
        }
    }
}
