/** Versioned x_storm source-map extension. All span offsets are UTF-8 bytes. */
export interface OriginalSpan { readonly source: number; readonly start: number; readonly end: number; }
export type OriginKind = 'source' | 'derived' | 'synthetic';
export type OriginPrecision = 'name' | 'token' | 'expression' | 'statement' | 'group';
export type OriginRelationRole = 'contribution' | 'definition' | 'callSite' | 'argument' | 'parameterUse' | 'useSite';
export interface OriginRelation { readonly role: OriginRelationRole; readonly span: OriginalSpan; }
/** Observed transformation facts, not a formal proof of semantic equivalence. */
export interface OptimizationReason {
  readonly code: string;
  readonly operation: 'rewrite' | 'rename' | 'relate' | 'synthesize' | 'remove';
  readonly before: string | null;
  readonly after: string | null;
  /** Explicit successful check, absent when only the structural action was recorded. */
  readonly basis: string | null;
  readonly facts: readonly { readonly key: string; readonly value: string }[];
}
export interface OptimizationOrigin {
  readonly kind: OriginKind;
  readonly primary: OriginalSpan | null;
  /** Ordered indexes into OptimizationMap.relations. */
  readonly related: readonly number[];
  readonly precision: OriginPrecision;
  readonly name: string | null;
  readonly transformation: string | null;
  /** Indexes into the containing extension's deduplicated reasons table. */
  readonly reasons: readonly number[];
}
export interface OptimizationMapping {
  readonly start: number;
  readonly end: number;
  /** Null means unknown, not synthetic and never a nearby source fallback. */
  readonly origin: number | null;
  /** Exact equal-byte source slice; supports offsets inside a retained token. */
  readonly copied: OriginalSpan | null;
  readonly inlineContexts: readonly number[];
}
export interface InlineExpansionContext {
  /** Original expression/body; not necessarily the full function declaration. */
  readonly definition: OriginalSpan;
  readonly callSite: OriginalSpan;
}
export interface GeneratedConstruct {
  readonly start: number;
  readonly end: number;
  readonly origin: number | null;
}
export interface SourceDisposition {
  readonly original: OriginalSpan;
  readonly reason: number;
  /** This is source provenance, not an executable breakpoint destination. */
  readonly replacementSources: readonly OriginalSpan[];
}
export interface SourceBufferIdentity { readonly bytes: number; readonly sha256: string; }
/** Result of compiler.validateSourceMap(code, map), after structural and hash checks. */
export interface OptimizationMap {
  readonly schemaVersion: 1;
  readonly producer: {
    readonly name: 'storm-lua-engine';
    readonly version: string;
    readonly revision: string;
    readonly dirty: boolean | null;
  };
  readonly coordinateUnit: 'utf8-bytes';
  readonly generated: SourceBufferIdentity;
  /** Aligned with the standard sources and sourcesContent arrays in the map JSON. */
  readonly sources: readonly SourceBufferIdentity[];
  readonly compilation: Readonly<Record<string, unknown>>;
  readonly origins: readonly OptimizationOrigin[];
  readonly reasons: readonly OptimizationReason[];
  readonly relations: readonly OriginRelation[];
  readonly contexts: readonly InlineExpansionContext[];
  readonly mappings: readonly OptimizationMapping[];
  readonly constructs: readonly GeneratedConstruct[];
  readonly dispositions: readonly SourceDisposition[];
  /** Canonical-map SHA-256; consistency check, not an authenticated signature. */
  readonly integrity: string;
}
