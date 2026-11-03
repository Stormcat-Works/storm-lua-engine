# screen-loop 合成の試行時由来付与の繰延べ化による追跡コスト削減（2026-10-30）

## 焦点
compile の追跡コストのうち、ordered-screen-loop-synthesis パスが
出力に反映されない失敗試行のたびに支払っていた由来付与コスト。
w1 の第65サイクルで、開発ホスト1 上で作業した。分岐元は統合先端
`db920cf9`（第158ラウンド）。

## 棚卸し
trackcost single（5 反復・中央値、開発ホスト1）では、draw 入力の
全コンパイル追跡差 637,342 回・all-off 差 2,605 回に対し、単独オン差は
ordered-screen-loop-synthesis +102,532 回、
ordered-draw-record-packing +6,504 回だった。directpass（11 反復・中央値）
でも同パスの単独オン差は 102,532 回で trackcost と完全一致し、探索増幅
ではなくパス単独の追跡コストと確定した。ノード数・コード長は追跡の
有無で変化せず、この直接入力ではループ合成は 0 件なのにコストが発生
しており、失敗試行のコストであることが分かる。

## 切り分け
- `derive_samples` を no-op 化する診断変種では追跡差が 102,532 回→
  3,152 回となり、derive／relate が約 99,380 回（約 97%）を占めると確定した。
- 試行ごとに `Vec` へ由来値を集めていた箇所を plan 内の単一 values
  arena へ集約するとバイト数は −309,872 B だったが、確保回数は不変
  だったため、残りは試行ごとの plan バッファ確保と診断し、plan の再利用へ進んだ。
- なお api-alias の内訳については、第64サイクルの数値が古いバイナリ由来の
  stale 値だったことが判明した（現行先端での再測定は 3,617 回、末尾改名分
  1,607 回は第64サイクルと一致）。transfer ブロック 2,010 回は記録内容
  そのもの＋具体化に伴う改名相互作用で、クリーンなレバーなしと結論した。

## 採用した変更（ローカルコミットのみ。push・PR なし）
- `crates/storm-lua-minify/src/passes/screen_loops.rs` のみ。
  `build_candidate` が共有 target arena に試行ノードを push しつつ由来を
  その場で derive していたのをやめ、由来呼び出しを `TrialOrigins`
  （op 列＋単一 values arena）に記録し、試行が採用された場合のみ
  `plan.apply(target, source)` で同じ呼び出し・同じ順序・同じ source で
  適用する。失敗試行は truncate され、plan は破棄される。
- op 種別は `Derive { node, start, len, reason }`（values は plan 内の
  単一バッファへの範囲参照）、`Synthetic { node, transformation }`、
  `Reset { node }`（derive_samples のフォールバック＝値の書き戻しと等価）。
  旧 `derive_samples` 関数は削除し、全呼出箇所を `plan.derive` に置換した。
  `mark_synthetic` の試行経路呼出はすべて `plan.synthetic` に置換した。
- plan を通した関数：`fallback_template`、`translated_template`（再帰呼出含む）、
  `periodic_affine_numbers`、`periodic_affine_expression`、`affine`、
  `finish_affine`、`periodic_affine`、`build_candidate`。`transform_node` の
  試行ループで plan を生成し、採用時に apply する。`transform_node` 末尾の
  block rewrite、strings checkpoint／rollback、シンボルカーソルには触れていない。
- plan は `transform_node` の文ループ外で 1 個だけ生成し、試行ごとに
  `plan.clear()`（`ops.clear()`＋`values.clear()`、容量は保持）で再利用する。
  採用された試行の由来記録自体は同一呼び出し・同一順序のため同一になる設計で、
  失敗試行の derive が source テーブルの interning 順序に影響し得るため、
  Tier FULL で Map バイト列まで含めて確認した。
- 回帰テスト 1 件を追加（`provenance_zero_tests`）：
  非 affine・非 periodic の先頭呼出列（失敗試行）と affine の後続呼出列
  （合成成功）を隣接させ、追跡コード＝無追跡コード、validate 通過、
  unknown 0、合成発火、先頭呼出の逐語残存を主張する。

## 測定（合成入力、開発ホスト1、中央値。時間差は主張しない）
- draw の ordered-screen-loop-synthesis（directpass、11 反復）：
  追跡差 102,532 回→**3,176 回（−96.9%）**。無追跡 123,041 回、
  コード長 4187、ノード数 1457→1457 は変更前後で同一。
- dense-normal の同パスは差 2 回で不変（発火なし）。
- 残る約 3,176 回は診断変種 D-E の水準（3,152 回）とほぼ一致し、
  採用時由来・スロット追加・block rewrite 等の残存分である。

## テスト
- 対象 3 クレート：syntax 134 件・minify 784 件（新規テスト含む）・
  build 127 件、すべて成功。
- ワークスペース全件：全スイート成功（失敗 0）。
- `cargo xtask check` 通過、`cargo fmt --check` 通過、
  `cargo clippy -p storm-lua-minify --lib` および
  `cargo clippy -p storm-lua-syntax --lib` は警告 0。

## コーパス検証（Tier FULL、開発ホスト1）
- manifest 照合：正本ハッシュ `abfec87a…` の manifest を送り、
  コーパスのローカルミラー で `sha256sum -c` を全件 OK（4,155/4,155）まで確認。
- base は分岐元 `db920cf9`、head は本ブランチ。検証バイナリの SHA-256 は
  base `cb91727f…`・head `ea2eb0ca…` で相違を確認（別ビルドであることの証拠）。
- 比較：4,155 件×4 条件＝16,620 行。4 条件すべてで identical=4,155、
  expl-only=0、map-core=0、hard=0、missing=0。成功／失敗ペアは
  (ok, ok) 4,128・(error, error) 27 で両側一致。すなわち生成コード・
  正規化済み Source Map（中核含む）・成功／失敗がすべてバイト同一で、
  意味差分・中核差分・説明系差分は 0。
- 開発ホスト3 での時間測定は実施しない。本件は追跡コスト削減で、
  決定的計数と Tier FULL が主証拠であり、時間改善を主張しないため
  （従来の追跡コストラウンドと同様）。

## 備考
- 本変更は `screen_loops.rs` に触れるため、休眠中の w7 の再開条件
  （search／orchestration／screen_loops／draw_records に実質的に触れる
  マージ）に該当し得る。CLAIMS.md に記録済みで、重なりの扱いは親の判断に委ねる。
- 次候補：(1) draw-record-packing の追跡差 7,120 回の内訳（directpass で
  測定済み）、(2) 他代表入力での再棚卸しの継続。
