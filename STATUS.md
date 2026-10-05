# Current status

## v0.3.1 印字・数値読取り・名前保存の検証完了（2026-10-05）

`3f342dc`・`5736d0b`・`f3e5c87`を検証し、[印字と名前保存の記録](docs/verification/compile-emission-20261005.md)へ保存した。直前の`627f8d1`と比較し、全探索4,054本の平均はMapなし1.9%、Mapあり0.7%短縮。1.2倍の全入力目標は未達で、負の再測定も記録している。Native841、SDK30、実WASM83、Playground/CLI38、3ブラウザーと完全map比較を確認。push・公開は行っていない。

## v0.3.1 候補評価と表記生成の追加改善（2026-10-05）

`627f8d1`までのAPI別名候補の改名AST削減、小整数表記、重複由来関係の確認を完了。[実装・回帰・実測](docs/verification/compile-latency-20261005.md)。比較元は`353522c`。同じ実コーパス全探索の平均ではMapなし8.2%、Mapあり6.8%短縮。平均比1.135倍を、個別の最大1.2倍の達成に置き換えない。未達・再確認でも残った負の差・採用しなかった試行は記録した。

Native836件、SDK30件、実WASM83件、Playground/CLI38件と3ブラウザー、固定240設定・実コーパス8,182成功組・全map一致を検証済み。今回この引継ぎ記録を保存する。実装はv0.3.1ブランチだけで、push・公開は行わない。

## v0.3.1 依存解析と構文比較の高速化（2026-10-05）

`353522c`までの関数summary依存の再利用、文字列snapshotなしの構文比較、安定aliasの借用、ASCII文字数計測を検証済み。[実装と実測](docs/verification/compile-summary-dependencies-20261005.md)。全探索4,054成功組の平均は前の検証済み版よりMapなし3.0%、Mapあり1.9%短縮。最大1.2倍は未達であり、負の反復観測も残している。

Native834、SDK30、実WASM83、Playground/CLI38、3ブラウザー、全mapを含む固定240設定・実コーパス8,182成功組を確認した。公開版ではなく、v0.3.1開発ブランチのcommitのみ。次のラウンドはこの同一binaryを基準にする。

## v0.3.1 全経路高速化の継続（2026-10-05）

`feat/minify-performance-v0.3.1`で、借用したAPI参照の分類、ASTの葉/演算子snapshot、1文字の短縮名、UTF-16座標cursorと字句識別子の共有を実装した。対象実装は`0de60afd5b4ee106a9cc48be3ad24c1c099fcbe5`。[実装・実測・回帰・未達条件](docs/verification/compile-continuation-20261005.md)。

比較元は前回の検査済み`a400c55`。今回のコーパス全探索4,054成功組の平均ではMapなし2.1%、Mapあり1.1%短縮、Map比は1.126。平均比を全入力の最大条件に置き換えない。反復した成功40条件の1.2倍以下は11、最大中央値比は4.779で、未達と負の観測を残す。

Native827、SDK30、実WASM83、Playground/CLI38件、3ブラウザー各16例、固定240構成と実コーパス8,182成功組で同一結果を確認した。ブラウザー試験では一時領域不足を確認し、同じ配布物を専用TMPDIRで再実行して成功。アプリ/依存/安全設定は変更していない。commitのみで、push・公開・main/release変更はしていない。

## v0.3.1 追加高速化の検証記録（2026-10-04）

前回報告後の実装 `f463b72`〜`a400c55` とその比較を[追加ラウンド](docs/verification/compile-next-round-20261004.md)へ保存した。819 Native、83実WASM、実コーパス8,182成功組と全マップの一致を確認。全探索の平均時間は直前版よりMapなし3.0%、Mapあり2.2%短縮したが、最大1.2倍は未達のまま。失敗した試案と負の観測を除外せず記録する。公開・pushは行っていない。

## v0.3.1 全コンパイル経路の高速化（開発中・2026-10-04）

