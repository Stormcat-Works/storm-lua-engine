//! 検出可能なAPIメタデータと実際にインストールされた関数の一致を保証します。
#[path = "support/assertions.rs"]
mod assertion_support;
use std::fmt::Write as _;
use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_spec::catalog::FUNCTIONS;

#[test]
fn every_catalog_entry_matches_its_implemented_profile() -> Result<(), Box<dyn std::error::Error>> {
    let mut plain = Microcontroller::new(Default::default())?;
    let mut developer = Microcontroller::new(MicrocontrollerConfig {
        environment: storm_lua_spec::environment::EnvironmentProfile::Extended,
        ..Default::default()
    })?;
    developer.enable_dev_logs()?;
    let mut regular = String::new();
    let mut extended = String::new();
    let mut seen = std::collections::HashSet::new();
    for function in FUNCTIONS {
        assert!(seen.insert(function.path));
        if function.availability == "host-extension" {
            writeln!(extended, "assert(type({})=='function')", function.path)?;
        } else {
            writeln!(regular, "assert(type({})=='function')", function.path)?;
        }
    }
    regular.push_str("assert(print==nil and type(debug.log)=='function' and debug.getinfo==nil)");
    plain.load(
        &assertion_support::with_assertions(regular.as_bytes()),
        "=catalog",
    )?;
    developer.load(extended.as_bytes(), "=developer-catalog")?;
    Ok(())
}

#[test]
fn addon_catalog_is_distinct_and_all_owned_functions_exist(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut addon = storm_lua_addon::Addon::new(Default::default())?;
    let mut script =
        String::from("assert(screen==nil and input==nil and output==nil and async==nil)\n");
    let mut seen = std::collections::HashSet::new();
    for path in storm_lua_spec::addon::FUNCTIONS {
        assert!(seen.insert(path));
        writeln!(script, "assert(type({path})=='function')")?;
    }
    addon.load(
        &assertion_support::with_assertions(script.as_bytes()),
        "=addon-catalog",
    )?;
    let mut events = std::collections::HashSet::new();
    for callback in storm_lua_spec::addon::EVENTS {
        assert!(events.insert(callback));
        assert!(!["onCreate", "onTick", "onDestroy", "httpReply"].contains(callback));
    }
    Ok(())
}

// ---- Component Mod ----
// カタログの availability と実際の登録状態の一致を保証します:
// implemented は常時、requires-provider は対応プロバイダ提供時のみ存在します。
struct AllProviders;
use std::sync::Arc;
use storm_lua_component::host::{
    ComponentProviders, ElectricSlots, EnvironmentQuery, FluidContents, FluidSlots, Heater,
    LogicSlots, ParseHandlers, Physics, Render, Sfx, TorqueSlots,
};
use storm_lua_vm::{runner::VmError, value::LuaValue};

