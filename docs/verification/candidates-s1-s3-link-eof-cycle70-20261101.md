# 検証記録: 候補 S1–S3＋link チャンク終端エラー帰属（w9 第70サイクル、2026-11-01）

対象コミット: リンク修正（第288ラウンド）と minify 修正（第289ラウンド）。
いずれも Tier FULL（累積 base = FULL-VERIFY-TIP `0fceb03`、第287ラウンド revert 後）で検証。

## 課題1: 生成型キャンペーン第5シード（seed 7001）

gen-c66 系統で 4,000 プログラムを生成し、rawexec（生 mlua Lua 5.3）×2 実行 ×
全構成（plain/opt/safe/target32/target128/abs8192）で比較した（28,000 行）。
非一致は 538 行 / 134 プログラムで、全件が既知クラスに閉じた。**実発散ゼロ**。

| クラス | 行数 | プログラム数 | 判定 |
|---|---|---|---|
| ORACLE-NAME | 410 | 82 | オラクルの注釈差（既知） |
| TRACE | 50 | 10 | 異常終了到達点差。全10件とも生側自体が runtime-error かつ生トレースが非決定的（run1≠run2）。f000665 は全行一致＋RL 行の到達回数差（±1）を実測確認 |
| TOLERANT | 48 | 12 | 1e-6 許容内（設計どおりの数値丸め） |
| UNSTABLE | 28 | 28 | 生側の非決定性（pairs 順序） |
| SRC-INVALID | 2 | 2 | 生成器アーティファクト（生 Lua がソース自体を拒否） |

## 課題2: raw-Lua 再検証パッケージ（開始時先端 `a0fc65d`／コード先端 `d97f193`）

保持スイート全てを原形のまま生 Lua 5.3 と比較した。raw 部門 325 件
（c54/c57/c58/c59/c62/c63h/c63g/c64/self68/tprogs/camp6601/6701/6801/6901/v24）:
AGREE 307、DIALECT-AGREE 3（方言が受理する goto/ローカル上限の文書化済み差異）、
NONUTF8 1（既知）、NONDET 3（生側非決定、c68/c69 の UNSTABLE 保持件と一致）、
TIMEOUT 9（生側が制限時間超過＝重量保持プログラム、判定不能として記録）、
DIVERGE-LOAD 2（c69 の SRC-INVALID 対 f001969/f003761）、DIVERGE 44。
DIVERGE 44 件は全件「コンパイル側全構成が一様に runtime-error」で、その実体は
方言 VM の資源上限（`toplevel:error:Limit`）による終了であり、生 mlua（上限なし）が
完走する重量保持プログラム（camp/tprogs の発散保持件）との非対称であることを
f001178 で実測確認した。新規発散ではない。
リンク部門 v25（44件）は c62/c63 のトリアージと完全一致（fail-closed 診断群と
c03/c03b/c04 の既知残課題のみ）。方言部門 v27（39件）は全件が文書化済みクラス
（lastwins/dialect-accept/both-reject/agree、p04b は文書化済み freeze モデルの
`10|30|20|2`）。リンク追加部門（c54/c59 の 18 JSON）はフラグ 0。

## 課題3: link のチャンク終端ロードエラーの帰属（第288ラウンドで修正）

ラッパ化モジュール内の未解決 goto 等、本体終端で判明するロードエラーは、
パーサが関数本体を閉じた時点の先読みトークンの行で報告される。参照チャンクでは
先読みは EOF であり帰属はモジュール EOF 行（c03/c03b では m:3）になるが、連結
チャンクでは先読みがラッパの生成 `end` の次の生成文へ進み、どの LinkedRange にも
属さない行が指されて逆引きが NONE になっていた。

修正（`crates/storm-lua-build/src/link.rs`）:
- `Sink::close_wrapper`: 本体最終 verbatim 区間が出力末尾（=元ソース末尾）に届く
  場合に限り、区間の行範囲を閉じ行まで含める（a: 本体が改行で終わる場合）、
  または閉じトークンを本体最終行へ相乗りさせる（b: 改行で終わらず最終行に
  コメント開始が無い場合）。バイト区間は一切変更しないため、バイト逆引き
  （`lookup_source_byte`）と Source Map のバイト契約は不変。行範囲の再計算で
  導出される経路（metadata/lifeboat）でも劣化は graceful。
- ラッパ定義の閉じを `end;` とし、空文で先読みトークンを閉じ行に留める。
- entry チャンクについても、末尾区間が末尾改行の EOF 行を含むよう拡張した。

検証: w6link 実測で c03/c03b が `plain 4 mapped=m:3`（参照帰属と一致）、
entry 形2件が main:3/main:2 に一致。v25 全44件の再実行では ERRLINE フラグが
消滅し、残差分は c04 の文中行番号 6→5（連結本質として記録済み、写像は不変）
のみ。単体テスト5件を追加し、既存テスト2件のレイアウト主張を新形へ更新した
（終端ラベル `::L:: end;` が素の Lua 5.3 と方言 VM の双方でロード可能である
ことを実測で確認済み。ラベルの後に「文」が続く場合にのみスコープ内跳躍になる）。

