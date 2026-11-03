# コンパイル CPU: core_optimize のパス適用ごとの全木走査の1本化 — 検証記録

対象: `crates/storm-lua-minify/src/orchestration.rs`（`apply`）／
`crates/storm-lua-syntax/src/size.rs`（`SizeCtx`／新規
`measure_size_with_empty_if_check`）
担当: w7 第85サイクル（コンパイル CPU 軸、第83サイクルの申し送り候補 (3)）

## 実測で特定した無駄

`core_optimize` の各パス適用（`apply`）は、パス関数の実行後に

1. 空 if の不変条件検査のための `walk`（全木走査）
2. `measure_size`（全木走査）

の2本の全木走査を毎回走らせていた。一時計装（プローブ専用ブランチ、
統合せず）による内訳実測（std 構成・OBJ-1、全 corpus 4,155 ファイル、
開発ホスト1、compile 全体の user 126.49s）:

- `apply` 呼び出し 215,376 回。パス関数本体 25,449.7ms に対し、
  空 if 走査 783.8ms、measure_size 1,947.7ms（2走査で計 2,731.5ms
  ＝コンパイル全体の約 2.2%、apply 経路の約 9.6%）。
- ラウンド境界の measured_size は 333.9ms、signature の Printer は
  298.9ms で、パス適用ごとの走査とは別枠（本変更の対象外）。

## 監査（PassResult.saved 契約と measure_size の使われ方）

- `apply` における `measure_size` の結果 `after` の用途は、(i) 次の
  パスの `before`（raw_size 連鎖）、(ii) PassRecord の記録条件
  （`after != before` または details あり）と `saved` の算出
  （`result.saved ?? before - after`）のみ。ラウンドの打ち切り・
  ロールバックはラウンド末尾に別途測る `measured_size`（リネーム
  込み）が判定し、勝者選択（search.rs）は最終生成コードで比較する
  ため、パス単位のサイズは探索判断に入らない。
- `PassRecord.saved` はビルド公開 API に載る報告メタデータであり、
  探索・打ち切りの入力ではない。
- したがって、2走査を「同一の値を返す1走査」に融合しても、勝者・
  生成コード・PassRecord 列は不変である。空 if の存在検出は、
  サイズ計測が到達可能な全ての文を走査する（文は block 経由でのみ
  現れ、式内の関数本体も block として計測される）ため、SizeCtx の
  走査に同乗させて等価に検出できる。検出は `arms[0]` を引く前に
  行い、違反時は呼び出し側が従来と同一の文言で assert する。

## 修正

- `size.rs`: `SizeCtx` に `Cell<bool>` の `empty_if` フラグを追加。
  `stat_seg` の If ケースで arms が空ならフラグを立てて打ち切る。
  新規公開関数 `measure_size_with_empty_if_check` が
  `(サイズ, 空 if の有無)` を返す。他の計測関数の値は不変。
- `orchestration.rs`: `apply` の独立した検査 walk（`assert_no_empty_if`）
  を廃し、`measure_size_with_empty_if_check` の1走査に置換。assert の
  文言（`invalid empty if after {name}`）と panic による中断の意味は
  従来どおり。パス適用ごとの全木走査は 2 本 → 1 本になる。
- `pass.rs` の `run_pass` は現行では単体テストからのみ使われる
  TS 移植の参照実装であり、実運用経路ではないため変更していない。

以上により Tier INVARIANT（出力不変が構造で示せる変更）として扱う。

## 検証

- 単体テスト: `size` に2件追加（通常木で `measure_size` と一致し
  空 if なし／外科的に空化した if を panic せず検出）。ワーク
  スペーステスト 1,606 件全通過（基点 1,604＋新規 2）。
- corpuscheck 4 構成 × 4,155 ファイル（16,620 行）: base
  （`0a4f228`）／head ともにソート済み TSV の SHA-256 が
  `743431aa4bb3142c3661c78dfdbf63219052704c8d763fbd35cda85d73279be7`
  でバイト一致（FULL-VERIFY-TIP の記録ハッシュと同一＝累積でも不変）。
  fallback 0 件。
- fmt: 変更行に指摘なし（リポジトリ全体では既存の差分が残るが
  本変更行は対象外）。clippy: 本変更による新規診断なし（基点で
  残っていた `passes/single_member_cache.rs:1955` の `unwrap_used`
  は他担当面の既存債務で、第237ラウンドで解消済み）。
- 時間（開発ホスト4、MEASURE.lock、submit 1 回・全入力同梱）:
  corpus の 20 件ごと抽出 208 ファイルの obj1 コンパイル合計
  （ファイル別経過時間の合算、交互 3 回）: base 中央値
  13,988,059µs → head 中央値 13,959,041µs（−0.21%）。rep の範囲は
  両側で重なっており、この標本では測定分解能以下である。採用の
  根拠は壁時計ではなく、決定論的な走査の消滅（全 corpus のコンパ
  イル一式で `apply` 215,376 回分の全木走査が 1 本化されること、
  計装実測で空 if 走査 783.8ms 分）に置く。壁時計の改善は主張しない。

## 残候補（未着手、次サイクル以降）

- 候補 (4) `build_parent_index` の密 Vec 化（第83サイクル時点で
  残り 55,869 回分）。本サイクルでは監査が通ったため候補 (3) を
  実施し、(4) は次サイクル候補として残す。
