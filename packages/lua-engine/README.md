# Storm Lua Engine

**StormworksのビークルLuaとAddon Luaを、アプリケーションに組み込むためのWASM SDK。**

このパッケージはLua 5.3の実行基盤、Stormworks向けのCPU描画、同梱フォント、デバッガ、Vehicleコンパイラ、型定義を含みます。実際のワールド、地形データ、UI、ネットワーク通信は利用するアプリケーションが担当します。

## インストール

`npm install @stormcat-works/storm-lua-engine@0.3.0`

WASMと型定義は同梱済みです。利用時にRustやEmscriptenをインストールする必要はありません。

## 入口を選ぶ

| 目的 | 入口 |
|---|---|
| ビークルLua | `loadRuntime()` → `engine.createVehicle(options)` |
| Addon Lua | `loadRuntime()` → `engine.createAddon(options)` |
| 描画のみ | `/raster`の`loadRaster()` → `createRaster(width, height)` |
| 解析・リンク・minify | `/compiler`の`loadCompiler()` |
| コンパイラWorker接続 | `/compiler-worker`。Workerの生成・終了はホストが管理 |
| Canvas表示 | `/canvas`の`CanvasPresenter` |

ビークルは`load(source)`、`tick()`、`draw(width, height)`で駆動します。Compositeは`vehicle.io`のFloat32Array/Uint8Arrayから読み書きし、画素は`vehicle.frame().pixels`で借用、`copy()`で所有します。メモリ拡張後はビューを取り直してください。

Addonは`load(source)`、`start()`、`tick(gameTicks)`、`dispatch(callback, args)`で駆動します。`g_savedata`は`savedata()`で取得し、`encodeSavedata`／`decodeSavedata`で携帯可能な形式にします。復元は新しいAddonの`newWorld:false`と`savedata`、または`reload(checkpoint)`を使用します。AddonにはComposite I/Oやdrawメソッドはありません。

## ホスト機能をつなぐ

`createVehicle({ mapProvider })`で`drawMap`用の同期地図rendererを指定できます。返す値は正確にwidth×height×4のRGBA bytes。未指定の地形を架空画像で代用しません。

`createAddon({ server: { getPlayers: () => [playerTable] } })`のように必要なserver関数を登録します。戻り値は常に結果の配列です。Lua integerにはbigint、floatにはnumber、byte stringにはUint8Array、テーブルには`luaTable`または明示的なentry listを使用します。同期queryへPromiseを返すことはできません。

`debug.log`はgame/extendedで利用でき、`print`はextended専用です。`onLog(record)`は公開関数を追加せず、ログをコンソールやIDEへ接続します。recordはsourceとbytesを持ち、Luaの実行が戻った後に配送されます。手動処理には`drainLogRecords`／`flushLogs`もあります。

HTTPは`drainHttpRequests`からhostへ渡され、hostが実通信を行ってから`httpReply(token, bytes)`または`cancelHttp(token)`を呼びます。自動で外部へ通信しません。

## ロードと破棄

ブラウザは`await loadRuntime()`、NodeではWASMを明示的に読み`loadRuntime({ wasmBinary })`を使用します。WASMのexport pathは`@stormcat-works/storm-lua-engine/wasm/storm_lua_wasm.wasm`。独自bundlerやWebViewでは`moduleUrl`／`wasmUrl`または`fromEmscripten(module)`でアセットを接続します。

load/tick/draw/start/resumeはcompleted/suspended/missingを返し、エラーはEngineErrorなどの例外です。停止中は次のcallbackを重ねず、debuggerで確認してresumeします。使用後は必ずdispose。AddonのonDestroyも呼びたい場合は先にdestroyを明示してください。

## 配布とライセンス

npm registryまたはGitHub Releasesのtarballからインストールできます。runtime npm依存はありません。ブラウザごとの制約、全server APIのホスト実装、ゲームのsave XML直接互換は含まれません。

利用者向けの正本は[docs.makkii.jp](https://docs.makkii.jp/storm-lua-engine/index)です。契約・検証・Node/Rustの実行例はソースリポジトリに残します。MIT License。描画構成要素と外部依存の権利表示は同梱のSCREEN_COMPONENTS_LICENSE、THIRD_PARTY_LICENSES.txt、TOOLCHAIN_LICENSES.txt、RUST_STD_LICENSES.htmlを参照してください。

## 名前付きソースと再初期化

v0.2.0の`requireLoader`はextended専用です。ホストが同期で`{source,name}`を供給し、SDKが同じVMの継続で実行します。include-onceで戻り値を捨てる方式であり、Lua標準のmodule requireや静的buildとは別です。任意ファイルアクセスや再入は許可しません。

Vehicleのloadは別チャンクの追加実行です。resetは正常完了した全loadを再実行し、required modulesのキャッシュも再作成します。Addonのloadは初回だけとし、開発用モジュールはrequireLoader経由で利用します。詳しくは[ソース読み込み](https://docs.makkii.jp/storm-lua-engine/source-loading)を参照してください。


## 最適化後のソースマップ（v0.3.0）

`/compiler`の`loadCompiler()`から、`compiler.minify(source, {sourceMap:true, sourceName:'controller.lua'})`を呼びます。成功時の`code`と`map`を一組で保存し、`compiler.validateSourceMap(code,map)`で検証済みの`OptimizationMap`を取得してください。標準Source Map v3と、最適化理由・関連元・inline文脈・削除記録を持つ`x_storm`が同じmap JSONに入ります。

`build(project,{minify:true,sourceMap:true})`と`buildLifeboat`、CompilerWorkerClientも対応します。通常のminifyはmapを明示指定したときだけ詳細追跡します。mapなしと生成Luaは同じで、mapを8192文字制限のLuaへ埋め込む必要はありません。

標準readerによる表示、UTF-8バイト範囲とエディタのUTF-16位置の変換、結果の保存、VMの生成行との接続はホストが担当します。SDK更新だけで既存エディタの画面やブレークポイントが自動的に元位置へ切り替わるわけではありません。実行環境が行しか返さない場合は列を推測せず候補を表示してください。

マップには原文全文と固定化した設定を含みます。共有先を確認し、生成後のLuaに別の編集・minify・前置きを加えた古いmapは使わないでください。詳細な利用方法とNodeの実行例は[Source Mapガイド](https://docs.makkii.jp/storm-lua-engine/source-maps)にあります。
