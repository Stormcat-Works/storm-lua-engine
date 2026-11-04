# Integration patterns and migration

実装済みのcompiler入口は[Compiler guide](../guide/compiler.md)、検証は[統合検証](../verification/compiler-integration-20260926.md)を参照する。全体の言語処理統合と公式フロントエンド案は[Compiler SDK統合設計](compiler-sdk.md)を参照する。以下はv0.1.0時点のconsumer接続方針であり、統合済みAPIを示すものではない。

公開ガイドは特定の私有アプリや内部パスを前提にせず、用途で分類する。実行できる[組み込み例](../../examples/README.md)と、下記のconsumer移行方針を区別する。[API状況](../specs/api.md)を参照。

| Consumer | Runtime dependency | Typical use |
|---|---|---|
| Rust Lua optimizer | specのみ必要なら通常依存。vm/microcontroller/rasterは検証用dev依存 | API catalog、ゲーム互換の実行差分 |
| Web IDE | npm runtime entry | 実行・入力・描画・debug。UI/VFSはIDE側 |
| Image-to-script tool | npm raster、必要な場合runtime | 命令候補の描画、生成Luaの実行確認 |
| Editor extension | npm runtime/raster | WebView内の実行・モニター表示。デバッグプロトコル接続はhost |
| Addon host / mission test | npm createAddon / Cargo addon | explicit server functions、game events、保存・復元 |
| Rust simulation/renderer | Cargo microcontroller/rasterを個別選択 | 独自スケジューラ・ScreenSink・CPU frame |

## Optimizer: direct mlua dependency or engine?

**ゲーム互換の差分テストは共通エンジンを経由する。** 独自にmock input/output/property/screenを登録したmlua環境を各ツールが保持し続けると、f32の境界や描画規則が分岐する。移行単位はcrate名の置換ではなく、検証harnessの置換とする。

通常コンパイルにLua実行が不要なら、エンジン実行系はdev-dependencyのまま。WASMコンパイラの製品依存へvendored LuaやEmscriptenを持ち込まない。ゲームAPI定義だけ必要ならspecに依存する。

**汎用Luaの言語仕様を独立に検査するテストは直接mluaを残してよい。** Lua全体の意味論を確認するテストにマイコンのallowlistやtick制限を強制しない。独立oracleをすべて同じharnessへ揃えると共通バグを検出しにくくなる。

`use mlua`を`use engine::mlua`へ機械的に置き換えるだけでは意味論の共通化にならない。raw backend escape hatchは必要なテストだけで用い、独自のゲームAPI mockを温存するために使わない。両経路が共存する間は同じmlua版・featuresを揃えて、二重backend/feature統合を確認する。

## Suggested order

共有エンジンのvehicle/addon実装と独立実行例は用意済み。次は最初の実consumerの既存APIを明示的なhost adapterへ接続し、同じscript・入力列・イベント列で比較する。ゲーム互換と汎用Luaの検証を分け、必要な独立oracleを残す。新たなserver APIを増やす場合も、まず実consumerに必要な関数から引数・戻り値・失敗条件を固定する。

旧harnessと新harnessを比較する際は、違いを必ずしも新側のバグと断定しない。正本仕様と第三のfixtureで判定する。描画の違いを解消するためにalphaやI/O精度を旧側へ戻すことはしない。
