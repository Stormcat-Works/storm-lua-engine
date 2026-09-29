/** ビークル/アドオンの明示的なプロファイル。暗黙的なクロック、トランスポート、Worker、描画コンテキストは持ちません。 */
export { ABI } from './generated.js';
export { LuaEngine, VehicleVm, loadRuntime, fromEmscripten } from './runtime.js';
export { AddonVm } from './addon.js';
export type { VehicleOptions, RuntimeInitOptions } from './runtime.js';
export type { AddonOptions, AddonEvent, MenuProperty } from './addon.js';
export type { ScriptOptions, LogRecord, LogLocation, LogHandler, HttpToken, HttpRequest } from './script.js';
export type { MapRequest, MapProvider, Rgba, ServerFunction, ServerFunctions } from './host.js';
export { EngineError } from './bridge.js';
export type { Outcome } from './bridge.js';
export type { Properties, PropertyEntry, PropertyValue } from './properties.js';
export { FrameLease } from './frame.js';
export type { DebugHandle, DebugValue, StackFrame, Variable, TableEntry, Breakpoint, StepMode } from './debug.js';
export { VEHICLE_API_CATALOG, ADDON_API_CATALOG, ADDON_EVENTS } from './catalog.js';
export { luaTable, luaText, luaField, encodeSavedata, decodeSavedata } from './values.js';
export type { LuaValue, LuaTable } from './values.js';

export type { EnvironmentProfile, HostBindings } from './environment.js';
export { bindingPaths } from './environment.js';

export { ENVIRONMENT_CATALOG } from './environment-catalog.js';

export type { SourceChunk, RequireLoader } from './source.js';
