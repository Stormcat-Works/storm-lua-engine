# minify レビュー指摘の修正・実装レポート

作業日: 2026-10-13 JST。対象: `minify-review-20261012.md` の R1–R6、N1–N13、S1–S9、F1–F7（35件）、および未存在tableキー、color subset探索、string.format候補生成の性能問題。

## 結果と作業場所

35件の原因に対する修正を実装し、保存済みの再現例、追加の境界例、Native/WASM、Source Mapの回帰を確認した。**生成文字数が増えるケースはある。** 不正だった出力を正す増加だけでなく、従来も正しかった巨大整数の等差列で公開APIの生成物が **79→342文字（+263）** になるケースも確認した。詳細と全増加観測は後述する。

実装時のcheckoutはリポジトリ内の `.worktrees/minify-review-fixes`、保存先ブランチは `codex/minify-review-fixes`。基点は `5a168d63eda82c898af4dcc72f8c2c3f9a178e51`。元のレビュー対象 `.worktrees/metadata-source-map-fix` の未コミットmetadata修正を継承している。今回その修正を再実装したとは数えず、既存の回帰試験を維持した。修正実装・検証時点では元の作業ディレクトリと別ブランチの変更を保持し、commit、push、公開、既存リリースの差し替えは行っていない。2026-10-13のブランチ整理では変更前の履歴・差分をローカルに保存し、重複するmetadata修正をこのブランチへまとめてコミットする。push・公開・既存リリースの差し替えは対象に含めない。

この文書が実装結果の単一レポートである。機械可読の比較結果・ログ・実行consumer・ハッシュは同じ実装checkoutの `local-minify-fix-validation/` に保存した。保存先はext4でありtmpfsではない。Cargoキャッシュだけを規定の `/tmp/cargo-targets` に置き、必要な比較consumerとWASM配布物は永続領域にも保持した。

## 修正の対応表

パス名は、特記しなければ `crates/storm-lua-minify/src/passes/` 配下を指す。グローバルなpass設定の無効化で封印せず、誤る条件を解析・値表現・適用条件で修正した。

| ID | 実装内容・安全な適用条件 | 主な箇所 |
|---|---|---|
| R1 | Lua文字列をbyte列で評価。decimal/hex escape、Unicode escape、surrogate、ASCIIの`\z`を保持。UTF-8として扱う必要のある変換はデコード不可なら適用しない。 | syntax `source_tools.rs` / `numeric.rs`、文字列利用pass |
| R2 | long string先頭改行除去、CR/LF/CRLF/LFCRの正規化を共通byte decoderへ統一。 | syntax `numeric.rs` / `source_tools.rs` |
| R3 | 行コメントはCRでも終了。字句位置の改行ペアを1改行として数える。 | syntax `lexer.rs` |
| R4 | hex整数はu64 wrappingで読み、hex floatはsignificandとsticky bitからbinary64へ丸める。subnormal、ties-to-even、overflowも処理。 | syntax `numeric.rs` |
| R5 | Infinityを表す元literalを保持し、式形の数値と負数の優先順位をprinter/size双方で扱う。 | syntax `print.rs` / `size.rs` |
| R6 | parser再帰128、AST深さ256の診断を追加。6400段の括弧・単項式・左結合式・blockをNative/WASMで通常の失敗として返す。 | syntax `parser.rs` |
| N1 | i64 MIN / -1、MIN % -1をLuaのwrapping規則で評価しpanicを除去。 | `literal_folding.rs` |
| N2 | 負の除数のfloor division/moduloとfloat moduloの符号調整をLuaに合わせる。 | 同上 |
| N3 | 64bit整数bit演算、64以上のshift、逆方向shift、非整数operandのエラーを保持。 | 同上 |
| N4 | Int/Float/Bool/Nil/byte stringを区別。i64とf64比較を丸めず判定し、暗黙coercion・concat・math関数の不正なfoldを抑止。 | 同上 |
| N5 | 未証明の値から`+0`/`*1`等を消さない。数値化や型エラーを維持する。 | 同上 |
| N6 | Safeでfloat subtype、負のゼロ、inductionとboolean consumerのfloat literalを保持。 | `literal_folding.rs` / `induction_values.rs` / `boolean_numerics.rs` |
| N7 | quotient/remainder融合は非負で2^53以下と証明できる整数local、正の範囲内除数、単一更新に限定。正例を維持し不明値・overflow例は元の式を残す。 | `quotient_remainder.rs` |
| N8 | 一致した直前文自身のwrite/call/orderも、共通式再利用の障壁に含める。 | `signed_factoring.rs` |
| N9 | split-sign store消去は同blockで宣言されたlocalで、nested functionからcaptureされない場合に限定。 | `split_sign.rs` |
| N10 | 二乗変形は値の数値型と移動可能性を確認。副作用を持つoperand/factorを複製・削除しない。整数値は元のpowerと同じfloat計算へ変換してから積を作る。 | 同上 / `numeric_proof.rs` |
| N11 | carrierの数値証明を追加し、整数中間積のchecked arithmeticが成立しない候補を作らない。元の積と候補の残余積も確認し、overflowを新設・除去しない。 | `multiplicative_carriers.rs` / `coefficient_carriers.rs` / `numeric_proof.rs` |
| N12 | table constructorのkeyもexact経路に含める。known functionの戻り値、内部計算、外部入力、途中のbinding定義へ固定点でexact要求を伝播。key収集は各tableにつき1回。 | `numeric_literals.rs` |
| N13 | format widthのLua制限と候補出力長を割り当て前に確認。短縮されない巨大候補を作らず、元のformat呼び出しと実行時エラーを残す。 | `literal_folding.rs` |
| S1 | table引数のfieldをnested functionがcaptureする場合scalar化しない。生成parameter名もfreshにする。 | `tables.rs` |
| S2 | `function t.f`/`function t:f`をtable書き込みとして扱う。 | 同上 |
| S3 | 挿入先で再resolveし、元と同じ宣言・scope・bindingへ解決する場合のみimmutable式を再利用。 | `immutable_values.rs` |
| S4 | expression keyへBindingIdを含め、同じ字面のshadowed localを統合しない。 | `immutable_synthesis.rs` / `immutable_values.rs` |
| S5 | 元から存在するglobal、予約API名・callback名との衝突を除外。private callback localを勝手に公開しない。 | `root_globals.rs` |
| S6 | goto/labelがあるchunkでは線形dataflowを前提とする3変換を適用しない。構造化制御の正例は継続して変換。 | `final_stores.rs` / `single_use_forwarding.rs` / `function_globalization.rs` |
| S7 | helperの全readが直接callか確認し、alias escapeを除外。helper本体が出力先globalを直接・間接に観測/変更する場合も除外。 | `destructive_results.rs` |
| S8 | 静的table keyをbyte列/i64/f64に分け、同値なintegral floatだけi64へ正規化。NaNは証明しない。 | `tables.rs` |
| S9 | table/function/number等をprefix expressionに使う場合は括弧を出力。size計測も同じ規則。 | syntax `print.rs` / `size.rs` |
| F1 | 全interner語彙との衝突を避けるfresh allocatorを追加。API/color/helper/loop/slot等の合成bindingに適用。既にparseされた生成コードの識別子を参照する箇所は同じsymbolを維持。 | syntax `ast.rs` と関連pass |
| F2 | buttonの移動対象argumentとhit/on/offのwrite・副作用を照合。非literal valueは必要時だけ呼ぶclosureにし、条件成立前のeager評価を避ける。sizeで不利なら元の処理を残す。 | `screen_buttons.rs` |
| F3 | helperへ移す自由変数がlocalをcaptureしないことを確認。局所bindingをrootの同名globalへ変更しない。 | 同上 |
| F4 | root初期化、callback相互呼び出し、escapeから到達するhelperをcallback専用slotへ移さない。 | `callback_slots.rs` |
| F5 | 通常/periodicの両等差判定でf64へ変換する前にi64精度を確認。2^53超の整数はvalue table等で保持。 | `screen_loops.rs` |
| F6 | API rootのmember mutationとalias/escapeを共通解析しbuiltin証明から除外。括弧、戻り値、tableへの格納も考慮。property固定化・API alias・副作用解析で共有。 | analysis `effects.rs` / `closed_fields.rs` |
| F7 | 戻り値を捨てるproperty callをliteral文に置換せず、由来付きの空doへ変換し通常の整理へ渡す。 | `property_read.rs` |

