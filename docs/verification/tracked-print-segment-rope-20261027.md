# tracked 印字のセグメント保持の連結ノード化（2026-10-27）

## 背景

tracked 印字（`output_with_positions`）では、Fragment が排出範囲の
セグメント列を Vec で持ち、合成のたびに相手側のセグメントを全件
コピーしていた。過去の計数ではセグメント移動が 50,410 回／10,725 件
で、セグメント機構が tracked 印字の超過のうち約 0.41ms を占めていた。
1 件目のインライン化は Fragment の肥大化で悪化し、append の移動化は
自セグメント空の場合に限られていたため、残りは構造変更でないと
削れない段階にあった。

## 変更内容（`crates/storm-lua-syntax/src/print.rs` のみ）

- Fragment の `segments: Vec<EmissionSegment>` をやめ、`root: Option<usize>`
  （rope の根）だけを持つ形にした。公開 API のシグネチャは不変で、
  Fragment のサイズは縮小した。
- rope ノードは次の 3 種である。
  - 葉：1 件の排出範囲（arena index と fragment 相対 offset）。
  - 連結：append の左右部分木と、右部分木のシフト量（append 時の
    左側テキスト長）。
  - シフト：セグメントなし接頭辞への append で、部分木全体を
    接頭辞長だけシフトするもの。
- 全体記録（`record`）の葉は offset 0 の葉で、既存範囲がある場合も
  従来の Vec 追加と同じ順序（既存範囲の後）になるよう連結する。
  全体記録は部分の全体を覆うため、offset 0 が正しい。
- rope ノードは print ごとに開始時・終了時にクリアする thread-local
  の再利用 arena に置く。Fragment は print の外に出ないこと、print が
  print を入れ子にしないことから、境界でのクリアで十分である。
  途中で放棄された print のノードも次回 print の開始時クリアで消える。
- 最終印字は rope を反復（スタック）走査して絶対範囲を確定する。
  再帰は使わないため、append 連鎖の深さに依存しない。
- 無追跡印字は fragment の根が常に空のため rope ノードを作らず、
  セグメントに関する確保は従来通りゼロのままである。

## 同一性

- 合成入力 4 件（dense-normal・dense-lexical・draw・scale-80）で、
  新旧バイナリの生成コード全文と全排出物（node・site・token・
  start・end・理由の code／before／after／basis）の SHA-256 が
  完全一致した。
- 対象 3 クレート（syntax 112 件・minify 544 件・build 126 件）と
  ワークスペース全件（50 スイート）が成功した。`cargo xtask check`
  通過、fmt クリーン、syntax lib の clippy 警告 0。

## 測定結果（計測専用マシン 開発ホスト3、旧・新バイナリ交互・順序回転・31 反復中央値）

- dense-normal：旧約 1.77〜1.96ms に対し新約 1.39〜1.43ms
  （8 回すべて新が速く、中央値で約 −23%）。
- dense-lexical：旧約 1.78〜1.80ms に対し新約 1.38〜1.40ms（約 −22%）。
- scale-80：旧約 1.04〜1.23ms に対し新約 0.69〜0.71ms（約 −33%）。
- draw：旧約 0.42〜0.43ms に対し新約 0.19ms（約 −55%）。
- 無追跡印字は全入力・全測定で新旧不変（dense-normal 約 0.75ms、
  dense-lexical 約 0.76〜0.83ms、draw 約 0.08ms、scale-80 約 0.37ms）。
- 測定時の .us 負荷は低水準（loadavg 0.06〜0.32）だった。
  絶対値は .us 固有のもので、過去の他マシン系列とは比較しない。

## コーパス検証（開発ホスト2、読み取り専用コーパス 4,155 件）

- 分岐元と head（コミット前の変更）で 4,155 ファイル×4 条件
  ＝16,620 行を比較し、欠落 0 行で両 TSV はバイト同一だった。
  成功／失敗・生成コード・Map 中核・正規化済み Map 全体に
  差分はなかった。

## 備考

- rope arena の容量はスレッドごとに保持される（print ごとに
  クリアするが容量は再利用する）。1 print あたりのノード数は
  排出件数＋append 回数程度である。
- 不採用とした代替案：セグメント列のチャンク化は、チャンク数自体が
  排出件数程度まで増えるため移動回数を減らせない。Fragment 内に
  rope arena を持たせる案は、Fragment の生成・合成が Printer を
  経由しないため、print.rs 全体の署名変更を要し採用しなかった。
