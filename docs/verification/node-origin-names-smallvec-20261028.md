# NodeOrigin.names の SmallVec 化による追跡コスト削減の検証（2026-10-28）

## 変更内容

compile の追跡コストのうち、改名 finish に残っていた names Vec
バッファの毎ノード確保を除去した。記録内容は変わらない。

- `crates/storm-lua-syntax/src/provenance.rs`
  - `NodeOrigin.names` を `Vec<(NameSite, StoredOrigin)>` から
    `smallvec::SmallVec<[(NameSite, StoredOrigin); 2]>`
    （型エイリアス `NameOrigins`）に変更した。単一名・2名束縛
    まではインラインで、3名以上は従来どおりヒープへ spill する。
  - ノード間でリストを共有しない。改名では origin 複製後に
    names をその場で書き換えるため、共有すると copy-on-write の
    複写が毎ノード発生してしまう。インライン化はその複写を
    発生させずにバッファ確保だけを消す。
  - シリアライズ形は JSON 配列のままで、既存テスト
    `names_serialize_as_a_json_array_after_the_vec_representation_change`
    が通る。
  - 回帰テスト
    `name_origins_keep_inline_and_spilled_entries_equal_through_clone`
    を追加し、2件インライン・3件目 spill・clone 等価・JSON 配列
    往復を検証した。
- `crates/storm-lua-syntax/src/node_arena.rs`
  - `SmallVec::retain` は `&mut` を取るため（`Vec::retain` は `&`）、
    `same_name` 関数ポインタ渡し 1 箇所だけクロージャ化した。
    他の retain／push／sort／iter はそのまま通る。
- 依存宣言（AGENTS.md の全依存宣言管理の規則どおり）
  - workspace の `Cargo.toml` に `smallvec = { version = "1",
    features = ["serde"] }` を追加し、syntax クレートは
    workspace 経由で参照する。
  - `tools/architecture.json` の storm-lua-syntax 外部依存へ
    `smallvec` を追加した（`cargo xtask check` が通る）。
  - `Cargo.lock` の差分は smallvec への serde 依存追加と syntax
    への smallvec 追加のみで、smallvec の版は 1.16.1 のまま。
  - 公開 API の既存シグネチャは不変。

## 切り分けの経緯

第19サイクルの分解では scope-renaming の finish 分が改名ノード
あたり 2 回（スロット Arc ボックス 1 回＋names Vec バッファ複写
1 回）で、finish 抜きの base は約 220 回まで枯れていた。
改名ノードの names は単一名か少数の束縛に限られるため、
インライン容量付きの列に変えることでバッファ分 1 回／改名
ノードを消せる。スロット Arc は各ノード固有のため残る。

## 測定（合成入力 dense-normal、VM、中央値）

- directpass（11 反復）：scope-renaming の追跡差が 2,142 回から
  1,181 回へ（−961 回＝改名 961 ノード分と一致）、
  root-local-globalization が 972 回から 652 回へ（−320 回＝
  名前 320 件分と一致）、api-alias-optimization が 4,526 回から
  3,405 回へ（−1,121 回）、constant-folding は 2,738 回で不変
  （変更ノードの names は空のため想定内）。無追跡側・saved・
  コード長・ノード数はすべて同一。時間差は主張しない。
- trackcost single（5 反復）：全コンパイルの追跡差が 77,897 回
  から 66,303 回へ（−11,594 回、約 −15%）。単独オン差は
  api-alias 14,193 回から 13,072 回へ（−1,121 回）、
  scope-renaming 9,822 回から 8,861 回へ（−961 回）、
  root-local 4,098 回から 3,458 回へ（−640 回、directpass の
  −320 回の 2 倍でパイプライン内の 2 回適用分とみられる）、
  constant-folding は 6,345 回で不変。all-off 差も 18,111 回から
  15,869 回へ（−2,242 回。パース時の名前シードで初回 push の
  バッファ確保が消えた分とみられる）。単独オン削減の合計
  （2,722 回）を上回る全コンパイル削減は、候補評価での origin
  複製が多数回走るためにインライン化の恩恵が複製回数分だけ
  積み上がったものとみられる（走査回数の厳密な内訳までは
  主張しない）。
- 注意（トレードオフ）：確保バイト数は増加する。NodeOrigin
  構造体自体がインライン分だけ大きくなり、Arc<NodeOrigin> の
  確保 1 件あたりのバイトが増えるため（directpass の
  scope-renaming 追跡バイト 1,395,005 B から 1,518,013 B へ、
  api-alias 31,364,320 B から 31,809,032 B へ）。本軸の主指標
  である確保回数は減るが、バイト数では悪化するため両方を
  記録に残す。時間改善は主張しない。

## 検証

- 対象 3 クレート（syntax 121＝新規 1 件含む・minify 617・
  build 126）全成功、ワークスペース全件 50 スイート・1,118 件
  全成功。`cargo xtask check`・`cargo fmt --check`・clippy
  （対象 3 クレート、警告 0）通過。
- 実世界コーパス 4,155 ファイル × 4 条件（開発ホスト2 の
  一時領域を使用、用後削除済み、Rust 1.97.1 で両バイナリを
  VM ビルドして .ud へ送付。送付時は corpuscheck-base／
  corpuscheck-head の別名で送り、.ud 上で SHA-256 を照合して
  確定した。検証バイナリの SHA-256 相違も確認：base
  f17e8cbc…、head 0f02ddaa…）：両 TSV はバイト同一
  （SHA-256 a7a36926…、16,620 行）。全条件 identical=4,155、
  hard・core・expl・missing すべて 0。成功 4,127／失敗 28 は
  両側同一。すなわち本変更はコーパス範囲の出力に一切影響しない。
  TSV は workers/w1/corpus-verify-names-smallvec-20261028/ に保存。
