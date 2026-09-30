# Changelog

## Unreleased — v0.3.0 development

- 関数localの共有スロット化・不要table nil代入除去で、無関係な構文まで由来を失う問題を修正。ローカル式/リテラルの置換、hybrid globalization、一時global packingでも各宣言・参照の元位置を保持する。短縮結果と候補選択は維持する。

- 内部最適化APIに由来追跡を追加し、候補・Workerと位置情報を一体で保持。未対応変換は明示Unknown。標準のminify Source Map SDK公開は後続。
- 低レベルAst.nodesは位置失効を管理するNodeArenaへ変更。元の構文的比較と通常のJSON形は維持するが、binary Worker contextは同じcompiler版で使用する。

- Parserで式全体と名前出現の範囲を保持し、Printerへ最終生成byte範囲の記録を追加。原文・生成位置の接続基盤であり、最適化後minify mapはまだ提供しない。

最適化後minifyソースマップはv0.3.0で実装する。0.2.1には含まない。

## 0.2.1 — 2026-09-30

- `targetSize`が未達の場合、同じ意味論・pass設定の最大探索（`searchMode:exhaustive`、targetなし）と同一の生成コードを返す。`searchMode:fast`やbeam幅の指定で未達時の品質を落とさない。targetなしのfastは従来どおり候補を限定する。
- 低レベルRustの`LinkedRange`にbyte範囲を追加。opaqueなWorker search contextは同じ版のcompiler間で使用する。描画命令ABI、Composite I/O、savedata形式は維持する。


- `targetSize`未達時の探索を全探索と共通化し、実行済みのコア最適化・候補評価を繰り返さない。Worker継続でも評価済み候補を保持する。時間による探索打ち切りは導入しない。
- 非短縮の通常/LifeBoatビルドのSource Mapをトークン・列単位へ詳細化し、UTF-16座標へ変換する。診断の元モジュール・列・終端も実際のコピー範囲から求め、合成コードと削除済み開発区間は未対応位置として明示する。最適化後mapはまだ返さない。

- IDE用のトークン/AST宣言検査、明示的LB include-onceビルドと開発専用区間除去、描画の開発用overflow viewportを追加。このIDE向け追加は通常game描画を変更せず、minify後ソースマップの実装とは分離する。

- Lua実行で利用できる可変長引数のみの`function(...)`をCompilerも正しく受理する。旧TypeScriptパーサー由来の閉じ括弧の消費漏れを修正。

- 識別子位置で記号や予約語を受理していた構文解析を修正し、`local =`等を構文エラーとして報告する。

- Vehicleの名前付きtick/draw callback、extended限定の明示controlNamespace、更新済みpropertyスナップショットを追加。同一Lua呼び出し内の状態変更をWASM再入なしに標準APIへ反映する。

- ログ生成時のチャンク名と実行行をRust LogRecord、構造化WASM、TS LogRecord.locationへ追加。Luaへのdebug API公開や常時line hookを追加しない。
- 取得不能な位置は省略し、旧ランタイムの位置なしログもTSで受理する。最適化後ソースマップを提供する変更ではない。
- ログの64KiB蓄積上限へチャンク名のbytesも含める。RustでLogRecordを直接構成する利用者はlocationフィールドが必要。

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
