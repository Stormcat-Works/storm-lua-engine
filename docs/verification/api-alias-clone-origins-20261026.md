# API 別名クローンの由来共有による追跡コスト削減（2026-10-26、未公開）

API 別名パス（`api-alias-optimization`）の追跡付き再適用で、クローンした
ノードの由来を共有するよう変更した。生成 Lua・候補選択・診断・schema1 の
マップ本体は維持する。Map 比 1.2 の目標は未達のままであり、この変更は
その達成を主張しない。

## 原因

`NodeArena::derive_from` の高速経路は、対象ノードの値全体が源ノードと
等しい場合にのみ由来スロットを共有する。API 別名の `clone_rewriting` は
置換した部分木を丸ごと省くため、最初の置換以降は後続ノードの子 ID が
ずれ、値の等価性がほぼすべてのノードで崩れる。結果として、変更のない
クローンにもかかわらず低速の書き換え経路（理由の生成・トークン／名前の
保持判定・新規スロット確保）が走っていた。置換ありクローンの再現では、
値が等しいノードは 6,883 個中 2,243 個まで減り、由来付きクローンは
0.58ms から 2.68ms へ増えた。

## 変更

- `crates/storm-lua-syntax/src/node_arena.rs` に `derive_from_clone` を
  追加した。源と対象が同じ source snapshot を共有し、かつノードの種類が
  一致する場合に由来スロットをそのまま共有する。種類が異なる場合や
  snapshot を共有しない場合は従来の `derive_from` へ委譲するため、
  本物の置換は従来どおり書き換え理由を記録する。スロット共有自体は
  `derive_from` の高速経路と同じ安全性に依る（後続の変更経路は書き込み
  前にスロットをコピーし、`IndexMut` は先にスロットを空にする）。
- `crates/storm-lua-minify/src/passes/api_aliases.rs` の
  `clone_rewriting` で、クローンしたノード（置換しなかったノード）に
  限り `derive_from_clone` を使うよう変更した。置換ノードと新規ブロック
  は従来の `derive_from` のままである。

## 測定

合成入力 dense-normal（46,022 バイト）で、VM 2 コア、各条件 31 反復の
中央値。変更前後を交互に測定した（変更後 2 回分の範囲を示す）。

| 測定 | 変更前 | 変更後 |
| --- | ---: | ---: |
| api-alias パス単体（追跡あり−なしの差） | 約 7.2ms | 約 3.7ms |
| api-alias のみ compile（Map あり−なしの差） | 約 25.2ms | 約 19.5〜20.1ms |
| 既定 compile（Map あり−なしの差） | 約 63.5ms | 約 50.7〜59.2ms |

既定 compile の差の幅は、他パス由来の分散コストと測定時の負荷変動を
含むため、単独の改善幅としてはパス単体と api-alias のみの値を主に見る。
生成コード長（10,921 バイト）・勝者・選択エイリアス・削減量は変更前後で
同一である。

## 検証

- 生成コードは変更前後で同一（SHA-256 一致）。
- 公開 API のマップは、mappings・names・sources・sourcesContent・version が
  変更前後で同一である。
- ただし `x_storm` の説明履歴は変わる。従来は変更のないクローンにも
  `api-alias-optimization` の書き換え理由が付いていたが、共有後はその
  冗長な一段が消える（理由文字列は 831 件から 823 件へ）。置換ノードと
  合成定義は従来どおり同パスを記録する。由来の primary・precision・
  kind と dispositions の中身は変わらず、理由の振り直しによる番号の
  付け替えのみが残りの差異である。`producer.revision` と `integrity`、
  未コミット時の `producer.dirty` は生成物を識別する側の正しい変化で
  あり、マップ本体の内容ではない。
- `cargo fmt --check`（対象クレート）・`cargo clippy`（対象クレート）を通過。
- テスト結果は下記に記録する。

## テスト

- `cargo test -p storm-lua-syntax -p storm-lua-minify -p storm-lua-build`：
  syntax 102 件・minify 440 件・build 126 件、すべて成功（失敗 0 件）。
- `cargo test --workspace --all-features --locked`：すべて成功（失敗 0 件）。
- `cargo xtask check`：依存所有・DAG・公開 Markdown・描画契約を通過。
