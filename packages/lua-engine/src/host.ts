/** 1つのEmscriptenインスタンスおよび1つのVM登録にスコープされた同期ホストサービス。 */
import { EngineError, byteArray, object, unsigned } from './bridge.js';
import type { RequireLoader } from './source.js';
import { MAP_COLORS, type MapColorKind } from './commands.js';
import { decodeLuaValue, decodeLuaValues, encodeLuaValue, encodeLuaValues, type LuaTable, type LuaValue } from './values.js';
export type Rgba = readonly [number,number,number,number];
export interface MapRequest {
  readonly width: number;
  readonly height: number;
  readonly center: readonly [number,number];
  readonly zoom: number;
  readonly colors: Readonly<Partial<Record<MapColorKind,Rgba>>>;
}
export type MapProvider = (request: MapRequest) => Uint8Array;
/** 戻り値が1つの場合でも結果リストが必要です。[] は戻り値なし、[null] は nil 1つを意味します。 */
export type ServerFunction = (...args: LuaValue[]) => readonly LuaValue[];
export type ServerFunctions = Readonly<Record<string,ServerFunction>>;
interface Services { readonly source?: RequireLoader; readonly server?: ServerFunctions; readonly functions?: ServerFunctions; readonly map?: MapProvider; readonly pause?: () => boolean; readonly component?: ComponentProviders }
const POLL_CONTINUE = Uint8Array.of(0), POLL_PAUSE = Uint8Array.of(1);
const encoder = new TextEncoder(), decoder = new TextDecoder('utf-8',{fatal:true});
function finite(value: unknown, name: string): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) throw new TypeError(`Invalid ${name}`);
  return value;
}
function rgba(value: unknown): Rgba {
  const bytes = byteArray(value);
  const [r,g,b,a] = bytes;
  if (bytes.length !== 4 || r === undefined || g === undefined || b === undefined || a === undefined) throw new TypeError('Invalid RGBA');
  return [r,g,b,a];
}
export function requireSynchronous(value: unknown): void {
  if (typeof value === 'object' && value !== null && 'then' in value && typeof value.then === 'function') {
    // 呼び出し元は同期契約違反エラーを受け取ります。リジェクトされたPromiseを監視するのは
    // 二重の未処理エラーを防ぐためだけであり、その遅延された値がLuaの結果として使われることはありません。
    void Promise.resolve(value).catch(() => undefined);
    throw new TypeError('Host services must be synchronous; Promise results are unsupported');
  }
}
/** Component の Composite 信号。Lua 表現（number_values / bool_values のサブテーブル）と対の形です。 */
export interface CompositeSignal { readonly numbers: readonly number[]; readonly booleans: readonly boolean[] }
/** 定義 XML の logic input/output を整数 index で読み書きします。 */
export interface ComponentLogicSlots {
  getInputLogicSlotBool(slot: bigint): boolean;
  getInputLogicSlotFloat(slot: bigint): number;
  getInputLogicSlotComposite(slot: bigint): CompositeSignal;
  setOutputLogicSlotBool(slot: bigint, value: boolean): void;
  setOutputLogicSlotFloat(slot: bigint, value: number): void;
  setOutputLogicSlotComposite(slot: bigint, value: CompositeSignal): void;
}
/** トルク（回転）接続スロット。 */
export interface ComponentTorqueSlots {
  slotTorqueIsConnected(slot: bigint): boolean;
  slotTorqueApplyMomentum(slot: bigint, value: number, dt: number): void;
  slotTorqueCreateBridge(first: bigint, second: bigint): void;
  slotTorqueDestroyBridge(slot: bigint): void;
  slotTorqueSetBridgeFactor(slot: bigint, factor: number): void;
  slotTorqueSetBridgeRatio(slot: bigint, ratio: number): void;
}
/** 流体接続スロット。流量の解決はホストのシミュレーション側です。 */
export interface ComponentFluidSlots {
  slotFluidResolveFlow(first: bigint, second: bigint, x: number, y: number, a: bigint, b: bigint, c: bigint, d: bigint): number;
  slotFluidResolveFlowToSlot(first: bigint, second: bigint, x: number, y: number, a: bigint, b: bigint, c: bigint): number;
}
/** 電気接続スロット。 */
export interface ComponentElectricSlots {
  slotElectricIsConnected(slot: bigint): boolean;
  slotElectricAddCharge(slot: bigint, charge: number): void;
  slotElectricRemoveCharge(slot: bigint, charge: number): void;
  slotElectricGetChargeFactor(slot: bigint): number;
}
/** 効果音。実再生はホストが行います。 */
export interface ComponentSfx {
  sfxPlayOnce(first: bigint, second: bigint, a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number): void;
  sfxPlayLoop(first: bigint, second: bigint, a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number): void;
  sfxUpdate(id: bigint, a: number, b: number, c: number, d: number, e: number, f: number, g: number): void;
  sfxStop(id: bigint): void;
}
/** 流体内容（タンク等）。ストレージの所有もホストに置きます。 */
export interface ComponentFluidContents {
  fluidContentsTransferVolume(from: bigint, to: bigint, volume: number): void;
  fluidContentsTransferVolumeExceptType(from: bigint, to: bigint, volume: number, exceptType: bigint): void;
  fluidContentsSetFluidTypeVolume(slot: bigint, fluidType: bigint, volume: number): void;
  fluidContentsGetFluidTypeVolume(slot: bigint, fluidType: bigint): number;
  fluidContentsGetVolume(slot: bigint): number;
  fluidContentsSetCapacity(slot: bigint, capacity: number): void;
  fluidContentsGetCapacity(slot: bigint): number;
  fluidContentsGetPressure(slot: bigint): number;
}
/** ヒーター。matrix はキー 1〜16 に対応する 16 数値です。 */
export interface ComponentHeater {
  heaterSetSphere(x: number, y: number, z: number, radius: number, temperature: number): void;
  heaterSetOrientedBox(matrix: readonly number[], a: number, b: number, c: number, d: number): void;
  heaterSetTemperature(temperature: number): void;
  heaterSetFactor(index: bigint, factor: number): void;
}
/** 描画。メッシュ/光源の実描画はホスト（renderer はエンジン範囲外）。 */
export interface ComponentRender {
  renderMesh0(matrix: readonly number[]): void;
  renderMesh1(matrix: readonly number[]): void;
  renderMesh2(matrix: readonly number[]): void;
  renderLight(a: number, b: number, c: number, d: number, e: number, f: number, g: number, first: boolean, second: boolean): void;
  renderLaser(matrix: readonly number[], first: number, second: number): void;
}
/** 物理世界への照会・変更。physicsRaycast の返値の形は未確定のため、ホスト所有の値をそのまま返します。 */
export interface ComponentPhysics {
  physicsImpulse(x: number, y: number, z: number, fx: number, fy: number, fz: number, local: boolean): void;
  physicsRaycast(x: number, y: number, z: number, dx: number, dy: number, dz: number): readonly LuaValue[];
  physicsGetLinearVelocity(): readonly number[];
  physicsGetLinearVelocityAtPoint(x: number, y: number, z: number): readonly number[];
  physicsGetAngularVelocity(): readonly number[];
  spawnProjectile(first: bigint, second: bigint, a: number, b: number, c: number, d: number, e: number, f: number): void;
}
/** 世界状態の照会。 */
export interface ComponentEnvironmentQuery {
  getSubmergenceFactor(a: number, b: number, c: number, d: number): number;
  getWindVelocity(): readonly number[];
}
/**
 * 定義データへの parse ハンドラ。方向（読む/書く）は Rust 側のパース状態が決め、
 * ハンドラは key と現在値を受けて（値, 成功）の組を返します。
 * パース期間外は null を返します（Lua へは false が 1 個だけ返ります）。
 */
