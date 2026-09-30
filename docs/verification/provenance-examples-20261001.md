# Storm Lua Engine v0.3.0 — 由来位置の実測例・理由付きSource Map（更新版）

2026-10-01。対象commit `c9855a3937518f7b2f25b2dd0bd929e5e8cf8c41`。元の`storm-lua-engine-provenance-examples-20261001.md`が記録した158231c時点の限界に対し、今回は実際の公開Compiler APIで再コンパイルして確認した更新版である。古い測定を新しい結果として扱わない。v0.3.0は未公開。

## 変わった点

保持された演算子/keywordをToken単位で辿れる。原文identityはcopiedの等バイト契約を持つ。インライン化したleafは元定義の位置を維持したまま呼び出し文脈を取得できる。typed relationと、最後の変換名とは別のreason/factsを持つ。削除された宣言/分岐は明示記録され、出力が空でも失われない。code/source/mapの組み違いはvalidatorで拒否する。

以下の表示位置は1-basedの行/UTF-16列、endは除外する。データのstart/endはUTF-8バイトの半開範囲。表に示した生成区間は実際に選ばれた最も具体的なmappingであり、needleの長さと同一とは限らない。親式の説明はconstructs、展開文脈はinlineContextsから取得する。

## 変数名・API別名化

原文：

| 行 | コード |
| --- | --- |
| 1 | `function onTick()` |
| 2 | `  local throttle = input.getNumber(1)` |
| 3 | `  output.setNumber(1, throttle)` |
| 4 | `  output.setNumber(2, throttle * 2)` |
| 5 | `end` |

生成：`a=output.setNumber function onTick()b=input.getNumber(1)a(1,b)a(2,b*2)end`

**生成1行68列〜1行69列（終端除外）**：`*`。分類はsource / token。

主な元範囲：rename.lua 4行32列〜4行33列、`*`。

正確なcopy区間：あり。選択区間と元bytesが一致する。

この例のmapサイズ：15,280 bytes。Unknown：0 bytes。

## 定数式の計算根拠

原文：

| 行 | コード |
| --- | --- |
| 1 | `function onTick()` |
| 2 | `  output.setNumber(1, 2 * 3)` |
| 3 | `end` |

生成：`function onTick()output.setNumber(1,6)end`

**生成1行37列〜1行38列（終端除外）**：`6`。分類はderived / expression。

主な元範囲：fold.lua 2行23列〜2行28列、`2 * 3`。

正確なcopy区間：なし。式・構文への由来であり、文字単位のコピーとは言わない。

判断：`constant-folding`、根拠コード`literal-operands-evaluated-and-size-nonincreasing`。

| 実際に記録した事項 | 値 |
| --- | --- |
| operator | `*` |
| left | `i64:2` |
| right | `i64:3` |
| evaluated | `i64:6` |
| emitted | `i64:6` |
| expressionSizeBefore | `3` |
| expressionSizeAfter | `1` |
| aggressive | `true` |
| absToleranceBits | `3d719799812dea11` |
| relToleranceBits | `3d719799812dea11` |

この例のmapサイズ：8,458 bytes。Unknown：0 bytes。

## 数値表記変更

原文：

| 行 | コード |
| --- | --- |
| 1 | `function onTick()` |
| 2 | `  output.setNumber(1, 0x10)` |
| 3 | `end` |

生成：`function onTick()output.setNumber(1,16)end`

**生成1行37列〜1行39列（終端除外）**：`16`。分類はderived / expression。

主な元範囲：normalized.lua 2行23列〜2行27列、`0x10`。

正確なcopy区間：なし。式・構文への由来であり、文字単位のコピーとは言わない。

判断：`numeric-literal-spelling`、根拠コード`canonical-literal-emission`。

この例のmapサイズ：7,940 bytes。Unknown：0 bytes。

## インライン展開したリテラル

原文：

| 行 | コード |
| --- | --- |
| 1 | `local function twice(value)` |
| 2 | `  return value * 2` |
| 3 | `end` |
| 4 | `function onTick()` |
| 5 | `  output.setNumber(1, twice(input.getNumber(1)))` |
| 6 | `end` |

生成：`function onTick()output.setNumber(1,input.getNumber(1)*2)end`

**生成1行56列〜1行57列（終端除外）**：`2`。分類はsource / expression。

主な元範囲：inline.lua 2行18列〜2行19列、`2`。

正確なcopy区間：あり。選択区間と元bytesが一致する。

展開文脈：定義`value * 2`、呼び出し`twice(input.getNumber(1))`。

