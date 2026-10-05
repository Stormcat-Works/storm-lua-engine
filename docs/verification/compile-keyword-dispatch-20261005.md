# v0.3.1 キーワード・builtin判定の高速化と不採用試案

2026-10-05。作業ブランチは`feat/minify-performance-v0.3.1`。基準は**`f3e5c87`**、今回の最終実装は**`5f1f58f021d90b50ebf776ca2d694d36e0d70a8e`**。前の印字・数値読取り・名前保存の改善を[検証記録](compile-emission-20261005.md)へ確定してから、別の比較として実施した。過去の改善率を今回の結果へ再加算しない。

[計画](../design/compile-traversal-pools-v031.md) / [機械記録](compile-keyword-dispatch-20261005.json)。**Map有無の最大1.2倍、理想1.1倍という目標は引き続き未達。** 平均比ではなく、個別のシーンと絶対時間も確認する。

## 1. 採用した変更

### 予約語の判定を配列走査から定数matchへ

字句解析は識別子を読むたびにLuaの予約語一覧を順に比較していた。公開されたKEYWORDS配列とprivateなmembership matchを同じリテラル宣言から生成し、語彙を二重管理せず定数文字列の分岐へ変えた。列挙順・大文字小文字・tokenの種類・元位置はそのまま保持する。

印字側の、文字列キーを通常の識別子として表現できるかという検査も同じ判定を使用する。ASCIIだけ許される条件はbyteで検査し、Unicodeのコードポイント展開を行わず、以前と同じ不許可文字を拒否する。キーワード・その部分文字列・似た綴り・Unicode・NUL・32,768疑似ランダム表記を従来の配列走査と比較した。

この処理はMap専用ではなく、parse・token基準サイズ・構造印字・字句短縮に共通する。Mapなしの処理を意図的に増やす分岐や待機は追加していない。

### builtinの副作用分類で同じprefix比較を繰り返さない

すでに束縛解決でbuiltinと確定した参照について、pure/total/effectの各候補全体へ同じrootのprefix比較を繰り返していた。公開API一覧と借用したroot/memberのmatchを一つの定義から生成し、入力名を結合し直さず分類する。

未解決のユーザー関数を名前だけでbuiltinと決め打ちする変更ではない。aliasの完全な名前、通常のmember、入れ子・ドット付きkey、似た綴り、host環境、pure/total/ordered/may-throwの契約は維持する。旧式の完全名比較と、既存のeffect解析全体の独立参照テストを利用した。公開API一覧の順序と内容もテストで固定している。

## 2. 採用しなかったもの

候補を思いついたことと、製品へ採用したことは分けた。次の3案は完全な出力比較に通ったが、WASM pilotで一貫した効果が得られず製品ソースを元へ戻した。

- 小さいcall/block/returnの子IDをstackに保持するsnapshot。大入力の差がほぼなく、小入力の通常側に負の観測があった。
- Map辞書の借用reasonキーと、連続する共通メタデータのID列再利用。Mapありの改善が明確でなく、大きな描画でも僅かな悪化を観測した。
- 既存Contribution列を順序付きの線形比較で証明して検索を省く処理。理論上の計算量だけで採用せず、実測で効果が確認できないため戻した。

各試案のpatch・テスト・WASM・全pilot sampleはlocal-validationに保持する。最終製品にはこれらの追加構造やキャッシュは残さない。

## 3. 全コーパスの新旧比較

4,155固有原文を既存のruntime property条件で比較。全探索成功4,054本、8192目標成功4,128本を別集計し、既知のトラップ7原文は別プロセスで検証する。全コーパスは各variant1回の広い観測であり、個別入力の最大時間の保証ではない。

単位ms/原文、同じ成功原文の平均。

| 条件 | 本数 | 旧Mapなし | 新Mapなし | 通常の短縮率 | 旧Mapあり | 新Mapあり | Mapあり短縮率 | 新しい比率 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| exhaustive / all-success | 4,054 | 49.869 | 49.491 | 0.8% | 56.102 | 56.290 | -0.3% | 1.137 |
| target8192 / all-success | 4,128 | 5.374 | 5.320 | 1.0% | 6.148 | 6.088 | 1.0% | 1.144 |
| target8192 / source-checkpoint | 4,089 | 0.372 | 0.355 | 4.6% | 0.482 | 0.499 | -3.6% | 1.406 |

比率の分母は同じ新実装のMapなしである。通常側がより速くなれば、Mapありの時間が短くなっても倍率は上がる。負の差や外れ値を隠さず、コード・Map・診断が同じ対応ペアで集計した。

