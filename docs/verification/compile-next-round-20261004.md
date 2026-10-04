# v0.3.1 コンパイル全経路・追加高速化 — 2026-10-04

Mapありの追加倍率を抑えつつ、Mapなしの絶対時間も短縮するため、パーサー、候補の比較キー、効果解析、名前短縮、パス判定、マップ整形、字句短縮の所有権を改善した。**全探索の成功4,054本では、Mapなし平均が3.0%短縮、Mapあり平均が2.2%短縮した。**

**最大1.2倍、理想1.1倍の条件は未達のままである。** 成功40シーン中、1.2以下は11、1.1以下は3。最大の中央値比は5.021倍。平均比だけで全シーンの目標を満たしたと扱わない。

[計画](../design/compile-next-round-v031.md) / [前回の全経路改善](compile-all-scenes-20261004.md) / [機械記録](compile-next-round-20261004.json)。ブランチは`feat/minify-performance-v0.3.1`。測定した製品実装は`a400c55dd145ca7de255153374cdc088aaeb6313`。commitのみ行い、push・公開・main/release更新は行っていない。

## 1. 比較元と今回の範囲

比較元は公開v0.3.0ではなく、前回報告した全経路改善の実装`1f9d58e`。前回の改善を今回の効果へ再加算しない。以前の一時ディレクトリに置いた基準WASMが失われていたため、同じcommit・build手順・toolchainで再構築し、**SHA-256が当時の基準バイナリと完全一致**することを確認した。

途中まで保存されていた前の実行のシーン出力は未完了として保持し、最終値へ混ぜない。以前の出力保存領域不足は環境の失敗として記録されており、コンパイラの正確性失敗と区別する。

生成Lua、候補集合・探索順・同点判定、数値契約、診断、標準Source Mapとx_storm schema1を維持した。Mapの精度・理由・関連元を削らず、完成したmapを返すまでをcompile時間に含める。Mapなしを意図的に遅くする処理、時間で探索を打ち切る処理、入力を跨ぐ永続的な結果cacheは追加していない。

## 2. 採用した改善

### 構文解析と構造比較の割り当て

パーサーは一度保持した入力を参照し、tokenを文字列の複製ではなく種類・位置・範囲として持つ。公開された所有トークンやParserの契約は維持し、ASTのノード順序、元位置、構文エラーを一致させる。

候補や式の同一性を調べるcanonical AST keyは、子ノードごとのString生成と結合をやめ、一つの出力バッファへ順番に書き込む。keyの文字列自体は従来と同じであり、ハッシュだけの近似比較や衝突を許す重複排除には置き換えていない。これらはMapの有無によらず使われる処理である。

### 効果解析と名前短縮

EffectAnalyzerは、同じ組込みAPI rootの名前を出現するたびに再確保しない。関数要約の更新では、実際に関数があるbindingだけを順番どおり確認し、疎な配列全体の反復走査を抑える。fixpointの順序と上限は変更しない。

さらに、EffectAnalyzer初期化の全ノード一覧を一時Vecへ保存して二回走査する処理をなくした。到達可能なノードを同じ順で一度だけ走査し、書込み回数と組込みroot候補を集める。後から書き込まれるrootは、alias伝播を始める前に取り除く。部分root、同一NodeIdの複数出現、到達しないarena slotを含む旧方式との比較を追加し、arena全体のwrite数へ置き換えない。

真偽だけを求める安全性・純粋性の問い合わせでは、返却先で観測されないread-setの配列を作らない。公開の完全なEffectsには従来どおり必要なread/write集合を返し、mutableなsummaryやstable_reads設定が変わっても古い結果を返すcacheを入れていない。

名前短縮の捕捉binding集合は、順序に意味を持たないHashSetから、ソート済みのコンパクトなID配列へ変えた。名前の選択順・生存区間・外側の変数との競合は独立した旧方式と比較する。パスIDの判定も唯一の正本レジストリから生成し、無効・未知の設定を受理する抜け道は作らない。

### マップと字句短縮

標準Source Mapは、既に順序付けられたtoken・行・区間境界を併合し、再度大きなアンカー集合を作ってsortしない。元テキストと実効設定を最終JSONまで借用し、中間Valueツリーへの複製を抑える。任意のcaller textと数値は正規のエンコーダーを通す。

保守的な字句短縮のCandidateJobは消費される値である。その完成済みコードとマップ配列を次段へ移す際、全体をcloneせず所有権を渡す。移動後の元バッファpointer・完全なマップ・検証コールバックの呼出と拒否結果を専用テストで確認した。

