# api-alias 由来転送の共有コード化による追跡コスト削減（2026-10-27、未公開）

## 焦点

api-alias 最適化のパス単独追跡差（合成入力 dense-normal、directpass
11 反復中央値で 6,450 回）の内訳。転送ブロックと、その後に転送済み
AST 上で走る scope rename に分かれる。

## 切り分け（合成入力 dense-normal、directpass 11 反復中央値）

api-alias の追跡差 6,450 回に対する一時変種（すべて revert 済み）:

- scope_rename_fast を外すと追跡差は 2,412 回に減少する。すなわち
  scope rename 分が 4,038 回。
- 転送ブロック全体を外すと追跡差は 0 回になる。すなわち scope
  rename のコストは転送済み AST 上で初めて発生する（転送なしの候補
  AST には由来がなく、rename の追跡コストが 0 になる）。
- したがって転送 2,412 回＋転送後 rename 4,038 回＝6,450 回。
  転送の主因は置換ノードの derive_from slow path で、置換ごとに
  fresh な理由 code `Arc<str>` を 1 個確保していた。rename 側は
  既に共有化済みのため、本サイクルでは転送側を実装した。

## 採用した変更（記録内容は同一）

- `crates/storm-lua-minify/src/passes/api_aliases.rs`:
  転送経路の `derive_from` を `derive_from_with_code` に置換した。
  `transfer_alias_origins_stack` に code 引数を追加し、転送全体で
  code `Arc<str>` を 1 個だけ作って使い回す。定義付き分岐でも同じ
  Arc を転送と新ルート導出の両方に使う。`derive_from_with_code`
  は fast path・slow path の構造が `derive_from` と同一で、slow
  path のみ呼出側 code Arc を使うため、記録される由来・理由は
  文字列内容で同一である。
- 等価性の主証拠は既存の `trial_provenance_tests`（新旧実装の
  AST・由来・理由スナップショット完全一致）で、本変更でも全件通過
  する。`derive_from_with_code` 自体の等価性は syntax 側の
  `derive_from_with_code_matches_derive_from` が保証する。

## 測定結果

- directpass api-alias-optimization 単独（dense-normal、11 反復中央値）:
  無追跡 115,531 回・30,389,192 B は変更前後で完全同一。追跡
  121,981 回から 121,341 回へ（追跡差 6,450 回から 5,810 回へ、
  −640 回、約 −10%）、バイト差も −25,600 B。saved=7127・
  コード長 13835・ノード数 6883→5940 は同一。時間差は主張しない。
- 640 回は置換ノード＋新ルート導出の fresh code 確保分と一致する。
  転送後 rename 分（4,038 回）は本変更の対象外で、残りの主因である。

## 検証

- 対象 3 クレート（syntax 113・minify 558・build 126）と
  ワークスペース全件（合計 1,051 件）が成功し、失敗は 0 件。
  xtask check・fmt・clippy（lib 対象、警告 0）を通過。
- 実コーパス比較（分岐元 c29034d00 と head、開発ホスト2 の
  一時領域の作業環境で、コーパスは読み取り専用でコピーなし、
  Rust 1.97.1 で両バイナリを VM ビルドして .ud へ送付し SHA-256
  を照合。検証バイナリの SHA-256: base fcc5204a…、head
  323343d4…）: 4,155 ファイル×4 条件、欠落 0。両 TSV は
  バイト同一（SHA-256 11741082…、16,620 行）。全条件
  identical=4,155、hard・core・expl・missing すべて 0。
  成功 4,127／失敗 28 は両側同一。すなわち本変更はコーパス範囲の
  出力に一切影響しない。TSV は workers/w1/corpus-verify-alias-code-20261027/
  に保存。開発ホスト2 の一時ファイルは用後削除済み。
  ベース用 worktree（sle-base-wt）も用後削除済み。
