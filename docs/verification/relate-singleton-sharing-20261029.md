# relate の単一 span 共有化による追跡コスト削減（2026-10-29）

## 焦点
compile 追跡コストのうち、immutable-carrier-synthesis パスの
残差 5,793 回（immutable.lua、directpass 11 反復・中央値）の
うち relate 由来の再確認。追跡専用・出力不変の変更に限る。

## 切り分け（分岐元 8f9390478、開発ホスト1）
一時診断変種（すべて cp 復元で除去）による内訳：
- relate_from_role 除去で差 5,793→2,849 回（relate 分 2,944 回）
- relate の span／relation 追加を除去すると差は relate 除去時と
  完全同一（2,849 回）。すなわち relate の理由付与・capture・
  import・スロット確保は追跡差 0 回で、残量は追加側だけである。
- 追加側のうち relations 追加のみ除去で差 4,313 回
  （relations 分 1,480 回）、related 追加のみ除去で差 4,329 回
  （related 分 1,464 回）。合計 2,944 回で閉じる。
- 診断印字による relate 呼出の実態：リプレイで 740 回、
  すべて contributor は primary 1 span のみ・related 空・
  relations 空、対象も related 空・relations 空であった。
  すなわち 1 呼出あたり related 追加 2 回・relations 追加 2 回
  （Arc 箱 1 回＋バッファ 1 回ずつ）で、合計約 4 回／呼出である。

## 採用した変更
`crates/storm-lua-syntax/src/node_arena.rs` のみ（＋回帰テスト 1 件）：
- `relate_snapshot` に高速路を追加した。contributor が
  primary 1 span のみで related・relations ともに空、かつ
  対象の related・relations ともに空の場合、従来の追加路が
  記録する内容は related 1 件（対象がその span 自身の場合は
  空のまま）と typed relation 1 件にちょうど一致するため、
  その単一ベクトルをインターン表（上限 4,096 件、既存の
  shared_synthetic_origin と同じ方式。上限超えは従来の fresh
  ベクトルへフォールバック）から共有して代入し、追加路を
  迂回する。記録されるベクトル内容は同一で、後続の変更は
  すべて Arc::make_mut 経由のため共有者同士の編集が漏れない。
- 回帰テスト
  `relate_singleton_fast_path_shares_vectors_and_matches_push_path`
  を追加した。2 対象が同一 contributor へ relate した結果の
  内容一致とベクトルのポインタ共有、自己 relate での related
  空・relation 記録を確認する。

## 測定（開発ホスト1、directpass 11 反復・中央値）
- immutable.lua：無追跡 32,228,515 回で不変、追跡
  32,234,308→32,231,364 回、追跡差 5,793→2,849 回
  （-2,944 回、約 -50.8%）。saved=1491・コード長 37040・
  ノード数 10499→10539 は同一。relate 分は全額消えた
  （診断変種 A の差と完全一致）。
- immutable-small.lua：無追跡 970,523 回で不変、追跡
  971,209→970,811 回、追跡差 686→288 回（-398 回）。
  saved・コード長・ノード数は同一。
時間差は主張しない。

## 検証
- 対象 3 クレート（syntax 130＝新規 1 件含む・minify 759・
  build 127）全成功、ワークスペース全件 50 スイート成功、
  xtask check・fmt・clippy（syntax・minify lib、警告 0）通過。
- Tier FULL コーパス検証（開発ホスト1 ローカルコーパス、
  manifest 照合 4,155/4,155 OK、manifest ハッシュ abfec87a…、
  内部並列 -P 4）：base 分岐元 8f9390478 対 head。
  検証バイナリ SHA-256 は base 1d3ac9a3…・head 6d8f917e… で
  相違を確認。4,155 ファイル×4 条件、欠落 0。
  両 TSV はバイト同一（SHA-256 40d852a9…、16,620 行ずつ）。
  全条件 identical=4,155、hard・core・expl・missing すべて 0。
  成功 4,127／失敗 28 は両側同一。
