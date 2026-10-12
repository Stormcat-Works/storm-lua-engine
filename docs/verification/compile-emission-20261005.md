# v0.3.1 コンパイル印字・名前保存の追加高速化

2026-10-05。基準は直前の検証済み **`627f8d1`**。今回の製品実装は **`f3e5c8769d987c4133df003cee078bee90c0c364`**。それ以前の解析共有・API別名候補の削減・数値表記の改善は基準に含まれており、今回の効果へ重ねて計上しない。

**全探索の成功4,054本では、Mapなしの平均が1.9%、Mapありが0.7%短縮した。** 今回の平均はMapなし49.288ms、Mapあり56.076ms。平均時間比は1.138倍。ただし、平均比を個別の最大1.2倍目標の達成判定には使わない。

[計画](../design/compile-emission-v031.md) / [比較元の記録](compile-latency-20261005.md) / [機械記録](compile-emission-20261005.json)。

## 1. 実装したこと

### Printerの一時文字列を借用する

識別子・演算子・表記の変わらない数値と二重引用符付き文字列を、immutableなASTから借用する。文字列を親へ組み合わせるまで不要な所有Stringを作らず、返却される最終codeは従来どおり所有Stringにする。関数を抜けた後の一時変数や変更できるASTへ参照を残さない。

演算子の固定個数の組み立てはstack上の配列を使い、引数・return・代入・テーブル等は直ちに消費するiteratorから連結する。各片のための一時Fragment配列を避ける。block/elseifの評価順、空文、改行カウント、先頭括弧のセミコロン、各emissionの順序と位置は維持する。

引用符処理は、従来と同じ短い安全な表記選択を使う。制御文字・エスケープ・長括弧・不正入力・Unicode・同長時のdouble quote優先を変えない。変更なしと確認できる二重引用符のときだけ借用し、公開qstr APIはStringを返す。

### 整数リテラルの完全走査を二度行わない

整数リテラルは最初のbyteで許容される先頭を確認した後、標準の整数parserへ残りのdigit・範囲の検査を委ねる。先に全てのdigitを検査してから、同じ文字列を整数parserで再走査する無駄を減らした。

受理する十進/16進の構文、先頭の符号、二重符号の拒否、overflow、hex bit pattern、従来の境界値の扱いは維持する。これは文法の緩和や数値精度の変更ではない。独立した従来実装をテストに残し、端点・不正な表記・長い入力と32,768個の疑似ランダムu64から作った複数表記を比較する。

### 短縮名の所有文字列と重複集合を減らす

書き換えない固定グローバル名は元ASTから借用し、1文字などの既存の静的短名候補は、その借用状態のまま利用する。トップレベルのローカル名は同じ列挙器が一度ずつ生成するので、既出ローカル名を別のHashSetへ重複保存しない。グローバルとの衝突判定、外側scopeの捕捉、生存区間、候補順と同点処理は維持する。

旧方式を独立したテスト参照へ残し、全67パスの入力/変換後と最大512bindingのケースで、割当名・AST・NodeId/SymbolId・全由来の一致を検査した。公開APIやschemaを変更せず、新しい永続キャッシュや外部依存を加えていない。

## 2. 計測条件

各比較は同じNode/WASMプロセスで旧版/新版とMap有無の順序を交互にする。原文のhashと基準WASMを固定し、WASM初期化・原文I/O・事前GC・外部validatorは時間外にする。Mapありでは完全なSource Map v3と詳細理由のJSONが返るまでをcompile時間に含める。

実コーパスは4,155原文、成功組8,182件を全探索と8192目標で別集計する。各variant1回の広い観測であり、個別入力の精密な最大時間保証ではない。22原文×2設定の44シーンは小入力21回、大入力7回、固定21原文の部分集合は3回の反復を行う。初回3%以上の負の観測と所定の対照は追加のwarmup・反復で再確認し、初回結果を置換しない。

## 3. 全コーパス

単位ms/原文、成功した同じ原文の平均。

| 条件 | 本数 | 旧Mapなし | 新Mapなし | 旧Mapあり | 新Mapあり | 新しい比率 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| exhaustive / all-success | 4,054 | 50.246 | 49.288 | 56.499 | 56.076 | 1.138 |
| target8192 / all-success | 4,128 | 5.409 | 5.372 | 6.213 | 6.173 | 1.149 |
| target8192 / source-checkpoint | 4,089 | 0.375 | 0.356 | 0.481 | 0.500 | 1.407 |

比率の分母は同じ新実装のMapなしである。普通の短縮を遅くする待機や余分な検査は追加しない。通常側がより速くなれば、Mapありの絶対時間が減っても比率は上がり得る。今回も絶対時間・追加時間差・比率を別に記録する。

## 4. 反復シーンと残る条件

