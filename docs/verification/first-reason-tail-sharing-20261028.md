# 初回理由テールの共有による追跡コスト削減の検証（2026-10-28）

## 変更内容

compile の追跡コストのうち、書き換え理由の記録に残っていた
テールノードの毎ノード確保を除去した。記録内容は変わらない。

- `crates/storm-lua-syntax/src/deferred_origin.rs`
  - `StoredOrigin::push_reason` で、新しいテールを作る場合に
    previous が空（初回理由）のときだけ、理由をキーとする共有
    テールを使うようにした。previous が空のテールノードは理由
    だけで決まるため、同一理由の初回理由は全 origin で 1 個の
    ノードを共有できる。共有テーブルは 4096 件で打ち切り、
    超過時は従来の fresh 確保へ fallback する。
  - previous があるテールは従来通り fresh に作るため、既存の
    理由履歴が混ざることはない。共有ノードへの後続 push は
    `Arc::make_mut` による copy-on-write で、共有者が互いの編集を
    観測することはない。観測・シリアライズは内容で展開するため、
    記録される理由列は fresh ノードと同一である。
  - 回帰テスト `identical_first_rewrite_reasons_share_one_tail_node`
    を追加し、同一初回理由のテールがポインタ一致すること、
    2 件目の異なる理由が共有ノードの後に連なること、共有元の
    理由列が不変であることを検証した。
  - 公開 API の既存シグネチャは不変。

## 切り分けの経緯

api-alias 転送の追跡差 1,771 回を診断変種で分解したところ、
置換ノードの slow path が 1,440 回（640 ノードで 2.25 回／件）、
複製分は約 0 回、固定費が 331 回だった。置換 slow path の内訳は、
同種置換 160 件が 3 回／件、異種置換 480 件が 2 回／件で、
スロット Arc・理由テール・名前 Vec 複写の組み合わせと一致する。
このうち理由テール 1 回／件が全置換ノードで共有可能だった。
なお、診断で一度試した中間 Arc の除去（`unwrap_or_clone` の
置換）は `unwrap_or_clone` が値を返すだけで Arc を確保しないため
無効で、 revert した。

## 測定（合成入力 dense-normal、VM、中央値）

- directpass（11 反復）：api-alias-optimization の追跡差
  5,167 回 → 4,526 回（−641 回）。constant-folding は
  3,218 回 → 2,738 回（−480 回、変更ノード 480 件分）。
  root-local-globalization は 1,292 回 → 972 回（−320 回、
  変換文 320 件分）。scope-renaming は 2,142 回で不変
  （改名は既存の共有テール経路のため）。
  無追跡側・saved・コード長・ノード数はすべて同一。
- trackcost single（5 反復）：全コンパイル（default）追跡差
  79,982 回 → 77,897 回（−2,085 回）。api-alias 単独オン
  14,834 回 → 14,193 回（−641 回）、constant-folding 単独オン
  6,825 回 → 6,345 回（−480 回）、root-local 単独オン
  4,418 回 → 4,098 回（−320 回）、scope-renaming 単独オン
  9,822 回・all-off 差 18,111 回は不変。時間差は主張しない。

## 検証

- 対象 3 クレート（syntax 120＝新規 1 件含む・minify 617・
  build 126）全成功、ワークスペース全件 50 スイート成功、
  `cargo xtask check`・`cargo fmt --check` 通過、対象 3 クレートの
  clippy（lib）警告 0。
- 実世界コーパス 4,155 ファイル × 4 条件：base `c63dccbe` と head
  の TSV はバイト同一（SHA-256
  `a7a36926032194106e07e068ab80ee5d51262659f61bc766a59e6415dd0acfce`、
  16,620 行）。全条件 identical=4,155、hard・core・expl・
  missing すべて 0。成功 4,127／失敗 28 は両側同一。
  すなわち本変更はコーパス範囲の出力に一切影響しない。
  TSV は `workers/w1/corpus-verify-first-tail-20261028/` に保存。
