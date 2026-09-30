# Implementation tasks

現在地は[STATUS](STATUS.md)。**v0.2.1は公開済み。後続機能は別ブランチで検証し、公開済み成果物と区別します。** 利用ガイド本文はdocs.makkii.jp、実装契約・設計・検証は本リポで管理します。

## v0.2.0のローカル完成確認（1〜5完了）

| 項目 | 完了条件 |
| --- | --- |
| 範囲固定 | 最適化後Source Mapは0.2.5/0.3.0へ分離。gameでのmulti-fileは非短縮build経由、開発includeはextended＋requireLoaderを維持 |
| 非短縮マップの利用例 | 元ファイル→生成行のbreakpoint、停止/step/caller/error→元位置を実SDKで往復。合成行の誤帰属・存在しない行へ移動しない |
| 契約レビュー | 環境、外部名、字句短縮、require、load/reset、停止/失敗状態、公開型と依存境界を確認し、実際の問題だけ修正 |
| 公開対象の点検 | 公開済み0.1.0以降のreachable履歴・現行tracked tree・梱包内容・権利表示を確認。非公開コーパスを製品へ含めない |
| 候補の検証記録 | 対象commit/ソースdigest、コマンド、結果、生成物hash、既知の制限を[候補記録](docs/verification/release-candidate-0.2.0.md)へ集約 |

## v0.2.0の公開工程（完了）

| 順番 | 工程 |
| --- | --- |
| 1 | 管理者の指示で候補をpushし、公開対象commitのLinux/Windows/macOS・WASM・ブラウザCIを確認 |
| 2 | 同一commitのSDK tarball、Playground静的ZIP、検証記録、SHA256SUMSを固定し、梱包済みconsumerを再実行 |
| 3 | CHANGELOGの日付確定、公開ブランチ、v0.2.0タグ、GitHub Release、npm公開とregistry再導入の確認 |
| 4 | 対応版Playgroundとdocs-siteを配備し、本番subpath・WASM・Worker・CSP・キャッシュ・保存を確認 |
| 5 | v0.2.0記事を公開状態に合わせて公開。v0.1.0記事は独立した初回公開記事として扱う |
| 6 | 必要な下流の固定参照と完了記録を更新。各アプリ全体の移行完了をEngine公開へ抱き合わせない |

手順と実行条件は[release](docs/release.md)。Playground CLIはリポジトリから使う確認用CLIとして提供し、独立npx packageや各OSの単体binaryを今回の条件に加えません。

[公開記録](docs/verification/release-0.2.0.md)に対象commit・CI・registry導入・本番確認を記録しました。Storm Min自身のnpm公開は別工程で、private CIの課金制限は未実行理由として明示しています。

## ログ発生位置（後続版）

- Rust/構造化WASM/TS: 実装・検証済み。[検証記録](docs/verification/log-locations-20260929.md)。
- 本文と位置の上限、取得不能な位置、旧レコード受理と不正位置拒否を検証する。
- Named include、alias、tick/draw、reset、Addon、停止/失敗、非短縮map、Nativeのdebug feature無効を実際に実行する。
- consumerのTerminalリンク・実行世代保存はconsumer側。新しいSDK版の公開・タグは別の承認工程。

## 後続項目

| 対象 | 扱い・確認条件 |
| --- | --- |
| 最適化後Source Map | **v0.3.0**。最適化ASTの由来追跡、位置の合成、変換・削除・複製への対応を別に設計・検証。非短縮マップを流用しない |
| 元変数・元の実行順での高度なデバッグ | 位置マップとは別機能。消えた値の復元を自動保証しない |
| Addonコンパイラ | 現在はVehicle専用。Addon指定は明示拒否を維持 |
| requireとextendedの分離、値返却型の動的loader | 現行経路を維持。具体的な追加host要件が出た段階で再評価 |
| 各consumerの採用 | Storm Min/Editor等の実装・検証はconsumer側が所有。Phys Sim/Storm Code全体の完成は別工程 |
| Propertyの保存精度・型不一致等の実ゲーム観察 | SDK間の一致とは区別する独立oracle。確認したゲーム版・モードの範囲を明示 |
| Addonの追加server/matrix規則 | 実hostとゲーム観察に基づき追加。未実装を成功stubで埋めない |
| map座標変換、描画専用TS rasterのmap provider | 実利用要求がある段階で入力・精度を検証して追加 |
| VSCode WebView、追加CPU/OS | 実WebView/CSP、i686/armv7等を実際に実行した証拠と分離 |
| SIMD/shared-memory/並列pool等 | 必要性と同条件の改善測定がある場合だけ検討 |
| 全収集コーパス回帰・包括的性能評価 | 今回実施した代表回帰を全件と表現しない。測定なしの速度向上を主張しない |

Storm MinのCLI/WebをPlaygroundへ移設・廃止しません。PlaygroundへIDE、共同編集、クラウド同期、ゲーム世界・仮物理を追加しません。過去版の確認件数と初回移管時の結果は各verificationに保持します。


## Storm Code向け実行ホスト操作（未公開）

名前付きtick/draw、即時property/input状態操作、typed property snapshot、runtime専用analyzeを実装。静的ビルド契約は維持する。独立consumerとNative/WASMで検証し、Storm Codeでの利用は同製品の段階移行へ記録する。未公開の識別可能なSDKスナップショットと既存npm版を混同しない。

## ソース位置追跡とtarget探索（P0/P1実装済み）

[実装計画](docs/design/source-provenance.md)のP0（探索共通化）とP1（リンク段の詳細範囲）は実装済み。P2の内部由来・候補/Worker接続、P3の全67パスの由来対応は実装済み。残りはP4の最終map/公開API、P5のconsumer接続と追跡コストの改善。4秒は実用規模での観測値であり、時間による探索打ち切りや全入力の保証上限ではない。

## v0.3.0 の残件

v0.2.1の公開は完了。P0〜P3を完了し、[全67パスの台帳](docs/design/source-provenance-pass-audit.json)はimplementedとなった。全パスの実変換・負例、実コンパイラ536設定、代表30入力・240設定、Native/WASMとクロスプラットフォームCIを検証した。[今回の検証記録](docs/verification/source-origin-all-passes-20261001.md)。

P4は標準Source Map v3と詳細由来の公開型、link合成、Rust/WASM/TS境界、code/map/source snapshotの識別。P5はPlaygroundの双方向範囲選択・関連由来・Unknown/Synthetic表示と実runtime位置への接続。計画と完成条件は[由来追跡計画](docs/design/source-provenance.md)が正本。

追跡追加コストの削減、追加コーパス・新しい組み合わせの回帰は継続する。現行パスに未実装の由来伝播が残っていることとは区別する。Unknownを推測で埋めず、生成コードの理由・元データ・複数の関連元を保持する。
