# signed-expression-factoring の試行由来なし化による追跡コスト削減（2026-10-28）

## 焦点
compile の追跡コストのうち、signed-expression-factoring パス
（`signed-expression-factoring`）が、出力に反映されない試行のたびに
支払っていた由来確保コスト。w1 の第28サイクルで、開発ホスト1 上で
作業した。分岐元は統合先端 `7717967`（第105ラウンド）。

## 棚卸し・ベースライン
第27サイクルで切り分け済みの候補。common.lua での trackcost single
（5 反復・中央値、開発ホスト1）では、同パスの単独オン差が
12,685 回で、単独オン出力は all-off とバイト同一（追跡あり・なしとも
onlycode で確認）だった。試行 9 件はいずれも敗北する。

## 切り分け（common.lua）
- 試行の由来なし化で単独オン差は 12,685 回→1,413 回となった。
- 残差の内訳を一時変種で切り分けた（すべて cp 復元で除去し、
  git の破壊的操作は使っていない）：
  - 再利用キャリアのスキャン（`count_scratch` 系）の寄与は 0 回
    （スキャンを空転させても 1,413 回で不変）。
  - 残りの約 1,412 回は、グループ発見（inspect）段階で
    `signed_form` が正規化 Bin を scratch へ push する際の
    `derive_from` によるもので、ラウンドあたり一度きりのコストである。
  - inspect 用 scratch を由来なし化する変種では差は 1 回まで落ちる
    ことを確認したが、採用しない。`Group.expression` は inspect の
    scratch に push された正規化 Bin のノード ID であり、試行は
    その scratch をクローンすることで同 ID の正規化ノードを
    共有している。inspect scratch を分離すると、勝者リプレイ側で
    正規化ノードの由来が失われ、勝者の定義値ノードの由来が変わる
    ためである。残差は候補あたりの無駄ではなく、勝者に必要な
    一度きりの発見コストとして残す。

## 採用した変更（ローカルコミットのみ。push・PR なし）
- `crates/storm-lua-minify/src/passes/signed_factoring.rs` のみ
  （＋回帰テスト 1 件）。候補適用処理を `apply_signed_candidate`
  へ抽出し、試行は `clone_without_origins` による由来なしクローンで
  構築してサイズ測定だけを行う。由来なしアリーナでは `derive_from`・
  `relate_within`・`rewrite` の記録・`mark_synthetic` がすべて no-op
  になるため、敗北する試行は追跡コストを払わない。
  サイズ測定（`measure_renamed_size`）は resolve と計測のみで
  由来に依存しないため、勝者選択は変わらない。
- 勝者が決まった場合は、同一の rewrite コードを tracked クローンで
  リプレイして適用する。由来なし試行をそのままコミットすることは
  しない（`clone_without_origins` の文書にも、由来が必要な場合の
  コミット禁止が明記されている）。勝者不在の場合は適用なし。
- 回帰テスト `wins_one_group_and_discards_losing_trials` を追加
  （勝者グループの因数分解と敗北グループの不変を同一ブロックで検証。
  期待出力は変更前コードで取得し、新旧で同一出力を確認）。

## 測定（合成入力 common.lua、開発ホスト1、中央値。時間差は主張しない）
- trackcost single（5 反復）：同パスの単独オン差 12,685 回→1,413 回
  （約 −89%）、バイト差 2,211,312 B→245,872 B。無追跡側の確保回数・
  コード長は変更前後で同一。時間測定（.us）は行わない
  （時間改善を主張しないため）。
- onlycode による単独オン出力のバイト同一を追跡あり・なしとも確認
  （all-off 出力と同一。変更後も維持）。

## 検証
- 作業側（開発ホスト1、Rust 1.97.1。sccache は起動時 probe が
  タイムアウトで応答しなかったため、全ビルドを
  `RUSTC_WRAPPER=env` で回避。sccache デーモン自体には触れていない）：
  対象 3 クレート（syntax 123・minify 644＝新規 1 件含む・build 126）
  全成功、ワークスペース全件テストも失敗 0。
  `cargo xtask check`・`cargo fmt --check`・clippy
  （対象 3 クレート lib、警告 0）通過。
- 実コーパス全件比較（Tier FULL、開発ホスト1 のローカルコーパス
  コーパスのローカルミラー、実行前に SHA-256 manifest 照合 4,155/4,155 OK、
  manifest ハッシュ
  `abfec87ab0c29dc088d4e93f25cf563849b062d3eb15eeaa5d54f8d08b8341c3`）：
  base は FULL-VERIFY-TIP `7717967`（分岐元と同一）、head は本変更を
  含む作業ツリー。両 TSV はバイト同一（SHA-256
  `f45aced283ffb974d0cef713f3fc43e84310dfa228dfad7aeca69595b66fb4ea`、
  16,620 行）。全 4 条件で identical=4,155、hard・map-core・
  expl-only・missing すべて 0、成否ペア ok/ok 4,127・error/error 28。
  検証バイナリの SHA-256 相違も確認
  （base `c99e456a…`、head `288ac559…`）。
  すなわち本変更はコーパス範囲の出力に一切の変化を与えていない。
- 検証区分は Tier FULL のため、FULL-VERIFY-TIP はマージ後の新先端へ
  更新する。

## 次サイクルの焦点（第1候補）
- draw.lua の ordered-screen-loop-synthesis の追跡差
  （単独オン差 145,736 回）。ただし同パスは w7 の候補探索軸と接する
  ため、着手前に CLAIMS.md で w7 との境界を確認すること。
- 第2候補：immutable.lua の immutable-carrier-synthesis の単独オン差
  （約 307 万回。ただし単独オン出力は変化するため発火系であり、
  試行の無駄とは別種の切り分けが必要）。
- 第3候補：signed 残差のうち inspect 正規化由来の扱い
  （勝者定義の由来を保ったままの削減は要設計。本サイクルでは見送り）。
