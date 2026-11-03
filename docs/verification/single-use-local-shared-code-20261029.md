# single-use-local-sinking の理由コード共有化による追跡コスト削減（2026-10-29）

## 変更

single-use-local-sinking パス（1 回だけ読まれるローカルへの定義式を
使用箇所へ置換するパス）で、パス全体で 1 個ずつ作った共有コード文字列を
由来記録に使い回すようにした。記録される文字列内容は変わらないため、
生成コード・由来・Map は同一で、変わるのは確保回数だけである。

- `crates/storm-lua-minify/src/passes/locals.rs`：`inline_single_use_locals_with_options`
  で `single-use-local-substitution` と `single-use-local-read` の共有
  `Arc<str>` を 1 個ずつ作り、`expand_tree`／`clone_expanded` へ渡す。
  `expand_tree` の文書き換えを `rewrite` から `rewrite_with_code` へ、
  2 箇所の `relate_from_role` を新設の `relate_from_role_code` へ変更した。
  `expand_tree` には `#[allow(clippy::too_many_arguments)]` を付けた
 （`clone_expanded` と同じ扱い）。
- `crates/storm-lua-syntax/src/node_arena.rs`：`relate_from_role_code` を新設した。
  既存の `relate_within_role_code` のアリーナ間版で、同じスナップショット・
  同じ role・同じコード文字列を `relate_snapshot` へ渡すだけである。
- 回帰テスト 1 件：`relate_from_role_code_matches_relate_from_role`
 （新旧両経路の由来が一致し、related が空でないことを検証）。

## 等価性の根拠

コード文字列は文字列内容で比較・格納されるため、共有 `Arc<str>` を渡しても
記録される由来・理由は新旧で同一である。これは root-local-globalization の
理由共有化（第9サイクル相当）や scope-renaming の変換コード共有化
（第4サイクル相当）と同じ設計で、`rewrite_with_code` 自体は第9サイクルで
導入済みの API である。`relate_from_role_code` は `relate_snapshot` への
引数をコード共有に変えるだけで、スナップショットの内容・role・
関連スパンの扱いは変わらない。Tier FULL コーパス比較（下記）でも
全条件で差分 0 を確認した。

## 切り分け（第55サイクル、locals-single.lua、directpass 11 反復・中央値）

- ベースライン：single-use-local-sinking の追跡差は 5,004 回
  （無追跡 39,363 回→追跡 44,367 回、saved=6780・コード長 35342・
  ノード数 11001→15501）。500 件の single-use ローカルが対象である。
- D0（relate 無効化）：追跡差 2,004 回。D1（derive_from_clone 無効化）：
  追跡差 2,004 回。D2（両方無効化）：追跡差 2,004 回。すなわち relate の
  3,000 回は derive による共有スロットへの後続 relate（共有由来ボックスの
  COW＋related／relations バッファ＋理由コード等）としてのみ発生し、
  どちらか一方だけでは 0 回である。これは記録内容そのもの（使用箇所との
  関連）であり、共有コード化では消えない表現フロアである。
- 残差 2,004 回のうち約 500 回が rewrite ごとの fresh コード `Arc<str>`
  であり、本変更で除去した。残りは由来ボックス・strip 側の除去理由コード等
  で、次候補（strip 側のコード共有化）として残る。

## 測定（変更後、locals-single.lua、directpass 11 反復・中央値、再現確認済み）

- 無追跡 39,363 回→38,865 回（-498 回）、追跡 44,367 回→43,369 回（-998 回）、
  追跡差 5,004 回→4,504 回（-500 回、約 -10.0%）。
- saved=6780・コード長 35342・ノード数 11001→15501 は変更前後で同一。
- 無追跡側の減少は、旧 `rewrite` 経路の `finish_rewrite` が無追跡でも
  `Arc::from(transformation)` を行っていたためで、`rewrite_with_code` では
  共有 Arc を渡すため消える。他パスが旧 `rewrite` を使う限り同じ無駄が
  残る点は次候補として記録する。

## 追加掃引（第55サイクル、directpass 14 パス×7 入力）

代表入力以外（locals-single／scale-80／immutable／dense-noalias／interval／
inline-50／draw）での掃引で、100 回超の追跡差は次のとおり。draw は
第54サイクルと完全一致し、新バイナリの再現性を確認した。

- locals-single：api-alias 3,382 回、scope-renaming 2,348 回、
  root-local-globalization 1,018 回、single-use-local-sinking 5,004 回
  （本変更の対象。ほかはフロア確定済みの値と同傾向）。
- immutable：api-alias 3,553 回、immutable-carrier-synthesis 7,273 回
  （ただし母数が約 3,251 万回で比率は約 0.02%。パス自体のコスト構造が
  別であり、今回は対象としない）。
- scale-80：api-alias 1,280 回、constant-folding 671 回など。
- dense-noalias／interval／inline-50：既知パスのみで新規レバーなし。

## 検証

- 対象 3 クレート：syntax 128 件（新規テスト 1 件含む）・minify 742 件・
  build 127 件、全成功。ワークスペース全件 50 スイート・1,251 件全成功。
  `cargo xtask check`・`cargo fmt --check`・lib の clippy（警告 0）通過。
- Tier FULL コーパス比較（開発ホスト1 ローカルコーパス、manifest 照合
  4,155/4,155 OK、manifest ハッシュ abfec87a…、内部並列 -P 4）：
  base＝統合先端 13d0e26 対 head。4,155 ファイル×4 条件、欠落 0。
  両 TSV はバイト同一（SHA-256 c4f4f6fb…）。全条件 identical=4,155、
  hard・core・expl・missing すべて 0。成功 4,127／失敗 28 は両側同一。
  検証バイナリの SHA-256 相違も確認（base e90f052f…、head 020a8f9b…）。
