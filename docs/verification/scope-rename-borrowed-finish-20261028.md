# scope-renaming の改名 finish におけるノード値 clone 除去の検証（2026-10-28）

## 変更内容

compile の追跡コストのうち、scope-renaming の改名 finish に残っていた
ノード値の clone を除去した。記録内容は変わらない。

- `crates/storm-lua-syntax/src/node_arena.rs`
  - `BorrowedOrigin`（sources・origin の Arc のみ）と
    `capture_origin_ref`、`finish_rename_changed` を追加した。
  - 従来の `finish_rename_inner` では、snapshot の node 値は changed
    判定以外に使われず（import 後の node は破棄される）。改名では綴りが
    変わることが構成上保証されるため、changed 前提の借用 finish が
    等価である。同一 sources なら NodeOrigin 構造体をクローンし、
    それ以外は既存の `import` による再マップを使う。body・names の
    derive は共有 tail 対応の既存経路と同一で、スロット格納も同一。
  - 公開 API の既存シグネチャは不変。
- `crates/storm-lua-minify/src/scope_rename.rs`
  - `apply_renames` の single・multi 両サイトで、capture_origin＋
    finish_rename_with_shared／with_code を capture_origin_ref＋
    finish_rename_changed に置換した。shared_tails は as_ref で渡す。
    single は綴りが変わること、multi は少なくとも 1 シンボルが変わる
    ことが構成上保証されるため、changed 前提を満たす。

## 回帰テスト

- `node_arena::cow_tests::finish_rename_changed_matches_finish_rename`
  （Local multi ノード＋Name ノードで、新旧両経路の由来・名前由来・
  シリアライズ済み AST・印字コードの一致を検証）

## 切り分けの根拠（合成入力 dense-normal、VM、中央値）

- api-alias の追跡差 5,809 回は、scope_rename_fast を外す変種で
  転送 1,771 回＋転送後改名 4,038 回に分解できた。
- 改名ノード数は単独 scope-renaming が 961 件（single 640・multi 321）
  に対し api-alias 内は 1,605 件（single 1,284・multi 321）で、差分
  644 件はエイリアス化で局所化した利用箇所（元は固定グローバルで
  改名対象外）が改名対象になった分である。per-node コストは単独約
  2.90 回・api-alias 内約 2.52 回で、api-alias 内の改名コスト増は
  ノード数の増によるもので単価の悪化ではない。
- scope-renaming の追跡差 2,784 回は、finish 抜きの変種で base
  862 回＋finish 1,922 回（改名ノードあたり 2 回＝スロット Arc ボックス
  1 回＋names Vec バッファ複写 1 回）に分解できた。finish 分は現行
  表現のほぼ下限で、base の主因が multi ノード 321 件の capture
  によるシンボル／値ベクタ複写であることが本変更の対象である。

## 測定（合成入力 dense-normal、VM、中央値）

- directpass（11 反復）：
  - scope-renaming：無追跡 5,498 回で不変、追跡 8,282 回→7,640 回
    （追跡差 2,784 回→2,142 回で −642 回、約 −23%。multi 321 件×
    2 ベクタ分と一致）。saved・コード長・ノード数は同一。
  - api-alias-optimization：無追跡 115,531 回で不変、追跡
    121,340 回→120,698 回（追跡差 5,809 回→5,167 回で −642 回。
    multi 件数が同一のため同量）。saved・コード長・ノード数は同一。
  - root-local-globalization・constant-folding は不変。
- trackcost single（5 反復）：
  - 全コンパイル追跡差 81,282 回→79,982 回（−1,300 回）。
  - scope-renaming 単独オン 10,464 回→9,822 回（−642 回）、
    api-alias 単独オン 15,476 回→14,834 回（−642 回）。
  - all-off 差 18,111 回は不変。その他パスの単独オン差はすべて同一。
- 時間差は主張しない。

## 実世界コーパス検証

- base（統合先端 `4ccec29c`）と head の 4,155 ファイル × 4 条件比較：
  両 TSV はバイト同一（SHA-256
  `a7a36926032194106e07e068ab80ee5d51262659f61bc766a59e6415dd0acfce`、
  16,620 行）。全条件 identical=4,155、hard・core・expl・missing
  すべて 0。成功 4,127／失敗 28 は両側同一。
  すなわち本変更はコーパス範囲の出力に一切影響しない。
- 検証バイナリの SHA-256 相違も確認（base `4512bd05…`、head
  `42d8456a…`）。開発ホスト2 の一時ファイルは用後削除済み。
  ベース用 worktree も用後削除済み。
