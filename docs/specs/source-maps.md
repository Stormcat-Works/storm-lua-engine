# Source maps and source locations

## Published and working-tree boundaries

Published **v0.2.0** returns a Source Map v3 JSON `code`/`map` pair only for `build(project, { minify: false })`. That published map is line-based, its columns are zero and it embeds `sourcesContent`. Standalone `minify` and `minify: true` do not return a post-optimization map.

The **unpublished source-provenance work** upgrades non-minified normal and LifeBoat builds to token/column anchors and exact internal byte ranges. It does not yet implement optimized-AST provenance or return a minified map. The complete design and implementation stages are in [Source provenance](../design/source-provenance.md). No published SDK artifact has been replaced.

A failed build has no artifact. Hosts must check the build result before using `code` or `map`. Compilation does not create a VM or execute Lua.

## Origin and location model (unpublished)

The linker owns ordered, disjoint `LinkedRange` intervals for verbatim source slices. Each has generated `[startByte,endByte)` and original `startByte` in a module snapshot. Line fields are a coarse projection retained for low-level line lookup, not the authority for column correspondence.

The mapper records generated token starts, line starts and verbatim-slice boundaries. Mapping anchors resolve to their exact original line and column. An arbitrary position inside a token resolves to the preceding anchor; this is not a character-by-character mapping. Internal verbatim ranges can translate byte offsets exactly without guessing from the encoded map.

Generated `do/end`, loader scaffolding, return-normalization prefixes, hoisted variables, injected namespace declarations and EOF receive explicit unmapped anchors. A generated prefix on the same line as original text remains unmapped until the original slice begins. Greatest-lower-bound consumers must not inherit a preceding origin across generated gaps.

LifeBoat development blocks, unused sections and removed directive comments are recorded when they are blanked. Their intervals are subtracted from the origin table. Preserving byte length and newlines while replacing Unicode with ASCII spaces does not make the replaced text original source. No final-string comparison is used to infer these origins.

`lib.util` maps to `lib/util.lua`; injected ambient members use the same logical path convention. `sourcesContent` retains the exact original snapshots of mapped sources, including original line endings. `names` is still empty. Optimized local names, expression transformations and eliminated-variable values are not encoded in this phase.

## Coordinates

Internal ranges are UTF-8 byte offsets. Lua/parser diagnostics use one-based lines and byte columns. Serialized map coordinates use zero-based lines and UTF-16 columns; common JS consumers expose one-based lines and zero-based columns. Convert deliberately rather than incrementing or reusing byte columns blindly.

The shared syntax `LineIndex` handles LF and CRLF without normalizing source text, validates character boundaries and supports EOF. It indexes non-ASCII width differences so mapping many tokens on a long line does not repeatedly scan that line. UTF-16 coordinates do not imply UTF-16 encoding of the Lua source itself.

## Host connection

Keep the exact map with the exact generated code loaded under a host-chosen chunk name, for example `@mapped-program.lua`. The chunk name is not a source filename from the map. The map neither carries a runtime handle nor automatically installs breakpoints.

To bind an original breakpoint, enumerate exact entries for its source path and line, deduplicate generated lines and use the loaded chunk identity. Missing entries remain unmapped; do not silently select a nearby original line. A mapped blank/comment-only line is not evidence that Lua can stop there. Executable-line binding belongs to the host/debugger.

When the runtime provides a generated column, map that actual column after converting coordinate units. When it provides only a line, do not invent column zero as the actual execution position. Enumerate origins on the exact generated line: a unique source/line may be displayed as a line-level association; multiple origins remain ambiguous and generated-only frames remain generated. Existing simple examples with original text at column zero continue to work, but are not a general column-recovery mechanism.

Recognized runtime error source/line information can be associated using the same rules. Preserve unrecognized errors. Code edits, minification, a different bundle or source snapshots invalidate the pairing. A code hash alone cannot distinguish two original inputs that compile to identical Lua.

Imported maps are not authorization to read files. Embedded `sourcesContent` supplies the source view; hosts must not automatically fetch arbitrary listed paths. Publishing embedded original source is an explicit host decision.

## Diagnostics versus debugging

`analyze` diagnoses original modules directly and returns module/range without requiring an output map. Build-time diagnostics after linking translate their byte positions through the exact verbatim intervals; generated-only or invalid positions lose their source attribution rather than snapping to another statement. End positions are preserved only while the whole diagnostic range remains within the same original slice. An end crossing generated glue or another source is omitted, not fabricated.

Pre-build source transformations must preserve or compose their own origin ranges. The in-engine LifeBoat blanking operations do so. External host transformations remain the host's responsibility.

This map does not restore optimized-away variables, original evaluation order, inlined stack frames or per-iteration origins of data-driven generated loops. These are separate compiler/debugger capabilities.

## Executable evidence

- `crates/storm-lua-syntax/src/source_position.rs`: byte/UTF-16 conversion, Unicode boundaries, CRLF and EOF.
- `crates/storm-lua-build/src/source_map.rs`: column anchors, multi-file boundaries, synthetic prefixes, exact original snapshots and LifeBoat exclusions.
- `crates/storm-lua-build/src/public_api.rs`: byte-column and end-range diagnostic composition.
- `packages/lua-engine/tests/wasm/compiler-provenance.test.mjs`: independent trace-mapping consumer, real compiler WASM and canonical target-search continuation across WASM instances.
- `conformance/tests/source_maps.rs`: actual-Lua Native execution of source-qualified breakpoints, caller locations and runtime errors.
- `packages/lua-engine/tests/wasm/source-map.test.mjs`: actual runtime WASM, generated gaps, original lint locations and omitted optimized maps.
- `examples/consumer/source-map.mjs`: independent installed SDK consumer of the non-minified path.

Test existence is not a claim that a particular revision passed; executed commands and results belong in verification records. The trace-mapping package is an example/test dependency, not a runtime dependency of the SDK.
