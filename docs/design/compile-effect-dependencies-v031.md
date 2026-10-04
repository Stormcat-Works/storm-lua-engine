# v0.3.1 解析依存の再利用とマップ費用の継続削減

2026-10-05。[全経路の目標](compile-all-scenes-v031.md)を維持する。Mapなし/ありの絶対時間を共に短縮し、同じ新実装のMap比1.2以下、可能なら1.1以下を狙う。分母を遅くする対応、出力・理由・精度の省略、探索打切りは行わない。

比較元は前の検証済み実装`0de60af`。以前の実装・検証を先に確定し、入力manifestと完成済みWASMを固定する。過去の効果を新しい改善へ再加算しない。

まずEffectAnalyzerの関数summary固定点計算を調べる。AST/alias表が不変で、読み取る関数summaryも変化していない場合、同じ本体を次のroundで再度走査する必要はない。依存は初回の効果解析が実際に読むbindingを保守的に使う。binding順、即時更新、20 round上限、初期summaryと同一キーならpure/totalを更新しない規則は維持する。公開Analyzerを作った後のqueryや設定変更へcacheを持ち越さない。

Map固有部分は、計測で残る費用を確認してから変更する。既に不採用にしたJSONブリッジや借用viewを根拠なく再導入しない。必要な検査や完全なJSON返却を計測範囲の外へ移さない。

独立した旧方式との比較、固定240設定・未達target、Worker新旧混成、シーン反復、通常/LifeBoat buildと実Luaコーパスを使う。小さい負の観測も除外せず、Native/WASM/SDK/ブラウザー/梱包の関連ゲートを通してからcommitする。作業ブランチは`feat/minify-performance-v0.3.1`。push・公開・main/release更新は行わない。