隣接回帰として、未存在fieldのさらに先のindexで発生するエラーを消さないようにし、配列要素と明示数値keyが重複するconstructorはLuaのSETLIST順序を字面だけで推定しないよう除外した。名前付き重複fieldの最後の値も確認した。

## 性能改善と不利な結果

release consumerで同一入力をbaseline/candidateへ与え、各入力7回のうち先頭1回を除いた6回の中央値。compileのpass時間を比較したもので、ゲームのFPSやcallback実行時間の測定ではない。baseline consumerはレビュー時にビルドしたものを変更前に保存、candidateは修正版から再ビルドした。VM上のwall-clock測定でCPU隔離はしておらず、数%の差を統計的な性能保証とは扱わない。入力、全観測値、consumerのSHA-256は証跡に含む。

- 未存在table key: 読み取り専用の`Option<NodeId>` lookupに変更。missのたびにscratch ASTへNilをpushしてCOW複製する処理を除去し、置換時だけ必要なnodeを生成。再帰置換もrootから1回にした。
- color subset: 全wrapperを含め32 binding以下、引数2個以上等、名前長と字句境界が一定と証明できる範囲では各singletonの増分を一度だけ実測して全部分集合のcostを計算。従来のmask順・tie-breakを保持。範囲外は従来探索を使う。13 group以上のgreedyは維持。
- format: 幅・出力長を生成前に検査。256 MiB address-space制限下で、旧版が約1 GBを確保しようとしてabortした入力は通常終了した（実行時の不正formatエラーは原文・生成物とも維持）。

| 入力 | 旧版 ms | 修正版 ms | 旧版/修正版 |
|---|---:|---:|---:|
| color-groups-1 | 0.0593 | 0.0657 | 0.90× |
| color-groups-3 | 0.1924 | 0.0983 | 1.96× |
| color-groups-6 | 1.3091 | 0.2347 | 5.58× |
| color-groups-9 | 9.8102 | 0.4335 | 22.63× |
| color-groups-10 | 20.2586 | 0.6261 | 32.36× |
| color-groups-11 | 44.3147 | 0.5937 | 74.64× |
| color-groups-12 | 95.1211 | 0.6462 | 147.20× |
| color-groups-13 | 2.9370 | 2.8570 | 1.03× |
| color-groups-16 | 4.9749 | 5.0383 | 0.99× |
| table-present-50 | 0.2635 | 0.1420 | 1.86× |
| table-present-100 | 0.5060 | 0.2735 | 1.85× |
| table-present-200 | 1.0185 | 0.5450 | 1.87× |
| table-present-400 | 2.0070 | 1.0620 | 1.89× |
| table-present-800 | 4.0745 | 2.1125 | 1.93× |
| table-present-1600 | 8.0965 | 4.2740 | 1.89× |
| table-missing-50 | 2.6945 | 0.1635 | 16.48× |
| table-missing-100 | 10.9585 | 0.2960 | 37.02× |
| table-missing-200 | 37.5395 | 0.5750 | 65.29× |
| table-missing-400 | 146.3255 | 1.1275 | 129.78× |
| table-missing-800 | 649.5370 | 2.2455 | 289.26× |
| table-missing-1600 | 2470.4465 | 4.6455 | 531.79× |

12 groupは約147倍、missing 1600は約532倍になった。一方、1 groupや13/16 groupでは測定上の小さな悪化もある（上表の全値を参照）。この探索領域は高速化の証明範囲外または追加の安全解析コストが優勢で、全入力の高速化は主張しない。color測定63比較では生成コードが完全一致した。新cost計算は1/3/6/9/12 group、衝突名の有無について全部分集合を従来の実測器と照合し、9,364 subsetで一致した。

buttonのdeferred closureを採用する場合にはLuaのclosure生成コストが生じる。今回のF2再現例ではsize選択でhelper化を採用せず、元の評価順を保つ出力となった。描画runtimeの速度向上を保証する変更ではない。

## 生成文字数の比較

ここでの文字数は **生成コード全体のUTF-16 code unit数、改行も含む**。Engineの`size`（zeroCostNewlinesによる課金除外あり）やUTF-8 byte数とは区別した。機械可読結果にはbyte数・改行数・旧新コードも保存してある。同じ設定の出力同士を比較し、原文長との比較ではない。

| 再現例群 | 比較可能 | 増加 | 同じ | 減少 | 最大増加 |
|---|---:|---:|---:|---:|---:|
| api | 11 | 9 | 2 | 0 | +129 |
| root | 63 | 30 | 22 | 11 | +14 |
| numeric | 154 | 59 | 94 | 1 | +44 |
| scope | 792 | 168 | 619 | 5 | +17 |

合計1,020比較で266増加、737同じ、17減少。設定違い・同じ再現例の重複を含み、独立プログラム数ではない。旧版のpanic、compile拒否、`ERROR`でコード比較できないケースはこの集計へ入れていない。APIはF7の旧版compile拒否を除く11件、numericは6件のpanic等を除く154件、scopeは単独passの拒否等を除く792件である。

代表例:

| ケース | 旧→新 | 増加 | 理由 |
|---|---:|---:|---|
| F5 Safe+Exact（隣接する巨大整数） | 76→205 | +129 | 同一値に潰していた等差表現をvalue tableへ変更 |
| F5 既定 | 76→129 | +53 | table保持と共通literal抽出 |
| F3 local capture | 460→556 | +96 | 誤ったroot helper化を不採用 |
| F2 eager value | 433→525 | +92 | 正しい評価順を持つ候補を選択 |
| N9 captured store | 11→55 | +44 | 外から観測される更新を保持 |
| S6 goto例 | 58→74 | +16 | 実行経路を無視するstore削除を不採用 |

別途、描画627比較では186増加・441同じ・減少0。単独passの生成識別子や、従来壊していたbyte stringを含む。公開APIだけでは129比較中30増加・99同じ。**旧版も観測上正しかった公開API 102比較では3増加・99同じ**だった。

- 旧版も正しかった大整数の等差列: `9007199254740990`から16個連続する値のdrawText。通常/Exact等3設定で **79→342（+263）**。2^53超をf64等差に乗せない保守的条件による圧縮率低下。
- 公開APIの最大増加: random drawText 64件、byte string混在で **403→1192（+789）**。旧版の出力は原文と異なっていた。
- 単独records passの最大増加: drawText 90件、**316→3109（+2793）**。UTF-8として扱えない文字列を誤って符号化する候補を不採用にするため。全pipelineの増加と混同しない。
- 単独passだけの比較には、fresh内部識別子の長さの増加も含む。公開pipelineのscope rename後と数値が一致するとは限らない。

全増加観測は末尾付録に列挙した。安全性の証明を拡張すれば圧縮率を戻せる余地はあるが、未証明の変換を今回の修正として再度有効にしていない。

