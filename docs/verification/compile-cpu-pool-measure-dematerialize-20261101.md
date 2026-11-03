# 検証記録: プール幅試行・boolean 試算の計測脱具体化（w7 第107サイクル、第295ラウンド）

対象コミット: `d8a3ee5`（perf(minify)）。基底は第294ラウンド先端 `eda4e86`、Tier FULL 累積 base は FULL-VERIFY-TIP `cb77237`。第106サイクルの試作（候補1+2、親承認済み）の本実装。

## 設計

- literal-pool / string-pool の幅試行は、幅ごとに `scope_rename_fast` で具体化してから `measure_size` していた。本変更では各幅のサイズ判定を `measure_renamed_size_unmemoized`（改名計画＋名前解決 measure、具体化なし）へ置換し、勝者幅だけを最後に1回 `scope_rename_fast` で具体化する。
- boolean_numerics の `measured()` は改名後 AST を破棄する試算専用であり、clone＋rename＋measure の全量を同じプリミティブで置換した。
- 勝者選択の不変性: `measure_renamed_size_unmemoized` が具体化後の measure と等しいことは scope_rename の単体テストで保証されており、全幅の判定サイズが従来と同一 → 勝者は構成的に同一。勝者 AST は従来と同一関数・同一入力で生成される。

## Tier FULL ゲート

- 累積 base `cb77237`: base・head とも 16,620 行（4,155 ファイル × 4 設定、ok 4,128／error 27 ×4）、整列 SHA-256 は両側とも `28f4530832f052a7c5f9f2b617f0ca9509f565e9fe11a275bdfc385b40f2759d`、**diff 0（バイト同一）**、フォールバック 0。
- なお開始時点の累積 base `81ab6ac` でも先行して同一ゲートを実施し、両側 `9713a05d…` でバイト一致を確認済み（第290–294ラウンドの着地に伴い base を 2 回差し替えて再実施した。途中の base `f938d38` でも両側 `c990e7dc…` でバイト一致を確認済み）。
- workspace テスト: 50 スイート合計 **1,750 passed / 0 failed**。`cargo xtask check` exit 0。fmt は既存ドリフト 10 件（変更ファイルへの指摘 0）、clippy（workspace lib）は変更ファイルへの警告 0。

## 決定的計数（std-nomap、c107 計装プローブ、出力行は stock とバイト一致を確認）

| 指標 | 第106サイクル stock（`64374d1`） | 本変更適用後（`c73973e`、本変更と同一内容。R294 着地前の先端で計装計測） |
|---|---:|---:|
| detach 総量 elems | 258,622,672 | 216,291,324 |
| scope_rename detach elems | 73,935,538 | 31,721,314 |
| literal-pool サイト | 28,107,224 | 6,842,064 |
| string-pool サイト | 18,063,646 | 2,970,014 |
| boolean measured サイト | 5,847,192 | 0 |

- 第106サイクルの試作計測（detach −42,202,149 elems）と整合する。

## .us2 壁時計（変更単体の切り分け計測、c101time、831 ファイル × 4 設定、3 rep 交代）

base = `81ab6ac`（着手時点の FULL-VERIFY-TIP）、head = 本変更のみを載せた `4a26b83`（rebase 前の同一内容）。生 TOTAL 行をそのまま転記する（結果ファイルは完了確認後に VM・.wsl の2点で SHA-256 `e8400931…` 一致を確認）:

```
TOTAL base rep1 micros=219139478 ok=3304 err=20
TOTAL head rep1 micros=222314652 ok=3304 err=20
TOTAL base rep2 micros=216242086 ok=3304 err=20
TOTAL head rep2 micros=213577020 ok=3304 err=20
TOTAL base rep3 micros=215741010 ok=3304 err=20
TOTAL head rep3 micros=214100941 ok=3304 err=20
```

- 合計: base 651,122,574µs → head 649,992,613µs（**−0.17%**）。rep 別は +1.45%／−1.23%／−0.76% とばらつきが大きく、第106サイクル試作時の ≈−0.9% より小さいが、事前登録した悪化条件（head 合計 ≥ base 合計）には該当しない。ステータスは全 rep・全側で一致（3,324 行、ok=3304／err=20）。
- 計測経緯の注記: 初回提出は提出セッションの切断で結果ストリームが途絶し、部分スナップショット（TOTAL 4行まで）は採用せず保全のみとした（`c107/us2-result-partial-snapshot.txt`）。上記は同一ジョブ tar（SHA-256 `278201cc…`）の再提出による完走結果である。
- 統合後の再確認（base = `cb77237`、head = 統合先端コード）も同一手順で実施し、結果は本サイクル記録（C107.md）に残す。悪化した場合は差し戻し判断を親へ仰ぐ。

## 統合

- MERGE.lock の短臨界区間で先端 CAS（`eda4e86` 不変）を確認し ff。統合先端は本記録コミット。**FULL-VERIFY-TIP はコード先端 `d8a3ee5` へ前進**。
