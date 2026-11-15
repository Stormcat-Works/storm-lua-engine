# WASM ABI and ownership

## Modules and exports

ABI1、wasm32、little-endian。Lua実行はEmscripten、raster-onlyはunknown-unknown。exportsの正本リストは[wasm-exports.json](../../tools/wasm-exports.json)。runtimeはsle、rasterはsls prefixを使う。

`abi_version`/`capabilities`で確認する。capability bitはraster=1、vehicle runtime=2、debug=4、addon=8、JS host services=16。標準Emscripten buildは31、raster-onlyは1。Native buildはJS host servicesのbitを持たない。`error_status/error_ptr/error_len`と`response_ptr/response_len`は診断の取得であり、直前のerrorをclearしない。他の通常operationはerrorをclearするので、cleanup前にerrorを読み取る。

## Fixed I/O

Rustの[IoBuffer](../../crates/storm-lua-spec/src/abi.rs)が正本。`cargo xtask generate`でTS/JSONへ同期する。

| Offset | Field | Length |
|---:|---|---:|
| 0 | input_numbers / Float32Array | 128 bytes |
| 128 | input_booleans / Uint8Array(0 or 1) | 32 bytes |
| 160 | output_numbers / Float32Array | 128 bytes |
| 288 | output_booleans / Uint8Array(0 or 1) | 32 bytes |

合計320bytes、alignment4。Rust boolやVecの内部表現をこの領域としてcastしない。vehicle VMごとにBoxで安定配置し、tick開始前にBooleanの0/1を検証する。output部分はホストから変更しない。

## Handles and buffers

VM/raster handleは12bit slot+20bit generation。0は不正。disposeでgenerationを進め、世代を使い切ったslotはretireする。上限4095slotsで、古いhandleを再利用しない。runtime内のvehicleとaddonは同じregistryの別variantで、mode違いを検査する。AddonにI/O bufferは確保しない。registry borrowが競合する場合はBusy。異なるmoduleのhandleは使用しない。

`alloc(length)`は1〜16MiBのmodule-owned staging bufferを返す。総量64MiB、最大4096allocations。`dealloc(ptr)`は登録済みallocationのみを解放する。入力関数は登録済みpointerと論理lengthを検査し、任意のhost pointerをunsafeにdereferenceしない。空payloadは1byteを確保し論理length0で渡す。

生pointerは貸与契約であり、free後の再利用をhostが禁止する。古いupload pointerのアドレスが将来再利用される可能性まで世代付きhandleと同じ保証にしない。通常TS APIがallocationとfreeを所有する。

