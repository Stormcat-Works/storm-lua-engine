# parse 由来 seed の字句解析再利用による固定費削減（2026-10-27、未公開）

compile の固定費のうち、由来 seed（`ArenaOrigins::seed`）で元ソースを
2 回目に字句解析していた部分をなくした。生成 Lua・候補選択・schema1 の
マップ本体は維持する。Map 比 1.2 の目標は未達のままであり、この変更は
その達成を主張しない。

## 原因

`parse_source_with_origin_positions` は、パース時に全トークンを字句解析
して `Parser::ts` へ保持しているにもかかわらず、seed 内で
`Lexer::new(text).all_borrowed()` により同じテキストをもう一度字句解析
していた。合成入力 dense-normal（46,022 バイト）では、plain parse
0.85ms・positions 付き parse 1.05ms に対し positions＋seed は 2.50ms で、
seed 単体は約 1.45ms あった。その内訳は、2 回目の字句解析が約
0.17〜0.27ms、name_spans ループが約 0.40ms、validate が約 0.15ms、
syntax token 検索＋収集が約 0.19ms、残りがノード毎の由来確保などである。
識別子綴りの共有（interning）も試したが、約 0.03ms しか効かず不採用と
した。name_spans ループの残りは、push・make_mut・validate・破棄に
分散しており、単独の小手先では削れないことを確認した。

## 変更

- `crates/storm-lua-syntax/src/parser.rs`・`provenance.rs` のみ変更し、
公開 API のシグネチャは変えていない。
- origin 経路ではパース直後に `Parser::ts` を `mem::take` し、綴りを
元ソースから切り出して `BorrowedToken` 化してから seed へ渡す。
パーサーのトークン列は字句解析器の出力と同一（同じ `next_borrowed`
の列で、EOF を含む）であるため、seed される由来は変わらない。
- seed の字句解析部分を `seed_with_tokens` へ分離した。旧 `seed`
ラッパーは未使用警告が出たため削除し、テスト側の expected 構築も
`seed_with_tokens`（fresh lex）へ寄せたため、fresh lex と parser
token の等価性は既存テストの直列化比較で担保される。
- 位置なしの `parse_source` や seed なしの `parse_source_with_positions`
の経路は変えていない。

## 測定

合成入力 dense-normal（46,022 バイト）。各条件 31 反復の中央値。
共有の 12 コアマシンで他ワーカーと共有のため、時間は参考値とする。

| 測定 | 変更前 | 変更後 |
| --- | ---: | ---: |
| positions＋seed（parse_with_origin_positions） | 約 2.50ms | 約 2.34〜2.40ms（約 −0.11〜0.16ms） |
| positions 付き parse | 約 1.05ms | 約 1.04ms（不変） |
| 全パス無効の Map あり compile | 9.61〜9.88ms | 9.93ms（ノイズ範囲） |

## 同一性

- 合成入力 dense-normal の解決結果は mappings 9,284・origins 10,725・
contexts 0・constructs 6,883 で変更前後同一。
- 実コーパス検証（StormMin real-world corpus、4,155 ファイル×4条件）では、
。
