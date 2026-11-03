# 検証記録: deferred rename（none レイアウト先行、w7 第104サイクル、第286ラウンド）

対象コミット: `ad5b11d`（配線）+ `2a8f237`（band 確定）。基底は第285ラウンド先端 `09a9985`、Tier FULL 累積 base は FULL-VERIFY-TIP `d97f193`。

## 設計

- 対象は target-free コンパイルの plain `none` レイアウトのみ（joint-api 族は第101サイクルの測定で敗者タイト率が高く後回し。`none-barrier-relaxed` の破棄 detach は第103サイクル census で 25 calls / 30,006 elems と僅少なため対象外）。
- `none` を未 rename のまま構築し、Phase-1 finalize（rename 系パスへは `false` を渡し、サイズ測定は第285ラウンドの mapping-aware print で正確に測る）で見積もる。他レイアウト確定後、Phase-1 サイズが最良他候補 + `DEFERRED_NONE_BAND` 以内（または他候補が残らない）場合に限り、stock 経路で再 finalize して正確な候補を出す。除外された `none` は候補を出さない（candidate_sizes 統計からも消える。これは統計上の帰結として記録する）。
- レイアウトが `none` のみのジョブでは deferral しても rename は勝者の自己コストでしかないため、deferral 自体を無効化する（構築時 rename と等価）。
- 実装は finalize 本体の関数抽出（`finalize_one_layout`、第101サイクルの抽出前例）＋ mode 切替。target 付きコンパイル経路は不変。

## 機構の正確性（band 無限大チェック）

`DEFERRED_NONE_BAND = 1,000,000`（常時再 finalize）で FULL コーパス（4,155 ファイル × 4 設定 = 16,620 行）を実行し、stock ベースライン（`d97f193` コード先端と等価な行集合、整列 SHA-256 `9713a05d…`）と**バイト一致（diff 0 行）**。抽出と再 finalize 機構自体が stock と等価であることの実証。

## Divergence census（probe 計測、Sequential ドライバ、両 nomap 設定族）

- 17,480 evaluate 行（default-nomap 8,740 + std-nomap 8,740）、全行で Phase-1 サイズ p と再 finalize サイズ s が揃った。
- δ = s − p: 中央値 0、第1百分位 −210、最小 −2,177、最大 +102。非ゼロは 6,264 行（35.8%）、発散のあるファイルは 1,533 / 4,123。
- Phase-1 finalize の scope-rename detach は全行合計で **0 elems / 0 calls**（Phase-1 は detach を一切払わない）。
- census 範囲の `none` stock detach 合計は 84,555,960 elems。
- 勝者・同着行（s ≤ best_other）3,956 行が band を制約する。最大要求は **683**（ファイル `dd541d0df0d0…`、optimized:locals、両設定族とも。p=3,599、s = best_other = 2,916 の同着行）。上位要求（683 / 310 / 298 / 272）はすべて同着行で、機構は「同着で Phase-1 が過大見積りし、除外すると勝者名が同点の他レイアウトへ移る」もの。
- 最大の下向き発散（δ ≈ −2,177）はファイル `971f17b2236b…` に集中するが、当該行は best_other が s より小さく勝者制約にならない（除外は純粋な節約、約 154k elems/行）。

## Band 選定と detach 重み付き削減見積もり（census 範囲、per-evaluate 集計）

| band | 除外行数 | 節約 elems | none stock detach 比 | 勝者反転行 |
|---|---|---|---|---|
| 160 | 3,794 | 27,029,112 | 32.0% | 18 |
| 683（census 最小） | 470 | 14,704,250 | 17.4% | 0 |
| **768（採用）** | **338** | **14,099,160** | **16.7%** | **0** |

採用値は census 最大要求 683 に小さいマージンを加えた 768。経験値であり証明ではない（FULL ゲートが受入判定）。

## Tier FULL ゲート

- band=160 の試行では 16 行（8 ファイル × default-map / std-map）が差分となった。内訳は勝者コード・サイズ・core map ともバイト同一で、map の full ハッシュのみ相違。JSON 直接比較で `x_storm.selection.layout` が `none` → `joint-api` に移った同着反転と特定した（census の B=160 反転予測 8 ファイルと完全一致）。**反転は黙って受け入れず**、band を census 由来の 768 に確定して解消した。
- band=768 で FULL 再実行: **16,620 行バイト一致（diff 0、整列 SHA-256 `9713a05d…`）、フォールバック 0**。
- workspace テスト: 50 スイート合計 **1,729 passed / 0 failed**（minify lib 1,119、syntax lib 144 を含む）。

## .us2 壁時計（測定専用ホスト、c101time、208 ファイル部分集合、全 4 設定、3 交互 rep）

