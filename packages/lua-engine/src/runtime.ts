import { environmentWire } from './environment.js';
import { adaptModule, Bridge, EngineError, object, unsigned, type Backend, type Outcome } from './bridge.js';
import { RawIoView, type CompositeIoViews } from './raw.js';
import { FrameLease } from './frame.js';
import { decodeProperties, encodeProperties, type PropertyEntry, type Properties } from './properties.js';
import { ScriptVm, type ScriptOptions, type LogHandler } from './script.js';
import { AddonVm, type AddonOptions } from './addon.js';
import { HostDispatcher, type MapProvider } from './host.js';
import { encodeLuaValue, encodeLuaValues, type LuaValue } from './values.js';
const encoder = new TextEncoder(), decoder = new TextDecoder('utf-8',{fatal:true});
export interface VehicleOptions extends ScriptOptions {
  readonly properties?: Properties;
  readonly mapProvider?: MapProvider;
  /** Opt-in native state controls for an extended harness. Must name an unused root table. */
  readonly controlNamespace?: string;
}
/** ビークルモードは Composite I/O と描画を所有し、アドオンのサーバーAPIは持ちません。 */
export class VehicleVm extends ScriptVm {
  readonly mode = 'vehicle' as const;
  readonly #io: RawIoView;
  constructor(bridge: Bridge, handle: number, onLog: LogHandler | undefined, releaseHost: () => void) {
    super(bridge,handle,onLog,releaseHost);
    if (bridge.query('mode',handle) !== 1) throw new EngineError(4,'Handle is not a vehicle');
    this.#io = new RawIoView(bridge.backend.memory,bridge.query('io_ptr',handle),bridge.invoke('abi_version'));
  }
  get io(): CompositeIoViews { this.alive(); this.bridge.assertIdle(); return this.#io.borrow(); }
  tick(): Outcome { return this.execute(() => { this.#io.validateInputs(); return this.bridge.call('tick',this.handle); }); }
  draw(width: number, height: number): Outcome { return this.execute(() => this.bridge.call('draw',this.handle,unsigned(width,'width'),unsigned(height,'height'))); }
  /** Execute an arbitrary named global in the normal tick phase, without changing onTick. */
  callTick(name: string, args: readonly LuaValue[] = []): Outcome {
    this.alive();this.bridge.assertIdle();this.#io.validateInputs();
    return this.control('vehicle',{action:'callTick',name:callbackName(name),arguments:encodeLuaValues(args)}).outcome;
  }
  /** Execute a named draw callback with the regular screen/raster and debugger continuation. */
  callDraw(name: string, width: number, height: number, args: readonly LuaValue[] = [], margin = 0): Outcome {
    return this.control('vehicle',{action:'callDraw',name:callbackName(name),arguments:encodeLuaValues(args),width:unsigned(width,'width'),height:unsigned(height,'height'),margin:unsigned(margin,'margin')}).outcome;
  }
  /** Owned, lossless property snapshot, including changes made by native development controls. */
  properties(): PropertyEntry[] { return decodeProperties(this.control('vehicle',{action:'properties'}).data); }
  frame(): FrameLease { this.alive(); return new FrameLease(this.bridge,this.handle,this.alive); }
  setProperties(properties: Properties): void {
    this.execute(() => this.bridge.upload(encodeProperties(properties),(pointer,length) => this.bridge.call('set_properties',this.handle,pointer,length)));
  }
  /** Recreate the VM and replay all completed loads in order. Clears the require cache. */
  reset(): void { this.execute(() => this.bridge.call('reset',this.handle)); }
}
function callbackName(name: string): string {
  if(typeof name!=='string'||name.length===0||encoder.encode(name).length>1024||name.includes('\0')) throw new TypeError('Invalid callback name');
  return name;
}
function budgets(options: ScriptOptions): [number,number] {
  if (options.onLog !== undefined && typeof options.onLog !== 'function') throw new TypeError('onLog must be a function');
  if (options.devLogs !== undefined && typeof options.devLogs !== 'boolean') throw new TypeError('devLogs must be Boolean');
  if (options.devLogs && options.environment !== 'extended') throw new TypeError('devLogs/print requires the extended environment');
  return [unsigned(options.instructionBudget ?? 1_000_000,'instructionBudget'),unsigned(options.memoryBytes ?? 8*1024*1024,'memoryBytes')];
}
export class LuaEngine {
  readonly bridge: Bridge;
  constructor(backend: Backend, private readonly hosts?: HostDispatcher) {
    this.bridge = new Bridge(backend);
    if (!(this.bridge.capabilities & 2)) throw new EngineError(6,'Runtime capability is absent');
  }
  private cleanup(handle: number, key: number): void {
    try { if (handle) this.bridge.call('dispose',handle); } finally { if (key) this.hosts?.remove(key); }
  }
  /** ユーザーコードが実行される前に、プロパティ、ログ記録、およびマッププロバイダが構成されます。 */
  createVehicle(options: VehicleOptions = {}): VehicleVm {
    const [instructions,memory] = budgets(options);
    const environment = environmentWire(options.environment, options.bindings, options.requireLoader);
    if(options.controlNamespace !== undefined) {
      if(options.environment !== 'extended' || typeof options.controlNamespace !== 'string' ||
        !/^[A-Za-z_][A-Za-z_0-9]*$/.test(options.controlNamespace)) throw new TypeError('controlNamespace requires extended and a root identifier');
    }
    let key = 0, handle = 0;
    try {
      if (options.mapProvider !== undefined || options.requireLoader !== undefined || Object.keys(options.bindings?.functions ?? {}).length) {
        if (options.mapProvider !== undefined && typeof options.mapProvider !== 'function') throw new TypeError('mapProvider must be a function');
        if (!this.hosts || !(this.bridge.capabilities & 16)) throw new EngineError(6,'This runtime cannot call JS host services');
        key = this.hosts.register({...(options.requireLoader === undefined ? {} : {source: options.requireLoader}), ...(options.mapProvider === undefined ? {} : {map: options.mapProvider}), ...(options.bindings?.functions === undefined ? {} : {functions: options.bindings.functions})});
      }
      const config = encoder.encode(JSON.stringify({...environment, ...(options.controlNamespace === undefined ? {} : {controlNamespace: options.controlNamespace}), properties: JSON.parse(decoder.decode(encodeProperties(options.properties ?? {}))) as unknown}));
      handle = this.bridge.upload(config, (pointer,length) => this.bridge.query('new_vehicle',instructions,memory,key,pointer,length));
      const vm = new VehicleVm(this.bridge,handle,options.onLog,() => { if (key) this.hosts?.remove(key); });
      if (options.mapProvider !== undefined) this.bridge.call('set_map_host',handle,key);

      if (options.devLogs) vm.enableLogs();
      return vm;
    } catch (error) { try { this.cleanup(handle,key); } catch (cleanupError) { throw new AggregateError([error,cleanupError],'VM creation and cleanup both failed'); } throw error; }
  }
  /** アドオンモードは、型レベルおよび生ハンドルの境界の両方で明確に分離されています。 */
  createAddon(options: AddonOptions = {}): AddonVm {
    const [instructions,memory] = budgets(options);
    const environment = environmentWire(options.environment, options.bindings, options.requireLoader);
    if (!(this.bridge.capabilities & 8)) throw new EngineError(6,'Addon profile is absent');
    const server = {...options.server}; const names = Object.keys(server);
    if (names.length > 512 || Object.values(server).some(value => typeof value !== 'function')) throw new TypeError('Invalid server function configuration');
    let key = 0, handle = 0;
    try {
      if (names.length || options.requireLoader !== undefined || Object.keys(options.bindings?.functions ?? {}).length) {
        if (!this.hosts || !(this.bridge.capabilities & 16)) throw new EngineError(6,'This runtime cannot call JS host services');
        key = this.hosts.register({server, ...(options.requireLoader === undefined ? {} : {source: options.requireLoader}), ...(options.bindings?.functions === undefined ? {} : {functions: options.bindings.functions})});
      }
      const config = encoder.encode(JSON.stringify({...environment,newWorld:options.newWorld ?? true,properties:JSON.parse(decoder.decode(encodeProperties(options.properties ?? {}))) as unknown,savedata:options.savedata === undefined ? null : encodeLuaValue(options.savedata),server:names}));
      handle = this.bridge.upload(config,(pointer,length) => this.bridge.query('new_addon',instructions,memory,key,pointer,length));
      const vm = new AddonVm(this.bridge,handle,options.onLog,() => { if (key) this.hosts?.remove(key); });
      if (options.devLogs) vm.enableLogs();
      return vm;
    } catch (error) { try { this.cleanup(handle,key); } catch (cleanupError) { throw new AggregateError([error,cleanupError],'Addon creation and cleanup both failed'); } throw error; }
  }
}
export interface RuntimeInitOptions { readonly moduleUrl?: string | URL; readonly wasmBinary?: Uint8Array; readonly wasmUrl?: string | URL }
const initialized = new WeakMap<object,LuaEngine>();
/** 注入可能な Emscripten ファクトリ出力により、バンドラや制約のある WebView をサポートします。 */
export function fromEmscripten(module: unknown): LuaEngine {
  const record = object(module), existing = initialized.get(record);
  if (existing) return existing;
  const hosts = new HostDispatcher(); record['sleHost'] = hosts.invoke;
  const engine = new LuaEngine(adaptModule(module,'sle',true),hosts);
  initialized.set(record,engine); return engine;
}
export async function loadRuntime(options: RuntimeInitOptions = {}): Promise<LuaEngine> {
  const namespace: unknown = await import(String(options.moduleUrl ?? new URL('./wasm/storm_lua_wasm.js',import.meta.url)));
  const factory = object(namespace)['default'];
  if (typeof factory !== 'function') throw new TypeError('Runtime module has no default factory');
  const configuration: Record<string,unknown> = {};
  if (options.wasmBinary) configuration['wasmBinary'] = options.wasmBinary;
  if (options.wasmUrl) configuration['locateFile'] = (path:string,prefix:string):string => path.endsWith('.wasm') ? String(options.wasmUrl) : prefix+path;
  return fromEmscripten(await factory(configuration));
}
