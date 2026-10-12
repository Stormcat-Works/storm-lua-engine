# v0.2.0 local candidate verification

2026-09-27 — **Local completion steps 1–5 passed. Not pushed, tagged, published or deployed.** The implementation under test is commit `8707a80f97722fb7ea6f1f7b7b86a58b5b37cdef`, based on the earlier `b9be456` preparation. Subsequent candidate-record changes are documentation only. The machine-readable [receipt](release-candidate-0.2.0.json) contains the tested revision, source digest, command arguments, exit codes, log digests and local artifact hashes.

The scope is fixed in [release](../release.md). Post-optimization source maps require a separate optimizer provenance redesign and are deferred to **v0.2.5 or v0.3.0**. No such implementation is claimed here.

## Completion of the requested five steps

| Step | Result |
| --- | --- |
| 1. Freeze scope | Current STATUS/TASKS and release boundaries were consolidated. Historical extraction counts are linked as historical records, not mixed into current results. Existing game static-build and extended include-loader paths remain unchanged. |
| 2. Permanent mapped-debug consumer | Added an independently installed Node example, two Native conformance tests, and five real-WASM tests. Original-file breakpoints, caller frames, stepping, reset and runtime error mapping are executed using the public SDK. |
| 3. Review changed contracts | Reviewed source-map ownership, environment/lexical paths, source loader, host callbacks, VM state transitions, completed-load reset and public dependency boundaries. Corrected the actual mapping defect described below; no new runtime feature or optimizer pass was added. |
| 4. Public-history/artifact review | Examined incoming reachable history and tracked content relative to v0.1.0, existing attribution/notice files and the actual tarball/site artifacts. Scans found no candidate credentials, private corpus copies or machine-home paths in the reviewed scope. |
| 5. Final candidate evidence | Reran 41 named gates; latest execution of each passed. Added a fresh 400-condition Native/WASM comparison and re-tested a retained local validation tarball. Commands, counts, hashes and limits are consolidated here and in the receipt. |

## Findings and corrections

### RC-MAP-01: generated glue referred to nonexistent source lines

Before correction, the linker's generated `do/end`, hoisted values and nil-result normalization were assigned increasing source lines from a nearby hint. A one-line module could therefore produce a map pointing to lines 2 or 3 of that source. Comparing the mapper only against its own `LinkedRange` table did not expose this error.

The linker now records ranges for verbatim source only. Synthetic-only output lines receive explicit unmapped segments in Source Map v3, so lookup cannot inherit the previous origin. A generated prefix followed by real source on the same output line may use that real source origin. No exact column-provenance claim is added.

Five independent small reproducers contained **13 out-of-bounds origins before the fix and zero after it**. Their generated Lua was byte-identical before and after; only the mapping changed. Permanent tests check source-content lengths, returned function bodies, injected ambient sources, CRLF and Unicode comments/strings. The downstream compiler fixtures were not regenerated or edited.

### RC-TEST-01: downstream npm verifier assumed a repository-local Cargo target

The first downstream `npm test` built its release executables correctly in the configured Cargo target directory, but seven JS tests tried to launch `target/release/...` relative to the repository. They failed to start the verifier binaries; this was a test-harness path issue, not seven Lua semantic failures.

The downstream test launcher now obtains `target_directory` from `cargo metadata`, preserves explicit binary overrides and verifies the executables exist before starting Node tests. The assertions and test population were not relaxed. The full downstream gate was rerun, including **25 passing independent JS tests**, the 640-tick control verification, Native tests and package/browser checks. The failed attempt remains in the local evidence rather than being discarded.

### RC-DOC-01: current and historical status were mixed

The former status/task pages still described the SDK version bump as unperformed, `debug.log` as opt-in, and initial extraction totals alongside later semantic corrections. Current pages now distinguish the 0.2.0 implementation, newly executed results, historical records and publication-only work. Game `debug.log`, extended `print`, current loader restrictions and the optimized-map deferral are explicit.

## Source-map host behavior verified

`examples/consumer/source-map.mjs` consumes an installed SDK plus a normal `@jridgewell/trace-mapping` dependency in the host. It is run by the isolated package gate, not only from a source checkout. Trace-mapping remains a **development/example dependency**, not an SDK runtime npm dependency.

The example builds multiple modules with `minify:false` and runs the bundle in the game environment without enabling require/pcall/print. An exact original `lib/util.lua:3` breakpoint pauses the generated bundle at the corresponding line, the caller maps to `main.lua:3`, step-over maps to the next original line, and execution returns output 6. Reinitializing the same code with new input gives 8. A separate returned-function error maps to `lib/broken.lua:3`.

Missing original lines are not silently snapped to a nearby line. Synthetic frames remain generated. The optimized build still returns no source map, rather than attaching a stale pre-optimization map. Native and WASM regressions exercise the same behavior.

