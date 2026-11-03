# 候補S7 検証記録（w9 第71サイクル、第294ラウンド）

対象: `redundant-setcolor-elimination` の並べ替え（reorder）が、セグメント
最後の setColor を除去して後方へ抜ける色状態を変える無言の誤コンパイル。
w6 第73サイクルの境界スイート v35（DR 系）で確定し、課題追加3として
w9 第71サイクルに追加された。修正コミットは統合先端 `cb77237`。

## 症状

主再現（w6 `s7-pixel.lua`）:

```lua
screen.setColor(255, 0, 0)
screen.drawRect(1, 1, 4, 4)
local function poke() screen.setColor(0, 0, 255) end
poke()
screen.setColor(255, 0, 0)
screen.drawRect(6, 1, 4, 4)
```

plain のトレースは
`setColor(255,0,0), drawRect(1,1,4,4), setColor(0,0,255), setColor(255,0,0), drawRect(6,1,4,4)`
で、opt／safe／target は
`setColor(255,0,0), drawRect(1,1,4,4), drawRect(6,1,4,4), setColor(0,0,255)`
となり、ブロックを抜ける色状態が赤でなく青になる（色状態は文・ティック・
コールバックを跨いで持続するため、後続描画の色が変わりうる）。最小形
（`s7-min.lua`）では終了時色状態のみが食い違う。

## 根本原因

`passes/screen_setcolor.rs` の reorder 規則: 同色の先行 set がある後続の
setColor（候補）について、その描画グループを介在する別色の set 群より前へ
移動し、候補自体を除去する。移動先でも描画の実効色は同色の先行 set により
保存されるが、**候補がセグメント内の最後の set の場合、その色はセグメント
から流れ出る色状態そのもの**であり、除去すると介在した別色（青）が後方の
状態になってしまう。規則にはこの「 outgoing 状態」の条件が欠けていた。

融合（one-use-function-fusion）は到達経路にすぎない。同形を直接の
`screen.setColor(0,0,255)` で書いた形でも同一の発散を実測した（本記録の
検証で確認）。通常の呼出し経由では呼出し文がバリアとなりセグメントが
分断されるため発動しなかった、というのが w6 の境界観察と整合する。

## 修正（`cb77237`）

`find_reorder_in_segment` に条件を追加: 候補がセグメント内の最後の set の
場合は移動しない。候補の後ろに別の set があれば、その set が outgoing
状態を再確立するため移動は健全であり、最適化は維持される。モジュール
doc に条件と理由を明記。既存単体テスト2件（`reorder_moves_disjoint_group_
and_removes_set` の旧形と `different_color_between_blocks_base_removal`）は
まさにこの不健全な形を期待値として符号化していたため、期待値を是正した
（前者は末尾 set を追加した形を正例として保持）。

## 検証

- 単体テスト: 新規2件（セグメント最後 set の遮断、融合後形状の遮断）を
  red→green で確認。storm-lua-minify 全 1,132 件合格。
- w6 候補5形（s7-pixel／s7-min／s7-two-stmts／s7-two-fns／s7-args）と
  直接形が、既定を含む全構成で src／plain と一致。
- スイート回帰: v35 は 58/59 一致。残る lp11 は修正前バイナリでも同一
  ハッシュで発散する既存の OUTPUT-STORE 認定クラス（本件と無関係）。
  v33 は 46/46 一致。v34 は 64 件一致＋認定済み OUTPUT-STORE 4 件のみ。
- Tier FULL（累積 base = FULL-VERIFY-TIP `f938d38`）: 全4構成で
  identical=4,147、hard=8、map-core=0、expl-only=0、missing=0。status
  内訳不変（ok 4,128／error 27）。hard 8 件は実行比較で検証し、1 件は
  全構成一致、7 件は「各描画の実効色・最終色状態・重なる描画の相対順序」
  がすべて保存される健全な並べ替え（画素同一性契約の範囲内）と判定した。
  総コードバイト（default-nomap）は 6,655,285 → 6,655,358（+73、
  不健全に除去されていた set の保持による微増）。
- ワークスペース全テスト 1,750 件合格。clippy は変更ファイル無指摘、
  rustfmt 済み。

## 申し送り

- 本記録の対象外として、第290–293ラウンドの記録で仮番 S7 とした
  `immutable-global-expression-reuse` の候補（再検証パッケージで確定した
  input.* 読取り結果の文脈跨ぎ再利用）は、w6 の番号付けでは S7 が本件に
  確定したため、改番のうえ親の割当て待ちとする。
