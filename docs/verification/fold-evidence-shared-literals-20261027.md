# constant-folding 証拠理由のリテラル・サイズ共有化による追跡コスト削減（2026-10-27、未公開）

## 焦点

constant-folding の fold 証拠理由の構築コスト。Un／Bin 証拠サイトでは、
証拠ノードごとに呼出側が演算子・リテラル事実・評価結果・生成値・
式サイズを文字列整形していた。`decision` 系の intern キャッシュは
ヒット時でも呼出側の確保を帳消しにできないため、この整形分が
追跡差として残っていた。

## 切り分け（合成入力 dense-normal、directpass 11 反復中央値）

fold_expressions の追跡差 9,357 回の内訳を一時変種で切り分けた
（すべて revert 済み）。derive 遅延経路 4,800 回、explain の記録
480 回、残り 4,077 回が証拠構築・決定内部・走査残差である。
derive 遅延経路は本サイクルの対象外とし、証拠構築側を実装した。

## 採用した変更（記録内容は同一）

- `crates/storm-lua-syntax/src/explanation.rs`: `DecisionFactValue`
  に `Borrowed(&str)` を追加した。借用値は Owned と同じ intern
  キャッシュ経由で共有され、ヒット時は確保なし、未ヒット時は
  intern 済み `Arc<str>` 1 個だけが確保される。呼出側が先に
  `String` を作る必要がなくなる。回帰テスト
  `decision_shared_matches_decision` に Borrowed の等価性を追加した。
- `crates/storm-lua-minify/src/passes/literal_folding.rs`:
  `FoldSharedFacts` にファクトキャッシュを追加した。リテラル事実は
  値そのものをキーにする（整数は値、浮動小数点数はビットパターン、
  文字列はバイト長、真偽値は値、nil は種別）ため、等しいキーからは
  等しい文字列が返る。式サイズは数値キーで共有する。初見でのみ
  整形し、以後は共有 `Arc<str>` を返す。`nonliteral` の生成値事実は
  プロセス全体で 1 個の静的共有値にした。
- Un／Bin 証拠サイトを、演算子は `Borrowed`、リテラル・サイズ・
  生成値は `Shared` に変えた。証拠の構築は従来どおり追跡時のみ
  行われる（gate の位置は Bin では候補選択の前後で整理したが、
  証拠が付く条件と内容は変わらない）。回帰テスト
  `shared_facts_match_literal_fact_and_sizes` を追加した
  （共有ファクト文字列が `literal_fact` と一致することと、
  繰返しが同一 `Arc` を返すこと）。

## 測定結果（合成入力 dense-normal、11 反復中央値・trackcost single 5 反復中央値）

- directpass constant-folding-expr: 無追跡 29,096 回は不変。
  追跡 38,453 回から 35,673 回へ（追跡差 9,357 回から 6,577 回へ、
  −2,780 回、約 −30%）。コード長 41,175・ノード数 6,883→7,363・
  changed 480 件は同一。
- directpass constant-folding 全体: 無追跡 39,839 回は不変。
  追跡 50,157 回から 47,377 回へ（追跡差 10,318 回から 7,538 回へ、
  −2,780 回）。
- trackcost single: 全コンパイル追跡差 113,836 回から 111,056 回へ
  （−2,780 回）。constant-folding 単独オン差 15,293 回から
  12,513 回へ（−2,780 回）。all-off 差（27,290 回）・他パス・
  無追跡側（1,855,853 回）はすべて同一で、削減が folding 単独と
  完全一致する。時間差は主張しない。

## 検証

- 対象 3 クレート（syntax 113・minify 552＝新規 1 件含む・build 126）
  全成功。ワークスペース全件 50 スイート・1,045 件全成功。
  xtask check・fmt・clippy（lib 対象、警告 0）通過。
- 実コーパス比較（分岐元 8e8536ef5 と head、開発ホスト2 の
  一時領域の作業環境で、コーパスは読み取り専用でコピーなし、
  Rust 1.97.1 で両バイナリを VM ビルドして .ud へ送付し SHA-256 を
  照合。検証バイナリの SHA-256: base d60c6414…、head 56a5077a…）:
  4,155 ファイル×4条件、欠落 0。両 TSV はバイト同一。
  全条件 identical=4,155、hard・core・expl・missing すべて 0。
  成功 4,127／失敗 28 は両側同一。すなわち本変更はコーパス範囲の
  出力に一切影響しない。TSV は作業記録
  corpus-verify-fold-facts-20261027/ に保存。開発ホスト2 の
  一時ファイルは用後削除済み。ベース用 worktree は用後削除済み。
