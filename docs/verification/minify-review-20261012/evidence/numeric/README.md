# Numeric / algebraic minifier audit evidence — 2026-10-12

対象は `5a168d6`、v0.3.1 の `metadata-source-map-fix` 作業ツリー。レビュー担当は製品ソースを変更していない。初期に旧ルートを調べたが、この証跡はすべて v0.3.1 に依存先を切り替えて再実行した結果。対象ファイルの内容ハッシュは [source-manifest.json](source-manifest.json)。

## 実行方法と範囲

`harness.rs` は独立 Cargo consumer。公開 `storm_lua_minify::compile_code` の生成物を、未変換ソースと別々の `mlua 0.10.5 / vendored Lua 5.3` VM で実行して比較する。callback ケースの同じ setup / run は各 VM に適用する。入力 `only` があるケースだけ、指定以外の pass を無効化して原因を分離した。通常ケースは全 pass が既定のまま。

```bash
python3 docs/verification/minify-review-20261012/evidence/numeric/reproduce.py \
  --repo <v0.3.1-checkout> --output numeric-rerun.jsonl
```

Cargo は保護 wrapper を通し、build artifact は `/tmp/cargo-targets` 配下だけに作る。sandbox が `findmnt` に同じ tmpfs を二重表示する環境では wrapper が拒否したため、今回のビルドは sandbox 外の同じ wrapper で実行した。SDK / product のビルド先設定は変更していない。

- [focused-cases.jsonl](focused-cases.jsonl) / [focused-results.jsonl](focused-results.jsonl): 40 入力 × 4 モード = 160 比較。`default=smallest+tolerant`、`safe=safe+tolerant`、`exact=smallest+exact`、`safe_exact=safe+exact`。
- [matrix-summary.json](matrix-summary.json): 22 リテラル × 22 リテラル × 18 演算子 × 3 モード = 26,136 比較。26.4 秒。`numeric_matrix.py <built-harness>` で再生成できる。出力先は実行ディレクトリ。
- [matrix-failures.jsonl](matrix-failures.jsonl): 上記の生の差分。**許可された subtype 変更も含むため、バグ件数ではない。** 両方が runtime error の組は行番号差を失敗に数えていない。
- [option-results.jsonl](option-results.jsonl) / [option-summary.json](option-summary.json): 15 再現例 × 4 モード × fast 2 通り × target 3 通り × origin 2 通り = 720 比較。1.05 秒。`option_matrix.py <built-harness>` で再生成。origin は `CompileOptions.origin_source` の内部 provenance tracking で、build SDK の Source Map API 全体の検証ではない。
- [format-resource-result.json](format-resource-result.json): 256 MiB のアドレス空間制限と core dump 無効化を設定した子プロセスで、不必要な約 1 GiB allocation が SIGABRT を起こすことを確認。

通常の focused 結果の `same` は raw 値・subtype・エラーテキストの完全一致。エラー内の行番号が変わっただけの `same=false` を欠陥として扱っていない。`ApiAssumptions` は Smallest で `finiteNumbers` と `integerFloatSubtypeMayChange` を true にする（numericMode が Exact でも同じ）。したがって Smallest での subtype 単独差、NaN/Infinity 前提外の代数変形は主指摘から除いた。Safe ではこの 2 flag が false のため、Safe の subtype / -0 破壊は指摘対象。

## 確認した欠陥（修正箇所の単位）

以下の行番号は対象 v0.3.1 の `crates/storm-lua-minify/src/passes/` 配下。各例の完全な生成コード・pass 記録は focused-results の同名 id にある。

### N1 — 整数除算・剰余の literal 評価が panic する [P1]

`literal_folding.rs:258,266`。`return 0x8000000000000000//-1` は Lua では i64 MIN、`%-1` は 0。コンパイラは Rust の `div_euclid` / `rem_euclid` をそのまま使い、overflow panic を起こす。Default と Safe。Exact は再現しない。`int-div-panic`, `int-mod-panic`。

通常の整数 overflow は Lua 5.3 の wrap 規則であり、未定義のプログラムではない。Rust panic を捕捉しても誤評価そのものの修正にはならない。

### N2 — 除算・剰余の丸め規則が Lua と異なる [P1]

`literal_folding.rs:258-274`。Default / Safe で再現。

| id / source | 未変換 | 生成物の実行 |
|---|---:|---:|
| `negative-divisor`: `5//-2,5%-2` | `-3,-1` | `-2,1` |
| `float-mod-positive`: `1.0%1e20` | `1.0` | `0` |
| `float-mod-negative`: `1.0%-1e20` | `-1e20` | `0` |

整数の Euclidean division は負の除数に対する Lua floor division と一致しない。浮動小数点の `((x%y)+y)%y` は不要な加算・剰余で大きな丸め損失を起こす。許容誤差や subtype の差では説明できない値の破壊。

### N3 — bit 演算の literal 評価に JavaScript / Rust の規則が残る [P1]

`literal_folding.rs:176-187,295-305`。Default / Safe。