`feat/minify-performance-v0.3.1`を維持し、Mapなし/ありの両方を速くする改善を`2741357`〜`1f9d58e`へcommitした。[計画](docs/design/compile-all-scenes-v031.md)、[全条件の実装・測定・検証](docs/verification/compile-all-scenes-20261004.md)。**push・公開・main/releaseへの反映はしていない。**

blockが変わらない区間の束縛/効果解析共有、借用トークン、parse結果の診断/探索/buildへの再利用、使い終えた位置indexの早期解放、字句短縮専用経路の不要な由来生成回避、標準mapと理由JSONの一回出力を実装した。生成Lua・候補選択・診断・schema1の意味と精度を維持する。

直前のAPI別名改善版との全探索の成功4,054本比較では、Mapなし平均が24.6%、Mapあり平均が22.9%短縮した。全コーパスは各variant1回、別に44条件の反復と小projectの追加反復を実施した。**最大1.2倍（理想1.1倍）は未達**。成功40条件の最大中央値比は4.594倍で、小さい原文返却・字句短縮などの固定費を残す。通常経路の負の観測も保存し、平均値だけで全シーン達成としない。

Native 806件、SDK 30件、実WASM 81件、Playground/CLI 38件、3ブラウザー各16例、独立パッケージ導入を検証した。固定240設定と未達target、全4,155原文の成功/失敗境界、新旧reader、Worker混成を確認。既知の全探索WASM trap7原文は別に確認し、今回修正したとは扱わない。

先行するAPI別名候補の改善は`153a8c9`、基盤は`d3bd35c`。そのときの平均Map追加時間66.3%削減は[別の比較](docs/verification/map-delta-reduction-20261004.md)であり、今回の効果へ再加算しない。v0.3.1はまだ未公開。

## v0.3.0 公開完了（2026-10-02）

`feat/minify-source-provenance-v0.3`で全67パスの内部由来対応に加え、最適化理由付きSource Map v3をRust/WASM/TSの開発APIへ接続した。`sourceMap:true`で`map`を返し、`validateSourceMap(code,map)`で内容指紋と詳細/標準mapの整合性を確認する。通常/LifeBoatの複数ファイル合成と、Worker経由の取得も対応。[実装・検証・性能](docs/verification/optimization-explanations-20261001.md)。

旧実測例で課題だった保持演算子/keywordのToken位置、identity/copy契約、型付き関連元、インライン化された子の文脈、原文の除去/置換記録を実装した。[更新した実測例](docs/verification/provenance-examples-20261001.md)。Source Map本体のversion=3とx_storm.schemaVersion=1を分離し、producerのエンジン版・revision・dirty、code/source hashと実効設定を保存する。

全67パスの対応と、固定30入力・240設定のUnknown 0%/生成Lua一致は維持する。理由コードと構造操作は保持するが、全パスの適用根拠を形式的に証明するものではない。basis/factsの詳細は実際に記録した判断のみで、未記録の根拠を推測しない。Source/Derivedの名前・式・文などの精度と、Synthetic/Unknownを区別する。

**P0〜P5とリリース前の利用検証・梱包・互換性棚卸しを完了した。** identity/copyの文字選択・カーソル・runtime行の範囲を正確に保つ仕上げも完了した。Unicode/CRLFと読み取り専用欄のキーボード操作、保存復元を3ブラウザーで再確認。[最終確認](docs/verification/p5-final-selection-20261001.md)。 Playgroundで元snapshot/生成Luaの双方向選択、複数由来と理由・除去記録を表示し、実VMのpause/step/log/errorへ接続する。実行時に生成行しか得られない場合は列を捏造せず候補を示す。入力編集後の旧map、保存されたVM結果と新しい実行を混同しない。[P5の完成確認](docs/verification/p5-playground-20261001.md)、[アプリ契約](docs/specs/playground-source-maps.md)。

