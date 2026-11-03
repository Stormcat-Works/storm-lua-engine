# 候補I（引数省略の試行本体断片折り畳みが再定義済み組み込みを評価する）の修正 — 検証記録

対象: `crates/storm-lua-minify/src/passes/omit_arguments.rs`（＋ `inline_functions.rs` のガード公開）
担当: w9 第64サイクル（候補H＝第252ラウンドの根因クラスの横断監査で発見。Codex 指摘ではなく本線の監査で発見）

## バグ

末尾引数省略（equivalent-trailing-argument-omission）が、関数本体の
試行コピーに対する断片折り畳みの誤判定を根拠に、意味の変わる省略を行った:

```lua
local function f(a) if math.max(1,2)==2 then return 0 else return a end end
function math.max(x,y) return 3 end
output.setNumber(1, f(7))   -- 素: 7 / 最適化: nil（引数が省略された）
output.setNumber(2, f(9))   -- 素: 9 / 最適化: nil
```

## 根本原因

`omit_arguments::normalized_body` は、パラメータをリテラル／nil で置換した
関数本体だけのコピーを試行 AST に構築し、`literal_folding::constant_fold`
（fold_expressions ＋ simplify_control_flow）を本体だけを根として3回適用した
印刷テキスト同士を比較して省略可否を決める。断片を根とする解析では
チャンク側の再定義・遮蔽が見えず、`math.max(1, 2)` が標準組み込みの値 2 で
評価され、`2==2` の分岐が畳まれて、本来は引数に依存する分岐が
「引数に依存しない」形に潰れていた。これは候補H（断片折り畳み）と
同型の盲点で、断片の消費者が inline_functions の外にもいたことが問題。

到達可能な再定義形（全てベース先端 `79c0acf` で発散を実測）:
Funcstat（定義は本体の前後どちらでも）、メンバー代入 `math.max = ...`、
別名経由の書込み `local m = math; m.max = ...`、ルート再束縛 `math = {...}`、
ローカル遮蔽 `local math = {...}`、パラメータ遮蔽 `function f(math, a)`、
別組み込み（math.floor）、式形（`math.max(1,2)==2 and 0 or a`）、
string.format の再定義、本体内の入れ子関数に含まれる組み込み呼出し。
単一呼出し形では inline 系が先に処理して表面化しないため、再現には
2箇所以上の呼出し位置が要る。引数が両分岐に現れる形は当然ながら安全。

なお fold 内のもう一つの解析依存（`is_movable`）は、この比較においては
差異を消す経路にならない: 移動・複製はパラメータ置換箇所を含む部分式にも
両変種へ同型に適用され、リテラル／nil のテキスト差異は残る。値を評価して
分岐を決める経路（eval_math と string.format ゼロ埋め）が唯一の消去経路であり、
修正のガードはその集合（第252の `FRAGMENT_EVALUABLE_BUILTINS`）と一致する。

## 修正

第252ラウンドの `fragment_fold_builtin_safe`（inline_functions）を
`pub(crate)` として公開し、omit_arguments が再利用する。省略判定の前に、
チャンク全体で構築した `EffectAnalyzer` で対象関数の本体を検査し、
断片で評価可能な組み込み呼出しの綴りがチャンク側でも同じ組み込みに
解決されない関数については、その関数の全呼出し位置で省略を行わない
（関数単位でキャッシュ）。置換は被呼出しの綴りを変えないため、
パラメータ置換で新たに評価可能になる呼出しもこの検査で覆われる。

## 検証

- 回帰テスト6件追加（Funcstat／メンバー代入／ローカル遮蔽／式形／
  string.format の各阻止形＋未再定義の組み込み条件では省略が従来どおり
  発火する正例）。ゲートを無効化する突然変異で阻止形5件だけが失敗する
  ことを確認した。
- 実地確認: 変更面スイート（既存ケース群＋今サイクルの新規境界ケース、
  計198ファイル× plain/opt/safe/target32/target128/abs8192）で全件一致。
- ワークスペーステスト 1,662 件全通過。clippy 警告集合はベースと同一。
  変更3ファイルとも rustfmt 適合。
- Tier FULL（累積、FULL-VERIFY-TIP `79c0acf` から）: 全コーパス
  16,620 行・fallback 0、両側 SHA-256 `4a98d093…` でバイト一致
  （変更ファイル 0 件——誤判定の形状は実コーパスに存在しない）。

## 付記: 「断片を根とする解析」の横断監査（第64サイクル課題1）

候補Hの根因クラス（部分木だけを根とする解析がチャンク文脈を見落とす）を
全パスで監査した結果:

- inline_functions の断片折り畳み群（fold_inlined_expression /
  fold_inlined_expression_with_shared / scratch 経路）: 第252のガード下流に
  全経路があることを確認。直接組み込み折り畳み分岐はチャンク解析器を使用。
- inline_functions の specialize_constant_arguments: 全チャンクのクローンに
  チャンクルートで constant_fold —— 文脈保存され健全。
- omit_arguments の normalized_body: **本候補**（未ガードだった唯一の箇所）。
- coefficient_carriers の候補証明: 全チャンク候補クローン＋チャンクルート。
- conditionals の refresh: 全 AST を再解決しチャンクルート（第89サイクルの
  鮮度知見と整合）。
- binding_rescaling / numeric_literals / interval_shifting /
  single_use_forwarding / available_expressions 合成 / search の候補折り畳みと
  scratch 合成群: いずれも全チャンク（クローン）＋チャンクルートで文脈保存。
- その他の EffectAnalyzer 構築は全てパスのチャンクルートを取る。
- constant_fold の他の呼出しはテスト内のみ。結論として、到達可能な発散を
  持つ未ガードの断片解析は omit_arguments のみだった。
