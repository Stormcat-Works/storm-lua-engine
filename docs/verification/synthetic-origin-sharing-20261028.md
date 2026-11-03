# 合成由来の共有による追跡コスト削減の検証（2026-10-28）

## 変更

`crates/storm-lua-syntax/src/node_arena.rs` のみ。

- `mark_synthetic` が、同一 transformation 名でマークする全ノードに
  対して 1 個の `NodeOrigin` を共有するようにした（上限 4096 件の
  静的キャッシュ。超過時は従来の fresh 生成へ fallback）。
- 合成由来は transformation 名だけで決まる（残りは
  `NodeOrigin::default`）ため、同一名の共有は記録内容が同一である。
- 共有の安全性：由来への in-place 変更はすべて `Arc::make_mut`
  経由の copy-on-write で行われるため、共有ノードを後から変更しても
  他の共有者は影響を受けない（ブロック文除去経路の derive_in_place
  がその例）。`derive_from_clone` の ptr_eq 短絡も、短絡前後で
  スロットの最終値が同一のため結果は変わらない。

## 不採用とした案

- `OriginSlots::from` における全 None ページの共有：attach 直後に
  ほぼ全ページへ書込みが入る api-alias 転送経路では、分離
  （detach）で 1 ページあたり 1 確保が戻り、collect 分と合わせて
  逆に +92 回・+47,104 B になったため revert した（診断で分離）。
  共有ページが有効なのは書込みが稀なテーブルに限る。

## 測定（合成入力 dense-normal、VM、中央値）

- directpass（11 反復）：api-alias-optimization の追跡差
  2,765 回 → 2,685 回（−80 回）。scope-renaming 1,181 回・
  root-local 652 回・constant-folding 2,258 回は不変。無追跡側・
  saved・コード長・ノード数はすべて同一。時間差は主張しない。
- trackcost single（5 反復）：全コンパイル追跡差 64,122 回 →
  63,932 回（−190 回）。単独オン差は api-alias 12,432 回 →
  12,352 回（−80 回）、scope-renaming・root-local・
  constant-folding・all-off 差は不変。単独オン削減を上回る
  全コンパイル削減の残りは、全コンパイル内の他の経路での
  同型の共有によるもので、厳密な内訳までは主張しない。

## テスト

- 回帰テスト `synthetic_marks_share_one_origin_and_cow_mutation_stays_local`
  を追加（同一名マークのポインタ共有と、ブロック文除去による
  copy-on-write 変更が他の共有者に漏れないことを検証）。
- 対象 3 クレート（build 126・minify 631・syntax 123＝新規 1 件含む）
  全成功、ワークスペース全件 50 スイート・1,134 件全成功。
  `cargo xtask check`・`cargo fmt --check`・clippy（対象 3 クレート、
  警告 0）通過。
- 検証区分は Tier INVARIANT（構成上出力不変の共有化）のため、全件
  コーパス（4,155 ファイル×4条件）は次回 Tier FULL の累積比較
  （起点 882169d0）へ繰り延べる。INVARIANT 連続は本サイクルで
  2 件目。
