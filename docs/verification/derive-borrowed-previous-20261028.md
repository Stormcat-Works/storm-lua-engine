# derive の previous 借用化による追跡コスト削減の検証（2026-10-28）

## 変更内容

compile の追跡コストのうち、constant-folding の derive 経路に残っていた
previous ノード値の clone を除去した。記録内容は変わらない。

- `derive_from` / `derive_from_with_code` の slow path
  （`crates/storm-lua-syntax/src/node_arena.rs`）
  - 従来は `capture_origin` で previous 値を clone したスナップショットを
    作ってから `finish_rewrite` 系へ渡していた。Bin/Un 演算子文字列の
    clone が changed ノードあたり 1 回かかっていた。
  - 改めに、source arena のスロットから origin を取り、previous 値は
    `&source.values[original]` として借りて渡す。rewrite 経路での
    previous の用途はすべて借用（理由の形状・トークン・名前の照合）で、
    呼び出し中に source arena は変更されないため、記録される origin は
    clone 経路と同一である。source に origin が無い・スロットが無い場合は
    従来の `None` スナップショット経路と同様に target スロットをクリアする。
  - `finish_rewrite_with_code` は新設の `finish_rewrite_borrowed` への
    薄い委譲になり、従来の snapshot 経路と借用経路は同一コアを通る。
    `restore_shared` / `import` は snapshot ではなく
    sources・origin の parts を受け取る形に変更した。
  - 公開 API のシグネチャは変わらず、呼出側の変更は不要。
    fold と api-alias 転送の両方がこの経路を使うため両方に効く。

## 回帰テスト

- `node_arena::relation_prefix_tests::derive_from_with_code_matches_snapshot_finish_rewrite`
  （借用経路 `derive_from_with_code` とスナップショット経路
  `capture_origin` + `finish_rewrite_with_code` が、変更ノードに対して
  同一の origin を記録することを検証）

## 測定（合成入力 dense-normal、VM、中央値）

- directpass（11 反復）：
  - constant-folding-expr：追跡差 3,697 回 → 3,217 回（−480 回）。
    未追跡側は 29,096 回で不変、code_len・nodes_after も不変。
  - api-alias-optimization：追跡差 5,810 回 → 5,809 回（−1 回）。
    転送の derive は大半が fast path（値不変）のため、slow path の
    clone 除去はここにはほぼ効かないことが判明した。
  - scope-renaming：2,784 回で不変、root-local-globalization：1,292 回で不変。
- trackcost（5 反復）：
  - 全コンパイル（default）追跡差 81,752 回 → 81,270 回（−482 回）。
  - only-constant-folding：7,305 回 → 6,825 回（−480 回）。
  - only-api-alias：15,477 回 → 15,476 回（−1 回）。all-off 差は 18,111 回で不変。
- サイクル 18 の変種分解との整合：derive 2,880 回の内訳のうち capture
  clone 480 回だけが除去され、残り（dispositions 960 回・reason tail
  480 回・slot box 480 回はいずれも下限）は残る。測定はその予測と一致した。

## コーパス検証

- base `e44e744c` と head を一時環境で比較
  （コーパスは読み取り専用でコピーなし、両バイナリは Rust 1.97.1 で
  ビルドし別名で送付、送付先で SHA-256 を照合）。
- 4,155 ファイル × 4 条件、欠落 0。両 TSV はバイト同一
  （SHA-256 449ba941…、16,620 行）。
- 全条件 identical=4,155、hard・core・expl・missing すべて 0。
  成功 4,127／失敗 28 は両側同一。すなわち本変更はコーパス範囲の
  出力に一切影響しない。
- TSV はワーカーの作業領域 `corpus-verify-derive-borrow-20261028/` に保存。
  一時ファイルとベース用 worktree は用後削除済み。

## 品質チェック

- 対象 3 クレート：syntax 118・minify 601・build 126、すべて 0 failed。
- ワークスペース全件：50 スイート、1,099 passed、0 failed。
- `cargo xtask check` 通過、`cargo fmt --check` 通過、
  対象 3 クレートの clippy（lib）警告 0。