| rep | stock (µs) | 本設計 (µs) |
|---|---|---|
| 1 | 54,977,656 | 61,441,819 |
| 2 | 55,055,220 | 61,688,585 |
| 3 | 55,083,372 | 61,450,000 |
| 合計 | 165,116,248 | 184,580,404 |

**+11.8%（悪化）**（全 rep で一貫、ステータスは全 rep・全行一致、824 ok / 8 err 行/側）。

【訂正 2026-11-01】初版に記載した −5.17% は証跡 `us2-result.txt` に存在しない誤記であり、本表が実測である。悪化の機構: B=768 で実際に除外されるのは 338/17,480 evaluate（1.9%）に過ぎず、複数レイアウトの全 evaluate が none の Phase-1 finalize を追加で払うため、detach 節約が壁時計では逆行した（第101サイクルと同型）。本設計の出力・選択は FULL バイト一致のとおり stock と同一であり、本節の訂正は性能評価のみに関わる。

## 残課題

- joint-api 族の deferred rename は未着手（第101サイクルのタイト率所見の再評価が前提）。
- band は corpus 固定の経験値。新規 corpus / コード変更時は FULL ゲートで再検証される。

## 本設計の revert（第287ラウンド、w7 第105サイクル、2026-11-01）

- **判断**: 本設計（配線 `ad5b11d`＋band `2a8f237`、第286ラウンド）を revert した。理由は性能評価のみである。.us2 壁時計の真値は **+11.8% 悪化**（上記 .us2 節の訂正参照）であり、「壁時計で悪化する deferral 設計は不採用」という第101サイクルの先例に従った。本設計の出力・選択は stock とバイト同一であり、正確性の問題は一切ない。
- **失敗機構**: B=768 で実際に除外されるのは 338/17,480 evaluate（1.9%）に過ぎない。複数レイアウトを持つ全 evaluate が none の Phase-1 finalize（パス列＋mapping-aware print）を追加で払い、勝者・同着行では stock finalize も重なる。detach 節約 14,099,160 elems（census 範囲の none stock detach の 16.7%）は実在するが、detach はコンパイル CPU の一部でしかなく、壁時計では追加 finalize が勝った。第101サイクル（joint-api 先行、+18.8% で不採用）と同型の失敗である。
- **revert の内容**: `2a8f237` と `ad5b11d` を新規コミットで revert（履歴書き換えなし）。revert 後の `crates/` は第285ラウンド先端 `09a9985` とバイト単位で一致する（diff 0 行）。第285ラウンドの mapping-aware print プリミティブ（`e64b44e`）は存続する。
- **再検証（Tier FULL、累積 base `2a8f237`）**: base（revert 前）・head（revert 後）とも 16,620 行、整列 SHA-256 `9713a05d5ab6f9d497510fa48a8aafc9693a13742cab8e1f98f1bfd0fe4b8548`、diff 0（status は ok 4,128／error 27 ×4 設定）。workspace テスト **1,729 passed / 0 failed**（50 スイート、revert 前と同数）。
- **.us2 壁時計の再確認**（c101time、同一 208 ファイル部分集合、全 4 設定、3 交互 rep。base = `2a8f237`＝本設計あり、head = revert 後）:

| rep | base (µs) | head (µs) |
|---|---|---|
| 1 | 61,269,987 | 55,188,203 |
| 2 | 61,262,835 | 55,372,459 |
| 3 | 62,143,274 | 54,977,883 |
| 合計 | 184,676,096 | 165,538,545 |

  生行（us2-result.txt より）: `TOTAL base rep1 micros=61269987 ok=824 err=8` / `TOTAL head rep1 micros=55188203 ok=824 err=8` / `TOTAL base rep2 micros=61262835 ok=824 err=8` / `TOTAL head rep2 micros=55372459 ok=824 err=8` / `TOTAL base rep3 micros=62143274 ok=824 err=8` / `TOTAL head rep3 micros=54977883 ok=824 err=8`。ステータスは全 rep・全行一致（832 行）。head は base 比 −10.4% で、第104サイクルの stock 合計 165,116,248µs に対して +0.26%（ノイズ域）＝**悪化は解消し stock 水準に復帰**。参考: base 合計は第104サイクルの本設計合計 184,580,404µs と +0.05% で一致し、測定の再現性も確認できた。

## 残課題（第287ラウンドで更新）

- joint-api 族の deferred rename と (A) 勝者具体化（rename なし具体化）は、**Phase-1 finalize の追加コストが detach 節約を上回るという上記の失敗機構を覆す設計変更がない限り、再提案しない**。detach 削減見積もりだけを根拠に着手しないこと。
- mapping-aware print プリミティブ自体は存続し、将来の設計が正確な Phase-1 サイズ計測を必要とする場合の基盤として残る。
