//! Component Mod Lua のAPIおよびコールバックカタログ（docs/specs/component.md 参照）。
//!
//! 64関数の名前・arity・引数型は実ゲーム v1.15.26 の静的解析で確定した内容です。
//! 引数不一致は全関数でLuaエラー（`Wrong number of args`）となり、型は無言で
//! 変換されます（table必須引数と `debug.log` の型ゲートだけが例外）。
//! `phase` は既存語彙（any/tick/draw）による情報目的の記録であり、
//! phase制限の存在を主張するものではありません（実ゲームにphase制限の
//! 記述は確認されていません）。

use crate::catalog::ApiFunction;

/// Component Mod のコールバック名。ライフサイクルの正本は environment 側の
/// `COMPONENT_CALLBACKS` で、本定数はカタログ利用者向けの別名です。
pub const CALLBACKS: &[&str] = crate::environment::COMPONENT_CALLBACKS;

/// エンジンが所有するAPI契約。availability は本実装の状態を表します。
/// matrix と debug.log はエンジン実装、その他はホスト提供
/// （requires-provider: 対応するプロバイダが提供された場合だけ登録されます）。
pub const FUNCTIONS: &[ApiFunction] = &[
    // debug（共通基盤）
    ApiFunction { path: "debug.log", parameters: "message: string | number", returns: "", phase: "any", availability: "implemented", effect: "log" },
    // parser（ホストのparseハンドラ越し。返値は「値, 成功bool」の2個、
    // パース期間外は成功bool=falseの1個だけ）
    ApiFunction { path: "parser.parseBool", parameters: "key: string, value: boolean", returns: "boolean, boolean", phase: "any", availability: "requires-provider", effect: "parse-definition" },
    ApiFunction { path: "parser.parseNumber", parameters: "key: string, value: number", returns: "number, boolean", phase: "any", availability: "requires-provider", effect: "parse-definition" },
    ApiFunction { path: "parser.parseString", parameters: "key: string, value: string", returns: "string, boolean", phase: "any", availability: "requires-provider", effect: "parse-definition" },
    // matrix（VM共有モジュール。addonの集合に scale を加えた13関数）
    ApiFunction { path: "matrix.multiply", parameters: "a: table, b: table", returns: "table", phase: "any", availability: "implemented", effect: "matrix" },
    ApiFunction { path: "matrix.multiplyXYZW", parameters: "matrix: table, x: number, y: number, z: number, w: number", returns: "number, number, number, number", phase: "any", availability: "implemented", effect: "matrix" },
    ApiFunction { path: "matrix.invert", parameters: "matrix: table", returns: "table", phase: "any", availability: "implemented", effect: "matrix" },
    ApiFunction { path: "matrix.transpose", parameters: "matrix: table", returns: "table", phase: "any", availability: "implemented", effect: "matrix" },
    ApiFunction { path: "matrix.identity", parameters: "", returns: "table", phase: "any", availability: "implemented", effect: "matrix" },
    ApiFunction { path: "matrix.scale", parameters: "x: number, y: number, z: number", returns: "table", phase: "any", availability: "implemented", effect: "matrix" },
    ApiFunction { path: "matrix.rotationX", parameters: "angle: number", returns: "table", phase: "any", availability: "implemented", effect: "matrix" },
    ApiFunction { path: "matrix.rotationY", parameters: "angle: number", returns: "table", phase: "any", availability: "implemented", effect: "matrix" },
    ApiFunction { path: "matrix.rotationZ", parameters: "angle: number", returns: "table", phase: "any", availability: "implemented", effect: "matrix" },
    ApiFunction { path: "matrix.translation", parameters: "x: number, y: number, z: number", returns: "table", phase: "any", availability: "implemented", effect: "matrix" },
    ApiFunction { path: "matrix.position", parameters: "matrix: table", returns: "number, number, number", phase: "any", availability: "implemented", effect: "matrix" },
    ApiFunction { path: "matrix.distance", parameters: "a: table, b: table", returns: "number", phase: "any", availability: "implemented", effect: "matrix" },
    ApiFunction { path: "matrix.rotationToFaceXZ", parameters: "x: number, z: number", returns: "table", phase: "any", availability: "implemented", effect: "matrix" },
    // component: 論理スロット
    ApiFunction { path: "component.setOutputLogicSlotBool", parameters: "slot: integer, value: boolean", returns: "", phase: "any", availability: "requires-provider", effect: "logic-slot" },
    ApiFunction { path: "component.setOutputLogicSlotFloat", parameters: "slot: integer, value: number", returns: "", phase: "any", availability: "requires-provider", effect: "logic-slot" },
    ApiFunction { path: "component.setOutputLogicSlotComposite", parameters: "slot: integer, value: table", returns: "", phase: "any", availability: "requires-provider", effect: "logic-slot" },
    ApiFunction { path: "component.getInputLogicSlotBool", parameters: "slot: integer", returns: "boolean", phase: "any", availability: "requires-provider", effect: "logic-slot" },
    ApiFunction { path: "component.getInputLogicSlotFloat", parameters: "slot: integer", returns: "number", phase: "any", availability: "requires-provider", effect: "logic-slot" },
    ApiFunction { path: "component.getInputLogicSlotComposite", parameters: "slot: integer", returns: "table", phase: "any", availability: "requires-provider", effect: "logic-slot" },
    // component: トルクスロット
    ApiFunction { path: "component.slotTorqueIsConnected", parameters: "slot: integer", returns: "boolean", phase: "any", availability: "requires-provider", effect: "torque-slot" },
    ApiFunction { path: "component.slotTorqueApplyMomentum", parameters: "slot: integer, value: number, dt: number", returns: "", phase: "any", availability: "requires-provider", effect: "torque-slot" },
    ApiFunction { path: "component.slotTorqueCreateBridge", parameters: "integer, integer", returns: "", phase: "any", availability: "requires-provider", effect: "torque-slot" },
    ApiFunction { path: "component.slotTorqueDestroyBridge", parameters: "slot: integer", returns: "", phase: "any", availability: "requires-provider", effect: "torque-slot" },
    ApiFunction { path: "component.slotTorqueSetBridgeFactor", parameters: "integer, number", returns: "", phase: "any", availability: "requires-provider", effect: "torque-slot" },
    ApiFunction { path: "component.slotTorqueSetBridgeRatio", parameters: "integer, number", returns: "", phase: "any", availability: "requires-provider", effect: "torque-slot" },
    // component: 流体スロット
    ApiFunction { path: "component.slotFluidResolveFlow", parameters: "integer, integer, number, number, integer, integer, integer, integer", returns: "number", phase: "any", availability: "requires-provider", effect: "fluid-slot" },
    ApiFunction { path: "component.slotFluidResolveFlowToSlot", parameters: "integer, integer, number, number, integer, integer, integer", returns: "number", phase: "any", availability: "requires-provider", effect: "fluid-slot" },
    // component: 電気スロット
    ApiFunction { path: "component.slotElectricIsConnected", parameters: "slot: integer", returns: "boolean", phase: "any", availability: "requires-provider", effect: "electric-slot" },
    ApiFunction { path: "component.slotElectricAddCharge", parameters: "slot: integer, charge: number", returns: "", phase: "any", availability: "requires-provider", effect: "electric-slot" },
    ApiFunction { path: "component.slotElectricRemoveCharge", parameters: "slot: integer, charge: number", returns: "", phase: "any", availability: "requires-provider", effect: "electric-slot" },
    ApiFunction { path: "component.slotElectricGetChargeFactor", parameters: "slot: integer", returns: "number", phase: "any", availability: "requires-provider", effect: "electric-slot" },
    // component: 効果音
    ApiFunction { path: "component.sfxPlayOnce", parameters: "integer, integer, number, number, number, number, number, number, number, number", returns: "", phase: "any", availability: "requires-provider", effect: "sound" },
    ApiFunction { path: "component.sfxPlayLoop", parameters: "integer, integer, number, number, number, number, number, number, number, number", returns: "", phase: "any", availability: "requires-provider", effect: "sound" },
    ApiFunction { path: "component.sfxUpdate", parameters: "integer, number, number, number, number, number, number, number", returns: "", phase: "any", availability: "requires-provider", effect: "sound" },
    ApiFunction { path: "component.sfxStop", parameters: "integer", returns: "", phase: "any", availability: "requires-provider", effect: "sound" },
    // component: 流体内容
    ApiFunction { path: "component.fluidContentsTransferVolume", parameters: "from: integer, to: integer, volume: number", returns: "", phase: "any", availability: "requires-provider", effect: "fluid-contents" },
    ApiFunction { path: "component.fluidContentsTransferVolumeExceptType", parameters: "from: integer, to: integer, volume: number, exceptType: integer", returns: "", phase: "any", availability: "requires-provider", effect: "fluid-contents" },
    ApiFunction { path: "component.fluidContentsSetFluidTypeVolume", parameters: "slot: integer, fluidType: integer, volume: number", returns: "", phase: "any", availability: "requires-provider", effect: "fluid-contents" },
    ApiFunction { path: "component.fluidContentsGetFluidTypeVolume", parameters: "slot: integer, fluidType: integer", returns: "number", phase: "any", availability: "requires-provider", effect: "fluid-contents" },
    ApiFunction { path: "component.fluidContentsGetVolume", parameters: "slot: integer", returns: "number", phase: "any", availability: "requires-provider", effect: "fluid-contents" },
    ApiFunction { path: "component.fluidContentsSetCapacity", parameters: "slot: integer, capacity: number", returns: "", phase: "any", availability: "requires-provider", effect: "fluid-contents" },
    ApiFunction { path: "component.fluidContentsGetCapacity", parameters: "slot: integer", returns: "number", phase: "any", availability: "requires-provider", effect: "fluid-contents" },
    ApiFunction { path: "component.fluidContentsGetPressure", parameters: "slot: integer", returns: "number", phase: "any", availability: "requires-provider", effect: "fluid-contents" },
    // component: ヒーター
    ApiFunction { path: "component.heaterSetSphere", parameters: "x: number, y: number, z: number, radius: number, temperature: number", returns: "", phase: "any", availability: "requires-provider", effect: "heat" },
    ApiFunction { path: "component.heaterSetOrientedBox", parameters: "matrix: table, number, number, number, number", returns: "", phase: "any", availability: "requires-provider", effect: "heat" },
    ApiFunction { path: "component.heaterSetTemperature", parameters: "temperature: number", returns: "", phase: "any", availability: "requires-provider", effect: "heat" },
    ApiFunction { path: "component.heaterSetFactor", parameters: "integer, number", returns: "", phase: "any", availability: "requires-provider", effect: "heat" },
    // component: 描画（phase は情報目的の記録）
    ApiFunction { path: "component.renderMesh0", parameters: "matrix: table", returns: "", phase: "draw", availability: "requires-provider", effect: "render" },
    ApiFunction { path: "component.renderMesh1", parameters: "matrix: table", returns: "", phase: "draw", availability: "requires-provider", effect: "render" },
    ApiFunction { path: "component.renderMesh2", parameters: "matrix: table", returns: "", phase: "draw", availability: "requires-provider", effect: "render" },
    ApiFunction { path: "component.renderLight", parameters: "number, number, number, number, number, number, number, boolean, boolean", returns: "", phase: "draw", availability: "requires-provider", effect: "render" },
    ApiFunction { path: "component.renderLaser", parameters: "matrix: table, number, number", returns: "", phase: "draw", availability: "requires-provider", effect: "render" },
    // component: 物理
    ApiFunction { path: "component.physicsImpulse", parameters: "x: number, y: number, z: number, fx: number, fy: number, fz: number, local: boolean", returns: "", phase: "any", availability: "requires-provider", effect: "physics" },
    ApiFunction { path: "component.physicsRaycast", parameters: "x: number, y: number, z: number, dx: number, dy: number, dz: number", returns: "any", phase: "any", availability: "requires-provider", effect: "physics" },
    ApiFunction { path: "component.physicsGetLinearVelocity", parameters: "", returns: "number, number, number", phase: "any", availability: "requires-provider", effect: "physics" },
    ApiFunction { path: "component.physicsGetLinearVelocityAtPoint", parameters: "x: number, y: number, z: number", returns: "number, number, number", phase: "any", availability: "requires-provider", effect: "physics" },
    ApiFunction { path: "component.physicsGetAngularVelocity", parameters: "", returns: "number, number, number", phase: "any", availability: "requires-provider", effect: "physics" },
    ApiFunction { path: "component.spawnProjectile", parameters: "integer, integer, number, number, number, number, number, number", returns: "", phase: "any", availability: "requires-provider", effect: "physics" },
    // component: 環境照会
    ApiFunction { path: "component.getSubmergenceFactor", parameters: "number, number, number, number", returns: "number", phase: "any", availability: "requires-provider", effect: "environment-query" },
    ApiFunction { path: "component.getWindVelocity", parameters: "", returns: "number, number, number", phase: "any", availability: "requires-provider", effect: "environment-query" },
];