CLI/Webの16確認例、workspace export/import、IndexedDBの大容量map保存・復元を検証済み。v0.3.0向けの[限定性能ラウンド](docs/verification/source-map-performance-20261002.md)を完了した。サイズ比較にしか使わない候補での由来処理を減らし、生成Luaとschema1の内容を維持。追加改善はv0.3.1以降へ送る。元変数値・寿命・bytecode PC・消えた実frame・loop反復の動的復元は別機能として未実装。**v0.3.0をnpm・GitHub・Playgroundへ公開し、registry再導入と本番3ブラウザーの確認まで完了した。** npm latestは0.3.0。[公開記録](docs/verification/release-0.3.0.md)に対象commit・配布物・実行結果を残す。[確定設計](docs/design/optimization-explanation-schema.md)、[公開スキーマ契約](docs/specs/optimization-map-extension.md)。

## v0.2.1 公開状況

2026-09-30: GitHub Release・タグ・検査済みSDK tarballとPlaygroundの本番配備は完了した。**npmもTrusted Publishingで公開し、latest=0.2.1とregistry再導入を確認済み**。未達targetはfast/beam設定によらず最大探索と同一出力を返す。513 Native、実WASM、3 OS CI、716未達比較、本番3ブラウザを確認。[公開完了記録](docs/verification/release-0.2.1.md)。