export interface ComponentParseHandlers {
  parseBool(key: Uint8Array, value: boolean): readonly [boolean, boolean] | null;
  parseNumber(key: Uint8Array, value: number): readonly [number, boolean] | null;
  parseString(key: Uint8Array, value: Uint8Array): readonly [Uint8Array, boolean] | null;
}
/** ホストが提供するドメイン実装の集合。提供されたドメインの関数だけが Lua 環境へ登録されます。 */
export interface ComponentProviders {
  readonly logic?: ComponentLogicSlots;
  readonly torque?: ComponentTorqueSlots;
  readonly fluid?: ComponentFluidSlots;
  readonly electric?: ComponentElectricSlots;
  readonly sfx?: ComponentSfx;
  readonly fluidContents?: ComponentFluidContents;
  readonly heater?: ComponentHeater;
  readonly render?: ComponentRender;
  readonly physics?: ComponentPhysics;
  readonly environment?: ComponentEnvironmentQuery;
  readonly parse?: ComponentParseHandlers;
}
type ComponentArgKind = 'int' | 'number' | 'bool' | 'composite' | 'matrix';
type ComponentReturnKind = 'void' | 'bool' | 'number' | 'composite' | 'vec3' | 'values';
interface ComponentSignature { readonly args: readonly ComponentArgKind[]; readonly returns: ComponentReturnKind }
const num = (count: number): ComponentArgKind[] => Array.from({length: count}, () => 'number' as const);
const int = (count: number): ComponentArgKind[] => Array.from({length: count}, () => 'int' as const);
/** Rust 側 trait 群と対の固定署名表。ドメイン名と Lua 側の関数名で引きます。 */
const COMPONENT_SIGNATURES: Readonly<Record<string, Readonly<Record<string, ComponentSignature>>>> = {
  logic: {
    getInputLogicSlotBool: {args: ['int'], returns: 'bool'},
    getInputLogicSlotFloat: {args: ['int'], returns: 'number'},
    getInputLogicSlotComposite: {args: ['int'], returns: 'composite'},
    setOutputLogicSlotBool: {args: ['int', 'bool'], returns: 'void'},
    setOutputLogicSlotFloat: {args: ['int', 'number'], returns: 'void'},
    setOutputLogicSlotComposite: {args: ['int', 'composite'], returns: 'void'},
  },
  torque: {
    slotTorqueIsConnected: {args: ['int'], returns: 'bool'},
    slotTorqueApplyMomentum: {args: ['int', 'number', 'number'], returns: 'void'},
    slotTorqueCreateBridge: {args: ['int', 'int'], returns: 'void'},
    slotTorqueDestroyBridge: {args: ['int'], returns: 'void'},
    slotTorqueSetBridgeFactor: {args: ['int', 'number'], returns: 'void'},
    slotTorqueSetBridgeRatio: {args: ['int', 'number'], returns: 'void'},
  },
  fluid: {
    slotFluidResolveFlow: {args: ['int', 'int', ...num(2), ...int(4)], returns: 'number'},
    slotFluidResolveFlowToSlot: {args: ['int', 'int', ...num(2), ...int(3)], returns: 'number'},
  },
  electric: {
    slotElectricIsConnected: {args: ['int'], returns: 'bool'},
    slotElectricAddCharge: {args: ['int', 'number'], returns: 'void'},
    slotElectricRemoveCharge: {args: ['int', 'number'], returns: 'void'},
    slotElectricGetChargeFactor: {args: ['int'], returns: 'number'},
  },
  sfx: {
    sfxPlayOnce: {args: [...int(2), ...num(8)], returns: 'void'},
    sfxPlayLoop: {args: [...int(2), ...num(8)], returns: 'void'},
    sfxUpdate: {args: ['int', ...num(7)], returns: 'void'},
    sfxStop: {args: ['int'], returns: 'void'},
  },
  fluidContents: {
    fluidContentsTransferVolume: {args: ['int', 'int', 'number'], returns: 'void'},
    fluidContentsTransferVolumeExceptType: {args: ['int', 'int', 'number', 'int'], returns: 'void'},
    fluidContentsSetFluidTypeVolume: {args: ['int', 'int', 'number'], returns: 'void'},
    fluidContentsGetFluidTypeVolume: {args: ['int', 'int'], returns: 'number'},
    fluidContentsGetVolume: {args: ['int'], returns: 'number'},
    fluidContentsSetCapacity: {args: ['int', 'number'], returns: 'void'},
    fluidContentsGetCapacity: {args: ['int'], returns: 'number'},
    fluidContentsGetPressure: {args: ['int'], returns: 'number'},
  },
  heater: {
    heaterSetSphere: {args: num(5), returns: 'void'},
    heaterSetOrientedBox: {args: ['matrix', ...num(4)], returns: 'void'},
    heaterSetTemperature: {args: ['number'], returns: 'void'},
    heaterSetFactor: {args: ['int', 'number'], returns: 'void'},
  },
  render: {
    renderMesh0: {args: ['matrix'], returns: 'void'},
    renderMesh1: {args: ['matrix'], returns: 'void'},
    renderMesh2: {args: ['matrix'], returns: 'void'},
    renderLight: {args: [...num(7), 'bool', 'bool'], returns: 'void'},
    renderLaser: {args: ['matrix', 'number', 'number'], returns: 'void'},
  },
  physics: {
    physicsImpulse: {args: [...num(6), 'bool'], returns: 'void'},
    physicsRaycast: {args: num(6), returns: 'values'},
    physicsGetLinearVelocity: {args: [], returns: 'vec3'},
    physicsGetLinearVelocityAtPoint: {args: num(3), returns: 'vec3'},
    physicsGetAngularVelocity: {args: [], returns: 'vec3'},
    spawnProjectile: {args: [...int(2), ...num(6)], returns: 'void'},
  },
  environment: {
    getSubmergenceFactor: {args: num(4), returns: 'number'},
    getWindVelocity: {args: [], returns: 'vec3'},
  },
};
function isTable(value: LuaValue): value is LuaTable {
  return typeof value === 'object' && value !== null && !(value instanceof Uint8Array) && value.kind === 'table';
}
function indexField(table: LuaTable, index: number): LuaValue | undefined {
  return table.entries.find(([key]) => key === BigInt(index) || (typeof key === 'number' && Number.isInteger(key) && key === index))?.[1];
}
function fieldByName(table: LuaTable, name: string): LuaValue | undefined {
  const bytes = encoder.encode(name);
  return table.entries.find(([key]) => key instanceof Uint8Array && key.length === bytes.length && key.every((b, i) => b === bytes[i]) || key === name)?.[1];
}
function compositeFromLua(value: LuaValue): CompositeSignal {
  if (!isTable(value)) throw new TypeError('Component composite must be a table');
  const numbers: number[] = [], booleans: boolean[] = [];
  const numberTable = fieldByName(value, 'number_values'), boolTable = fieldByName(value, 'bool_values');
  for (let index = 1; index <= 32; index++) {
    const element = numberTable !== undefined && isTable(numberTable) ? indexField(numberTable, index) : undefined;
    numbers.push(typeof element === 'number' ? element : typeof element === 'bigint' ? Number(element) : 0);
    const flag = boolTable !== undefined && isTable(boolTable) ? indexField(boolTable, index) : undefined;
    booleans.push(flag !== undefined && flag !== null && flag !== false);
  }
  return {numbers, booleans};
}
function compositeToLua(signal: CompositeSignal): LuaTable {
  if (signal.numbers.length !== 32 || signal.booleans.length !== 32) throw new TypeError('CompositeSignal requires exactly 32 numbers and 32 booleans');
  const entries = (values: readonly (number | boolean)[]): (readonly [LuaValue, LuaValue])[] => values.map((value, index) => [BigInt(index + 1), value] as const);
  return {kind: 'table', entries: [
    [encoder.encode('number_values'), {kind: 'table', entries: entries(signal.numbers)}],
    [encoder.encode('bool_values'), {kind: 'table', entries: entries(signal.booleans)}],
  ]};
}
function matrixFromLua(value: LuaValue): number[] {
  if (!isTable(value)) throw new TypeError('Component matrix must be a table');
  const matrix: number[] = [];
  for (let index = 1; index <= 16; index++) {
    const element = indexField(value, index);
    if (typeof element === 'number') matrix.push(element);
    else if (typeof element === 'bigint') matrix.push(Number(element));
    else throw new TypeError('Component matrix requires 16 numeric entries');
  }
  return matrix;
}
function matrixToLua(matrix: readonly number[]): LuaTable {
  if (matrix.length !== 16) throw new TypeError('Component matrix requires exactly 16 numbers');
  return {kind: 'table', entries: matrix.map((value, index) => [BigInt(index + 1), value] as const)};
}
function componentArgument(kind: ComponentArgKind, value: LuaValue): unknown {
  switch (kind) {
    case 'int': if (typeof value === 'bigint') return value; if (typeof value === 'number' && Number.isInteger(value)) return BigInt(value); break;
    case 'number': if (typeof value === 'number') return value; if (typeof value === 'bigint') return Number(value); break;
    case 'bool': if (typeof value === 'boolean') return value; break;
    case 'composite': return compositeFromLua(value);
    case 'matrix': return matrixFromLua(value);
  }
  throw new TypeError(`Invalid component argument for ${kind}`);
}
function componentResults(kind: ComponentReturnKind, result: unknown): LuaValue[] {
  switch (kind) {
    case 'void': return [];
    case 'bool': if (typeof result === 'boolean') return [result]; break;
    case 'number': if (typeof result === 'number') return [result]; break;
    case 'composite': return [compositeToLua(result as CompositeSignal)];
    case 'vec3': {
      const values = result as readonly number[];
      if (Array.isArray(values) && values.length === 3 && values.every(value => typeof value === 'number')) return [...values];
      break;
    }
    case 'values': if (Array.isArray(result)) return result as LuaValue[];
  }
  throw new TypeError(`Invalid component result for ${kind}`);
}
export class HostDispatcher {
  #next = 1;
  readonly #services = new Map<number,Services>();
  register(services: Services): number {
    if (this.#next > 0xffff_ffff) throw new EngineError(2,'Host service identities exhausted');
    const key = this.#next++;
    this.#services.set(key,services); return key;
  }
  remove(key: number): void { this.#services.delete(key); }
  invoke = (key: number, bytes: Uint8Array): Uint8Array => {
    const service = this.#services.get(key);
    if (!service) throw new EngineError(6,'No host services registered for this VM');
    const request = object(JSON.parse(decoder.decode(bytes)) as unknown);
    if (request['kind'] === 'pausePoll') {
      if (!service.pause) throw new EngineError(6,'No pause poll is configured');
      const pause = service.pause();
      if (typeof pause !== 'boolean') { requireSynchronous(pause); throw new TypeError('pausePoll must synchronously return a Boolean'); }
      return pause ? POLL_PAUSE : POLL_CONTINUE;
    }
    if (request['kind'] === 'source') {
      if (!service.source || typeof request['name'] !== 'string') throw new EngineError(6, 'Source provider is not configured');
      const result = service.source(request['name']);
      requireSynchronous(result);
      const chunk = object(result);
      const name = chunk['name'], raw = chunk['source'];
      if (typeof name !== 'string' || !name.length || encoder.encode(name).length > 1024 || name.includes('\0')) throw new TypeError('Invalid source chunk name');
      if (typeof raw !== 'string' && !(raw instanceof Uint8Array)) throw new TypeError('Source provider must return text or Uint8Array');
      const source = typeof raw === 'string' ? encoder.encode(raw) : raw;
      if (source.length > 1024 * 1024) throw new EngineError(2, 'Lua source exceeds 1 MiB');
      return encoder.encode(JSON.stringify({name, source: Array.from(source)}));
    }
    if (request['kind'] === 'server' || request['kind'] === 'binding') {
      const name = request['name'];
      const functions = request['kind'] === 'server' ? service.server : service.functions;
      if (typeof name !== 'string' || !functions || !Object.hasOwn(functions,name)) throw new EngineError(6,'Server function is not implemented by this host');
      const callback = functions[name];
      if (typeof callback !== 'function') throw new TypeError('Invalid server callback');
      const results = callback(...decodeLuaValues(request['args']));
      requireSynchronous(results);
      return encoder.encode(JSON.stringify(encodeLuaValues(results)));
    }
    if (request['kind'] === 'map') {
      if (!service.map) throw new EngineError(6,'No host map provider');
      const width = unsigned(finite(request['width'],'width'),'width'), height = unsigned(finite(request['height'],'height'),'height');
      const center = request['center'];
      if (!Array.isArray(center) || center.length !== 2) throw new TypeError('Invalid map center');
      const colors = request['colors'];
      if (!Array.isArray(colors) || colors.length !== 8) throw new TypeError('Invalid map palette');
      const palette: Partial<Record<MapColorKind,Rgba>> = {};
      MAP_COLORS.forEach((kind,index) => { if (colors[index] !== null) palette[kind] = rgba(colors[index]); });
      const pixels = service.map({width,height,center:[finite(center[0],'center X'),finite(center[1],'center Z')],zoom:finite(request['zoom'],'zoom'),colors:palette});
      requireSynchronous(pixels);
      if (!(pixels instanceof Uint8Array) || pixels.length !== width*height*4) throw new TypeError('Map provider must synchronously return exactly width*height*4 RGBA bytes');
      return pixels;
    }
    if (request['kind'] === 'component') {
      const domain = request['domain'], name = request['name'];
      const providers = service.component;
      if (typeof domain !== 'string' || typeof name !== 'string' || !providers || !Object.hasOwn(providers, domain)) throw new EngineError(6, 'Component provider is not implemented by this host');
      const signature = COMPONENT_SIGNATURES[domain]?.[name];
      const provider = providers[domain as keyof ComponentProviders];
      if (!signature || typeof provider !== 'object' || provider === null) throw new EngineError(6, 'Component provider is not implemented by this host');
      const method = (provider as unknown as Record<string, unknown>)[name];
      if (typeof method !== 'function') throw new EngineError(6, 'Component provider is not implemented by this host');
      const args = decodeLuaValues(request['args']);
      if (args.length !== signature.args.length) throw new TypeError('Invalid component argument count');
      const typed = signature.args.map((kind, index) => componentArgument(kind, args[index] as LuaValue));
      const result = (method as (...values: unknown[]) => unknown).apply(provider, typed);
      requireSynchronous(result);
      return encoder.encode(JSON.stringify(encodeLuaValues(componentResults(signature.returns, result))));
    }
    if (request['kind'] === 'parse') {
      const handlers = service.component?.parse;
      const name = request['name'];
      if (!handlers) throw new EngineError(6, 'Parse handler is not implemented by this host');
      const key = decodeLuaValue(request['key']);
      if (!(key instanceof Uint8Array)) throw new TypeError('Parse key must be bytes');
      const value = decodeLuaValue(request['value']);
      let result: readonly [LuaValue, boolean] | null;
      if (name === 'parseBool') {
        if (typeof value !== 'boolean') throw new TypeError('parseBool value must be Boolean');
        result = handlers.parseBool(key, value);
      } else if (name === 'parseNumber') {
        if (typeof value !== 'number' && typeof value !== 'bigint') throw new TypeError('parseNumber value must be a number');
        result = handlers.parseNumber(key, Number(value));
      } else if (name === 'parseString') {
        if (!(value instanceof Uint8Array)) throw new TypeError('parseString value must be bytes');
        result = handlers.parseString(key, value);
      } else throw new EngineError(6, 'Parse handler is not implemented by this host');
      requireSynchronous(result);
      // null はパース期間外を表し、Rust 側が Lua へ false 1 個へ変換します。
      if (result === null) return encoder.encode('null');
      if (!Array.isArray(result) || result.length !== 2 || typeof result[1] !== 'boolean') throw new TypeError('Parse handler must return a value and a success flag, or null');
      return encoder.encode(JSON.stringify([encodeLuaValue(result[0]), encodeLuaValue(result[1])]));
    }
    throw new TypeError('Unknown host service request');
  };
}
