# コンパイル CPU: temporary_globals のハッシャ差し替え＋evaluate 内訳計装 — 検証記録

対象: `crates/storm-lua-minify/src/passes/temporary_globals.rs`
担当: w7 第87サイクル（軸: std 構成の exhaustive 探索のコンパイル CPU）

## 課題 (1): temporary_globals のハッシャ差し替え（第86サイクル監査の実装編）

第86サイクルの監査で、`temporary_globals.rs` の整数キー写像
（BindingId／ScopeId＝u32、usize の色キー）については反復順序が
出力に観測できないことが確定した。本サイクルは監査の前提条件
P1〜P4 を厳守して差し替えを実装した：

- P1: DSATUR の比較関数（最終 tie-break＝`position` の全順序）には
  一切触れていない。
- P2: 保険として、色の intern（`__gs{color}` 名の `strings.fresh`）
  を色の昇順に固定した。従来は色集合の反復順（プロセスごとに
  変動しうる）で intern していた。
- P3: 差し替え範囲は本ファイルの整数キー写像に限定した。他パスの
  HashMap 群には触れていない（P4： それらは別監査が前提）。
- 実装： ファイル内に局所的な FxHash 系ハッシャ
  （rotate-left 5＋乗算、`storm-lua-syntax/src/provenance.rs` と
  `storm-lua-build/src/optimized_source_map.rs` の既存前例と同型、
  新規依存なし）を定義し、本ファイルの全 `HashMap`／`HashSet` を
  `FastMap`／`FastSet`（`BuildHasherDefault<FastHasher>`）へ置換した。
  写像・集合の内容と全ての判定は不変で、変わるのはキー ハッシュの
  演算のみ。

### 決定論カウント（std-nomap、全 corpus 4,155 ファイル、同一計装を両側に適用）

計装は両側とも本ファイルの写像に数え取り用の BuildHasher ラッパを
挟み、`hash_one` 回数（＝`build_hasher` 呼出回数、成長時の再ハッシュ
を含む）を数えた。プローブの出力行（std-nomap 4,155 行）は
corpuscheck の FULL 実行の std-nomap 行と両側とも完全一致し、
計装の忠実性を確認した。

| 側 | ハッシュ演算回数（std-nomap 1 パス） | ハッシャ |
|---|---|---|
| base `4152d1e` | 450,657,284 ／ 451,506,235（2 回） | SipHash（std 既定） |
| head | 455,726,638（2 回とも同一） | FxHash 系（本変更） |

base の 2 回で約 0.19% ぶれるのは RandomState のシードで反復順序が
変わり、到達可能性探索の早期終了位置などが動くため。head は
決定論ハッシャのため完全再現する。演算回数そのものは同水準
（演算の総量は内容で決まる）で、消滅するのは **SipHash による
キー ハッシュ約 4.51×10^8 回／std-nomap コーパス 1 パス** であり、
1 回あたり数ワードの乗算・回転に置き換わる。第83サイクルの
callgrind では SipHash の `hash_one` がコンパイル self の約 11%
だった。

### 検証（合格条件： Tier FULL＋E3 再実施）

- **Tier FULL**： FULL-VERIFY-TIP `49d57e3` を base とし、head との
  累積比較（4 構成 × 4,155 ファイル＝16,620 行）。base／head とも
  ソート済み TSV の SHA-256 が `743431aa…` で**バイト一致**
  （fallback 0、batch-killed なし）。`compare-corpus.py` でも
  全構成 identical＝4,155、hard／map-core／expl-only／missing
  いずれも 0、ステータス対応は ok 4,128／error 27 で両側一致。
  第238・239ラウンド（Tier INVARIANT）を含む累積でも出力不変。
- **E3 再実施**： 第86サイクルの合成入力 3 件（スロット 5／6／9）に
  ついて、full／packonly 両構成 × 40 プロセスで生成コードと Map
  （corpuscheck と同一の正規化： core と full-norm の 2 ハッシュ）を
  比較。base／head の全組合せで出力は各 1 種類に収束し、
  base の単一出力と head の単一出力は完全一致。生成コードの
  SHA-256 は第86サイクルの記録値とも一致した。
