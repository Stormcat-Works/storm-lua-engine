# literal-call-folding の tiny fold 間での FoldSharedFacts 共有による追跡コスト削減（2026-10-29、未公開）

compile の追跡コストのうち、literal-call-folding パスに残っていた
tiny fold ごとの FoldSharedFacts 構築コストを除去した。同パスは
ユーザ定義関数のリテラル呼出ごとに tiny tracked arena でルート fold を
再現するが、そのたびに `fold_expressions` が新しい FoldSharedFacts
（aggressive フラグ文字列・許容誤差ビット文字列の書式化と、リテラル
事実・サイズ事実の初回書式化キャッシュ）を構築していた。1 パス内の
全 tiny fold は同一の aggressive フラグとデフォルト許容誤差で走るため、
1 個の FoldSharedFacts をパス全体で共有する形に変更した。

等価性の根拠：共有される事実文字列は (aggressive, 許容誤差, リテラル値,
式サイズ) の純粋関数であり、共有しても記録される文字列内容は同一で、
変わるのは確保回数だけである。`fold_expressions_with_options` の既存の
呼出側は従来どおり呼出ごとに新しい共有テーブルを作るため挙動は不変。
回帰テストは、別 fold で事前に埋めた共有テーブルでの fold と新規
テーブルでの fold とで、生成コードと GeneratedOrigins が完全一致する
ことを検証する（`shared_facts_reuse_matches_fresh_fold`）。

測定（開発ホスト1、directpass 11 反復・中央値、inline.lua、folded=500）：
無追跡 184,171 回→181,676 回、追跡 204,801 回→196,318 回で、
追跡差は 20,630 回→14,642 回（約 −29.0%、41.3 回／件→29.3 回／件）。
saved=3001・コード長 4443・ノード数は変更前後で同一。
時間改善は主張しない（決定的計数のみで判定）。

検証区分は Tier FULL。実コーパス全件比較（4,155 ファイル×4 条件、
開発ホスト1 のローカルコーパス、実行前に SHA-256 manifest 照合
4,155/4,155 OK、manifest ハッシュ
`abfec87ab0c29dc088d4e93f25cf563849b062d3eb15eeaa5d54f8d08b8341c3`）で、
base（分岐元 f89de98）と head の TSV はバイト同一（SHA-256
`5210f6e1982483923446d683915522db9da582086e6d44d1eff6524ae71ecc84`、
16,620 行ずつ）。全条件で identical=4,155、hard・core・expl・missing
すべて 0 件。成功 4,127／失敗 28 は両側同一。
