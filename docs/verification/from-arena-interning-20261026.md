# 由来解決の interning 高速化（2026-10-26、未公開）

`GeneratedOrigins::from_arena`（選択された候補の由来解決）の内部コストを下げた。
生成 Lua・候補選択・診断・schema1 の意味と精度は維持する。Map 比 1.2 の目標は
未達のままであり、この変更はその達成を主張しない。

## 変更

`crates/storm-lua-syntax/src/provenance.rs` の `from_arena` のみを変更した。

- 従来、重なり区間の active 集合を `BTreeSet` で管理していたが、キーが
  （区間長・名前サイト tiebreak・emission 番号）で一意であり、集合の大きさは
  入れ子の深さ程度に小さいため、ソート済み `Vec`＋二分探索へ置き換えた。
  `first()`・挿入・除去の順序意味は同一で、範囲ごとのヒープ確保をなくした。
- 由来・inline context の interning マップのハッシュを、決定的な FxHash 風の
  小さいハッシュへ置き換えた（依存追加なし・標準ライブラリのみ）。
  origins・contexts・mappings の出力順序はすべて初出順で決まるため、
  ハッシュ関数の変更は出力を変えない。`append`/`append_detail` は
  ハッシュ実装に依存しないようジェネリクス化し、テスト側の従来マップも
  そのまま通る。

## 測定

合成入力 dense-normal（46,022 バイト、原文 480 行相当）で、VM 2 コア、
load average 0.2〜1.7 程度、各条件 21 反復の中央値。

| 測定 | 変更前 | 変更後 |
| --- | ---: | ---: |
| 由来解決のみ（原文 AST の印字＋解決） | 13.2〜14.4ms | 8.7〜10.5ms |
| 全パス無効時の compile（Map あり−なしの差） | 約 15.7ms | 約 13.0ms |
| 既定 compile の Map あり−なしの差 | 約 72ms | 約 72ms（誤差範囲） |

既定 compile の差がほぼ変わらないのは、選択される候補の生成物が小さく、
解決コスト自体が差のうち数 ms に過ぎないためである。解決コストの削減は
大きな生成物・全パス無効経路・由来解決を直接使う側で効く。

## 検証

- 生成コードは変更前後で同一（SHA-256 一致）。
- 公開 API のマップは、mappings・names・sources・sourcesContent・version が
  変更前後で同一。`x_storm` の差異は `producer.revision`（変更前 64c2d88 から
  本コミットへ）と、それを覆う `integrity` のみである。未コミット時には
  さらに `producer.dirty` が true になることも確認した。いずれも
  生成物を識別する側の正しい変化であり、マップ本体の内容は同一である。
- `cargo fmt --check`（対象クレート）・`cargo clippy`（対象クレート）を通過。
- テスト結果は下記に記録する。

## テスト

- `cargo test -p storm-lua-syntax -p storm-lua-minify -p storm-lua-build`：
  syntax 102 件・minify 440 件・build 126 件、すべて成功（失敗 0 件）。
- `cargo xtask check`：依存所有・DAG・公開 Markdown・描画契約を通過。
- `cargo fmt --check`（対象クレート）・`cargo clippy`（対象クレート）を通過。