impl LogicSlots for AllProviders {
    fn get_input_bool(&self, _slot: i64) -> Result<bool, VmError> {
        Ok(false)
    }
    fn get_input_float(&self, _slot: i64) -> Result<f64, VmError> {
        Ok(0.0)
    }
    fn get_input_composite(
        &self,
        _slot: i64,
    ) -> Result<storm_lua_spec::io::CompositeSignal, VmError> {
        Ok(Default::default())
    }
    fn set_output_bool(&self, _slot: i64, _value: bool) -> Result<(), VmError> {
        Ok(())
    }
    fn set_output_float(&self, _slot: i64, _value: f64) -> Result<(), VmError> {
        Ok(())
    }
    fn set_output_composite(
        &self,
        _slot: i64,
        _value: storm_lua_spec::io::CompositeSignal,
    ) -> Result<(), VmError> {
        Ok(())
    }
}
impl TorqueSlots for AllProviders {
    fn is_connected(&self, _slot: i64) -> Result<bool, VmError> {
        Ok(false)
    }
    fn apply_momentum(&self, _slot: i64, _value: f64, _dt: f64) -> Result<(), VmError> {
        Ok(())
    }
    fn create_bridge(&self, _a: i64, _b: i64) -> Result<(), VmError> {
        Ok(())
    }
    fn destroy_bridge(&self, _slot: i64) -> Result<(), VmError> {
        Ok(())
    }
    fn set_bridge_factor(&self, _slot: i64, _factor: f64) -> Result<(), VmError> {
        Ok(())
    }
    fn set_bridge_ratio(&self, _slot: i64, _ratio: f64) -> Result<(), VmError> {
        Ok(())
    }
}
impl FluidSlots for AllProviders {
    fn resolve_flow(
        &self,
        _a: i64,
        _b: i64,
        _x: f64,
        _y: f64,
        _r: [i64; 4],
    ) -> Result<f64, VmError> {
        Ok(0.0)
    }
    fn resolve_flow_to_slot(
        &self,
        _a: i64,
        _b: i64,
        _x: f64,
        _y: f64,
        _r: [i64; 3],
    ) -> Result<f64, VmError> {
        Ok(0.0)
    }
}
impl ElectricSlots for AllProviders {
    fn is_connected(&self, _slot: i64) -> Result<bool, VmError> {
        Ok(false)
    }
    fn add_charge(&self, _slot: i64, _charge: f64) -> Result<(), VmError> {
        Ok(())
    }
    fn remove_charge(&self, _slot: i64, _charge: f64) -> Result<(), VmError> {
        Ok(())
    }
    fn get_charge_factor(&self, _slot: i64) -> Result<f64, VmError> {
        Ok(0.0)
    }
}
impl Sfx for AllProviders {
    fn play_once(&self, _ints: [i64; 2], _numbers: [f64; 8]) -> Result<(), VmError> {
        Ok(())
    }
    fn play_loop(&self, _ints: [i64; 2], _numbers: [f64; 8]) -> Result<(), VmError> {
        Ok(())
    }
    fn update(&self, _id: i64, _numbers: [f64; 7]) -> Result<(), VmError> {
        Ok(())
    }
    fn stop(&self, _id: i64) -> Result<(), VmError> {
        Ok(())
    }
}
impl FluidContents for AllProviders {
    fn transfer_volume(&self, _from: i64, _to: i64, _volume: f64) -> Result<(), VmError> {
        Ok(())
    }
    fn transfer_volume_except_type(
        &self,
        _from: i64,
        _to: i64,
        _volume: f64,
        _except: i64,
    ) -> Result<(), VmError> {
        Ok(())
    }
    fn set_fluid_type_volume(&self, _slot: i64, _ty: i64, _volume: f64) -> Result<(), VmError> {
        Ok(())
    }
    fn get_fluid_type_volume(&self, _slot: i64, _ty: i64) -> Result<f64, VmError> {
        Ok(0.0)
    }
    fn get_volume(&self, _slot: i64) -> Result<f64, VmError> {
        Ok(0.0)
    }
    fn set_capacity(&self, _slot: i64, _capacity: f64) -> Result<(), VmError> {
        Ok(())
    }
    fn get_capacity(&self, _slot: i64) -> Result<f64, VmError> {
        Ok(0.0)
    }
    fn get_pressure(&self, _slot: i64) -> Result<f64, VmError> {
        Ok(0.0)
    }
}
impl Heater for AllProviders {
    fn set_sphere(&self, _c: [f64; 3], _r: f64, _t: f64) -> Result<(), VmError> {
        Ok(())
    }
    fn set_oriented_box(&self, _m: [f64; 16], _v: [f64; 4]) -> Result<(), VmError> {
        Ok(())
    }
    fn set_temperature(&self, _t: f64) -> Result<(), VmError> {
        Ok(())
    }
    fn set_factor(&self, _i: i64, _f: f64) -> Result<(), VmError> {
        Ok(())
    }
}
impl Render for AllProviders {
    fn render_mesh0(&self, _m: [f64; 16]) -> Result<(), VmError> {
        Ok(())
    }
    fn render_mesh1(&self, _m: [f64; 16]) -> Result<(), VmError> {
        Ok(())
    }
    fn render_mesh2(&self, _m: [f64; 16]) -> Result<(), VmError> {
        Ok(())
    }
    fn render_light(&self, _n: [f64; 7], _f: [bool; 2]) -> Result<(), VmError> {
        Ok(())
    }
    fn render_laser(&self, _m: [f64; 16], _a: f64, _b: f64) -> Result<(), VmError> {
        Ok(())
    }
}
impl Physics for AllProviders {
    fn impulse(&self, _p: [f64; 3], _f: [f64; 3], _l: bool) -> Result<(), VmError> {
        Ok(())
    }
    fn raycast(&self, _o: [f64; 3], _d: [f64; 3]) -> Result<Vec<LuaValue>, VmError> {
        Ok(Vec::new())
    }
    fn get_linear_velocity(&self) -> Result<[f64; 3], VmError> {
        Ok([0.0; 3])
    }
    fn get_linear_velocity_at_point(&self, _p: [f64; 3]) -> Result<[f64; 3], VmError> {
        Ok([0.0; 3])
    }
    fn get_angular_velocity(&self) -> Result<[f64; 3], VmError> {
        Ok([0.0; 3])
    }
    fn spawn_projectile(&self, _i: [i64; 2], _n: [f64; 6]) -> Result<(), VmError> {
        Ok(())
    }
}
impl EnvironmentQuery for AllProviders {
    fn get_submergence_factor(&self, _a: [f64; 4]) -> Result<f64, VmError> {
        Ok(0.0)
    }
    fn get_wind_velocity(&self) -> Result<[f64; 3], VmError> {
        Ok([0.0; 3])
    }
}
impl ParseHandlers for AllProviders {
    fn parse_bool(&self, _k: &[u8], v: bool) -> Result<Option<(bool, bool)>, VmError> {
        Ok(Some((v, true)))
    }
    fn parse_number(&self, _k: &[u8], v: f32) -> Result<Option<(f32, bool)>, VmError> {
        Ok(Some((v, true)))
    }
    fn parse_string(&self, _k: &[u8], v: &[u8]) -> Result<Option<(Vec<u8>, bool)>, VmError> {
        Ok(Some((v.to_vec(), true)))
    }
}