The integration contract is in [source-maps](../specs/source-maps.md); the user-facing procedure is in [docs.makkii.jp](https://docs.makkii.jp/storm-lua-engine/source-maps).

## Final executed results

Environment: Linux x86_64, Rust 1.97.1, Node 22.22.1, npm 9.2.0 and matched Emscripten 6.0.6. Cargo output used the environment's tmpfs build-cache policy. No global toolchain configuration was changed.

| Check | Result in this candidate run |
| --- | --- |
| Engine Native workspace/all-features | **472 passed / 0 failed / 0 ignored** |
| Engine all-target/all-feature Clippy and default workspace check | Passed; warnings denied |
| Format, architecture and generated-data gates | Passed; 15 classified crates, dependency ownership/DAG verified |
| Rustdoc | Passed with broken intra-doc links denied |
| Native examples | microcontroller, differential, compiler and addon_host passed |
| Python tools | **19 passed** |
| Screen fixture integrity | **731** accepted RGBA cases and font digest verified |
| SDK TypeScript/contract tests | **27 passed** |
| Actual runtime/raster/compiler WASM | **48 passed**, plus independent Lua backend probe |
| Runtime browser conformance | Chromium, Firefox and WebKit; each ran 731 direct RGBA and 731 Lua cases plus host/debug/Worker checks |
| Compiler-only browser Worker | All three browser engines passed; only compiler WASM fetched |
| Playground shared/CLI tests | **15 passed**, including 13 SDK recipes |
| Playground browsers | **13 recipes × 3 engines = 39 successful recipe executions**; persistence, bad import recovery, cancellation and independent WASM loading also passed |
| Independent installed SDK | **60-file package** installed offline; runtime/raster/compiler/source-loader and mapped-debug consumer examples passed |
| Storm Min Native | **492 passed / 0 failed / 0 ignored**, full all-target Clippy passed |
| Storm Min independent JS semantics | **25 passed**, including the 640-tick control program |
| Storm Min distribution paths | WASM, Node/types, CLI, browser WASM and static Web gates passed |
| Fresh downstream Native/WASM comparison | **400 conditions: 392 byte-identical successful outputs and eight matching explicit environment rejections** |
| docs-site | Mint validation passed; no broken links |
| Blog | Astro check/build passed; both Lua articles remain drafts and are excluded from published output |
| Worker packaging | Production-layout static site built; deployment **dry-run only** passed |

Counts above are not added together to imply distinct independent semantic cases: Native functions can contain fixture loops, WASM checks reuse the same SDK contracts, and recipe tests are repeated across browsers. Source-map unit/conformance tests are included in the corresponding totals.

## Manual review beyond gates

Reviewed the shared environment catalog and its use in runtime/analysis/build; lexical compaction and its no-AST-transform boundary; external-global name ownership; require's Lua wrapper and synchronous compiler callback; source validation/cache limits; the completed/pending load separation; reset's replacement-on-success behavior; reentrancy checks and HTTP/debug/log transitions; and normal dependency graphs.

The loader still runs a returned Lua function outside its host C callback. No reentrancy guard was removed. The same coroutine and instruction budget continue across required chunks. Only completed explicit loads are replayed, and external host side effects are still not rollbackable. Game/extended and loader semantics were not expanded for this release check.

Normal compiler dependencies contain neither a Lua VM nor a runtime profile; normal microcontroller dependencies contain no compiler. Existing Composite layout, DrawCommand/command-wire and low-level ABI/owned-value format files were unchanged from v0.1.0. An example-only source-map consumer does not introduce a new public SDK dependency or crate.

The actual Playground desktop and mobile screenshots were opened and inspected. Source, environment, execution controls, monitor output and result details remained visible, with horizontal scrolling confined to the intended source/navigation areas and no viewport overflow. The Browser plugin was unavailable, so the maintained Playwright runner was used. Functional tests run under strict CSP in all three browsers; Chromium supplies visual screenshots. This task did not restyle the app or add new product features.

## Public content and artifact review

The reachable incoming history through tested commit `8707a80` contains **eight commits** above published v0.1.0. The metadata/content review inspected **335 incoming commit/blob objects, 3,396,841 bytes**, with no matching credential/private-key/home-path/internal-corpus candidates and no tracked build archives, dependency directories or private corpus files. The source import is an ordinary linear addition, not a merge of the private optimizer repository's historical graph.

Compiler/conformance additions contain product implementation and self-contained test programs. The accepted screen data and its existing MIT attribution are retained. Project, Rust/Lua dependencies, toolchain/system notices and Playground helper notices remain included. The toolchain-notice consistency gate and installed-package license checks passed. The tree/history review is bounded; it is not a proof that every possible secret format or legal provenance issue is automatically detectable.

The retained **local validation tarball**, not a published or permanently approved release asset, has:

- Name: `stormcat-works-storm-lua-engine-0.2.0.tgz`
- Size: **1,238,238 bytes**, **60 files**
- SHA-256: `97f31ba49211fafab5fc50cfde63edf57d0f035109b678956326d50f6a0659c2`

It was installed offline and the exact installed package executed successfully. SDK and Playground copies of runtime, raster and compiler WASM are byte-identical. Per-file source/artifact hashes and log digests are recorded in the [machine receipt](release-candidate-0.2.0.json); full local logs and screenshots are retained outside the product repository. Private input names/content are not copied into this public report.

## What this does not finish

No push, remote CI, main/develop integration, tag, npm publish, GitHub Release, production Worker deployment, docs deployment or blog publication occurred. Remote Linux/Windows/macOS validation of the eventual public commit and final distribution-asset fixation remain release-stage work.

Non-minified maps remain line-granular; mixed-source lines are best effort, and a mapped comment/blank line is not necessarily executable. No post-minify origin propagation, restored optimized variable state, new require mode, Addon compiler, real-game recalibration, complete collected-corpus readmission or speedup claim is included. Storm Min and Playground remain separate frontends. No new external consumer product migration was performed as part of this review.

The local candidate is ready for the separately authorized publication steps in [release](../release.md), not already published.
