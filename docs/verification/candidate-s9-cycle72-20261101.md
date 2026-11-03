# 検証記録: 候補S9（immutable-global-expression-reuse の load-order 大域 carrier）

- 実施: w9 第72サイクル（2026-11-01）
- 対象コミット: 本記録の直前の修正コミット（分岐元 `8e9fefc`＝FULL-VERIFY-TIP／第296ラウンド先端）
- 対象ファイル: `crates/storm-lua-minify/src/passes/immutable_values.rs` のみ（回帰テスト 5 件を含む）
- ティア: FULL（生成コードが変わる正しさ修正）

## 問題

`immutable-global-expression-reuse`（pass id 18）は、チャンクレベル文に束縛
された式（carrier）と一致する式を関数本体内で carrier への参照に置換する。
大域 carrier はその束縛文がロード時に実行されて初めて値を持つが、pass は
置換先の関数が束縛文より前にロード時に実行されうるかを検査していなかった。
束縛前に呼ばれた関数は carrier 大域を nil のまま読み、式の値の代わりに nil
を観測する（第72サイクルの S8 検証記録で副次所見として記録、w6 第76サイクル
が実測で候補化）。

w6 の最小再現（loadorder-min.lua）:

```lua
local t = {n = 40}
function f() return t.n + 2 end
output.setNumber(1, f())
G = t.n + 2
output.setNumber(2, G)
```

plain は output (1,42)／(2,42) を出すが、最適化形は (1,nil)／(2,42) を出す。
間接呼出し（h 経由）でも同一ハッシュ対で発散する。発火には決定的式で十分で、
純粋な定数式（`math.max(1,2)` 等）は定数畳み込みで値が一致するため見かけ上
不発になる。局所 carrier は不発（後方宣言の局所は前方定義の関数内では別束縛
に解決され、候補生成後の解決同一性チェックが候補を却下する）。

## 修正

`LoadOrderGuard` を新設し、大域 carrier ごとに、その束縛文までのロード
プレフィクス（束縛文を含む）で実行されうる関数集合を過剰近似で求め、その
内部にある置換サイトを抑止する（サイト単位の抑止であり、到達不能な関数内の
置換は温存される）。

- 到達可能性: プレフィクス各文および到達済み関数の本体の「外側」（関数
  リテラルの内側は定義のみで実行されないため剪定）にある関数参照（呼出し
  位置・値位置の双方）を種とし、本体の参照で推移閉包を取る。即時実行
  リテラル（IIFE）の本体は外側として走査する。汎用 for の反復子式も被呼出子
  として分類する。
- 動的呼出し: 被呼出子が既知のチャンク関数にも builtin／API 参照にも解決
  できない呼出し（メソッド呼出し・別名経由・計算されたターゲット等）と
  `setmetatable` の設置は、その時点で束縛済みの全関数へ到達しうるとみなす。
- 同定: 名前束縛関数は BindingId、フィールド束縛関数（`function T.m()` /
  `T.m = function`）はドット連鎖キーで同定する。識別できない関数リテラルは
  それを含む root 文の位置で扱い、プレフィクス内のものは抑止側に倒す。
- 割当て／関数文のターゲット名そのものは宣言であって参照ではないため、
  走査ではターゲットの部分式（基底連鎖とキー）のみを対象とする。

## 検証

- 単体テスト: minify クレート 1,145 件合格（immutable_values は 38 件）。
  新規 5 件: ロード時直接呼出し・間接呼出し・IIFE での抑止、束縛後の呼出し・
  ティック内のみの呼出しでの温存（対照）。
- w6 形状の回帰（diffexec）: loadorder-min／loadorder-indirect／
  loadorder-tick と対照 lo3（束縛後呼出し）／lo4（ティック内のみ）が
  plain／opt／safe で全て一致。
- S8 拡張集合の再確認（c72tickexec）: c75-candidates 8 形＋c75-suite-v37
  30 形は修正前と同一の閉じ方（全 S8 族が一致、残る lc-e08 は OUTPUT-STORE
  の文書化済みクラスのみ）。
- Tier FULL: 累積 base `8e9fefc` → head で全 4 構成 identical=4,152、
  hard=3、map-core=0、missing=0、status 不変（ok 4,128／error 27）。
  hard 3 件は各 +2 バイト（抑止された置換 1 件ずつ）で、diffexec の実行
  トレースは base／head とも全構成一致を確認した（本修正による保守的
  抑止のみが差分）。総バイト +24。
- 変更ファイルは rustfmt 無指摘、clippy は変更ファイル無指摘。
