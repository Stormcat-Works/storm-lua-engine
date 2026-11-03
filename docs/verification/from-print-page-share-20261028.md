# from_print のページ共有化による Map 経路固定費の削減（2026-10-28、未公開）

Map 経路の固定費のうち、`DeferredGeneratedOrigins::from_print` が
印字済みノードのスロットを1件ずつ clone して新しい arena を組む部分が、
計測専用マシンでの交互測定で dense-normal 約 0.27ms を占めていた。
`from_arena` は印字に現れるノードのスロットしか参照しないため、
参照されるスロットの集合さえ保てば解決結果は変わらない。

## 変更

- `crates/storm-lua-syntax/src/origin_slots.rs` に `OriginSlots::select`
  を追加した（crate 内公開、公開 API 不変）。参照マスクを受け取り、
  全スロット参照済みのページは `Arc` ごと共有し、部分参照のページは
  参照済みスロットだけを clone して再構築し、未参照のページは新規の
  全 `None` ページにする。長さは元のまま保つ。
- `crates/storm-lua-syntax/src/provenance.rs` の
  `DeferredGeneratedOrigins::from_print` を、従来の平坦ベクタへの
  全件コピー＋再ページ化から、参照マスク＋ `select` による構築へ
  置き換えた。印字ノードの走査は2回から1回になった。
- 共有の安全性：ページの変更経路はすべて `Arc::make_mut` 経由で、
  共有ページは書込み時に分離されるため、元の arena のスロットが
  他経路の書込みに晒されることはない。`from_arena` 側はスロットを
  読み取り専用でしか使わない。
- 回帰テスト `select_shares_full_pages_and_rebuilds_partial_pages` を
  追加した（全参照ページの共有、部分参照ページの再構築、未参照
  ページの `None` 化、長さの保持を検証）。

## 切り分け

- 計測専用マシンでの順序回転・交互測定（31 反復中央値）では、
  変更前の from_print 差分は dense-normal 約 0.270ms、
  dense-lexical 約 0.230ms、draw 約 0.041ms、scale-80 約 0.100ms
  だった。
- 変種測定（測定専用パッチ、測定後に復元）では、clone 除去だけで
  差分が dense-normal 約 0.064ms まで縮み、ページ変換除去では
  約 0.038ms まで縮んだ。すなわち from_print の大半はスロット
  `Arc` の clone そのもので、走査＋ベクタ＋ページ変換は小さい。
- ページ被覆の計数（決定的）では、合成入力4件すべてで印字ノードは
  連続した全ノード（dense-normal は distinct 6883件＝max id＋1）で、
  108ページ中107ページが全参照、残り1ページだけが末尾の部分
  ページだった。したがってページ共有化で clone 件数は約 6883件から
  末尾ページの約35件へ減る。

## 効果

- 計測専用マシンでの順序回転・交互測定（31 反復中央値）では、
  from_print 差分は dense-normal 約 0.270ms から約 0.057ms、
  dense-lexical 約 0.230ms から約 0.031ms、draw 約 0.041ms から
  約 0.003ms、scale-80 約 0.100ms から約 0.015ms になった。
- 固定費全体（print＋from_print＋resolve）では、dense-normal が
  約 7.632ms から約 7.455ms になった（逐次ラベルのため幅を持って
  見ること）。決定的件数は変更前後で同一（dense-normal は
  mappings 9283・origins 10723・constructs 6881・dispositions 0）。

## 検証

- 対象3クレート（syntax 114＝新規1件含む・minify 580・build 126）
  全件成功。ワークスペース全件 50 スイートで失敗なし。
  xtask check・fmt・syntax の clippy（警告なし）通過。
- 実コーパス全件比較は下記の通り。

## 実コーパス全件比較

- base（統合先端 `f7ce9a4d`）と head（未コミット変更）の両バイナリで
  実世界コーパス 4,155ファイル×4条件を比較した。バイナリの SHA-256
  は相異なることを確認済み（base `e01c12a7…`、head `7f2015e1…`）。
- 両 TSV はバイト同一（SHA-256 `6f4ca108…`、16,620行、欠落0行）。
  成功／失敗・生成コード・Map中核・正規化済みMap全体すべて完全一致で、
  差分0件。内訳は ok 16,508行／error 112行で両側同一。
  すなわち目標なしコンパイルはバイト同一である。
