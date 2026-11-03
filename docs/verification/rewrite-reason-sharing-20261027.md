# 検証記録：rewrite 理由の共有化による追跡コスト削減（2026-10-27）

## 変更の概要

root-local-globalization の残り追跡差 2,572 回のうち、文ごとの rewrite
理由の構築コストを共有化した。

- `crates/storm-lua-syntax/src/explanation.rs`：固定形状文字列
  （local／assignment／function-statement など 25 種）をプロセス全体で
  1 度だけ構築する静的テーブルから供給するよう `shape()` を変更した
  （数値・文字列・演算子などの値依存形状は従来通り整形する）。
  `OptimizationReason::rewrite_shared` を追加した（呼出側所有の code
  Arc を clone して使う）。旧 `rewrite` は不要になったため削除した。
- `crates/storm-lua-syntax/src/deferred_origin.rs`：
  `rewrite_in_place_code` を追加した（既存の `transform_code` を使う）。
  旧 `rewrite_in_place` は不要になったため削除し、deferred 由来の
  対応テストは共有経路を使うよう更新した。
- `crates/storm-lua-syntax/src/node_arena.rs`：
  `finish_rewrite_with_code`・`rewrite_with_code` を追加し、既存の
  `finish_rewrite`・`rewrite` はそこへ委譲した。Block rewrite 時の
  除去フォールバック理由も共有 code を使う。
- `crates/storm-lua-minify/src/passes/root_globals.rs`：パス内で code
  Arc を 1 個だけ作り、文 rewrite 2 箇所とルート Block rewrite を
  `rewrite_with_code` に変更した。

記録内容（由来・理由・生成コード）は文字列内容で同一のため変わらない。
回帰テスト `rewrite_with_code_matches_rewrite` を追加した（Local→Assign
の文 rewrite とメンバ除去を伴う Block rewrite で、新旧両経路の由来・
シリアライズ済み AST・印字コードが一致することを検証する）。

## 測定（Muse の VM、2 コア）

合成入力 dense-normal（46,022 バイト）、計数アロケータ付きハーネス、
各条件 11 反復の中央値。

- root-local-globalization 単独：無追跡 2,002 回・471,775 B から
  2,003 回・471,815 B（±1 回の誤差範囲で実質不変）。追跡 4,574 回から
  3,295 回へ（−1,279 回、追跡差 +2,572 回→+1,292 回で約 −50%）。
  文あたり 6 回から 2 回（理由テール節とスロットのみ残る）になった。
- 全コンパイル（同一ヘッドでの stash 前後比較、5 反復中央値）：
  追跡差 122,325 回から 116,717 回へ（−5,608 回、約 −4.6%）。
  無追跡側は 1,838,375 回から 1,838,376 回で実質不変。
  単独オン差も 15,528 回から 14,248 回へ（−1,280 回）減った。
- 時間差は負荷変動が大きく、時間改善は主張しない。

## 検証

- 対象 3 クレート（syntax 108＝新規 1 件含む・minify 475・build 126）
  全成功。ワークスペース全件（50 スイート）成功。
  xtask check・fmt・clippy を通過。lib 対象の clippy 警告は 0
  （--all-targets では minify のテストコード由来の既存警告 4 件のみで、
  本変更のファイル外）。
- 実コーパス全件（4,155 ファイル×4 条件、base `e115013e` 対 head）の
  比較では、成功／失敗（4,127／28）・生成コード・Map 中核・
  正規化済み Map 全体のすべてが差分 0 件で、両 TSV はバイト同一だった。
  すなわち本変更はコーパス範囲の出力に一切影響しない。

## 残り

root-local-globalization 残りの追跡差 1,292 回の主因は、名前コピー
640 回（新規名スロットと名前列バッファで、ノードごとにスロットが
必要なため表現変更なしでは削減できない）と、文 rewrite のテール節＋
スロット 640 回、ルート Block／resolve 12 回である。