元範囲を複数まとめる二つの呼出箇所では、既存の順序を維持するbatch操作を使う。後述のscalar処理への汎用hash索引はWASM実測で遅くなったため、採用していない。

## 3. 採用しなかった案

**結果の一括JSONブリッジ**は小入力のMapなしを遅くし、安定した絶対時間改善を示さなかったため撤回した。欠落フィールド・順序・非有限数・負のゼロ等の回帰テストは既存ブリッジにも必要なので残した。

**scalarな由来集合の一時hash索引**も撤回した。Nativeのsampleではこの関数が重く見えたが、同じ入力のWASM比較では描画例のMapありが約26〜28%遅くなった。元のallocation-freeなscalar検索へ戻し、修正後を別に再測定した。NativeのprofileだけでWASMも速くなるとは扱わず、悪化した試行を最終結果へ混ぜない。

**最終マップの追加借用view**も、別候補として試したが採用しなかった。元の配列と名前をさらに借用し、schema1の全情報を維持する構造だったが、同じWASM条件の比較では安定した追加速度改善を示さなかった。専用の内部表現を増やす価値が確認できなかったため、該当二ファイルだけを元へ戻した。参照テスト、試行差分、採用しなかった測定もローカル検証領域に残した。最終結果にその試行を混ぜない。

## 4. 実コーパスの比較

固定4,155原文について、変更前後とMapなし/ありを同じNodeプロセスで順序を入れ替えて比較した。各variant1回の広い観測。失敗と既知trapは成功の平均へ含めない。全探索と8192目標は別々の母集団である。単位ms/原文、平均値。

| 条件 | 本数 | 旧Mapなし | 新Mapなし | なし短縮率 | 旧Mapあり | 新Mapあり | あり短縮率 | 新Map比 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 全探索・全成功 | 4,054 | 56.117 | 54.428 | 3.0% | 62.321 | 60.925 | 2.2% | 1.119倍 |
| 全探索・車両由来 | 3,950 | 51.431 | 49.955 | 2.9% | 57.209 | 55.991 | 2.1% | 1.121倍 |
| 8192目標・全成功 | 4,128 | 6.107 | 5.752 | 5.8% | 6.907 | 6.620 | 4.1% | 1.151倍 |
| 8192目標・原文返却 | 4,089 | 0.383 | 0.349 | 8.8% | 0.542 | 0.502 | 7.3% | 1.440倍 |

全探索の平均追加時間は **6.203→6.497ms**。平均時間比は **1.111→1.119倍**だった。Mapなしも速くなると分母が小さくなるため、絶対時間・追加時間・倍率は別々に記録する。分母は常に同じ変更後実装のMapなしである。

実コーパス全体の単発測定だけで個別の最大時間を保証しない。別に21原文を、以前の時間分位点と高負荷・高倍率の選定規則で選び、各3反復した。この集合は重い入力を意図的に含み、コーパス全体の平均とは分けて保存する。

## 5. 空入力から巨大入力までの反復

22原文×2経路=44条件、うち成功40条件と、診断で失敗することが正しい4条件。10KB超は7反復、それ以外は21反復。WASM初期化・原文I/O・事前GC・外部マップ再検証を時間外にし、各入力にも同じ2回のwarmupを適用した。Map ONでは最終JSONが返るまでを含める。単位ms、各variantの中央値。

| シーン・経路 | 旧Mapなし | 新Mapなし | 旧Mapあり | 新Mapあり | 新比 |
| --- | ---: | ---: | ---: | ---: | ---: |
| empty / target8192 | 0.122 | 0.124 | 0.220 | 0.179 | **1.440倍** |
| tiny-callback / target8192 | 0.128 | 0.133 | 0.212 | 0.190 | **1.427倍** |
| unicode-crlf / target8192 | 0.153 | 0.160 | 0.264 | 0.211 | **1.319倍** |
| rw-23 / exhaustive | 19.161 | 18.161 | 21.512 | 21.347 | **1.175倍** |
| rw-02 / target8192 | 1.328 | 1.249 | 1.938 | 1.738 | **1.391倍** |
| rw-02 / exhaustive | 783.920 | 755.560 | 843.107 | 821.298 | **1.087倍** |
| vehicle-control / exhaustive | 1,375.700 | 1,347.164 | 1,537.441 | 1,499.343 | **1.113倍** |
| vehicle-control / target8192 | 210.466 | 206.747 | 252.992 | 246.564 | **1.193倍** |
| draw-4096 / exhaustive | 1,189.057 | 1,161.415 | 2,029.478 | 2,021.967 | **1.741倍** |
| draw-4096 / target8192 | 1,207.357 | 1,170.000 | 2,061.194 | 2,027.894 | **1.733倍** |
| dense-lexical / exhaustive | 1.412 | 1.238 | 4.801 | 5.469 | **4.417倍** |
| corpus-a37299b70f1f / exhaustive | 1.434 | 1.434 | 6.671 | 6.383 | **4.452倍** |
| corpus-4028de71b17c / exhaustive | 16.615 | 15.835 | 76.736 | 73.659 | **4.652倍** |

