# compile-cpu: one-use-function-fusion チェック前倒し 検証記録（直列ループ第300ラウンド）

## 変更の概要

`crates/storm-lua-minify/src/passes/inline_functions.rs` の
`inline_one_use_statement_and_tail_functions` において、試行の
clone＋alpha-clone の後に置かれていた source のみの純粋述語3件を、
clone 構築の前へ前倒しした（+40/−29）。

- no_site: `find_fusion_site_block`（source の Ast と Resolution のみで判定）
- tail_target: `tail_assign_target_safe`（source／res／site／info／enclosing のみ）
- goto_scope: `fusion_site_jumps_into_new_locals`（source／enclosing／site のみ）
  ＋前提 `splice_declares_locals`

勝者不変性の要点:

- 3述語の入力は全てラウンドの source 側構造であり、試行 arena（trial）の
  値を一切読まない。clone の前後どちらで評価しても結果は同一である。
- `splice_declares_locals` の source 側等価形: alpha_params の空否は
  `info.parameters`（source の Function ノードから記録され、alpha-clone は
  パラメータ数を変えない）の空否と一致し、alpha 本体の Local／Localfunc
  文の有無は alpha-clone が文のノード種別を変えないため、source 本体
  （body_statements）の同検査と一致する。
- serial の消費位置は現行どおり（`serial += 1` の直後にチェックを置き、
  clone はその後）。棄却された試行が消費していた通し番号は従来どおり
  消費されるため、後続の勝者が得る生成名（`__i{serial}_{bid}`）は従来と
  同一で、コミットされる出力はバイト単位で不変である。
- 前倒し後に残る試行依存の棄却は free_ref（splice 後の再解決）と
  size_gate のみで、これらは対象外とした。

## 背景

- w7 第109サイクルの fusion センサス（試行 5,361 件、採用率 68.7%）で、
  棄却の 91.0% が no_site であり、前倒し可能な3帰結の試行窓 detach 合計
  5,792,216 elems が回収見込みとして特定された。
- w7 第110サイクルで試作し、事前登録閾値 (i) FULL バイト同一、
  (ii) 試行窓 detach 削減 ≥4.5M（実測 5,792,216、全て 0 に消滅）、
  (iii) lib テスト通過を満たした。(iv) .us2 壁時計は +0.12%（ノイズ内、
  rep 別に符号が割れる）で字義どおりの推薦条件を満たさず推薦保留と
  なったが、2026-11-02 にユーザーが採用を決定したため、直列ループで
  本実装した。試作差分（`proto-fusion-hoist.diff`）は blob
  `611111a..5efd74b` として現行先端へ無変更で適用できた。

## 検証（Tier FULL、累積 base `c65a629`）

- corpus: StormMin 実世界コーパス 4,155 ファイル（マニフェスト照合
  4,155/4,155 OK）×4 構成＝16,620 行。corpuscheck は bench-harness の
  同一ソースから base（`c65a629`）／head（本実装）両側をビルドした
  （エンジンへの path 依存先のみが異なる）。バイナリ SHA-256:
  base `e04fc8ccb7a9b29e685fb9a2ee84f2785ad35f1eea0c9e337aeb82cb4c7242d0`、
  head `07bae1a86eec58efa40e5d5915ca618bd1eeac2198303de570abad82bde57cc6`。
  正本 SHA `3c07a763…` とは一致しない（第73サイクルと同事情: .ud の
  bench corpuscheck は --batch 非対応の単独ファイルモード版であり、
  同一性主張は同一 bench ソースからの両側ビルド＋SHA 記録で担保する）。
- ドライバ事故の記録: 当初 `run-corpus-batch.py` を使ったが、上記の
  とおり corpuscheck が `--batch` 非対応のため引数を入力パスと誤認し、
  偽の行4行（id `--batch`）を生成、全 4,155 ファイルが単独ファイル
  モードへフォールバックした。偽行を除いた salvage TSV は、レガシー
  ドライバ（`run-corpus.sh`、ファイル毎プロセス・12 並列）で再実行
  した base TSV とバイト一致したため、最終結果は両側ともレガシー
  実行の TSV で確定させた。
- 結果: 整列 TSV の SHA-256 が両側とも
  `42ce28cacbfec693acb1c1dc3b338251ebe46e5a1f09dab284c4bd393e9b4e28`
  で一致（16,620 行バイト同一）。`compare-corpus.py`: 全構成
  identical=4,155、hard=0、map-core=0、expl-only=0、missing=0。
  status 不変（構成ごと ok 4,126／error 29）。
- detach について: 本実装単独のセンサス計装（FUSION_STATS 試行窓）は
  試作専用のため再測していない。試作時（tiny-literal 本実装との併用）
  の実測では総 detach 201,781,380 → 195,989,164 elems
  （−5,792,216）であった。採用根拠は FULL バイト同一と、試作で実証
  された決定的 detach の消滅である。
- テスト: ワークスペース全体 50 スイート・1,775 passed / 0 failed
  （exit 0）。うち storm-lua-minify lib は 1,148 件全合格。
- fmt: 変更ファイル無指摘（`cargo fmt --all -- --check` の差分は
  従来から存在する他7ファイル10箇所のみで、本変更で増減なし）。
- clippy（storm-lua-minify lib）: 警告集合は base と同一（8件）。
  inline_functions.rs への指摘は 2842／2980／3062 行の既存3件のみで、
  変更域（3412 行以降）への新規指摘はない。

## 統合

- 実装 `7cdf7a5`＋本記録を統合先端へ fast-forward し、第300ラウンド
  として確定。FULL-VERIFY-TIP を `7cdf7a5` へ前進させた。
