# コンパイル CPU: 親インデックス構築の共有化 — 検証記録

対象: `crates/storm-lua-minify/src/passes/inline_functions.rs`
（`function_infos` / `declaration_execution_guaranteed` / `build_parent_index`）
担当: w7 第83サイクル（新軸: std 構成の exhaustive 探索のコンパイル CPU）

## 実測で特定した支配項

std 構成（OBJ-1 相当、target なし、corpus 4,155 ファイル、開発ホスト1
同一機・同一バイナリの内訳計測）:

- prepare 38.05s（27.2%）/ evaluate 101.89s（72.8%）/ select は無視可。
  parse は全体の 0.2% 未満。prepare の内部では `core_optimize` が 92.4%
  （全体の 23.9%）。構造バリアントの dedup（全 Ast 等価比較）は 0.06% で
  無視可（COW アリーナの共有で既に安い）。
- 重いファイル 11 件の callgrind プロファイルで、`function_infos` の
  候補フィルタ `declaration_execution_guaranteed` が候補ごとに
  `build_parent_index`（全 AST 走査＋HashMap 2 個の構築）を呼ぶ連鎖が
  命令数の約 21.6% を占めた。
- 決定論カウント（std-nomap 全 corpus）で裏付け: `function_infos`
  呼び出し 56,420 回・検査候補 238,034 件に対し、親インデックス構築は
  235,010 回・走査ノード総数 619,215,269。候補 1 件ごとに木全体の
  インデックスを作り直していた。

## 修正

親インデックスは `(ast, root)` の純関数で、`retain` のチェック中に対象の
木は変化しない。`function_infos` が `Option<ParentIndex>` を保持し、
書き込み数ゲートを通過した最初の候補の時点で遅延構築して全候補で
共有するようにした。ゲートで落ちる候補しかない呼び出しでは従来どおり
構築しない。判定内容・走査順序・結果は変わらないため Tier INVARIANT。

## 検証

- 決定論カウント（変更後、同条件）: 構築 235,010 回 → 55,869 回
  （−76.2%）、走査ノード 619,215,269 → 63,063,782（−89.8%）。
  `function_infos` 呼び出し 56,420 回・検査候補 238,034 件は不変。
- std-nomap の split 計測（base `aa18171` vs head、全 4,155 ファイル）:
  ジョブ数・勝者（構造/レイアウト）・エラーは完全一致（差分は時間列のみ）。
- Tier FULL 相当の corpuscheck（4 構成 × 4,155 ファイル、16,620 行）:
  base vs head で TSV バイト一致（SHA-256
  `743431aa4bb3142c3661c78dfdbf63219052704c8d763fbd35cda85d73279be7`）。
  このハッシュは FULL-VERIFY-TIP `295e52c` の第234ラウンド記録と
  同一であり、累積でも出力不変。
- テスト: storm-lua-minify 1,017 件、ワークスペース全体 1,602 件、
  いずれも全通過。
- fmt / clippy: 本変更に新規診断なし。clippy の既存エラー
  （`passes/single_member_cache.rs:1955` の `unwrap_used`、workspace の
  deny 設定違反）は base `aa18171` でも同一に存在する他担当面の債務で、
  本サイクルでは触れず記録のみとする。

## 時間計測（開発ホスト4、MEASURE.lock、submit 1 回）

corpus 全リストの 20 件ごとに抽出した 208 ファイルの固定部分集合で
`c83probe split`（std-nomap）を base / head 交互に 3 回ずつ実行。
user 時間の中央値: base 15.66s → head 14.45s（−7.7%）。全 rep の範囲は
base 15.60–15.78s / head 14.38–14.57s で重ならない。
参考（開発ホスト1、同条件の単独実行）: 全量 split の user 時間
139.88s → 133.17s（−4.8%）。内訳では prepare が 38.05s → 32.27s
（−15.2%）と大きく、evaluate は 101.89s → 100.90s。

## 残候補（未着手、次サイクル以降）

- `api_aliases::clone_rewriting`（重いファイルで inclusive 約 38%）:
  レイアウト候補ごとに clone＋resolve＋書換えを最大 3 系統（通常 /
  barrier 緩和 / pure-call carrier）構築し、dedup は構築後に行われる。
  生成側で系統の適用可否を先に数える余地がある（第186ラウンドの一般則）。
- `temporary_globals` の整数キー HashMap 群（SipHash の `hash_one`
  が全体の self 約 11%）: DSATUR の安定ソート tie-break が反復順序に
  依存しうるため、ハッシャ差し替えは出力順序の観測可能性を潰してから。
- `core_optimize` のパス適用ごとに行う `measure_size` と空 if 走査
  （1 ラウンド最大 36 適用 × 2 回の全木走査）: `PassResult.saved` の
  契約監査が前提。
