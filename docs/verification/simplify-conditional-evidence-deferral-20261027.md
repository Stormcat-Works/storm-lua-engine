# simplify の条件証拠構築の遅延化による追跡コスト削減（2026-10-27、未公開）

## 焦点

constant-folding 内の simplify（`simplify_control_flow`）のパス単独追跡差
（合成入力 dense-normal、directpass 11 反復中央値で 961 回）の内訳。

## 切り分け（合成入力 dense-normal、directpass 11 反復中央値）

simplify の追跡差 961 回に対する一時変種（診断後に手動で元に戻し、
worktree をクリーンに戻した。注：サービス再起動で /tmp の退避ファイルが
消えたため、復元は cp ではなく逆向きの編集で行った）:

- 証拠構築を丸ごと外すと追跡差は 961→1 回になる。すなわち simplify の
  追跡差はほぼ全額、変更されない `If` ノードへの
  `conditional_evidence` 構築の空振りである。`simplify_node` は証拠を
  構築してから変更判定を行い、未変更ノードでは構築した理由を破棄して
  いたため、arm 真偽文字列・join 文字列・facts ボックスなどの構築コスト
  が未変更ノードの分だけ積み上がっていた。

## 採用した変更（記録内容は同一）

- `crates/storm-lua-minify/src/passes/literal_folding.rs`:
  子走査後・arm 剪定前の時点で arm 真偽を 2 ビット／arm（unknown／
  true／false）で 64 ビットに pack したスナップショットだけを取る
  （32 arm まで。超過時は従来の eager 経路へ fallback）。
  ノードが変更された場合にだけスナップショットから従来と同一の理由を
  再構築する。スナップショットは剪定前の状態で取るため、剪定で置き換え
  られたノードにも従来と同じ剪定前証拠が付く。無追跡入力では
  スナップショット・eager とも None を返す。
- 実装上の注意：初版では変更判定の前に理由を構築していたため、未変更
  ノードでも構築コストが残り、追跡差は 801 回止まりだった。変更判定を
  先に行い、変更されたノードだけ理由を構築するよう修正したところ、
  追跡差は 1 回になった。遅延化では「構築の遅延」だけでなく
  「判定の順序」も確認すること。
- 回帰テスト `deferred_conditional_evidence_matches_eager` を追加した
  （5 形状以上の `If` ノードで eager 理由とスナップショット再構築理由の
  完全一致を検証）。

## 測定（合成入力 dense-normal、VM）

- directpass 11 反復中央値：simplify 追跡差 961→1 回（無追跡 12,663 回は
  不変）。constant-folding 全体は 7,538→6,578 回（−960 回）。
  api-alias 5,810 回・scope-renaming 2,784 回・root-local 1,292 回は
  不変。無追跡側・コード長・ノード数・saved はすべて同一。
- trackcost single 5 反復中央値：全コンパイル追跡差 81,246→74,526 回
  （−6,720 回）、constant-folding 単独オン差 13,793→9,953 回
  （−3,840 回）、all-off 差 18,327 回は不変。単独走査の −960 回の
  整数倍になっているのは、パイプライン内で `simplify_control_flow` が
  複数箇所（`constant_fold` 内・orchestration・search など）から
  呼ばれるためで、走査回数の厳密な内訳までは主張しない。
- 時間差は主張しない。

## 検証

- 対象 3 クレート（syntax 113・minify 574＝新規 1 件含む・build 126）
  全成功。ワークスペース全件 50 スイート・1,067 件全成功、失敗 0。
  xtask check・fmt・clippy（lib 対象、警告 0）通過。--all-targets の
  clippy 警告 4 件は search.rs テストコードの既存警告（本変更の
  ファイル外）のため対応しない。
- 実コーパス全件比較（base `3f2d05875` と head、開発ホスト2 の
  一時領域の作業環境で、コーパスは読み取り専用でコピーなし、Rust
  1.97.1 で両バイナリを VM ビルドして .ud へ送付。検証バイナリの
  SHA-256 相違も確認：base `e8dc489b…`、head `6fc16a05…`）：
  4,155 ファイル×4条件、欠落 0。両 TSV はバイト同一
  （SHA-256 `11741082…`、16,620 行）。全条件 identical=4,155、
  hard・core・expl・missing すべて 0。成功 4,127／失敗 28 は両側同一。
  すなわち本変更はコーパス範囲の出力に一切影響しない。
  TSV は `hidden_files/workers/w1/corpus-verify-simplify-defer-20261027/`
  に保存。開発ホスト2 の一時ファイルは用後削除済み。ベース用
  worktree（sle-base-wt）も用後削除済み。
- Map 比 1.2 の目標は未達のまま。
