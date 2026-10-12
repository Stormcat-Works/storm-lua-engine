# 構文・リテラル・出力処理の追加レビュー

対象は v0.3.1 (`5a168d6`) に metadata/source-map 修正を加えたチェックアウト。以下のパス・行番号はこの対象のもの。独立 consumer から公開 minify API を呼び、生成物と原文を別の Lua 5.3 VM で実行した。数値は i64 と f64 のビット列、文字列は実バイト列で比較した。

## R1 — [P1] quoted string のデコードがバイト列と Lua escape を破壊する

対象: `crates/storm-lua-syntax/src/numeric.rs:505-511,555-585`。

```lua
return "\255", "\xFF", "\u{1F600}"
```

原文の最初の 2 値は 1 byte の `FF`、最後は UTF-8 の `F0 9F 98 80`。生成物は次のようになる。

```lua
return "ÿ","ÿ","\\u{1F600}"
```

最初の 2 値は `C3 BF`、最後は escape の字面そのものの `5C 75 7B 31 46 36 30 30 7D` になる。`char::from_u32` で byte を Unicode scalar として保存すること、Lua 5.3 の `\u{...}` に未対応のまま失敗を「両端の quote を除く」処理へ落とすことが原因。`\z` も Unicode の `is_whitespace` を使い、Lua が残す全角空白まで除去する。

既定、Safe、Exact、Safe+Exact と sourceMap ON/OFF で再現。`function onTick()output.setNumber(1,#"\255")end` は実 WASM Vehicle でも **1 → 2**。Unicode escape の長さは **4 → 9**。256 byte の decimal/hex escape を列挙し、128–255 の両表記で同じ不具合を確認した。extended の lexical 経路は今回の quoted-string 値を維持した。

修正方向: Lua string の内部値を byte 列として扱い、escape decoder と emitter を Lua 5.3 の規則で共有する。未対応・不正 escape を字面の別文字列へ変える fallback は使用しない。byte array 全域、Unicode escape、ASCII のみを飛ばす `\z` を回帰条件にする。

