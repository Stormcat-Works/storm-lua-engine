# NodeOrigin の contexts／dispositions のインライン小リスト化による追跡コスト削減（2026-10-29、未公開）

## 概要

compile の追跡コストのうち、表現レベルの由来スロット共有設計
（ユーザー承認事項4）の第一歩として、NodeOrigin のノード単位の
由来リスト2種をインライン化した。従来 `contexts` は
`Arc<Vec<InlineContext>>`、`dispositions` は
`Arc<Vec<SourceDisposition>>` で、空の共有 Vec への初回書込みが
Arc ボックス＋バッファの 2 確保を払っていた。ほとんどのノードは
0〜1 件しか持たないため、両者を `SmallVec<[T; 1]>` 化し、1 件までは
スロット内で 0 確保、2 件以上は従来の Vec 同様ヒープへ spill する
形に変更した（実装は syntax クレートの provenance.rs・node_arena.rs
のみ＋回帰テスト 1 件）。シリアライズ形は JSON 配列のままで同一
であり、記録される由来・理由・生成コードは変わらない。

## 等価性の根拠

- SmallVec は配列としてシリアライズ／デシリアライズされるため、
  記録形は従来の Vec と同一。PartialEq は要素比較で同一。
- 呼出側の変更は `Arc::make_mut` の除去のみで、書込み内容と順序は
  同一（contexts は先頭への insert のまま、dispositions は push の
  まま）。import() の source-id 書換えは所有コピーに対する in-place
  書換えで、Arc 共有の有無に依存しない。
- スロット自体の Arc 共有（OriginSlots のページ COW）は不変。
  SmallVec の spill は所有バッファへの複写で他スロットとバッファを
  共有しないため、従来の `Arc::make_mut` による分離と等価。
- 回帰テスト `node_origin_small_lists_keep_array_form_and_order` が、
  1 件・2 件の配列形と NodeOrigin 全体の round-trip を検証する。

## 測定（開発ホスト1、directpass・trackcost、11 反復・中央値）

- literal-call-folding（inline.lua、folded=500）：追跡差
  4,634 回→2,634 回（約 -43.2%）。削減 2,000 回は contexts 1,000 回
  と dispositions 1,000 回の初回書込み分と一致する。
  inline-50.lua は 472 回→272 回、inline-1.lua は 19 回→15 回、
  common.lua は 3 回→3 回（折り畳み 0 件のため不変）。無追跡側の
  確保回数・バイト数、saved・コード長・ノード数は変更前後で完全同一。
- 全コンパイル（trackcost、dense-normal.lua、5 反復）：追跡差
  53,322 回→52,365 回（約 -957 回）。無追跡側は完全同一。
- 他腕（directpass の stash 前後比較）：api-alias-optimization、
  scope-renaming、temporary-global-packing、
  redundant-parentheses-elimination の確保回数に実質変化なし
  （temporary-global-packing の common.lua のみ差 +6 回の誤差範囲）。
- バイト数のトレードオフ：inline.lua の追跡バイトは 22,517,026 B
  から 22,221,026 B へ -296,000 B だが、dense-normal 全コンパイルの
  追跡バイト差は 31,895,734 B から 35,565,478 B へ +3,669,744 B。
  原因は NodeOrigin 構造体の肥大化（インライン領域分だけ全スロットの
  Arc ボックスが大きくなる）で、由来リストが空のノードにも及ぶ。
  件数を主指標とする本軸では改善だが、バイト数では後退するため
  ここに記録する。時間改善は主張しない。

## 検証

- 対象 3 クレート（syntax 127＝新規 1 件含む・minify 706・build 126）
  全成功。ワークスペース全件 50 スイート・1,213 件全成功。
- `cargo xtask check`・fmt・clippy（syntax・minify lib、警告 0）通過。
  `--all-targets` の警告 8 件は search.rs テストコードの既存分のみ
  （変更ファイル外）。
- コーパス検証は Tier FULL。分岐元 bf9744c 対 head、実行前に
  SHA-256 manifest 照合 4,155/4,155 OK。4,155 ファイル×4条件、
  欠落 0。両 TSV はバイト同一（SHA-256 304e2da9…）。
  全条件 identical=4,155、hard・core・expl・missing すべて 0。
  成功 4,127／失敗 28 は両側同一。検証バイナリの SHA-256 相違も
  確認（base 6d8f310b…、head a99d6338…）。
