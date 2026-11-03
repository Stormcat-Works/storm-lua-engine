# 検証記録: tiny-literal-inlining の in-place 試行評価（w7 第110サイクル）

対象コミット: `4d5b89f`（perf(minify)）。基底は第296ラウンド先端 `b35abfd`、Tier FULL 累積 base は FULL-VERIFY-TIP `8e9fefc`。第109サイクルの試作（親承認済み）の本実装。統合は hub 復旧後の段取りで行うため、本記録コミット時点では未統合・FULL-VERIFY-TIP は不変。

## 設計

- `inline_tiny_literal_bindings` は候補ごとに arena 全体を clone し、計測して勝者の clone だけを残していた。本変更ではラウンド内に試行 arena を1本だけ保持し、候補ごとに `replace_binding_with_literal`＋`remove_local_binding` を in-place 適用 → `measure_size` → 試行が書いた全スロットを pristine な round source から復元する。触れるスロットの同定は `tiny_trial_touched_slots`（読み箇所は replace と同じ述語、宣言文とそれを含むブロックは remove 側の書き込み先）。
- 勝者は試行 arena をコミットせず、同じ編集関数を live arena へ replay して確定する。編集関数は (arena の値, source, res) に対して決定的で、live arena は round source の値を保持しているため、コミットされる arena は nodes・provenance とも従来の fresh-clone 試行と一致する。
- 候補列挙の順序・絞り込み・gain 述語は無変更で、勝者選択は不変。
- 試作からの差異はコメント整形と rustfmt のみ（コードは試作と同一）。字義どおり差分が変わったため .us2 は再測定した。

## Tier FULL ゲート

- 累積 base `8e9fefc`: base・head とも 16,620 行（4,155 ファイル × 4 設定、ok 4,128／error 27 ×4）、整列 SHA-256 は両側とも `28f4530832f052a7c5f9f2b617f0ca9509f565e9fe11a275bdfc385b40f2759d`、**diff 0（バイト同一）**、フォールバック 0。
- workspace テスト: **1,758 passed / 0 failed**（50 スイート＋doc-tests）。`cargo xtask check` exit 0。fmt は変更ファイル無指摘、clippy（workspace）は base／head で警告集合が同一（変更ファイルへの指摘 0）。
- minify lib テスト 1,140 件全通過。

## 決定的計数（std-nomap、c99probe 計装プローブ、出力行は FULL head の std-nomap 行と整列 SHA 一致を確認）

| 指標 | stock（第296ラウンド先端） | 本変更適用後（`4d5b89f`） |
|---|---:|---:|
| detach 総量 elems | 216,293,772 | 201,781,380 |
| core:tiny-literal-inlining | 18,888,213 | 4,375,821 |

- 同パス detach −14,512,392 elems（−76.8%）、総量 −6.71%。残存分は勝者 replay が live arena の共有ページを書き換える際のページコピーと、ラウンドごとの初回 detach で、機構どおり。第109サイクル試作の計測値と完全一致した。

## .us2 壁時計（c101time、831 ファイル × 4 設定、3 rep 交代）

base = `8e9fefc`（FULL-VERIFY-TIP）、head = 本実装 `4d5b89f`。同一ジョブ（tar SHA-256 `d900dc7f…`）を複数回提出し、完走した2回の生 TOTAL 行を転記する（いずれも完了確認後に VM・.wsl の2点で結果ファイルの SHA-256 一致を確認。ステータスは全 rep・全側で一致、3,324 行、ok=3304／err=20）。

run1（開始時 load 1.01 → 終了時 1.22）:

```
TOTAL base rep1 micros=217240529 ok=3304 err=20
TOTAL head rep1 micros=221956037 ok=3304 err=20
TOTAL base rep2 micros=215971373 ok=3304 err=20
TOTAL head rep2 micros=215049872 ok=3304 err=20
TOTAL base rep3 micros=215129383 ok=3304 err=20
TOTAL head rep3 micros=217855952 ok=3304 err=20
```

- 合計: base 648,341,285µs → head 654,861,861µs（+1.01%、rep 別 +2.17／−0.43／+1.27%）。

run2（開始時 load 1.22 → 終了時 1.08）:

```
TOTAL base rep1 micros=220985152 ok=3304 err=20
TOTAL head rep1 micros=216742592 ok=3304 err=20
TOTAL base rep2 micros=215313163 ok=3304 err=20
TOTAL head rep2 micros=217431277 ok=3304 err=20
TOTAL base rep3 micros=216216082 ok=3304 err=20
TOTAL head rep3 micros=215578268 ok=3304 err=20
```

- 合計: base 652,514,397µs → head 649,752,137µs（**−0.42%**、rep 別 −1.92／+0.98／−0.30%）。
- 第109サイクル試作時の同一ジョブは −0.47%（base 649,853,985 → head 646,802,471µs、低負荷時）。全3サンプルの符号は測定時のホスト負荷（load 1.0〜1.2 の他負荷あり）に応じて ±1% の範囲で揺れており、壁時計の悪化は成立していない。採用は決定的 detach の消滅に依拠する。
- 計測経緯の注記: 本サイクルの .us2 は混雑しており、初回提出は base rep1 までで停滞、複数回の再提出は MEASURE.lock の30分待ちや接続断（broken pipe）に遭った。完走した上記2回のみを正式結果とし、全 TOTAL が 0 となった無効な完走出力1件は破棄した。詳細はサイクル記録 C110.md の環境・事故節に残す。

## 統合

- 未統合（本サイクルはマージを行わない方針）。統合時は本記録コミットまでを ff し、FULL-VERIFY-TIP をコード先端 `4d5b89f` へ前進させる。
