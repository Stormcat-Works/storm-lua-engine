// Component Mod のホスト提供ドメイン。コンポーネントが属する車両・
// シミュレーションの状態を読む/変える関数は、Addon の server 関数と同じ原則で
// ホストが提供します（未提供を成功 stub で埋めない）。提供されたドメインの
// 関数だけが Lua 環境へ登録されます。
//
// 引数は Lua 側の並びのまま、bindings が実ゲームの変換規則（整数・数値は
// 変換失敗で 0、bool は false）で変換してから渡します。slot index や
// fluid type の範囲検証はエンジンでは行わず、ホスト実装の責任です。
use std::rc::Rc;
use storm_lua_spec::io::CompositeSignal;
use storm_lua_vm::{runner::VmError, value::LuaValue};

/// 論理スロット。定義 XML の logic input/output を整数 index で読み書きします。
pub trait LogicSlots {
    /// `component.getInputLogicSlotBool(slot)`
    fn get_input_bool(&self, slot: i64) -> Result<bool, VmError>;
    /// `component.getInputLogicSlotFloat(slot)`
    fn get_input_float(&self, slot: i64) -> Result<f64, VmError>;
    /// `component.getInputLogicSlotComposite(slot)`。値の形は
    /// [`CompositeSignal`]（32 number + 32 bool）を想定しています（未確定 U3）。
    fn get_input_composite(&self, slot: i64) -> Result<CompositeSignal, VmError>;
    /// `component.setOutputLogicSlotBool(slot, value)`
    fn set_output_bool(&self, slot: i64, value: bool) -> Result<(), VmError>;
    /// `component.setOutputLogicSlotFloat(slot, value)`
    fn set_output_float(&self, slot: i64, value: f64) -> Result<(), VmError>;
    /// `component.setOutputLogicSlotComposite(slot, value)`
    fn set_output_composite(&self, slot: i64, value: CompositeSignal) -> Result<(), VmError>;
}

/// トルク（回転）接続スロット。
pub trait TorqueSlots {
    /// `component.slotTorqueIsConnected(slot)`
    fn is_connected(&self, slot: i64) -> Result<bool, VmError>;
    /// `component.slotTorqueApplyMomentum(slot, value, dt)`
    fn apply_momentum(&self, slot: i64, value: f64, dt: f64) -> Result<(), VmError>;
    /// `component.slotTorqueCreateBridge(a, b)`。2 引数の役割は未確定で、
    /// Lua の並びのまま渡します。
    fn create_bridge(&self, first: i64, second: i64) -> Result<(), VmError>;
    /// `component.slotTorqueDestroyBridge(slot)`
    fn destroy_bridge(&self, slot: i64) -> Result<(), VmError>;
    /// `component.slotTorqueSetBridgeFactor(slot, factor)`
    fn set_bridge_factor(&self, slot: i64, factor: f64) -> Result<(), VmError>;
    /// `component.slotTorqueSetBridgeRatio(slot, ratio)`
    fn set_bridge_ratio(&self, slot: i64, ratio: f64) -> Result<(), VmError>;
}

/// 流体接続スロット。流量の解決はシミュレーション本体の仕事です。
pub trait FluidSlots {
    /// `component.slotFluidResolveFlow(...)`。引数構成は実測どおり
    /// （integer, integer, number, number, integer, integer, integer, integer）。
    /// 各引数の意味と返値（解決した流量）は未確定です（U4）。
    #[allow(clippy::too_many_arguments)]
    fn resolve_flow(
        &self,
        first: i64,
        second: i64,
        x: f64,
        y: f64,
        rest: [i64; 4],
    ) -> Result<f64, VmError>;
    /// `component.slotFluidResolveFlowToSlot(...)`。引数構成は実測どおり
    /// （integer, integer, number, number, integer, integer, integer）。
    /// 各引数の意味と返値は未確定です（U4）。
    fn resolve_flow_to_slot(
        &self,
        first: i64,
        second: i64,
        x: f64,
        y: f64,
        rest: [i64; 3],
    ) -> Result<f64, VmError>;
}

/// 電気接続スロット。charge の蓄積・係数計算はホストの電気モデル側です。
pub trait ElectricSlots {
    /// `component.slotElectricIsConnected(slot)`
    fn is_connected(&self, slot: i64) -> Result<bool, VmError>;
    /// `component.slotElectricAddCharge(slot, charge)`
    fn add_charge(&self, slot: i64, charge: f64) -> Result<(), VmError>;
    /// `component.slotElectricRemoveCharge(slot, charge)`
    fn remove_charge(&self, slot: i64, charge: f64) -> Result<(), VmError>;
    /// `component.slotElectricGetChargeFactor(slot)`
    fn get_charge_factor(&self, slot: i64) -> Result<f64, VmError>;
}