この例のmapサイズ：11,373 bytes。Unknown：0 bytes。

## 共通定数引数の特殊化

原文：

| 行 | コード |
| --- | --- |
| 1 | `local function add(value, offset)` |
| 2 | `  return value + offset` |
| 3 | `end` |
| 4 | `a = add(1, 200)` |
| 5 | `b = add(3, 200)` |

生成：`local function add(value)return value+200 end a=add(1)b=add(3)`

この例だけは全パスを無効化しconstant-argument-specializationだけを有効にした観察例。他の例は指定した数値モード等を使う通常minify。

**生成1行39列〜1行42列（終端除外）**：`200`。分類はderived / expression。

主な元範囲：shared.lua 4行12列〜4行15列、`200`。

正確なcopy区間：あり。選択区間と元bytesが一致する。

関連元（contribution）：`200`。

関連元（parameterUse）：`offset`。

関連元（argument）：`200`。

関連元（argument）：`200`。

判断：`constant-argument-specialization`、根拠コード`same-literal-at-every-analyzed-call-and-unmodified-parameter`。

| 実際に記録した事項 | 値 |
| --- | --- |
| parameterIndex | `1` |
| analyzedCallSites | `2` |
| value | `number:200` |
| removedParameter | `true` |

この例のmapサイズ：14,442 bytes。Unknown：0 bytes。

## 八つの出力を一つのループへ

原文：

| 行 | コード |
| --- | --- |
| 1 | `function onTick()` |
| 2 | `  output.setNumber(1, 10)` |
| 3 | `  output.setNumber(2, 20)` |
| 4 | `  output.setNumber(3, 30)` |
| 5 | `  output.setNumber(4, 40)` |
| 6 | `  output.setNumber(5, 50)` |
| 7 | `  output.setNumber(6, 60)` |
| 8 | `  output.setNumber(7, 70)` |
| 9 | `  output.setNumber(8, 80)` |
| 10 | `end` |

生成：`function onTick()a={10,20,30,40,50,60,70,80}for b=1,8 do output.setNumber(b,a[b])end end`

**生成1行21列〜1行23列（終端除外）**：`10`。分類はsource / expression。

主な元範囲：loop.lua 2行23列〜2行25列、`10`。

正確なcopy区間：あり。選択区間と元bytesが一致する。

**生成1行45列〜1行51列（終端除外）**：`for b=`。分類はsynthetic / group。

主な元位置はない。明示的な自動生成を元の近い行で埋めない。

正確なcopy区間：なし。式・構文への由来であり、文字単位のコピーとは言わない。

この例のmapサイズ：16,175 bytes。Unknown：0 bytes。

## 削除された宣言と分岐

原文：

| 行 | コード |
| --- | --- |
| 1 | `function onTick()` |
| 2 | `  local unused = 99` |
| 3 | `  if false then` |
| 4 | `    output.setNumber(2, 123)` |
| 5 | `  end` |
| 6 | `  output.setNumber(1, 7)` |
| 7 | `end` |

生成：`function onTick()output.setNumber(1,7)end`

**生成1行37列〜1行38列（終端除外）**：`7`。分類はsource / expression。

主な元範囲：dead.lua 6行23列〜6行24列、`7`。

正確なcopy区間：あり。選択区間と元bytesが一致する。

除去・置換記録（抜粋）：

- 元`if false then ⏎     output.setNumber(2, 123) ⏎   end`：remove / `constant-control-flow`、basis=`lua-truthiness-of-known-conditions`。
- 元`local unused = 99`：remove / `dead-local-elimination`、basis=`all-declared-bindings-unread-unwritten-and-initializers-movable`。

生成側に実行箇所を捏造していない。元snapshotに原文を残すことと、元の処理が実行可能なことは別。

この例のmapサイズ：8,774 bytes。Unknown：0 bytes。

## 原文早期返却のcopy契約

原文：

| 行 | コード |
| --- | --- |
| 1 | `function onTick()` |
| 2 | `  local throttle = input.getNumber(1)` |
| 3 | `  output.setNumber(1, throttle)` |
| 4 | `  output.setNumber(2, throttle * 2)` |
| 5 | `end` |

生成：`function onTick() ⏎   local throttle = input.getNumber(1) ⏎   output.setNumber(1, throttle) ⏎   output.setNumber(2, throttle * 2) ⏎ end ⏎ `

targetSize=8192を満たすため原文をそのまま返す。改行も原文と同一で、上の⏎は表示上の置換のみ。

