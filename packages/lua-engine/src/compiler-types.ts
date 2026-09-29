import type { EnvironmentProfile } from './environment.js';
/** Compiler data contracts. Compilation is synchronous after WASM initialization; hosts own scheduling. */
export type CompilerTarget = 'vehicle';
export type CompileMode = 'safe' | 'smallest';
export type NumericMode = 'tolerant' | 'exact';
export type SearchMode = 'exhaustive' | 'fast';
export type SearchResultMode = SearchMode | 'satisficing' | 'lexical';

export interface NumericTolerance {
  abs: number;
  rel: number;
}
export interface PropertyConfig {
  mode?: 'runtime' | 'hardcode';
  numbers?: Record<string, number>;
  bools?: Record<string, boolean>;
  texts?: Record<string, string>;
}
export interface CompileOptions {
  environment?: EnvironmentProfile;
  hostBindings?: string[];
  /** Vehicle only in this release. Addon compilation is explicitly unsupported. */
  target?: CompilerTarget;
  mode?: CompileMode;
  property?: PropertyConfig;
  zeroCostNewlines?: boolean;
  passToggles?: Record<string, boolean>;
  numericTolerance?: NumericTolerance;
  numericMode?: NumericMode;
  searchMode?: SearchMode;
  searchBeamWidth?: number;
  /** Activates OBJ-2 satisficing search when present. */
  targetSize?: number;
}
export interface PassRecord {
  name: string;
  saved?: number;
  detail?: string;
  elapsedMs?: number;
}

/**
 * Diagnostic schema shared by `compile()`, `compileProject()`, and `analyze()`
 * (design doc §7). `code` is a stable kebab-case identifier: once published its
 * meaning never changes (codes may be retired, never reused for something else).
 * `message` is always English; localize in the host app by keying off `code`.
 */
export interface Diagnostic {
  code: string;
  severity: 'error' | 'warning' | 'info';
  message: string;
  /** Module key this diagnostic applies to. Omitted for project-wide diagnostics (e.g. entry-not-found). */
  module?: string;
  /** 1-based position. Omitted means "applies to the whole module/project". */
  range?: DiagnosticRange;
}
export interface DiagnosticRange {
  line: number;
  col: number;
  endLine?: number;
  endCol?: number;
}

export interface CandidateSize {
  structural: string;
  layout: string;
  size: number;
}
export interface SearchStats {
  mode: SearchResultMode;
  beamWidth: number | null;
  candidates: number;
  attempted: number;
  parseRejected: number;
  semanticRejected: number;
  coreVariants: number;
  structuralVariants: number;
  structuralExplored: number;
  structural: string;
  layout: string;
  candidateSizes: CandidateSize[];
  targetSize?: number;
  targetMet?: boolean;
  stoppedEarly?: boolean;
  checkpoints?: number;
  stage?: string;
}
export interface CompilerAssumptions {
  finiteNumbers: boolean;
  integerFloatSubtypeMayChange: boolean;
  numericMode: NumericMode;
  numericTolerance: NumericTolerance;
}
export interface CompileResult {
  assumptions?: CompilerAssumptions;
  ok: boolean;
  /** @deprecated Kept for backward compatibility only; scheduled for removal in 1.0. Same string as the first `severity: 'error'` entry in `diagnostics`. Prefer `diagnostics`, which is populated even on fatal failure (`ok: false`) — e.g. `code: 'syntax-error'` or `code: 'compile-failed'`. */
  error?: string;
  code?: string;
  original?: number;
  size?: number;
  saved?: number;
  baseline?: number;
  elapsedMs?: number;
  passes?: PassRecord[];
  diagnostics?: Diagnostic[];
  search?: SearchStats;
  apiAliases?: string[];
  disabledPasses?: string[];
  propertyMode?: 'runtime' | 'hardcode';
  propertyReadsHardcoded?: number;
  zeroCostNewlines?: number;
}
export interface PropertyScanResult {
  ok: boolean;
  numbers: string[];
  bools: string[];
  texts: string[];
  dynamicCount: number;
}
export interface PassMetadataEntry {
  id: string;
  recordName: string;
}

// --- Project API (design doc §2/§5/§6) ---

/**
 * A multi-module Lua project. The core has no notion of file paths: `modules`
 * is a flat map from module key to Lua source text. Path <-> key translation
 * (e.g. `lib/util.lua` <-> `lib.util`) is the caller's responsibility (the CLI
 * does this for `--modules-dir`; see design doc §9).
 */
export interface LuaProject {
  /** One of the `modules` keys. This module becomes the exported chunk's body. */
  entry: string;
  /** Module key -> Lua source text. */
  modules: Record<string, string>;
  /** Ambient namespaces (standard-library-like injection). Root global name -> definition. */
  ambient?: Record<string, AmbientNamespace>;
}
export interface AmbientNamespace {
  members: Record<string, AmbientMember>;
}
export type AmbientMember =
  { kind: 'module'; source: string } | { kind: 'environmentOnly' };

export interface AnalyzeOptions {
  /** build (default): static linker parity; runtime: named chunks with host-resolved includes. */
  mode?: 'build' | 'runtime';
  environment?: EnvironmentProfile;
  hostBindings?: string[];
  target?: CompilerTarget;
  /** Diagnostic codes to suppress. `severity: 'error'` codes cannot be suppressed. */
  disabledRules?: string[];
}
export interface AnalyzeResult {
  /** false only on an internal error; syntax errors are reported via `diagnostics`, not `ok`. */
  ok: boolean;
  diagnostics: Diagnostic[];
}

export interface ProjectCompileOptions extends CompileOptions {
  /** false = link only (readable output + Source Map). Default true. */
  minify?: boolean;
}
export interface ProjectCompileResult {
  assumptions?: CompilerAssumptions;
  ok: boolean;
  error?: string;
  code?: string;
  /** Source Map v3 (JSON string). Present only when `minify:false`. */
  map?: string;
  /** Linked module keys, in execution order. */
  usedModules?: string[];
  /** Injected ambient members. Root name -> member names. */
  injectedAmbient?: Record<string, string[]>;
  diagnostics?: Diagnostic[];
  // The following mirror CompileResult and are present only when `minify:true`.
  original?: number;
  size?: number;
  saved?: number;
  baseline?: number;
  elapsedMs?: number;
  passes?: PassRecord[];
  search?: SearchStats;
  apiAliases?: string[];
  disabledPasses?: string[];
  propertyMode?: 'runtime' | 'hardcode';
  propertyReadsHardcoded?: number;
  zeroCostNewlines?: number;
}


/** Lossless static values, or expressions that require real Lua execution. */
export type SourceLiteral = {kind:'nil'} | {kind:'bool';value:boolean} | {kind:'integer';value:string} |
  {kind:'number';bits:string} | {kind:'bytes';value:number[]} |
  {kind:'table';entries:[SourceLiteral,SourceLiteral][]} | {kind:'dynamic'};
/** UTF-8 byte spans over the exact original source (not UTF-16 editor offsets). */
export interface SourceStatement {
  kind:'call'|'do'|'other';start:number;end:number;line:number;
  function:string|null;method:boolean;arguments:SourceLiteral[];body:SourceStatement[];
}
export interface SourceComment {start:number;end:number;line:number;text:string;long:boolean}
export interface SourceInspection {ok:boolean;error:string|null;statements:SourceStatement[];comments:SourceComment[];replacedRoots:string[]}
