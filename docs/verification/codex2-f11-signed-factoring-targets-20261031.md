# signed factoring が代入ターゲットを置換しメンバ書込みが消える（Codex 第2回指摘11、2026-10-31、ワーカー w9）

## 症状

FINDINGS.md 記載の正確なプログラム（第54サイクルで両先端で再現済み）:

```lua
local t = { x = 3 }
local c = 0
t.x = t.x + 1
c = c + t.x
t.x = t.x + 1
c = c + t.x
t.x = t.x + 1
c = c + t.x
return c, t
```

原文は (15, {x=6}) を返すが、Smallest／target 絶対指定（8192）では
(15, {x=3}) を返した。生成コードは `a=c.x; a=a+1; b=b+a; …` の形で、
書込み先までキャリアに置換されており、実テーブルへの書込みが一切
行われていなかった。`signed-expression-factoring` の無効化で一致が復旧
する。なお output.setNumber への適応形では再現しない（グループ形成が
変わるため）。

## 機構

2つの欠陥の協働である。

1. `rewrite_signed` の再帰置換は式の位置を区別せず、代入ターゲット位置
   （`Node::Index` の基底など）まで factored 式をキャリアで置換していた。
2. グループ分割の安全検査は、出現文どうしの「あいだ」にある分の書込み
   しか見ていなかった。出現文の合間にあるメンバ書込みは barrier の
   may_throw 経由で偶然分割されていたが、出現文自身が factored 式の
   基底テーブルへのメンバ書込み文である場合、その書込みでグループを
   分ける検査が無かった。さらに `count_matched_bid` がターゲット位置の
   出現を一致として数えるため、書込み対象のテーブル束縛がキャリアの
   再利用（同一束縛への代入）で上書きされ得る経路も残っていた。

## 修正

`crates/storm-lua-minify/src/passes/signed_factoring.rs` のみ（+127 行）。

- `rewrite_signed`：`Node::Assign` では値の式だけを書き換え、ターゲット
  はそのまま保持する。
- グループ分割条件に `statement_writes_group_member` を追加：出現文の
  代入ターゲット（Index／Paren をたどった基底名）の BindingId が
  グループの読み束縛に含まれれば、その出現文でグループを分割する。
- `count_matched_bid`：`Node::Assign` では値の式だけを数え、ターゲット
  位置の出現を一致として数えない。

パス単独プローブ（正確形／書込みがグループ末尾／書込みがグループ先頭／
書込みが出現の合間）の4形で、修正後にすべて正しい意味論へ転換すること
を確認した。ターゲットが factored 式の真部分式で RHS に読みがある場合
（`t.longname = t.longname + 1`）も、ターゲットは保持され RHS のみ
factor される。

## 検証

- c54-cases 再実行（diffexec、plain／最適化／safe／target32／target128 の
  5構成×3ティック）：f11-exact／f11 は全構成でトレース一致に転換
  （修正前は Smallest／target 系で乖離）。他ケースは従前どおり
  （f05／f10 系はエラー保存のまま、f06 は指摘6 の MR ヘッダ形として
  乖離継続——本修正の対象外、w4 担当分）。
- 回帰テスト4件を signed_factoring のテストに追加。いずれも修正前の
  コードで失敗することを確認済み。
- ワークスペーステスト 1,521 件合格（従前 1,517＋新規4件）。
  `cargo xtask check` 合格。clippy の診断は本変更ファイルにゼロ
  （single_member_cache.rs／loop_member_reads.rs の error は
  第205〜208ラウンド由来の既存分）。
- Tier FULL（base = 12eca85eb、head = 本修正、コーパス 4,155 件×
  4条件、マニフェスト全件照合済み）：status は全条件で不変
  （ok 4,128／error 27 の組が同一）。生成コードが変わったのは 46 件で、
  すべて diffexec の5構成×3ティックで base／head／原文のトレースが
  一致した（観測可能な振る舞いの変化なし）。うち3件は誤って消えて
  いたメンバ書込みが復活した形（書込み数 18→20、28→30、18→20）、
  残りはグループ分割に伴う生成名の変動。文字数は default-nomap 合計で
  +857 文字（不正に短くなっていたコードの是正分）。
- 統合時の再検証（base を当時の FULL-VERIFY-TIP
  e225ac969 へ付け替えて Tier FULL を再実行）：変更ファイル集合は
  同じ 46 件、文字数差分 +857、status 不変、diffexec トレース一致
  46/46 と、付け替え前と完全に同じ結果を再現した。ワークスペース
  テストは rebase 済みツリーで 1,530 件合格。
