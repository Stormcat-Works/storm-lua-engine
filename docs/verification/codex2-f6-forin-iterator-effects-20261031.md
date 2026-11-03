# Forin の暗黙の反復子呼出が効果解析に欠落（Codex 第2回指摘6の根、2026-10-31、ワーカー w9）

## 症状

`for _ in it do end` の反復子 `it` がグローバルへ書く副作用（`x=1`）を、効果解析が
ループ文の効果に数えていなかったため、ループ前の書込み（`x=0`）がループ後も
有効とみなされ、ループ後の読みが古い値に畳み込まれた。
再現（第54サイクル c54-cases f06b、対象なし）: 期待 1 に対しコンパイル後は 0。

## 機構

Lua の generic-for は各ステップで反復子自体を（状態値・制御値とともに）呼ぶ。
`EffectAnalyzer::statement_into` の Forin 処理は、反復子式リストと本体の効果だけを
集め、反復子呼出そのものの効果（書込み・呼出・throw）を数えていなかった。

## 修正

Forin 処理で `iterator_call_into` を呼び、暗黙の反復子呼出の効果を合流させた。
分類は直接呼出と同じ規則に従う: 組込みと証明できれば screen./pure/effect の
分類どおり、名前が判明したローカル反復子ならその要約（読み・書込み・呼出・
ordered・may_throw・stable）を合流、それ以外は未知呼出
（calls=true、may_throw=true、stable=false）。

## 検証

- 回帰テスト 2 件を追加（ローカル反復子の要約の書込みが Forin の効果に流れる、
  未知反復子は未知呼出になる）。同クレート 119 件合格、ワークスペース全件
  1,455 件合格・失敗 0、clippy 警告 0。
- c54-cases f06b（対象なし）: 修正前は opt／safe が plain と乖離（出力 0）だったが、
  修正後は全構成が plain と一致（output.setNumber(91,1)）。
- c54-cases f06（target、DIFFEXEC_TARGET_ABS=8192）: 対象なし側は一致するが
  abs8192 構成では依然として plain と乖離する（出力 0 vs 12）。これは target
  現れに必要な MR ヘッダバリア側の残件であり、triage の割当どおり w4 の
  パス群スライス担当（本ラウンドは effects 側の根のみを修正）。
- Tier FULL（基点 `480bd6dee`＝FULL-VERIFY-TIP）: 全 4 条件で 16,620 行すべて
  同一（identical=4,155／hard=0／expl-only=0／map-core=0、ステータス不変
  ok 4,128／error 27、生成コードの文字数差分 0）。コーパス内では今回の効果
  変更が下流の最適化判断を変えるケースはなかった。
