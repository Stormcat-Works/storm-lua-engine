# 生成由来の解決時 interning のメモ化（2026-10-27、未公開）

compile の固定費のうち、生成由来の解決（`GeneratedOrigins::from_arena`）
に残る interning の無駄をなくした。生成 Lua・候補選択・schema1 の
マップ本体は維持する。Map 比 1.2 の目標は未達のままであり、この変更は
その達成を主張しない。

## 原因

全パス無効の compile で、Map ありと Map なしの差は約 7.3ms 残り、
そのうち解決本体が約 4.0ms と最大の塊だった。内訳は、gap ループの
interning が約 1.07ms、constructs ループが約 0.81ms（ほぼ interning）、
dispositions ループが約 0.57ms、残りの骨格が約 1.19ms だった。
gap・constructs の interning は 1 件あたり約 120ns かかり、由来の clone と
ハッシュ・照合が毎回走っていた。

- 連続する gap はしばしば同じ勝者（最も具体的な active 範囲）を持つが、
  勝者が変わらない場合でも毎回 interning し直していた。
- constructs はノード毎の主由来を interning するが、その主由来は多くの
  場合すでにどこかの gap の勝者として interning 済みだった。
- dispositions は 10,726 件のノード id を HashSet へ insert して重複除去
  していたが、ノード id は arena への索引そのものである。

## 変更

- `crates/storm-lua-syntax/src/provenance.rs` のみ変更し、公開 API の
  シグネチャは変えていない。
- gap ループに直前勝者キャッシュを入れた。reason がなく、選択した
  StoredOrigin が直前 gap とポインタ同一なら、interning を迂回して
  既存 id を再利用する。reason がある場合は由来が派生するため対象外で、
  従来どおり interning する。`append_detail` は id を返すよう分割し、
  merge 処理は `append_with_id` へ分離した。
- constructs ループに node メモを入れた。reason がなく、主由来が gap で
  すでに interning 済みなら、その id を再利用する。token／名前由来は
  対象外で、reason ありも従来どおり interning する。
- dispositions の重複除去を HashSet から `Vec<bool>` の bitset へ変えた。
  意味は同一である。

メモ化した id は正確である。arena のスロットは解決中は不変で、同一の
StoredOrigin に対する `get()` は同一の由来値を返し、reason なしの由来は
派生しないため、interning が返す id と一致する。

## 測定

合成入力 dense-normal（46,022 バイト、ノード 6,883 件、emission
10,725 件）で、開発ホスト2 上で 31 反復の中央値を測った。

- 解決を含む測定（`resolve_into_owned`）：約 5.46ms から約 4.72〜4.74ms
  へ減った（約 −13%）。解決本体は約 4.0ms から約 3.33ms へ減った。
- 解決結果の件数は変更前後で同一（mappings 9,284・origins 10,725・
  contexts 0・constructs 6,883）。
- 全パス無効の compile の Map 差は約 7.3ms から約 6.7〜6.9ms へ減った。

## 検証

- 対象 3 クレート（syntax・minify・build）とワークスペース全件のテストが
  成功した。xtask check・fmt・clippy（警告 0）を通過した。
- 実コーパス全件（4,155 ファイル×4 条件）の比較では、成功／失敗、
  生成コード、Map 中核、正規化済み Map 全体のすべてが差分 0 件だった。

## 不採用

- seed の融合（名前ループの主ループ化＋syntax token のスタック配列化）は
  同サイクルで試したが、positions＋seed が 2.25〜2.27ms から
  2.26〜2.28ms と変わらず、不採用として revert した。token・名前の残り
  コストは走査や `Arc::make_mut` ではなく由来そのものの構築と確保にあり、
  融合だけでは削れない。由来表現の変更なしに seed をさらに削る手は
  見当たらない。