成功条件で、観測上Mapなしが速くなったのは **24/40**、Mapありが速くなったのは **35/40**。全条件・全sampleは削除せず保存し、どの入力でも必ず速くなったとは表現しない。

### 負の観測の追加確認

初回でどちらかのモードが3%以上遅くなった成功シーンを選び、対照としてempty・tiny-callback・rw-23を追加した。小入力では16warmup＋31反復、大入力では4warmup＋7反復で別の測定を行った。最初の値を後から置換していない。

| シーン・経路 | 初回なし短縮率 | 再測定なし短縮率 | 初回あり短縮率 | 再測定あり短縮率 | 再測定Map比 |
| --- | ---: | ---: | ---: | ---: | ---: |
| empty / exhaustive | -4.0% | 8.3% | 0.2% | 13.1% | 1.197倍 |
| empty / target8192 | -1.6% | 3.8% | 18.9% | 24.1% | 1.532倍 |
| comments / exhaustive | 4.7% | 6.9% | 4.5% | 14.0% | 1.196倍 |
| comments / target8192 | -6.4% | 2.6% | 14.7% | 20.0% | 1.551倍 |
| tiny-callback / exhaustive | -2.2% | 3.7% | 3.0% | 10.4% | 1.189倍 |
| tiny-callback / target8192 | -3.8% | 5.2% | 10.2% | 23.9% | 1.487倍 |
| fold-expression / exhaustive | -13.4% | 3.5% | 15.3% | 4.5% | 1.228倍 |
| fold-expression / target8192 | -3.3% | 4.0% | 14.0% | 21.0% | 1.409倍 |
| unicode-crlf / exhaustive | 14.1% | 0.8% | -0.5% | 6.6% | 1.162倍 |
| unicode-crlf / target8192 | -4.5% | 3.4% | 20.0% | 23.1% | 1.461倍 |
| property-runtime / exhaustive | -4.4% | -1.3% | 9.5% | 4.2% | 1.174倍 |
| property-runtime / target8192 | -10.2% | 6.0% | 4.6% | 21.2% | 1.462倍 |
| exact-safe / exhaustive | 0.9% | 4.5% | -10.3% | 4.0% | 1.300倍 |
| exact-safe / target8192 | 1.4% | 4.6% | 9.8% | 22.6% | 1.409倍 |
| extended-lexical / exhaustive | -0.4% | -0.6% | 11.6% | 15.4% | 1.614倍 |
| extended-lexical / target8192 | -6.1% | -2.8% | 19.6% | 13.0% | 1.626倍 |
| dense-lexical / exhaustive | 12.3% | 4.5% | -13.9% | 5.2% | 4.839倍 |
| dense-lexical / target8192 | -9.4% | 1.3% | -12.1% | 5.7% | 4.500倍 |
| many-plain-blocks / exhaustive | -1.2% | 4.4% | 6.7% | 4.9% | 1.130倍 |
| many-plain-blocks / target8192 | 0.7% | 5.2% | -3.5% | 14.9% | 1.399倍 |
| rw-23 / exhaustive | 5.2% | 6.0% | 0.8% | 4.8% | 1.162倍 |
| rw-23 / target8192 | 3.3% | 6.5% | 3.4% | 15.0% | 1.410倍 |

追加確認でも負の差が残る場合、それを未解消の観測として扱う。sub-msの相対差と、数秒の処理での差は絶対時間も併せて読む。共有VMの負荷・JIT・GCのばらつきが残り、非常に小さい差を普遍的な性能差と断定しない。

## 6. analyze・link・通常/LifeBoat build

既存の同一module/ソースを使い、4回warmup＋9反復、変更前後を交互に実行した。結果・診断・マップ全体の一致を確認する。`link`は非短縮buildであり、従来の標準mapは返す。`link-map`は詳細拡張も生成するので、単純なmap OFF/ONとは異なる。成功と診断失敗を別に残す。

