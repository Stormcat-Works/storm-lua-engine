# literal-call-folding の tiny tracked arena のパス内再利用による追跡コスト削減（2026-10-29、未公開）

compile の追跡コストのうち、literal-call-folding パスに残っていた
tiny tracked arena の呼出ごとの生成コストを除去した。

## 切り分け

第37サイクルまでの残差（21.3 回／件）の内訳を診断変種で切り分けた
（診断は全て一時変種で、測定後に元の実装へ復元した）。
診断変種は directpass（inline.lua、11 反復・中央値、folded=500）で測った。

- tiny arena 以降を丸ごと外すと追跡差は 630 回まで落ちる。すなわち
  tiny arena 群全体が 10,012 回で、tiny 経路外は 630 回（1.26 回／件）。
- relate のみ外すと 2,500 回減（5.0 回／件）、context のみ外すと
  1,000 回減（2.0 回／件）、転送 derive のみ外すと変化なし（共有
  スロット fast path のため 0 回）。
- relate＋context 両方外すと 3,500 回減で、単独値の和と一致する。
- tiny arena の生成＋tmp 由来付けのみ残すと追跡差は 2,630 回で、
  tiny 経路外 630 回との差 2,000 回（4.0 回／件）が arena 生成分。
  tmp derive 自体は共有スロット fast path で 0 回であり、残りは
  inherit_ast と初回 push のコストである。
- origin slots はページ式（64 件／ページ）で、空の arena への初回
  push はページ用 Vec・ページボックス・ページバッファの 3 確保を
  払う。すなわち呼出ごとの新規 arena 生成が約 4 回の追跡差の主因で、
  これが本サイクルの実装対象である。

## 採用した変更

1 パス内で 1 個の tiny tracked arena を使い回し、呼出ごとに
reset_for_reuse で値・スロットをゼロへ切り詰める形に変更した。

- `crates/storm-lua-syntax/src/origin_slots.rs`：clear_keep_page を
  追加（全スロットを破棄しつつ初回ページのバッファだけを保持する。
  内容はゼロへの切り詰めと同一で、保持されるのは容量だけ）。
  push はページが存在しない場合にのみ新規ページを追加する形に
  変更した（従来の長さ条件では、ページを保持したゼロ長テーブルへの
  次 push が不要な第2ページを確保してから初回ページへ書くため。
  従来到達可能な状態では新旧条件は一致する）。
- `crates/storm-lua-syntax/src/node_arena.rs`：reset_for_reuse を
  追加（値の clear とスロットの clear_keep_page。源スナップショットと
  arena レベルの disposition テーブルには触れない）。
- `crates/storm-lua-minify/src/passes/inline_functions.rs`：
  fold_literal_function_calls が tiny arena を 1 個だけ生成し、
  rewrite_literal_calls 経由で try_install_scratch_folded_call へ
  渡す。インストール節の開始時に reset_for_reuse を呼ぶ。

等価性の根拠：リセットは前呼出の全ノード・全由来を破棄する。
各呼出は空の arena から tmp・被演算子・ルートを push し直すため、
ノード ID の割り振りも含めて新規生成時と同一の arena を観測する。
変わるのはバッファの容量だけで、値・スロット内容・源スナップショット・
disposition テーブルは同一である。tiny fold はリセット後の小さな
arena に対して従来通り走るため、fold の resolve・解析・複製の
対象も従来と同一である。回帰テスト
reused_tiny_arena_folds_consecutive_literal_calls を追加し、
1 パス内の 2 呼出がともに折り畳まれ、生成コード・unknown 0・
本体由来の保持を検証する。

測定（開発ホスト1、directpass 11 反復・中央値、inline.lua、folded=500）：
無追跡 181,676 回から 180,179 回へ、追跡 192,318 回から 188,825 回へで、
追跡差は 10,642 回から 8,646 回へ（約 -18.8%、21.3 回／件から
17.3 回／件）。無追跡側の減少は arena 生成自体の絶対コスト削減で、
追跡差の減少 1,996 回と一致する。saved=3001・コード長 4443・
ノード数は変更前後で同一。時間改善は主張しない（決定的計数のみで判定）。

検証区分は Tier FULL。実コーパス全件比較（4,155 ファイル×4 条件、
開発ホスト1 のローカルコーパス、実行前に SHA-256 manifest 照合
4,155/4,155 OK、manifest ハッシュ
abfec87ab0c29dc088d4e93f25cf563849b062d3eb15eeaa5d54f8d08b8341c3）で、
base（分岐元 d98a0ea）と head の比較結果は下記のとおり。
base と head の TSV はバイト同一（SHA-256 5210f6e1982483923446d683915522db9da582086e6d44d1eff6524ae71ecc84、16,620 行ずつ）。全条件で identical=4,155、hard・core・expl・missing すべて 0 件。成功 4,127／失敗 28 は両側同一。検証バイナリの SHA-256 は base ab4138d7…・head 0d9df5a9…で相違を確認。
