# Documentation index

## 利用するアプリケーションを開発する

利用者向けガイドの本文は[docs.makkii.jp](https://docs.makkii.jp/storm-lua-engine/index)を正本とします。[導入](https://docs.makkii.jp/storm-lua-engine/getting-started)、[コンパイラ](https://docs.makkii.jp/storm-lua-engine/compiler)、[ソース読み込み](https://docs.makkii.jp/storm-lua-engine/source-loading)、[環境](https://docs.makkii.jp/storm-lua-engine/environments)、[API一覧](https://docs.makkii.jp/storm-lua-engine/api-reference)、[Playground](https://docs.makkii.jp/storm-lua-engine/playground)。

本リポジトリには[実行可能な例](../examples/README.md)、[環境契約](specs/environments.md)、[ソース読み込み契約](specs/source-loading.md)、[Playground開発手順](../app/README.md)、検証記録とrelease手順を維持します。docs/guideのファイルは旧リンクからの移設案内です。

## エンジンを開発・検証する

仕様は「このライブラリの契約」と「実装済み範囲」を分離する。未確認のゲーム挙動は確定事実として扱わず、対応するfixtureゲートと[TASKS](../TASKS.md)で管理する。

| Document | Owns |
|---|---|
| [Architecture](design/architecture.md) | crate責務・依存方向・ホストとの境界 |
| [Source provenance verification](verification/source-provenance-20260930.md) | P0/P1の実装、Native/WASM性能、列対応、残工程の検証 |
| [Optimized source provenance](design/source-provenance.md) | 最適化後map、link/lintの詳細位置、未達target探索の共通化計画 |
| [Optimization explanation schema](design/optimization-explanation-schema.md) | versioned producer/理由/由来/採用候補の確定設計 |
| [Optimization map extension](specs/optimization-map-extension.md) | Source Map v3＋x_stormの公開API・座標・指紋検証 |
| [Compiler SDK plan](design/compiler-sdk.md) | 言語処理統合の境界・移行ゲート。現在の実装はCompiler guideとSTATUSで区別 |
| [Playground](design/playground.md) | SDK全機能を試すCLI/Web、app配置、公開Worker、Addon Lab移行の範囲 |
| [Playground decision](adr/0006-playground-coexistence.md) | Storm Minとの併存とSDK確認専用アプリの採用理由 |
| [Compiler integration decision](adr/0005-compiler-sdk-integration.md) | 言語処理移管の判断理由と保留事項 |
| [API surface](specs/api.md) | 現在の公開API、今後の高/低レイヤーAPI |
| [Numeric I/O](specs/numeric-io.md) | f32/f64の境界・チャネル・出力保持 |
| [Addon](specs/addon.md) | 独立profile・lifecycle・savedata・host server契約 |
| [Properties](specs/properties.md) | 型・精度・更新時点・欠損処理 |
| [Debugger](specs/debugger.md) | ホスト検査・実行制御・値転送・デバッグ拡張 |
| [Screen](specs/screen.md) | 描画・RGBA・同梱フォント |
| [WASM ABI](specs/wasm-abi.md) | レイアウト・寿命・エラー・Worker |
| [Runtime and host](specs/runtime-host.md) | サンドボックス・Addon・HTTP・地図 |
| [Publication workflow](design/publication-workflow.md) | npm OIDC公開、検査済みtarball、再実行とCI起動 |
| [Release](release.md) | 版・配布物・公開手順 |
| [Distribution](design/distribution.md) | Cargo/npm、target、ビルド手順 |
| [Integration](design/integration.md) | 利用形態・最適化ツールの移行判断 |
| [Testing and performance](design/testing-performance.md) | fixture・CI・性能測定 |
| [Dependencies](design/dependencies.md) | 外部依存の採否・backendバージョン |
| [Decisions](adr/0004-self-contained-contracts.md) | 採用判断と理由 |
| [Verification](verification/repository.md) | 今回の実行結果と未検証範囲 |

[AGENTS](../AGENTS.md)は作業規則、[CONTRIBUTING](../CONTRIBUTING.md)は開発手順、[STATUS](../STATUS.md)は現在地の単一正本。

## v0.2.0候補の確認

[非短縮マップ契約](specs/source-maps.md) · [候補の検証記録](verification/release-candidate-0.2.0.md)。利用者向け本文はdocs.makkii.jpのガイドを参照します。

現在の公開版: [v0.2.0の公開・配備確認](verification/release-0.2.0.md) / [機械記録](verification/release-0.2.0.json)。
