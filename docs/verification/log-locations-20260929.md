# ログ発生位置の検証（2026-09-29）

状態: 後続ブランチで検証完了。公開済みv0.2.0の機能とは区別する。
ブランチ: `feat/storm-code-integration`、基点: `6e90a82`。
契約の正本: [runtime-host](../specs/runtime-host.md#ログ発生位置未公開の後続版)。

## 実装と確認範囲

VMの共通ログ関数で実行時のLuaフレームを検査し、構造化ログの位置へ保存する。VMに新たな常時line hookは追加していない。WASMは位置の有無を含め配送し、TSはoptionalなLogLocationへデコードする。Luaへdebug.getinfoを公開せず、onLogからのVM再入も許可しない。

Nativeのdebug feature無効状態、ログ生成時と配送時の違い、別名のprint、requireで呼んだ別ファイル、tick/draw、reset、停止・失敗前のログ、Addon、非UTF-8、位置なし、チャンク名のメモリ上限を検証する。非短縮リンクのcode/mapを用いた元ファイルへの変換も、実WASMと通常のSource Map consumerで検証する。

通常のファイル名付き実行の位置を提供する変更であり、最適化後の原文位置や列、消失した末尾呼び出しの復元は対象外。Storm CodeのTerminal・実行世代との接続はconsumer側の次段階。性能改善は主張せず、ログ頻度別の追加コストは未計測。

## 実行記録

| ゲート | 結果 |
|---|---|
| Native変更対象conformance | 4 PASS |
| VM no-default-featuresのログ単体 | 2 PASS |
| TypeScript build・consumer・SDK JS | 29 PASS |
| 隔離npm consumer（梱包済みSDK） | PASS。位置表示例も実行し、main.lua:1とlib/logger.lua:2を確認 |
| Python工具・画面fixture検査 | 19 PASS、731ケースとfont digestを検証 |
| WASM runtime/rasterの再ビルド | PASS |
| 実WASM全スイート | 53 PASS（今回追加5件を含む） |
| Chromium / Firefox / WebKit | すべてPASS。各731の直接RGBA・731のLua描画ケース、独立Worker2台、debugger/hostサービス/ログ位置を確認 |
| 全Native回帰 | 478 PASS / 0 fail / 0 skip |
| fmt / clippy / default check / docs / architecture | 全てPASS（rustdoc broken intra-doc linksはエラー扱い） |

初回のWASMビルドはOS付属emccとwasm-optの組み合わせが不整合で失敗した。環境に導入済みの同一Emscripten SDKをPATHで選択し直して成功した。プロジェクトのビルドスクリプトに個人パスは追加しない。

初回の全Nativeビルドは6GiBのtmpfsを使い切り、テスト開始前に失敗した。実行プロセスが終了したことを確認し、このEngineのdevビルドキャッシュのみをcargo clean --profile devで削除した。CARGO_INCREMENTAL=0、CARGO_PROFILE_DEV_DEBUG=0、CARGO_PROFILE_TEST_DEBUG=0、CARGO_BUILD_JOBS=2で全回帰を再実行して成功した。Luaのdebug featureを無効化する設定ではない。

## 公開境界

package/Cargoの公開版番号、タグ、npm公開、Playground配備は変更しない。Rust LogRecordの新しいフィールド、TSのoptionalな追加情報、ログ上限へのチャンク名算入はCHANGELOGのUnreleasedに明示する。旧JSONレコードは受理するが、不正な位置を無視して成功扱いしない。