| シーン・設定 | 旧Mapなしms | 新Mapなしms | 旧Mapありms | 新Mapありms | 新比率 |
| --- | ---: | ---: | ---: | ---: | ---: |
| empty / exhaustive | 0.511 | 0.514 | 0.577 | 0.571 | 1.111 |
| empty / target8192 | 0.173 | 0.177 | 0.239 | 0.253 | 1.433 |
| tiny-callback / exhaustive | 0.717 | 0.721 | 0.842 | 0.855 | 1.186 |
| tiny-callback / target8192 | 0.175 | 0.184 | 0.243 | 0.248 | 1.348 |
| dense-lexical / exhaustive | 1.496 | 1.535 | 7.533 | 7.503 | 4.888 |
| dense-lexical / target8192 | 1.494 | 1.558 | 7.526 | 7.450 | 4.782 |
| rw-23 / exhaustive | 21.547 | 21.812 | 25.341 | 25.281 | 1.159 |
| rw-23 / target8192 | 0.349 | 0.352 | 0.443 | 0.464 | 1.319 |
| rw-02 / exhaustive | 686.401 | 700.459 | 750.606 | 756.378 | 1.080 |
| rw-02 / target8192 | 1.253 | 1.257 | 1.663 | 1.673 | 1.332 |
| vehicle-control / exhaustive | 1,180.007 | 1,188.831 | 1,332.306 | 1,325.775 | 1.115 |
| vehicle-control / target8192 | 191.556 | 191.230 | 231.205 | 232.873 | 1.218 |
| draw-4096 / exhaustive | 1,075.360 | 1,046.902 | 1,856.024 | 1,880.364 | 1.796 |
| draw-4096 / target8192 | 1,071.178 | 1,064.260 | 1,865.756 | 1,908.215 | 1.793 |
| corpus-4028de71b17c / exhaustive | 15.951 | 15.703 | 73.737 | 72.852 | 4.639 |
| corpus-4028de71b17c / target8192 | 15.949 | 15.749 | 74.427 | 73.313 | 4.655 |

成功40条件中、1.2倍以下は11、1.1倍以下は1。最大の中央値比は4.888倍。Mapなしが速くなった条件は13、Mapありは17。この有限の観測を、あらゆる入力が必ず高速化した証明と扱わない。最大1.2倍の目標は引き続き未達。

特に字句短縮・原文返却は今回の構造Printerや名前割当を通らない場合がある。その経路の小さな増減を今回の構造最適化の成果と断定しない。Map固有の出力・内容hash・位置対応の固定費は残る。

### 負の観測の追加確認

| シーン | 設定 | 反復数 | Mapなし短縮率 | Mapあり短縮率 | 比率 |
| --- | --- | ---: | ---: | ---: | ---: |
| empty | exhaustive | 31 | -1.5% | -2.2% | 1.179 |
| empty | target8192 | 31 | -4.3% | 5.9% | 1.416 |
| newline | exhaustive | 31 | -1.8% | -1.3% | 1.175 |
| newline | target8192 | 31 | 6.9% | 3.9% | 1.520 |
| comments | exhaustive | 31 | 4.1% | -0.5% | 1.226 |
| comments | target8192 | 31 | -0.1% | 2.7% | 1.492 |
| tiny-callback | exhaustive | 31 | -3.1% | 4.2% | 1.110 |
| tiny-callback | target8192 | 31 | 2.2% | 3.1% | 1.425 |
| fold-expression | exhaustive | 31 | 2.6% | 1.8% | 1.240 |
| fold-expression | target8192 | 31 | -5.8% | -6.1% | 1.340 |
| exact-safe | exhaustive | 31 | 6.8% | 0.9% | 1.335 |
| exact-safe | target8192 | 31 | 5.5% | -1.0% | 1.397 |
| extended-lexical | exhaustive | 31 | 0.6% | 2.1% | 1.556 |
| extended-lexical | target8192 | 31 | 7.0% | 1.3% | 1.565 |
| env-lexical | exhaustive | 31 | 10.5% | 6.4% | 1.665 |
| env-lexical | target8192 | 31 | 0.2% | 9.1% | 1.424 |
| dense-lexical | exhaustive | 31 | -3.1% | -0.0% | 4.798 |
| dense-lexical | target8192 | 31 | -12.6% | -0.9% | 4.250 |
| many-plain-blocks | exhaustive | 31 | 0.8% | -0.4% | 1.162 |
| many-plain-blocks | target8192 | 31 | 0.4% | 2.1% | 1.436 |
| many-scratch-blocks | exhaustive | 31 | -0.1% | 0.9% | 1.119 |
| many-scratch-blocks | target8192 | 31 | -1.2% | 1.3% | 1.421 |
| rw-23 | exhaustive | 31 | 1.7% | 2.2% | 1.166 |
| rw-23 | target8192 | 31 | 5.1% | 2.2% | 1.296 |
| corpus-a37299b70f1f | exhaustive | 31 | 5.9% | 0.3% | 4.559 |
| corpus-a37299b70f1f | target8192 | 31 | -1.8% | -1.7% | 4.543 |

共有VMのJIT・GC・同時負荷の変動がある。再確認で負の差が残った条件もそのまま示す。分母を意図的に遅くした変更はないが、全シーンで統計的に有意な改善を証明したとは言わない。

