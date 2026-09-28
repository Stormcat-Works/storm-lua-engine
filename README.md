# Storm Lua Engine

**v0.2.0 公開済み。** Compiler SDK、環境プロファイル、開発用require、Playgroundを提供しています。[利用ガイド](https://docs.makkii.jp/storm-lua-engine/index)と[変更点](CHANGELOG.md)を参照してください。

**StormworksのLuaを、あなたのアプリケーションで動かす。**

RustとTypeScript／WebAssembly向けの組み込みライブラリです。ビークルLua、アドオンLua、画素単位の描画、デバッグを共有しながら、UI・ワールド・通信・実行タイミングは利用するアプリケーションが管理できます。

[はじめる](docs/guide/getting-started.md) · [Addonを組み込む](docs/guide/addons.md) · [ホスト機能とログ](docs/guide/host-services.md) · [APIリファレンス](docs/guide/api-reference.md) · [実行例](examples/README.md)

## Storm Lua Engine: Playground

SDKの機能を試すCLI/Webを`app/`に実装しています。Storm MinのCLI/Webとは併存します。Vehicle、Addon、解析・最適化、描画単体、デバッガ、値・保存、ホスト拡張を実SDKで操作できます。ゲーム世界や仮物理は持ちません。[起動と操作](app/README.md)。公開・配備は別の手順です。

## インストール

TypeScript／JavaScript: `npm install @stormcat-works/storm-lua-engine`

実行用・描画専用WASM、型定義、フォントを同梱しています。利用するだけならRustやEmscriptenは不要です。[GitHub Releases](https://github.com/Stormcat-Works/storm-lua-engine/releases)ではnpm tarballと、そのまま配信できるAddon Labも配布します。

Rust: 必要なクレートを公開Gitタグで参照します。例えば`storm-lua-addon = { git = "https://github.com/Stormcat-Works/storm-lua-engine", tag = "v0.2.0" }`。詳しい設定は[導入ガイド](docs/guide/getting-started.md)を参照してください。

## できること

| 用途 | 提供するもの |
|---|---|
| ビークルLuaの開発・検証 | `onTick`／`onDraw`、Composite I/O、プロパティ、描画命令 |
| アドオンLuaの実行 | 独立したAddonモード、イベント、`g_savedata`、メニュープロパティ、`matrix.*`、ホストの`server.*` |
| 画面表示・画像生成 | CPUラスタライザ、同梱ビットマップフォント、描画専用WASM |
| IDE・テストハーネス | ブレークポイント、ステップ実行、変数・テーブル検査、watch、ログ配送 |
| 自分のワールドとの接続 | `drawMap`の地図プロバイダー、同期サーバー関数、HTTP要求・返信キュー |

**描画仕様と採用済みの入出力は、このリポジトリが所有します。** 固定した743ケースのRGBAを直接描画とLua実行の両方から検証し、下流アプリの実装へ逆依存しません。通常のビルド・実行にゲーム本体や外部フォントは不要です。[描画仕様](docs/specs/screen.md)

## 使うモードを選ぶ

**ビークル:** `const vehicle = engine.createVehicle({ properties: { Gain: 2 } });`

`vehicle.load(source)`で読み込み、`vehicle.io.inputNumbers[0] = 3.5`、`vehicle.tick()`で実行します。画面は`vehicle.draw(96, 96)`の後に`vehicle.frame()`から取得します。

**アドオン:** `const addon = engine.createAddon({ newWorld: true, server: hostFunctions });`

`addon.load(source)`、`addon.start()`、`addon.tick(1)`の順に駆動します。プレイヤーやビークルの状態は`hostFunctions`で接続し、イベントは`addon.dispatch(...)`で渡します。AddonにはComposite I/Oや`draw()`はありません。

**描画だけ:** `/raster`の`loadRaster()`を使用します。Lua VMをロードせず、バイナリ命令バッチからRGBAを生成できます。

これらは使い方の概要です。インストール、エラー処理、保存、破棄まで含む例は[Node利用例](examples/consumer/node.mjs)と[Rust利用例](conformance/examples/addon_host.rs)を参照してください。

## ブラウザで試す

[Playground](app/README.md)は通常の公開SDK APIを使う公式の確認用アプリです。小さな独立例は[examples](examples/README.md)に残しています。

## アプリケーションに主導権を残す

エンジンは勝手にタイマー、Worker、Canvas、ネットワーク接続を作りません。地図を描くなら地図プロバイダーを、ゲームの状態を読むならサーバー関数をホストが渡します。未提供のサービスを、空のプレイヤー一覧や架空の地形で成功扱いすることもありません。

`print()`と`debug.log()`は`onLog`でコンソール・IDEパネル・ファイルなどへ接続できます。ログの元データはバイト列のまま保持し、表示時の文字コード変換もホストが選びます。

Rustでは必要なクレートだけを直接利用します。TypeScriptでは1つのnpmパッケージに実行用・描画専用のWASMと型定義を収録します。Compositeは`f32`、Lua内部は`f64`／`i64`を維持し、通常のビークルI/OにJSON変換を使いません。Addonの複合データやホストサービスには、型を失わない構造化転送を使用します。

## 検証と現在の範囲

Native、Node／WASM、Chromium・Firefox・WebKit、独立したnpmインストール環境で検証します。各OS・CPU・ブラウザの実測範囲は[検証記録](docs/verification/repository.md)に分けて記載しています。対応予定と検証済みを同一視しません。

ワールドの物理計算、地形データ、`server.*`の世界に対する実処理、HTTPクライアント、ゲームのセーブXMLそのものは同梱しません。未実装のAPIと追加検証は[TASKS.md](TASKS.md)を参照してください。

## 開発に参加する

ビルド・テスト・依存境界の確認は[CONTRIBUTING.md](CONTRIBUTING.md)、内部設計は[Architecture](docs/design/architecture.md)へ。利用するだけの場合、内部クレート構造を理解する必要はありません。

MIT License。描画参照元と依存ライブラリの権利表示は[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)にまとめています。StormworksおよびGeometaとは独立したコミュニティプロジェクトです。

公開後のregistry導入・本番配備の確認は[0.2.0公開記録](docs/verification/release-0.2.0.md)、Webは[Playground](https://www.makkii.jp/tools/stormworks/storm-lua-engine/)を参照してください。
