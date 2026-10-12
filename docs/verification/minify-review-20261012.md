# Lua Engine minify 詳細レビュー — 2026-10-12

**不具合・堅牢性上の問題を 35 項目、性能上の問題を 3 項目に整理した。** 性能 3 項目のうち巨大 format allocation は不具合 N13 と重複する。件数は修正課題のグループ数であり、同じ原因で発生する多数の差分を別々のバグとして数えていない。

既存の **Native 875 件 / WASM 91 件はすべて成功**したが、追加検証では値の破壊、スコープの混同、描画・副作用順序の変化、正しい Lua のコンパイル拒否、コンパイラ panic を確認した。`safe` と `numericMode:"exact"` を同時指定しても残る問題がある。今回の結果から、その設定を包括的な回避策とは扱えない。

今日修正された **metadataTag + minify:false + sourceMap:true の問題は、修正済み作業ツリーで再現しなかった**。通常 build / LifeBoat build とも `ok:true`、詳細 map の公開 validator が成功し、既存の metadata 専用 Native/WASM 回帰も通った。今回の追加指摘とは分けている。

## 対象・保存範囲

- 基点: 公開 v0.3.1、commit `5a168d63eda82c898af4dcc72f8c2c3f9a178e51`。
- 対象 branch: `codex/fix-metadata-source-map`、リポジトリ内 `.worktrees/metadata-source-map-fix`。別セッションの未コミット修正を含む。公開済み npm/WASM そのものに修正済みだと主張するものではない。
- 開始時のルートは旧 `feat/storm-code-integration` (`7834de8`) だった。ユーザーの説明と worktree 一覧から対象を切り替え、採用した指摘は v0.3.1 側で再現し直した。
- 製品コード、既存の未コミット変更、branch / commit / 公開状態は変更していない。追加したのは本レビューと再現・検証証跡。
- 以下の `crates/...` の行番号はすべて**上記の対象 checkout**を指す。ルートの旧ブランチの行番号ではない。
- 証跡はこの文書と同じ永続ファイルシステム上の [evidence](minify-review-20261012/evidence/README.md) に保存。Cargo cache のみ tmpfs を使用。保存したファイルのサイズ・SHA-256、対象 source manifest と使用 WASM の hash を確認した。

対象ソース 372 entry は既存の修正検証 snapshot と全件一致した。使用 compiler WASM は 2,732,292 bytes、SHA-256 `da412ddd757aa54e4c2b1ac83e1819ee99a9806db2d6708d7f2e99ae48f33f25`。照合結果は [existing-wasm-verification.json](minify-review-20261012/evidence/root/existing-wasm-verification.json)。今回再ビルドした Native consumer と、この照合済み実 WASM を組み合わせて検証した。

## 検証結果の読み方

Lua の意味論の基準は vendored Lua 5.3 / mlua 0.10.5。原文と生成物を別 VM で実行し、integer/float、f64 bit、文字列 byte、引数個数、入出力・描画呼び出し順を比較した。代表例は本物の Engine WASM Vehicle と raster でも追試した。Stormworks 実ゲーム本体は起動しておらず、ゲームへの適合性を今回新たに証明したものではない。

`Smallest` が返す `finiteNumbers` / `integerFloatSubtypeMayChange` の assumptions を確認し、その契約で許された subtype 単独差や非有限数だけを用いた違いは一般のバグ数へ算入しなかった。Safe の契約違反、有限値の符号・桁の破壊、文字列・参照・副作用の破壊とは区別している。数値担当の Safe は Safe+Tolerant、scope 担当の Safe は Safe+Exact。各再現の JSON に実際の設定を残している。

P1 は正当な入力の意味破壊・コンパイル停止等を優先修正する指摘、P2 は限定的な契約違反・入力拒否・堅牢性・性能の指摘。再現件数を製品全体での発生率とは解釈しない。

| 検証 | 実行数と結果 |
| --- | --- |
| `cargo test --workspace --all-features --locked` | 875 passed、0 failed、0 ignored |
| `node --test packages/lua-engine/tests/wasm/*.test.mjs` | 91 passed、0 failed、0 skipped |
| literal / printer / 生成式 corpus | 14,440 実行。元の実行成功 8,802、差分 1,521。既知原因群に集約 |
| hex 境界 corpus | 3,006 実行、差分 842。境界へ偏らせた入力 |
| 算術 matrix | 26,136 実行。Exact の 8,316 正常入力は一致、396 は双方 runtime error。Default/Safe には確認済みの差分・panic |
| 数値重点 / 設定 matrix | 160 / 720 実行。許された subtype 差は raw 不一致から除外して評価 |
| scope / table の構造化 corpus | 480 実行。原文失敗 0。395 一致、70 差分、15 コンパイル拒否 |
| closed-field / method / alias の追加 corpus | 60 実行。57 一致、3 拒否（S9） |
| 描画パターン corpus | 627 実行。501 一致、126 差分（R1 が 120、F5 が 6） |
| Source Map ON/OFF / 設定 | 3,000 + 210 compile 実行。1,605 組で code / 成否の差 0。後者の成功 98 組は公開 map validator も成功 |
| 実 WASM Vehicle / raster | 72 比較。原文失敗 0、68 差分・拒否。差分のない 4 比較は Exact による算術変換の抑止 |
| 独立追試・原因分離 | pass ON/OFF、別担当 consumer、保存済み portable runner で再現確認 |

追加検証は約 4.9 万のケース・設定実行を含む。同じ入力の設定違い、既知バグの原因分離を含み、すべてを独立した Lua プログラム数や成功テスト数とは数えていない。`ok:true`、再パース成功、`semanticRejected:0`、Source Map validator 成功は実行結果の同値保証ではなかった。

## 指摘一覧