## 5. analyze・build・LifeBoat

今回直接変えたのは印字・数値読取り・名前保存であり、非短縮のanalyze全体を書き換えたものではない。同じ固定module群を旧版と比較し、完全な返却値（passの時間とmap生成者情報のみ除外）を確認した。初回各9反復と、負の観測に対する追加31反復はローカルJSONに保存する。

| 入力 | 操作 | 旧ms | 新ms | 短縮率 |
| --- | --- | ---: | ---: | ---: |
| rw-23 | analyze | 0.295 | 0.279 | 5.2% |
| rw-23 | link | 0.360 | 0.363 | -0.8% |
| rw-23 | link-map | 0.278 | 0.292 | -5.3% |
| rw-23 | build-minify | 17.224 | 17.220 | 0.0% |
| rw-23 | build-minify-map | 16.138 | 16.046 | 0.6% |
| rw-02 | analyze | 1.949 | 1.939 | 0.5% |
| vehicle-control | analyze | 1.966 | 2.041 | -3.8% |
| vehicle-control | link | 2.899 | 2.906 | -0.3% |
| vehicle-control | link-map | 2.277 | 2.318 | -1.8% |
| draw-4096 | analyze | 10.003 | 10.119 | -1.2% |
| draw-4096 | link | 19.103 | 19.056 | 0.2% |
| draw-4096 | link-map | 13.020 | 13.063 | -0.3% |
| project-1-modules | analyze | 0.112 | 0.121 | -8.5% |
| project-1-modules | link | 0.200 | 0.182 | 9.1% |
| project-1-modules | link-map | 0.206 | 0.202 | 2.2% |
| project-1-modules | build-minify | 2.193 | 2.186 | 0.3% |
| project-1-modules | build-minify-map | 2.763 | 2.776 | -0.5% |
| project-16-modules | analyze | 0.255 | 0.251 | 1.9% |
| project-16-modules | link | 0.410 | 0.410 | -0.0% |
| project-16-modules | link-map | 0.563 | 0.564 | -0.1% |
| project-16-modules | build-minify | 9.239 | 8.932 | 3.3% |
| project-16-modules | build-minify-map | 11.837 | 11.409 | 3.6% |
| project-64-modules | analyze | 0.643 | 0.630 | 2.0% |
| project-64-modules | link | 1.418 | 1.442 | -1.7% |
| project-64-modules | link-map | 2.884 | 2.973 | -3.1% |
| lifeboat-16-includes | analyze | 0.225 | 0.285 | -26.6% |
| lifeboat-16-includes | link | 0.625 | 0.643 | -2.9% |
| lifeboat-16-includes | link-map | 0.642 | 0.641 | 0.2% |

`link`は非短縮buildで従来の標準mapを返す。`link-map`は詳細x_stormを含むので、普通のMapなし/ありとは区別する。診断で失敗する入力は成功側の平均へ混ぜない。

## 6. 正確性と品質

- Native workspace **841件**、SDK **30件**、実WASM **83件**、Playground/CLI **38件**が成功。
- 固定240設定、未達target240件、reader相互読取り480回、Worker混成6経路、通常/LifeBoat8ビルドを検証。
- 実コーパス8,182成功組で生成Lua・診断・全検索統計・完全なmapが一致。map比較から除いたのはproducerと、それに依存するintegrityのみ。
- 既知の全探索WASMトラップ7原文は、新旧56条件で同じ結果を確認。本作業で修正したものとは扱わない。
- fmt、default、clippy、rustdoc、architecture、Python、画面fixture、権利表示、梱包済みSDK consumerが成功。

Browser pluginがないため既存Playwrightを使用し、localの専用routeでChromium・Firefox・WebKit各16例、Map双方向選択・理由・保存復元・実VMの停止/ログ/エラーを確認した。console/page errorは0件。デスクトップと390px幅のスクリーンショットを確認する。UI/CSPは変更しない。hostedの3 OS CIは未実行で、Native件数はローカルLinuxの結果である。

## 7. 保存状態

ブランチは`feat/minify-performance-v0.3.1`。実装commitは次のとおり。

- `f3e5c87`: perf: retain borrowed fixed names during scope allocation
- `5736d0b`: perf: validate integer literal digits once per parse
- `3f342dc`: perf: borrow unchanged text while composing compiler output

基準WASM: `bcdb1d5ba314a240655115f3fecf84797641892f6e761032b4e3a4fe67fe7eb9`。今回WASM: `4703eda1dfb2b4e57d47e37f025864062b631b4a3448eafe3a93e7a0ef7bb879`。最終実装のproducer.dirty=false。開発版のパッケージ表記は0.3.0のまま、revisionで公開済み0.3.0と区別する。

commitのみ行い、push・main/release・タグ・npm・本番配備は変更していない。private Luaとmap本文は元の検証領域に保持し、公開記録・共有ファイルへ含めない。raw samples・CSV・再実行scriptは同じlocal-validationへ保存した。
