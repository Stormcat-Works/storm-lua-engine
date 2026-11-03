# コンパイル CPU: single-use-forwarding の全チャンク読み数え一括化（2026-11-01、w7 第88サイクル）

## 変更

- 実装: `ccb4d68`（`crates/storm-lua-minify/src/passes/single_use_forwarding.rs` のみ、+78/−17、うち単体テスト +31 行程度を含む）
- 内容: `collect_plans` が候補ごとに呼んでいた `all_read_count`（対象 binding の読みを数えるために木全体を走査、B 本の候補で B 回の全走査）を、全 binding の読み数を 1 回の走査で数える `all_read_counts`（`HashMap<BindingId, usize>`）へ置換し、`forward_single_use_globals` の走査エポック（resolve＋EffectAnalyzer 構築から候補確定まで）内で遅延構築して共有する。キャッシュは `apply_plans` が木を変更する前に破棄される。照会側は `map.get(&bid).copied().unwrap_or(0)` で、従来の戻り値と同一の数を返す。

## レバー選定の根拠（第88サイクル計装）

std-nomap 全 corpus のパス別計装（プローブ行は corpuscheck の std-nomap 行とソート後にバイト一致を確認）で、phase パス群 46.13s の最大は single-use-forwarding 10.57s（22.9%）、finalize パス群 70.00s の上位は split-sign 7.33s／conditionals 6.32s／final-stores 6.18s。single-use-forwarding の内部では `collect_plans` が 6.26〜7.02s を占め、その内訳は `all_read_count` 88,477 回・2.06s（1 回あたり全木走査）、`scoped_read_count` 5,034,090 回・1.88s、`statement_contains_write` 539,664 回・1.06s、`effects_for_statement` 6,212,402 回・1.41s であった。`all_read_count` の唯一の呼出箇所は root を起点とする照会であり、同一エポック内では AST と resolution が不変のため、結果は binding ごとに定数である——これが一括化の根拠。

他の上位パスは採用しなかった: split-sign は 3 段の clone＋resolve＋解析が設計上のコストで機械的な共有化が効かない。conditionals の文ごと refresh（clone＋resolve＋解析、3.54s）は候補判定が解析の鮮度に依存するため畳めない。数値／文字列プールの width 試行は勝者選択そのもので、出力に触れる。final-stores は 95% が無変更だが、到達可能な単独の最終ストア除去（第169ラウンドの事例）があるため安易なゲートは意味を変える。

## 出力不変性（Tier INVARIANT）

- 走査の訪問ノード集合と読み述語（`Name` ノードで、書き込みでなく、binding へ解決されるもの）は旧 `all_read_count` と文字どおり同一であり、数える対象が 1 binding から全 binding へ広がっただけである。各 binding の件数は旧関数の戻り値と厳密に一致する。
- エポック内では木も resolution も変更されない（変更は `apply_plans` のみで、その時点でキャッシュは破棄済み）。したがって全照会が旧実装と同一の数を返し、計画・適用・出力は不変。
- 固定化: 旧逐 binding 走査をテスト内参照として残し、全 binding について map の値と一致することを assert する単体テスト `all_read_counts_match_per_binding_counts` を追加（関数境界を越える読みとシャドーされた同名ローカルを含むソースで検証）。既存の analysis_reuse テスト（最適化経路と参照実装の比較）も通過。

## 決定論カウント（計装プロトタイプ、std-nomap 全 corpus）

- 全木走査の回数: 88,477 回 → 19,925 回（−77.5%）。照会回数 88,477 は不変で、残りは map 参照で応答。
- `collect_plans` 時間（同一計装）: 7.02s → 5.18s。パス全体: 10.78s → 9.45s。

## 検証

- ワークスペーステスト: 1,618 件全通過（従来 1,614＋新規テスト等）。
- clippy: base／head で警告集合が同一（minify 5 件＋vm 1 件、いずれも既存で本変更ファイルには無し）。対象ファイル単体の `rustfmt --check` 無指摘。
- Tier 分類は INVARIANT だが、合格 gate として Tier FULL 相当の base／head 全 corpus 比較を実施: base＝`1f29208`、head＝本変更。16,620 行（4,155 ファイル×4 条件）が両側とも SHA-256 `743431aa…` でバイト一致、compare-corpus は全条件 identical=4,155、expl-only／map-core／hard／missing すべて 0、fallback 0。status は ok 4,128／error 27 が両側一致。
- .us2 測定（submit 1 回・全入力同梱、208 ファイル std-nomap、交互 3 rep、per-rep 合計 µs）: base 中央値 13,809,673（13,809,673／13,838,627／13,794,269）→ head 中央値 13,702,744（13,665,152／13,810,118／13,702,744）で −0.77%。3 rep 全てで head が base を下回ったが rep 範囲は重なるため、壁時計の主張はこの範囲に留める。採用の主根拠は決定論的な全木走査の消滅である（第238・第239ラウンドの前例どおり）。status は全 rep・両側一致（ok 206／err 2）。

## FULL-VERIFY-TIP について

本ラウンドは Tier INVARIANT のため、前例（第238・第239ラウンド）どおり FULL-VERIFY-TIP は前進させない。ただし上記のとおり FULL 比較自体は実施しバイト一致を確認済みである。なお申し送り: 第241ラウンド（Tier FULL、バイト一致）統合時点で marker は `1f29208` へ前進しているはずだが、記録ファイルは `9e6549b` のままである。本サイクルの base 実行でも `1f29208` の corpus ハッシュ `743431aa…` を独立に再現済みであり、marker の訂正は親側の台帳事項とする。
