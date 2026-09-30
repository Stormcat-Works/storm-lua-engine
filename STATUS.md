# Current status

## v0.3.0 開発中

`feat/minify-source-provenance-v0.3`で、Parser/Printerの範囲記録にoptionalな由来テーブルを接続した。通常/目標探索、字句短縮、原文早期返却、JSON/binary Worker継続に最終候補と一致する内部GeneratedOriginsを持たせる。変更/新規nodeの由来不明は明示し、コピー/名前短縮/定数化など19パスへ伝播処理を追加した。[今回の検証](docs/verification/source-origin-propagation-030-20260930.md)。

標準の最適化後minify mapはまだSDKへ提供しない。全67パス中19 partial/48 pendingで、インライン化・共通化・描画data/loop変換を含むP3が主な残件。その後P4の標準map/link合成/Rust-WASM-TS公開、P5のPlayground双方向表示とruntime位置接続を行う。[実装計画](docs/design/source-provenance.md)。

## v0.2.1 公開状況

2026-09-30: npm/GitHub/Playgroundへの公開とregistryからの独立導入検証は完了。npm latestは0.2.1。Trusted Publishingの初回承認は済んでおり、認証待ちは残っていない。[公開記録](docs/verification/release-0.2.1.md)。公開済みtag、tarball、release branchへv0.3.0開発コードを混ぜない。

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