- ワークスペーステスト 1,614 件全通過（第239ラウンドと同数；
  本変更は新規テストなし）。clippy（storm-lua-minify）エラー 0、
  fmt は変更行に指摘なし。

### 時間計測（開発ホスト4、MEASURE.lock、submit 1 回・全入力同梱）

208 ファイル（20 件ごと抽出の固定部分集合）の std-nomap コンパイルに
ついてファイル別経過時間の合算を base（`4152d1e`）／head 交互に 3 回：
base 中央値 13,941,784µs（rep: 13,849,744／14,068,999／13,941,784）、
head 中央値 13,988,961µs（rep: 13,878,288／14,019,309／13,988,961）。
中央値差は +0.34% で rep の範囲が重なり、**壁時計の改善は主張
しない**（第238・239ラウンドの前例どおり）。採用根拠は上記の
決定論的な SipHash 演算の消滅に置く。status は全 rep・両側で一致
（ok 206／err 2）。

## 課題 (2): evaluate 側の内訳計装（測定のみ、実装なし）

第83サイクルの内訳（prepare 27.2%／evaluate 72.8%）のうち、
evaluate の中身を `search.rs` の継ぎ目にステージ計装
（原子カウンタ＋経過ナノ秒の累積、bench 専用・未統合）で切り分けた。
std-nomap、全 corpus、プロセス user 169.35s。プローブの出力行は
corpuscheck の std-nomap 行と完全一致（計装の忠実性）。

全体（user 時間に対する割合）： parse 0.39s（0.2%）、
prepare 43.03s（25.4%）、evaluate 125.81s（74.3%）、
select 0.02s（無視可）。第83比で prepare の割合が下がったのは
第235〜239ラウンドの prepare 側削減の反映。

evaluate 内部（125.81s に対する割合）：

| 区分 | 時間 | 割合 | 備考 |
|---|---|---|---|
| finalize パス群（レイアウトごと） | 47.34s | 37.6% | 17,448 レイアウト × 約 2.71ms |
| phase パス群（ジョブごと） | 39.65s | 31.5% | 8,719 ジョブ × 約 4.55ms |
| レイアウト構築 | 23.26s | 18.5% | ジョブあたり約 2.68ms（clone＋別名解析等） |
| 変種選択（サイズ印刷含む） | 12.22s | 9.7% | サイズ印刷 17,724 回 × 約 0.69ms |
| print_final＋再パース＋仕上げ | 2.81s | 2.2% | レイアウトあたり約 0.16ms |
| その他（ジョブ設定・枝刈り等） | 約 0.5s | 0.4% | |

決定論カウント： 準備ジョブ 8,724 件（うち字句パス 5 件）、
評価ジョブ 8,719 件、レイアウト 17,448 件（ジョブあたり 2.00）、
候補 17,448 件（全レイアウトが候補を 1 件生成；std-nomap では
パース／意味の却下 0）、サイズ印刷 17,724 回
（draw 変種の追加分 276 回を含む）。

### 次レバー候補の順位（本計装に基づく記録のみ）

1. **finalize／phase のパス列（evaluate の 69.1%）**： 同じパス族が
   ジョブごとに 1 回（phase）とレイアウトごとに 1 回（finalize）
   走る。次の一手は両列へのパス別時間の計装で上位パスを特定する
   こと（第83 callgrind の inclusive 参考値： api_aliases の
   clone＋再解決 約 38%、temporary_globals 約 22%、ただし重い
   11 ファイルの標本）。
2. **レイアウト構築（18.5%）**： レイアウトごとの phase 木の clone と
   別名解析。生成側で捨てられる候補の構築回避（第186ラウンドの
   一般則）と共有化の余地を、clone ノード数の決定論カウントで
   先に測ること。
3. **変種選択のサイズ印刷（9.7%）**： 全変種を勝者判定のためだけに
   全文印刷している（17,724 回）。AST 上のサイズ計測
   （`measure_size` 系）で代替できれば、勝者以外の印刷を消せる。
   ただし変種ごとの pooling パスも同じ区分に含まれるため、
   着手前に印刷だけの割合を分ける追加計装が要る。
4. print_final＋再パース（2.2%）と select（無視可）には
   レバーは見当たらない。
