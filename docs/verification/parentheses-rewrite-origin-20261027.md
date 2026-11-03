# 冗長括弧除去の capture＋代入＋finish から rewrite() への置換による追跡コスト削減（2026-10-27、未公開）

冗長括弧除去パス（`redundant-parentheses-elimination`）で、ノードごとに
`capture_origin`＋`IndexMut` 代入＋`finish_rewrite` を行っていた箇所を、
`NodeArena::rewrite()` に置き換えた。公開 API のシグネチャは変えておらず、
生成 Lua・候補選択・記録内容は維持する。Map 比 1.2 の目標は未達のままであり、
この変更はその達成を主張しない。

## 原因

追跡差の内訳切り分け（合成入力 dense-normal、計数アロケータ付きハーネス、
11 反復中央値）では、parentheses 単独の追跡差は無追跡 5,766 回→追跡
8,649 回（＋2,883 回）だった。内訳は、`capture_origin` がノード値を
clone する際の Vec バッファ確保で、値が不変のノードでも全ノード分
発生していた。dense-normal では除去 0 件のため、追跡差はほぼ全額が
この capture の値 clone だった。

`finish_rewrite` 自体は値不変時に共有スロット復帰の高速経路を持つため、
無駄は capture 側にだけあった。単一アリーナのこのパスには分離した
源がないため、分離型走査向けの `derive_from` 化は適用できない。

## 変更

- `crates/storm-lua-minify/src/passes/parentheses.rs` のみ変更。
  `rewrite`／`rewrite_inner` を、ノード存続時は新値を返す
  `RewriteOutcome::Kept`、括弧除去時は既存の子を返す
  `RewriteOutcome::Replaced` に分けた。呼出側は存続時に
  `NodeArena::rewrite(id, value, "parentheses")` を呼ぶ。
- `rewrite()` は旧値を `mem::replace` で取り出すため値 clone がなく、
  値不変時は早期復帰で共有 origin を温存し、変更時は従来と同じ
  `finish_rewrite` 経路（同一理由・同一記録）を通る。括弧除去時は
  従来どおり何もせず、子の origin を温存する。
- 変更前後の等価性は次の点で担保される。子走査は当該ノード自身の
  値を変えないため、`rewrite()` が取り出す旧値は capture 時と同一。
  値不変の判定も同一の比較である。origin を持たないノードやスロット
  なしのノードでは、従来・新経路とも no-op になる。

## 測定

合成入力 dense-normal、11 反復中央値。無追跡側は変更前後で完全同一。

- parentheses 単独：無追跡 5,766 回・24,024 B→追跡も 5,766 回・
  24,024 B で完全同一（追跡差 ＋2,883 回→0 回）。
  saved=0・コード長・ノード数は同一。
- 時間差は負荷変動が大きく、時間改善は主張しない。

## 検証

- 回帰テスト：
  `keeps_outer_paren_with_changed_child_and_removed_inner` を追加し、
  子が変わった状態で残る括弧（理由経路）と、残る内側括弧の上の
  除去される外側括弧（両 rewrite outcome）を、origin 監査付き
  プリンタ（unknown 0 バイト）で検証した。
- 対象 3 クレート（syntax 106・minify 444＝新規 1 件含む・build 126）と
  ワークスペース全件（50 スイート・930 件）が成功。xtask check・
  fmt・clippy（警告 0）を通過。
- 実コーパス全件（4,155 ファイル×4 条件）の比較では、成功／失敗
  （4,127／28）・生成コード・Map 中核・正規化済み Map 全体のすべてが
  差分 0 件で、両 TSV はバイト同一だった。
