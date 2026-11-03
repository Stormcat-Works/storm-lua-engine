# 検証記録：形状文字列と fold 由来コードの共有化による追跡コスト削減（2026-10-27）

## 変更の概要

constant-folding の追跡コストのうち、形状文字列と由来コード文字列の
確保を共有化した。記録内容は文字列内容で同一のため変わらない。

- `crates/storm-lua-syntax/src/explanation.rs`：`shape()` で従来は
  整形していた演算子形状（binary／unary の全種）と真偽形状を、静的
  テーブルから供給する共有化に変えた。Lua の演算子集合は閉じている
  ため、未知の綴りは従来通り整形へフォールバックし、記録文字列は
  正確に維持される。回帰テスト
  `operator_and_boolean_shapes_are_shared_and_exact` を追加した
  （共有ポインタの同一性と記録文字列の正確性、未知綴りの整形）。
- `crates/storm-lua-syntax/src/node_arena.rs`：
  `derive_from_with_code` を追加した。fast path は `derive_from` と
  同一で、slow path のみ呼出側所有の code Arc を使う。
  回帰テスト `derive_from_with_code_matches_derive_from` を追加した。
- `crates/storm-lua-minify/src/passes/literal_folding.rs`：
  `fold_node` と `simplify_node` を `derive_from` から
  `derive_from_with_code` に変え、fold 全体で code Arc を 1 個だけ作る
  `fold_code()`／`simplify_code()`（OnceLock 共有）を使う。

## 測定結果（決定的な計数。時間差は主張しない）

合成入力 dense-normal、VM 上、計数アロケータ付きハーネス。
分岐元 43947c84 での stash 前後比較（directpass は 11 反復、
trackcost single は 5 反復の中央値）。

- directpass constant-folding 単独：無追跡 39,834 回・1,815,250 B は
  変更前後で完全同一。追跡 53,032 回→51,592 回（追跡差
  13,198 回→11,758 回で −1,440 回、約 −11%）、バイト
  2,726,154 B→2,692,554 B（−33,600 B）。コード長 41,181・
  ノード数 6,883→7,363 は同一。
- trackcost single 全 sweep の allocs／bytes 比較：変化は
  constant-folding 単独オン（18,172 回→16,732 回、
  345,146 B→311,546 B）のみで、他パス・all-off 差（27,292 回）・
  無追跡側はすべて同一。全コンパイル追跡差は
  116,717 回→115,277 回（−1,440 回）で、folding 単独の削減と
  完全一致する。形状共有化が他パスに波及しないことが計数で
  確認できた。

## 検証

- 対象 3 クレート（syntax・minify・build）のテストが全成功
  （終了コード 0。syntax は 110 passed を末尾で確認）。
  ワークスペース全件 50 スイート・989 件も全成功（復帰時の検証。
  WIP はその後不変）。
- xtask check・cargo fmt --check・clippy（lib 対象、警告 0）を通過。
  --all-targets での clippy 警告 4 件は minify のテストコード由来の
  既存警告（本変更のファイル外）のため対応しない。
- 実コーパス全件（4,155 ファイル×4 条件、base 43947c84 と head）の
  比較では、成功／失敗（4,127／28）・生成コード・Map 中核・
  正規化済み Map 全体のすべてが差分 0 件で、両 TSV はバイト同一
  だった。すなわち本変更はコーパス範囲の出力に一切影響しない。
