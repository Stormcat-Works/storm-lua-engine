# Implementation tasks

## AD-01/AD-02 host接続（2026-10-17）

host側のHTTP worker・sceneキュー・実ゲームとの応答比較はconsumer側で接続中。Engine追加APIの検証と公開工程は区別する。

## v0.3.1のローカル公開候補（2026-10-12、完了）

最新性能・Addon・metadata・743件の描画契約を統合し、Native/WASM/SDK/3ブラウザ/Playground/梱包consumerと固定240設定・実コーパス・反復性能を再検証した。[候補記録](docs/verification/release-candidate-0.3.1.md)。既知7入力のtrapと、部分/反復名mapの不具合も解消した。source manifestと検査済みtarball/ZIP、hash、release notes、ガイドpatchを保存した。

残る公開工程は、最終commitの3 OS CI、producer/revisionを確定した配布物の再検査、タグ・GitHub Release・npm・本番配備・registry再導入。現在の候補を公開済みにはしない。

## v0.3.2以降へ送る改善

- 全入力でMap/plain比1.2以内にする目標（今回の最大反復中央値比7.019）。通常側を遅くせず、情報・精度を削らず追加研究する。
- 大きなmapの容量・ピークメモリ、追加candidate/snapshot/poolの改善。
- Addon最適化、完全なserver API analyzer、元変数/storage/lifetime/bytecode PCの動的復元はそれぞれ独立の後続機能。

以下の開発ラウンドは過去の実測・未達記録として保持し、その研究目標をv0.3.1の追加公開ゲートにしない。

## v0.3.1 全経路の性能要件（開発中）

2026-10-05の[追加実装と比較](docs/verification/compile-continuation-20261005.md)を完了。API参照の文字列確保、走査snapshot、短縮名、位置変換の重複を削減した。両モードの絶対時間・差分・倍率を別々に記録し、全条件の1.2倍目標と負の観測の確認は継続する。

[全コンパイル経路の計画](docs/design/compile-all-scenes-v031.md)と[実装・検証](docs/verification/compile-all-scenes-20261004.md)を正本にする。API別名選択の先行改善に加え、block解析共有・借用token・parse/位置再利用・字句短縮・map一回出力は実装済み。

未完了条件は、同じ新実装のMapなしを基準とした**最大1.2倍以内、可能なら1.1倍**。全体平均を最大条件の代わりにしない。通常処理を遅くして分母を大きくする対応は禁止する。

次に確認するのは、小さい原文返却と保守的字句短縮に残るmap固定費、大きな描画の追加由来費用、通常経路で見られた小さな負の性能差の再現性。生成Lua・全map・候補/診断・既知の失敗境界を比較し、測定が悪化した条件を除外しない。7原文の全探索WASM trapは独立の未修正課題として保持する。

作業は`feat/minify-performance-v0.3.1`へ検証単位でcommit。push・版番号変更・公開は別の明示工程。元変数値/フレーム等の別デバッグ機能や、schema非互換変更を性能改善へ混ぜない。

## v0.3.1 Mapあり/なしの差分削減（進行中）

目標は同じ新実装・入力・設定でMapありをMapなしの最大1.2倍以内にすること。全体平均の達成を、個別の最大の達成と取り違えない。[実測と検証](docs/verification/map-delta-reduction-20261004.md)。

| 工程 | 現在の状態 |
| --- | --- |
| 既存の高速化基盤と実コーパス調査の保存 | 検証して別commitへ保存済み |
| API別名候補のMap固有処理 | 不採用候補の由来構築を省き、採用した集合だけを追跡付きで再適用。実装・回帰・実コーパス比較済み |
| 1.2倍の目標 | 全件達成は未確認ではなく、反復部分集合に未達例が存在する。開発中のまま維持 |
| 小入力・原文返却・字句短縮のMap固定費 | 次の対象。今回のAPI別名変更を通らない経路であることを区別する |
| 最後に必要な由来付き改名・印字 | 次の調査対象。位置・理由・関連元を削らず、候補に同じ計算を繰り返していないか調べる |
| 全般的なblock解析の再利用 | 通常経路の高速化として別軸で管理。Map追加時間の削減効果と混ぜない |
| 既知の7原文の全探索WASM trap | 新旧に同じ問題があることを確認。今回の改善に紛れて成功扱い・黙ったfallbackをしない |
| v0.3.1公開 | 未実施。性能/互換確認と明示的な公開工程の後に実施 |

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

[公開記録](docs/verification/release-0.2.0.md)に対象commit・CI・registry導入・本番確認を記録しました。consumer自身のnpm公開は別工程で、consumer側remote CIは未実行として区別しています。

## ログ発生位置（後続版）

- Rust/構造化WASM/TS: 実装・検証済み。[検証記録](docs/verification/log-locations-20260929.md)。
- 本文と位置の上限、取得不能な位置、旧レコード受理と不正位置拒否を検証する。
- Named include、alias、tick/draw、reset、Addon、停止/失敗、非短縮map、Nativeのdebug feature無効を実際に実行する。
- 利用側IDEのログ表示リンク・実行世代保存はconsumer側。新しいSDK版の公開・タグは別の承認工程。

## 後続項目