#[test]
fn component_catalog_matches_provider_availability() -> Result<(), Box<dyn std::error::Error>> {
    let all = Arc::new(AllProviders);
    let providers = ComponentProviders {
        logic: Some(all.clone()),
        torque: Some(all.clone()),
        fluid: Some(all.clone()),
        electric: Some(all.clone()),
        sfx: Some(all.clone()),
        fluid_contents: Some(all.clone()),
        heater: Some(all.clone()),
        render: Some(all.clone()),
        physics: Some(all.clone()),
        environment: Some(all.clone()),
        parse: Some(all),
    };
    let mut component =
        storm_lua_component::Component::new(storm_lua_component::ComponentConfig {
            providers,
            ..Default::default()
        })?;
    let mut script =
        String::from("assert(server==nil and property==nil and input==nil and output==nil)\n");
    let mut seen = std::collections::HashSet::new();
    for function in storm_lua_spec::component::FUNCTIONS {
        assert!(seen.insert(function.path));
        writeln!(script, "assert(type({})=='function')", function.path)?;
    }
    assert_eq!(seen.len(), 64);
    component.load(
        &assertion_support::with_assertions(script.as_bytes()),
        "=component-catalog",
    )?;

    let mut bare = storm_lua_component::Component::new(Default::default())?;
    let mut script = String::new();
    for function in storm_lua_spec::component::FUNCTIONS {
        let expected = if function.availability == "implemented" {
            "function"
        } else {
            "nil"
        };
        writeln!(script, "assert(type({})=='{expected}')", function.path)?;
    }
    bare.load(
        &assertion_support::with_assertions(script.as_bytes()),
        "=component-catalog-bare",
    )?;

    let mut callbacks = std::collections::HashSet::new();
    for callback in storm_lua_spec::component::CALLBACKS {
        assert!(callbacks.insert(*callback));
    }
    assert_eq!(
        callbacks,
        [
            "onParse",
            "onAddToSimulation",
            "onTick",
            "onRender",
            "onRemoveFromSimulation"
        ]
        .into_iter()
        .collect()
    );
    Ok(())
}
