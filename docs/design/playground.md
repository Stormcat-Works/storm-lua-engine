# Storm Lua Engine: Playground

決定日: 2026-09-26。**CLI/Web・Worker設定を実装し、Addon Labの移行・撤去を完了。** 初回の公開・配備は[v0.2.0の公開記録](../verification/release-0.2.0.md)を参照する。操作は[app guide](../../app/README.md)、検証は[Playground検証](../verification/playground-20260926.md)。SDKの実装状況は[STATUS](../../STATUS.md)、判断理由は[ADR 0006](../adr/0006-playground-coexistence.md)を参照する。本文はPlaygroundの責務・範囲・移行の正本であり、公開済みの利用ガイドではない。

## 1. 目的とStorm Minとの併存

正式名称は **Storm Lua Engine: Playground**。Engineの公開SDKの全機能を実際に試し、何ができるか、どう呼び出すか、結果や失敗がどう返るかを確かめるためのCLI/Webとする。それを超える独立製品機能は追加しない。

Storm Minは短縮・ゲーム向け出力に特化した既存CLI/Webを維持する。PlaygroundはStorm Minの改名、移設、後継、廃止理由ではない。Storm Minのコマンド、Web画面、配布、保存データ、公開ルートをPlayground追加のために削除・変更しない。

共通アルゴリズムはSDKに一つだけ置く。同じminify APIを二つのフロントエンドから呼ぶことは許容する。共有するのは言語処理・実行・描画・汎用接続アダプタであり、二つのアプリを同じUIや同じ既定操作へ統合する必要はない。Storm Minの非公開コーパス・回帰ハーネスも維持する。

## 2. 配置と配布の所有者

| 配置 | 所有するもの |
| --- | --- |
| `app/` | Playgroundのアプリ、アプリ用設定、公開・配備に使う設定と手順 |
| `app/cli/` | Node/TypeScriptで同じWASM SDKを明示駆動するCLI。Webと操作処理を共有 |
| `app/web/` | SDKを試すWeb画面、画面状態、ブラウザ側の接続・操作 |
| `app/wrangler.jsonc` | makkii.jpでLua Engine / Playgroundを公開するCloudflare Workerの設定 |
| `app/`配下の必要な配信コード・スクリプト | 上記Workerに実際に必要な処理、静的成果物の梱包・公開手順 |
| 既存の`crates/`・`packages/lua-engine/`・`tools/` | 共通SDKとSDK自身のビルド・検証。アプリ専用処理は増やさない |
| `examples/` | 小さく独立した組み込み例。大型Addon LabはPlaygroundへ整理する |

ルート直下に新しい`cli/`、`web/`、Playground専用のWorker設定や配備スクリプトを追加しない。SDKは`app/`へ依存せず、アプリ依存やWeb資産をSDK利用者へ必須にしない。CLI/Webも通常の公開SDK APIを使い、内部ASTや非公開fixtureへの特権アクセスを前提にしない。

現在の起動・CLI引数・確認例は[app/README](../../app/README.md)を参照します。Worker名は`storm-lua-engine-playground`、設定routeは`www.makkii.jp/tools/stormworks/storm-lua-engine/*`です。設定・dry-runの完了と実際の公開は区別し、既存Storm Minのrouteを変更しません。

### 二種類のWorkerを区別する

Cloudflare Workerはサイト公開のためのもの。設定と必要な配信処理は`app/`で管理し、HTML/JS/WASM/公開例を配信する。利用者のLuaをサーバーへ送るcompile/run APIは追加しない。静的配信で要件が満たされる場合、不要なWorkerハンドラは作らない。

ブラウザのWeb Workerは利用者端末上でコンパイラや実行SDKを駆動するもの。アプリが明示的に生成・停止・破棄し、汎用の通信や候補処理は必要に応じSDKの共通アダプタを利用する。配信用WorkerへコンパイラやLua実行を移す意味ではない。

