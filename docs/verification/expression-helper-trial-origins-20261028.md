# expression-helper-inlining の試行由来なし化による追跡コスト削減（2026-10-28）

## 焦点
compile の追跡コストのうち、expression-helper-inlining パス
（`inline_expression_functions`）が、出力に反映されない試行のたびに
支払っていた由来確保コスト。w1 の第30サイクルで、
開発ホスト1 上で作業した。分岐元は統合先端 `75364db`
（第109ラウンド）。

## 棚卸し
新先端での再棚卸し（合成入力、trackcost single 5 反復・
directpass 11 反復の中央値、開発ホスト1）では、dense-normal は
第29サイクル終了時と同一だった。他の合成入力へ広げた結果、
inline.lua で expression-helper-inlining の単独オン差が 12,275 回、
common.lua で redundant-parentheses-elimination が 1,038 回など
（発火 0・差残り型）を確認し、規模の大きい前者を実装対象とした。
literal-call-folding・available-expression-reuse・
single-use-local-sinking・temporary-global-packing は発火系のため
本サイクルでは取らない。

## 切り分け（expression-helper-inlining、inline.lua）
- directpass 11 反復・中央値で同パス単独は無追跡 24,144 回→
  追跡 36,419 回（差 12,275 回）で単独オン値と一致した。
- V1（rewrite 全体無効化）で差は 2 回まで落ち、コストはほぼ全額
  `rewrite_expression_helpers` 内であることを確定した。
- V2（試行クローン手前で復帰）でも差は 2 回のままで、歩行・
  ゲート・resolve・measure は無寄与となり、ほぼ全額が試行クローン
  （`clone_subtree_with_replacements`）側であることを確定した。
- V3（引数コピー側 finish_rewrite 無効化）で差は 275 回まで落ち、
  主因は引数部分木コピー1ノードあたりの finish_rewrite
  （"function-inline-argument-copy"）であることを確定した。
  計装では試行あたり body 13 ノード・引数 4 ノード・
  500 試行／反復を確認した。
- `derive_from_clone` の slow path 通過（shared_sources=false）も
  確認したが、本体複製側は V8（本体 derive 無効化）で差が変わらず
  ほぼ無寄与で、引数側だけが高いという内訳の非対称の機構までは
  特定できていない（後続の調査課題として残す）。
- 診断パッチはすべて退避からの復元で除去し、git の破壊的操作は
  使っていない。

## 採用した変更（ローカルコミットのみ。push・PR なし）
- `crates/storm-lua-minify/src/passes/inline_functions.rs` のみ
  （＋回帰テスト 1 件）。`inline_expression_functions` の試行を
  由来なしクローン（`clone_without_origins`）上で実行し、サイズ測定
  ＋bindings 検査で採用が確定した場合にだけ tracked なラウンド源で
  同一 rewrite をリプレイしてコミットする。無追跡側は試行をそのまま
  コミットする（従来の plain 経路と同一で二重実行なし）。
- 等価性の根拠：rewrite の判断材料は値・resolve・effects・サイズ
  のみで由来に依存せず、リプレイは従来の tracked 経路と同一の源・
  同一の引数のため、コミットされる AST・由来は従来と同一。
  試行・リプレイの共通ブロックはクロージャ化した（ヘルパー関数版は
  clippy の引数超過警告が出たため）。
- syntax 側の共有ヘルパー案は実測で効果なしのため不採用・除去し、
  repo 差分は minify 1 ファイルのみである。
- 回帰テスト `tracked_trial_replay_matches_untracked_output` を追加
  （追跡付きと無追跡のインライン結果が同一で、"2+1" へのインライン
  ＋宣言除去を確認）。

## 測定（変更後、開発ホスト1、中央値。時間差は主張しない）
- directpass expression-helper-inlining（inline.lua、11 反復）：
  無追跡 24,143 回・追跡 24,144 回で追跡差 12,275 回→1 回。
  saved=0・コード長 7444・ノード数 4022 は同一。
- trackcost single（inline.lua、5 反復）：同パス単独オン差
  12,275 回→1 回。全コンパイル default 差 16,500 回→8,189 回
  （単独オン差との加算性は主張しない）。
- 成功入力（success.lua、directpass 11 反復）：追跡・無追跡とも
  code_len 43・saved=35 で同一出力（"2+1" へのインライン＋宣言除去を
  確認）。成功時の追跡差はリプレイ 1 回分だけ残る設計どおり。
- onlycode：単独オン出力は追跡あり・なしとも all-off とバイト同一
  （7444 バイト）。

## 検証
- 対象 3 クレート（syntax 124・minify 646＝新規 1 件含む・
  build 126）全成功、ワークスペース全件 50 スイート・1,150 件
  全成功・失敗 0、xtask check・fmt・clippy（対象 3 クレート、
  警告 0）通過。--all-targets の clippy 警告 6 件は search.rs
  テスト由来の既存警告（本変更のファイル外）。
- 実コーパス全件比較（Tier FULL、開発ホスト1 のローカルコーパス
  コーパスのローカルミラー、実行前に SHA-256 manifest 照合
  4,155/4,155 OK、manifest ハッシュ
  `abfec87ab0c29dc088d4e93f25cf563849b062d3eb15eeaa5d54f8d08b8341c3`）：
  base は分岐元 `75364db`、head は本変更の作業ツリー。
  検証バイナリの SHA-256 は base `d7e792a2…`・head `ad6baac3…` で
  相違を確認。4,155 ファイル×4条件、欠落 0。両 TSV はバイト同一
  （SHA-256 `3b78e3d76febf8762c95edd11c4913f09c24ac618f3c76f0eba10d34ede40ed6`）。
  全条件 identical=4,155、hard・core・expl・missing すべて 0。
  成功 4,127／失敗 28 は両側同一。