/// 効果音。実再生はホストが行います（コマンド記録型にするかはホストの選択）。
pub trait Sfx {
    /// `component.sfxPlayOnce(...)`（integer, integer, number×8）。
    fn play_once(&self, ints: [i64; 2], numbers: [f64; 8]) -> Result<(), VmError>;
    /// `component.sfxPlayLoop(...)`（integer, integer, number×8）。
    fn play_loop(&self, ints: [i64; 2], numbers: [f64; 8]) -> Result<(), VmError>;
    /// `component.sfxUpdate(...)`（integer, number×7）。
    fn update(&self, id: i64, numbers: [f64; 7]) -> Result<(), VmError>;
    /// `component.sfxStop(id)`。
    fn stop(&self, id: i64) -> Result<(), VmError>;
}

/// 流体内容（タンク等）。ストレージ（slot 0〜3 × fluid type 0〜12、各 type の
/// 量は f32）の所有もホストに置きます（エンジン側に二重の台帳を作らない）。
pub trait FluidContents {
    /// `component.fluidContentsTransferVolume(from, to, volume)`
    fn transfer_volume(&self, from: i64, to: i64, volume: f64) -> Result<(), VmError>;
    /// `component.fluidContentsTransferVolumeExceptType(from, to, volume, exceptType)`
    fn transfer_volume_except_type(
        &self,
        from: i64,
        to: i64,
        volume: f64,
        except_type: i64,
    ) -> Result<(), VmError>;
    /// `component.fluidContentsSetFluidTypeVolume(slot, fluidType, volume)`
    fn set_fluid_type_volume(&self, slot: i64, fluid_type: i64, volume: f64)
        -> Result<(), VmError>;
    /// `component.fluidContentsGetFluidTypeVolume(slot, fluidType)`
    fn get_fluid_type_volume(&self, slot: i64, fluid_type: i64) -> Result<f64, VmError>;
    /// `component.fluidContentsGetVolume(slot)`
    fn get_volume(&self, slot: i64) -> Result<f64, VmError>;
    /// `component.fluidContentsSetCapacity(slot, capacity)`
    fn set_capacity(&self, slot: i64, capacity: f64) -> Result<(), VmError>;
    /// `component.fluidContentsGetCapacity(slot)`
    fn get_capacity(&self, slot: i64) -> Result<f64, VmError>;
    /// `component.fluidContentsGetPressure(slot)`
    fn get_pressure(&self, slot: i64) -> Result<f64, VmError>;
}

/// ヒーター。加熱の実効果はホストの熱モデル側です。
pub trait Heater {
    /// `component.heaterSetSphere(x, y, z, radius, temperature)`
    fn set_sphere(&self, center: [f64; 3], radius: f64, temperature: f64) -> Result<(), VmError>;
    /// `component.heaterSetOrientedBox(...)`。matrix 引数の位置（先頭）と
    /// 残り 4 数値の意味は未確定です（U4）。
    fn set_oriented_box(&self, matrix: [f64; 16], values: [f64; 4]) -> Result<(), VmError>;
    /// `component.heaterSetTemperature(temperature)`
    fn set_temperature(&self, temperature: f64) -> Result<(), VmError>;
    /// `component.heaterSetFactor(index, factor)`
    fn set_factor(&self, index: i64, factor: f64) -> Result<(), VmError>;
}

/// 描画。メッシュ/光源の実描画はホスト（renderer はエンジン範囲外）。
pub trait Render {
    /// `component.renderMesh0(matrix)`
    fn render_mesh0(&self, matrix: [f64; 16]) -> Result<(), VmError>;
    /// `component.renderMesh1(matrix)`
    fn render_mesh1(&self, matrix: [f64; 16]) -> Result<(), VmError>;
    /// `component.renderMesh2(matrix)`
    fn render_mesh2(&self, matrix: [f64; 16]) -> Result<(), VmError>;
    /// `component.renderLight(...)`（number×7, bool×2）。
    fn render_light(&self, numbers: [f64; 7], flags: [bool; 2]) -> Result<(), VmError>;
    /// `component.renderLaser(...)`。matrix 引数の位置（先頭）と残り 2 数値の
    /// 意味は未確定です（U4）。
    fn render_laser(&self, matrix: [f64; 16], first: f64, second: f64) -> Result<(), VmError>;
}

