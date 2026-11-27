# VM スナップショット（完全な VM 状態の捕捉・復元）2026-11-04

storm-compat-server の suspend 機能（項目4）の土台として、`storm-lua-vm` に
完全な VM 状態捕捉を追加した。再実行や savedata のみの復元ではなく、
ヒープグラフそのものを保存・復元する。

- ブランチ: `muse/vm-snapshot`（ベース `cccac2215b87e0848605dc9377a64345ced03a28`）
- 実装: `crates/storm-lua-vm/src/snapshot.rs`（新規）
- 配線: `crates/storm-lua-vm/src/runner.rs`（登録パスの保持）、
  `crates/storm-lua-vm/src/value.rs`（`register_function` での自動登録）、
  `crates/storm-lua-vm/src/lib.rs`（モジュール宣言）

## 公開 API

- `Vm::snapshot(&self) -> Result<Vec<u8>, VmError>`
- `Vm::restore(&mut self, blob: &[u8]) -> Result<(), VmError>`
- `Vm::register_host_path(&mut self, path: &[&str]) -> Result<(), VmError>`
  … ホストが環境へ種付けしたテーブル/関数をパスで明示登録する追加 API。
- `snapshot::FORMAT_VERSION: u32 = 1`

いずれも既存のアイドル境界（`ensure_idle`）でゲートする。実行中・失敗済みの
VM では型付きエラー（Busy / Failed）を返し、黙って初期化したりはしない。

## 捕捉方式

サンドボックス環境テーブルを根とするヒープグラフ歩行。

- スカラ: nil / 真偽値 / 整数 / 浮動小数（ビット列のまま。NaN ペイロードと
  -0.0 を保持）/ 文字列（バイト列のまま、\0 を含んでもよい）。
- テーブル: オブジェクト ID マップで同一性を保持。素のテーブルの循環
  （`t.self = t`）も往復する。キー・値ともグラフ値。
- Lua クロージャ: プロトタイプは `Function::dump` のバイトコードで重複排除
  し、アップバリューはセル単位で捕捉する。セルの同一性は
  `lua_upvalueid` で判定し、復元時は新セルへ `lua_upvaluejoin` で再結合
  するため、2 つのクロージャが共有するアップバリューは復元後も共有される。
- ホスト値レジストリ: ホストが種付けした値は環境内のパス
  （例 `["math", "random"]`）で登録し、シンボル参照として保存する。復元先の
  新鮮な実体へ再結合するため、古いクロージャのコピーは持ち込まれない。
  - 登録は構築時に自動で行う（環境のトップレベル項目＋その直下 1 階層）。
    `register_function` も名前空間パスを自動登録する。ホストは
    `register_host_path` で追加できる。
  - パスは捕捉・復元のたびに現在の環境へ再解決する（ポインタ対応は
    捕捉ごとに作り直す）。ホストが構築後にエントリを差し替えても
    （例: compat の CRT random 差し替え）追随する。
  - 登録済みホストテーブル自体は「歩行して再マップ」する：スクリプト側の
    変更・追加・削除は素のキー集合として記録され、復元先の同パスの
    テーブルへ調整（reconcile）される。
  - 登録済みホストテーブルの中に直接置かれた C クロージャ
    （例 `debug.log`）は値として保持できないため、キーだけを記録し、
    復元先の同キー実体の存在を確認して温存する。
- 復元先の環境テーブルと同期コールバック用の環境ハンドルは同一オブジェ
  クトを共有するため、復元は既存の環境テーブルを置換せず、その場で
  調整する（オブジェクト同一性を維持）。

## 型付きエラー（panic しない・黙って初期化しない）

以下の遭遇はすべて型付きエラー（`ErrorKind::Unsupported`）で表面化する。

- スレッド値（コルーチン）を含むグラフ
- ユーザーデータを含むグラフ
- メタテーブルを持つテーブル
- 実グローバルテーブルへの到達（サンドボックス境界の破れ）
- 未登録の C クロージャ（登録漏れのバグとして表面化させる）

blob 側の検証も全て型付きエラー：マジック不一致・版不一致は
`InvalidArgument` / `Unsupported`、参照不整合・上限超過は
`InvalidArgument` / `Limit`。復元はプロファイル（Game / Extended）の
一致も要求する。

## 形式の概要

クレート所有のバージョン付きバイナリ形式（新規依存なし、手書きの
リトルエンディアン符号化）。

```
"SLVMSNP\0" | u32 版 | u8 プロファイル
シンボル表（パスの列）| プロトタイプ表（dump バイト列）
平テーブル表（ID 順、キー値ペア）| ホストテーブル表（シンボル→定義）
クロージャ表（プロトタイプ参照＋セル ID 列）| セル表（グラフ値）
根のグラフ値（必ず平テーブル 0 = 環境）
```