## 4. 44シーンの反復

小入力・空・コメントのみ・Unicode/CRLF・property固定・exact/safe・字句短縮・多数block・実コード・巨大制御・巨大描画・不正入力を含む22原文×2設定。10KB超は7回、それ以下は21回。等しいwarmup後、旧新とMap有無の順序を入れ替えて測定した。

時間にはWASM呼出から生成Luaと完全なMap JSONが返るまでを含む。原文I/O、初期化、事前GC、外部からのvalidateSourceMap、結果比較は時間外で、両版に同条件。共有VMのJIT・負荷による変動は残る。

| シーン・設定 | 旧Mapなし | 新Mapなし | 旧Mapあり | 新Mapあり | 新比率 |
| --- | ---: | ---: | ---: | ---: | ---: |
| empty / exhaustive | 0.255 | 0.234 | 0.310 | 0.296 | 1.266 |
| empty / target8192 | 0.072 | 0.067 | 0.110 | 0.109 | 1.630 |
| tiny-callback / exhaustive | 0.388 | 0.383 | 0.462 | 0.439 | 1.145 |
| tiny-callback / target8192 | 0.077 | 0.072 | 0.118 | 0.115 | 1.593 |
| dense-lexical / exhaustive | 0.832 | 0.904 | 4.143 | 4.043 | 4.471 |
| dense-lexical / target8192 | 0.823 | 0.846 | 3.945 | 3.930 | 4.643 |
| rw-23 / exhaustive | 13.056 | 12.949 | 15.252 | 15.195 | 1.173 |
| rw-23 / target8192 | 0.159 | 0.162 | 0.229 | 0.222 | 1.374 |
| rw-02 / exhaustive | 697.986 | 699.581 | 750.173 | 752.103 | 1.075 |
| rw-02 / target8192 | 1.259 | 1.257 | 1.684 | 1.657 | 1.318 |
| vehicle-control / exhaustive | 1,189.785 | 1,172.283 | 1,316.832 | 1,301.707 | 1.110 |
| vehicle-control / target8192 | 189.447 | 189.659 | 228.261 | 228.398 | 1.204 |
| draw-4096 / exhaustive | 1,034.438 | 1,023.573 | 1,815.199 | 1,798.477 | 1.757 |
| draw-4096 / target8192 | 1,048.991 | 1,030.484 | 1,890.949 | 1,877.821 | 1.822 |
| corpus-4028de71b17c / exhaustive | 15.806 | 15.892 | 75.362 | 73.318 | 4.614 |
| corpus-4028de71b17c / target8192 | 15.867 | 15.665 | 73.463 | 72.337 | 4.618 |

成功40条件のうち通常側の短縮は27条件、Mapありは29条件。1.2倍以下は10条件、1.1倍以下は1条件、最大中央値比は4.704倍である。悪化した観測を除いて「全て速くなった」と扱わない。

### 負の観測と対照条件の追加反復

最初の3%以上の悪化と所定の対照を追加warmup・反復で測り直した。初回を消さず別の結果として保存する。

| シーン | 条件 | 反復 | 通常短縮率 | Mapあり短縮率 | 比率 |
| --- | --- | ---: | ---: | ---: | ---: |
| empty | exhaustive | 31 | -1.7% | -6.5% | 1.232 |
| empty | target8192 | 31 | -2.6% | 5.8% | 1.445 |
| tiny-callback | exhaustive | 31 | 2.8% | 2.8% | 1.198 |
| tiny-callback | target8192 | 31 | 2.0% | 3.6% | 1.445 |
| extended-lexical | exhaustive | 31 | -0.6% | -1.7% | 1.508 |
| extended-lexical | target8192 | 31 | -1.9% | 1.6% | 1.536 |
| dense-lexical | exhaustive | 31 | -4.0% | 0.9% | 4.725 |
| dense-lexical | target8192 | 31 | -3.0% | 1.2% | 4.647 |
| rw-23 | exhaustive | 31 | -0.3% | 0.2% | 1.169 |
| rw-23 | target8192 | 31 | -1.9% | -0.9% | 1.379 |
| corpus-a37299b70f1f | exhaustive | 31 | -1.5% | -0.4% | 4.699 |
| corpus-a37299b70f1f | target8192 | 31 | -1.6% | -0.8% | 4.541 |

## 5. analyze / build

固定された通常module・複数module・LifeBoat includeの返却値を新旧比較した。初回各9反復、負の観測は31反復の追加確認も記録する。非短縮linkはMap指定なしでも従来の標準mapを返すため、詳細mapとの比較は単純なMap有無と異なる。

