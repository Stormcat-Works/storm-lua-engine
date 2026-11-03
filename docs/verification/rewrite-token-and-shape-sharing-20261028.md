# rewrite の token・形状共有化による追跡コスト削減の検証（2026-10-28）

## 変更内容

compile の追跡コストのうち、constant-folding の追跡差に対する 2 件の
共有化。いずれも記録内容は文字列内容で同一のため変わらない。

1. token 全落ち時の共有空ベクタ再利用
   （`crates/storm-lua-syntax/src/node_arena.rs`）
   - `finish_rewrite_with_code` では、保持される token が 1 件もない場合でも
     共有 token ベクタを複写してから retain で空にしていた。
   - 二項式をリテラルへ fold するように、演算子サイトだけを持つノードが
     リテラルへ変わる場合は token が全落ちするため、共有の空ベクタを
     そのまま使うようにした。空ベクタは空ベクタであり、直列化形も同じ。
   - `empty_tokens` を `pub(crate)` 化したのみで、公開 API は変わらない。
2. Num／Str 形状の共有キャッシュ化
   （`crates/storm-lua-syntax/src/explanation.rs`）
   - rewrite 理由の before／after 形状のうち、Num は綴りそのもの、Str は
     バイト長だけをキャッシュキーにするため、ヒット時は format 自体が
     不要で確保 0 回になる。キャッシュは decision intern と同じく
     4096 件で打ち切り、超過時は従来の fresh 確保へ fallback する。
   - 演算子・真偽などの固定形状は従来どおり静的テーブルから供給する。

## 回帰テスト

- `node_arena::cow_tests::rewrite_dropping_all_tokens_reuses_empty_token_vector`
  （Bin の token 1 件を確認してから Num へ rewrite し、token が空で
  共有 empty ベクタとポインタ一致することを検証）
- `explanation::tests::numeric_and_string_shapes_are_shared_and_exact`
  （Num は綴り、Str はバイト長での共有と内容の完全一致を検証）

## 測定（合成入力 dense-normal、VM）

- directpass constant-folding-expr（11 反復中央値）：
  無追跡 29,096 回は不変。追跡 35,673 回→34,713 回（token で −960 回）
  →33,753 回（形状でさらに −960 回）。追跡差 6,577 回→4,657 回
  （合計 −1,920 回、約 −29%）。コード長・ノード数・changed 480 件は同一。
- trackcost single（5 反復中央値）：
  全コンパイル追跡差 68,956 回→66,396 回（合計 −2,560 回、約 −3.7%）。
  constant-folding 単独オン差 9,953 回→8,033 回（−1,920 回）。
  root-local 単独オン差も token 変更で 5,284 回→4,644 回（−640 回。
  root-local の文 rewrite でも token が全落ちするため）。
  api-alias・scope-renaming・all-off 差（18,327 回）・無追跡側はすべて同一。
- 時間差は主張しない。

## 実コーパス全件比較

- base `2a11ea602`（第73ラウンド）と head を、開発ホスト2 の
  実世界コーパス（4,155 ファイル×4条件、読み取り専用でコピーなし、
  Rust 1.97.1 で両バイナリを VM ビルドして .ud へ送付し SHA-256 を照合）
  で比較した。検証バイナリの SHA-256 相違も確認
  （base `a3f7cee6…`、head `9d1123ff…`）。
- 両 TSV はバイト同一（SHA-256 `03692103…`、16,620 行）。
  全条件 identical=4,155、hard・core・expl・missing すべて 0。
  成功 4,127／失敗 28 は両側同一。すなわち本変更はコーパス範囲の
  出力に一切影響しない。
- 開発ホスト2 の一時ファイルは用後削除済み。ベース用 worktree も
  用後削除済み。

## テスト

- 対象 3 クレート（syntax 115＝新規 2 件含む・minify 586・build 126）
  全成功。ワークスペース全件 50 スイート・1,081 件全成功、失敗 0。
- xtask check・fmt・clippy（lib 対象、警告 0）を通過。
