# v0.2.0 preparation and source-loading verification

Date: 2026-09-27. This record covers the changes after the existing compiler/environment/Playground integration. Version 0.2.0 is prepared locally; no push, tag, npm publication or production deployment is implied.

## E3: explicit development require

Added a shared VM-owned `RequireLoader` and high-level Rust/TypeScript configuration. The host supplies text and a stable chunk name synchronously. The VM compiles a function inside the trusted callback, then an internal Lua wrapper calls it after the callback returns. This retains the same Lua coroutine, instruction budget, callback phase and debugger continuation rather than reentering public load APIs or spawning an independent execution.

The contract is include-once with discarded returns, shared globals and separate chunk locals. Cache keys are logical names. Marking occurs before execution, so cycles do not rerun and a runtime error remains cached. Missing sources, host resolution failure and syntax errors can be retried. The cache is per VM and recreated during reset/reload. Game mode and conflicting require bindings reject configuration; neither filesystem nor package/load/loadfile access is exposed.

## E4: completed load sequence

Vehicle retains all completed explicit loads in order, with 128-chunk/8-MiB bounds. A suspended load becomes durable only when its continuation completes. A failed, syntax-invalid or abandoned load is not replayed. Reset recreates current properties, bindings and loader, then replays the completed sequence. If replay fails, the old VM is retained; external host side effects are not rolled back.

Addon intentionally retains its initial-only load lifecycle. One entry can require development prelude/modules/epilogue without changing savedata restoration or onCreate order. Tests verify this with reload. The changes do not provide a full runtime snapshot or make successive edited sources a program-replacement API.

## Executed checks

| Check | Result |
| --- | --- |
| Native workspace, all features | **468 passed**, zero failed or ignored |
| New actual-Lua source tests | **13 passed**, included above |
| Clippy workspace, all targets/features | Passed with warnings denied |
| SDK TypeScript/contract tests | **27 passed** |
| Real runtime/raster/compiler WASM suite | **43 passed**, including eight new source-loading cases; independent Lua backend probe passed |
| Installed package consumer | Packed **60 files**, offline installed; runtime, raster, compiler and new source-loading consumer ran successfully |
| Playground strict build and shared/CLI tests | **15 passed**, including 13 recipes |
| Playground real browsers | Chromium/Firefox/WebKit: **13 recipes each**, plus persistence, bad imports, cancellation, and independent WASM-loading checks |
| Storm Min downstream | Full Rust workspace and all-target Clippy passed; existing Node/type, CLI, browser-WASM and static-Web checks passed using the 0.2.0 compiler |
| Public docs | Mint build validation passed; no broken links |
| Blog | Astro check: zero errors/warnings/hints; build passed; draft slug absent from 25 output/feed/sitemap/raw-text artifacts |

No regression fixture was regenerated for this change. The compiler optimization semantics from the prior environment correction are unchanged.

## Direct execution and review

The independent package consumer pauses at `@lib/utility.lua:2`, resumes into its caller, loads a suffix, and obtains output 21 before and after reset. The Playground recipe obtains 27 before and after reset and records two source-resolver calls, demonstrating a fresh include cache. Native and WASM tests cover nested/circular names, distinct aliases, discarded returns, local scoping, syntax/runtime failure differences, suspended load abandonment, and resource limits.

Source review checked that loaded Lua executes outside the source provider's C callback. Removing reentrancy guards or invoking a fresh public VM operation from a host callback was not used. Instruction exhaustion is not reset by nested require; source bytes, counts and heap usage are bounded. Failed replay does not replace the prior live instance. Host closure state and external side effects remain owned by the application.

The new source recipe's actual desktop screenshot and existing mobile layout were inspected. The example, selected extended profile, source, result log and reinitialization controls render without clipping. The browser plugin is not available in this session, so the maintained Playwright runner was used. Functional tests run in all three browsers under strict CSP; Chromium screenshots provide visual evidence.

## Documentation and version scope

Six user-guide bodies moved to docs.makkii.jp. Repository guide paths are retained as short relocation links, while implementation contracts, architecture, verification, release procedure and executable examples remain with code. Added source-loading and 0.1-to-0.2 guides; no os.clock-specific guide was added.

Workspace crates, SDK package and Playground are version 0.2.0. CHANGELOG records accumulated unpushed changes with an Unreleased heading. User-guide pages identify their version; the blog article remains draft:true and is excluded from normal output. Existing savedata/project formats and rendering/Composite ABI are unchanged.

## Limits

These checks do not constitute integration of external consumer products or every LifeBoatAPI feature. Include semantics are distinct from standard Lua module-return require and the compiler's static project linker. No map API or clock API was added. The full collected real-world Lua corpus was not readmitted, and performance gains are not claimed. Publication and deployments remain separate authorized operations.
