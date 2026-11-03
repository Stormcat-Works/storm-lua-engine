# compile-cpu: tiny-literal-inlining in-place 試行評価 検証記録（直列ループ第301ラウンド）

## 変更の概要

- w7 第110サイクルの本実装（ブランチ `muse/w7-c110`: 実装 `4d5b89f`＋
  検証記録 `efe581f`、分岐元 `b35abfd` から2コミット）を、第300ラウンド
  先端 `3912ac7` へ cherry-pick した（適用後: 実装 `4481b5e`＋検証記録
  `26f234b`）。競合はなく、追加／削除行の集合が原本と一致することを
  確認した。c110 側の検証記録は
  `docs/verification/compile-cpu-tiny-literal-inplace-20261102.md`。
- 内容（`crates/storm-lua-minify/src/passes/locals.rs`、+82/−20）:
  tiny-literal-inlining の試行評価を、試行ごとの arena fresh-clone から、
  リサイクルした試行 arena への in-place 適用＋round source からの
  undo（復元）へ変更した。勝者は live arena へ replay され、コミット
  される arena の nodes・provenance は従来方式と一致するため、勝者
  選択と出力は不変である。
- 採用根拠（w7 第110サイクル時点の決定的計数）: 同パス detach
  18,888,213 → 4,375,821 elems（−76.8%）。c110 の .us2 壁時計は
  2回完走で +1.01%／−0.42% と符号が割れ、悪化は成立していない。

## 検証（Tier FULL、累積 base `7cdf7a5`）

- corpus: StormMin 実世界コーパス 4,155 ファイル（マニフェスト照合
  4,155/4,155 OK）×4 構成＝16,620 行／側。corpuscheck は bench-harness
  の同一ソースから base（`7cdf7a5`）／head（本実装）両側をビルドした
  （エンジンへの path 依存先のみが異なる）。バイナリ SHA-256:
  base `350818d007281b76fabf84dc7999cac2528304059d52d8ac93f3010afe89e2a2`、
  head `8430fa2f86e6424a1cf0f545f8971dd5aea5411af85e1fa94fbbaaf3ea1ae839`。
  .ud の bench corpuscheck は --batch 非対応のため、両側ともレガシー
  ドライバ（`run-corpus.sh`）で実行した。
- 結果: 整列 TSV の SHA-256 が両側とも
  `42ce28cacbfec693acb1c1dc3b338251ebe46e5a1f09dab284c4bd393e9b4e28`
  で一致（16,620 行バイト同一）。`compare-corpus.py`: 全構成
  identical=4,155、hard=0、map-core=0、expl-only=0、missing=0。
  status 不変（構成ごと ok 4,126／error 29）。
- テスト: ワークスペース全体 50 スイート・1,775 passed / 0 failed
  （exit 0、第300ラウンドと同数）。
- fmt: `cargo fmt --all -- --check` の差分は従来から存在する
  他ファイル10箇所のみで、locals.rs は無指摘。
- clippy（storm-lua-minify lib）: 警告8件で base と同一集合。
  locals.rs への指摘はない。

## 統合

- 実装 `4481b5e`＋c110 検証記録＋本記録を統合先端へ fast-forward し、
  第301ラウンドとして確定。FULL-VERIFY-TIP を `4481b5e` へ前進させた。
