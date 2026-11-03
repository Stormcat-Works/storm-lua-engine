# 末尾引数省略の試行ボディの由来なし化による追跡コスト削減（2026-10-28）

## 焦点
compile の追跡コストのうち、等価な末尾引数省略パス
（`equivalent-trailing-argument-omission`）が、出力に反映されない
試行のたびに支払っていた由来確保コスト。w1 の第27サイクルで、
開発ホスト1 上で作業した。分岐元は統合先端 `7794245be`
（第104ラウンド）。

## 棚卸し
新先端での再棚卸し（合成入力、trackcost single 5 反復・
directpass 相当の単独オン測定、開発ホスト1）では、dense-normal
は第26サイクル終了時と同一で、小粒パスは最大 12 回だった。
他の合成入力へ広げた結果、inline.lua で同パスの単独オン差が
78,041 回、common.lua で signed-expression-factoring が 12,685 回と
大きく、両者とも単独オン出力は all-off とバイト同一
（追跡あり・なしとも）だった。本サイクルは規模の大きい
末尾引数省略側を実装対象とし、signed-expression-factoring 側は
切り分け済みの次候補として記録に残す。

## 切り分け（末尾引数省略、inline.lua）
- パス全体の no-op 化で単独オン差は 78,041 回→0 回となり、
  コストはパス内部と確定した。
- `can_omit` を常に偽にする変種（正規化ボディ構築を省き、
  resolve・関数表・走査・ループ素地だけ残す）では差は 1 回まで
  崩れた。すなわちほぼ全額が `can_omit` 内の正規化ボディ評価
  によるものである。
- 機構：`normalized_body` は `inherit_ast` で試行アリーナを作って
  いた。`inherit_ast` は tracked 側では空の由来スロット表付きの
  アリーナを返すため、試行ボディの複写 push と 3 回の
  constant_fold が capture／finish の由来確保を払い、比較後に
  試行ごと破棄されていた。呼出点×keep 候補×正規化 2 体分の
  積み上げが 78,041 回である。無追跡側は由来表がないため
  この差は追跡側だけに現れる。

## 採用した変更（ローカルコミットのみ。push・PR なし）
- `crates/storm-lua-minify/src/passes/omit_arguments.rs` のみ。
  `normalized_body` の試行アリーナを、由来表なしの空アリーナ
  （`nodes: Default::default()`、文字列表は源から共有）に変更した。
  試行ボディは印字文字列での比較にだけ使われ、由来は一切読まれない
  ため、印字結果は不変である。実 AST への書込み（実際に省略が
  決まった場合の `rewrite`）は従来どおり tracked 側で行われる。
- 回帰テストの新規追加はなし。変更は配分のみで出力・由来は
  不変であり、挙動は既存の
  `compact_normalization_matches_full_arena_with_shadowing_and_captures`
 （正規化ボディ文字列と全アリーナ正規化文字列の一致を主張）と
  provenance マトリクスの同パス項目で固定されている
  （いずれも本サイクルの対象テストで通過）。

## 測定（合成入力 inline.lua、開発ホスト1、中央値。時間差は主張しない）
- trackcost single（5 反復）：同パスの単独オン差 78,041 回→1 回。
  無追跡側の確保回数・コード長は変更前後で同一
  （default 無追跡 809,855 回・コード長 0・候補数 4 も同一）。
- onlycode による単独オン出力のバイト同一を追跡あり・なしとも確認
  （all-off 出力と同一）。

## 検証
- 作業側（開発ホスト1、Rust 1.97.1、sccache 回避の
  `RUSTC_WRAPPER=env` でビルド）：対象 3 クレート
  （syntax 123・minify 643・build 126）全成功、ワークスペース全件
  50 スイート・1,146 件全成功。`cargo xtask check`・
  `cargo fmt --check`・clippy（対象 3 クレート lib、警告 0）通過。
- 実コーパス全件比較（Tier FULL、開発ホスト1 のローカルコーパス
  コーパスのローカルミラー、実行前に SHA-256 manifest 照合 4,155/4,155 OK、
  manifest ハッシュ
  `abfec87ab0c29dc088d4e93f25cf563849b062d3eb15eeaa5d54f8d08b8341c3`）：
  base は FULL-VERIFY-TIP `8cd3067a4`、head は本変更を含む作業ツリー。
  両 TSV はバイト同一（SHA-256
  `f45aced283ffb974d0cef713f3fc43e84310dfa228dfad7aeca69595b66fb4ea`、
  16,620 行）。全 4 条件で identical=4,155、hard・map-core・
  expl-only・missing すべて 0、成否ペア ok/ok 4,127・error/error 28。
  すなわち第104ラウンド（Tier INVARIANT）と本変更は、コーパス範囲の
  出力に一切の変化を与えていない。
- 検証区分は Tier FULL（累積比較）のため、FULL-VERIFY-TIP は
  マージ後の新先端へ更新する。

## 次サイクルの焦点（第1候補）
- signed-expression-factoring の試行由来コスト
  （common.lua で単独オン差 12,685 回、単独オン出力は all-off と
  バイト同一）。切り分け済み：検査側は 2 回、試行ループ側が
  ほぼ全額で、内訳はキャリア生成の derive／relate 約 10,116 回と
  rewrite finish 約 2,486 回。試行 9 件はいずれも敗北する。
  候補は、試行を由来なしクローンで実行し、勝者のみ実 AST へ
  適用する分離（勝者不在の場合は適用なし）で、試行由来の
  付け直し設計に注意すること。
- 第2候補：draw.lua の ordered-screen-loop-synthesis の追跡差
  （単独オン差 145,736 回、単独オン出力は all-off と同一長）。
  ただし同パスは w7 の候補探索軸と接するため、着手前に
  CLAIMS.md で w7 との境界を確認すること。
- 第3候補：immutable.lua の immutable-carrier-synthesis の単独オン差
  （約 307 万回・約 2.3 秒。ただし単独オン出力は変化するため
  発火系であり、試行の無駄とは別種の切り分けが必要）。
