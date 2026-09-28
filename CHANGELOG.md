# Changelog

## Unreleased

### 修正

- 描画: 1/256pxの格子点どうしのちょうど中間（k+1/512）に乗った座標を、画面の大きさに応じたf32の頂点計算で丸める。drawCircleFの水平な辺がサンプル行に乗った場合、下の辺の行を点灯し、上の辺の行は点灯しない。いずれもStormworks v1.15.23の撮影結果に合わせた。画面契約は5ケースの期待値を更新し、撮影ページ由来の12ケースを追加して743ケース。

## 0.2.0 — 2026-09-27

Compiler SDK、実行環境の選択、開発用ソース読み込み、Playgroundをまとめたリリースです。

### 追加

- Vehicle向けCompiler SDKを責務別クレートと`/compiler`へ統合。解析、lint、静的リンク、minify、プロパティ走査、独立したコンパイラWASMを提供。
- ホスト所有のWorkerへ接続する`/compiler-worker`アダプタ。
- 任意のホスト値・同期関数を高レベルAPIから追加・置換する`bindings`。
- extended専用の`requireLoader`。ホストが供給するテキストを別名付きチャンクとして共有環境で実行し、include-once・戻り値破棄・入れ子・ファイル単位のデバッグに対応。
- `app/`にStorm Lua Engine: PlaygroundのCLI/Webと静的配信Worker設定を追加。Storm MinのCLI/Webと併存。

### 非互換・挙動の変更

- 既定をgame環境へ変更。debug.logは利用可能で、pcall/xpcall/error/print/assert/グローバルunpackはextendedへ分離。onLogは公開関数を増やさない。
- Vehicleのresetは最後のソースだけでなく、正常完了した全loadを順に再実行。失敗・未完了のloadは履歴に入れず、履歴を128件/8MiBに制限。
- 単一ソースとプロジェクトの環境診断を統一。未知・廃止のパスIDはfalse指定でも拒否。
- general-expression-factoring、repeated-expression-factoring、scalar-vector-loop-synthesis、redundant-nil-fallback-eliminationを撤去。
- 独立Addon Labを削除し、SDK確認をPlaygroundへ移行。仮物理/3Dワールドは移さない。

### 修正

- ソースに代入のない外部グローバルを短名またはnilへ変換しない。
- 動的_ENV・拡張環境・ホスト置換は、トークンと行位置を保持する字句短縮へ切り替え、conservative-minificationで通知。
- クロージャに捕捉されたproperty/inputの値を後から再取得する変換を防止。
- 不正な元ソースを公開finalizerへ渡した場合のpanicを診断へ変更。
- 非公開fixtureの生成を通常コンパイラビルドから分離。

- 非短縮リンクの合成行を架空の元行へ割り当てない。元ファイルのbreakpoint・step・エラー位置を往復するconsumer例とNative/WASM回帰を追加。

### ドキュメントと配布

- PlaygroundはCloudflare Workers Buildsからreleaseブランチpushでビルド・自動更新。設定とビルド処理はapp/で管理。npm公開は独立した明示操作。
- 隔離パッケージのSource Map検証はlockfileのtarballを再利用し、新規CIのnpm metadata cacheに依存しない。

- 利用者向けガイド本文を[docs.makkii.jp](https://docs.makkii.jp/storm-lua-engine/index)へ移設。リポジトリには契約・設計・検証・公開手順と実行可能な例を維持。
- Rust workspace、npm SDK、Playgroundを0.2.0に統一。savedata形式、描画命令ABI、Composite I/Oレイアウトは変更しない。


## 0.1.0 — 2026-09-26

Storm Lua Engineの初回リリース。

- RustとTypeScript／WebAssembly向けに、Lua 5.3の実行基盤を提供。
- ビークルとAddonを独立したAPI・実行環境として提供。Composite I/O、プロパティ、コールバック、イベント、savedataに対応。
- CPUラスタライザ、ビットマップフォント、Luaを含まない描画専用WASMを同梱。731件の固定RGBAケースを直接描画と実Lua経路で検証。
- ブレークポイント、ステップ実行、変数・テーブル検査、watch、ログのホスト接続を提供。
- Addonの同期server関数、地図プロバイダー、HTTP要求・返信を利用側の実装へ接続。
- npm SDK、RustのGitタグ、Node／Rust／ブラウザの利用例、Three.jsとCodeMirrorによるAddon Labを提供。
- 配布用の権利表示、toolchain通知、WASM・export・パス検査と独立したnpm consumer検証を整備。

ゲームのワールド、物理、地形、実際のネットワーク通信はホスト側の責務です。対応APIと制約は[利用ガイド](docs/guide/getting-started.md)、追加の検証・実装項目は[TASKS](TASKS.md)、未信頼コードの実行条件は[SECURITY](SECURITY.md)を参照してください。
