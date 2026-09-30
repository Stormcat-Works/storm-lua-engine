# v0.3.0 P2 — パース元範囲と生成位置の基盤

2026-09-30。未公開のv0.3.0開発。公開済みv0.2.1のタグ・配布物は変更しない。ここで実装したのはParser/Printerの位置記録であり、最適化器の全パスを通るソースマップの完成ではない。

## 実装

`NodePositions.span`はパースされた全ノードの半開UTF-8バイト範囲を保持する。単なる演算子の位置ではなく、二項式の左右の被演算子、括弧、receiverから続くindex/call全体を含む。従来の診断用`get`の行/列は維持した。

`NameSite`で、名前式、local/loop束縛、function引数、tableの名前付きfield、dot/colon member、goto/labelを区別する。同じinterned symbolを共有しても、出現ごとに別範囲を持つ。暗黙のselfやvarargへ架空の識別子範囲を作らない。通常の非記録parserには位置テーブルの割り当てを追加しない。

`Printer.output_with_positions`は、通常のPrinterと同じ処理で最終文字列と`NodeEmission`を組み立てる。生成の開始/終了バイト、NodeId、任意のNameSiteを保持する。括弧・区切り・改行が入るたびに実際のappend位置から相対範囲を移動し、完成した文字列を検索して由来を推定しない。Printerの意味論を別実装へ複製しない。

通常出力ではemission vectorを空のまま保持する。区切り文字の一時String割り当てを避け、先頭fragmentの不要なコピーも除去した。コードサイズ・候補選択に位置メタデータを混ぜていない。

## 明示した境界

NodeEmissionは生成側のAST位置であり、そのまま元コードのソースマップではない。ASTを変更しない場合はNodePositionsと接続できるが、ノード置換、新しいArena、インライン化、統合などの後は各パスが由来を引き継ぐ必要がある。元位置不明のノードへ近い位置を捏造しない。

文字列のkeyをdot identifierとして出力した場合、元のidentifier slotは存在しないことがある。その場合も元の文字列ノードの範囲とは区別する。nil RHSが印字段階で省略された場合は、そのnilノードの生成範囲を返さない。

`minify`や`build(minify:true)`の公開成果物には、まだ最適化後mapを付けていない。ASTの由来テーブルの世代・複製・統合と各パスの接続、Source Map v3への出力、TS/Worker/Playgroundへの接続は残る。

## テスト

新しいParser5件とPrinter5件のテストで、全ノード範囲、Pratt演算子、複合suffix、同名/影のある変数、宣言/引数/field/member/label、Unicode/CRLF/EOF/空block、リテラル正規化、nilの省略、通常出力とのbyte一致・改行数一致を検証した。

Native workspaceは523件成功。fmt、clippy `-D warnings`、rustdoc、SDK30件、実WASM69件、独立Lua backend probe、Chromium/Firefox/WebKitのcompiler Workerを確認した。Runtime WASMは変更対象外の既存アセットを再利用し、新しいcompiler WASMをreleaseでビルドした。

既存の30入力について、safe/smallest・exact/tolerant・改行有無の240設定でv0.2.1との生成Lua、文字数、候補集合が一致した。さらに各入力のfast/beam1・target=0未達の30件でも最大探索とbyte-identicalだった。[機械記録](source-emissions-030-20260930.json)にartifact hashと試験範囲を記録した。

通常のminify経路のNode/WASM時間も同一プロセスの独立インスタンスで交互比較した。初期化・ファイル入出力を除く各3回の中央値で、位置記録を有効にしたPrinterや未実装の全最適化後マップの追加コストを表す値ではない。

| 入力・設定 | v0.2.1 | v0.3.0開発版 |
| --- | ---: | ---: |
| 約2万文字の制御例・全探索 | 3.789秒 | 3.287秒 |
| 同例・8192目標 | 0.355秒 | 0.397秒 |
| 約13万文字の矩形例・全探索 | 3.323秒 | 3.421秒 |
| 同例・8192未達 | 3.726秒 | 3.733秒 |

実行負荷やJIT/allocatorの状態による変動を含む。短いtarget達成の例で約42ms増え、矩形の全探索で約98ms増えた観測もそのまま記録する。すべてが高速化したとは主張しない。内部の小さな文字列割り当てを減らす修正後の値である。

最終compiler WASM SHA-256は`c250a321b8bba34a75a7b04cad65e49d7d9d98b3d86e7ad33e5de6d57150936c`。523 Native、clippy、実WASM/SDK/ブラウザを同じ最終ソースで確認し、過去の候補結果を再利用していない。

## 実際の対応例

`cargo run -p storm-lua-syntax --example source_emissions`は、パース元を変更せず、生成された位置と元位置を実際に接続する。行は1-based、列は0-based UTF-16で表示する。

| 生成位置 | 元位置 | 対象 |
| --- | --- | --- |
| 1行6列 | 2行6列 | local valueの宣言 |
| 2行21列 | 3行21列 | function引数のvalue |
| 3行0列 | 3行35列 | function内のvalue参照 |
| 5行25列 | 4行25列 | 呼び出し側のvalue参照 |

同じ`value`という文字列でも別の原文位置を保持する。数値は原文の`0x10`から`16`として印字され、元の範囲と生成範囲の長さが異なる。これは位置の対応であり、文字列の同一性・一文字対一文字のマップとは説明しない。

## 残り

[実装計画](../design/source-provenance.md)のP2を進めた段階。次は共通の由来ID/元範囲テーブルを変換と候補分岐へ渡し、各最適化パスの引き継ぎを実装する。変換のない場合に位置が辿れることと、最大短縮後に元ソースへ戻れることは別の完成条件である。


## GitHub上の最終確認

実装commit `47436c6d0035465929341145c7a737496b0a5797`に対する[CI run 36660638108](https://github.com/Stormcat-Works/storm-lua-engine/actions/runs/36660638108)が、Linux/Windows/macOS NativeとWASMの全jobで成功した。WASM jobには実ブラウザーと梱包consumer、Playgroundの検証も含む。公開ワークフロー側main commit `7caef99`のCI `36660638197`も別に成功した。両者は異なるソースを検証するrunであり、同一commitへの重複起動ではない。
