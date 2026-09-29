/** ホストが作成したWorker/MessagePortでcompiler専用RPCを使う薄い接続層。 */
import {loadCompiler, type Compiler, type CompilerInitOptions} from './compiler.js';
import type {AnalyzeOptions, CompileOptions, LuaProject, ProjectCompileOptions} from './compiler-types.js';

/** Worker生成・終了はホストが所有します。接続時にもLua VMは作成しません。 */
export interface CompilerEndpoint extends EventTarget { postMessage(message: unknown): void }
const protocol = 'storm-lua-compiler-v1';
type Operation = 'inspectSource' | 'stripDevelopment' | 'buildLifeboat' | 'minify' | 'build' | 'analyze' | 'scanProperties' | 'passIds' | 'passMetadata';
function record(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : null;
}
function message(event: Event): Record<string, unknown> | null {
  return 'data' in event ? record(event.data) : null;
}

/** 明示的に作成されたWorkerへ接続します。disposeは接続だけを破棄します。 */
export class CompilerWorkerClient {
  #next = 1;
  #disposed = false;
  readonly #pending = new Map<number, {resolve(value: unknown): void; reject(error: Error): void}>();
  constructor(private readonly endpoint: CompilerEndpoint) {
    endpoint.addEventListener('message', this.#message);
    endpoint.addEventListener('error', this.#error);
    endpoint.addEventListener('messageerror', this.#error);
  }
  #message = (event: Event): void => {
    const data = message(event);
    if (data?.['protocol'] !== protocol || typeof data['id'] !== 'number') return;
    const pending = this.#pending.get(data['id']);
    if (!pending) return;
    this.#pending.delete(data['id']);
    if (data['ok'] === true) pending.resolve(data['value']);
    else pending.reject(new Error(typeof data['error'] === 'string' ? data['error'] : 'Invalid compiler Worker response'));
  };
  #error = (event: Event): void => {
    this.dispose(new Error('message' in event && typeof event.message === 'string' ? event.message : 'Compiler Worker failed'));
  };
  private request<T>(operation: Operation, args: unknown[]): Promise<T> {
    if (this.#disposed) return Promise.reject(new Error('Compiler Worker client is disposed'));
    const id = this.#next++;
    if (!Number.isSafeInteger(id)) return Promise.reject(new Error('Compiler request identifiers exhausted'));
    return new Promise<T>((resolve, reject) => {
      this.#pending.set(id, {resolve: value => resolve(value as T), reject});
      try { this.endpoint.postMessage({protocol, id, operation, args}); }
      catch (error) { this.#pending.delete(id); reject(error); }
    });
  }
  inspectSource(source: string): Promise<ReturnType<Compiler['inspectSource']>> { return this.request('inspectSource',[source]); }
  stripDevelopment(source: string): Promise<string> { return this.request('stripDevelopment',[source]); }
  buildLifeboat(project: LuaProject, options: ProjectCompileOptions = {}): Promise<ReturnType<Compiler['buildLifeboat']>> { return this.request('buildLifeboat',[project,options]); }
  minify(source: string, options: CompileOptions = {}): Promise<ReturnType<Compiler['minify']>> { return this.request('minify', [source, options]); }
  build(project: LuaProject, options: ProjectCompileOptions = {}): Promise<ReturnType<Compiler['build']>> { return this.request('build', [project, options]); }
  analyze(project: LuaProject, options: AnalyzeOptions = {}): Promise<ReturnType<Compiler['analyze']>> { return this.request('analyze', [project, options]); }
  scanProperties(source: string): Promise<ReturnType<Compiler['scanProperties']>> { return this.request('scanProperties', [source]); }
  passIds(): Promise<ReturnType<Compiler['passIds']>> { return this.request('passIds', []); }
  passMetadata(): Promise<ReturnType<Compiler['passMetadata']>> { return this.request('passMetadata', []); }
  /** 未完了Promiseを拒否しlistenerを解除します。Worker.terminate()は呼びません。 */
  dispose(reason = new Error('Compiler Worker client disposed')): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.endpoint.removeEventListener('message', this.#message);
    this.endpoint.removeEventListener('error', this.#error);
    this.endpoint.removeEventListener('messageerror', this.#error);
    for (const pending of this.#pending.values()) pending.reject(reason);
    this.#pending.clear();
  }
}

/** Worker側へ明示的にcompiler処理を登録します。返す関数でlistenerを解除します。 */
export function serveCompiler(endpoint: CompilerEndpoint, options: CompilerInitOptions = {}): () => void {
  let compiler: Promise<Compiler> | undefined;
  let disposed = false;
  const handler = (event: Event): void => {
    const data = message(event);
    if (data?.['protocol'] !== protocol) return;
    const id = data['id'];
    if (typeof id !== 'number' || !Number.isSafeInteger(id)) return;
    const send = (value: Record<string, unknown>): void => { if (!disposed) endpoint.postMessage({protocol, id, ...value}); };
    void (async () => {
      try {
        if (!Array.isArray(data['args'])) throw new TypeError('Invalid compiler Worker arguments');
        compiler ??= loadCompiler(options);
        const api = await compiler;
        const args = data['args'];
        let result: unknown;
        switch (data['operation']) {
          case 'inspectSource':
            if(typeof args[0] !== 'string') throw new TypeError('inspectSource requires text');
            result=api.inspectSource(args[0]); break;
          case 'stripDevelopment':
            if(typeof args[0] !== 'string') throw new TypeError('stripDevelopment requires text');
            result=api.stripDevelopment(args[0]); break;
          case 'buildLifeboat': result=api.buildLifeboat(args[0] as LuaProject,args[1] as ProjectCompileOptions); break;
          case 'minify':
            if (typeof args[0] !== 'string') throw new TypeError('minify source must be text');
            result = api.minify(args[0], args[1] as CompileOptions); break;
          case 'build': result = api.build(args[0] as LuaProject, args[1] as ProjectCompileOptions); break;
          case 'analyze': result = api.analyze(args[0] as LuaProject, args[1] as AnalyzeOptions); break;
          case 'scanProperties':
            if (typeof args[0] !== 'string') throw new TypeError('scanProperties source must be text');
            result = api.scanProperties(args[0]); break;
          case 'passIds': result = api.passIds(); break;
          case 'passMetadata': result = api.passMetadata(); break;
          default: throw new TypeError('Unknown compiler Worker operation');
        }
        send({ok: true, value: result});
      } catch (error) { send({ok: false, error: String(error instanceof Error ? error.message : error)}); }
    })();
  };
  endpoint.addEventListener('message', handler);
  return () => { disposed = true; endpoint.removeEventListener('message', handler); };
}