| 入力 | 操作 | 成功 | 旧ms | 新ms | 短縮率 |
| --- | --- | --- | ---: | ---: | ---: |
| rw-23 | analyze | yes | 0.433 | 0.408 | 5.9% |
| rw-23 | link | yes | 0.372 | 0.357 | 4.0% |
| rw-23 | link-map | yes | 0.366 | 0.314 | 14.3% |
| rw-23 | build-minify | yes | 17.217 | 19.762 | -14.8% |
| rw-23 | build-minify-map | yes | 16.959 | 16.173 | 4.6% |
| rw-02 | analyze | yes | 2.211 | 2.029 | 8.2% |
| rw-02 | link | 診断で失敗 | 1.567 | 1.481 | 5.5% |
| rw-02 | link-map | 診断で失敗 | 1.491 | 1.428 | 4.2% |
| vehicle-control | analyze | yes | 2.092 | 1.943 | 7.1% |
| vehicle-control | link | yes | 3.030 | 2.910 | 4.0% |
| vehicle-control | link-map | yes | 2.769 | 2.486 | 10.2% |
| draw-4096 | analyze | yes | 10.004 | 10.020 | -0.2% |
| draw-4096 | link | yes | 19.105 | 19.155 | -0.3% |
| draw-4096 | link-map | yes | 15.325 | 14.259 | 7.0% |
| project-1-modules | analyze | yes | 0.105 | 0.107 | -2.6% |
| project-1-modules | link | yes | 0.151 | 0.145 | 4.0% |
| project-1-modules | link-map | yes | 0.215 | 0.187 | 12.9% |
| project-1-modules | build-minify | yes | 2.441 | 2.404 | 1.5% |
| project-1-modules | build-minify-map | yes | 2.992 | 2.878 | 3.8% |
| project-16-modules | analyze | yes | 0.321 | 0.294 | 8.4% |
| project-16-modules | link | yes | 0.428 | 0.429 | -0.2% |
| project-16-modules | link-map | yes | 0.604 | 0.575 | 4.7% |
| project-16-modules | build-minify | yes | 9.828 | 9.496 | 3.4% |
| project-16-modules | build-minify-map | yes | 12.477 | 12.180 | 2.4% |
| project-64-modules | analyze | yes | 0.731 | 0.760 | -3.9% |
| project-64-modules | link | yes | 1.495 | 1.457 | 2.5% |
| project-64-modules | link-map | yes | 3.135 | 3.037 | 3.1% |
| lifeboat-16-includes | analyze | yes | 0.300 | 0.313 | -4.4% |
| lifeboat-16-includes | link | yes | 0.646 | 0.618 | 4.4% |
| lifeboat-16-includes | link-map | yes | 0.784 | 0.761 | 2.9% |

build/analyzeの改善率はminify全体の値と足し合わせない。小さなprojectの負の観測がある場合も表から省かない。パーサー等の共通基盤の変更を、すべてのprojectで同じ速度改善が出るという保証にはしない。

### build/analyzeの再確認

初回で3%以上の悪化があったproject群と固定の小project対照を、16warmup＋31反復で再確認した。初回の値は上表に残す。

| 入力 | 操作 | 初回短縮率 | 再確認旧ms | 再確認新ms | 再確認短縮率 |
| --- | --- | ---: | ---: | ---: | ---: |
| rw-23 | analyze | 5.9% | 0.236 | 0.229 | 2.8% |
| rw-23 | link | 4.0% | 0.271 | 0.258 | 5.0% |
| rw-23 | link-map | 14.3% | 0.301 | 0.255 | 15.4% |
| rw-23 | build-minify | -14.8% | 14.857 | 15.003 | -1.0% |
| rw-23 | build-minify-map | 4.6% | 17.221 | 17.114 | 0.6% |
| project-1-modules | analyze | -2.6% | 0.131 | 0.153 | -17.1% |
| project-1-modules | link | 4.0% | 0.223 | 0.224 | -0.3% |
| project-1-modules | link-map | 12.9% | 0.287 | 0.268 | 6.4% |
| project-1-modules | build-minify | 1.5% | 2.562 | 2.500 | 2.4% |
| project-1-modules | build-minify-map | 3.8% | 4.588 | 4.356 | 5.0% |
| project-16-modules | analyze | 8.4% | 0.413 | 0.376 | 8.9% |
| project-16-modules | link | -0.2% | 0.578 | 0.547 | 5.4% |
| project-16-modules | link-map | 4.7% | 0.956 | 0.871 | 8.9% |
| project-16-modules | build-minify | 3.4% | 10.747 | 10.493 | 2.4% |
| project-16-modules | build-minify-map | 2.4% | 12.977 | 12.904 | 0.6% |
| project-64-modules | analyze | -3.9% | 0.720 | 0.687 | 4.7% |
| project-64-modules | link | 2.5% | 1.614 | 1.570 | 2.7% |
| project-64-modules | link-map | 3.1% | 3.182 | 3.077 | 3.3% |
| lifeboat-16-includes | analyze | -4.4% | 0.330 | 0.323 | 1.9% |
| lifeboat-16-includes | link | 4.4% | 0.763 | 0.713 | 6.5% |
| lifeboat-16-includes | link-map | 2.9% | 0.785 | 0.796 | -1.3% |


