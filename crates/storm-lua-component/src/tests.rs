//! Component プロファイルのテスト。記録型モックホストで転送の忠実度を見ます。
use super::*;
use crate::host::{
    ComponentProviders, ElectricSlots, EnvironmentQuery, FluidContents, FluidSlots, Heater,
    LogicSlots, ParseHandlers, Physics, Render, Sfx, TorqueSlots,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use storm_lua_spec::io::CompositeSignal;
use storm_lua_vm::runner::VmError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ParseMode {
    /// パース期間外（None を返す）。
    Outside,
    /// 値をそのまま成功で返す。
    #[default]
    Echo,
    /// key が無い読み取り（型の 0 と失敗を返す）。
    Missing,
}

#[derive(Default)]
struct MockHost {
    calls: RefCell<Vec<String>>,
    parse_mode: Cell<ParseMode>,
    parse_calls: RefCell<Vec<String>>,
    torque_momentum: RefCell<Vec<(i64, f64, f64)>>,
    sfx_once: RefCell<Vec<([i64; 2], [f64; 8])>>,
    impulse: RefCell<Vec<([f64; 3], [f64; 3], bool)>>,
    light: RefCell<Vec<([f64; 7], [bool; 2])>>,
    projectile: RefCell<Vec<([i64; 2], [f64; 6])>>,
    mesh0: RefCell<Vec<[f64; 16]>>,
    heater_sphere: RefCell<Vec<([f64; 3], f64, f64)>>,
    composite_out: RefCell<Vec<(i64, CompositeSignal)>>,
    transfer_except: RefCell<Vec<(i64, i64, f64, i64)>>,
    raycast_args: RefCell<Vec<([f64; 3], [f64; 3])>>,
    float_slots: RefCell<Vec<i64>>,
}

impl MockHost {
    fn record(&self, call: String) {
        self.calls.borrow_mut().push(call);
    }
}

impl LogicSlots for MockHost {
    fn get_input_bool(&self, slot: i64) -> Result<bool, VmError> {
        self.record(format!("getInputLogicSlotBool({slot})"));
        Ok(true)
    }
    fn get_input_float(&self, slot: i64) -> Result<f64, VmError> {
        self.float_slots.borrow_mut().push(slot);
        Ok(2.5)
    }
    fn get_input_composite(&self, _slot: i64) -> Result<CompositeSignal, VmError> {
        let mut signal = CompositeSignal::default();
        signal.numbers[0] = 1.5;
        signal.booleans[0] = true;
        Ok(signal)
    }
    fn set_output_bool(&self, slot: i64, value: bool) -> Result<(), VmError> {
        self.record(format!("setOutputLogicSlotBool({slot},{value})"));
        Ok(())
    }
    fn set_output_float(&self, slot: i64, value: f64) -> Result<(), VmError> {
        self.record(format!("setOutputLogicSlotFloat({slot},{value})"));
        Ok(())
    }
    fn set_output_composite(&self, slot: i64, value: CompositeSignal) -> Result<(), VmError> {
        self.composite_out.borrow_mut().push((slot, value));
        Ok(())
    }
}

impl TorqueSlots for MockHost {
    fn is_connected(&self, _slot: i64) -> Result<bool, VmError> {
        Ok(true)
    }
    fn apply_momentum(&self, slot: i64, value: f64, dt: f64) -> Result<(), VmError> {
        self.torque_momentum.borrow_mut().push((slot, value, dt));
        Ok(())
    }
    fn create_bridge(&self, _first: i64, _second: i64) -> Result<(), VmError> {
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

impl FluidSlots for MockHost {
    fn resolve_flow(
        &self,
        _first: i64,
        _second: i64,
        _x: f64,
        _y: f64,
        _rest: [i64; 4],
    ) -> Result<f64, VmError> {
        Ok(3.25)
    }
    fn resolve_flow_to_slot(
        &self,
        _first: i64,
        _second: i64,
        _x: f64,
        _y: f64,
        _rest: [i64; 3],
    ) -> Result<f64, VmError> {
        Ok(4.5)
    }
}

impl ElectricSlots for MockHost {
    fn is_connected(&self, _slot: i64) -> Result<bool, VmError> {
        Ok(true)
    }
    fn add_charge(&self, _slot: i64, _charge: f64) -> Result<(), VmError> {
        Ok(())
    }
    fn remove_charge(&self, _slot: i64, _charge: f64) -> Result<(), VmError> {
        Ok(())
    }
    fn get_charge_factor(&self, _slot: i64) -> Result<f64, VmError> {
        Ok(0.75)
    }
}

impl Sfx for MockHost {
    fn play_once(&self, ints: [i64; 2], numbers: [f64; 8]) -> Result<(), VmError> {
        self.sfx_once.borrow_mut().push((ints, numbers));
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

impl FluidContents for MockHost {
    fn transfer_volume(&self, _from: i64, _to: i64, _volume: f64) -> Result<(), VmError> {
        Ok(())
    }
    fn transfer_volume_except_type(
        &self,
        from: i64,
        to: i64,
        volume: f64,
        except_type: i64,
    ) -> Result<(), VmError> {
        self.transfer_except
            .borrow_mut()
            .push((from, to, volume, except_type));
        Ok(())
    }
    fn set_fluid_type_volume(
        &self,
        _slot: i64,
        _fluid_type: i64,
        _volume: f64,
    ) -> Result<(), VmError> {
        Ok(())
    }
    fn get_fluid_type_volume(&self, _slot: i64, _fluid_type: i64) -> Result<f64, VmError> {
        Ok(11.0)
    }
    fn get_volume(&self, _slot: i64) -> Result<f64, VmError> {
        Ok(22.0)
    }
    fn set_capacity(&self, _slot: i64, _capacity: f64) -> Result<(), VmError> {
        Ok(())
    }
    fn get_capacity(&self, _slot: i64) -> Result<f64, VmError> {
        Ok(33.0)
    }
    fn get_pressure(&self, _slot: i64) -> Result<f64, VmError> {
        Ok(44.0)
    }
}

impl Heater for MockHost {
    fn set_sphere(
        &self,
        center: [f64; 3],
        radius: f64,
        temperature: f64,
    ) -> Result<(), VmError> {
        self.heater_sphere.borrow_mut().push((center, radius, temperature));
        Ok(())
    }
    fn set_oriented_box(&self, _matrix: [f64; 16], _values: [f64; 4]) -> Result<(), VmError> {
        Ok(())
    }
    fn set_temperature(&self, _temperature: f64) -> Result<(), VmError> {
        Ok(())
    }
    fn set_factor(&self, _index: i64, _factor: f64) -> Result<(), VmError> {
        Ok(())
    }
}

impl Render for MockHost {
    fn render_mesh0(&self, matrix: [f64; 16]) -> Result<(), VmError> {
        self.mesh0.borrow_mut().push(matrix);
        Ok(())
    }
    fn render_mesh1(&self, _matrix: [f64; 16]) -> Result<(), VmError> {
        Ok(())
    }
    fn render_mesh2(&self, _matrix: [f64; 16]) -> Result<(), VmError> {
        Ok(())
    }
    fn render_light(&self, numbers: [f64; 7], flags: [bool; 2]) -> Result<(), VmError> {
        self.light.borrow_mut().push((numbers, flags));
        Ok(())
    }
    fn render_laser(&self, _matrix: [f64; 16], _first: f64, _second: f64) -> Result<(), VmError> {
        Ok(())
    }
}

impl Physics for MockHost {
    fn impulse(
        &self,
        position: [f64; 3],
        force: [f64; 3],
        local: bool,
    ) -> Result<(), VmError> {
        self.impulse.borrow_mut().push((position, force, local));
        Ok(())
    }
    fn raycast(
        &self,
        origin: [f64; 3],
        direction: [f64; 3],
    ) -> Result<Vec<LuaValue>, VmError> {
        self.raycast_args.borrow_mut().push((origin, direction));
        Ok(vec![LuaValue::Number(9.5), LuaValue::Bool(true)])
    }
    fn get_linear_velocity(&self) -> Result<[f64; 3], VmError> {
        Ok([1.0, 2.0, 3.0])
    }
    fn get_linear_velocity_at_point(&self, _point: [f64; 3]) -> Result<[f64; 3], VmError> {
        Ok([4.0, 5.0, 6.0])
    }
    fn get_angular_velocity(&self) -> Result<[f64; 3], VmError> {
        Ok([7.0, 8.0, 9.0])
    }
    fn spawn_projectile(&self, ints: [i64; 2], numbers: [f64; 6]) -> Result<(), VmError> {
        self.projectile.borrow_mut().push((ints, numbers));
        Ok(())
    }
}

impl EnvironmentQuery for MockHost {
    fn get_submergence_factor(&self, _args: [f64; 4]) -> Result<f64, VmError> {
        Ok(0.5)
    }
    fn get_wind_velocity(&self) -> Result<[f64; 3], VmError> {
        Ok([4.0, 5.0, 6.0])
    }
}

impl ParseHandlers for MockHost {
    fn parse_bool(&self, key: &[u8], value: bool) -> Result<Option<(bool, bool)>, VmError> {
        self.parse_calls.borrow_mut().push(format!(
            "parseBool({},{value})",
            String::from_utf8_lossy(key)
        ));
        Ok(match self.parse_mode.get() {
            ParseMode::Outside => None,
            ParseMode::Echo => Some((value, true)),
            ParseMode::Missing => Some((false, false)),
        })
    }
    fn parse_number(&self, key: &[u8], value: f32) -> Result<Option<(f32, bool)>, VmError> {
        self.parse_calls.borrow_mut().push(format!(
            "parseNumber({},{value})",
            String::from_utf8_lossy(key)
        ));
        Ok(match self.parse_mode.get() {
            ParseMode::Outside => None,
            ParseMode::Echo => Some((value, true)),
            ParseMode::Missing => Some((0.0, false)),
        })
    }
    fn parse_string(
        &self,
        key: &[u8],
        value: &[u8],
    ) -> Result<Option<(Vec<u8>, bool)>, VmError> {
        self.parse_calls.borrow_mut().push(format!(
            "parseString({},{})",
            String::from_utf8_lossy(key),
            String::from_utf8_lossy(value)
        ));
        Ok(match self.parse_mode.get() {
            ParseMode::Outside => None,
            ParseMode::Echo => Some((value.to_vec(), true)),
            ParseMode::Missing => Some((Vec::new(), false)),
        })
    }
}

fn full_component() -> (Component, Rc<MockHost>) {
    let mock = Rc::new(MockHost::default());
    let config = ComponentConfig {
        providers: ComponentProviders {
            logic: Some(Rc::clone(&mock) as Rc<dyn LogicSlots>),
            torque: Some(Rc::clone(&mock) as Rc<dyn TorqueSlots>),
            fluid: Some(Rc::clone(&mock) as Rc<dyn FluidSlots>),
            electric: Some(Rc::clone(&mock) as Rc<dyn ElectricSlots>),
            sfx: Some(Rc::clone(&mock) as Rc<dyn Sfx>),
            fluid_contents: Some(Rc::clone(&mock) as Rc<dyn FluidContents>),
            heater: Some(Rc::clone(&mock) as Rc<dyn Heater>),
            render: Some(Rc::clone(&mock) as Rc<dyn Render>),
            physics: Some(Rc::clone(&mock) as Rc<dyn Physics>),
            environment: Some(Rc::clone(&mock) as Rc<dyn EnvironmentQuery>),
            parse: Some(Rc::clone(&mock) as Rc<dyn ParseHandlers>),
        },
        ..Default::default()
    };
    (Component::new(config).expect("component"), mock)
}

fn bare_component() -> Component {
    Component::new(Default::default()).expect("component")
}

fn start(component: &mut Component, source: &[u8]) {
    component.load(source, "=test").expect("load");
    component.parse().expect("parse");
    component.add_to_simulation().expect("add");
}

fn probe_type(component: &mut Component, expression: &str) -> String {
    component
        .vm
        .execute(
            format!("__probe = type({expression})").as_bytes(),
            "=probe",
        )
        .expect("probe executes");
    match component.vm.global("__probe") {
        Ok(LuaValue::Bytes(bytes)) => String::from_utf8(bytes).expect("utf8"),
        other => panic!("unexpected probe value: {other:?}"),
    }
}

fn global(component: &Component, name: &str) -> LuaValue {
    component.vm.global(name).expect("global")
}

#[test]
fn base_environment_matches_the_shared_game_subset() {
    let mut component = bare_component();
    component.load(b"", "=test").expect("load");
    // Game 系統と共通の subset: 存在しない base 関数。
    for name in ["select", "pcall", "error", "print", "_G", "server", "property", "input", "output"] {
        assert_eq!(probe_type(&mut component, name), "nil", "{name}");
    }
    // base 6 関数と完全な math/table/string（string.dump を含む）。
    for name in [
        "ipairs", "next", "pairs", "tonumber", "tostring", "type", "math.floor", "table.insert",
        "string.format", "string.dump",
    ] {
        assert_eq!(probe_type(&mut component, name), "function", "{name}");
    }
    assert_eq!(probe_type(&mut component, "debug"), "table");
    assert_eq!(probe_type(&mut component, "debug.log"), "function");
    assert_eq!(probe_type(&mut component, "debug.getinfo"), "nil");
    assert_eq!(probe_type(&mut component, "component"), "table");
    assert_eq!(probe_type(&mut component, "parser"), "table");
    assert_eq!(probe_type(&mut component, "matrix"), "table");
}

#[test]
fn provider_domains_gate_registration() {
    let mut bare = bare_component();
    bare.load(b"", "=test").expect("load");
    // 未提供ドメインの関数は登録されない（stub で埋めない）。
    assert_eq!(probe_type(&mut bare, "component.getInputLogicSlotBool"), "nil");
    assert_eq!(probe_type(&mut bare, "component.renderMesh0"), "nil");
    assert_eq!(probe_type(&mut bare, "parser.parseBool"), "nil");
    // エンジン実装分は常にある（matrix は component 集合の13関数、scale を含む）。
    assert_eq!(probe_type(&mut bare, "matrix.identity"), "function");
    assert_eq!(probe_type(&mut bare, "matrix.scale"), "function");

    let mock = Rc::new(MockHost::default());
    let mut logic_only = Component::new(ComponentConfig {
        providers: ComponentProviders {
            logic: Some(Rc::clone(&mock) as Rc<dyn LogicSlots>),
            ..Default::default()
        },
        ..Default::default()
    })
    .expect("component");
    logic_only.load(b"", "=test").expect("load");
    assert_eq!(probe_type(&mut logic_only, "component.getInputLogicSlotBool"), "function");
    assert_eq!(probe_type(&mut logic_only, "component.slotTorqueIsConnected"), "nil");
}

#[test]
fn lifecycle_order_is_enforced() {
    let mut component = bare_component();
    assert!(component.parse().is_err());
    assert!(component.tick(0).is_err());
    assert!(component.remove_from_simulation().is_err());
    component.load(b"", "=test").expect("load");
    assert!(component.load(b"", "=test").is_err());
    assert!(component.add_to_simulation().is_err());
    component.parse().expect("parse");
    assert!(component.tick(0).is_err());
    component.add_to_simulation().expect("add");
    component.tick(0).expect("tick");
    component.render().expect("render");
    component.remove_from_simulation().expect("remove");
    assert!(component.tick(0).is_err());
    assert!(component.render().is_err());
}

#[test]
fn undefined_and_non_function_callbacks_are_missing() {
    let mut component = bare_component();
    start(&mut component, b"onTick = 5\n");
    assert_eq!(component.tick(1).expect("tick"), RunOutcome::Missing);
    assert_eq!(component.render().expect("render"), RunOutcome::Missing);
    assert_eq!(component.status(), ComponentStatus::Ok);
    component.remove_from_simulation().expect("remove");
}

#[test]
fn status_mapping_and_tick_gate() {
    // load = 3。失敗後もライフサイクルは進む。
    let mut component = bare_component();
    let error = component.load(b"boom()", "=test").expect_err("load");
    assert!(error.message.contains("boom"));
    assert_eq!(component.status(), ComponentStatus::LoadFailed);
    assert_eq!(component.status().code(), Some(3));
    component.parse().expect("parse proceeds");

    // parse = 5。
    let mut component = bare_component();
    component
        .load(b"function onParse() boom() end", "=test")
        .expect("load");
    component.parse().expect_err("parse");
    assert_eq!(component.status(), ComponentStatus::CallbackFailed);
    assert_eq!(component.status().code(), Some(5));
    component.add_to_simulation().expect("add proceeds");

    // tick = 2 + ゲート + remove は失敗後も配送される。
    let mut component = bare_component();
    start(
        &mut component,
        b"__n = 0\nfunction onTick(t) __n = __n + 1 boom() end\nfunction onRemoveFromSimulation() __removed = true end",
    );
    component.tick(1).expect_err("tick");
    assert_eq!(component.status(), ComponentStatus::TickFailed);
    assert_eq!(component.status().code(), Some(2));
    assert_eq!(global(&component, "__n"), LuaValue::Integer(1));
    assert_eq!(component.tick(1).expect("gated tick"), RunOutcome::Missing);
    assert_eq!(global(&component, "__n"), LuaValue::Integer(1));
    assert_eq!(
        component.remove_from_simulation().expect("remove"),
        RunOutcome::Completed
    );
    assert_eq!(global(&component, "__removed"), LuaValue::Bool(true));

    // render = 5。
    let mut component = bare_component();
    start(&mut component, b"function onRender() boom() end");
    component.render().expect_err("render");
    assert_eq!(component.status(), ComponentStatus::CallbackFailed);
}

#[test]
fn game_ticks_is_passed_as_an_integer_including_zero() {
    let mut component = bare_component();
    start(&mut component, b"function onTick(t) __t = t end");
    component.tick(0).expect("tick 0");
    assert_eq!(global(&component, "__t"), LuaValue::Integer(0));
    component.tick(u32::MAX).expect("tick max");
    assert_eq!(global(&component, "__t"), LuaValue::Integer(i64::from(u32::MAX)));
}

#[test]
fn measured_argument_shapes_reach_the_host() {
    let (mut component, mock) = full_component();
    start(
        &mut component,
        b"function onTick(t)\n  component.sfxPlayOnce(1, 2, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8)\n  component.slotTorqueApplyMomentum(2, 9.5, 0.25)\n  component.physicsImpulse(1, 2, 3, 4, 5, 6, true)\n  component.renderLight(1, 2, 3, 4, 5, 6, 7, true, false)\n  component.spawnProjectile(3, 4, 1, 2, 3, 4, 5, 6)\n  component.heaterSetSphere(1, 2, 3, 4, 5)\n  component.renderMesh0(matrix.translation(1, 2, 3))\n  local x = component.physicsGetLinearVelocity()\n  __vx = x\n  local d, hit = component.physicsRaycast(1, 2, 3, 0, 1, 0)\n  __d = d __hit = hit\nend",
    );
    component.tick(1).expect("tick");
    assert_eq!(
        mock.sfx_once.borrow().as_slice(),
        &[([1, 2], [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8])]
    );
    assert_eq!(mock.torque_momentum.borrow().as_slice(), &[(2, 9.5, 0.25)]);
    assert_eq!(
        mock.impulse.borrow().as_slice(),
        &[([1.0, 2.0, 3.0], [4.0, 5.0, 6.0], true)]
    );
    assert_eq!(
        mock.light.borrow().as_slice(),
        &[([1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0], [true, false])]
    );
    assert_eq!(
        mock.projectile.borrow().as_slice(),
        &[([3, 4], [1.0, 2.0, 3.0, 4.0, 5.0, 6.0])]
    );
    assert_eq!(
        mock.heater_sphere.borrow().as_slice(),
        &[([1.0, 2.0, 3.0], 4.0, 5.0)]
    );
    let mesh = mock.mesh0.borrow();
    assert_eq!(mesh.len(), 1);
    assert_eq!((mesh[0][0], mesh[0][5], mesh[0][10]), (1.0, 1.0, 1.0));
    assert_eq!((mesh[0][12], mesh[0][13], mesh[0][14]), (1.0, 2.0, 3.0));
    assert_eq!(
        mock.raycast_args.borrow().as_slice(),
        &[([1.0, 2.0, 3.0], [0.0, 1.0, 0.0])]
    );
    assert_eq!(global(&component, "__vx"), LuaValue::Number(1.0));
    assert_eq!(global(&component, "__d"), LuaValue::Number(9.5));
    assert_eq!(global(&component, "__hit"), LuaValue::Bool(true));
}

#[test]
fn host_forwarding_keeps_values_faithful() {
    let (mut component, mock) = full_component();
    start(
        &mut component,
        b"function onTick(t)\n  __f = component.getInputLogicSlotFloat(3)\n  component.setOutputLogicSlotComposite(1, { number_values = { [1] = 2.5, [32] = -1 }, bool_values = { [1] = true, [2] = true } })\n  local c = component.getInputLogicSlotComposite(0)\n  __n1 = c.number_values[1] __b1 = c.bool_values[1]\n  component.fluidContentsTransferVolumeExceptType(0, 1, 2.5, 7)\nend",
    );
    component.tick(1).expect("tick");
    // slot index は整数のまま届く。
    assert_eq!(mock.float_slots.borrow().as_slice(), &[3]);
    assert_eq!(global(&component, "__f"), LuaValue::Number(2.5));
    // Composite は欠けた要素を 0/false で埋めて往復する。
    let out = mock.composite_out.borrow();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, 1);
    assert_eq!(out[0].1.numbers[0], 2.5);
    assert_eq!(out[0].1.numbers[31], -1.0);
    assert_eq!(out[0].1.numbers[1], 0.0);
    assert!(out[0].1.booleans[0] && out[0].1.booleans[1] && !out[0].1.booleans[2]);
    assert_eq!(global(&component, "__n1"), LuaValue::Number(1.5));
    assert_eq!(global(&component, "__b1"), LuaValue::Bool(true));
    // 引数は欠けずに届く。
    assert_eq!(mock.transfer_except.borrow().as_slice(), &[(0, 1, 2.5, 7)]);
}

#[test]
fn composite_argument_must_be_a_table() {
    let (mut component, _mock) = full_component();
    start(
        &mut component,
        b"function onTick(t) component.setOutputLogicSlotComposite(1, 5) end",
    );
    let error = component.tick(1).expect_err("tick");
    assert!(
        error.message.contains("lua_table_to_composite: invalid table"),
        "{}",
        error.message
    );
    assert_eq!(component.status(), ComponentStatus::TickFailed);
}

#[test]
fn debug_log_has_arity_errors_and_a_type_gate() {
    // ログ記録は Extended 環境の開発機能（VM の契約どおり）。
    let mut component = Component::new(ComponentConfig {
        environment: storm_lua_spec::environment::EnvironmentProfile::Extended,
        ..Default::default()
    })
    .expect("component");
    component.enable_dev_logs().expect("logs");
    start(
        &mut component,
        b"function onTick(t) debug.log('hello') debug.log(42) debug.log(true) end",
    );
    component.tick(1).expect("tick");
    let records = component.drain_log_records();
    assert_eq!(records.len(), 2);
    assert!(records.iter().any(|record| String::from_utf8_lossy(&record.bytes).contains("hello")));

    let mut component = bare_component();
    start(&mut component, b"function onTick(t) debug.log() end");
    let error = component.tick(1).expect_err("tick");
    assert!(error.message.contains("Wrong number of args"), "{}", error.message);
    assert_eq!(component.status(), ComponentStatus::TickFailed);

    let mut component = bare_component();
    start(&mut component, b"function onTick(t) debug.log('a', 'b') end");
    assert!(component.tick(1).is_err());
}

#[test]
fn lua_filename_is_the_chunk_name_and_long_sources_are_not_truncated() {
    let mut component = bare_component();
    component
        .load(b"__ok = 1", "scripts/my_component.lua")
        .expect("load");
    assert_eq!(global(&component, "__ok"), LuaValue::Integer(1));

    let mut component = bare_component();
    let error = component
        .load(b"boom()", "scripts/my_component.lua")
        .expect_err("load");
    assert!(
        error.message.contains("my_component.lua"),
        "{}",
        error.message
    );

    // 定義 XML 経由の 0x2000 バイト切り詰めはファイル読みには適用されない。
    let mut source = String::from("--");
    source.push_str(&"x".repeat(9000));
    source.push_str("\n__long = 42\n");
    let mut component = bare_component();
    component.load(source.as_bytes(), "=long").expect("load");
    assert_eq!(global(&component, "__long"), LuaValue::Integer(42));
}

#[test]
fn tick_timeout_records_timed_out_without_a_numeric_code() {
    let config = ComponentConfig {
        limits: storm_lua_vm::ExecutionLimits {
            time_limit: Some(std::time::Duration::from_millis(50)),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut component = Component::new(config).expect("component");
    start(&mut component, b"function onTick(t) while true do end end");
    component.tick(1).expect_err("tick");
    assert_eq!(component.status(), ComponentStatus::TimedOut);
    assert_eq!(component.status().code(), None);
    assert_eq!(component.tick(1).expect("gated tick"), RunOutcome::Missing);
}

fn dummy_arg(parameter: &str) -> &'static str {
    let ty = parameter.rsplit(": ").next().unwrap_or(parameter).trim();
    if ty.contains("table") {
        "matrix.identity()"
    } else if ty.contains("boolean") {
        "true"
    } else if ty.contains("integer") {
        "1"
    } else if ty.contains("number") {
        "1.5"
    } else if ty.contains("string") {
        "\"k\""
    } else {
        "1"
    }
}

#[test]
fn every_catalog_function_enforces_its_arity() {
    // U1: 全64関数は引数の個数が合わない呼び出しを Lua エラーにする。
    // 正しい個数では（モックホストの既定応答で）エラーにならない。
    for function in storm_lua_spec::component::FUNCTIONS {
        let args: Vec<&str> = if function.parameters.is_empty() {
            Vec::new()
        } else {
            function.parameters.split(", ").map(dummy_arg).collect()
        };
        let (mut component, _mock) = full_component();
        let script = format!("{}({})", function.path, args.join(", "));
        let result = component.load(script.as_bytes(), "=arity");
        assert!(
            result.is_ok(),
            "{} with {} args failed: {:?}",
            function.path,
            args.len(),
            result.err()
        );

        let (mut component, _mock) = full_component();
        let mut extra = args.clone();
        extra.push("1");
        let script = format!("{}({})", function.path, extra.join(", "));
        let error = component
            .load(script.as_bytes(), "=arity")
            .expect_err("too many args must fail");
        assert!(
            error.message.contains("Wrong number of args"),
            "{}: {}",
            function.path,
            error.message
        );

        if !args.is_empty() {
            let (mut component, _mock) = full_component();
            let script = format!("{}({})", function.path, args[..args.len() - 1].join(", "));
            let error = component
                .load(script.as_bytes(), "=arity")
                .expect_err("too few args must fail");
            assert!(
                error.message.contains("Wrong number of args"),
                "{}: {}",
                function.path,
                error.message
            );
        }
    }
}

#[test]
fn parser_semantics_follow_the_host_parse_handlers() {
    // パース期間外は false が1個だけ返る（U2）。
    let (mut component, mock) = full_component();
    mock.parse_mode.set(ParseMode::Outside);
    component
        .load(
            b"function onParse() local v, s = parser.parseBool('k', true) __v = v __s = s end",
            "=test",
        )
        .expect("load");
    component.parse().expect("parse");
    assert_eq!(global(&component, "__v"), LuaValue::Bool(false));
    assert_eq!(global(&component, "__s"), LuaValue::Nil);

    // 期間内は (値, 成功) の2個。変換は無言（key の数値は文字列化、
    // parseBool の値は toboolean、parseNumber の文字列は数値化、失敗は 0）。
    let (mut component, mock) = full_component();
    component
        .load(
            b"function onParse()\n  local n, ok = parser.parseNumber('answer', 2.5)\n  __n = n __ok = ok\n  local b = parser.parseBool(42, 1)\n  __b = b\n  __m = parser.parseNumber('k', '3.5')\n  __z = parser.parseNumber('k', 'nope')\nend",
            "=test",
        )
        .expect("load");
    component.parse().expect("parse");
    assert_eq!(global(&component, "__n"), LuaValue::Number(2.5));
    assert_eq!(global(&component, "__ok"), LuaValue::Bool(true));
    assert_eq!(global(&component, "__b"), LuaValue::Bool(true));
    assert_eq!(global(&component, "__m"), LuaValue::Number(3.5));
    assert_eq!(global(&component, "__z"), LuaValue::Number(0.0));
    assert!(
        mock.parse_calls
            .borrow()
            .iter()
            .any(|call| call == "parseBool(42,true)"),
        "{:?}",
        mock.parse_calls.borrow()
    );

    // key が無い読み取りは型の 0 と成功=false。
    let (mut component, mock) = full_component();
    mock.parse_mode.set(ParseMode::Missing);
    component
        .load(
            b"function onParse() local v, ok = parser.parseString('nope', 'd') __v = v __ok = ok end",
            "=test",
        )
        .expect("load");
    component.parse().expect("parse");
    assert_eq!(global(&component, "__v"), LuaValue::Bytes(Vec::new()));
    assert_eq!(global(&component, "__ok"), LuaValue::Bool(false));
}

#[cfg(feature = "debug")]
#[test]
fn suspended_tick_resumes_and_keeps_status() {
    use storm_lua_vm::runner::StepMode;
    let (mut component, _mock) = full_component();
    component
        .set_breakpoints(vec![("=comp".to_string(), 2)])
        .expect("breakpoints");
    component
        .load(b"function onTick(t)\n  __done = t\nend", "=comp")
        .expect("load");
    component.parse().expect("parse");
    component.add_to_simulation().expect("add");
    assert_eq!(component.tick(3).expect("tick"), RunOutcome::Suspended);
    assert_eq!(component.status(), ComponentStatus::Ok);
    assert_eq!(
        component.resume(StepMode::Continue).expect("resume"),
        RunOutcome::Completed
    );
    assert_eq!(global(&component, "__done"), LuaValue::Integer(3));
    assert_eq!(component.status(), ComponentStatus::Ok);
}