memory.growは非共有memoryの旧ArrayBufferをdetachする。[MDN](https://developer.mozilla.org/en-US/docs/WebAssembly/Reference/JavaScript_interface/Memory/grow)。RawIoViewはbuffer identityの変化でviewを再生成する。WASMへの入力自体が同じmemoryを指す場合、allocがdetachする前に入力をcopyする。borrowをalloc/await/frame更新/disposeを跨いで保持しない。

FrameLeaseはframe epochとVM/rasterの生存を確認する。copyはJS所有のsnapshot。既に返したTypedArrayを後から回収・無効化する機能ではない。frameのptr/len/寸法を再照会し、raw RGBAと表示用変換を分離する。

## Binary drawing commands

各recordはLE u16 opcode、u16 reserved=0、u32 payload bytesの8byte header。座標はLE f64、色はRGBA4byte、textはUTF-8 bytes。

| Opcode | Payload |
|---:|---|
| 1 | RGBA4bytes |
| 2 | drawClear、payload0 |
| 3 | line、f64×4 |
| 4/5 | rect/rectF、f64×4 |
| 6/7 | circle/circleF、f64×3 |
| 8/9 | triangle/triangleF、f64×6 |
| 10 | text、x/yのf64×2 + bytes |
| 11 | textBox、x/y/w/h/hAlign/vAlignのf64×6 + bytes |
| 12 | host map、world X/Z/zoomのf64×3 |
| 13 | map palette、slot u8 + RGBA4bytes |

decoderは全batchを検証してからsubmitする。raster adapterはUTF-8も検証してからframeをclearする。上限8MiB/65536commands/text1MiB。壊れた長さ・unknown opcode・reserved bitsを無視しない。

## Status and control plane

0=completed、1=Lua error、2=limit、3=invalid handle、4=invalid argument、5=busy、6=unsupported、7=suspended、8=missing callback、9=failed VM、10=host service failure。0/7/8以外をTS EngineErrorにする。pointer/queryの0とerrorはerror_statusで区別する。番号の正本は`storm_lua_spec::abi::status`で、`cargo xtask generate`がTSの`STATUS`へ同期する。

runtimeの`error_detail_ptr/error_detail_len`は、直前のerrorの[位置情報](runtime-host.md#エラーの位置)をUTF-8のJSONで返す。形は`{"stage":"syntax"|"runtime"|null,"location":{"chunk","line"}|null,"stack":[{"level","source","line","functionName"}]}`。位置情報がないerrorでは長さ0。寿命は`error_ptr/error_len`と同じで、次の通常operationがclearする。raster-onlyのmoduleはこのexportを持たない。

property更新はbyte-label付きentry JSON。Numberは16桁f64 bits、Textはbyte配列。`sle_vehicle`の`setProperty`は同じ形のentryを1件、`removeProperty`はbyte配列のlabelを受け、削除はその項目があったかどうかをBooleanで返す。debugはaction付きJSONでbreakpoints/resume/requestPause/cancelPause/stack/locals/upvalues/watch/tableを表現する。resumeは任意のBoolean `carry`を受け取る。`breakable_lines`はloadと同じくソースとチャンク名のupload bufferを受け取り、行番号の配列をcold responseで返す。Vehicleの設定JSONの`pausePoll: true`は、同期host importへ`{"kind":"pausePoll"}`を送る問い合わせを有効にする。応答は1byteの0または1で、それ以外はhost errorとする。debugを含まないbuildは、これらをunsupportedで拒否する。i64/u64はdecimal string、f64はbits、Lua stringはbytes。cold response上限16MiB。

## Threading

baselineは独立Workerごとの非共有memory。SharedArrayBuffer/pthreads/SIMDを必須にしない。Workerへ渡すときはframe.copyのowned bufferをtransferする。WASM memoryをtransferしない。同期API自体にWorkerやtimerを組み込まず、[example](../../examples/browser/runtime-worker.js)でhostによる並列化を示す。

## Synchronous host calls

`server.*`のhost関数とmap providerは、`tools/host-library.js`の同期import経路で呼ぶ。Rust→JSの要求は先にJS所有bytesへコピーし、host callbackから返る結果もコピーしてからRust側へ渡す。Rustの生pointerをhost callbackへ保持させない。Rust側のallocationはcallback復帰後に行う。

mapの入力は少量の座標/palette JSON、戻りはraw RGBA。serverの入力/戻りはlossless tagged valuesで、任意のLuaヒープへの参照は渡さない。この境界はcold/host-service経路であり、通常vehicle tickのI/OへJSONを追加するものではない。

TS bridgeは同じmoduleへの再入とcallback中の新規I/Oビュー借用を拒否する。既存raw viewやraw exportsを直接操作するtrusted hostは、この契約を自身で守る必要がある。raw C ABIは悪意ある同じJS realmのホストを隔離するものではない。

`new_addon`と`addon`はモード確認付きの設定・イベント・checkpoint操作。`http`はdrain/reply/cancel、`drain_log_records`はsource付きbytesログ。JSON requestは4MiB、host responseは16MiB以下に制限する。HTTP generation/debug epochのu64は10進文字列で運ぶ。保存形式の詳細は[Addon](addon.md)。
