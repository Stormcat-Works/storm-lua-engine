# strip 側の除去理由コード共有化による追跡コスト削減（2026-10-29）

## 変更

single-use-local-sinking パスで除去されるローカル宣言の strip 側
（`strip_removed_locals`）で、除去理由のコード文字列をパス全体で
1 個の共有 `Arc<str>` にした。記録される文字列内容は変わらないため、
生成コード・由来・Map は同一で、変わるのは確保回数だけである。

- `crates/storm-lua-syntax/src/node_arena.rs`：
  `retain_block_statements_with_code` を新設し、従来の
  `retain_block_statements` はそこへ委譲する（呼出 1 回分の
  `Arc::from` のみ）。ブロック由来の `derive_in_place` を
  `derive_in_place_code` へ、除去理由の生成を
  `record_removal_with_code`／`removal_reason_with_code`（記録済み
  Remove 理由がある場合は従来通りそれを複写し、ない場合のみ共有
  コードを使う）へ変更した。旧 `record_removal`／`removal_reason`
  は呼出がなくなったため除去し、lib の未使用警告を出さない。
- `crates/storm-lua-minify/src/passes/locals.rs`：
  `remove_statement_from_blocks_with_code` を新設し、
  `strip_removed_locals` に除去コードを引数で渡す。
  `inline_single_use_locals_with_options` で
  `unused-local-declaration-removal` の共有 `Arc<str>` をパス全体で
  1 個だけ作り、各ラウンドの strip 呼出へ渡す。従来は除去する宣言
  ごとに `retain_block_statements` が除去理由の fresh なコード文字列を
  1 個作っていた（ブロック由来は最初の 1 回で変換済みになるため、
  除去件数分の上乗せはほぼ除去理由分である）。
- 回帰テスト 1 件：
  `retain_block_statements_with_code_matches_retain`
  （新旧両経路の印字由来が一致することを検証）。

## 等価性の根拠

コード文字列は文字列内容で比較・格納されるため、共有 `Arc<str>` を
渡しても記録される由来・理由は新旧で同一である。これは
root-local-globalization の理由共有化や scope-renaming の変換コード
共有化、single-use-local-sinking の理由コード共有化（第55サイクル）
と同じ設計である。Tier FULL コーパス比較（下記）でも全条件で
差分 0 を確認した。

## 測定（変更前後、locals-single.lua、directpass 11 反復・中央値、再現確認済み）

- 変更前（分岐元 9130c9b9＝第142ラウンド）：無追跡 38,865 回・
  追跡 43,369 回、追跡差 4,504 回（saved=6780・コード長 35342・
  ノード数 11001→15501。第55サイクル終了時と一致）。
- 変更後：無追跡 38,866 回（±1 回の誤差範囲で実質不変）・
  追跡 42,869 回、追跡差 4,003 回（-501 回、約 -11.1%）。
  saved・コード長・ノード数は変更前後で同一。
- 削減幅は除去される single-use ローカル宣言の件数（約 500 件）と
  一致し、除去理由コードの共有化がそのまま効いたことを示す。
  残差 4,003 回のうち relate 由来の約 3,000 回は第55サイクルで
  表現フロアと確定済みのため、本サイクルでは掘らない。

## 検証

- 対象 3 クレート：syntax 129 件（新規テスト 1 件含む）・minify 745 件・
  build 127 件、全成功。ワークスペース全件 1,255 件全成功。
  `cargo xtask check`・`cargo fmt --check`・syntax／minify lib の
  clippy（警告 0）通過。
- Tier FULL コーパス比較（開発ホスト1 ローカルコーパス、manifest 照合
  4,155/4,155 OK、manifest ハッシュ abfec87a…、内部並列 -P 4）：
  base＝統合先端 9130c9b9 対 head。4,155 ファイル×4 条件、欠落 0。
  両 TSV はバイト同一（SHA-256 80ef8b65…）。全条件 identical=4,155、
  hard・core・expl・missing すべて 0。成功 4,127／失敗 28 は両側同一。
  検証バイナリの SHA-256 相違も確認（base 19012934…、head 39052893…）。
