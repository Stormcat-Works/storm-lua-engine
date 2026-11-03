# available-expression-reuse の展開位置・文位置ガード（2026-10-30、未公開）

## バグ

available-expression-reuse パスは、直線ブロック内で既に計算済みの式を
キャリア（単一ローカル）へ置換していたが、置換先の位置が多値を展開する
位置かどうかを区別していなかった。キャリアは呼出の第1返値しか保持しない
ため、次の形で意味が変わっていた。

- コンストラクタ末尾：`local c = pair()` の後に `local t = {pair()}`
  と書くと、最適化後は `t = {c}` となり `#t` が 2 から 1 になった。
- 呼出最終引数：`take(pair())` が `take(c)` となり、第2引数が nil になった。
- 文位置の呼出：`pair()` 文がキャリア名だけの文に置換され、最適化
  コンパイル全体が失敗した（フォールバックなしの失敗方向）。

いずれも、単独パス無効化スイープ（正本 73 件）で
available-expression-reuse の無効化でのみ直ることを確認した。
直接のコンストラクタ（先行する単値利用なし）では発火しないため、
トリガーは「同一呼出の先行する単値利用」である。

## 修正

`crates/storm-lua-minify/src/passes/available_expressions.rs` のみ。
`rewrite_expression` に `no_call_reuse` フラグを追加し、次の位置では
Call ノードのキャリア置換を行わない（元の呼出を残す）：

- 展開位置：テーブル最終 Arr フィールド、呼出最終引数、Return 最終式、
  名前数＞式数の Local／Assign 最終式、式数＜3 の汎用 for 最終式。
- 文位置：Callstat の呼出。

名前数＝式数の Local／Assign では末尾呼出は 1 値に切り詰められるため
キャリアと一致し、再利用を維持する（既存テストの後退を避けるための
条件。汎用 for は反復子・状態・制御の 3 値を消費するため式数＜3 を
展開条件とした）。括弧は切り詰めと同じく 1 値化するため、括弧化ではなく
置換の抑止とした。

## 回帰テスト

同ファイルの tests に 3 件追加（コンストラクタ末尾・呼出最終引数・文位置）。
いずれも修正前は失敗し、修正後は成功することを両方向で確認した。
`cargo test -p storm-lua-minify --lib passes::available_expressions` は
全件成功。ワークスペース全件・xtask check・fmt・clippy（minify lib、
警告 0）も通過。

## Tier FULL コーパス比較

分岐元＝統合先端 3354f3b00（第151ラウンド）、開発ホスト1、
manifest 照合 4,155/4,155 OK（manifest ハッシュ abfec87a…、VM 正本と一致）、
内部並列 -P 4。base／head とも 16,620 行・欠落 0 件。
4 条件すべて identical=4,148、hard=7、map-core=0、expl-only=0。
ステータス対は全条件で ok 4,128／error 27 が両側同一で、ステータス変化は
0 件。生成コード変化は 7 ファイル（全 4 条件で同一の 7 件）で、文字数差は
+3〜+23 文字（呼出を残すための増加で、バグ修正として許容される範囲）。
変化 7 件の実行照合（w6diff、3 tick）：修正前は plain と不一致だった 5 件が
修正後は plain と完全一致に回復し、残り 2 件は修正前後でトレース不変
（pure host read の再利用による読取順序・回数の差で、値の差ではない
既存パターン）。したがって本修正による回帰はない。

## 別件（本サイクルの副産物・未修正）

境界合成スイート v1（61 入力）で、字句勝者（`_ENV` を含む入力）の
Source Map がエンジン自身の `optimized_source_map::validate` に失敗する
ことを確認した（`exact-copy attribution does not match source and generated
bytes`。リネーム済みローカルのトークンが exact-copy として記録されるため）。
実行トレースは全構成一致で、意味の破れではない。schema-2 領域
（canonical_map／optimized_source_map は w2 の主担当）のため、本サイクルでは
修正せず、親への申し送りとする。

マージ時の rebase 後（分岐元 e44102759＝第152ラウンド）についても累積 FULL 比較を実施し、同一の結果（identical 4,148・hard 7・同一の 7 ファイル・ステータス変化 0・中核差分 0・欠落 0）を得た。