## 回帰検証

- Rust workspace全feature: **891 tests passed、0 failed**（実行0件のcrateを件数に足していない）。既存の全pass provenance matrix、採用される正例、parser/printerの計測一致、metadata標準/詳細map回帰を含む。
- WASM既存試験＋追加の深さ/panic回帰: **92 passed、0 failed、0 skipped**。compiler WASMは今回のコードでビルドした。runtime/raster WASMは変更対象外で、レビュー済み同版の配布物をコピーしハッシュを保存した。
- SDK TypeScript/consumer契約・JS試験: **30 passed**。Python tools: **19 passed**。
- 保存済みnumeric focused **160**、option matrix **720**。Safe/Exact/fast/targetSize/origin有無を含む。異なるエラー位置だけで失敗としない分類を維持。
- 数値演算matrix **26,136**: Safe/Exactの値・subtype差0。既定の128差は全て指定tolerance内であることを追加確認。両方runtime errorは1,152件であり、正常値一致とは別集計。
- syntax corpus **14,440 requests**: 原文を正常実行できた **8,802** 比較で差0。原文エラーをpassとは数えていない。
- hex corpus **3,006**: 差0（旧版は842差）。
- scope保存例 **982**: 意味上の未解決差0。既定モードの`0.0`→`0`という許容subtype差が1件あり、raw文字列の全一致とは報告していない。
- API代表 **12**: 実Luaの観測結果一致。永続的conformance fixtureでは詳細map ON/OFFの **24** 組も比較し、map validatorを実行。
- 描画生成 **627**: 原文/生成物のtrace差0。records、ordered/periodic loop、motif、公開pipelineを含む。
- Source Map parity **500 source × 6設定 = 3,000 compile**、ON/OFF **1,500組**で生成コード差0、panic0、compile失敗0。
- 実WASM Vehicle/raster: 基本48 + API24 = **72比較**で原文失敗0、生成物差0。input/output、複数tick、drawの結果、画素ハッシュを比較。
- `string.format("%01000000000d",1)`の制限付きconsumer: 256 MiB上限下でexit0、コンパイル約1.45 ms、consumer全体約3.61 ms。旧版は約1 GB allocationでabort。処理を空値へ隠蔽せず不正formatの実行時エラーを維持。

常設回帰: `crates/storm-lua-minify/src/review_regressions.rs`、color cost oracle、`conformance/tests/minify_review.rs` とfixture、WASM compiler test。修正に伴う既存正例の更新では、quotient融合・carrier再利用の証明条件を満たす入力にし、実際に変換が採用されることを維持した。期待結果を誤動作へ合わせる変更はしていない。

