# 混在マップの name プール順序不一致で自前 validate が失敗（Codex 第2回指摘9、2026-10-31、ワーカー w2）

## 症状

低レベル API 経路（lexical_minify_with_origins → origin を derived で変異 →
encode()）で、encode() は成功するのに、その出力が自前 validate() で
"standard mappings do not agree with the detailed origins" と失敗した。
再現（第81サイクル c81probe、合成ソース return a+b）: 名前付き origin の
うち a を derived("example-rewrite") で変異させると再現する。b の変異や
高レベル minify 経路では発火しない（第54サイクルでも両先端で確認済み）。

## 機構

標準フィールドの names は元の origin 順でプールされる（この例では
[a, b]）。一方、混在形の展開（expand_token_table と
merge_token_and_residual）はトークン表由来の単純 origin を先に並べ、
残余 origin を後ろへ回す。a が derived 化で残余へ移ると、展開後の
origin 列は [return, +, b, a] となり、再計算した name プールは
[b, a] になってワイヤ上の標準マッピングの name ID と一致しなくなる。

## 修正

extract_token_table に、再編成（単純 origin を元の相対順で先に、
残余 origin を元の相対順で後ろに）後の一意 name 列が、元の origin 順の
一意 name 列と一致する場合に限ってトークン表を抽出するゲートを追加した。
一致しない形は拡張全体を従来レイアウトで出すため、由来帰属は全スパンで
保持され、由来不明のスパンは生じない。validate／validate_for_code は
不変。生成コードとリネームには触れていない。

## 検証

- 回帰テスト1件を追加（a の derived 化では従来レイアウトへ倒れて
  validate 通過、b の derived 化では混在形を維持して validate 通過）。
  対象クレート 135 件合格、ワークスペース全件 1,456 件合格・失敗 0、
  xtask check 通過、fmt 適合、対象クレート clippy 警告 0。
  ワークスペース全体 clippy は storm-lua-minify の既存テストコード
  （loop_member_reads.rs の expect_used 4件、本変更の差分外）で失敗する。
- c81probe: 4 origin すべての derived 変異で encode → validate 通過。
  高レベル経路も validate 通過を再確認。
- 構成済み project／lifeboat の再確認（最大の字句勝者＋helper の
  2モジュール構成、第178ラウンドと同一構成）: 混在形を維持
  （project tokens 76,681・残余 origins 20、lifeboat tokens 76,689・
  残余 origins 84）、生成コード長は base と同一、validate 通過。
  Map 長の base 比 -1B は未コミットツリーの producer メタデータ差で
  レイアウト差ではない。
- Tier FULL（基点 82fab7e6c＝着手時 FULL-VERIFY-TIP、実行前に manifest
  照合 4,155/4,155 OK）: 16,620 行すべてバイト同一
  （identical=4,155／hard=0／expl-only=0／map-core=0／missing=0、
  ステータス不変 ok 4,128／error 27）。コーパス単一 compile の中では
  ゲートが発火する形は存在しなかった。
