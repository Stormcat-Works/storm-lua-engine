# v0.3.1 minify / build / lint 性能改善計画

2026-10-02。v0.3.1は**短縮時間の最適化を主目的**とする。対象はminify全体で、同じ解析・AST・印字・リンク基盤にあるlint/analyze/buildの高速化も歓迎する。特に `sourceMap:true` の追加コストを削減する。

## 優先順位

1. **minifyの総時間**
   - `sourceMap:false` を最優先の通常経路として維持する。
   - `sourceMap:true` は位置・理由・schema1を維持したまま、追跡・最終map生成の追加コストを下げる。
2. **探索・候補評価**
   - 同じASTの再walk、同じprinter出力、同じresolution/analysis、捨てる候補のclone・metadata更新を減らす。
   - target未達時は最大探索と同一出力を返す現在の契約を変えない。
3. **lint/analyze/build**
   - parser/tokenization/resolution/linkでminifyと共有できる計算を重複しない。
   - ただしキャッシュのために新しい永続APIや複雑な増分コンパイラを投機的に追加しない。
4. **Source Map**
   - 候補探索中の由来clone、最終Printer/flatten、Source Map v3とx_stormの重複計算、JSON構築/validateをprofileする。
   - Token/Name/Expression/Statement精度、理由、関連元、inline context、dispositionを削らない。

## 正確性の固定条件

性能改善で次を変更しない。

- 生成Luaと候補順・target達成判定。
- safe/smallest、exact/tolerant、property、environment、host bindingの意味論。
- 全67最適化パスの有効条件。
- `sourceMap:true` と `false` の生成Lua一致。
- Source Map v3、`x_storm.schemaVersion:1`、producer/指紋、理由・関連元・inline context・削除記録。
- Unknownを近傍位置や広い親位置で埋めない規則。
- 時間で探索を打ち切る方式を追加しない。

## ベースライン

公開v0.3.0のソースcommitを基準に同一VMで測る。計測自体のI/O、WASM初期化、fixture読み込みはcompile時間と分ける。

### minify

代表入力:
- 小〜中規模の実コード群（固定30入力）。
- `vehicle-control`: 約2万文字の制御例。
- `draw-4096`: 約13万文字の描画例。

設定:
- safe/smallest × exact/tolerant × zeroCostNewlines on/off。
- smallest+tolerantで全探索。
- targetSize=8192。
- target未達（fast/beam1を含め最大探索と同一結果を確認）。
- Source Map off/on。

主指標:
- compile wall time（複数回の中央値）。
- map有効時の倍率。
- 生成サイズ、candidateSizes、出力SHA-256。
- map bytesとvalidation time。
- 必要な場合はpeak RSS。

### lint / analyze / build

同じ原文を使い、
- analyze/lintのみ。
- 単一module build（非短縮）。
- 複数module link build（非短縮）。
- project build + minify。

を測る。改善できる共有ホットパスが見つかった場合だけ対象へ含める。

## profileの進め方

Native release binaryを主なprofile対象とする。WASM/Nodeは最終的な利用者観測値として別に測る。

1. `perf record/report`でminify全体の上位関数を確認。
2. stage単位:
   - parse / resolve / initial transforms
   - layout / search
   - pass trial
   - Printer / size evaluation
   - source provenance
   - final Source Map / JSON
3. pass単位:
   - 合計時間
   - 呼出回数
   - 採用されなかった候補に使った時間
4. sourceMap ON/OFF差分を同一入力で比較する。

## 最初に確認する既知の候補

v0.3.0で既に観測されたもの:

- ordered screen-call factoring
- affine / translated wrapper merging
- layout前の長いpre-layout処理
- provenance付き候補clone
- Printerによるサイズ評価の繰り返し
- resolution / walk / expression_sizeの再計算
- final map flatten / JSON / validator

ただし、過去profileだけで変更先を決めず、v0.3.1開始時の同一commitで再測定する。

## 実装ルール

- まず最も大きいコストから直す。
- 変更単位ごとにbefore/afterを同条件で測る。
- 速度のために候補を減らす変更は、同じ結果を保証できる重複排除の場合だけ許容する。
- memoization/cacheは、keyが最適化意味論を完全に表現できる場合のみ採用する。
- map無効時に由来用分岐やallocationを増やさない。
- map有効時も、最終候補に不要な由来生成は避ける。
- public schema/API変更は原則行わない。必要になった場合は性能変更と混ぜず別判断にする。

## v0.3.1の性能目標（2026-10-03確定）

同じ入力・設定・実行環境で、**Source Mapありのminify総時間を、mapなしの最大1.2倍以下**にする。mapありの時間には追跡・標準mapと理由付きJSONの生成まで含める。生成後のホストによるvalidateは別途測定し、コンパイラ内部の必須検証を計測から外さない。

評価は固定代表30入力の全設定、および既存の巨大制御/描画・字句短縮例を対象にする。条件ごとに順序を交互にして複数回測定し、中央値の比率を算出、**その最大値**も報告する。平均だけ、重い入力だけ、全探索だけを示して全体の1.2倍達成と扱わない。小入力やtarget早期返却も除外しない。絶対時間と生map容量を併記する。

この目標のためにmapなしを遅くしたり、cacheをmapありの計測だけに与えたり、精度や根拠を削ることは禁止する。1.2倍は実装の受入目標で、OS負荷を含む任意入力の実行時間保証ではない。未達が残れば入力・設定を記録し、達成したと報告しない。

通常minifyの短縮、analyze/buildの共有ボトルネック改善も進める。性能以外のAPI/schema非互換変更は抱き合わせない。

## 検証

最終候補では最低限:

- 固定30入力・240設定。
- target未達と最大探索の同一出力。
- 全67パス由来契約。
- map ON/OFF同一Lua。
- v0.3.0との出力・map意味論比較（意図的なproducer/version差を除く）。
- Native workspace、clippy、rustdoc、architecture。
- SDK、実WASM、compiler Worker。
- Chromium / Firefox / WebKit。
- 独立npm tarball consumer。

性能記録には計測host・revision・binary/WASM SHA-256・反復回数・中央値・ばらつき・入力SHAを残す。

## v0.3.1以降へ送るもの

- schema非互換なmap再設計。
- binary source-map format。
- 分散/並列探索。
- 永続incremental compiler cache。
- 元変数値・frame・PC等の追加デバッグ情報。
- 同一結果を保証できない探索剪定。

これらは今回の短縮時間改善へ抱き合わせない。