| 入力 | 操作 | 旧ms | 新ms | 短縮率 |
| --- | --- | ---: | ---: | ---: |
| rw-23 | analyze | 0.288 | 0.284 | 1.4% |
| rw-23 | link | 0.364 | 0.346 | 4.9% |
| rw-23 | link-map | 0.298 | 0.286 | 3.9% |
| rw-23 | build-minify | 14.435 | 18.942 | -31.2% |
| rw-23 | build-minify-map | 16.165 | 17.364 | -7.4% |
| rw-02 | analyze | 1.977 | 2.012 | -1.8% |
| vehicle-control | analyze | 2.094 | 1.982 | 5.3% |
| vehicle-control | link | 3.073 | 3.065 | 0.3% |
| vehicle-control | link-map | 2.369 | 2.333 | 1.5% |
| draw-4096 | analyze | 10.095 | 9.983 | 1.1% |
| draw-4096 | link | 19.255 | 19.199 | 0.3% |
| draw-4096 | link-map | 12.785 | 12.793 | -0.1% |
| project-1-modules | analyze | 0.098 | 0.095 | 3.3% |
| project-1-modules | link | 0.173 | 0.133 | 23.0% |
| project-1-modules | link-map | 0.204 | 0.197 | 3.4% |
| project-1-modules | build-minify | 2.233 | 2.232 | 0.0% |
| project-1-modules | build-minify-map | 2.681 | 2.652 | 1.1% |
| project-16-modules | analyze | 0.268 | 0.255 | 4.9% |
| project-16-modules | link | 0.427 | 0.417 | 2.3% |
| project-16-modules | link-map | 0.546 | 0.549 | -0.6% |
| project-16-modules | build-minify | 9.034 | 9.033 | 0.0% |
| project-16-modules | build-minify-map | 11.687 | 11.649 | 0.3% |
| project-64-modules | analyze | 0.640 | 0.629 | 1.7% |
| project-64-modules | link | 1.415 | 1.406 | 0.6% |
| project-64-modules | link-map | 2.981 | 2.891 | 3.0% |
| lifeboat-16-includes | analyze | 0.215 | 0.207 | 3.9% |
| lifeboat-16-includes | link | 0.641 | 0.609 | 5.1% |
| lifeboat-16-includes | link-map | 0.655 | 0.622 | 5.0% |

## 6. 正確性とQA

Native workspace **845件**、SDK **30件**、実WASM **83件**、Playground/CLI **38件**が成功。固定240設定、未達target240比較、新旧reader480回、Worker混成6経路、通常/LifeBoat8ビルドを確認した。

全コーパス8,182成功組の生成Lua、診断、検索統計、pass記録、全map JSONが一致。Map比較から除いたのはproducerと、それに依存するintegrityだけ。失われた情報を粗い親位置で埋めたり、理由を間引いたりしない。既知の7原文のトラップは56条件で新旧同じ結果で、今回の修正対象ではない。

fmt、default、clippy、rustdoc、architecture、Python、画面fixture、権利表示、独立パッケージconsumerが通過。Browser pluginがないため既存Playwrightを利用し、localの専用routeをChromium・Firefox・WebKitで各16例、Map双方向選択・理由・保存復元・実VM停止/ログ/エラーを検証。console/page errorは0件。デスクトップと390px幅も確認した。UI/CSP・本番配備は変更しない。Native件数はlocal Linuxで、hosted 3 OS CIは今回は未実行。

## 7. 保存状態と継続条件

- `4382d8d`: docs: record measured traversal and lookup optimization scope
- `a40098e`: perf: classify lexer keywords from the canonical vocabulary
- `5f1f58f`: perf: classify proven builtins by exact borrowed name components

基準WASM `4703eda1dfb2b4e57d47e37f025864062b631b4a3448eafe3a93e7a0ef7bb879`、今回WASM `c47911b9bbcc490bbe89811d627695c6d70880c9f1ed369dbffd31a19db385d3`。producer.dirty=false、生成版はrevisionで識別する。パッケージ表記は開発中の0.3.0のまま、v0.3.1が公開済みとは扱わない。

commitのみ行い、push・main/release・tag・npm・本番更新は行っていない。原文・sourcesContentは共有成果物へ含めない。全sample・失敗・不採用案・再実行scriptはlocal-validationへ保持する。

比率1.2（理想1.1）の条件は継続する。次も、Map固有費用だけでなく通常のparse/解析/探索/印字を含めて改善する。無効側を遅くする、出力の一部を後で生成する、理由や精度を削る、探索を時間で打ち切る方法は採用しない。