グラフ値はタグ付き（Nil/Bool/Int/Num ビット/Str/Table/HostTable/
Closure/HostRef/HostEntry）。復号は検査付きカーソルで行い、blob
サイズ・要素数・文字列長に上限（Limit）を設ける。

## テスト

`cargo test -p storm-lua-vm`: **54 passed / 0 failed**（従来 39 件＋新規
15 件）。最終ログはレーン内 `vm-snapshot-test-final.log`（本記録に件数を
転記し、レーン撤去とともに破棄する）。

新規テスト（`snapshot::tests`）:

1. `shared_table_identity_round_trips` … 共有テーブルの同一性
2. `cyclic_table_round_trips` … 循環テーブルの往復
3. `closures_sharing_an_upvalue_still_share_after_restore` …
   共有アップバリューの片方からの変更が他方に見える
4. `mutated_upvalue_state_round_trips` … 変更済みアップバリュー状態
5. `nested_data_and_strings_round_trip` … 入れ子・\0 含み文字列
6. `restore_into_the_same_vm_rewinds_state` … 同一 VM への巻き戻し
7. `host_table_mutations_round_trip_and_entries_rebind_to_fresh_instances` …
   ホストテーブルの変更の往復＋実体が新鮮な側であること（カウンタで検証）、
   未登録 C エントリの温存
8. `host_function_stored_in_a_script_table_rebinds` …
   スクリプトのテーブルにしまった登録済み関数の再結合
9. `extended_profile_wrapped_pcall_round_trips` …
   Extended の pcall ラッパ（Lua クロージャ）の往復
10. `thread_in_graph_is_a_typed_error`
11. `userdata_in_graph_is_a_typed_error`
12. `metatable_in_graph_is_a_typed_error`
13. `snapshot_and_restore_refuse_a_busy_vm` … 非アイドル時の拒否
14. `snapshot_refuses_a_failed_vm` … 失敗済み VM の拒否
15. `corrupt_and_wrong_version_blobs_are_typed_errors`

併せて確認済み:

- `cargo check -p storm-lua-vm --features debug,backend-mlua` … 成功
- `cargo check -p storm-lua-addon -p storm-lua-microcontroller` … 成功
- `cargo clippy -p storm-lua-vm --lib` … 成功（残る警告は既存
  `dialect.rs` のもののみ。`--all-targets` のエラー 3 件も既存
  `dialect.rs` テスト側のもので、本変更の対象外）
- `cargo fmt --check -p storm-lua-vm` … クリーン

## 監査（vm-serialization-audit-20261104）との照合

監査の実現可能性判定は実装でも裏付けられた。実装で判明した精密化は 2 点。

- レジストリは「構築時のポインタ表」では不十分だった。ホストは構築後に
  環境エントリを差し替え得る（compat の `install_crt_random` が
  `math.random` を差し替える）ため、パス登録＋捕捉ごとの再解決とした。
- `sandbox` は標準ライブラリの実テーブルを環境へ入れるが、種付けの自動
  登録は「トップレベル＋直下 1 階層」で過不足なく賄えた。`string` /
  `table` / `math` / `debug` の各エントリがこの範囲に収まる。

監査の主張を覆す結果はなかった。

## 名前付きの残件・限界

- **コルーチン捕捉は対象外**：両プロファイルともスクリプトから
  コルーチンを生成できない（ライブラリ自体を公開していない）。公開 C
  API では実行中コルーチンの状態捕捉もできないため、遭遇自体を
  型付きエラーにしてある。
- **sandbox 既定 `math.random` の種は未捕捉**：種は Rust クロージャ内の
  `Rc<Cell<u64>>` にあり、ヒープから到達できない。恒久対応は
  storm-lua-vm に種の取得・設定 API を足すフォローアップとする。
  compat はホスト所有の scene CRT random を使うため本件の影響を受けない
  （その状態はワールド側スナップショットが持つ）。
  （2026-11-27 追記：v0.5.0 で storm-lua-vm に `random_stream_state` /
  `set_random_stream_state` が実装され、このフォローアップは完了した。
  上記は当時の記録として残す。）
- **復元はトランザクションではない**：復元途中でエラーになった VM は
  破棄すること（メソッドの doc に明記）。捕捉側は無変更。
- パスの要素は UTF-8 文字列のみ登録可能（非 UTF-8 キーは対象外）。
- 文字列のインターン同一性は保持しない（Lua の文字列は値比較であり、
  意味論上の差はない）。
