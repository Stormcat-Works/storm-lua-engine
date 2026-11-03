# TOTAL_API 分類の引数検査欠落（Codex 第2回指摘10、2026-10-31、ワーカー w9）

## 症状

`local u = math.floor(nil)` のように、静的にエラーになる引数で TOTAL_API（決して
throw しないと分類された組込み）を呼ぶ未使用呼出が、デッドコード除去で消え、
元は引数エラーで止まるプログラムが正常終了するようになった。
再現（第54サイクル c54-cases）: f10 `math.floor(nil)`、f10b `math.abs({})`、
f10c `type()` の3変種すべてで、コンパイル後はエラーなく 1 を返した。

## 機構

`storm_lua_analysis::effects` の呼出分類は、組込み名が TOTAL_API に含まれるだけで
`may_throw = false` とし、引数の個数も型も見ていなかった。基盤段階（トグル対象外）の
デッド呼出除去はこの判定を信頼して呼出文を削除する。

## 修正

`EffectAnalyzer::total_api_call_throws` を導入し、expr_into と参照実装
（allocating_expr_into）の両方の TOTAL_API 判定を置換した:

- TOTAL_API でなければ従来どおり throw しうる。
- 実効引数が 0 個なら throw しうる（全 TOTAL_API メンバは最低1引数を要する。
  末尾の Call/Vararg は 0 個に展開しうるため、その上に載る引数は個数に数えない）。
- math メンバについては、静的に非数値と分かる引数を検査する（一引数メンバは
  第1引数のみ、max/min は全固定引数）。数値判定は Lua 5.3 の文字列強制規則に沿う
  （数値文字列は数値として受理、inf/nan 等の綴りは拒否）。静的に型が分からない
  引数は従来どおり判定しない（カタログの動的値に対する既存契約を維持）。

## 検証

- 回帰テスト 2 件を追加（静的に throw する6形: floor(nil)／abs({})／type()／
  max()／floor('x')／tostring()、total のままの6形: floor(2.5)／abs(-1)／
  floor('3')／type(1)／max(1,2)／floor(x) 動的引数）。同クレート 117 件合格、
  ワークスペース全件 1,453 件合格・失敗 0、clippy 警告 0。
- c54-cases f10／f10b／f10c: 修正後、通常・safe の両モードでコンパイル後も
  元と同一の引数エラー（floor/abs/type）になることを確認。
- Tier FULL（基点 `535a49b5b`＝FULL-VERIFY-TIP）: 全 4 条件で identical=4,151／
  hard=4／expl-only=0／map-core=0、ステータス不変（ok 4,128／error 27）。
  hard 4 件はすべて diffexec で base／head 両側のトレースが一致（生成コード合計は
  条件あたり +41 文字）。うち a6bb010e は base／head の双方で opt と plain の
  トレースが異なる既存の乖離を含むが、両ビルドで完全に同一であり本修正の
  導入ではない。