| 対象 | 扱い・確認条件 |
| --- | --- |
| 最適化後Source Map | **v0.3.0**。最適化ASTの由来追跡、位置の合成、変換・削除・複製への対応を別に設計・検証。非短縮マップを流用しない |
| 元変数・元の実行順での高度なデバッグ | 位置マップとは別機能。消えた値の復元を自動保証しない |
| Addonコンパイラ | 現在はVehicle専用。Addon指定は明示拒否を維持 |
| requireとextendedの分離、値返却型の動的loader | 現行経路を維持。具体的な追加host要件が出た段階で再評価 |
| 各consumerの採用 | 実装・検証はconsumer側が所有。利用側製品全体の完成は別工程 |
| Propertyの保存精度・型不一致等の実ゲーム観察 | SDK間の一致とは区別する独立oracle。確認したゲーム版・モードの範囲を明示 |
| Addonの追加server/matrix規則 | 実hostとゲーム観察に基づき追加。未実装を成功stubで埋めない |
| map座標変換、描画専用TS rasterのmap provider | 実利用要求がある段階で入力・精度を検証して追加 |
| VSCode WebView、追加CPU/OS | 実WebView/CSP、i686/armv7等を実際に実行した証拠と分離 |
| SIMD/shared-memory/並列pool等 | 必要性と同条件の改善測定がある場合だけ検討 |
| 全収集コーパス回帰・包括的性能評価 | 今回実施した代表回帰を全件と表現しない。測定なしの速度向上を主張しない |

Storm MinのCLI/WebをPlaygroundへ移設・廃止しません。PlaygroundへIDE、共同編集、クラウド同期、ゲーム世界・仮物理を追加しません。過去版の確認件数と初回移管時の結果は各verificationに保持します。


## consumer向け実行ホスト操作（未公開）

名前付きtick/draw、即時property/input状態操作、typed property snapshot、runtime専用analyzeを実装。静的ビルド契約は維持する。独立consumerとNative/WASMで検証し、利用側製品への統合はconsumer側が所有する。未公開の識別可能なSDKスナップショットと既存npm版を混同しない。

## ソース位置追跡とtarget探索（P0/P1実装済み）

[実装計画](docs/design/source-provenance.md)のP0（探索共通化）とP1（リンク段の詳細範囲）は実装済み。P2の内部由来・候補/Worker接続、P3の全67パスの由来対応は実装済み。P4の最終map/公開APIとP5のconsumer接続も実装・検証済み。v0.3.0向けの限定追跡コスト改善は完了し、追加はv0.3.1以降へ送る。4秒は実用規模での観測値であり、時間による探索打ち切りや全入力の保証上限ではない。

## v0.3.0 の現在地と残件

v0.2.1の公開、P0/P1の探索/リンク、P2の候補/Worker由来、P3の全67パス対応は完了。固定30入力・240設定のUnknown 0%を維持する。P4の標準Source Map v3＋x_storm、元ファイルへの合成、Rust/WASM/TS API、producer/code/source/設定の識別・検証を実装した。[P4と説明情報の検証](docs/verification/optimization-explanations-20261001.md)。

[元の実測例の更新](docs/verification/provenance-examples-20261001.md)に、細かな位置・copy・文脈・理由が実際に返る例を残した。理由のbasis/factsは、実装が観測した条件・数値だけを格納する。全パスの網羅的な正当化証明や、不採用候補の全探索履歴は約束しない。

P5のPlayground双方向選択・理由/関連元/Unknown/Synthetic表示と実runtimeの生成行への接続は完了。 原文をそのまま返すidentityを含め、選択範囲と生成runtime行を原文全体へ拡張しない。Shift・方向キー・Home・End、Unicode/CRLF、再読み込みまで[最終仕上げ](docs/verification/p5-final-selection-20261001.md)で確認した。CLI/Web共通のartifact/workspace検証、再読み込み・別ブラウザーへの持ち運び、実際のbreakpoint/step/log/error、3ブラウザーと大容量mapを検証した。[P5と梱包の記録](docs/verification/p5-playground-20261001.md)。

コードとマップの組み違い、削除行の誤った逆引き、中断後の古い非同期結果の混入を防ぎ、入力や保存物を黙って置換しない。Native/WASMの代表240設定・全67パス回帰と配布tarballの独立導入を確認した。

限定性能ラウンドは[実装・検証済み](docs/verification/source-map-performance-20261002.md)。さらなる性能最適化・map容量削減はv0.3.1以降。各パスの詳細な適用根拠追加、元変数の値/storage/lifetime、bytecode PC、特定loop反復、実行履歴は独立の後続項目であり、P5完了条件に混ぜない。

v0.3.0の公開工程は2026-10-02に完了した。[公開記録](docs/verification/release-0.3.0.md)。既公開0.2.1のタグと配布物は固定したまま、main/release/npmは0.3.0へ更新した。


## v0.3.1以降のSource Map性能課題

v0.3.0公開前の最小限ラウンドは完了。今後は追加profileの結果に基づき、他の候補/snapshotの複製、最終印字/由来平坦化、JSON構築・共有表・validatorのメモリ負荷を改善する。まず前後のピークmemoryを分離して測定する。現在の8.23MBという描画例のmap容量は未縮小である。

優先度はP0〜P5の公開を阻害しない後続作業。生成Lua・候補選択・精度・理由・schema互換性とmapなし性能を維持し、同じ240設定と新旧完全map比較を再実行する。binary形式・schema非互換刷新・情報省略を先に決めない。


## v0.2.1 公開工程（2026-09-30完了）

npmの初回Trusted Publisher承認、検査済みtarballのregistry公開、registryからの再取得・独立導入と公開状態文書の更新が完了した。GitHubとPlaygroundも公開・検証済み。詳細は[公開記録](docs/verification/release-0.2.1.md)。


## 2026-10-05 印字ラウンドの検証引継ぎ

`f3e5c87`までの実装の検証・計測・失敗ケース再確認を完了し、docs/verification/compile-emission-20261005.mdに保存した。最大Map比1.2（理想1.1）と、Map無効時も速くする要件は継続する。最適化の全入力・毎回の改善が完了したとは扱わない。
- [ ] Validate the explicit shared CRT random provider against independent native sampler traces and the actual host's complete consumer/checkpoint lifecycle.