| ID | 優先度 | 問題 | 代表的な影響 |
| --- | --- | --- | --- |
| [R1](#r1) | P1 | quoted string の byte/escape デコード | `#"\255"` が 1 → 2 |
| [R2](#r2) | P1 | long string の改行正規化 | CRLF が余分な byte になる |
| [R3](#r3) | P1 | CR-only 行コメント | 後続 callback が消える |
| [R4](#r4) | P1 | hex 数値の Lua 非互換 parsing | Exact でも 0 → 2^64、有限値 → Infinity |
| [R5](#r5) | P2 | 数値 literal を式へ変える printer | Safe+Exact で 0 → Infinity |
| [R6](#r6) | P2 | minify 再帰深さの無制限 | Native abort / WASM stack exception |
| [N1](#n1) | P1 | i64 MIN の除算・剰余 folding | compiler panic / WASM trap |
| [N2](#n2) | P1 | floor division / modulo の評価 | 負の除数、float modulo の誤値 |
| [N3](#n3) | P1 | bit 演算の shift / int32 規則 | `1<<64` が 0 → 1 |
| [N4](#n4) | P1 | literal の coercion・比較 | i64 比較、文字列比較、型・error の変化 |
| [N5](#n5) | P1 | 算術 identity の除去 | 数値化される文字列を文字列のまま返す |
| [N6](#n6) | P2 | Safe での subtype / -0 破壊 | math.type と無限大の符号が変化 |
| [N7](#n7) | P1 | quotient/remainder fusion | 正の商 → 負の商 |
| [N8](#n8) | P1 | signed factoring の更新境界 | 代入前の古い値を再利用 |
| [N9](#n9) | P1 | split-sign の store 消去 | closure 外から見える更新が消える |
| [N10](#n10) | P1 | split-sign の二乗変形 | 呼び出し回数・順序、整数 overflow が変化 |
| [N11](#n11) | P1 | product/coefficient 再結合 | 有限 float の積に i64 overflow を導入 |
| [N12](#n12) | P1 | exact 経路の近似保護不足 | table key 不一致、string.pack の内容変化 |
| [N13](#n13) | P1 | string.format の巨大候補生成 | 不要な巨大 allocation / abort |
| [S1](#s1) | P1 | table parameter scalarization | closure の capture 消失、nil 参照 |
| [S2](#s2) | P1 | table member 関数定義の扱い | `function nil()` を作って compile 拒否 |
| [S3](#s3) | P1 | immutable value reuse の挿入先 | 同名の別 local を捕捉 |
| [S4](#s4) | P1 | carrier synthesis の式 identity | 別 BindingId の同じ字面を統合 |
| [S5](#s5) | P1 | root local の global 化 | 元の global と衝突、private callback を公開 |
| [S6](#s6) | P1 | goto を無視した dataflow | 実行されない代入値を出力 |
| [S7](#s7) | P1 | 関数 alias の escape 無視 | 間接呼び出しの引数・戻り値が失われる |
| [S8](#s8) | P1 | table key の f64 化 | 隣接 i64 key を混同 |
| [S9](#s9) | P1 | table の prefix expression 出力 | `{{}}[1]` の構文不正を生成 |
| [F1](#f1) | P1 | 合成名の freshness 不足 | 利用者変数と API/helper/loop 変数が衝突 |
| [F2](#f2) | P1 | button 抽出の eager 評価 | 最終値 321 → 21、不要な式を実行 |
| [F3](#f3) | P1 | button helper の root 移動 | local capture を同名 global に変更 |
| [F4](#f4) | P1 | callback 専用判定の到達漏れ | 初期化時の helper 呼び出しが nil |
| [F5](#f5) | P1 | screen loop の i64 等差判定 | 描画する整数値が書き換わる |
| [F6](#f6) | P1 | builtin member mutation の無視 | API 差替え後も古い関数/値を利用 |
| [F7](#f7) | P2 | 戻り値を捨てる property の固定化 | 数値だけの不正な文を生成 |

## 修正の優先順と追加ゲート

1. **共通の Lua 値・構文の基盤を修正する。** R1–R5、N1–N6、S8、S9。byte string、i64、float、優先順位は多くの pass が共有し、後段の工夫だけでは回避できない。コンパイラ panic / 過大 allocation（N1/N13/R6）も通常の失敗制御へ収める。
2. **binding・到達・副作用の証明を揃える。** S1–S7、F1–F4/F6、N8–N10。SymbolId の文字列一致だけで binding の同一性を判断しない。goto、closure、escape、root 初期化、member mutation を具体的な回帰として追加する。
3. **型と精度に依存する変換を個別に再検証する。** N7/N11/N12/F5。許容する近似と、キー・byte 表現・整数 overflow を変える操作を分離する。
4. **public API の full pipeline 回帰を残す。** 単独 pass だけでなく Default / Safe / Exact / fast / targetSize / sourceMap の交差を含める。今回の原文・生成物を Lua VM と Engine Vehicle で実行する試験を、修正の受入れ条件にする。

個別 pass OFF で復旧したものは各詳細に記載した。ただし S6 のように複数 pass が同じ入力を壊すものがあり、1 個の無効化で全 pipeline の正しさを保証できない。targetSize の早期終了は短い再現を隠すため、「8192 未満の例が通る」だけでは最適化の回帰を検出できない。

## 性能の実測

| 問題 | 条件・測定 | 結果・改善方向 |
| --- | --- | --- |
| 欠損 table key の COW 複製 | release、parse+単独 pass+print、5 回中央値 | 400/800/1600 参照で 148/589/2398 ms。欠損値を得るためだけに全 AST を copy。読み取り専用 lookup と Nil 生成を分離 |
| color group の全 subset 探索 | release、各色 8 回、warm-up 後 6 回中央値 | 12 group 95.1 ms、13 group 2.78 ms。全 subset の clone/resolve/size 計算を共有・削減。探索品質を落とす場合は別契約として検証 |
| format folding の不要な巨大生成 | 非最適化 dev、単発の補助測定 | width 100 万 → 1000 万で約 130 → 1269 ms。最終的には候補棄却。長さと Lua format validity を生成前に判定（N13） |

これらは compiler の処理時間でありゲームの FPS ではない。仮想環境の有限測定で、異なる機種・入力・WASM の絶対時間を保証しない。正しさを損なう候補削減を性能修正として入れることは避ける。

## 詳細・最小再現

## 構文・リテラル

証跡ディレクトリ: [evidence/root](minify-review-20261012/evidence/root/findings.md)。この節の裸の証跡ファイル名は同ディレクトリ内を指す。

対象は v0.3.1 (`5a168d6`) に metadata/source-map 修正を加えたチェックアウト。以下のパス・行番号はこの対象のもの。独立 consumer から公開 minify API を呼び、生成物と原文を別の Lua 5.3 VM で実行した。数値は i64 と f64 のビット列、文字列は実バイト列で比較した。

<a id="r1"></a>

### R1 — [P1] quoted string のデコードがバイト列と Lua escape を破壊する

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

<a id="r2"></a>

### R2 — [P1] long string の改行正規化が Lua と異なる

対象: `crates/storm-lua-syntax/src/numeric.rs:497-503,548-553`。

下記の `<CR><LF>` は説明用表記で、入力には実際の改行 byte を入れる。

```text
return [[a<CR><LF>b]]
```

Lua は `61 0A 62` を返す。minify は `return"a\r\nb"` を出力して `61 0D 0A 62` に変える。CR-only も `0D` が残る。opening delimiter の直後の LFCR は 1 つの改行として全体を除くべきなのに、LF だけを除いて CR を残す。quoted string の escaped LFCR も同様に extra CR を残す。

既定 / Safe+Exact / map ON/OFF で再現。実 WASM の `output.setNumber(1,#[[a<CR><LF>b]])` は **3 → 4**。Windows 系改行のスクリプト中にある long string の内容・サイズ・キー・描画文字列が変わり得る。

修正方向: long/escaped newline に対して LF、CR、CRLF、LFCR の Lua 正規化を適用し、その後に byte 列を再エンコードする。source text の改行保持と string value の改行正規化を混同しない。

証跡: `syntax-corpus-failures.jsonl` の `long-string-newline`、`literal-probes.jsonl` の `short_lfcr`、`wasm-game-runtime.jsonl`。

<a id="r3"></a>

### R3 — [P1] CR-only の行コメントが後続コードを飲み込む

対象: `crates/storm-lua-syntax/src/lexer.rs:299-301`。位置計数の `233-245` も LF のみを改行としている。

```text
-- comment<CR>return 42
```

Lua の戻り値は 42。既定の minify 出力は空、extended の lexical 出力は CR のみになり、戻り値がなくなる。コメントの終わりを LF しか認めないため、その後のコードがコメントとして除去される。これは AST 最適化を無効にする lexical 経路にも残る。

実 WASM Vehicle の `-- comment<CR>function onTick()output.setNumber(1,42)end` でも、原文の callback が短縮後に消える。Safe+Exact、map ON/OFF で再現。

修正方向: Lua が認める全改行形式を共通の lexer/位置計数処理へ実装する。改行 pair を 2 行に数えず、トークン位置と Source Map 側の位置表現の整合も確認する。

証跡: `root-option-matrix.jsonl`, `wasm-probes.jsonl`, `wasm-game-runtime.jsonl`。

<a id="r4"></a>

### R4 — [P1] hexadecimal number の読取りが Lua と一致せず、Exact でも値が変わる

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

<a id="r5"></a>

### R5 — [P2] number token を式へ置き換える printer が優先順位を保持しない

対象: `crates/storm-lua-syntax/src/print.rs:154-161,447`。

```lua
return 2/1e999
```

`{mode:"safe",numericMode:"exact"}` で `return 2/1/0` になり、**0.0 → +Infinity**。無限大の literal を `1/0` という binary expression にしているのに、Node::Num の出力は周囲の precedence を無視してそのまま返す。

この指摘は非有限数を許さない Smallest の前提には依存しない。Safe が返す assumptions は `finiteNumbers:false` / `integerFloatSubtypeMayChange:false`。map の有無でも再現。一般に literal renderer から複合式を返す設計と、AST printer の優先順位処理の境界が崩れている。

修正方向: 数値 token の元綴りを保持するか、代替式を AST として表現して適切に括弧を付ける。`*`, `/`, `//`, `%`, unary, power, concat の両 operand で検証する。

証跡: `root-option-matrix.jsonl` の `return 2/1e999`、`wasm-probes.jsonl`。

<a id="r6"></a>

### R6 — [P2] minify の再帰解析に深さ制限がなく、Native abort / WASM 例外になる

対象: `crates/storm-lua-syntax/src/parser.rs:561-603` と再帰する prefix/body 処理。`source_tools.rs:361-378` の inspection 用制限は minify に適用されない。

```python
source = "return " + "(" * depth + "1" + ")" * depth
```

`targetSize:8192` の公開 minify 呼び出しで検証。Native の未最適化 consumer は depth=3,200（6,408 bytes）で **SIGABRT / stack overflow**、保存された release WASM は depth=6,400（12,808 bytes）で **RangeError: Maximum call stack size exceeded**。core dump は無効にし、子プロセスで試験した。

同じ release WASM の `inspectSource` は depth=200 から `source nesting exceeds inspection limit` を通常の結果として返すが、minify は depth=3,200 まで `ok:true` を返した。この深さは Lua 自身の構文深さ上限を超えるため、正しい Lua の意味破壊としては数えない。編集中・生成途中の入力を通常の診断として拒否できず、呼出し元を巻き込む堅牢性の不具合である。閾値は build/stack 環境に依存する。

修正方向: 解析入口にトークン数・深さ等の明示的な予算を置き、再帰へ入る前に diagnostic へ変換する。生成 AST の再帰処理にも同じ制約を適用し、compile / analyze / inspect の対象範囲を明確にする。

証跡: `depth-probes.json`, `depth-wasm.json`。単なるテスト実行不能ではなく、実際の process exit / WASM exception を記録した。

### 検証範囲と再実行

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

## 数値・代数変換

証跡ディレクトリ: [evidence/numeric](minify-review-20261012/evidence/numeric/README.md)。この節の裸の証跡ファイル名は同ディレクトリ内を指す。

対象は `5a168d6`、v0.3.1 の `metadata-source-map-fix` 作業ツリー。レビュー担当は製品ソースを変更していない。初期に旧ルートを調べたが、この証跡はすべて v0.3.1 に依存先を切り替えて再実行した結果。対象ファイルの内容ハッシュは [source-manifest.json](minify-review-20261012/evidence/numeric/source-manifest.json)。

### 実行方法と範囲

`harness.rs` は独立 Cargo consumer。公開 `storm_lua_minify::compile_code` の生成物を、未変換ソースと別々の `mlua 0.10.5 / vendored Lua 5.3` VM で実行して比較する。callback ケースの同じ setup / run は各 VM に適用する。入力 `only` があるケースだけ、指定以外の pass を無効化して原因を分離した。通常ケースは全 pass が既定のまま。

```bash
python3 docs/verification/minify-review-20261012/evidence/numeric/reproduce.py \
  --repo <v0.3.1-checkout> --output numeric-rerun.jsonl
```

Cargo は保護 wrapper を通し、build artifact は `/tmp/cargo-targets` 配下だけに作る。sandbox が `findmnt` に同じ tmpfs を二重表示する環境では wrapper が拒否したため、今回のビルドは sandbox 外の同じ wrapper で実行した。SDK / product のビルド先設定は変更していない。

- [focused-cases.jsonl](minify-review-20261012/evidence/numeric/focused-cases.jsonl) / [focused-results.jsonl](minify-review-20261012/evidence/numeric/focused-results.jsonl): 40 入力 × 4 モード = 160 比較。`default=smallest+tolerant`、`safe=safe+tolerant`、`exact=smallest+exact`、`safe_exact=safe+exact`。
- [matrix-summary.json](minify-review-20261012/evidence/numeric/matrix-summary.json): 22 リテラル × 22 リテラル × 18 演算子 × 3 モード = 26,136 比較。26.4 秒。`numeric_matrix.py <built-harness>` で再生成できる。出力先は実行ディレクトリ。
- [matrix-failures.jsonl](minify-review-20261012/evidence/numeric/matrix-failures.jsonl): 上記の生の差分。**許可された subtype 変更も含むため、バグ件数ではない。** 両方が runtime error の組は行番号差を失敗に数えていない。
- [option-results.jsonl](minify-review-20261012/evidence/numeric/option-results.jsonl) / [option-summary.json](minify-review-20261012/evidence/numeric/option-summary.json): 15 再現例 × 4 モード × fast 2 通り × target 3 通り × origin 2 通り = 720 比較。1.05 秒。`option_matrix.py <built-harness>` で再生成。origin は `CompileOptions.origin_source` の内部 provenance tracking で、build SDK の Source Map API 全体の検証ではない。
- [format-resource-result.json](minify-review-20261012/evidence/numeric/format-resource-result.json): 256 MiB のアドレス空間制限と core dump 無効化を設定した子プロセスで、不必要な約 1 GiB allocation が SIGABRT を起こすことを確認。

通常の focused 結果の `same` は raw 値・subtype・エラーテキストの完全一致。エラー内の行番号が変わっただけの `same=false` を欠陥として扱っていない。`ApiAssumptions` は Smallest で `finiteNumbers` と `integerFloatSubtypeMayChange` を true にする（numericMode が Exact でも同じ）。したがって Smallest での subtype 単独差、NaN/Infinity 前提外の代数変形は主指摘から除いた。Safe ではこの 2 flag が false のため、Safe の subtype / -0 破壊は指摘対象。

### 確認した欠陥（修正箇所の単位）

以下の行番号は対象 v0.3.1 の `crates/storm-lua-minify/src/passes/` 配下。各例の完全な生成コード・pass 記録は focused-results の同名 id にある。

<a id="n1"></a>

#### N1 — 整数除算・剰余の literal 評価が panic する [P1]

`literal_folding.rs:258,266`。`return 0x8000000000000000//-1` は Lua では i64 MIN、`%-1` は 0。コンパイラは Rust の `div_euclid` / `rem_euclid` をそのまま使い、overflow panic を起こす。Default と Safe。Exact は再現しない。`int-div-panic`, `int-mod-panic`。

通常の整数 overflow は Lua 5.3 の wrap 規則であり、未定義のプログラムではない。Rust panic を捕捉しても誤評価そのものの修正にはならない。

<a id="n2"></a>

#### N2 — 除算・剰余の丸め規則が Lua と異なる [P1]

`literal_folding.rs:258-274`。Default / Safe で再現。

| id / source | 未変換 | 生成物の実行 |
|---|---:|---:|
| `negative-divisor`: `5//-2,5%-2` | `-3,-1` | `-2,1` |
| `float-mod-positive`: `1.0%1e20` | `1.0` | `0` |
| `float-mod-negative`: `1.0%-1e20` | `-1e20` | `0` |

整数の Euclidean division は負の除数に対する Lua floor division と一致しない。浮動小数点の `((x%y)+y)%y` は不要な加算・剰余で大きな丸め損失を起こす。許容誤差や subtype の差では説明できない値の破壊。

<a id="n3"></a>

#### N3 — bit 演算の literal 評価に JavaScript / Rust の規則が残る [P1]

`literal_folding.rs:176-187,295-305`。Default / Safe。

- `1<<64`: Lua は `0`、生成物は `1`。shift count を `&63` で循環させている。
- `4>>-1`: Lua は `8`、生成物は `0`。負の shift は逆方向の shift である。
- `~4294967296.0`: Lua は `-4294967297`、生成物は `-1`。float operand を JS int32 に落としている。
- `~1.5`: Lua の「整数表現を持たない」runtime error が `-2` に置換される。

<a id="n4"></a>

#### N4 — literal の型変換・比較が Lua の値を保持しない [P1]

`literal_folding.rs:147-226,230-310,1024-1044`。

- `9007199254740993>9007199254740992` は true → false、`9007199254740993==9007199254740992.0` は false → true。整数を f64 に落として比較している。Default / Safe。
- `true+1`、`nil-1`、`"10"<2`、`math.floor(true)` は Lua runtime error が通常値に変わる。Default / Safe。
- `math.min("2","10")` は有効な Lua で文字列 `"10"` を返すが、生成物は整数 `2`。`min` を数値変換している。Default / Safe。
- `1.0 .. "x"` は `"1.0x"` → `"1x"`。`literal_to_js_string` が Lua の文字列化を再現していない。Default / Safe。
- `math.abs(9007199254740993)` は Safe でも `9007199254740992` に変わる。

<a id="n5"></a>

#### N5 — 算術 identity の除去が必要な型変換を消す [P1]

`literal_folding.rs:800-830`。`local x="3" return x*1` は数値 `3.0` → 文字列 `"3"`。`local x=false return x+0` は runtime error → false。Default / Safe。operand の数値性を証明せず `+0`, `*1`, `/1` を除去している。

<a id="n6"></a>

#### N6 — Safe で subtype / -0 を保持できない [P2]

Safe の公開契約は `integerFloatSubtypeMayChange=false`, `finiteNumbers=false`。

- `literal_folding.rs:70-75`: `return math.type(1/1)` は `"float"` → `"integer"`。Num の folded result を整数 spelling にする。`return 1/-0.0` は `-Infinity` → `+Infinity`。
- `induction_values.rs:14-19,130-137`: `for i=1.0,1.0 do return math.type(math.floor(i)),math.type(i) end` は `integer,float` → `float,float`。整数値を持つ float と integer subtype を区別せず induction variable を扱う。**Safe+Exact でも再現。**
- `boolean_numerics.rs:75-83,195-210`: `local x=3 local bit=x&1>0 return math.type(bit and 1.0 or 0.0)` は float → integer。**Safe+Exact でも再現。** 数値 boolean consumer の literal token subtype を保持していない。

Smallest の同じ subtype 差は公開 assumptions が許容するため、ここでは Safe の不整合として扱う。

<a id="n7"></a>

#### N7 — quotient/remainder fusion が integer overflow を変える [P1]

`quotient_remainder.rs:124`。Default と Smallest+Exact。次のプログラムは約 `+9.223372036854776e17,2` を返すが、`x=x//10` への置換後は `-922337203685477581,2`。

```lua
local x=0x8000000000000000
local r=x%10
x=(x-r)/10
return x,r
```

元の `x-r` は i64 wrap 後に float division する。直接 floor-divide することとは数値として等価でない。`quotient-isolated` でも同 pass 単独で再現。

<a id="n8"></a>

#### N8 — signed expression factoring が代入後の古い値を使う [P1]

`signed_factoring.rs:308-325`。Default / Smallest+Exact。比較する statement の**間**だけ barrier を調べ、共通式を含む statement 自体の書き込みを無視する。

```lua
local x=1 local a,b,c
x=x*12345
a=x*12345 b=x*12345 c=x*12345
return x,a,b,c
```

期待 `12345,152399025,152399025,152399025` がすべて `12345` になる。`signed-expression-factoring` と `scope-renaming` だけでも再現した。

<a id="n9"></a>

#### N9 — split-sign の消去が外側から観測される書き込みを削除する [P1]

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

<a id="n10"></a>

#### N10 — split-sign の二乗変形が副作用・整数演算を変える [P1]

`split_sign.rs:297-321`。Default。

- `math.max(x,0)^2*f()-math.min(x,0)^2*f()` → `x*math.abs(x)*f()` により f の実行が 2 回 → 1 回。`split-effects`。
- x 部分が同じ `f()` AST でも、呼び出し結果が変わる場合がある。`split-changing-operand` は結果 `-3` → `2`。
- x が整数 `4000000000` の場合、元の `^2` は float の `1.6e19`、変形後の整数積は wrap した `-2446744073709551616`。`split-overflow`。

AST の同形性しか調べず、値の安定性・calls・型を確認していない。

<a id="n11"></a>

#### N11 — product/coefficient carrier の組み替えが整数 overflow を導入する [P1]

`multiplicative_carriers.rs:51-62,158-208,229-246`、`coefficient_carriers.rs:47-78,316-334,367-389`。input.getNumber(1) を Engine 契約と同じ float `1.0` として実行。

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

<a id="n12"></a>

#### N12 — numeric approximation が exact route を保護しきれない [P1]

`numeric_literals.rs:196-207,226-240,289-304,458-484`。Default。

- table constructor の数値 key は exact と判断されない。`local t={[1.23456789012345]=7} ... t[property.getNumber("x")]` の key が `1.23456789` に変わり、元の key を返す property で `7` → nil。`approx-table-key`。同 pass 単独の `local t={[1.23456789012345]=7}return t[1.23456789012345]` でも key と index が食い違う。
- exact な呼び出しから到達する user-function は `exact_functions` に記録するだけで、rewrite context に渡していない。function return の literal が `string.pack` に渡されても近似される。`approx-function-return`。Safe / Exact はこれらを維持する。

<a id="n13"></a>

#### N13 — string.format folding が未採用候補へ巨大 allocation を行う [P1、性能にも影響]

`literal_folding.rs:639-655`。Lua 5.3 が拒否する長い width を usize として受け入れ、サイズ比較より前に `"0".repeat(zeros)` を実行する。

- `return string.format("%018446744073709551615d",1)` は Lua runtime error を保持せず、コンパイラで `capacity overflow` panic。Default / Safe。
- `return string.format("%01000000000d",1)` は制限した子プロセス内で 999,999,999 bytes の allocation を試行し SIGABRT。実測は [format-resource-result.json](minify-review-20261012/evidence/numeric/format-resource-result.json)。メモリ上限解除やホスト全体の OOM は試していない。
- 同じ 30 字程度の入力で width を 1,000,000 → 10,000,000 にすると、Safe compile が約 130 ms → 1,269 ms。Exact は双方約 1.1 ms。**未最適化 Rust build の単発値**であり、本番の絶対時間や速度保証ではない。いずれも候補は長すぎて捨てられ、出力コードは同じ string.format 呼び出しのまま。

まず Lua が許可する format の範囲を検証し、生成文字列の長さだけで候補が改善しないと分かる場合は文字列を作らず棄却できる。

### オプション横断結果

15 の代表例では fast の有無と provenance の有無によって再現の成否は変わらなかった。targetSize 未指定と 0（達成不能）の結果も同じ。targetSize=10000 は変換前の短縮段階で止まり、今回の 15 バグを通らなかった。これは特定の早期終了経路で欠陥が隠れるという結果であり、最適化の正しさを保証しない。

26,136 ケースの算術 matrix では Exact は全 8,316 成功入力が値・subtype 一致、396 組は双方 runtime error。Default と Safe の差分件数は `matrix-summary.json` に記録。これは当該 literal matrix の結果であり、Exact 全体が安全という結論ではない（N7/N8、Safe+Exact の N6 は別の構造化入力で再現）。

### 独立した scope 指摘の反証確認

[scope-cross-summary.json](minify-review-20261012/evidence/numeric/scope-cross-summary.json) に別担当 S1–S8 の 30 比較を記録した。この担当の別 runner でも input.getNumber を明示的に float `i*3.0` とし、S5 は onDraw で output API を呼ばない原文へ変更して再実行した。すべての報告モードの再現性が一致し、数値 subtype や callback phase に起因する偽陽性は確認しなかった。これは別担当の件数と重複する確認で、新規バグ件数に加算しない。

### 修正時に守るべき検証境界

- literal evaluator は Lua の integer / float / byte string を型ごとに評価し、Rust / JS の便利な変換を意味論の代わりに使わない。overflow、負の除数、負・64 以上の shift、エラーを回帰対象にする。
- algebraic rewrite は等価な数式だけでなく、operand の subtype、overflow、calls、capture、当該 statement 自体の書き込みを含めて適用可否を証明する。
- exact-route の dataflow は table constructor key と呼び先の return literal まで保護情報を rewrite へ届ける。
- Safe の float literal は結果の subtype と符号付きゼロを保持する。Smallest の既存 assumptions を Safe に暗黙拡大しない。
- string.format の候補サイズは allocation の前に上限判定し、不正 format を折り畳まない。

### 参照・未検証範囲

演算の基準は実行した vendored Lua 5.3。規則の確認には [Lua 5.3 arithmetic / bitwise operations](https://www.lua.org/manual/5.3/manual.html#3.4.1) と [Lua 5.3 string library source](https://www.lua.org/source/5.3/lstrlib.c.html) も使用した。実ゲーム適合性、WASM runtime 差、build SDK の全 Source Map 経路はこの数値担当の実行範囲外。

common_offsets と literal_pool / exact expression synthesis もコードを読み、代表例を試したが、この担当では追加の確定バグを見つけていない。網羅的正当性証明ではない。Lua byte string の破壊と hex literal parser / renderer の問題は別担当へ引き継いだため、ここでは重複計上していない。

## スコープ・table・状態変換

証跡ディレクトリ: [evidence/scope](minify-review-20261012/evidence/scope/scope-review.md)。この節の裸の証跡ファイル名は同ディレクトリ内を指す。

対象は `5a168d6`（v0.3.1）を基点とする metadata-source-map-fix 作業ツリー。minify / analysis / syntax の製品コードは変更していない。別セッションの未コミット修正を保持したまま調査した。

### 検証方法と集計

独立 Cargo consumer から `storm_lua_minify::compile_code` と個別 pass を呼び出し、原文・生成コードを別々の vendored Lua 5.3 (`mlua 0.10.5`) VM で実行した。`input.getNumber(i)=3*i`、`input.getBool(i)=i%2==0`、`property.getNumber=3` 等の明示的な入力を使用し、`output.setNumber/setBool` 呼び出しを値・型・引数個数・順序まで記録。各原文・生成物について onTick→onDraw を 2 回実行した。これは実ゲームや実 Engine I/O 境界の検証ではなく、Lua 意味論の差分検証である。

`findings-matrix.jsonl` 以降の `default` は実際の `CompileOptions::default()`（Smallest / Tolerant / Exhaustive / zero_cost_newlines=true）。`exact` は NumericMode::Exact、`safe` は Safe + Exact。`target-probes*.jsonl` は zero_cost_newlines=false でも確認した初期調査。個別 pass 名を mode にした行は full pipeline ではない。

- 24 テンプレート × 5 値設定 × 4 モード = **480 件**の構造化差分検証。原文の構文・実行エラー 0 件。結果は **395 一致、70 出力不一致、15 コンパイル拒否**。不一致と拒否は以下の既知の再現クラスへ集約できる。
- 上記のうち scope-renaming 単独 120 件はすべて一致した。
- full pipeline の 360 件は 275 一致、70 不一致、15 拒否。Safe / Exact でも複数の不具合を防げない。
- 他に個別再現、pass ON/OFF、67 pass の切り分け検査を実施。詳細は各 JSONL。エラーを一致・成功として集計していない。
- 実行時間ではなく実行回数を持つ instruction hook により、各 Lua VM は約 100 万 instruction を上限とした。原文エラーとコンパイル拒否は区別した。
- 別担当の独立 runner でも S1–S8 を 30 比較し、input.getNumber を明示的に float `i*3.0` として全モードの再現性を確認した。S4 / S7 はさらに公開 build API consumer で 8 比較し、原因 pass 無効化による復旧も確認。数値 subtype や callback phase に起因する偽陽性は見つからなかった。重複検証なので新規件数へ加算していない（`../numeric/scope-cross-summary.json`、`../api/cross-scope-results.jsonl`）。

<a id="s1"></a>

### S1 — [P1] table parameter scalarization が closure の capture を失う

箇所: `crates/storm-lua-minify/src/passes/tables.rs:1358-1360` および `rewrite_table_parameter_body` の Function skip。

```lua
local function f(t)
  local g=function()return t.x end
  return t.x+g()
end
function onTick()
  output.setNumber(1,f({x=input.getNumber(1)}))
end
```

原文は 6。default / exact / safe のすべてで `f(t)` の table 引数が scalar に置き換わり、closure 内の `t.x` だけが残る。生成コードは global `t` を参照して `attempt to index a nil value` になる。個別 pass でも再現し、`table-parameter-scalarization=false` で復旧。

ネスト関数を「解析しない」ことと「その関数による capture が存在しない」ことを同一扱いにしている。parameter を消す前に全 descendant の BindingId capture を検査するか、capture を含めて意味を保持する変換が必要。

証跡: `findings-matrix.jsonl` の `closure-table-param`、`target-probes.jsonl` の mode=`table-param`。

<a id="s2"></a>

### S2 — [P1] immutable-table flattening が `function table.member` を書き込みと認識しない

箇所: `crates/storm-lua-minify/src/passes/tables.rs:493-510,554-565`。

```lua
local t={}
function t.f()return 3 end
function onTick()output.setNumber(1,t.f())end
```

原文は 3。default / exact / safe のすべてが `No semantically valid optimization candidate survived verification.` で失敗する。個別 pass の出力は次の構文不正コード。

```lua
function nil()return 3 end
function onTick()output.setNumber(1,nil())end
```

`Node::Assign` の target は AssignTarget context になるが、`Node::Funcstat` の target は一般の読み込みとして走査される。空 table の未存在キーとして f を nil に畳み、table 自体も削除する。member function 定義は Lua で普通に使う構文なので影響は限定的な edge case に留まらない。immutable-table-flattening=false で復旧する。

証跡: `immutable-funcstat.jsonl`、`namespace-toggle-isolation.jsonl`。

<a id="s3"></a>

### S3 — [P1] immutable value reuse が挿入先の同名 local を捕捉する

箇所: `crates/storm-lua-minify/src/passes/immutable_values.rs:442-447`。

```lua
local tau=math.pi*2
function onTick()
  local tau=input.getNumber(1)
  output.setNumber(1,math.pi*2)
  output.setNumber(2,math.pi*2)
  output.setNumber(3,tau)
end
```

原文の値列は `2*pi, 2*pi, 3`。default / exact / safe では `3,3,3` へ変わる。式そのものの identity は BindingId を含めているが、再利用 carrier は元の SymbolId/spelling をそのまま各参照位置へ挿入する。その場所の tau が別 BindingId であることを確認していない。

個別 `reuse_immutable_values` でも再現。`immutable-global-expression-reuse=false` で復旧。候補の全利用位置で carrier binding が可視か検証するか、競合しない新名へ carrier と全正当な参照を先に rename する必要がある。

証跡: `findings-matrix.jsonl` の `carrier-shadow`、`target-probes2.jsonl` mode=`immutable-reuse`。

<a id="s4"></a>

### S4 — [P1] immutable carrier synthesis が同じ字面の別 binding を統合する

箇所: `crates/storm-lua-minify/src/passes/immutable_synthesis.rs:47-48,82-96`。

```lua
local a
 a=123456
function f()return a+789012 end
local a
 a=222222
function g()return a+789012 end
function onTick()
  output.setNumber(1,f())
  output.setNumber(2,g())
end
```

原文は `912468,1011234`。default / exact は両方 `912468`。Safe はこの再現では一致した。expression_key が Printer の文字列 `a+789012` だけなので、別の a の BindingId を持つ式も同一グループになる。最初の a から算出する 1 個の carrier に統合される。

個別 pass および `only=[immutable-carrier-synthesis,scope-renaming]` で再現し、synthesis を無効化すると復旧。S3 とは別のアルゴリズム・別の欠陥であり、carrier 可視性チェックだけでは直らない。BindingId を含む式 identity と、全依存値が確定した挿入位置が必要。

証跡: `immutable-synthesis-shadow.jsonl`。

<a id="s5"></a>

### S5 — [P1] root-local globalization が元からある global と local を統合する

箇所: `crates/storm-lua-minify/src/passes/root_globals.rs:26-48,54-59`。

```lua
x=0
function onDraw()
  x=x+1
end
local x=input.getNumber(1)
function onTick()output.setNumber(1,x)end
```

原文の onTick→onDraw×2 で output 1 は `3,3`。default / exact / safe は `3,4`。output は onTick 内だけで呼び、onDraw は状態のみを更新する。root_name_counts は Local / Localfunc / plain Funcstat だけを数え、ordinary Assign と global reference を除外している。その結果、onDraw が捕捉する既存 global x と onTick が捕捉する新しい local x を同一 global x にする。

同じ pass には固定名の別の公開範囲問題もある。次の原文にはホストが呼ぶ global onTick がないが、短縮後は公開 callback になり、毎 tick output へ 5 を書き込む。

```lua
local function onTick()output.setNumber(1,5)end
```

両方 default / exact / safe と個別 pass で再現。root-local-globalization=false で復旧。宣言の字面の数ではなく lexical binding と既存 globals を基準に、衝突しない名前へ変換し、fixed callback / API root 名を新規公開しないことが必要。

証跡: `root-collision-tick-only.jsonl`（主再現）、`findings-matrix.jsonl` の `root-local-callback`。初期の `root-global-collision` は synthetic host の onDraw output を含むので Engine 検証には主再現を使用する。

<a id="s6"></a>

### S6 — [P1] goto/label を越える dataflow の変換が値を誤る

箇所: `final_stores.rs:151-154,243-245`、`single_use_forwarding.rs:237-243`。関連する function-local slot coloring にも別の個別 pass 再現あり。

```lua
function onTick()
  local x
  x=1
  goto done
  x=2
  ::done::
  output.setNumber(1,x)
end
```

原文は 1。default / exact / safe は 2。final-dead-store-elimination 単独では x=1 が削除され、nil を出力する。goto/label を通常の空 effect として逆向き live-set 計算へ通すので、実行されない x=2 が x=1 の上書きであると誤認する。

また `local x` を取り除いた global 版では single-use-global-forwarding 単独でも x=2 を label の先へ転送し、実行されないはずの値 2 を出力する。中間 statement の call/dependency write だけを障壁にしており label/goto の到達関係を扱っていない。final-dead-store-elimination だけを OFF にしても full pipeline の誤りは解消しない。

CFG による到達・支配・liveness がない間は、goto/label を含む対象関数でこれらの変換を保守的に止める必要がある。function_globalization の直線的 lifetime coloring も backward goto で global slot を再利用してしまうため、同じ対象検査に含めるべき。

証跡: `findings-matrix.jsonl` の `goto-forward` / `goto-global-forward`、`target-probes2.jsonl` の `back-goto` mode=`function`。

<a id="s7"></a>

### S7 — [P1] destructive result globalization が alias からの呼び出しを破壊する

箇所: `crates/storm-lua-minify/src/passes/destructive_results.rs:82-119,185-196`。

```lua
v=100
function f(x)x=x+1 return x end
local saved=f
function onTick()
  v=f(1)
  v=f(2)
  output.setNumber(1,saved(10))
end
```

原文は 11。default / exact では f が引数・戻り値なしの関数になり、saved(10) の戻り値がなくなる。Safe はこの再現では一致する。変換条件は直接の `f(...)` 呼び出ししか調べず、f の値が saved に escape したことを調べない。

個別 pass と only=destructive-result-globalization で再現し、pass OFF で復旧。すべての参照が検証済み直接 call であることを BindingId 単位で証明する必要がある。追加検査として、callee が別関数経由で destination global を読むケースでも個別 pass の意味差を確認した（full default では別の変換により当該再現の誤差は出なかったため、独立の本番再現としては数えていない）。

証跡: `findings-matrix.jsonl` の `escaped-destructive-function`、`target-probes2.jsonl` の `destructive-side`。

<a id="s8"></a>

### S8 — [P1] table の integer64 キーを f64 に変換して別キーを同一視する

箇所: `crates/storm-lua-minify/src/passes/tables.rs:316-325,366-368`。

```lua
local t={[9007199254740992]=10,[9007199254740993]=20}
function onTick()output.setNumber(1,t[9007199254740993])end
```

Lua 5.3 の原文は 20。default / exact / safe は 10 になる。StaticTableKey::Num(f64) / num_val によって隣接する 64-bit integer がともに 9007199254740992 へ丸められ、table_get が最初のフィールドを返す。NumericMode::Exact でも止まらない。

immutable-table-flattening 単独で再現し、pass OFF で復旧した。integer と float の Lua key 正規化を保った表現にして、integer の識別へ f64 を使わないこと。通常の数値定数畳み込みとは異なる table lookup 層の問題である。

証跡: `findings-matrix.jsonl` の `int64-table-key`、`target-probes.jsonl` mode=`immutable-table`。

<a id="s9"></a>

### S9 — [P1] local sinking 後の table を prefix expression として構文不正に出力する

箇所: `crates/storm-lua-syntax/src/print.rs:464-494`、`crates/storm-lua-minify/src/passes/locals.rs:734-767`。

```lua
local t={{}}
local a=t[1]
function onTick()output.setBool(1,a==a)end
```

原文は毎 tick true。default / exact / safe のすべてが `No semantically valid optimization candidate survived verification.` で失敗する。single-use-local-sinking 単独の生成物は次の構文不正コード。

```lua
local a={{}}[1]
function onTick()output.setBool(1,a==a)end
```

Lua の table constructor は、index / call の receiver となるには `({{}})[1]` のように括弧が必要。Printer の Index は `expr(obj,15)` を呼ぶが、Table 分岐は parent precedence を考慮せず早期 return する。Function 分岐にも同様の形があるため、prefix-expression grammar を共通処理する修正が適切。`single-use-local-sinking=false` で復旧し、only=[single-use-local-sinking,scope-renaming] で再現する。

追加の 3 原文 × Default / Safe / Exact / Safe+Exact × origin_source の有無 = 24 検査でも全て同じ compile rejection。これは内部 provenance の有無の検査であり、公開 SDK の sourceMap API 全体を検証したという意味ではない。この追加 matrix は同じチームの `numeric/harness.rs` を使用した。

証跡: `local-sink-minimal.jsonl`、`local-sink-table-index.jsonl`、`local-sink-origin-matrix.jsonl`。

### 未採用・限定した観察

- 同じ table constructor key の重複で first/last の差を確認したが、Lua 言語の constructor assignment order の扱いを精査する必要があるため、確定 finding に数えていない。
- scope_rename 単独では 120 件すべて一致し、repeated local、nested closure、loop capture、repeat-until、shadowing に新しい問題を確認できなかった。全入力の正しさを保証する意味ではない。
- 現行 NodeArena は Arc/COW なので、単なる clone 呼び出しを見ただけで全 AST の複製と断定しなかった。既存キーを読む table flattening の測定は概ね線形だった。欠損キーについては別途 COW 発生を測定した。

### PERF1 — [P2 / performance] 未存在 table key の flattening が lookup ごとに全 AST を COW 複製する

箇所: `crates/storm-lua-minify/src/passes/tables.rs:591-593,373`、`crates/storm-lua-syntax/src/node_arena.rs:149-150`。

`local t={x=123}` と N 個の `output.setBool(1,t.missing==nil)` を持つ callback を、個別 `flatten_immutable_tables` で処理した。計測対象は parse + pass + print、Lua 実行を含まない。CARGO_PROFILE_DEV_DEBUG=0、CARGO_INCREMENTAL=0 の **非最適化 dev build**、各 5 回の中央値。wall-clock 値そのものを release 性能として扱わない。

| アクセス数 | 欠損 key の flattening | parse + print だけ |
|---:|---:|---:|
| 50 | 10.4 ms | 0.44 ms |
| 100 | 37.0 ms | 0.82 ms |
| 200 | 138 ms | 1.67 ms |
| 400 | 536 ms | 3.31 ms |
| 800 | 2,117 ms | 6.55 ms |
| 1,600 | 8,413 ms | 12.9 ms |

N を倍にすると pass 全体が約 4 倍になる。既存 key の場合は 1,600 件でも 46.0 ms で概ね線形だった。`scratch=clone_table_ast(source)` 自体は Arc clone だが、`table_get` が absent を返すために scratch へ Nil を push すると `Arc::make_mut` により AST が全複製される。それを各 lookup ごとに破棄して再実行している。

読み取り専用 lookup を `Option<NodeId>` などに変更し、Nil を生成する必要がある場合も最終出力 arena にだけ作ると、この繰り返しを避けられる。性能改善後は missing nested key の runtime error 保持も合わせて検証が必要。

証跡: `table-flatten-missing-timing.jsonl` / `table-flatten-timing.jsonl`、`benchmark.py` / `benchmark_missing.py`。

最適化 release build でも同じ測定を実施した。各 5 回の中央値は以下。`CARGO_INCREMENTAL=0 cargo build --release`、他の release profile override なし。

| アクセス数 | 欠損 key の flattening (release) | parse + print だけ (release) |
|---:|---:|---:|
| 50 | 2.66 ms | 0.075 ms |
| 100 | 10.0 ms | 0.126 ms |
| 200 | 37.8 ms | 0.298 ms |
| 400 | 148 ms | 0.539 ms |
| 800 | 589 ms | 1.16 ms |
| 1,600 | 2,398 ms | 2.19 ms |

既存 key の 1,600 件は release で 8.02 ms。欠損 key では最適化ビルドでも約 300 倍遅く、二次的な増加傾向が残る。入力サイズやテーブル内容の違いもある比較なので比率そのものを製品全体の速度差とは解釈しない。証跡は `table-flatten-missing-release-timing.jsonl` / `table-flatten-release-timing.jsonl`。

## API・描画・候補探索

証跡ディレクトリ: [evidence/api](minify-review-20261012/evidence/api/findings.md)。この節の裸の証跡ファイル名は同ディレクトリ内を指す。

対象は `5a168d6`（v0.3.1）の当日 metadata/source-map 修正を含む作業ツリー。製品コードは変更していない。以下の行番号はその対象ツリーのもの。Lua 5.3（mlua 0.10.5、vendored）で元コードと公開 `storm_lua_build::public_api::compile` の結果を別々に実行し、input/property を固定供給して出力・描画呼び出し列を比較した。実ゲームの画素一致試験ではない。

証跡の `cases.jsonl` は完全な入力、options、ホスト設定、実行順を保持し、`results.jsonl` は生成コード・コンパイラ診断・両者の実行結果を保持する。以下はすべて公開 API から再現する。F1〜F6 は `ok:true` の生成コードの誤動作、F7 は正しい入力のコンパイル失敗である。

<a id="f1"></a>

### F1 — [P1] 合成識別子が利用者の束縛と衝突する（既定設定）

対象:

- `crates/storm-lua-minify/src/passes/api_aliases.rs:174-177`
- `crates/storm-lua-minify/src/passes/screen_call_factoring.rs:107-109`
- `crates/storm-lua-minify/src/passes/output_loops.rs:141-146`
- `crates/storm-lua-minify/src/passes/screen_loops.rs:624-629`

合成名を `intern` するだけで、既存の名前・スコープとの衝突を避けていない。後段の scope rename はすでに合流した束縛を復元できない。

最小に近い公開 API 再現:

```lua
function onTick() __api0=input.getNumber(1) end
function onDraw()
 screen.drawText(0,0,__api0)
 screen.drawText(0,1,__api0)
 screen.drawText(0,2,__api0)
end
```

生成コードは `a=screen.drawText; function onTick()a=input.getNumber(1)end; ...a(0,0,a)...` となる。入力 1 を与えた後の onDraw が、元コードでは `1,1,1` を描画し、生成コードでは number の呼び出しエラーになる。

同じ欠陥を別々のパスでも確認した:

| 証跡 case | 既存の名前 | 元の結果 | 生成後 |
| --- | --- | --- | --- |
| F1-api-name-collision | `__api0` | `1,1,1` | number の呼び出しエラー |
| F1-color-name-collision | `__sc0` | 42 を 8 回描画 | number の呼び出しエラー |
| F1-output-name-collision | `__stormmin_output_values_0` | 最後の出力は 1 | 合成配列に上書きされる |
| F1-screen-name-collision | `__stormmin_screen_index_0` | 1 を 8 回描画 | `0,1,2,3,4,5,6,7` |

修正方向: 全合成名を既存の全識別子と照合する決定的な fresh-name allocator で生成する。`draw_sequences` などが既に行っている全インターナー名の収集も参考になる。ローカル捕捉も含め、全合成名の生成箇所を横断して回帰確認する。

<a id="f2"></a>

### F2 — [P1] button helper の引数化が条件付き評価と副作用順序を変える（既定設定）

対象: `crates/storm-lua-minify/src/passes/screen_buttons.rs:425-430`、同 `323-329`。

もともと `hit(...)` → `active` → `pulse` の判定後に評価していた代入 RHS を、helper 呼び出しの第 4 引数 `site.value` に移す。このため hit が変えるグローバルを古い値で読み、実行しない分岐の式まで先に評価する。label/x/y の読み取りも元の色変更・hit との順序から動く。

F2-button-eager-value は 3 個のボタンを持ち、hit が `value=value+100` を行う。各ボタンの条件成立時に `value=value+1 / +7 / +13` を実行する。元の最終描画は **321**、既定 minify 後は **21**。x/y は onTick の入力から取得し、先行する定数畳み込みにより問題が隠れない通常の動的座標を使っている。

追加の単独パス・公開 passToggles 試験では、元は通らない `if pulse then value=bad+1 end`（bad は table）の RHS を先行評価し、生成後に初めて arithmetic error が発生する例も再現した。

修正方向: 条件下の RHS を無条件の関数引数へ移してよいという証明が必要。例外可能性、読み取り対象に対する hit/on/off の書き込み、読み取り回数・順序を検査する。証明できない場合は抽出を行わないか、元の評価位置で実行する構造を維持する。

<a id="f3"></a>

### F3 — [P1] button helper の root 移動が lexical local を別の global に変える（既定設定）

対象: `crates/storm-lua-minify/src/passes/screen_buttons.rs:323-329`、同 `355-364`、同 `461-467`。

helper を root の先頭に挿入する一方、元の `active`、`pulse`、代入先、callee ノードをそのまま使用する。元の callback 内 local が、新しい helper 内では同名 global として解決される。すべての自由変数・代入先が root から参照できるという検査がない。

F3-button-local-capture の onDraw は `local value=0` を持ち、`function kept() return value end` にも capture させている。このため別の function-globalization が誤変換を偶然隠せない。3 ボタン操作後、元の最終描画 **21** に対し既定生成コードは **0**。onDraw のパラメータが value/active/pulse である例でも再現した。

修正方向: 抽出先のスコープから全 free binding が同じ binding に解決されることを確認する。代入先 local の capture を保持できない場合は適用不可にする。F2 の評価順検査とは独立の修正が必要。

<a id="f4"></a>

### F4 — [P1] callback slotting が初期化時に呼ばれる関数を削除する（既定設定）

対象: `crates/storm-lua-minify/src/passes/callback_slots.rs:352-378`。

root の呼び出しは owner `0` として calls に収集されるが、採用判定は `reachable(onTick)` と `reachable(onDraw)` しか参照しない。初期化時にも呼ばれる helper を callback 専用と誤判定し、root の定義を削除して callback 内へ移す。

F4-callback-init-call は通常の property 初期化値を 55 個保持し、top-level で `tickHelper(10)` を一度呼んで状態を初期化する。onTick は同 helper を使い、onDraw は別 helper を使う。55 個の live binding は、名前を共有する候補がサイズ上で勝つ条件を作る。元コードは正常に初期化・tick・draw を実行するが、生成コードは初期化中の **nil 関数呼び出し**で停止する。

修正方向: root 初期化、他の entrypoint、escape した closure などからの到達を除外して初めて callback 専用と判定する。callback 同士の再入・直接呼び出しも共有可能性の前提として別途点検する必要がある。現時点の実行再現は root 初期化の欠落についてのもの。

<a id="f5"></a>

### F5 — [P1] screen loop の f64 等差判定が exact モードでも 64-bit 整数を破壊する

対象: `crates/storm-lua-minify/src/passes/screen_loops.rs:72-89`。

すべての `Node::Num` を `num_val` で f64 化し、その値だけで等差列・同値を判定する。`9007199254740992` と `9007199254740993` は Lua 5.3 では異なる整数だが、この判定では同値になる。

```lua
function onDraw()
 screen.drawText(0,0,9007199254740992)
 screen.drawText(0,5,9007199254740993)
 screen.drawText(0,10,9007199254740992)
 screen.drawText(0,15,9007199254740993)
 -- 証跡は同じ交互パターンを計 8 呼び出し使用
end
```

生成コードは `for a=0,7 do screen.drawText(0,a*5,9007199254740992) end` となる。F5-screen-i64-default と F5-screen-i64-safe-exact の両方で確認。`mode:"safe", numericMode:"exact"` でも発生する。整数を text 引数として使うため、描画座標の f32 丸めの問題ではない。

修正方向: Lua の integer/float を区別した数値表現で証明する。最低限、f64 で正確に表現できない integer literal はこの affine 変換から除外する。`draw_records::Atom` の整数を保つ処理は同じ境界ケースを保持できた。

<a id="f6"></a>

### F6 — [P1] API member の書き換えを builtin の証明が無視する（既定設定・safe/exact）

対象: `crates/storm-lua-minify/src/passes/api_aliases.rs:40-45`、同 `99-109`、`crates/storm-lua-analysis/src/effects.rs:401-421`、`crates/storm-lua-minify/src/passes/property_reads.rs:50-60`。

`node_write` は member 代入の Index ノードをここで期待する write として扱わないため、`screen.drawText = function...` の左辺まで API alias の bare name に書き換わる。また EffectAnalyzer の builtin root invalidation は root 変数への write だけを調べるため、member を置き換えた後も元の builtin として分類する。

F6-api-table-mutation は drawText を自作関数に差し替え、`screen[property.getText("fn")]()`（fn は drawText）を 1 回、静的な drawText を 3 回呼ぶ。その回数を次の onTick で output に送る。元は **4**。生成後は動的呼び出しが古いホスト API に流れ、**3** になる（記録 stub では余分な `dynamic` 描画も観測）。既定設定と safe/exact の両方で確認。

F6-property-function-mutation では `property.getNumber=function(k)return 7 end` があるのに、hardcode option `numbers.X=42` がその後の呼び出しを 42 に置き換える。元出力 **7** に対し **42**。

修正方向: 代入/関数宣言の左辺を文脈付きで扱い、Index の代入先を値読み取りとして alias 化しない。root/member の書き換えやエイリアス経由の mutation がある場合は、その builtin の不変性・純粋性・hardcode 可否を無効化する。この問題は F1 の fresh name 修正では解決しない。

<a id="f7"></a>

### F7 — [P2] 結果を捨てる property 呼び出しを hardcode すると不正な文になる

対象: `crates/storm-lua-minify/src/passes/property_reads.rs:33-41`、同 `55-74`。

```lua
property.getNumber("X")
function onTick() output.setNumber(1,1) end
```

`{property:{mode:"hardcode",numbers:{X:42}}}` で `Callstat(Call(...))` の子だけを数値に置き換え、`Callstat(Num(42))` を残す。Lua の文として 42 は無効なので既定探索の全候補が再パースで失敗し、`ok:false` / `No semantically valid optimization candidate survived verification.` を返す。getBool/getText でも同様。targetSize 0/8192 では `satisficing checkpoint failed reparse validation` になる。

修正方向: 文脈を保持して置換し、結果が不要な pure property 呼び出しは文ごと削除する。呼び出しの副作用・エラーを消せることは F6 の builtin 証明と合わせて確認する。

### 性能 — color subset 探索の 12 group 境界

対象: `crates/storm-lua-minify/src/passes/screen_call_factoring.rs:214-241`。

eligible group が 12 個以下の場合、全 `2^N-1` 部分集合について AST 複製とプログラム全体の renamed size 計測を行う。13 個になると greedy 探索に切り替わり、入力が増えたのに大幅に速くなる。

Rust release consumer、sourceMap 無効、各色 8 回、初回ウォームアップを捨てた 6 サンプルの中央値:

| 色 group 数 | Lua サイズ | 単独 pass の中央値 |
| --- | --- | --- |
| 9 | 2036 文字 | 9.602 ms |
| 10 | 2260 文字 | 20.076 ms |
| 11 | 2484 文字 | 43.972 ms |
| 12 | 2708 文字 | 95.144 ms |
| 13 | 2932 文字 | 2.777 ms |

12 group は 13 group の約 **34.3 倍**。`perf-release-summary.json` に全サンプルと min/max を保存した。これは単独 pass の計測であり、WASM 全体レイテンシや実ゲームの FPS ではない。全体 AST の clone/resolve を削減する、同じコストモデルの共有部分を再利用する、決定性と最終サイズ契約を維持できる pruning を検討する価値がある。単に探索候補を捨てる変更はサイズ最小化契約との比較が必要。

### 追加の検証範囲

- 描画 differential generator: **627 実行**。120 種の線形/周期/乱数/反復/平行移動パターン × record packing・screen loop・periodic loop・motif outlining・公開 compile の 600 実行と、64-bit 整数境界の 27 実行。
- 501 実行は呼び出し列一致。126 差分の内訳は、別担当で扱う Lua バイト文字列 printer 破損に起因する 120 件、F5 に起因する 6 件。多数の差分を独立したバグ数として数えていない。
- sourceMap/options 交差試験: **210 compile 実行 / 105 ON/OFF 組**。既定・fast beam 1/16・target 0/8192・newline 無効・safe/exact と、Unicode sourceName、extended/_ENV の lexical 経路を含む。成功した **98 組すべて**で map の公開 validator が成功し、ON/OFF のコード/成否が一致。7 組は F7 の入力で両方失敗。今回の交差試験から新しい map 不整合は検出しなかった。
- ソースコードの再パースおよび Source Map validator が成功しても、上記 wrong-code は検出されない。`semanticRejected:0` は実行意味論の合格を表すものではない。公開 compile は外部の semantic verifier を使わない。

### 再実行

監査対象 checkout を `--repo` に渡す。harness 自体は一時 consumer として作り、製品ファイルには書き込まない。Cargo cache は必ず既存規約どおり `/tmp/cargo-targets` 配下。

```bash
python3 docs/verification/minify-review-20261012/evidence/api/run.py \
  --repo /path/to/audited-checkout --release > api-results.jsonl
```

追加入力は `--cases FILE`。`draw-fuzz.py /path/to/audited-checkout` と `map-matrix.py` は JSON Lines を標準出力へ生成する。上の各 finding は `cases.jsonl` の同じ case 名で確認できる。証跡一覧・サイズ・SHA-256 は `manifest.json` を参照。

### 独立反証確認と実 Engine 追試

別担当 S4（immutable carrier の同名 binding 混同）と S7（escaped destructive function）を、この担当の公開 build API consumer でも 8 ケース再実行した。Default/Exact の不一致、Safe+Exact の一致、原因 pass を無効化した場合の復旧がすべて一致した。全式は有限値であり、許容された subtype 変更では説明できない。証跡は `cross-scope-cases.jsonl` / `cross-scope-results.jsonl`。新しい finding 数には加算していない。

root 担当は本ファイルの 12 再現例を実 WASM Engine Vehicle（float input、property、128×64 raster）でも SourceMap OFF/ON の **24 比較**として独立追試し、元コードのエラー 0 件、生成後の差分 24 件を確認した。詳しくは [wasm-game-api.jsonl](minify-review-20261012/evidence/root/wasm-game-api.jsonl)。本担当の呼び出し列 mock と実 Engine の追試は区別して記録している。

保存した `run.py` の release 再実行も成功し、12 件すべての生成コード・元の観測値・compile 成否が保存記録と一致した。

## 実行コマンド・制約・保存の確認

Native 既存ゲートの実行環境は `CARGO_TARGET_DIR=/tmp/cargo-targets/storm-lua-engine/minify-review-20261012`, `CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_INCREMENTAL=0`, `CARGO_BUILD_JOBS=4`。Cargo は指定の保護 wrapper を使用した。sandbox 内の重複 mount 表示で wrapper が拒否したため、実際の tmpfs を確認して同じ wrapper を sandbox 外で実行した。通常の root filesystem へ Cargo cache を逃がしていない。

```bash
cargo test --workspace --all-features --locked
node --test packages/lua-engine/tests/wasm/*.test.mjs
```

実際には上記コマンドを監査対象 checkout から実行し、ログと exit code を保存した。Native は `v031-workspace-tests.log/.rc`、WASM は `wasm-all-tests.log/.rc`。875 / 91 はこの実行の集計で、以前の別セッションの数値を流用していない。保存済みの consumer 再現手順も再実行し、root 63 件と numeric 160 件は timing を除いて同じ結果、API 12 例も再現した。

初期の旧 checkout の全 workspace コマンドは、その checkout の未コミット `metadata_tag` 追加に既存 conformance initializer が追随しておらず compile error になった。`workspace-tests.log/.rc` はこの対象違いの記録で、v0.3.1 の失敗・追加 finding に算入していない。検証途中の harness 側の JSON 区切り・JS I/O フィールド指定は修正して再実行し、製品不具合に数えていない。

単独 pass と公開 full pipeline、mock host と実 Engine Vehicle、Native Lua と WASM、Source Map 整合と実行意味論、原文が有効なケースと深さ上限超過を区別した。全 pass / 全入力の正当性証明、外部 consumer 全体の UI、実ゲーム本体、公開 release 再生成は対象外。製品コードを修正していないため、修正版の完成や公開可能性を今回の結果から主張しない。

レビュー成果物の一覧・サイズ・SHA-256 は [manifest.json](minify-review-20261012/manifest.json)。`/tmp` の実行 binary や cache が消えても、保存した harness / generator / 入力から再生成できる。ルートディスクの残量は作業前・途中・終了時に確認し、4 GiB の下限を維持した。
