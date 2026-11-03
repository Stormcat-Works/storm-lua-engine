# 候補H（断片折り畳みが再定義済み組み込みを定数化する）の修正 — 検証記録

対象: `crates/storm-lua-minify/src/passes/inline_functions.rs`
担当: w9 第63サイクル（第62サイクルの敵対的検証で発見。Codex 第3回 #2・immutable 再利用とは別根因）

## バグ

同一チャンク内で組み込みを再定義しても、リテラル呼出し折り畳みが
旧い組み込みの値で定数化した:

```lua
local a = math.max
local function g() return math.max(1, 2) end
function math.max(x, y) return 99 end
output.setNumber(1, g())   -- 素: 99 / 最適化: 2
```

## 根本原因

全体チャンクに対する折り畳みは健全である: `literal_folding` の
全体パスと `rewrite_literal_calls` の組み込み分岐は、チャンク全体で
構築した `EffectAnalyzer` を使い、`mutated_builtin_roots` が Funcstat・
メンバー代入・別名経由の書込み・for 状態変数を全て記録するため、
再定義後の解決は失敗し折り畳まれない。

不健全だったのは `inline_functions` 内の「断片」折り畳みである。
候補関数の本体を複製し、合成した断片（対象 AST 内の
`Block[Return(expr)]`、または切り離した scratch アリーナ）を根として
`literal_folding::fold_expressions*` を呼ぶ経路
（`fold_inlined_expression` / `fold_inlined_expression_with_shared` /
`scratch_fold_atom`）は、断片だけを根とする別の `EffectAnalyzer` を
構築する。チャンク側の再定義・遮蔽は断片からは見えず、`math` は
組み込みとして解決され、`math.max(1, 2)` が 2 に畳まれていた。

到達可能な再定義形（全てベース先端 `7d618d6` で発散を実測）:
Funcstat（定義の前後とも）、メンバー代入 `math.max = ...`、
別名経由の書込み `local m = math; m.max = ...`、`math` の
ローカル遮蔽、本体内の入れ子位置（`math.max(1, 2) + 1`）。
パラメータ置換では Name/Index 形の被呼出しは生成されない
（候補の引数はリテラルのみ）ため、到達可能性は本体に直接
書かれた綴りの経路に限られる。

## 修正

`rewrite_literal_calls` が候補本体を断片折り畳みする前に、
チャンク側の解析で検査するガード `fragment_fold_builtin_safe` を
追加した。本体の部分木（入れ子の関数本体を含む）を走査し、
断片評価器が評価し得る被呼出し（`eval_math` の `math.abs/atan/
cos/floor/max/min/sin/sqrt/tan` と `string.format` のゼロ詰め形）
について、その綴りの経路がチャンク側
`resolve_builtin_reference` でも同じ組み込みに解決されることを
要求する。1件でも失敗すれば、その候補の断片折り畳みを丸ごと
行わない（文位置・式位置・scratch・フォールバックの全経路が
この1箇所の下流にある）。

- 断片内部の再定義・遮蔽は断片側の解析自身が見るため、
  ガードは外側チャンクの文脈だけを担う。別名綴り（`m.max`）は
  断片でも畳まれないため対象外。
- `string.format` は同じ評価器（`fold_expressions`）が実装する
  ため集合に含めたが、ベースで発散する形状は5形状の試行では
  見つからなかった（サイズ/形状ゲートが断片経路では発火しない）。
  防御的な包含であり、実バグは `math.*` 評価器である。

## 検証

- 回帰テスト8件を `inline_functions` テストに追加（Funcstat 前後・
  メンバー代入・別名書込み・ローカル遮蔽・string.format・入れ子・
  対照群）。ガードを無効化すると math 系6件が失敗することを確認
  （対照群と string.format は両状態で通過）。
- パイプライン実測（diffexec、詳細度3）: 上記全変種＋原ケースが
  ベースで発散し、修正後は素と一致。別名取得 `local a = math.max`
  の併用形でも、再定義後の `g()` は 99・取得済み `a(1, 2)` は 2 と
  素の意味論どおり。
- ワークスペーステスト: 1,652 件全通過（第62サイクル 1,644＋新規8）。
  clippy は完了し、警告集合はベースと同一（追加コード由来なし）。
- Tier FULL（累積、FULL-VERIFY-TIP `7d618d6` から）: コーパス
  16,620 行が base／head とも SHA-256
  `4a98d0939f13cb4f3f888393087bbb64f8945cf3e5a9154e9bb7680ade733028`
  でバイト一致（fallback 0、変更ファイル 0 件）。間に挟まる
  第251ラウンド（w7、Tier INVARIANT）も同じハッシュで不変。