SDKはサイトのホスト名・配備パスを持たない。実際の公開時にサブパス、WASM/Worker URL、MIME/CSP、キャッシュ世代とloaderの版一致を検査する。Storm Minの公開Worker・routeを上書きしない。

## 3. 全機能を試せることの定義

公開SDKの利用者向け機能ごとに、操作方法、入力例、結果の確認方法、対応環境を用意する。成功するボタンだけでなく、未対応・誤入力・実行制限などの失敗も確認できるようにする。内部最適化パスごとに専用画面を作ったり、内部ASTの補助関数すべてをGUI化したりすることは目的にしない。

以下は実装開始時の機能群。詳細な公開面は[利用側API](../guide/api-reference.md)、[compiler guide](../guide/compiler.md)、モード別APIカタログと実exportを照合して確定する。操作/例/テストの実装対応は[app guide](../../app/README.md)と[検証](../verification/playground-20260926.md)を参照します。低レイヤーRust専用のprobeはconformanceの独立実行例で試します。

| 機能群 | Playgroundで試す内容 |
| --- | --- |
| 初期化と独立利用 | runtime / raster / compilerの明示ロード、提供されたmodule/bytes、生成・破棄、不要モジュールをロードしない利用 |
| ソース処理 | analyze、単一ソースminify、複数モジュールbuild、非短縮リンク、ambient、診断、source map、property走査、pass一覧・レポート・各設定 |
| Vehicle実行 | load、全32chの数値/Boolean入力と出力、tick、複数サイズのdraw、property初期設定・更新、reset、出力保持 |
| 描画単体 | DrawCommandの入力・encode、Luaなしraster、色・図形・文字、frameの借用/所有コピー、Canvas表示、地図providerの接続 |
| Addon実行 | load/start/tick、型付きイベントdispatch、destroy、menu property、server関数の登録・呼び出し・複数戻り値、matrix機能 |
| デバッグ | breakpoint、continue/into/over/out、stack、locals、upvalues、table展開、watch、停止世代と無効化、明示的なwatch評価 |
| 値と保存 | bytesと文字列、i64、非有限数・負のゼロ、Lua table、savedataのencode/decode/reloadと不正形式のreject |
| ホストサービス | print/debug.log、ログ取り出し・配送、HTTP request/reply/cancel、map provider、提供したhostサービスの失敗 |
| 制限・低レイヤー・接続 | instruction/memory budget、missing/error/busy、公開raw境界のmode/handle/lifetime、Worker転送と各プラットフォームの対応差 |

一つの汎用的な入力/出力パネルや、小さな実行例で複数APIを試せてよい。GUI操作が適さない公開低レイヤー機能は、CLIや組み込みprobeで直接試せる入口を用意する。CLI/Webの能力差は明示し、試せない公開機能を黙って除外して「全機能対応」としない。未実装のSDK機能は対応外の理由を示すだけとし、Playgroundのために架空の実装を足さない。

完了判定には公開機能群と操作/実行例/テストの対応表を使う。Nativeのみ、ブラウザのみなどの対応条件と未完了項目を記録し、後からSDKの機能が追加された場合も対応表を更新する。この表はUI用の新しいゲームAPIカタログや意味論の第二正本にしない。

SDKではAddon実行に加え、[buildAddon](../specs/addon-build.md)による非短縮リンクと構文・環境検証を利用できる。Addon最適化と全server APIの解析は未提供である。PlaygroundではSDKの対応範囲と実際に試せる操作を区別し、操作・例・テストの対応を上記の対応表で管理する。未提供のAddon最適化をPlayground完成の前提にはしない。

## 4. 操作とホストの境界

解析、ビルド、最適化、ロード、実行は別の明示操作にする。解析で実行VMを更新せず、ビルドでLuaを実行せず、ロードで暗黙にminifyしない。原文・非短縮ビルド・最適化結果のどれを実行するかは利用者が選ぶ。単一Luaの直接ロードも維持する。

