# metadata除去と詳細Source Mapの修正

2026-10-12。公開v0.3.1（`5a168d6`）を基点にした未公開の修正。公開済みタグと配布物は変更しない。

## 原因と修正

`metadataTag`、`minify:false`、`sourceMap:true`を併用すると、空白化した先頭metadataをリンカーの完全一致コピー区間に残したまま、除去前のsourcesContentへ合成していた。Nativeと公開WASMで追加回帰が失敗することを確認した。整合性検査による拒否は正当であり、validatorは変更しない。

metadataの処理時に、元のUTF-8バイト区間を記録する。通常/LifeBoat/Addonのリンク後にその区間を出力位置へ移し、共通の区間差分処理でコピー区間から除外する。文字列の比較で由来を推測せず、元snapshotを置換しない。標準mapでは空白化部分をunmappedにし、詳細mapではcopiedを持たないSyntheticとする。残りのコピー区間は元のbyte/UTF-16位置を保持する。

用いた共通処理はLifeBoatの開発区間除去に使っていた区間差分処理である。map生成では全除去された使用moduleのsourcesContentも保持する。metadataTag未指定時は元projectを借用し、除去区間の走査・コピー区間の再構築を行わない。

## 回帰と検証

Nativeの`metadata_map_tests`と実WASMのCompiler回帰に、報告例、Unicode/CRLF、長いcomment delimiter、コメント直後の同じ行に続くコード、複数module、metadataだけのmodule、Addon標準map、最適化ON/OFF、標準/詳細map、8192目標での原文返却と保守的短縮を含めた。標準Source Map readerで残存tokenの元位置と除去部分の非対応を確認し、詳細validatorでcopy内容の整合性を確認する。metadataとLifeBoat開発専用区間の同時除去も検証する。

最終関連ゲートはすべてexit 0。Native全workspace/all-featuresは875成功・失敗/ignored 0、SDK型/独立consumer型/JSは30成功、最終Rustから再ビルドした実WASMスイートは91成功・失敗/skip 0。Chromium/Firefox/WebKitのcompiler Worker、隔離した梱包SDK consumer、fmt、全workspace/all-target/all-feature clippy（警告をエラー）、architecture/docs/fixture、artifact/package検査、diff checkも成功した。

初回の修正前回帰はNative exit 101、公開WASM exit 1。最終検査中に追加回帰の不要な構造体updateをclippyが拒否し、記述を直して再検査した。これらの初回ログも保持する。最終Native回帰後の変更は、回帰テストの意味を変えない不要updateの除去と文書の結果更新だけである。

ログ・rc・再ビルドしたcompilerとソースpatchはGit除外の`local-validation/metadata-source-map/`の永続領域へ保存する。ビルド用Cargo cacheはtmpfsであり、再開の唯一の依存にしない。

## 公開境界

JS/Rustの公開APIとSource Map v3・x_storm schema1は維持する。公開v0.3.1へ修正を差し替えたとは扱わない。修正版のcommit・push・版番号・公開はこの検証と区別する。

ローカルtarballを隔離導入した独立consumerでも、報告された通常/LifeBoatのmetadata・非短縮・詳細map条件が成功した。producer.dirty=trueの修正スナップショットであり、npm公開済み0.3.1とは区別する。ログと機械記録・ソースmanifest・ローカルtarballのhashを永続領域で検証した。