- `1<<64`: Lua は `0`、生成物は `1`。shift count を `&63` で循環させている。
- `4>>-1`: Lua は `8`、生成物は `0`。負の shift は逆方向の shift である。
- `~4294967296.0`: Lua は `-4294967297`、生成物は `-1`。float operand を JS int32 に落としている。
- `~1.5`: Lua の「整数表現を持たない」runtime error が `-2` に置換される。

### N4 — literal の型変換・比較が Lua の値を保持しない [P1]

`literal_folding.rs:147-226,230-310,1024-1044`。

- `9007199254740993>9007199254740992` は true → false、`9007199254740993==9007199254740992.0` は false → true。整数を f64 に落として比較している。Default / Safe。
- `true+1`、`nil-1`、`"10"<2`、`math.floor(true)` は Lua runtime error が通常値に変わる。Default / Safe。
- `math.min("2","10")` は有効な Lua で文字列 `"10"` を返すが、生成物は整数 `2`。`min` を数値変換している。Default / Safe。
- `1.0 .. "x"` は `"1.0x"` → `"1x"`。`literal_to_js_string` が Lua の文字列化を再現していない。Default / Safe。
- `math.abs(9007199254740993)` は Safe でも `9007199254740992` に変わる。

### N5 — 算術 identity の除去が必要な型変換を消す [P1]

`literal_folding.rs:800-830`。`local x="3" return x*1` は数値 `3.0` → 文字列 `"3"`。`local x=false return x+0` は runtime error → false。Default / Safe。operand の数値性を証明せず `+0`, `*1`, `/1` を除去している。

### N6 — Safe で subtype / -0 を保持できない [P2]

Safe の公開契約は `integerFloatSubtypeMayChange=false`, `finiteNumbers=false`。

- `literal_folding.rs:70-75`: `return math.type(1/1)` は `"float"` → `"integer"`。Num の folded result を整数 spelling にする。`return 1/-0.0` は `-Infinity` → `+Infinity`。
- `induction_values.rs:14-19,130-137`: `for i=1.0,1.0 do return math.type(math.floor(i)),math.type(i) end` は `integer,float` → `float,float`。整数値を持つ float と integer subtype を区別せず induction variable を扱う。**Safe+Exact でも再現。**
- `boolean_numerics.rs:75-83,195-210`: `local x=3 local bit=x&1>0 return math.type(bit and 1.0 or 0.0)` は float → integer。**Safe+Exact でも再現。** 数値 boolean consumer の literal token subtype を保持していない。

Smallest の同じ subtype 差は公開 assumptions が許容するため、ここでは Safe の不整合として扱う。

### N7 — quotient/remainder fusion が integer overflow を変える [P1]

`quotient_remainder.rs:124`。Default と Smallest+Exact。次のプログラムは約 `+9.223372036854776e17,2` を返すが、`x=x//10` への置換後は `-922337203685477581,2`。

```lua
local x=0x8000000000000000
local r=x%10
x=(x-r)/10
return x,r
```

元の `x-r` は i64 wrap 後に float division する。直接 floor-divide することとは数値として等価でない。`quotient-isolated` でも同 pass 単独で再現。

### N8 — signed expression factoring が代入後の古い値を使う [P1]

`signed_factoring.rs:308-325`。Default / Smallest+Exact。比較する statement の**間**だけ barrier を調べ、共通式を含む statement 自体の書き込みを無視する。

```lua
local x=1 local a,b,c
x=x*12345
a=x*12345 b=x*12345 c=x*12345
return x,a,b,c
```

期待 `12345,152399025,152399025,152399025` がすべて `12345` になる。`signed-expression-factoring` と `scope-renaming` だけでも再現した。

### N9 — split-sign の消去が外側から観測される書き込みを削除する [P1]

`split_sign.rs:108-130,206-215`。Default / Safe。直後の同じ block 内の読みだけを調べ、captured binding が外側から読まれることを考慮していない。

```lua
local p=99
local function f(x)
  p=math.max(x,0) x=math.min(x,0) x=p+x
  return x
end
local x=f(5)
return x,p
```

期待 `5,5` が `5,99` になる。pass 単独でも再現。

### N10 — split-sign の二乗変形が副作用・整数演算を変える [P1]

`split_sign.rs:297-321`。Default。

- `math.max(x,0)^2*f()-math.min(x,0)^2*f()` → `x*math.abs(x)*f()` により f の実行が 2 回 → 1 回。`split-effects`。
- x 部分が同じ `f()` AST でも、呼び出し結果が変わる場合がある。`split-changing-operand` は結果 `-3` → `2`。
- x が整数 `4000000000` の場合、元の `^2` は float の `1.6e19`、変形後の整数積は wrap した `-2446744073709551616`。`split-overflow`。

AST の同形性しか調べず、値の安定性・calls・型を確認していない。

### N11 — product/coefficient carrier の組み替えが整数 overflow を導入する [P1]

`multiplicative_carriers.rs:51-62,158-208,229-246`、`coefficient_carriers.rs:47-78,316-334,367-389`。input.getNumber(1) を実ゲームと同じ float `1.0` として実行。

```lua
tau=4000000000*4000000000
function onTick()
  local x=input.getNumber(1)
  output.setNumber(1,x*4000000000*4000000000)
  output.setNumber(2,tau)
end
```

