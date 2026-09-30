/** Compiler-only entry point. Importing it does not instantiate any WASM module. */
import type {
  OptimizationMap, SourceInspection, AnalyzeOptions, AnalyzeResult, CompileOptions, CompileResult, LuaProject,
  PassMetadataEntry, ProjectCompileOptions, ProjectCompileResult, PropertyScanResult,
} from './compiler-types.js';
export type * from './compiler-types.js';

/** A loaded compiler. Calls do not execute Lua, create Workers, or access files. */
export interface Compiler {
  /** Reject stale/mismatched code, snapshots or map metadata. Throws on invalid data. */
  validateSourceMap(code: string, map: string): OptimizationMap;
  /** Inspect source declarations through the shared lexer/parser; does not execute Lua. */
  inspectSource(source: string): SourceInspection;
  /** Remove actual development directives, retaining byte/line positions. */
  stripDevelopment(source: string): string;
  /** Compile LB include-once sources; does not alter the ordinary build contract. */
  buildLifeboat(project: LuaProject, options?: ProjectCompileOptions): ProjectCompileResult;
  /** Optimize a single vehicle Lua source. Invalid Lua is returned as diagnostics. */
  minify(source: string, options?: CompileOptions): CompileResult;
  /** Link a logical project, optionally optimizing the single generated source. */
  build(project: LuaProject, options?: ProjectCompileOptions): ProjectCompileResult;
  /** Analyze without generating or executing a program. */
  analyze(project: LuaProject, options?: AnalyzeOptions): AnalyzeResult;
  /** Inspect statically named property reads. */
  scanProperties(source: string): PropertyScanResult;
  /** All live identifiers, independent of display-name records. */
  passIds(): string[];
  /** Display-name records for transformations that emit named reports. */
  passMetadata(): PassMetadataEntry[];
}

/** Asset selection for an explicit compiler initialization. */
export interface CompilerInitOptions {
  readonly moduleUrl?: string | URL;
  readonly wasmUrl?: string | URL;
  /** Node hosts can supply bytes read from their chosen asset location. */
  readonly wasmBinary?: Uint8Array;
}

interface CompilerModule {
  validateSourceMap(code: string, map: string): OptimizationMap;
  inspectSource(source: string): SourceInspection;
  stripDevelopment(source: string): string;
  buildLifeboat(project: LuaProject, options: ProjectCompileOptions | undefined): ProjectCompileResult;
  default(options: { module_or_path: string | URL | Uint8Array }): Promise<unknown>;
  compile(source: string, options: CompileOptions | undefined): CompileResult;
  compileProject(project: LuaProject, options: ProjectCompileOptions | undefined): ProjectCompileResult;
  analyze(project: LuaProject, options: AnalyzeOptions | undefined): AnalyzeResult;
  scanProperties(source: string): PropertyScanResult;
  passIds(): string[];
  passMetadata(): PassMetadataEntry[];
}

function checkedModule(value: Record<string, unknown>): CompilerModule {
  for (const key of ['default', 'validateSourceMap', 'inspectSource', 'stripDevelopment', 'buildLifeboat', 'compile', 'compileProject', 'analyze', 'scanProperties', 'passIds', 'passMetadata']) {
    if (typeof value[key] !== 'function') throw new TypeError(`Invalid compiler module export: ${key}`);
  }
  // The generated module is built from the matching Rust adapter; the runtime
  // check above diagnoses wrong asset selection before any call is forwarded.
  return value as unknown as CompilerModule;
}

/**
 * Load only the compiler WASM. Call from a host-owned Worker when compilation
 * must not block the UI or simulation. Engine runtime/raster assets are unused.
 * One module URL identifies one wasm-bindgen instance; use a separate Worker
 * for an independent memory/initialization lifecycle.
 */
export async function loadCompiler(options: CompilerInitOptions = {}): Promise<Compiler> {
  if (options.wasmBinary !== undefined && options.wasmUrl !== undefined) {
    throw new TypeError('Choose wasmBinary or wasmUrl, not both');
  }
  const moduleUrl = options.moduleUrl ?? new URL('./compiler-wasm/compiler.js', import.meta.url);
  const module = checkedModule(await import(String(moduleUrl)) as Record<string, unknown>);
  await module.default({ module_or_path: options.wasmBinary ?? options.wasmUrl ?? new URL('./compiler-wasm/compiler_bg.wasm', import.meta.url) });
  return Object.freeze({
    validateSourceMap: (code: string, map: string) => module.validateSourceMap(code, map),
    inspectSource: (source: string) => module.inspectSource(source),
    stripDevelopment: (source: string) => module.stripDevelopment(source),
    buildLifeboat: (project: LuaProject, settings?: ProjectCompileOptions) => module.buildLifeboat(project, settings),
    minify: (source: string, settings?: CompileOptions) => module.compile(source, settings),
    build: (project: LuaProject, settings?: ProjectCompileOptions) => module.compileProject(project, settings),
    analyze: (project: LuaProject, settings?: AnalyzeOptions) => module.analyze(project, settings),
    scanProperties: (source: string) => module.scanProperties(source),
    passIds: () => module.passIds(),
    passMetadata: () => module.passMetadata(),
  });
}
