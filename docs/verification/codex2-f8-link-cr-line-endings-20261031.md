# link の行区切り計算が CR 行末でずれ panic／無効な連結を生む（Codex 第2回指摘8、2026-10-31、ワーカー w9）

## 症状

第54サイクルで先端 12eca85eb において再現済み（panic はバイト同一）。

- モジュール `"--x😀\rreturn 3"`（minify:false）で link 中に panic：
  "start byte index 6 is not a char boundary; it is inside '😀'"。
- ASCII のみの `"local x=1\rreturn x"`（minify:false）では panic しないが、
  `__stormmin_link_m =x=1\rreturn x` という無効な連結 Lua が生成された。
- 同ケースの minify:true では "expected expression at 4:21, got =" と
  誤って拒否された。
- LF 版と CRLF 版は正常（CRLF は `\n` がペアの2バイト目にあるため、
  `\n` だけを探す計算でも偶然一致していた）。

## 機構

パーサ位置は lexer の意味論で付けられる。lexer（storm-lua-syntax）は
`\n`、`\r`、`\r\n`、`\n\r` のいずれも1つの改行として数える。一方
link.rs のバイト位置計算は `\n` だけを数えていた：

- `line_col_to_byte`（require 文の開始位置と TailReturn の return 開始
  位置の変換）——CR 行末では行頭を取り違え、文字の途中や文の途中を指す。
- `line_of`／`count_newlines`（区間の元行・出力行の計算）——同じずれ。
- `append_source_slice` の末尾行計算（`matches('\n')` と
  `ends_with('\n')`）——同じずれ。

なお、位置の別層である `LineIndex`（storm-lua-syntax）は LF/CRLF の
行索引として設計されており、本件の対象ではない。バグは link.rs が
lexer 由来の位置を `\n` だけのバイト計算で受ける混在にあった。
同じ crate の lifeboat.rs／lifeboat_ambient.rs にも `\n` だけを数える
行計算があるが、それらは本件の再現経路（compile_project → link.rs）
ではなく、出力行の対応付けに限る別リンカのため本修正には含めず、
同種の不整合として申し送りに留める。

## 修正

`crates/storm-lua-build/src/link.rs` のみ。

- 共通ヘルパ `newline_len` を追加：バイト位置が行区切りの先頭なら
  その長さ（ペア `\r\n`／`\n\r` は2、単独は1）を返す。
- `line_col_to_byte`、`count_newlines` を `newline_len` ベースの
  走査に置き換え、`line_of` は `count_newlines` へ委譲。
- `append_source_slice` の末尾行計算も `count_newlines` を使い、
  末尾が `\r` の場合も「改行で終わる」として扱う。

## 検証

- c54 link ケース再実行（compile_project 経路、c54link）：修正前に
  panic した unicode／CR（minify:false）が正常な連結＋正しい戻り値
  （3）に転換。無効な連結だった ASCII／CR（minify:false）は
  `local x=1` を保持する正常な形＋戻り値 1 に転換。誤って拒否された
  ASCII／CR（minify:true）も受理され戻り値 1。LF／CRLF の対照は不変。
  追加ケース（エントリ側 CR＋2行目 require → 42、`\n\r` ペア → 3、
  CR モジュールの minify → 5）も全て正常。
- 回帰テスト4件を link.rs に追加（line_col_to_byte／count_newlines の
  単位検証、unicode＋CR、CR の文、CR 行の後の require）。いずれも
  修正前のコードで失敗することを確認済み。
- c54 の f 系ケース（diffexec 5構成）は従前どおり（f06 のみ指摘6 の
  MR ヘッダ形として乖離継続——本修正の対象外）。
- ワークスペーステスト 1,538 件合格（従前 1,534＋新規4件）。
  `cargo xtask check` 合格。clippy の診断は本変更ファイルにゼロ
  （single_member_cache.rs 等の error は既存分）。
- Tier FULL（base = 7837d73＝指摘11 の head、head = 本修正、コーパス
  4,155 件×4条件）：corpuscheck TSV は base／head でバイト単位で
  完全一致（ok 4,128／error 27 は全条件不変）。コーパスは単一ファイル
  のコンパイルであり link 経路を通らないため無影響が期待どおりで、
  かつ生成コードへの波及が無いことの確認となった。