**生成1行1列〜6行1列（終端除外）**：`function onTick() ⏎   local throttle = input.getNumber(1) ⏎   output.setNumber(1, throttle) ⏎   output.setNumber(2, throttle * 2) ⏎ end ⏎ `。分類はsource / group。

主な元範囲：identity.lua 1行1列〜6行1列、`function onTick() ⏎   local throttle = input.getNumber(1) ⏎   output.setNumber(1, throttle) ⏎   output.setNumber(2, throttle * 2) ⏎ end ⏎ `。

正確なcopy区間：あり。選択区間と元bytesが一致する。

この例のmapサイズ：4,240 bytes。Unknown：0 bytes。

## 演算子とendの正確な位置

原文：

| 行 | コード |
| --- | --- |
| 1 | `function onTick()` |
| 2 | `  output.setNumber(1, input.getNumber(1) + 2)` |
| 3 | `end` |

生成：`function onTick()output.setNumber(1,input.getNumber(1)+2)end`

**生成1行55列〜1行56列（終端除外）**：`+`。分類はsource / token。

主な元範囲：operators.lua 2行42列〜2行43列、`+`。

正確なcopy区間：あり。選択区間と元bytesが一致する。

**生成1行58列〜1行61列（終端除外）**：`end`。分類はsource / token。

主な元範囲：operators.lua 3行1列〜3行4列、`end`。

正確なcopy区間：あり。選択区間と元bytesが一致する。

この例のmapサイズ：10,075 bytes。Unknown：0 bytes。

## 全コードが削除された場合

原文：

| 行 | コード |
| --- | --- |
| 1 | `local unused=99 if not true then output.setNumber(1,7)end` |

生成：**空文字列**

除去・置換記録（抜粋）：

- 元`if not true then output.setNumber(1,7)end`：remove / `constant-control-flow`、basis=`lua-truthiness-of-known-conditions`。
- 元`local unused=99`：remove / `dead-local-elimination`、basis=`all-declared-bindings-unread-unwritten-and-initializers-movable`。

生成側に実行箇所を捏造していない。元snapshotに原文を残すことと、元の処理が実行可能なことは別。

この例のmapサイズ：4,446 bytes。Unknown：0 bytes。

## Unicode/CRLFでのidentity

原文：

| 行 | コード |
| --- | --- |
| 1 | `-- 😀雪` |
| 2 | `function onTick()` |
| 3 | ` local message='あ😀';output.setNumber(1,7)` |
| 4 | `end` |

生成：`-- 😀雪 ⏎ function onTick() ⏎  local message='あ😀';output.setNumber(1,7) ⏎ end ⏎ `

targetSize=8192を満たすため原文をそのまま返す。改行も原文と同一で、上の⏎は表示上の置換のみ。

**生成1行1列〜5行1列（終端除外）**：`-- 😀雪 ⏎ function onTick() ⏎  local message='あ😀';output.setNumber(1,7) ⏎ end ⏎ `。分類はsource / group。

主な元範囲：unicode.lua 1行1列〜5行1列、`-- 😀雪 ⏎ function onTick() ⏎  local message='あ😀';output.setNumber(1,7) ⏎ end ⏎ `。

正確なcopy区間：あり。選択区間と元bytesが一致する。

この例のmapサイズ：4,116 bytes。Unknown：0 bytes。

## 現在もできないこと

原文の変数名が分かることと、停止時の元の値を復元することは別。元binding ID・scope・storage/lifetimeの対応、bytecode PC、loop反復ごとの元命令、time-travelは未実装。原文の削除部分へbreakpointを置けるとは扱わない。

関連元の役割とinlineContextsは、実際のLua stackを復活させる機能ではない。標準v3部分だけを読む一般的なリーダーには複数寄与元や最適化の説明は表示されない。P5のUIはこの詳細APIを利用して別途作る。

reasonには観測された構造的操作だけのものもある。basis=nullを正当化が証明済みと表示せず、factsを記録していない条件を推測で埋めない。全探索で捨てた候補の全履歴も通常mapに含めない。

## APIと仕様

開発版Compilerで`compiler.minify(source, {sourceMap:true, sourceName:'controller.lua'})`を呼び、成功とcode/mapの存在を確認してから`compiler.validateSourceMap(code,map)`を使う。空のcodeも正常な生成物なので、文字列のtruthinessではなくundefinedとの比較で判定する。

[確定設計](../design/optimization-explanation-schema.md)、[拡張スキーマ](../specs/optimization-map-extension.md)、[全体の検証・性能](optimization-explanations-20261001.md)、[全例の実測JSON](provenance-examples-20261001.json)に詳細を保存した。
