# Architecture

## Semantic owners

| Owner | Responsibility | Direct internal dependencies |
|---|---|---|
| `storm-lua-syntax` | Lua syntax/AST/visitor, numeric literals, printing and size | none |
| `storm-lua-analysis` | binding/effect analysis, project validation, lint and property scanning | syntax |
| `storm-lua-minify` | optimization passes, deterministic candidate search and cost | syntax, analysis |
| `storm-lua-build` | logical project linking, source maps and coarse compiler API | syntax, analysis, minify |
| `storm-lua-compiler-wasm` | compiler-only wasm-bindgen adapter, no VM initialization | analysis, minify, build |
| `storm-lua-spec` | I/O・property・draw types、ScreenSink、binary batch、API catalog、ABI constants | none |
| `storm-screen-raster` | 画面契約の幾何/文字/色をpixelへ展開、同梱font | spec |
| `storm-lua-vm` | Lua backend、sandbox、制限・continuation・debugger、owned values、構造化log | none |
| `storm-lua-microcontroller` | Lua API登録、tick/draw phase、signal/property/command状態 | spec, vm |
| `storm-lua-addon` | Addon lifecycle、menu property、savedata、server登録、matrix、HTTP | spec, vm |
| `storm-lua-bridge` | adapter間で共有する世代付きhandle、upload/response/errorの所有権 | spec |
| `storm-lua-wasm` | Native契約のEmscripten公開、JSON cold codec、mode検査、JS host import、I/O/frame所有 | spec, vm, microcontroller, addon, raster, bridge |
| `storm-screen-wasm` | Lua非依存のbinary raster境界 | spec, raster, bridge |

許可edgeは[architecture.json](../../tools/architecture.json)で機械検査する。大小やLOCではなく、実際の利用条件と所有者で分割する。共有bridgeは2つのWASM adapterに重複していた安全な所有権処理のownerで、ゲーム処理・Lua・JSON・rasterへ依存しない。

microcontrollerはrasterに依存しない。DrawCommandとScreenSinkを通して、命令記録・CPU frame・独自描画先を合成する。WASM runtimeは利用者の便宜のためrasterを合成するが、Native APIへ強制しない。

## Native and JS

NativeはCargoによるsource依存で、C ABI/JSONへ迂回しない。backend固有の拡張はVMの`backend-mlua`featureに明示する。通常利用APIのために全mlua型をre-exportしない。

JSは1npm packageでruntime/raster/raw/debug/commands/canvasの入口を持つ。raster-onlyはLuaをロードしない。runtimeとrasterのmoduleを両方作るとmemory/allocator/handle registryは独立する。相互にpointerを渡さない。

hot pathは固定binary32 I/O、command batch、借用frame。cold pathはsource/property/debug/logの構造化codec。propertyとdebugの数値/文字列は損失のないタグ付き表現を使う。Canvas、Worker、物理世界、実通信はhostが明示的に組み立てる。

## Tests and growth

conformanceはtest-only、xtaskはtoolであり、製品から依存できない。declared dependenciesはtarget/optional/dev/aliasを含めて検査する。generated API/ABI/fontと採用済み画面コーパスのhashも同じgateで検証する。

Addonはvmを直接使う別crateとして実装した。microcontroller/rasterへ依存しない。TSもVehicleVm/AddonVmを分け、runtimeの単一registryにenumとして保持する。ログとowned-value変換はVM側で共有し、モードごとに重複実装しない。

worldに作用するserver関数、HTTPクライアント、地形データ、物理計算、IDE永続状態はhostが提供する。host-serviceの同期呼び出しとHTTPの非同期request/replyを区別する。map providerはrasterの任意依存で、Lua-onlyの命令記録には不要。必要なcallerがない抽象や空の成功stubは追加しない。

## Compiler integration branch

The compiler owners are implemented on the development branch; see the [compiler guide](../guide/compiler.md) and [verification](../verification/compiler-integration-20260926.md). Runtime-only consumers do not depend on compiler crates; compiler-only consumers do not normally depend on the VM. The conformance package combines them through dev dependencies.

The syntax visitor and dependency-graph validator have one owner each. Analysis does not depend on the linker or minifier. `storm-lua-build` composes shared services rather than becoming a new implementation core. Low-level implementation modules needed by sibling crates are distinguished from documented host-level entry points.

The compiler WASM is a separate module and memory from runtime/raster. `@stormcat-works/storm-lua-engine/compiler` loads it explicitly; it neither initializes a VM nor creates a Worker. Host applications own lifecycle and scheduling. Official CLI/Web relocation is a later frontend stage, not a dependency of the SDK.

## Synchronous world events

The VM provides an owned `SynchronousCallbacks` entry for host services that must
complete a callback before returning to Lua. Addon exposes the lifecycle-aware
`SynchronousEvents` entry and validates its event catalog. Hosts still own world
state, admission and event ordering. They must release host state borrows before
calling scripts and decide how to record callback errors; VM side effects are not
rolled back.

Each nested event executes a protected Lua coroutine in the same VM environment.
The active VM's instruction control is shared. Idle invocations receive a fresh
budget; nested invocations cannot replenish it, and short nested events count
every instruction. Synchronous events cannot yield or overlap a suspended main
continuation. A rejected busy preflight preserves that continuation. Reload,
destruction and VM drop invalidate old entries. No raw VM pointer or backend type
is exposed by these APIs.

The event entry does not create a second world model, a scheduler, HTTP routing or
server API implementations. Synchronous nested events are atomic with respect to
the host debugger's main continuation and do not create debugger continuations of
their own. The host remains responsible for script order and post-callback world
snapshots.

同期host eventのprotected coroutineはmluaのVM単位のhookを共有するため、
入れ子から戻るたびに直前のthreadの命令数・debug hookを復元する。復元失敗も
明示的なterminal errorとする。入れ子の短いcallbackで命令数の未計数分を
蓄積できないよう、同じ予算を命令ごとに消費する。

Native Addonのhost呼出し内で通常callback error後も後続eventを許す場合、
`begin_synchronous_event_pass` のowned guardを外側のhost呼出しの終了まで保持する。
guardから取得したevent handleのcloneはpassを延命しない。通常のcallback errorは
その都度Errを返し、pass終了で失敗状態を確定する。命令数・メモリ制限、yield、
hook復元の失敗は即時停止する。scopeなしの経路の既存の即時失敗契約は維持する。
Addon同期eventの非function globalは欠落handlerとして扱う。