## 課題4: 候補 S1–S3（w6 第65サイクル確定分）

### S1: tiny-literal-inlining が幻影 `self` 出現へリテラルを代入する（第289ラウンド）

根: `replace_binding_with_literal` は解決 binding id が一致する全 Name を置換するが、
メソッド本体内の `self` 出現は AST が暗黙パラメータを束縛しないため外側の
`local self` へ解決されており、実行時はレシーバを指す。第277ラウンドの幻影保護
（scalarization/flattening/改名）はこのパスに届いていなかった。
修正（locals.rs）: メソッド本体内の `self` 出現ノード集合（closed_fields と同じ
`implicit_self_depth` 方式で算出）を置換対象から除外した。出現単位の除外なので、
メソッド外の真正な読みへの代入は維持される。

### S2: carrier 合成の trap 監査が連結 `..` の確実な trap を評価しない（同）

根: `contains_certain_trap` の Bin アームに `..` が無く、bool/nil 被演算子の連結が
「確実な trap」と評価されなかった（eval_binary も `..` を扱わない）。
修正（immutable_synthesis.rs）: `..` 専用アームを追加した。被演算子が Table/Function
ノード（評価時点で `__concat` を持ち得ない新鮮な値）、または前提値が Bool/Nil の
場合に確実な trap とする。`==`/`~=` は trap しないため対象外のまま。

### S3: 意味ガードが single-use-local-sinking の畳み込みに依存する（同）

根: (ii) numeric_literals の単一定義追跡4箇所（callee_member_name /
table_root_binding / callee_index / callee_is_exact_member）が反復上限4で打ち切られ、
sinking 無効化時に深さ4以上の別名チェインへ届かない。(iv) callee_field_value_is_exact が
基底 binding の定義そのものがコンストラクタであることを要求し、`math = t` 形
（t の単一定義がコンストラクタ）の保護が sinking の畳み込み依存だった。
修正（numeric_literals.rs）: 追跡を共通ヘルパ `chase_single_definition` に統一し、
上限を訪問済み binding 集合に置換した。健全性: 各ステップの正当化（単一定義の
binding は全ての読みでその定義式の値を生む）は深さに依らない。終了性: 真正な
別名チェインは binding を再訪しない（定義が推移的に自分自身の値になることはなく、
自己参照は外側 binding へ解決される）。相互単一代入（`x = y; y = x`）のような
循環定義グラフでも訪問済み集合が保守的に打ち切る。加えて field-value 検査が
基底の単一定義をコンストラクタまで追跡するようにし、非リテラルキーのフィールドが
名前付きフィールドより後にある場合は値を未知として exact（安全側）に倒すようにした。

検証: 単体テスト（深さ4/6チェイン、書込みなしチェイン、循環終了、大域根 m108_03 形、
動的キーの前後）。スイート検証は v28 全60件＋c65 候補5件＋m108_03 を、
既定／sinking 無効化／（候補については）S2 のマスク解除トグル3種
（expression-helper-inlining／signed-expression-factoring／scope-renaming 無効化）
で実行し **147/147 一致**（ad00/ad15 は設計どおりの tolerant 丸めとして記録）。

## Tier FULL（累積、base `0fceb03` → head 第289ラウンド）

- corpuscheck（正本ハーネス SHA-256 `3c07a763…`）で全 4,155 ファイル × 4 構成:
  両側とも 16,620 行、ok/error 内訳不変。
- 比較（compare-corpus.py）: 全4構成で identical=4,155、hard=0、map-core=0、
  expl-only=0、missing=0。status は ok 4,128 / error 27 で不変。
- raw TSV 指紋は両側とも `9713a05d…` でバイト同一、総コードバイト 26,620,904、
  差分 0。本サイクルの修正はいずれもコーパス内に現れない形状の誤りに対する
  もので、生成コードへの影響はゼロだった（link 修正は単一ファイルコンパイルの
  対象外であり、別途リンクテスト152件と v25 スイートで検証した）。
- ワークスペーステスト: 1,738 件 pass（第284ラウンド時点 1,728 + 本サイクル 10）。
- rustfmt は変更4ファイルで無指摘、clippy は変更ファイルで無指摘。

## 次サイクル候補として記録: S4（VM の math.random 非整数引数）

w6 第67サイクルで範囲確定済み。修正対象は math.random の引数1・2の検証のみ:
PUC が "number has no integer representation" で拒否するのに VM が受理する形7件
（切捨て受理4形、切捨てで空区間になり VM 独自文になる2形、範囲外 float で
mlua 変換エラーになる1形）。対象外: 整数値 float、文字列強制、randomseed の小数、
無引数形。根は sandbox.rs `install_random` の `Variadic<i64>` 変換。本サイクルでは
記録のみ（スコープ外）。
