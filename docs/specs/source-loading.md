# Named sources and development include loading

This is the v0.2.0 contract. It does not expose a filesystem, networking, Lua bytecode loading or standard `package` facilities.

## Explicit host resolver

`RequireLoader` in Rust and `requireLoader` in TypeScript supply a `SourceChunk { source, name }` synchronously. Resolution is enabled only in `extended`; an independent `require` host binding conflicts and is rejected. Without a configured loader, `require` remains nil even in extended. Vehicle and Addon use the same VM implementation. The host decides module names, paths, snapshot/version policy and access control.

The resolver is called only for a logical name that has not already been loaded in that VM. It returns text and an exact chunk name, such as `@lib/helper.lua`; it must not call back into the same VM. Async/Promise responses are rejected. Browsers should supply source already available in the execution Worker rather than attempt an asynchronous fetch in a synchronous Lua call.

## Include-once semantics

This interface implements the LifeBoat-style development include behavior, not the Lua standard module return-value convention:

1. Look up the logical name in a VM-local cache.
2. Ask the host for source, validate boundaries and compile a text chunk in the existing script environment.
3. Mark the name before running the compiled chunk. Circular includes of that same logical name return without reexecuting it.
4. Execute the chunk without arguments and discard all returned values. The `require` wrapper itself returns zero values.

Global state is shared; chunk locals remain local to that chunk. Two logical names resolving to the same physical filename are distinct cache keys. Missing source, a resolver exception, or syntax failure is not marked loaded. A runtime failure after execution starts remains marked, including when Lua pcall catches that failure. This choice matches the include convention; it is not advertised as standard Lua require.

The Rust callback only compiles and returns a Lua Function to an internal Lua wrapper. That wrapper invokes it after the C callback returns. It runs on the calling coroutine, keeping the parent's instruction budget, phase, source-qualified breakpoints and suspension/resume. The public owned-value host-function interface still does not transport Lua Functions or permit reentry.

One chunk is at most 1 MiB. Chunk/module names are nonempty UTF-8 text, at most 1024 bytes, without NUL. Module resolution is capped at 1024 successfully compiled sources and 8 MiB cumulative source bytes per VM. The Lua heap limit still applies to compiled functions and cached state. Host callback runtime/allocations remain host responsibilities; no wall-clock interruption guarantee is added.

## Vehicle load history

Vehicle `load` appends a named chunk to the existing environment. Each call executes even if its source/name repeats. It is not replacement, hot reload, or cached require. Unreplaced callbacks/global values remain present; chunk locals are separate.

Only completed loads enter the replay history. A debugger-suspended load is pending until it completes on resume. A failed load, a syntax-invalid load or a suspended load abandoned by reset does not enter the completed history. Earlier source-visible side effects from a failed load are not rolled back in the old VM; reset creates a fresh state.

`reset` reinstalls current properties, host bindings and require loader, then reexecutes every completed load in its original order. The module cache is new, so the host may be asked for sources again. Deterministic reset requires the host to keep a consistent source snapshot. This resets runtime I/O and debugger/HTTP identities, not the host closure's external state. No intervening ticks, draws, HTTP replies, property changes or external effects are replayed; this is initialization replay, not a session snapshot.

History is bounded to 128 chunks and 8 MiB; overflow is rejected before executing another chunk. Empty text is a valid retained chunk. Reset replaces the current instance only after all replayed chunks succeed. External callbacks invoked during a failed replay cannot be rolled back. For a different program, create another VM instead of accumulating edited copies with load.

## Addon lifecycle

Addon `load` remains the one-time entry-source operation. Allowing arbitrary additional loads would make property declarations, savedata restoration and onCreate ordering ambiguous. For development prelude/modules/epilogue, use one entry that calls configured require names. `reload(savedata)` recreates the VM and include cache, runs that same entry and its required sources, restores savedata after top-level initialization, and awaits explicit `start()` as before.

## Compiler distinction

Runtime include loading and compiler static project linking are different contracts. Compiler build does not invoke a runtime loader or read host files. It keeps the existing static module/ambient contract. For direct minify of a development include source, use extended and declare `require` in hostBindings; this uses lexical-only compaction rather than claiming a game-ready linked artifact. Addon compilation remains unsupported.

The user guide is maintained at [docs.makkii.jp](https://docs.makkii.jp/storm-lua-engine/source-loading). Small independent consumers live in examples, and the actual-runtime regression is in `conformance/tests/sources.rs` and the WASM source suite.


## IDE source inspection and LB game build (unreleased)

The compiler exposes inspectSource, stripDevelopment and buildLifeboat separately from ordinary static build. Inspection uses the shared lexer/parser and UTF-8 byte ranges, retains real comments and ordered literal table fields, and marks expressions dynamic rather than evaluating them. Malformed, oversized, shadowed or nonliteral declarations must not be rewritten as empty/zero configuration by a host.

buildLifeboat creates include-once loaders with private chunk locals and shared globals, discards return values, and preserves logical-name cache behavior. Direct literal includes can appear inside functions/conditions; dynamic, reassigned or aliased require is a development-runtime capability but is rejected by the self-contained game build. Ordinary build retains its return-value module contract.

Real section/endsection comments support EXACT (default), PATTERN using bounded Lua pattern matching, instance thresholds and named/nested regions. Strings and ordinary comments do not count as code references. __LB_SIMULATOR_ONLY__ and explicit Storm Code managed setup blocks are removed before resolving game dependencies, independently of minification. Offsets/newlines are preserved during removal and non-minified code uses the existing link source map; no optimization source map is provided. Pattern/depth/module/size budgets fail visibly rather than silently keep or delete ambiguous sections.

Source inspection is limited to 2 MiB, 200,000 lexical tokens and bounded syntax/literal nesting. It exposes exact i64 strings, binary64 bits and byte strings. It is opt-in and does not change default optimization parsing or search settings.