証跡: `literal-probes.jsonl`, `syntax-corpus-failures.jsonl`, `root-option-matrix.jsonl`, `wasm-game-runtime.jsonl`。Lua の基準は [5.3 lexical conventions](https://www.lua.org/manual/5.3/manual.html#3.1)。

## R2 — [P1] long string の改行正規化が Lua と異なる

対象: `crates/storm-lua-syntax/src/numeric.rs:497-503,548-553`。

下記の `<CR><LF>` は説明用表記で、入力には実際の改行 byte を入れる。

```text
return [[a<CR><LF>b]]
```

Lua は `61 0A 62` を返す。minify は `return"a\r\nb"` を出力して `61 0D 0A 62` に変える。CR-only も `0D` が残る。opening delimiter の直後の LFCR は 1 つの改行として全体を除くべきなのに、LF だけを除いて CR を残す。quoted string の escaped LFCR も同様に extra CR を残す。

既定 / Safe+Exact / map ON/OFF で再現。実 WASM の `output.setNumber(1,#[[a<CR><LF>b]])` は **3 → 4**。Windows 系改行のスクリプト中にある long string の内容・サイズ・キー・描画文字列が変わり得る。

修正方向: long/escaped newline に対して LF、CR、CRLF、LFCR の Lua 正規化を適用し、その後に byte 列を再エンコードする。source text の改行保持と string value の改行正規化を混同しない。

証跡: `syntax-corpus-failures.jsonl` の `long-string-newline`、`literal-probes.jsonl` の `short_lfcr`、`wasm-game-runtime.jsonl`。

## R3 — [P1] CR-only の行コメントが後続コードを飲み込む

対象: `crates/storm-lua-syntax/src/lexer.rs:299-301`。位置計数の `233-245` も LF のみを改行としている。

```text
-- comment<CR>return 42
```

Lua の戻り値は 42。既定の minify 出力は空、extended の lexical 出力は CR のみになり、戻り値がなくなる。コメントの終わりを LF しか認めないため、その後のコードがコメントとして除去される。これは AST 最適化を無効にする lexical 経路にも残る。

実 WASM Vehicle の `-- comment<CR>function onTick()output.setNumber(1,42)end` でも、原文の callback が短縮後に消える。Safe+Exact、map ON/OFF で再現。

修正方向: Lua が認める全改行形式を共通の lexer/位置計数処理へ実装する。改行 pair を 2 行に数えず、トークン位置と Source Map 側の位置表現の整合も確認する。

証跡: `root-option-matrix.jsonl`, `wasm-probes.jsonl`, `wasm-game-runtime.jsonl`。

## R4 — [P1] hexadecimal number の読取りが Lua と一致せず、Exact でも値が変わる

対象: `crates/storm-lua-syntax/src/numeric.rs:48,182-184,191-192,213` およびその値を使う `print::render_num` / `numeric_literals`。

| 入力 | Lua 5.3 | Safe+Exact の生成物 |
| --- | --- | --- |
| `return 0x1.` | float 1.0 | `0/0`、NaN |
| `return 0x10000000000000000` | integer 0（64-bit wrap） | `0x1p64`、約 1.8447e19 |
| `return 0x.8p1024` | 有限、bits `7fe0000000000000` | `1/0`、Infinity |
| `return 0x2p-1075` | 最小 subnormal、bits `0000000000000001` | `0.0` |
| `return 0x1.0000000000000800001p0` | bits `3ff0000000000001` | `1.0`、bits `3ff0000000000000` |

Lua が認める末尾 dot を NaN にする、長い hex integer を `u64::from_str_radix` の overflow で float と誤分類する、mantissa を逐次 f64 演算で丸める、`2^exp` を先に作って overflow/underflow させる、という複数の Lua 互換性欠落が同じ読取り層にある。

近似が禁止された Safe+Exact でも全例を確認し、sourceMap ON でも誤った値の code が成功する。hex integer wrap のケースは実 WASM Vehicle の数値出力でも **0 → 約 1.8447e19**。

修正方向: Lua の整数分類・64-bit wrap と正しく丸めた hex-float parsing を実装する。少なくとも正確に評価できない原綴りは正規化対象にせず保持し、f64 での自己 roundtrip だけを同値判定にしない。

証跡: `root-option-matrix.jsonl`、固定 seed 3,006 入力の `hex-corpus-summary.json` / `hex-corpus-failures.jsonl`。この corpus は境界を偏らせた試験であり、842 件の差分を独立バグ数や一般の発生率には使わない。

## R5 — [P2] number token を式へ置き換える printer が優先順位を保持しない

対象: `crates/storm-lua-syntax/src/print.rs:154-161,447`。

```lua
return 2/1e999
```

`{mode:"safe",numericMode:"exact"}` で `return 2/1/0` になり、**0.0 → +Infinity**。無限大の literal を `1/0` という binary expression にしているのに、Node::Num の出力は周囲の precedence を無視してそのまま返す。

この指摘は非有限数を許さない Smallest の前提には依存しない。Safe が返す assumptions は `finiteNumbers:false` / `integerFloatSubtypeMayChange:false`。map の有無でも再現。一般に literal renderer から複合式を返す設計と、AST printer の優先順位処理の境界が崩れている。

修正方向: 数値 token の元綴りを保持するか、代替式を AST として表現して適切に括弧を付ける。`*`, `/`, `//`, `%`, unary, power, concat の両 operand で検証する。

証跡: `root-option-matrix.jsonl` の `return 2/1e999`、`wasm-probes.jsonl`。

## R6 — [P2] minify の再帰解析に深さ制限がなく、Native abort / WASM 例外になる

対象: `crates/storm-lua-syntax/src/parser.rs:561-603` と再帰する prefix/body 処理。`source_tools.rs:361-378` の inspection 用制限は minify に適用されない。

```python
source = "return " + "(" * depth + "1" + ")" * depth
```

`targetSize:8192` の公開 minify 呼び出しで検証。Native の未最適化 consumer は depth=3,200（6,408 bytes）で **SIGABRT / stack overflow**、保存された release WASM は depth=6,400（12,808 bytes）で **RangeError: Maximum call stack size exceeded**。core dump は無効にし、子プロセスで試験した。

同じ release WASM の `inspectSource` は depth=200 から `source nesting exceeds inspection limit` を通常の結果として返すが、minify は depth=3,200 まで `ok:true` を返した。この深さは Lua 自身の構文深さ上限を超えるため、正しい Lua の意味破壊としては数えない。編集中・生成途中の入力を通常の診断として拒否できず、呼出し元を巻き込む堅牢性の不具合である。閾値は build/stack 環境に依存する。

修正方向: 解析入口にトークン数・深さ等の明示的な予算を置き、再帰へ入る前に diagnostic へ変換する。生成 AST の再帰処理にも同じ制約を適用し、compile / analyze / inspect の対象範囲を明確にする。

証跡: `depth-probes.json`, `depth-wasm.json`。単なるテスト実行不能ではなく、実際の process exit / WASM exception を記録した。

## 検証範囲と再実行

- lexical / printer corpus: **14,440 実行**、うち元の Lua 実行が成功したもの **8,802**、値差分 **1,521**。byte escapes・改行・hex literal を含み、12,000 個の固定 seed 式も生成した。ランダム式の未説明差分はなく、差分は上記 literal 読取り / renderer の問題に集約された。元がエラーだった入力は成功の件数に含めていない。
- hex boundary corpus: **3,006 実行、842 差分**。原因群は R4/R5。
- 500 入力 × 6 設定 = **3,000 compile 実行**では、1,500 組の sourceMap ON/OFF の生成 code・成否がすべて一致。これは map 追加がコードを変えないことの検査であり、実行結果の同値保証ではない。
- 実 WASM compiler の 30 probe（2 件は今回の metadata 修正条件、1 件は整数除算 panic、1 件は panic 後の通常呼出し）。metadata の通常/LifeBoat ビルドはどちらも `ok:true` かつ map validator 成功。整数除算 panic は `RuntimeError: unreachable` で再現した。
- 実 WASM Vehicle で root / scope の **48 比較**と API / 描画の **24 比較**。元の失敗は 0、生成物との違いはそれぞれ **44 / 24**。後者は実 raster の画素 hash も比較した。実ゲーム本体を起動した検査ではない。

```bash
python3 docs/verification/minify-review-20261012/evidence/root/run.py \
  --repo <audited-checkout> --output root-replay.jsonl
node docs/verification/minify-review-20261012/evidence/root/game-runtime.mjs \
  <audited-checkout> wasm-game-runtime-rerun.jsonl
node docs/verification/minify-review-20261012/evidence/root/game-api.mjs \
  <audited-checkout> \
  docs/verification/minify-review-20261012/evidence/api/cases.jsonl \
  wasm-game-api-rerun.jsonl
```

`run.py` は consumer を `/tmp` に再生成し、Cargo cache だけを `/tmp/cargo-targets` に置く。上記の結果・入力・ソースは本ディレクトリに永続保存する。JS 検証は指定 checkout のビルド済み SDK assets を使用する。今回使用した 372 source entry と WASM の保存 manifest/hash は照合済みで、結果は `existing-wasm-verification.json` にある。
