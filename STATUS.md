# Current status

2026-09-27 — **Storm Lua Engine v0.2.0のローカルリリース候補の完成確認1〜5が完了。未push・未公開・未配備。** 版番号はworkspace、npm SDK、Playgroundとも0.2.0です。範囲と公開手順は[release](docs/release.md)、残件は[TASKS](TASKS.md)、利用ガイドは[docs.makkii.jp](https://docs.makkii.jp/storm-lua-engine/index)です。

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
| 文書 | 利用ガイドはdocs-site、契約・設計・検証・実行例は本リポ。ブログの初回版/次版記事は下書き |

## 候補の最終確認

実装commit `8707a80`に対する41種の最終ゲートがすべて成功しました。Engine Native472件、WASM48件、SDK JS27件、Playground15件＋3ブラウザ各13例、Storm Min Rust492件／独立JS25件と新しいNative/WASM400条件を再検証。[0.2.0 candidate](docs/verification/release-candidate-0.2.0.md)と[機械記録](docs/verification/release-candidate-0.2.0.json)へ集約しています。過去の件数や出力一致を、新しい候補の実行結果として使い回していません。

既存の検証記録: [source loaderと0.2準備](docs/verification/source-loading-20260927.md)、[環境修正](docs/verification/environment-contract-20260926.md)、[Playground](docs/verification/playground-20260926.md)。初回移管の685条件一致はその時点の証跡であり、後続の正確性修正後の出力保証ではありません。

## 公開との境界

GitHub Actionsの公開対象commit確認、正式配布物の固定、タグ・npm・GitHub Release、Playground/ガイド/ブログの本番公開は別の明示工程です。通常pushからnpmを自動publishしません。Workerは専用routeの設定とdry-runまでで、Storm Minのrouteを変更しません。

**最適化後Source Mapはv0.2.5またはv0.3.0へ分離**します。由来情報を考慮せず実装された最適化器の大規模変更であり、v0.2.0の完成条件に含めません。Addonコンパイラ、require方式の統合・gameとloaderの分離、追加map API、全consumerの移行完了も今回の必須条件ではありません。

初回公開の事実と当時の測定は[0.1.0検証記録](docs/verification/release-0.1.0.md)を参照してください。

Optional host-shared Windows CRT random input is being added through VM/Add-on APIs. Lua conversion and sampling stay in the Engine; the host owns context state/order/checkpoint. Isolated defaults remain separately specified. Native conformance and consumer integration are in progress; this is not a release or complete simulation-context acceptance.
