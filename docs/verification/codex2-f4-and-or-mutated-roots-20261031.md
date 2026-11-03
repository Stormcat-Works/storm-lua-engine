# mutated_builtin_roots の and/or 経由エイリアス見逃し（Codex 第2回指摘4、2026-10-31、ワーカー w9）

## 症状

`local m = input.getBool(0) and math or {}` のような条件付きエイリアス経由で
ビルトインテーブルを書き換えても（`m.abs = function() return 99 end`）、コンパイラは
`math` を不変と信じ続け、古い `math.abs` を事前捕捉・定数畳み込みした。
再現（第54サイクル c54-cases f04／f04b）: 期待 99 に対し、対象なし形では
1,2,3,4（リテラルへ畳み込み）、target 形では 2（`string.gsub` コールバック内の
変異後に事前捕捉した旧 `math.abs` を使用）。

## 機構

`storm_lua_analysis::effects::mutated_builtin_roots` は、値式の基底バインディングを
`base()`／`named_base()` で取る。どちらも `Binop(and/or)` を基底なしとして落とすため、
and/or チェーン経由で届くエイリアスには辺が張られず、そのエイリアスへの書込みも
エスケープ（呼出・戻り値・テーブルへの値渡し）も、元テーブルへの変異として記録されなかった。

## 修正

値フローに and/or 葉展開を導入した。`value_bases`（エイリアス辺用、`base` を葉に適用）と
`named_value_bases`（エスケープ用、`named_base` を葉に適用）は、Paren と
Binop(and/or) を再帰的に降りて到達しうる葉すべてを集める。代入先の基底にも
`target_bases` として同じ展開を適用した。和集合（may）の意味論は不変で、過小評価だけが解消される。

## 検証

- 回帰テスト 4 件を `storm_lua_analysis` に追加（and/or エイリアス書込み、素のエイリアス書込み、
  呼出へ渡した and/or 値のエスケープ、通常テーブルだけの and/or は無印）。同クレート 115 件合格、
  ワークスペース全件 1,451 件合格・失敗 0。
- c54-cases f04（target 絶対 8192）／f04b（対象なし）: 修正後、全構成の実行トレースが plain と一致
  （f04 は output.setNumber(91,99)、f04b は 99,99,99,99）。
- Tier FULL（基点 `2f906671b`＝FULL-VERIFY-TIP）: 全 4 条件で identical=4,150／hard=5／
  expl-only=0／map-core=0、ステータス不変（ok 4,128／error 27）。hard 5 件はすべて
  and/or エイリアスを含むファイルで、生成コード合計は条件あたり +122 文字（バグ修正による
  保守化のコスト）。5 件とも head 側 diffexec で plain／opt／safe のトレースが一致し、
  base 側トレースとも一致（実行意味の変化なし）。
