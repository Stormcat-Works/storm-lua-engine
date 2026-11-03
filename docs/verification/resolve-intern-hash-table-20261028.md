# resolve の由来 interning における map キー clone 除去による固定費削減（2026-10-28、未公開）

Map 経路の固定費のうち、`GeneratedOrigins::from_arena` の由来 interning が
計測専用マシンでの変種測定で最大の単一片だった。従来は
`FastMap<Origin, u32>` に由来を intern しており、1 件の intern ごとに
map キー用の clone と origins ベクタ用の clone の 2 回の clone が起きて
いた。実測では由来値はほぼすべてちょうど 1 回だけ intern されるため、
map キー用の clone は結果に寄与しない。

## 変更

- `crates/storm-lua-syntax/src/provenance.rs` のみに変更した（公開 API 不変）。
- ハッシュ→id 表 `OriginInterner` を追加した。`FastHasher` で計算した
  ハッシュを `by_hash: FastMap<u64, u32>` に写し、等価性は既に格納済みの
  origins 側の値と比較して判定する。ハッシュが衝突した id は
  `overflow: Vec<(u64, u32)>` に置き、値比較で区別するため、衝突が結果を
  変えることはない。distinct な由来の clone は origins push の 1 回だけに
  なった。
- `from_arena` の gap ブロック・constructs ループ・最終 append を新経路
  （`append_detail_hashed`／`intern_hashed`）へ寄せた。最終 append は
  origin が `None` のため `append_with_id` の直接呼びと等価である。
- 旧 `append`／`append_detail` は test 専用の reference 実装だけが使うため
  `#[cfg(test)]` 化し、呼び手のなくなった `intern_origin` は削除した。

## 切り分け

- 計測専用マシンでの変種測定（resolvebench、dense-normal、31 反復中央値、
  3 ラウンド交互・順序回転）では、print＋from＋resolve 約 6.85ms に対し、
  gap 省略 約 5.16ms（gap ブロック約 1.68ms）、constructs 省略 約 5.92ms
  （constructs 約 0.92ms）、選択省略 約 5.33ms、intern 省略 約 4.24ms
  （intern 全体約 2.61ms＝10,723 intern で約 243ns/件）、ソート省略
  約 6.51ms（ソート約 0.34ms、順序変化で mappings 数が変わるため参考値）
  だった。intern が最大の単一片である。
- 決定的件数は新旧で完全一致した（dense-normal mappings 9283・
  origins 10723・constructs 6881・dispositions 0、dense-lexical 9287／
  10726／6882／0、draw 1955／1789／1457／0、scale-80 4643／5363／
  3441／0）。

## 効果

- 計測専用マシンでの新旧交互測定（31 反復中央値、各入力 3 ラウンド、
  順序回転）では、print＋from＋resolve は dense-normal が旧中央値
  約 6.76ms から新約 6.05ms（約 -0.71ms、約 -10.5%、3 ラウンド全勝）、
  dense-lexical が約 6.84ms から約 6.13ms（約 -0.70ms、約 -10%、3 ラウンド
  全勝）、draw が約 0.98ms から約 0.81ms（約 -0.17ms、約 -18%、3 ラウンド
  全勝）、scale-80 が約 3.09ms から約 1.80ms（約 -1.29ms、約 -42%、3 ラウンド
  全勝）になった。scale-80 の削減率が大きい理由は未解明だが、12 ペア
  すべてで新が速く、悪化方向の証拠はない。
- 実コーパス全件比較（4,155 ファイル×4 条件、欠落 0 行、両 TSV バイト同一、
  SHA-256 `79523c51…`）では、成功／失敗（ok 16,508 行／error 112 行）・
  生成コード・Map 中核・正規化済み Map 全体すべて完全一致し、差分 0 件
  だった。base バイナリと head バイナリの SHA-256 は相異なることを確認済み
  （base `5b6c2e83…`、head `5bd6b9c2…`）。