/// 物理世界への照会・変更。
pub trait Physics {
    /// `component.physicsImpulse(x, y, z, fx, fy, fz, local)`
    fn impulse(&self, position: [f64; 3], force: [f64; 3], local: bool) -> Result<(), VmError>;
    /// `component.physicsRaycast(x, y, z, dx, dy, dz)`。返値の形は未確定のため、
    /// ホスト所有の値をそのまま Lua へ返します（U4）。
    fn raycast(&self, origin: [f64; 3], direction: [f64; 3]) -> Result<Vec<LuaValue>, VmError>;
    /// `component.physicsGetLinearVelocity()`。返値（3 数値）は想定です（U4）。
    fn get_linear_velocity(&self) -> Result<[f64; 3], VmError>;
    /// `component.physicsGetLinearVelocityAtPoint(x, y, z)`。返値は想定です（U4）。
    fn get_linear_velocity_at_point(&self, point: [f64; 3]) -> Result<[f64; 3], VmError>;
    /// `component.physicsGetAngularVelocity()`。返値（3 数値）は想定です（U4）。
    fn get_angular_velocity(&self) -> Result<[f64; 3], VmError>;
    /// `component.spawnProjectile(...)`（integer, integer, number×6）。
    /// 返値なしは想定です（U4）。
    fn spawn_projectile(&self, ints: [i64; 2], numbers: [f64; 6]) -> Result<(), VmError>;
}

/// 世界状態の照会。
pub trait EnvironmentQuery {
    /// `component.getSubmergenceFactor(...)`（number×4）。引数の意味と
    /// 返値（1 数値）は未確定・想定です（U4）。
    fn get_submergence_factor(&self, args: [f64; 4]) -> Result<f64, VmError>;
    /// `component.getWindVelocity()`。返値（3 数値）は想定です（U4）。
    fn get_wind_velocity(&self) -> Result<[f64; 3], VmError>;
}

/// 定義データへの parse ハンドラ（`parser.parseBool/parseNumber/parseString`）。
///
/// 第 1 引数の key に対応する値を、ホスト側の定義データ（型別ベクタ）へ
/// 読み書きします。方向（読む = 値が定義値で上書きされる / 書く = 値が定義へ
/// 記録される）はホスト側のパース状態で決まり、Lua からは見えません。
///
/// 戻り値の意味:
/// - `None`: パース期間外。Lua 側は `false` を 1 個だけ受け取ります。
/// - `Some((値, 成功))`: Lua 側は `(値, 成功 bool)` の 2 個を受け取ります。
///   key が無く読む方向では、値は型の 0（0.0 / false / 空文字列）で成功は false です。
pub trait ParseHandlers {
    /// `parser.parseBool(key, value)`
    fn parse_bool(&self, key: &[u8], value: bool) -> Result<Option<(bool, bool)>, VmError>;
    /// `parser.parseNumber(key, value)`。値は定義データ側の精度（f32）です。
    fn parse_number(&self, key: &[u8], value: f32) -> Result<Option<(f32, bool)>, VmError>;
    /// `parser.parseString(key, value)`
    fn parse_string(&self, key: &[u8], value: &[u8]) -> Result<Option<(Vec<u8>, bool)>, VmError>;
}

/// ホストが提供するドメイン実装の集合。`None` のドメインは登録されません。
#[derive(Clone, Default)]
pub struct ComponentProviders {
    /// 論理スロット。
    pub logic: Option<Rc<dyn LogicSlots>>,
    /// トルクスロット。
    pub torque: Option<Rc<dyn TorqueSlots>>,
    /// 流体スロット。
    pub fluid: Option<Rc<dyn FluidSlots>>,
    /// 電気スロット。
    pub electric: Option<Rc<dyn ElectricSlots>>,
    /// 効果音。
    pub sfx: Option<Rc<dyn Sfx>>,
    /// 流体内容。
    pub fluid_contents: Option<Rc<dyn FluidContents>>,
    /// ヒーター。
    pub heater: Option<Rc<dyn Heater>>,
    /// 描画。
    pub render: Option<Rc<dyn Render>>,
    /// 物理。
    pub physics: Option<Rc<dyn Physics>>,
    /// 環境照会。
    pub environment: Option<Rc<dyn EnvironmentQuery>>,
    /// parse ハンドラ。
    pub parse: Option<Rc<dyn ParseHandlers>>,
}