## 7. 正確性と品質ゲート

固定30入力×8設定=240構成で、コード・検索統計・診断・全マップの一致を確認した。target未達240件、相互reader480回、新旧混成Worker6経路、通常/LifeBoat8ビルドも検証した。

実コーパスの8,182成功組では、コード・診断・候補/パス情報・全map JSONが一致した。マップ比較から外すのは生成版のproducerと、それに依存するintegrityだけ。位置・精度・理由の根拠/値・関連元・inline context・copy・除去記録を間引かない。既知の全探索WASM trap7原文は56条件で別に新旧確認し、本ラウンドで修正したとは扱わない。

| 検査 | 結果 |
| --- | --- |
| Native workspace | **819件成功** |
| SDK | **30件成功** |
| 実WASM | **83件成功** |
| Playground/CLI | **38件成功** |
| Chromium・Firefox・WebKit | 各16例＋Source Mapの実操作が成功 |
| fmt/default/clippy/rustdoc/architecture/Python/権利表示/画面fixture | 全成功 |
| 梱包済みSDKの独立導入 | 成功 |

ブラウザーは既存Playwrightを使用した。対象flowは「専用route → 確認例 → Map作成 → 原文/生成Luaの選択 → 理由・関連元 → 保存/再import → 実VMの停止・step・log・error」。意味のある初期画面、URL/title、console/page error 0を確認し、デスクトップと390px幅のスクリーンショットも確認した。UI/CSPは変更していない。Browser pluginは未提供のため使用していない。

Native件数はローカルLinux。hostedの3 OS CIと本番配備は実施していない。

## 8. コミットと再現識別子

| commit | 内容 |
| --- | --- |
| `10c47e5` | docs: plan another cross-path compilation performance round |
| `f463b72` | perf: retain parser token ranges instead of cloned token strings |
| `87185cd` | perf: stream canonical AST keys into a single output buffer |
| `b8efb75` | perf: avoid repeated builtin root allocation and sparse summary scans |
| `4c6687a` | perf: borrow map metadata and merge ordered source anchors directly |
| `a5bfec9` | perf: omit unobserved read sets from boolean effect queries |
| `9960588` | perf: use compact capture sets during short-name assignment |
| `d34a563` | perf: compile exact pass membership from the canonical registry |
| `794fcff` | test: preserve compiler result field order and numeric representations |
| `4491050` | perf: preserve ordered provenance unions without quadratic scans |
| `911754c` | perf: transfer completed lexical candidate buffers without cloning |
| `fdf1707` | perf: retain allocation-free scalar joins after WASM comparison |
| `a400c55` | perf: seed builtin effects without materializing preorder nodes |

- 基準compiler WASM SHA-256: `a64a9688544465205c323b56a76f6f94600ac4b1adb251f3f2b6c8f4b1f934cc`
- 今回compiler WASM SHA-256: `bbaedaa3c10952ec1a60e513dd51f16adf39ef5b3de18e16b89f154c7e21ad3a`
- 実装commit: `a400c55dd145ca7de255153374cdc088aaeb6313`
- producer.dirty: false

計測script、固定した新旧WASM、raw JSONL/gzip、反復sampleとCSVはローカル検証領域へ保存した。以前の一時領域消失によって測定継続が難しくならないよう、今回の再現scriptと固定binaryは保持している。公開資料へ私有Lua原文・sourcesContent・個人の絶対パスを入れない。

## 9. 残る目標

Mapなし・あり双方の絶対時間と、Mapの追加費用を引き続き別々に追う。最大1.2倍、理想1.1倍の目標は平均時間比へ置き換えない。特に原文返却・保守的字句短縮の固定費、大きな描画の由来処理、負の時間差が残る経路は次の改善対象である。

「Mapなしを遅くすれば倍率が下がる」「理由を減らせば生成量が減る」「JSONを後で生成すればcompileが速く見える」という手段は採用しない。全シーンでの絶対時間の短縮を同時に狙い、次の改修も新しい実装同士の同条件比較で判定する。v0.3.1は開発中であり、公開版0.3.0は変更していない。