CLI/Webが入力、tick/drawの進行、停止、表示を管理する。SDKへアプリ専用のスケジューラやワールドを追加しない。必要な汎用loader、Worker通信、診断位置変換を各ホストへ重複実装させない一方、用途のない抽象や共有UI基盤も追加しない。

Addonのserver関数や地図、HTTP返信は、入力可能な最小のテストホスト・固定の明示サンプルで確認する。これはユーザーが選んだテストデータとして表示し、未提供のAPIや本当の外部通信を成功stubへ置換しない。ゲーム世界全体のserver APIを実装する必要はない。HTTP送信は自動実行せず、Playgroundの基本範囲はrequestの観測と明示reply/cancelにとどめる。

入力ソース、設定、入力列など再現に必要なアプリ状態は一つの正本で持つ。ローカルリロードとversion付き入出力を共通のアプリ規約に沿って扱い、保存失敗や非互換形式を隠さない。完全なVMメモリや停止位置が保存できるとは説明しない。アカウント、クラウド同期、共同編集、プラグイン、IDEのワークスペース管理は追加しない。

## 5. Addon Labの移行・廃止（完了）

`examples/addon-lab`は独立アプリとして削除し、SDKを試すために必要な部分を`app/`のPlaygroundへ移行する。単なるディレクトリ移動・名称変更で、港と仮物理のアプリを存続させるものではない。

| Addon Labの要素 | 扱い |
| --- | --- |
| `session.ts`の実Addonロード・イベント・ログ・savedata接続 | PlaygroundのAddon操作へ整理して再利用し、公開SDK経由を維持 |
| `host.ts`のhost登録・戻り値・イベント配送の例 | SDKを示す最小の明示テストホストへ縮小。船・世界の業務ロジックは持ち込まない |
| ソース編集・入力検査・失敗時の保全 | 必要な薄いUI・再現データとして選別。汎用IDEへ拡張しない |
| `world.ts`、`scene.ts`、船団巡回・プレハブ・港・仮物理 | Playgroundの要件に含めず、旧アプリと共に削除。3D表示やThree.jsを維持するための用途を作らない |
| SDK接続・保存・エラー処理のテスト | 移行先へ適応して残す。仮物理専用の期待値は理由を記録して撤去 |
| 旧project形式 | 自動変換を追加しない。非互換は明示し、旧データを黙って受理・上書きしない |
| CI・配布ZIP・ライセンス・紹介リンク | app側の実装・テスト・梱包に合わせて同じ移行単位で更新 |

先に残す操作とテストを対応付け、移行先で動かし、不要なシミュレーション部分と旧ディレクトリを削除する。旧Labを削除しただけで全SDKのPlaygroundが完成したとは扱わない。長期の二重開発はせず、移行完了時には旧アプリへの実行/ビルド参照を撤去する。

既存の小さなNative/Node/browser組み込み例とSDK本体のconformanceは別資産として維持する。過去の検証記録は新アプリの成功証跡へ書き換えない。

## 6. 完了条件と公開

各機能群が実SDKを呼び、入力から結果・失敗まで確認できる。CLI/Webの起動・入出力・明示操作、ブラウザのWorker分離、ロード対象、停止・破棄、保存形式のrejectを検証する。担当者自身も実画面・CLI出力と生成コードを確認し、ボタンの存在やテスト0件で完了にしない。

Addon LabのSDK確認部分を引き継ぎ、旧アプリと不要依存・古いCI/梱包参照を削除する。Storm MinのCLI/Webは維持する。SDKのruntime-only / compiler-only / raster-only依存分離と、アプリ資産をSDKへ混ぜない梱包を確認する。

makkii.jp向け配備は`app/`で管理する。正確なroute・版・公開手順を確認してから明示的に実行し、今回の設計決定をデプロイ実施として扱わない。ガイドはSDKの利用契約とPlaygroundの操作を分け、ブログではStorm Minの廃止・置換ではなく、共通SDKと新しい確認用アプリの追加として説明する。