npm公開workflowは実公開まで成功した。[run 36667537171](https://github.com/Stormcat-Works/storm-lua-engine/actions/runs/36667537171)のattempt 1で公開し、attempt 2で同一版の再公開を省略してregistry再取得・consumerを確認した。[自動化の検証記録](docs/verification/publication-workflow-20260930.md)。v0.3.0は専用branch `feat/minify-source-provenance-v0.3`で開発中であり、本公開とは分離する。

以下の0.2.0と統合ブランチの記述は過去の実装・検証記録であり、現在の公開状態は本節が正本である。

2026-09-27 — **Storm Lua Engine v0.2.0をnpm/GitHubへ公開し、Playground・ガイド・2記事の本番公開まで完了。** 版番号はworkspace、npm SDK、Playgroundとも0.2.0です。範囲と公開手順は[release](docs/release.md)、残件は[TASKS](TASKS.md)、利用ガイドは[docs.makkii.jp](https://docs.makkii.jp/storm-lua-engine/index)です。

## 後続ブランチの作業（未公開）

`feat/storm-code-integration`でログ発生位置を実装・検証済み。Rustの生成時記録、WASMの構造化配送、TSのoptionalなlocationを接続する。契約は[runtime-host](docs/specs/runtime-host.md#ログ発生位置未公開の後続版)。これは公開済みv0.2.0の機能ではなく、Storm Code本体のTerminalへの接続はconsumer側の後続作業。最適化後ソースマップはこの作業に含めない。

検証: Native478件、実WASM53件、SDK JS29件、3ブラウザ、隔離パッケージconsumer、fmt/clippy/default check/rustdoc/architectureを通過。[記録](docs/verification/log-locations-20260929.md)。

## Storm Code実行ホストの接続準備（未公開）

同じ専用ブランチで、名前付きVehicle tick/draw、extended限定controlNamespace、更新後propertyスナップショット、analyzeのruntime/build区分を追加。Designと通常実行で標準APIを再実装せず利用するための公開操作。新しい状態操作を行った場合だけ固定I/Oへ戻し、描画やloadが未消費のホスト入力を上書きしない。識別子位置で記号や予約語を受理していた既存構文不具合も修正した。詳細はruntime-host仕様。[検証記録](docs/verification/vehicle-host-controls-20260929.md): Native487、WASM60、SDK JS30、3ブラウザ、梱包consumerと各静的ゲート通過。

## v0.2.0に含む実装

| 領域 | 現在の範囲 |
| --- | --- |
| Runtime | Vehicle/Addon、信号・プロパティ、描画、ホストデバッガ、保存、明示HTTP・地図・server接続 |
| Compiler | Vehicle用syntax/analysis/minify/buildと独立WASM・TS入口。Storm MinのCLI/Webは同じSDKを利用 |
| 環境 | gameが既定、extendedは明示選択。debug.logは両環境、print等はextended。ログ配送先と関数公開を分離 |
| 正確性 | 4パス撤去、外部グローバル保持、_ENVと拡張環境の字句短縮、捕捉値の寿命と不正finalizer入力の修正 |
| ホスト拡張 | 初期化前のvalues/functions bindings、reset/reloadで再適用、専用compiler Worker接続 |
| ソース読み込み | extended専用のLB方式requireLoader。Vehicleは正常完了したload履歴をresetで再実行 |
| 非短縮Source Map | game向けプロジェクトをbuild(minify:false)し、元ファイルの停止位置・エラー位置へ戻す。行単位、合成行は原文位置を捏造しない |
| Playground | app/のCLI/Web、13確認例、明示Worker、入力・結果保存、入出力、中断。Storm Minと併存。Addon Labは撤去済み |
| 文書 | 利用ガイドはdocs-site、契約・設計・検証・実行例は本リポ。ブログの初回版/次版記事も公開済み |

## 候補の最終確認

実装commit `8707a80`に対する41種の最終ゲートがすべて成功しました。Engine Native472件、WASM48件、SDK JS27件、Playground15件＋3ブラウザ各13例、Storm Min Rust492件／独立JS25件と新しいNative/WASM400条件を再検証。[0.2.0 candidate](docs/verification/release-candidate-0.2.0.md)と[機械記録](docs/verification/release-candidate-0.2.0.json)へ集約しています。過去の件数や出力一致を、新しい候補の実行結果として使い回していません。

既存の検証記録: [source loaderと0.2準備](docs/verification/source-loading-20260927.md)、[環境修正](docs/verification/environment-contract-20260926.md)、[Playground](docs/verification/playground-20260926.md)。初回移管の685条件一致はその時点の証跡であり、後続の正確性修正後の出力保証ではありません。

## 公開・自動配備

[公開検証記録](docs/verification/release-0.2.0.md)と[機械記録](docs/verification/release-0.2.0.json)に、GitHub Actionsの3 OS/WASM、公開済みnpmの新規導入、本番3ブラウザ各13例、ガイドとブログの応答を集約しています。SDKタグv0.2.0とtarballは固定し、公開後に差し替えていません。

Playgroundは専用Workerで公開し、releaseブランチへのpushからCloudflare Workers Buildsでソースビルド・検証・自動配備します。main/developのpushでは配備せず、npm publishは別の明示工程です。配備後のCSP調整はappのHTML nonceと専用パスのゾーン設定で行い、SDKのLua実装は変更していません。

Storm Minのpublic SDK参照は専用ブランチへpush済みですが、同privateリポのremote CIは課金制限で開始前に停止しています。Engineの公開CIは成功済みで、両者を混同しません。

**最適化後Source Mapはv0.2.5またはv0.3.0へ分離**します。由来情報を考慮せず実装された最適化器の大規模変更であり、v0.2.0の完成条件に含めません。Addonコンパイラ、require方式の統合・gameとloaderの分離、追加map API、全consumerの移行完了も今回の必須条件ではありません。

初回公開の事実と当時の測定は[0.1.0検証記録](docs/verification/release-0.1.0.md)を参照してください。


## Storm Code統合API（未公開）

共有parserによるsource inspection、独立したLBビルド、開発用overflow viewportを実装・検証。[記録](docs/verification/storm-code-integration-20260930.md)。通常build、通常game描画、最適化探索、minify後mapは変更しない。

## 最適化後位置追跡の基盤（未公開・別作業ブランチ）

`feat/optimized-source-maps`で[計画](docs/design/source-provenance.md)のP0/P1を実装。未達target探索は共通コアと一意候補を再利用する。非短縮の通常/LifeBoatビルドはUTF-8コピー範囲とトークン/UTF-16列のmapを保持し、合成prefix・削除済み開発区間に元位置を付けない。診断の列と終端も同じ範囲から変換する。

最適化ASTの由来引き継ぎ、最終minify出力のmap、元変数復元、Playgroundの双方向表示は未実装。`minify`と`build(minify:true)`がmapを返すとは扱わない。公開・版上げ・他ブランチの置換は行っていない。実行済みテスト・性能比較・未実装範囲は[検証記録](docs/verification/source-provenance-20260930.md)に記録する。