実行コマンド（作業checkoutのルート。Cargoは環境規定のラッパー経由、targetはtmpfs）:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo check --workspace --locked
cargo xtask check
cargo doc --workspace --all-features --no-deps --locked
npm --prefix packages/lua-engine ci
npm --prefix packages/lua-engine test
node tools/build-compiler.mjs
node tools/test-wasm.mjs
python3 -m unittest discover -s tools/tests -v
```

加えて、レビュー証跡に同梱された独立consumer（root/numeric/scope/api）を修正版のpath dependenciesでビルドして再実行した。全結果のログと終了コード、入力/比較JSON、比較スクリプトを `local-minify-fix-validation/` に保存した。`manifest.json`は保存先のsize/hashを記録する。再開時はファイル実在とhashを確認すること。

### ブランチ整理時の再検証（2026-10-13）

コミット対象の修正スナップショットに対し、fmt、xtask、全workspace/all-features Native試験、既定feature check、全target/all-feature clippy、Rustdoc、公開workflow入力検査、Python tools、SDK、compiler WASM再ビルド、実WASM試験、screen fixtureの12ゲートを再実行し、すべてexit 0を確認した。Native891、WASM92（skip 0）、SDK30、Python19件。過去の大量コーパス・性能測定の結果は上記の保存済み記録であり、この再検証で再測定した数には含めない。runtime/rasterのコードは変更しておらず、そのWASMは同版の既存assetを使用した。今回のコマンド・ログ・終了コードはローカルのブランチ整理バックアップに保存する。

## 契約への影響・限界

1. **深いが有効なLuaを診断で拒否する場合がある。** parser128/AST256は今回導入した資源境界。平坦な多数文はこの深さ制限の対象ではない。大スクリプト全般の固定文字数制限を追加したものではない。
2. Rust syntax utility `decode_lua_string` は `String` から `Option<String>` へ変更した。バイナリ値をUTF-8へ強制変換しないためのAPI変更で、直接利用するRust consumerはNoneを扱う必要がある。byte列が必要なら `source_tools::string_bytes` を使う。`quote_lua` はbyte sliceも受け取る。SDKのJSコンパイルAPIの型は変えていない。
3. gotoを含むchunkの線形dataflow変換、未知の数値型、overflowを証明できないcarrier、local captureを含むbutton、2^53超のscreen等差は保守的に適用を見送る。この条件による文字数増加を測定した。CFG解析や全Luaプログラムの形式的証明を実装したわけではない。
4. Native/WASMの一致と実ゲームへの適合は別。Stormworks実ゲーム上の新たな撮影・実行はしていない。ゲーム描画仕様や既存743 RGBA fixtureは変更していない。
5. compiler latencyの計測をruntime性能向上へ読み替えない。全入力での速度・文字数改善、未発見バグがないことは保証しない。
6. 既存metadata修正は維持し、空白化後の原文への差し替えやvalidator緩和はしていない。元のsourcesContentとexact-copy検査を保ったまま今回の回帰を通した。

## 付録A: 再現例の全文字数増加観測

同一入力の設定違い・単独passを含む。labelのファイル行番号はレビュー時のJSONL行を指す。全ソース、旧新コード、UTF-8 byte数は `character-comparison.jsonl` にある。

| 群 | ケース / 設定 | 旧文字数 | 新文字数 | 増加 |
|---|---|---:|---:|---:|
| api | F1-output-name-collision | 165 | 185 | +20 |
| api | F1-screen-name-collision | 82 | 102 | +20 |
| api | F2-button-eager-value | 433 | 525 | +92 |
| api | F3-button-local-capture | 460 | 556 | +96 |
| api | F4-callback-init-call | 1039 | 1041 | +2 |
| api | F5-screen-i64-default | 76 | 129 | +53 |
| api | F5-screen-i64-safe-exact | 76 | 205 | +129 |
| api | F6-api-table-mutation | 183 | 222 | +39 |
| api | F6-property-function-mutation | 85 | 104 | +19 |
| root | 0 | 8 | 10 | +2 |
| root | 1 | 8 | 10 | +2 |
| root | 2 | 12 | 14 | +2 |
| root | 3 | 12 | 14 | +2 |
| root | 4 | 12 | 14 | +2 |
| root | 21 | 10 | 15 | +5 |
| root | 22 | 10 | 15 | +5 |
| root | 23 | 10 | 15 | +5 |
| root | 24 | 10 | 15 | +5 |
| root | 25 | 10 | 15 | +5 |
| root | 28 | 8 | 13 | +5 |
| root | 29 | 8 | 13 | +5 |
| root | 30 | 10 | 13 | +3 |
| root | 31 | 10 | 13 | +3 |
| root | 32 | 10 | 13 | +3 |
| root | 36 | 10 | 24 | +14 |
| root | 37 | 10 | 24 | +14 |
| root | 38 | 10 | 24 | +14 |
| root | 39 | 10 | 24 | +14 |
| root | 49 | 9 | 12 | +3 |
| root | 50 | 9 | 12 | +3 |
| root | 51 | 9 | 12 | +3 |
| root | 52 | 9 | 12 | +3 |
| root | 53 | 9 | 12 | +3 |
| root | 56 | 0 | 9 | +9 |
| root | 57 | 0 | 9 | +9 |
| root | 58 | 0 | 9 | +9 |
| root | 59 | 0 | 9 | +9 |
| root | 60 | 0 | 9 | +9 |
| root | 61 | 1 | 10 | +9 |
| numeric | negative-divisor/default | 10 | 11 | +1 |
| numeric | negative-divisor/safe | 10 | 11 | +1 |
| numeric | float-mod-positive/default | 8 | 10 | +2 |
| numeric | float-mod-positive/safe | 8 | 10 | +2 |
| numeric | float-mod-negative/default | 8 | 24 | +16 |
| numeric | float-mod-negative/safe | 8 | 28 | +20 |
| numeric | unary-bit-32/default | 8 | 17 | +9 |
| numeric | unary-bit-32/safe | 8 | 17 | +9 |
| numeric | unary-bit-fractional/default | 8 | 10 | +2 |
| numeric | unary-bit-fractional/safe | 8 | 10 | +2 |
| numeric | coercion-bool/default | 8 | 13 | +5 |
| numeric | coercion-bool/safe | 8 | 13 | +5 |
| numeric | coercion-nil/default | 8 | 12 | +4 |
| numeric | coercion-nil/safe | 8 | 12 | +4 |
| numeric | coercion-comparison/default | 10 | 12 | +2 |
| numeric | coercion-comparison/safe | 10 | 12 | +2 |
| numeric | coercion-math-min/default | 8 | 10 | +2 |
| numeric | coercion-math-min/safe | 8 | 10 | +2 |
| numeric | coercion-math-floor/default | 8 | 22 | +14 |
| numeric | coercion-math-floor/safe | 8 | 22 | +14 |
| numeric | concat-float/default | 10 | 16 | +6 |
| numeric | concat-float/safe | 10 | 16 | +6 |
| numeric | identity-string/default | 9 | 11 | +2 |
| numeric | identity-string/safe | 9 | 11 | +2 |
| numeric | identity-bool/default | 10 | 14 | +4 |
| numeric | identity-bool/safe | 10 | 14 | +4 |
| numeric | folded-subtype/default | 19 | 21 | +2 |
| numeric | folded-subtype/safe | 19 | 21 | +2 |
| numeric | folded-negative-zero/default | 11 | 13 | +2 |
| numeric | folded-negative-zero/safe | 11 | 13 | +2 |
| numeric | quotient-overflow/default | 46 | 49 | +3 |
| numeric | quotient-overflow/exact | 46 | 49 | +3 |
| numeric | signed-stale/default | 56 | 62 | +6 |
| numeric | signed-stale/exact | 56 | 62 | +6 |
| numeric | split-effects/default | 52 | 62 | +10 |
| numeric | split-changing-operand/default | 68 | 87 | +19 |
| numeric | split-capture/default | 11 | 55 | +44 |
| numeric | split-capture/safe | 11 | 55 | +44 |
| numeric | split-overflow/default | 33 | 41 | +8 |
| numeric | induction-subtype/default | 48 | 60 | +12 |
| numeric | induction-subtype/safe | 48 | 60 | +12 |
| numeric | induction-subtype/exact | 48 | 60 | +12 |
| numeric | induction-subtype/safe_exact | 48 | 60 | +12 |
| numeric | boolean-subtype/default | 19 | 21 | +2 |
| numeric | boolean-subtype/safe | 19 | 21 | +2 |
| numeric | boolean-subtype/exact | 21 | 42 | +21 |
| numeric | boolean-subtype/safe_exact | 21 | 42 | +21 |
| numeric | product-carrier/default | 94 | 113 | +19 |
| numeric | product-carrier/safe | 94 | 113 | +19 |
| numeric | approx-table-key/default | 82 | 88 | +6 |
| numeric | approx-function-return/default | 144 | 150 | +6 |
| numeric | approx-table-key-isolated/default | 50 | 56 | +6 |
| numeric | signed-stale-isolated/default | 68 | 74 | +6 |
| numeric | signed-stale-isolated/exact | 68 | 74 | +6 |
| numeric | split-effect-isolated/default | 92 | 114 | +22 |
| numeric | split-capture-isolated/default | 65 | 101 | +36 |
| numeric | split-capture-isolated/safe | 65 | 101 | +36 |
| numeric | quotient-isolated/default | 58 | 61 | +3 |
| numeric | quotient-isolated/exact | 58 | 61 | +3 |
| scope | extra-probes.jsonl:1:default | 96 | 99 | +3 |
| scope | extra-probes.jsonl:2:exact | 96 | 99 | +3 |
| scope | findings-matrix.jsonl:1:default | 58 | 74 | +16 |
| scope | findings-matrix.jsonl:2:exact | 58 | 74 | +16 |
| scope | findings-matrix.jsonl:3:safe | 58 | 74 | +16 |
| scope | findings-matrix.jsonl:4:default | 70 | 74 | +4 |
| scope | findings-matrix.jsonl:5:default | 58 | 74 | +16 |
| scope | findings-matrix.jsonl:6:default | 58 | 66 | +8 |
| scope | findings-matrix.jsonl:7:exact | 58 | 66 | +8 |
| scope | findings-matrix.jsonl:8:safe | 58 | 66 | +8 |
| scope | findings-matrix.jsonl:9:default | 62 | 66 | +4 |
| scope | findings-matrix.jsonl:10:default | 62 | 66 | +4 |
| scope | findings-matrix.jsonl:11:default | 123 | 127 | +4 |
| scope | findings-matrix.jsonl:12:exact | 123 | 127 | +4 |
| scope | findings-matrix.jsonl:13:safe | 123 | 127 | +4 |
| scope | findings-matrix.jsonl:16:default | 89 | 97 | +8 |
| scope | findings-matrix.jsonl:17:exact | 89 | 97 | +8 |
| scope | findings-matrix.jsonl:18:safe | 89 | 97 | +8 |
| scope | findings-matrix.jsonl:19:default | 137 | 149 | +12 |
| scope | findings-matrix.jsonl:21:default | 102 | 108 | +6 |
| scope | findings-matrix.jsonl:22:exact | 102 | 108 | +6 |
| scope | findings-matrix.jsonl:23:safe | 102 | 108 | +6 |
| scope | findings-matrix.jsonl:24:default | 113 | 119 | +6 |
| scope | findings-matrix.jsonl:26:default | 41 | 42 | +1 |
| scope | findings-matrix.jsonl:27:exact | 41 | 42 | +1 |
| scope | findings-matrix.jsonl:28:safe | 41 | 42 | +1 |
| scope | findings-matrix.jsonl:29:default | 41 | 47 | +6 |
| scope | findings-matrix.jsonl:31:default | 91 | 93 | +2 |
| scope | findings-matrix.jsonl:32:exact | 91 | 93 | +2 |
| scope | findings-matrix.jsonl:34:default | 105 | 113 | +8 |
| scope | immutable-funcstat.jsonl:4:immutable-table | 72 | 82 | +10 |
| scope | immutable-funcstat.jsonl:8:immutable-table | 74 | 89 | +15 |
| scope | immutable-synthesis-shadow.jsonl:1:default | 96 | 99 | +3 |
| scope | immutable-synthesis-shadow.jsonl:2:exact | 96 | 99 | +3 |
| scope | immutable-synthesis-shadow.jsonl:6:default | 161 | 164 | +3 |
| scope | local-sink-minimal.jsonl:4:locals | 57 | 59 | +2 |
| scope | local-sink-table-index.jsonl:5:locals | 115 | 117 | +2 |
| scope | local-sink-table-index.jsonl:9:locals | 13 | 15 | +2 |
| scope | root-collision-tick-only.jsonl:1:default | 92 | 98 | +6 |
| scope | root-collision-tick-only.jsonl:2:exact | 92 | 98 | +6 |
| scope | root-collision-tick-only.jsonl:3:safe | 92 | 98 | +6 |
| scope | root-collision-tick-only.jsonl:4:root | 92 | 98 | +6 |
| scope | structured-corpus.jsonl:21:default | 105 | 111 | +6 |
| scope | structured-corpus.jsonl:22:exact | 105 | 111 | +6 |
| scope | structured-corpus.jsonl:23:safe | 105 | 111 | +6 |
| scope | structured-corpus.jsonl:25:default | 105 | 111 | +6 |
| scope | structured-corpus.jsonl:26:exact | 105 | 111 | +6 |
| scope | structured-corpus.jsonl:27:safe | 105 | 111 | +6 |
| scope | structured-corpus.jsonl:29:default | 105 | 111 | +6 |
| scope | structured-corpus.jsonl:30:exact | 105 | 111 | +6 |
| scope | structured-corpus.jsonl:31:safe | 105 | 111 | +6 |
| scope | structured-corpus.jsonl:33:default | 105 | 111 | +6 |
| scope | structured-corpus.jsonl:34:exact | 105 | 111 | +6 |
| scope | structured-corpus.jsonl:35:safe | 105 | 111 | +6 |
| scope | structured-corpus.jsonl:37:default | 108 | 114 | +6 |
| scope | structured-corpus.jsonl:38:exact | 108 | 114 | +6 |
| scope | structured-corpus.jsonl:39:safe | 108 | 114 | +6 |
| scope | structured-corpus.jsonl:41:default | 122 | 124 | +2 |
| scope | structured-corpus.jsonl:121:default | 102 | 110 | +8 |
| scope | structured-corpus.jsonl:122:exact | 104 | 110 | +6 |
| scope | structured-corpus.jsonl:123:safe | 104 | 110 | +6 |
| scope | structured-corpus.jsonl:125:default | 104 | 110 | +6 |
| scope | structured-corpus.jsonl:126:exact | 104 | 110 | +6 |
| scope | structured-corpus.jsonl:127:safe | 104 | 110 | +6 |
| scope | structured-corpus.jsonl:129:default | 104 | 110 | +6 |
| scope | structured-corpus.jsonl:130:exact | 104 | 110 | +6 |
| scope | structured-corpus.jsonl:131:safe | 104 | 110 | +6 |
| scope | structured-corpus.jsonl:133:default | 104 | 110 | +6 |
| scope | structured-corpus.jsonl:134:exact | 104 | 110 | +6 |
| scope | structured-corpus.jsonl:135:safe | 104 | 110 | +6 |
| scope | structured-corpus.jsonl:137:default | 105 | 111 | +6 |
| scope | structured-corpus.jsonl:138:exact | 105 | 111 | +6 |
| scope | structured-corpus.jsonl:139:safe | 105 | 111 | +6 |
| scope | structured-corpus.jsonl:141:default | 82 | 88 | +6 |
| scope | structured-corpus.jsonl:341:default | 58 | 74 | +16 |
| scope | structured-corpus.jsonl:342:exact | 58 | 74 | +16 |
| scope | structured-corpus.jsonl:343:safe | 58 | 74 | +16 |
| scope | structured-corpus.jsonl:345:default | 58 | 74 | +16 |
| scope | structured-corpus.jsonl:346:exact | 58 | 74 | +16 |
| scope | structured-corpus.jsonl:347:safe | 58 | 74 | +16 |
| scope | structured-corpus.jsonl:349:default | 58 | 74 | +16 |
| scope | structured-corpus.jsonl:350:exact | 58 | 74 | +16 |
| scope | structured-corpus.jsonl:351:safe | 58 | 74 | +16 |
| scope | structured-corpus.jsonl:353:default | 58 | 74 | +16 |
| scope | structured-corpus.jsonl:354:exact | 58 | 74 | +16 |
| scope | structured-corpus.jsonl:355:safe | 58 | 74 | +16 |
| scope | structured-corpus.jsonl:357:default | 60 | 77 | +17 |
| scope | structured-corpus.jsonl:358:exact | 60 | 77 | +17 |
| scope | structured-corpus.jsonl:359:safe | 60 | 77 | +17 |
| scope | structured-corpus.jsonl:361:default | 96 | 102 | +6 |
| scope | structured-corpus.jsonl:362:exact | 96 | 102 | +6 |
| scope | structured-corpus.jsonl:363:safe | 96 | 102 | +6 |
| scope | structured-corpus.jsonl:365:default | 96 | 102 | +6 |
| scope | structured-corpus.jsonl:366:exact | 96 | 102 | +6 |
| scope | structured-corpus.jsonl:367:safe | 96 | 102 | +6 |
| scope | structured-corpus.jsonl:369:default | 96 | 102 | +6 |
| scope | structured-corpus.jsonl:370:exact | 96 | 102 | +6 |
| scope | structured-corpus.jsonl:371:safe | 96 | 102 | +6 |
| scope | structured-corpus.jsonl:373:default | 96 | 102 | +6 |
| scope | structured-corpus.jsonl:374:exact | 96 | 102 | +6 |
| scope | structured-corpus.jsonl:375:safe | 96 | 102 | +6 |
| scope | structured-corpus.jsonl:377:default | 99 | 115 | +16 |
| scope | structured-corpus.jsonl:378:exact | 99 | 115 | +16 |
| scope | structured-corpus.jsonl:379:safe | 99 | 115 | +16 |
| scope | structured-corpus.jsonl:421:default | 118 | 120 | +2 |
| scope | structured-corpus.jsonl:441:default | 89 | 93 | +4 |
| scope | structured-corpus.jsonl:442:exact | 89 | 93 | +4 |
| scope | structured-corpus.jsonl:445:default | 89 | 93 | +4 |
| scope | structured-corpus.jsonl:446:exact | 89 | 93 | +4 |
| scope | structured-corpus.jsonl:449:default | 89 | 93 | +4 |
| scope | structured-corpus.jsonl:450:exact | 89 | 93 | +4 |
| scope | structured-corpus.jsonl:453:default | 89 | 93 | +4 |
| scope | structured-corpus.jsonl:454:exact | 89 | 93 | +4 |
| scope | structured-corpus.jsonl:457:default | 92 | 95 | +3 |
| scope | structured-corpus.jsonl:458:exact | 92 | 95 | +3 |
| scope | structured-corpus.jsonl:462:exact | 91 | 99 | +8 |
| scope | structured-corpus.jsonl:463:safe | 91 | 99 | +8 |
| scope | structured-corpus.jsonl:465:default | 89 | 99 | +10 |
| scope | structured-corpus.jsonl:466:exact | 91 | 99 | +8 |
| scope | structured-corpus.jsonl:467:safe | 91 | 99 | +8 |
| scope | structured-corpus.jsonl:469:default | 91 | 99 | +8 |
| scope | structured-corpus.jsonl:470:exact | 91 | 99 | +8 |
| scope | structured-corpus.jsonl:471:safe | 91 | 99 | +8 |
| scope | structured-corpus.jsonl:473:default | 91 | 99 | +8 |
| scope | structured-corpus.jsonl:474:exact | 91 | 99 | +8 |
| scope | structured-corpus.jsonl:475:safe | 91 | 99 | +8 |
| scope | structured-corpus.jsonl:477:default | 94 | 104 | +10 |
| scope | structured-corpus.jsonl:478:exact | 94 | 104 | +10 |
| scope | structured-corpus.jsonl:479:safe | 94 | 104 | +10 |
| scope | target-probes.jsonl:1:default | 58 | 74 | +16 |
| scope | target-probes.jsonl:2:exact | 58 | 74 | +16 |
| scope | target-probes.jsonl:3:safe | 58 | 74 | +16 |
| scope | target-probes.jsonl:4:final | 70 | 74 | +4 |
| scope | target-probes.jsonl:9:default | 123 | 127 | +4 |
| scope | target-probes.jsonl:10:exact | 123 | 127 | +4 |
| scope | target-probes.jsonl:11:safe | 123 | 127 | +4 |
| scope | target-probes.jsonl:21:root | 97 | 103 | +6 |
| scope | target-probes.jsonl:25:default | 64 | 72 | +8 |
| scope | target-probes.jsonl:26:exact | 64 | 72 | +8 |
| scope | target-probes.jsonl:27:safe | 64 | 72 | +8 |
| scope | target-probes.jsonl:33:default | 89 | 97 | +8 |
| scope | target-probes.jsonl:34:exact | 89 | 97 | +8 |
| scope | target-probes.jsonl:35:safe | 89 | 97 | +8 |
| scope | target-probes.jsonl:65:default | 91 | 93 | +2 |
| scope | target-probes.jsonl:66:exact | 91 | 93 | +2 |
| scope | target-probes2.jsonl:4:root | 103 | 109 | +6 |
| scope | target-probes2.jsonl:11:default | 102 | 108 | +6 |
| scope | target-probes2.jsonl:12:exact | 102 | 108 | +6 |
| scope | target-probes2.jsonl:13:safe | 102 | 108 | +6 |
| scope | target-probes2.jsonl:14:root | 113 | 119 | +6 |
| scope | target-probes2.jsonl:21:default | 89 | 97 | +8 |
| scope | target-probes2.jsonl:22:exact | 89 | 97 | +8 |
| scope | target-probes2.jsonl:23:safe | 89 | 97 | +8 |
| scope | target-probes2.jsonl:28:immutable-reuse | 137 | 149 | +12 |
| scope | target-probes2.jsonl:39:destructive | 135 | 143 | +8 |
| scope | target-probes2.jsonl:41:default | 91 | 93 | +2 |
| scope | target-probes2.jsonl:42:exact | 91 | 93 | +2 |
| scope | target-probes2.jsonl:49:destructive | 105 | 113 | +8 |
| scope | target-probes2.jsonl:51:default | 58 | 74 | +16 |
| scope | target-probes2.jsonl:52:exact | 58 | 74 | +16 |
| scope | target-probes2.jsonl:53:safe | 58 | 74 | +16 |
| scope | target-probes2.jsonl:61:default | 114 | 129 | +15 |
| scope | target-probes2.jsonl:62:exact | 114 | 129 | +15 |
| scope | target-probes2.jsonl:63:safe | 114 | 129 | +15 |
| scope | target-probes3.jsonl:1:default | 41 | 42 | +1 |
| scope | target-probes3.jsonl:2:exact | 41 | 42 | +1 |
| scope | target-probes3.jsonl:3:safe | 41 | 42 | +1 |
| scope | target-probes3.jsonl:4:root | 41 | 47 | +6 |

## 付録B: 描画追加コーパスの全文字数増加観測

`compile`が公開pipeline、それ以外は単独pass。旧版同値の列はそのconsumerで原文traceと一致したかを表し、一般的な正しさの証明ではない。入力順で記載し、同名で設定の異なる行も省略していない。入力は `draw-cases.jsonl`、比較は `draw-character-comparison.json`。

| ケース | 経路 | 旧版同値 | 旧文字数 | 新文字数 | 増加 |
|---|---|---|---:|---:|---:|
| fuzz-0-linear-drawText-4-records | records | no | 127 | 130 | +3 |
| fuzz-0-linear-drawText-4-screen | screen | no | 127 | 130 | +3 |
| fuzz-0-linear-drawText-4-screen-periodic | screen-periodic | no | 127 | 130 | +3 |
| fuzz-0-linear-drawText-4-motifs | motifs | no | 127 | 130 | +3 |
| fuzz-0-linear-drawText-4-compile | compile | no | 89 | 92 | +3 |
| fuzz-1-periodic-drawText-4-records | records | no | 125 | 131 | +6 |
| fuzz-1-periodic-drawText-4-screen | screen | no | 125 | 131 | +6 |
| fuzz-1-periodic-drawText-4-screen-periodic | screen-periodic | no | 125 | 131 | +6 |
| fuzz-1-periodic-drawText-4-motifs | motifs | no | 125 | 131 | +6 |
| fuzz-1-periodic-drawText-4-compile | compile | no | 87 | 93 | +6 |
| fuzz-2-translations-drawText-8-records | records | no | 204 | 262 | +58 |
| fuzz-2-translations-drawText-8-screen | screen | no | 333 | 348 | +15 |
| fuzz-2-translations-drawText-8-screen-periodic | screen-periodic | no | 349 | 352 | +3 |
| fuzz-2-translations-drawText-8-motifs | motifs | no | 259 | 262 | +3 |
| fuzz-2-translations-drawText-8-compile | compile | no | 155 | 158 | +3 |
| fuzz-3-repeat-drawLine-64-screen | screen | yes | 2946 | 3161 | +215 |
| fuzz-6-repeat-drawLine-48-screen | screen | yes | 2202 | 2357 | +155 |
| fuzz-7-periodic-drawLine-90-screen | screen | yes | 4188 | 4508 | +320 |
| fuzz-8-periodic-drawCircleF-64-screen | screen | yes | 2470 | 2634 | +164 |
| fuzz-9-periodic-drawText-90-records | records | no | 426 | 3162 | +2736 |
| fuzz-9-periodic-drawText-90-screen | screen | no | 3087 | 3162 | +75 |
| fuzz-9-periodic-drawText-90-screen-periodic | screen-periodic | no | 1238 | 1313 | +75 |
| fuzz-9-periodic-drawText-90-motifs | motifs | no | 3087 | 3162 | +75 |
| fuzz-9-periodic-drawText-90-compile | compile | no | 1044 | 1119 | +75 |
| fuzz-10-translations-drawRectF-48-screen | screen | yes | 1498 | 1598 | +100 |
| fuzz-11-periodic-drawRectF-8-screen | screen | yes | 378 | 388 | +10 |
| fuzz-12-periodic-drawLine-48-screen | screen | yes | 2224 | 2379 | +155 |
| fuzz-13-periodic-drawCircleF-64-screen | screen | yes | 2470 | 2634 | +164 |
| fuzz-14-translations-drawRectF-8-screen | screen | yes | 262 | 272 | +10 |
| fuzz-16-periodic-drawLine-32-screen | screen | yes | 1474 | 1569 | +95 |
| fuzz-21-translations-drawCircleF-48-screen | screen | yes | 1260 | 1340 | +80 |
| fuzz-23-periodic-drawText-24-records | records | no | 294 | 789 | +495 |
| fuzz-23-periodic-drawText-24-screen | screen | no | 765 | 789 | +24 |
| fuzz-23-periodic-drawText-24-screen-periodic | screen-periodic | no | 500 | 524 | +24 |
| fuzz-23-periodic-drawText-24-motifs | motifs | no | 765 | 789 | +24 |
| fuzz-23-periodic-drawText-24-compile | compile | no | 306 | 330 | +24 |
| fuzz-24-repeat-drawRectF-8-screen | screen | yes | 384 | 394 | +10 |
| fuzz-25-repeat-drawCircleF-24-screen | screen | yes | 932 | 984 | +52 |
| fuzz-27-random-drawText-64-records | records | no | 426 | 2070 | +1644 |
| fuzz-27-random-drawText-64-screen | screen | no | 2019 | 2070 | +51 |
| fuzz-27-random-drawText-64-screen-periodic | screen-periodic | no | 2019 | 2070 | +51 |
| fuzz-27-random-drawText-64-motifs | motifs | no | 2019 | 2070 | +51 |
| fuzz-27-random-drawText-64-compile | compile | no | 403 | 1192 | +789 |
| fuzz-29-repeat-drawCircleF-16-screen | screen | yes | 628 | 656 | +28 |
| fuzz-30-translations-drawCircleF-48-screen | screen | yes | 1260 | 1340 | +80 |
| fuzz-31-periodic-drawRectF-24-screen | screen | yes | 1110 | 1175 | +65 |
| fuzz-32-linear-drawText-8-records | records | no | 199 | 278 | +79 |
| fuzz-32-linear-drawText-8-screen | screen | no | 303 | 306 | +3 |
| fuzz-32-linear-drawText-8-screen-periodic | screen-periodic | no | 303 | 306 | +3 |
| fuzz-32-linear-drawText-8-motifs | motifs | no | 275 | 278 | +3 |
| fuzz-32-linear-drawText-8-compile | compile | no | 157 | 160 | +3 |
| fuzz-33-repeat-drawRectF-48-screen | screen | yes | 2214 | 2369 | +155 |
| fuzz-35-translations-drawLine-8-screen | screen | yes | 259 | 269 | +10 |
| fuzz-36-repeat-drawText-90-records | records | no | 329 | 2995 | +2666 |
| fuzz-36-repeat-drawText-90-screen | screen | no | 2926 | 2995 | +69 |
| fuzz-36-repeat-drawText-90-screen-periodic | screen-periodic | no | 1260 | 1329 | +69 |
| fuzz-36-repeat-drawText-90-motifs | motifs | no | 2034 | 2995 | +961 |
| fuzz-36-repeat-drawText-90-compile | compile | no | 1066 | 1135 | +69 |
| fuzz-41-repeat-drawLine-64-screen | screen | yes | 2946 | 3161 | +215 |
| fuzz-42-translations-drawLine-8-screen | screen | yes | 259 | 269 | +10 |
| fuzz-43-periodic-drawCircleF-48-screen | screen | yes | 1830 | 1946 | +116 |
| fuzz-44-repeat-drawCircleF-90-screen | screen | yes | 3466 | 3722 | +256 |
| fuzz-45-translations-drawLine-16-screen | screen | yes | 565 | 590 | +25 |
| fuzz-49-repeat-drawText-48-records | records | no | 287 | 1595 | +1308 |
| fuzz-49-repeat-drawText-48-screen | screen | no | 1556 | 1595 | +39 |
| fuzz-49-repeat-drawText-48-screen-periodic | screen-periodic | no | 792 | 831 | +39 |
| fuzz-49-repeat-drawText-48-motifs | motifs | no | 1325 | 1595 | +270 |
| fuzz-49-repeat-drawText-48-compile | compile | no | 598 | 637 | +39 |
| fuzz-50-translations-drawRectF-32-screen | screen | yes | 1006 | 1071 | +65 |
| fuzz-51-periodic-drawCircleF-64-screen | screen | yes | 2470 | 2634 | +164 |
| fuzz-52-translations-drawLine-48-screen | screen | yes | 1490 | 1590 | +100 |
| fuzz-53-repeat-drawRectF-90-screen | screen | yes | 4140 | 4460 | +320 |
| fuzz-54-linear-drawText-90-records | records | no | 316 | 3100 | +2784 |
| fuzz-54-linear-drawText-90-screen | screen | no | 1210 | 1282 | +72 |
| fuzz-54-linear-drawText-90-screen-periodic | screen-periodic | no | 1210 | 1282 | +72 |
| fuzz-54-linear-drawText-90-motifs | motifs | no | 3028 | 3100 | +72 |
| fuzz-54-linear-drawText-90-compile | compile | no | 1064 | 1136 | +72 |
| fuzz-55-random-drawText-16-records | records | no | 280 | 625 | +345 |
| fuzz-55-random-drawText-16-screen | screen | no | 613 | 625 | +12 |
| fuzz-55-random-drawText-16-screen-periodic | screen-periodic | no | 613 | 625 | +12 |
| fuzz-55-random-drawText-16-motifs | motifs | no | 613 | 625 | +12 |
| fuzz-55-random-drawText-16-compile | compile | no | 257 | 419 | +162 |
| fuzz-56-translations-drawCircleF-32-screen | screen | yes | 854 | 906 | +52 |
| fuzz-57-repeat-drawCircleF-24-screen | screen | yes | 932 | 984 | +52 |
| fuzz-58-periodic-drawLine-48-screen | screen | yes | 2224 | 2379 | +155 |
| fuzz-59-repeat-drawText-8-records | records | no | 194 | 279 | +85 |
| fuzz-59-repeat-drawText-8-screen | screen | no | 276 | 279 | +3 |
| fuzz-59-repeat-drawText-8-screen-periodic | screen-periodic | no | 371 | 374 | +3 |
| fuzz-59-repeat-drawText-8-motifs | motifs | no | 276 | 279 | +3 |
| fuzz-59-repeat-drawText-8-compile | compile | no | 177 | 180 | +3 |
| fuzz-60-periodic-drawCircleF-16-screen | screen | yes | 581 | 601 | +20 |
| fuzz-62-periodic-drawText-24-records | records | no | 294 | 786 | +492 |
| fuzz-62-periodic-drawText-24-screen | screen | no | 765 | 786 | +21 |
| fuzz-62-periodic-drawText-24-screen-periodic | screen-periodic | no | 500 | 521 | +21 |
| fuzz-62-periodic-drawText-24-motifs | motifs | no | 765 | 786 | +21 |
| fuzz-62-periodic-drawText-24-compile | compile | no | 306 | 327 | +21 |
| fuzz-63-random-drawText-32-records | records | no | 329 | 1124 | +795 |
| fuzz-63-random-drawText-32-screen | screen | no | 1106 | 1124 | +18 |
| fuzz-63-random-drawText-32-screen-periodic | screen-periodic | no | 1106 | 1124 | +18 |
| fuzz-63-random-drawText-32-motifs | motifs | no | 1106 | 1124 | +18 |
| fuzz-63-random-drawText-32-compile | compile | no | 306 | 694 | +388 |
| fuzz-64-periodic-drawCircleF-8-screen | screen | yes | 275 | 287 | +12 |
| fuzz-66-repeat-drawRectF-90-screen | screen | yes | 4140 | 4460 | +320 |
| fuzz-69-repeat-drawCircleF-90-screen | screen | yes | 3466 | 3722 | +256 |
| fuzz-71-repeat-drawRectF-64-screen | screen | yes | 2962 | 3177 | +215 |
| fuzz-72-random-drawText-32-records | records | no | 327 | 1110 | +783 |
| fuzz-72-random-drawText-32-screen | screen | no | 1068 | 1110 | +42 |
| fuzz-72-random-drawText-32-screen-periodic | screen-periodic | no | 1068 | 1110 | +42 |
| fuzz-72-random-drawText-32-motifs | motifs | no | 1068 | 1110 | +42 |
| fuzz-72-random-drawText-32-compile | compile | no | 304 | 680 | +376 |
| fuzz-73-translations-drawText-16-records | records | no | 271 | 529 | +258 |
| fuzz-73-translations-drawText-16-screen | screen | no | 652 | 694 | +42 |
| fuzz-73-translations-drawText-16-screen-periodic | screen-periodic | no | 416 | 428 | +12 |
| fuzz-73-translations-drawText-16-motifs | motifs | no | 517 | 529 | +12 |
| fuzz-73-translations-drawText-16-compile | compile | no | 222 | 234 | +12 |
| fuzz-75-translations-drawRectF-90-screen | screen | yes | 2845 | 3050 | +205 |
| fuzz-78-translations-drawRectF-16-screen | screen | yes | 568 | 593 | +25 |
| fuzz-79-translations-drawLine-8-screen | screen | yes | 259 | 269 | +10 |
| fuzz-81-periodic-drawCircleF-24-screen | screen | yes | 890 | 930 | +40 |
| fuzz-85-periodic-drawRectF-48-screen | screen | yes | 2236 | 2391 | +155 |
| fuzz-86-linear-drawText-24-records | records | no | 250 | 846 | +596 |
| fuzz-86-linear-drawText-24-screen | screen | no | 508 | 523 | +15 |
| fuzz-86-linear-drawText-24-screen-periodic | screen-periodic | no | 508 | 523 | +15 |
| fuzz-86-linear-drawText-24-motifs | motifs | no | 831 | 846 | +15 |
| fuzz-86-linear-drawText-24-compile | compile | no | 362 | 377 | +15 |
| fuzz-89-repeat-drawText-32-records | records | no | 271 | 1089 | +818 |
| fuzz-89-repeat-drawText-32-screen | screen | no | 1062 | 1089 | +27 |
| fuzz-89-repeat-drawText-32-screen-periodic | screen-periodic | no | 642 | 669 | +27 |
| fuzz-89-repeat-drawText-32-motifs | motifs | no | 916 | 1089 | +173 |
| fuzz-89-repeat-drawText-32-compile | compile | no | 448 | 475 | +27 |
| fuzz-90-translations-drawCircleF-90-screen | screen | yes | 2387 | 2551 | +164 |
| fuzz-91-repeat-drawRectF-24-screen | screen | yes | 1112 | 1177 | +65 |
| fuzz-92-periodic-drawText-32-records | records | no | 310 | 1128 | +818 |
| fuzz-92-periodic-drawText-32-screen | screen | no | 1095 | 1128 | +33 |
| fuzz-92-periodic-drawText-32-screen-periodic | screen-periodic | no | 638 | 671 | +33 |
| fuzz-92-periodic-drawText-32-motifs | motifs | no | 1095 | 1128 | +33 |
| fuzz-92-periodic-drawText-32-compile | compile | no | 444 | 477 | +33 |
| fuzz-94-periodic-drawCircleF-32-screen | screen | yes | 1202 | 1270 | +68 |
| fuzz-95-repeat-drawCircleF-8-screen | screen | yes | 324 | 332 | +8 |
| fuzz-96-periodic-drawCircleF-8-screen | screen | yes | 275 | 287 | +12 |
| fuzz-97-repeat-drawLine-32-screen | screen | yes | 1468 | 1563 | +95 |
| fuzz-99-translations-drawText-24-records | records | no | 295 | 775 | +480 |
| fuzz-99-translations-drawText-24-screen | screen | no | 1014 | 1080 | +66 |
| fuzz-99-translations-drawText-24-screen-periodic | screen-periodic | no | 464 | 482 | +18 |
| fuzz-99-translations-drawText-24-motifs | motifs | no | 757 | 775 | +18 |
| fuzz-99-translations-drawText-24-compile | compile | no | 270 | 288 | +18 |
| fuzz-101-linear-drawText-90-records | records | no | 316 | 3109 | +2793 |
| fuzz-101-linear-drawText-90-screen | screen | no | 1228 | 1291 | +63 |
| fuzz-101-linear-drawText-90-screen-periodic | screen-periodic | no | 1228 | 1291 | +63 |
| fuzz-101-linear-drawText-90-motifs | motifs | no | 3046 | 3109 | +63 |
| fuzz-101-linear-drawText-90-compile | compile | no | 1082 | 1145 | +63 |
| fuzz-104-translations-drawCircleF-64-screen | screen | yes | 1735 | 1851 | +116 |
| fuzz-105-repeat-drawRectF-64-screen | screen | yes | 2962 | 3177 | +215 |
| fuzz-106-repeat-drawText-8-records | records | no | 207 | 279 | +72 |
| fuzz-106-repeat-drawText-8-screen | screen | no | 276 | 279 | +3 |
| fuzz-106-repeat-drawText-8-screen-periodic | screen-periodic | no | 371 | 374 | +3 |
| fuzz-106-repeat-drawText-8-motifs | motifs | no | 276 | 279 | +3 |
| fuzz-106-repeat-drawText-8-compile | compile | no | 177 | 180 | +3 |
| fuzz-107-repeat-drawCircleF-64-screen | screen | yes | 2476 | 2648 | +172 |
| fuzz-108-random-drawText-24-records | records | no | 306 | 845 | +539 |
| fuzz-108-random-drawText-24-screen | screen | no | 824 | 845 | +21 |
| fuzz-108-random-drawText-24-screen-periodic | screen-periodic | no | 824 | 845 | +21 |
| fuzz-108-random-drawText-24-motifs | motifs | no | 824 | 845 | +21 |
| fuzz-108-random-drawText-24-compile | compile | no | 283 | 527 | +244 |
| fuzz-111-translations-drawCircleF-8-screen | screen | yes | 230 | 238 | +8 |
| fuzz-112-repeat-drawLine-8-screen | screen | yes | 382 | 392 | +10 |
| fuzz-113-translations-drawCircleF-32-screen | screen | yes | 854 | 906 | +52 |
| fuzz-114-linear-drawText-90-records | records | no | 316 | 3028 | +2712 |
| fuzz-114-linear-drawText-90-screen | screen | no | 1138 | 1210 | +72 |
| fuzz-114-linear-drawText-90-screen-periodic | screen-periodic | no | 1138 | 1210 | +72 |
| fuzz-114-linear-drawText-90-motifs | motifs | no | 2956 | 3028 | +72 |
| fuzz-114-linear-drawText-90-compile | compile | no | 992 | 1064 | +72 |
| fuzz-115-translations-drawLine-24-screen | screen | yes | 750 | 790 | +40 |
| fuzz-118-repeat-drawRectF-16-screen | screen | yes | 748 | 783 | +35 |
| integer-9007199254740992-8-screen-0 | screen | no | 124 | 327 | +203 |
| integer-9007199254740992-8-screen-2 | screen | no | 124 | 327 | +203 |
| integer-9007199254740992-8-screen-2 | screen | no | 124 | 327 | +203 |
| integer-9007199254740992-8-compile-0 | compile | no | 76 | 129 | +53 |
| integer-9007199254740992-8-compile-2 | compile | no | 76 | 205 | +129 |
| integer-9007199254740992-8-compile-2 | compile | no | 76 | 205 | +129 |
| integer-9007199254740990-16-screen-0 | screen | yes | 151 | 464 | +313 |
| integer-9007199254740990-16-screen-2 | screen | yes | 151 | 464 | +313 |
| integer-9007199254740990-16-screen-2 | screen | yes | 151 | 464 | +313 |
| integer-9007199254740990-16-compile-0 | compile | yes | 79 | 342 | +263 |
| integer-9007199254740990-16-compile-2 | compile | yes | 79 | 342 | +263 |
| integer-9007199254740990-16-compile-2 | compile | yes | 79 | 342 | +263 |
