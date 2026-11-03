# constant-folding の and/or 畳み込みによる多値切り詰め喪失（2026-10-30、未公開）

## バグ

`and`／`or` 式は Lua では常にちょうど 1 値を返す（右辺が多値式でも
調整される）。constant-folding パスは、左辺が既知のリテラル（伝播した
名前を含む）の場合に `and`／`or` ノード全体を選択した子ノードで置換
していたが、選択した子が裸の呼出または `...` の場合、その 1 値調整が
失われ、展開位置では多値が展開して意味が変わっていた。

- `local a, b, c = true and pair()`（`pair()` は 3 値を返す）と書くと、
  plain では `(7, nil, nil)` のところ最適化後は `(7, 8, 9)` になった。
- 同型は `false or pair()`、`nil or pair()`、`"x" and pair()`、
  伝播した名前（`local c = true` に対する `c and pair()`）、
  比較の畳み込み（`(1 == 1) and pair()`）、vararg（`true and ...`）、
  呼出最終引数・テーブル最終要素・return 最終式でも発火した。
- safe モードでも発火した（smallest のみではない）。
- 発火しない対照：`false and pair()`（false スカラーへ畳む）、
  `true or pair()`（true スカラーへ畳む）、`0 or pair()`（0 スカラーへ
  畳む）、`{} and pair()`（テーブルリテラルは真値畳みの対象外）、
  右辺側の恒等畳み（`pair() or false` 等は当該パスでは畳まれない）。

単独パス無効化スイープ（正本 73 件）で constant-folding の無効化でのみ
直ることを確認した（トグルは `passToggles`＝camelCase で送ること。
snake_case は無視される）。

## 原因箇所

`crates/storm-lua-minify/src/passes/literal_folding.rs` の 2 箇所に
同一の穴があった（constant-folding パスは fold と exact simplify の
両段からなり、トグルは両段をまとめて制御する）。

1. `fold_node_inner` の BinOp 畳み（`and`／`or` で左辺リテラルが既知の
   場合に選択子で置換）。
2. `fold_exact_safe_node_inner` の同型の BinOp 畳み（simplify 段）。

両箇所とも、BinOp ノード自体が持っていた 1 値調整を、子ノードへの
置換で落としていた。同ファイル内の Paren 処理は「括弧化された呼出は
1 値であり、括弧を外すと全値が露出する」ことを明記して慎重に扱って
いるが、and/or 畳みにはその配慮がなかった。

## 修正

同ファイルのみ。両箇所で、選択した子ノードが Call または Vararg の
場合に `Node::Paren` で包んでから置換する（既に括弧化されている子は
そのまま）。非展開位置でも括弧化は意味を変えず、+2 文字はバグ修正
として許容される。左辺リテラルへの畳み（スカラー選択）や両辺リテラ
ルの評価（`eval_binary`）は不変。

## 回帰テスト

同ファイルの tests に 3 件追加。いずれも修正前は失敗し、修正後は成功
することを両方向で確認した。

- `and_or_fold_parenthesizes_multivalue_call`（fold＋simplify 両段経由）：
  `true and f()`／`false or f()`／`nil or f()` が `(f())` になること、
  `true and ...` が `(...)` になること、スカラー畳み（`true and 7` 等）
  が括弧なしで従来どおり畳まれること。
- `fold_expressions_parenthesizes_multivalue_call`（fold 段のみ）。
- `exact_safe_fold_parenthesizes_multivalue_call`（simplify 段のみ）。

## 検証

- 境界合成スイート v3（38 入力）：修正前は MATCH 33・TRACE 4・INV 1
  （意図的不正）、修正後は MATCH 35・TRACE 2・INV 1。残る TRACE 2 件は
  非バグ（下記）。
- and/or プローブ群（12＋8 入力）：修正前は TRACE 12 件、修正後は
  全件 MATCH。
- ワークスペース全件テスト 1,317 件成功・失敗 0 件、xtask check・
  fmt・clippy（lib）通過。
- Tier FULL コーパス比較（開発ホスト1、manifest ハッシュ abfec87a…、
  照合 4,155/4,155 OK）：分岐元 7490f5e（第162ラウンド先端）対修正
  head、および累積起点 a300a99e（第161ラウンド先端）対修正 head の
  両比較で、各 4 条件とも identical 4,149 件・hard 6 件・map-core 0 件・
  説明系のみ 0 件・欠損 0 件・ステータス変化 0 件（ok 4,128 件・error
  27 件は両側同一）。両比較の base 側 TSV は SHA-256 バイト同一で、
  第162ラウンドの出力変化が 0 件であることとも一致する。hard 6 件は
  いずれも生成コードの差が括弧 1 対の追加のみ（括弧を除去すると base
  と head は完全一致）で、修正後バイナリでの実行照合では 6 件とも
  plain／opt／safe のトレースが完全一致した。

## 非バグの TRACE（スイート v3 の残り 2 件）

- `v3-for-triple`：文字列キーテーブルに対する `next` 反復の順序が
  VM 状態ごとに非決定的で、同一入力の再実行でトレースハッシュが
  入れ替わる（3 回の再実行で plain／opt／safe のハッシュが入れ替わり
  を確認）。w9 の既知の注意点（文字列キーの pairs 順は構成依存）と
  同じクラス。
- `v3-for-ipairs-call`：入力設計の誤り。`for i, v in ipairs, {5, 6}, 0`
  は反復ごとに `ipairs(t, var)` を呼び直す誤用で、命令予算（Limit）
  まで無限に反復する。plain と opt／safe の差は予算到達までの反復
  回数の差（最適化コードの命令数が少ないため）で、既知の予算
  アーティファクト。意図した形（`ipairs({5, 6})` の 3 値を返す呼出を
  汎用 for に渡す）は別プローブで MATCH を確認した。
