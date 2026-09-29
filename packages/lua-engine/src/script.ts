/** 両方のLuaプロファイルで共有されるクライアントライフサイクル、デバッガ、および明示的なホスト配信ルート。 */
import { Bridge, EngineError, byteArray, object, unsigned, type Outcome } from './bridge.js';
import { decodeDebugValue, decodeStack, decodeVariables, type DebugHandle, type DebugValue, type StackFrame, type Variable, type TableEntry, type Breakpoint, type StepMode } from './debug.js';
/** Execution-time source identity; never an instruction to fetch a filesystem path. */
export interface LogLocation { readonly chunk: string; readonly line: number }
export interface LogRecord {
  readonly source: 'print' | 'debug.log';
  readonly bytes: Uint8Array;
  /** Absent when the runtime cannot identify an active Lua caller (or an older runtime). */
  readonly location?: LogLocation;
}
export type LogHandler = (record: LogRecord) => void;
export interface HttpToken { readonly generation: bigint; readonly id: number }
export interface HttpRequest { readonly token: HttpToken; readonly port: number; readonly request: Uint8Array }
export interface ScriptOptions {
  readonly environment?: import('./environment.js').EnvironmentProfile;
  readonly bindings?: import('./environment.js').HostBindings;
  /** Explicit include-once development loader. Requires extended; independent of file access. */
  readonly requireLoader?: import('./source.js').RequireLoader;
  readonly instructionBudget?: number;
  readonly memoryBytes?: number;
  readonly devLogs?: boolean;
  /** 中断/失敗を含むLua復帰後のログ配送先。設定してもprintは有効化しません。 */
  readonly onLog?: LogHandler;
}
import { requireSynchronous } from './host.js';
const encoder = new TextEncoder();
function tokenWire(token: HttpToken): {generation:string;id:number} {
  if (typeof token.generation !== 'bigint' || token.generation <= 0n || token.generation > (1n<<64n)-1n) throw new RangeError('Invalid HTTP generation');
  return {generation:token.generation.toString(),id:unsigned(token.id,'request id')};
}
/** タイマーやトランスポートは作成されません。ホストがすべてのエントリポイントを明示的に駆動します。 */
export abstract class ScriptVm {
  abstract readonly mode: 'vehicle' | 'addon';
  #disposed = false;
  #busy = false;
  constructor(readonly bridge: Bridge, readonly handle: number, private readonly onLog: LogHandler | undefined, private readonly releaseHost: () => void) {}
  protected alive = (): void => { if (this.#disposed) throw new EngineError(3,'Lua VM is disposed'); };
  protected execute<T>(action: () => T, deliverLogs = true): T {
    this.alive();
    if (this.#busy) throw new EngineError(5,'Reentrant VM operation');
    this.#busy = true;
    try {
      let result: {value:T} | {error:unknown};
      try { result = {value:action()}; } catch (error) { result = {error}; }
      if (deliverLogs && this.onLog && !this.#disposed) {
        try { this.deliver(this.onLog); }
        catch (error) { if ('error' in result) throw new AggregateError([result.error,error],'Lua operation and log delivery both failed'); throw error; }
      }
      if ('error' in result) throw result.error;
      return result.value;
    } finally { this.#busy = false; }
  }
  private records(): LogRecord[] {
    this.bridge.call('drain_log_records',this.handle);
    const data = this.bridge.response();
    if (!Array.isArray(data)) throw new TypeError('Invalid log response');
    return data.map(value => {
      const record = object(value); const source = record['source'];
      if (source !== 'print' && source !== 'debug.log') throw new TypeError('Invalid log source');
      const rawLocation = record['location'];
      let location: LogLocation | undefined;
      if (rawLocation !== undefined && rawLocation !== null) {
        const raw = object(rawLocation);
        if (typeof raw['chunk'] !== 'string') throw new TypeError('Invalid log source chunk');
        if (typeof raw['line'] !== 'number') throw new TypeError('Invalid log source line');
        const line = unsigned(raw['line'], 'log source line');
        if (line === 0) throw new TypeError('Log source line must be positive');
        location = {chunk: raw['chunk'], line};
      }
      return {source,bytes:byteArray(record['bytes']),...(location === undefined ? {} : {location})};
    });
  }
  private deliver(sink: LogHandler): number {
    const records = this.records(); const errors: unknown[] = [];
    for (const record of records) { try { requireSynchronous(sink(record)); } catch (error) { errors.push(error); } }
    if (errors.length) throw new AggregateError(errors,'Log delivery failed; all drained records were attempted once');
    return records.length;
  }
  /** Vehicle appends a named chunk; Addon accepts only its initial entry. */
  load(source: string | Uint8Array, name = '=script'): Outcome {
    return this.execute(() => {
      const code = typeof source === 'string' ? encoder.encode(source) : source;
      return this.bridge.upload(code,(pointer,length) => this.bridge.upload(encoder.encode(name),(namePtr,nameLen) => this.bridge.call('load',this.handle,pointer,length,namePtr,nameLen)));
    });
  }
  enableLogs(): void { this.execute(() => this.bridge.call('enable_logs',this.handle)); }
  drainLogRecords(): LogRecord[] { return this.execute(() => this.records(),false); }
  drainLogs(): Uint8Array[] { return this.drainLogRecords().map(record => record.bytes); }
  /** onLog に代わる明示的な配信手段。コンソール出力やテキストデコードが暗黙的に選択されることはありません。 */
  flushLogs(sink: LogHandler): number { return this.execute(() => this.deliver(sink),false); }
  dispose(): void {
    if (this.#disposed) return;
    this.execute(() => { this.bridge.call('dispose',this.handle); this.#disposed = true; this.releaseHost(); },false);
  }
  protected control(name: string, request: Record<string,unknown>): {outcome:Outcome;data:unknown} {
    return this.execute(() => this.bridge.upload(encoder.encode(JSON.stringify(request)),(pointer,length) => {
      const outcome = this.bridge.call(name,this.handle,pointer,length);
      return {outcome,data:this.bridge.response()};
    }));
  }
  drainHttpRequests(): HttpRequest[] {
    const data = this.control('http',{action:'drain'}).data;
    if (!Array.isArray(data)) throw new TypeError('Invalid HTTP request list');
    return data.map(value => {
      const request = object(value), token = object(request['token']);
      const generation = token['generation'];
      if (typeof generation !== 'string' || !/^\d+$/.test(generation)) throw new TypeError('Invalid HTTP generation');
      const id = unsigned(Number(token['id']),'request id');
      return {token:{generation:BigInt(generation),id},port:unsigned(Number(request['port']),'port'),request:byteArray(request['request'])};
    });
  }
  httpReply(token: HttpToken, reply: string | Uint8Array): Outcome {
    const bytes = typeof reply === 'string' ? encoder.encode(reply) : reply;
    if (bytes.length > 1024*1024) throw new RangeError('HTTP reply exceeds 1 MiB');
    return this.control('http',{action:'reply',token:tokenWire(token),reply:Array.from(bytes)}).outcome;
  }
  cancelHttp(token: HttpToken): void { this.control('http',{action:'cancel',token:tokenWire(token)}); }
  private debug(request: Record<string,unknown>): {outcome:Outcome;data:unknown} {
    if (!(this.bridge.capabilities & 4)) throw new EngineError(6,'Debugger is not included in this module');
    return this.control('debug',request);
  }
  setBreakpoints(points: readonly Breakpoint[]): void { this.debug({action:'breakpoints',points}); }
  resume(mode: StepMode = 'continue'): Outcome { return this.debug({action:'resume',mode}).outcome; }
  stack(): StackFrame[] { return decodeStack(this.debug({action:'stack'}).data); }
  locals(level = 0): Variable[] { return decodeVariables(this.debug({action:'locals',level:unsigned(level,'level')}).data); }
  upvalues(level = 0): Variable[] { return decodeVariables(this.debug({action:'upvalues',level:unsigned(level,'level')}).data); }
  evaluateWatch(expression: string, level = 0): DebugValue { return decodeDebugValue(this.debug({action:'watch',level:unsigned(level,'level'),expression}).data); }
  expandTable(handle: DebugHandle, start = 0, limit = 64): TableEntry[] {
    const wire = {vmId:handle.vmId.toString(),pauseEpoch:handle.pauseEpoch.toString(),slot:unsigned(handle.slot,'slot')};
    const data = this.debug({action:'table',handle:wire,start:unsigned(start,'start'),limit:unsigned(limit,'limit')}).data;
    if (!Array.isArray(data)) throw new TypeError('Invalid table response');
    return data.map(value => { const entry=object(value); return {key:decodeDebugValue(entry['key']),value:decodeDebugValue(entry['value'])}; });
  }
}