第 1 出力は `+1.6e19` だが、`x*tau` にすると整数計算で overflow 済みの tau を使い `-2.4467440737095516e18` になる。Default **と Safe**。`product-carrier`。

`k=4000000000` と 2 個の `x*k*4000000000` から `coefficient=k*4000000000` を合成する場合も同じ破壊。こちらは Default。`coefficient-carrier`。浮動小数の微小な再結合誤差の範囲ではない。

### N12 — numeric approximation が exact route を保護しきれない [P1]

`numeric_literals.rs:196-207,226-240,289-304,458-484`。Default。

- table constructor の数値 key は exact と判断されない。`local t={[1.23456789012345]=7} ... t[property.getNumber("x")]` の key が `1.23456789` に変わり、元の key を返す property で `7` → nil。`approx-table-key`。同 pass 単独の `local t={[1.23456789012345]=7}return t[1.23456789012345]` でも key と index が食い違う。
- exact な呼び出しから到達する user-function は `exact_functions` に記録するだけで、rewrite context に渡していない。function return の literal が `string.pack` に渡されても近似される。`approx-function-return`。Safe / Exact はこれらを維持する。

### N13 — string.format folding が未採用候補へ巨大 allocation を行う [P1、性能にも影響]

`literal_folding.rs:639-655`。Lua 5.3 が拒否する長い width を usize として受け入れ、サイズ比較より前に `"0".repeat(zeros)` を実行する。

- `return string.format("%018446744073709551615d",1)` は Lua runtime error を保持せず、コンパイラで `capacity overflow` panic。Default / Safe。
- `return string.format("%01000000000d",1)` は制限した子プロセス内で 999,999,999 bytes の allocation を試行し SIGABRT。実測は [format-resource-result.json](format-resource-result.json)。メモリ上限解除やホスト全体の OOM は試していない。
- 同じ 30 字程度の入力で width を 1,000,000 → 10,000,000 にすると、Safe compile が約 130 ms → 1,269 ms。Exact は双方約 1.1 ms。**未最適化 Rust build の単発値**であり、本番の絶対時間や速度保証ではない。いずれも候補は長すぎて捨てられ、出力コードは同じ string.format 呼び出しのまま。

まず Lua が許可する format の範囲を検証し、生成文字列の長さだけで候補が改善しないと分かる場合は文字列を作らず棄却できる。

## オプション横断結果

15 の代表例では fast の有無と provenance の有無によって再現の成否は変わらなかった。targetSize 未指定と 0（達成不能）の結果も同じ。targetSize=10000 は変換前の短縮段階で止まり、今回の 15 バグを通らなかった。これは特定の早期終了経路で欠陥が隠れるという結果であり、最適化の正しさを保証しない。

26,136 ケースの算術 matrix では Exact は全 8,316 成功入力が値・subtype 一致、396 組は双方 runtime error。Default と Safe の差分件数は `matrix-summary.json` に記録。これは当該 literal matrix の結果であり、Exact 全体が安全という結論ではない（N7/N8、Safe+Exact の N6 は別の構造化入力で再現）。

## 独立した scope 指摘の反証確認

[scope-cross-summary.json](scope-cross-summary.json) に別担当 S1–S8 の 30 比較を記録した。この担当の別 runner でも input.getNumber を明示的に float `i*3.0` とし、S5 は onDraw で output API を呼ばない原文へ変更して再実行した。すべての報告モードの再現性が一致し、数値 subtype や callback phase に起因する偽陽性は確認しなかった。これは別担当の件数と重複する確認で、新規バグ件数に加算しない。

## 修正時に守るべき検証境界

- literal evaluator は Lua の integer / float / byte string を型ごとに評価し、Rust / JS の便利な変換を意味論の代わりに使わない。overflow、負の除数、負・64 以上の shift、エラーを回帰対象にする。
- algebraic rewrite は等価な数式だけでなく、operand の subtype、overflow、calls、capture、当該 statement 自体の書き込みを含めて適用可否を証明する。
- exact-route の dataflow は table constructor key と呼び先の return literal まで保護情報を rewrite へ届ける。
- Safe の float literal は結果の subtype と符号付きゼロを保持する。Smallest の既存 assumptions を Safe に暗黙拡大しない。
- string.format の候補サイズは allocation の前に上限判定し、不正 format を折り畳まない。

## 参照・未検証範囲

演算の基準は実行した vendored Lua 5.3。規則の確認には [Lua 5.3 arithmetic / bitwise operations](https://www.lua.org/manual/5.3/manual.html#3.4.1) と [Lua 5.3 string library source](https://www.lua.org/source/5.3/lstrlib.c.html) も使用した。実ゲーム適合性、WASM runtime 差、build SDK の全 Source Map 経路はこの数値担当の実行範囲外。

common_offsets と literal_pool / exact expression synthesis もコードを読み、代表例を試したが、この担当では追加の確定バグを見つけていない。網羅的正当性証明ではない。Lua byte string の破壊と hex literal parser / renderer の問題は別担当へ引き継いだため、ここでは重複計上していない。
