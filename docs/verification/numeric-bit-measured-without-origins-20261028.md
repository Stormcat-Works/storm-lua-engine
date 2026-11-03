# numeric-bit パスのサイズ試算由来除去による追跡コスト削減（2026-10-28、未公開）

## 焦点

numeric-boolean-bit-specialization のパス単独追跡差（合成入力
dense-normal、directpass 11 反復中央値で 2,786 回）の内訳。変更のない
入力（saved=0・ノード数不変）でも追跡差が付いていた。

## 切り分け（合成入力 dense-normal、directpass 11 反復中央値）

numeric-bit の追跡差 2,786 回に対する一時変種（診断後に worktree 外の
退避ファイルからの cp 復元で元に戻し、git の破壊的操作は使っていない）:

- `measured()` を 0 を返す変種に置き換えると追跡差は 2,786→1 回になる。
  すなわち追跡差のほぼ全額は `measured()` 内の `scope_rename_fast`
  （追跡付きアリーナ上での改名由来構築）である。pass 本体の書き換え
  （resolve・collect・validate・候補構築・Rewriter）は約 1 回分のみ。
- 改名の値は由来の有無に依存せず、`measured()` の改名後 AST はサイズ
  試算後に破棄される disposable 試算である。候補選択は構文と束縛だけに
  依存する（api_aliases の候補選択と同じパターン）。

## 採用した変更（記録内容は同一）

- `crates/storm-lua-minify/src/passes/boolean_numerics.rs`:
  `measured()` で `inherit_ast`＋`clone` の代わりに
  `ast_utils::clone_without_origins` を使う。改名結果の値・サイズは
  由来の有無で変わらないため、候補選択と最終出力は同一。
- 回帰テスト `measured_size_matches_untracked_rename` を追加（追跡付き
  改名と由来なし改名のノード値・改名後サイズ・`measured` 値の一致を
  検証。由来なし AST は監査プリンタで印字できないためノード値の直接
  比較とした。`Node` に `Debug` がないため `assert!(a == b)` を使う）。

## 測定（合成入力 dense-normal、directpass 11 反復中央値・trackcost single 5 反復中央値）

- directpass numeric-bit: 無追跡 5,390 回→追跡 5,391 回（差 +2,786 回→
  +1 回）。saved=0・コード長・ノード数は同一。
- trackcost single: numeric-bit 単独オン差 +2,786 回→+1 回、他パス・
  all-off 差（18,327 回）は同一。全コンパイル追跡差 74,526 回→
  68,956 回（−5,570 回、約 −7.5%。単独オン削減の約 2 倍で、full
  パイプラインでは同パスが複数回走るためとみられるが、走査回数の
  厳密な内訳までは主張しない）。無追跡側は 1,844,633 回→1,844,631 回
  で実質不変。時間差は主張しない。

## 検証

- 対象 3 クレート（syntax 113・minify 581＝新規 1 件含む・build 126）
  全成功、ワークスペース全件 50 スイート成功、xtask check・fmt・
  clippy（lib 対象、警告 0）通過。--all-targets の clippy 警告 4 件は
  minify テストコードの既存警告（本変更のファイル外）。
- 実コーパス全件比較（base 163f635dd と head、開発ホスト2 の
  一時領域を使用、用後削除済み、Rust 1.97.1 でビルド。送付時は
  corpuscheck-base／corpuscheck-head の別名で送り、.ud 上で SHA-256
  を照合して確定。検証バイナリの SHA-256 相違も確認：base
  e10e3d36…、head 4e20f35d…）: 4,155 ファイル×4条件、欠落 0。両 TSV
  はバイト同一（SHA-256 11741082…、16,620 行）。全条件
  identical=4,155、hard・core・expl・missing すべて 0。成功 4,127／
  失敗 28 は両側同一。すなわち本変更はコーパス範囲の出力に一切影響
  しない。
