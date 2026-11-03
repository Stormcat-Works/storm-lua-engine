# seed の validate 高速化と parser token 直接受け渡し（2026-10-27、未公開）

compile の固定費のうち、由来 seed（`ArenaOrigins::seed_with_tokens`）に
残る 2 つの無駄をなくした。生成 Lua・候補選択・schema1 のマップ本体は
維持する。Map 比 1.2 の目標は未達のままであり、この変更はその達成を
主張しない。

## 原因

第16ラウンドで seed の 2 回目の字句解析をなくした後、positions＋seed は
約 2.34〜2.40ms 残った。その内訳のうち、validate が約 0.15ms、parser
token から BorrowedToken への変換 Vec（8,645 件）が別途かかっていた。

- validate のうち、スロット毎の名前重複検査に `BTreeSet` を使っていた。
  名前列は大半が 1 件以下で、1 件のスロットでは重複は原理的に起きず、
  2 件以上の場合も線形走査で同じ重複を報告できる。BTreeSet の確保は
  名前付きスロット毎に 1 回のヒープ確保を強いていた。
- origin 経路ではパーサーのトークン列を `BorrowedToken` 化する中間 Vec
  を作ってから seed へ渡していた。綴りは元ソースのスライスであり、
  seed が必要とするのは位置・終端・綴りの一致判定だけだった。

validate の debug-only 化は検討したが、release での不正 span 検出の
安全網を失うため採用しない。span 検査・名前照合・スロット走査そのものは
温存し、挙動を変えない範囲だけを高速化した。

## 変更

- `crates/storm-lua-syntax/src/parser.rs`・`provenance.rs` のみ変更し、
  公開 API のシグネチャは変えていない。
- `ArenaOrigins::validate` の名前重複検査を、BTreeSet 確保から線形走査へ
  変えた。報告する重複（およびその順序）は従来と同一で、逆直列化・
  from_parts 経路の検査も同じ意味のままである。
- `seed_with_tokens` を `SeedToken` トレイトで汎用化した。fresh-lex の
  `BorrowedToken` は綴りを直接持ち、パーサーの `ParserToken` は元ソース
  から綴りを切り出して一致判定する。origin 経路はパーサーのトークン列を
  直接 seed へ渡し、中間 Vec を作らない。範囲外トークンは従来どおり
  `parser token is outside its source` で失敗する（検査対象トークンに
  限る点だけが変わるが、パーサーのトークンは同一ソースの字句解析結果
  のため実際には起きない）。
- fresh-lex 経路と parser-token 経路の同一性は、既存テスト
  `conditional_source_tracking_uses_the_same_parse_and_complete_origins`
  の直列化比較で担保される。

## 測定

合成入力 dense-normal（46,022 バイト）。各条件 31 反復の中央値。
共有の 12 コアマシンで他ワーカーと共有のため、時間は参考値とする。

| 測定 | 変更前 | 変更後 |
| --- | ---: | ---: |
| positions＋seed（parse_with_origin_positions） | 約 2.36ms（2.342〜2.374ms） | 約 2.27ms（2.249〜2.286ms、約 −0.09ms） |
| うち validate の BTreeSet 回避のみ | 約 2.36ms | 約 2.30ms（約 −0.06ms） |
| positions 付き parse | 約 1.04ms | 約 1.05ms（不変） |

## 同一性

- 合成入力 dense-normal のノード数 6,883・トークン数 8,645 は変更前後同一。
- 実コーパス検証（StormMin real-world corpus、4,155 ファイル×4条件）では、
  成功／失敗（成功 4,127／失敗 28 ファイル）・生成コード・Map 中核・
  エラー文ハッシュ・正規化済み Map 全体のすべてが完全一致し、差分は
  0 件だった。両 TSV はバイト同一である。
