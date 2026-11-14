/** 1つのEmscriptenインスタンスおよび1つのVM登録にスコープされた同期ホストサービス。 */
import { EngineError, byteArray, object, unsigned } from './bridge.js';
import type { RequireLoader } from './source.js';
import { MAP_COLORS, type MapColorKind } from './commands.js';
import { decodeLuaValues, encodeLuaValues, type LuaValue } from './values.js';
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
interface Services { readonly source?: RequireLoader; readonly server?: ServerFunctions; readonly functions?: ServerFunctions; readonly map?: MapProvider; readonly pause?: () => boolean }
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
    throw new TypeError('Unknown host service request');
  };
}
