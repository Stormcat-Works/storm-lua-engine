# Public API and support levels

利用側の手順は[Getting started](../guide/getting-started.md)、メソッド一覧は[consumer API reference](../guide/api-reference.md)。ここでは所有者と保証範囲を定義する。

## 実装と所有者

| Layer | Public surface |
|---|---|
| Rust spec | Composite/property/draw/map/HTTPの契約、binary command decoder、vehicle/addon別catalog、ABI constants |
| Rust raster | `ScreenRaster`、`ScreenSink`、同梱font、scalar描画、任意の`MapProvider` |
| Rust VM | 制限付き実行、`call_with`、`LuaValue`／`HostFunction`、構造化logging、optional debugger |
| Rust vehicle | `Microcontroller`のload/tick/draw/reset、signal/property、命令列、HTTP要求・返信 |
| Rust addon | `Addon`のload/start/tick/dispatch/destroy、checkpoint/reload、menu property、server登録、matrix、HTTP |
| TS main | `loadRuntime`、`fromEmscripten`、`LuaEngine`、`VehicleVm`、`AddonVm`、保存・値ヘルパー、モード別catalog |
| TS specialized | `/raster`、`/raw`、`/commands`、`/debug`、`/canvas` |
| Rust compiler | syntax、analysis、minify、buildの公開API。実行VMを初期化せずに利用できる |
| TS compiler | `/compiler`、`/compiler-worker`。解析・最適化・プロジェクトビルドとWorker経由の操作 |

`engine.createVehicle()`と`engine.createAddon()`は異なる型を返す。addonにはComposite I/Oと描画APIを公開しない。WASMの単一generational registryもmodeを検査し、別profileの関数を誤って呼べないようにする。曖昧なcreateVm/LuaVmの互換aliasは提供しない。

## Availability

vehicleのAPI catalogは`VEHICLE_API_CATALOG`、addonは`ADDON_API_CATALOG`と`ADDON_EVENTS`。Rust specからTS/JSONへ生成する。標準ライブラリはVM側のallowlist、serverの追加関数はhostが提供した名前だけを登録する。

catalogのavailabilityはimplemented／host-extension／requires-providerを区別する。`drawMap`の記録は実装済みだが、そのreplayにはproviderが必要。providerが無い場合のUnsupportedは、地図画像を暗黙に生成することより優先する。effect説明はoptimizerの純粋性の証明ではない。

## Lifecycle and state

load/tick/draw/start/dispatch/resumeなどはcompleted／suspended／missingを区別し、失敗はResult／EngineErrorで返す。suspendedは同じcontinuationを保持する。未定義callbackはmissingであり、Lua処理を実行したという意味ではない。

Vehicleのloadは現在の環境へチャンクを追加実行し、正常完了した全チャンクをresetで順に再実行する。失敗・未完了のloadは履歴へ追加しない。新しい独立プログラムには新しいvehicleを作る。Addonは明示的な段階制約を持ち、top-level→checkpoint復元→onCreate→イベントの順。詳細は[Addon contract](addon.md)。

ログはVM内のbounded queueからホストへ渡す。TSのonLogはLuaから戻った後の配送。HTTPは要求・返信を分離し、同期server queryと混同しない。同じmoduleへの再入、ホスト関数のPromise結果を拒否する。

## Memory and unsupported scope

Compositeはf32固定I/O、画素は借用FrameLeaseまたは明示copy。Lua/host/saveの値はi64/f64/bytesを区別する。Addonの複合データにはcold-path codecを使用し、ゼロコピーを保証しない。[ABI](wasm-abi.md)

実world、全server APIのゲーム側実処理、地形データ、ネットワーククライアント、VFS/IDE project管理、ゲームsaveXMLはエンジンが所有しない。matrix.rotationToFaceXZなど未確認の関数をstubで埋めない。ゲーム挙動の全edge caseの実測済みを主張せず、[TASKS](../../TASKS.md)に検証範囲を残す。

開発用のrequireLoaderはextendedの明示機能であり、ホストがソースと名前を供給する。[source loading契約](source-loading.md)がキャッシュ・継続・予算・load履歴を所有する。
