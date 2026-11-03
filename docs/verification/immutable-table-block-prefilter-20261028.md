# immutable-table-flattening のブロック事前フィルタによる追跡コスト削減（2026-10-28、未公開）

## 焦点

compile の追跡コストのうち、immutable-table-flattening パスに残っていた、
テーブル束縛のないブロックに対する毎ブロックの複製コスト。合成入力
dense-normal ではパス自体が発火 0 件（生成コード長は all-off と同一）
なのに、パス単独の追跡差が 322 回残っていた。

## 切り分け

新先端での再棚卸しで、単独オン差の上位は
api-alias 12,352 回・scope-renaming 8,861 回・constant-folding 5,865 回・
root-local 3,458 回で、次点が immutable-table-flattening の 322 回だった
（それ以外は 12 回以下）。directpass では同パスが無追跡 9,514 回→
追跡 9,675 回（差 161 回）で、パイプライン単独オン差 322 回は
ちょうどその 2 倍（パスがパイプライン内で 2 回走るため）と一致する。

一時変種による分解（すべて診断後に revert し、最終差分には含まない）：

- 内側関数を no-op 化すると追跡差は 0 回になり、コストはすべて
  `flatten_immutable_tables_in_block` の内部にあることを確定した。
  このときブロック訪問数は 161 件だった（診断カウンタによる）。
- 複製だけを残す変種（resolve・走査・試行なし）でも追跡差は
  161 回のままだった。すなわちブロックあたり追跡側だけ 1 回の
  追加確保が `clone_table_ast`（`NodeArena` の複製）で生じており、
  その内訳は無追跡側のノード列複製 1 回に加え、追跡側だけ
  由来スロット列の複製 1 回が乗るためである。resolve・走査・試行は
  追跡差に寄与しない。
- 161 ブロック × 1 回＝161 回が directpass の差と完全一致し、
  原因がブロックあたりの複製そのものであることが確定した。

## 採用した変更（minify クレートの tables.rs のみ＋回帰テスト 2 件）

- `block_has_table_binding` を新設し、ラウンドループの先頭で、
  ブロック直下の文にテーブルリテラル束縛が存在しない場合は
  複製・resolve・走査・試行をすべて行わずに break する形に変更した。
  束縛の条件は `init` への登録条件の必要条件そのものである
  （Local は arity 一致＋Table 式あり、Assign は arity 一致＋
  Name 目標と Table 式の同一位置ペアあり）。純粋性・binding id の
  有無は `init` をさらに絞るだけのため、フィルタが真になるブロックは
  従来経路と同一に処理され、偽になるブロックは従来も空の `init` で
  break していた（AST への変更なし）。
- 回帰テスト 2 件を追加（`provenance_table_function_tests.rs`）：
  テーブル束縛のないブロック群が無変更であること、ネストした
  ブロック内の flatten 対象テーブルが従来どおり flatten されること
  （フィルタがブロック単位であることの確認）。

## 等価性の根拠（Tier INVARIANT）

スキップされるブロックは従来経路でも空の `init` で break し、
AST に一切の変更を加えなかった。フィルタ条件は `init` 登録の
必要条件と同一のため、スキップの有無で `init`・候補・試行・
コミットのいずれも変わらない。したがって生成コード・Source Map・
由来監査は構成上不変である。由来機構そのものの表現変更は行って
おらず、変更は minify クレートの tables.rs 内に留まる。

## 測定（合成入力 dense-normal、VM、中央値。時間差は主張しない）

- directpass（11 反復）：immutable-table-flattening の追跡差
  161 回→0 回。無追跡側も 9,514 回→6,252 回、追跡側も
  9,675 回→6,252 回で両側とも同一値に収束した。saved=0・
  コード長・ノード数は変更前後で同一。
- trackcost single（5 反復）：同パスの単独オン差 322 回→0 回。
  api-alias・scope-renaming・constant-folding・root-local・
  all-off 差は不変。
- trackcost default（5 反復）：全コンパイル追跡差 54,284 回→
  53,640 回（−644 回、約 −1.2%）。単独オン差の 2 倍と一致する
  （パスがパイプライン内で 2 回走るため）。無追跡側・コード長・
  候補数はすべて同一。
- directpass（11 反復）：api-alias 追跡差 2,685 回・scope-renaming
  1,181 回・root-local 652 回・constant-folding 2,258 回はすべて不変。

## 検証

- 対象 3 クレート（syntax 123・minify 639＝新規 2 件含む・build 126）
  全成功、ワークスペース全件 50 スイート・1,142 件全成功。
  `cargo xtask check`・`cargo fmt --check`・clippy（対象 3 クレート、
  警告 0）通過。
- 検証区分は Tier INVARIANT（構成上出力不変の、空 `init` ブロックの
  複製・resolve 省略）のため、全件コーパスは次回 Tier FULL の
  累積比較へ繰り延べる。INVARIANT 連続は本サイクルで 1 件目。
