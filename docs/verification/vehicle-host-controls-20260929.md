# Vehicle実行ホスト操作の検証（2026-09-29）

状態: 専用ブランチで検証済み、未公開。基点7ee0b69、ブランチfeat/storm-code-integration。
契約は[runtime-host](../specs/runtime-host.md)。公開済みv0.2.0の機能と混同しない。

## 変更

- 名前付きtick/draw callbackを通常のMicrocontrollerフェーズ、命令予算、デバッグ継続へ接続。
- extended限定のcontrolNamespaceで、同じLua呼び出し内の後続property/input読み取りへ状態を即時反映。JSホストのWASM再入は拒否したまま。
- 更新済みpropertyの型・byte label/text・f64スナップショットを公開。
- analyzeのruntime modeはビルド用のrequire制約を適用せず、共有パーサー・環境診断を維持。省略時の静的ビルド契約は変更しない。
- 既存の識別子検証漏れ（local =、予約語を名前として受理）を修正。

## 回帰で検出して修正した問題

全回帰で、load/draw後のinput同期が未使用のホスト入力を古いState入力で上書きする問題を検出した。native controlで変更された入力だけをdrainする方式へ修正し、controlNamespaceの有無の両方でload→draw→tick間の入力保持を追加検証した。

以前のログ位置変更でhostSmokeの返却項目へlogLocationsを追加した際、既存host.testの期待オブジェクトへの反映が漏れていた。今回の全WASM回帰で検出し、期待する実行項目を修正した。

## 実行結果

| ゲート | 結果 |
|---|---|
| Native全workspace・all-features | 485 PASS、0 FAIL |
| TypeScript build・公開型consumer・SDK JS | 30 PASS |
| 実WASM全スイート | 59 PASS |
| Chromium / Firefox / WebKit | 3ブラウザPASS。名前付きcallback/control/propertyの追加シナリオ、各731描画専用ケース・731Lua描画ケース、Worker2台を確認 |
| 梱包SDKの隔離インストール | PASS |
| fmt / architecture / clippy / default check / rustdoc | PASS |

Nativeゲートは共有tmpfs満杯で一度中断し、自作業の生成物だけを清掃して再実行した。CARGO_INCREMENTAL=0、CARGO_PROFILE_DEV_DEBUG=0、CARGO_PROFILE_TEST_DEBUG=0、CARGO_PROFILE_TEST_OPT_LEVEL=1、CARGO_BUILD_JOBS=1。Lua debuggerのfeatureは有効なまま。

## 範囲外

LifeBoatの全API・ビルドsection処理、アプリのsource generation/Terminal接続、最適化後ソースマップ、実ゲーム比較はこの検証の対象外。Rustの標準library/raster等の所有者をアプリへ移していない。タグ、npm公開、Playground配備を行わない。
