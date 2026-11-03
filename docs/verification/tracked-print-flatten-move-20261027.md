# tracked 印字の flatten における arena 項目の move 化（2026-10-27、未公開）

tracked 印字の最終段階（flatten）では、rope の葉の順に arena 項目を
clone して排出していた。各 arena 項目はちょうど1葉から参照されるため、
clone する代わりに arena 全体を `std::mem::take` で取り出し、葉の順に
take して排出するようにした。排出物の順序・値は変わらない。arena は
print ごとに record が再構築するため、同一 Printer での再印字にも
問題はない。公開 API は変わらない。

分解測定（計測専用マシンでの新旧交互測定、31 反復中央値）では、
tracked 印字の残りのうち flatten が約 0.26ms、literal_reason が約
0.20ms、残り（arena push＋record 諸経費）が約 0.24ms だった。
move 化の効果は、dense-normal で約 −3%（5ラウンド全勝）、
dense-lexical で互角、draw で不変、scale-80 で約 −1% だった。
flatten の残りの大半は走査自体のコストで、clone 除去だけでは
大きくは削れないことも確認した。

合成入力4件で新旧バイナリの生成コード全文＋全排出物が SHA-256 で
完全一致した。実コーパス全件比較（4,155 ファイル × 4 条件）でも
両 TSV がバイト同一で差分 0 件だった（別途記録）。
対象クレートのテスト（syntax 113・minify 551・build 126）と
ワークスペース全件 50 スイートが成功し、xtask check・fmt・
clippy（syntax、警告0）を通過した。
