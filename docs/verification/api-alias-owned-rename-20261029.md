# api-alias の勝者リネーム所有権化による追跡コスト削減（2026-10-29、未公開）

## 概要

compile の追跡コストのうち、api-alias-optimization パスの追跡差の
主因である最終段の scope リネーム（`scope_rename_fast`）の追跡
コストを削減した。勝者 AST は所有権のある状態で手元にあるため、
リネームをクローン経由ではなく所有権版（`scope_rename_fast_owned`）
で直接適用する形に変更した（実装は minify クレートの
scope_rename.rs の共有化と api_aliases.rs の呼出切替えのみ＋回帰
テスト 1 件）。記録される由来・理由・生成コードは変わらない。

## 切り分け（診断変種はすべて復元済み。directpass、11 反復・中央値）

分岐元（第128ラウンド）での api-alias-optimization 単独の追跡差は
inline.lua 1,286 回、common.lua 1,333 回、dense-normal.lua 2,685 回
だった。内訳は次のとおり。

- 転送段のみ（最終リネーム省略）：inline 155 回、common 157 回、
  dense 890 回。
- 転送段のうち attach＋定義マーキング＋ルート由来まで（歩行省略）：
  inline 131 回、common 134 回、dense 239 回。すなわち転送歩行
  自体は inline 24 回、common 23 回、dense 651 回で、置換ノードの
  由来ボックスが大半を占める現行表現のフロアである。
- 最終リネーム段（全体−転送段）：inline 1,131 回、common 1,176 回、
  dense 1,795 回で、追跡差の約 7〜9 割を占める。これが本変更の対象
  である。リネーム段の内訳は、改名ノードごとの由来スロット Arc
  ボックス（表現フロア）と、クローン由来の共有ページに対する初回
  書込みごとのページ複写（64 スロット／ページで 2 確保／ページ）
  であり、後者のみが所有権化で削れる。

## 実装と等価性の根拠

- `scope_rename.rs` の `apply_renames` を、作業 arena 自身から
  改名判定を読む `apply_renames_inner` に変更した。ノードは一度だけ
  訪問され、訪問時に書き換えるのはそのノード自身のシンボルのみで、
  子 ID は変わらないため、判定の読取元を源クローンから作業 arena
  へ変えても判定結果は同一である。従来の呼出側は
  `apply_renames`（クローンを渡す薄いラッパー）経由で挙動不変。
- `scope_rename_fast_owned` を追加した。リネーム計画は由来なし
  複製（`clone_without_origins`）から取る。計画は構文と文字列集合
  のみに依存し由来には依存しないため、計画結果は追跡あり・なしで
  同一である。計画複製は由来スロット表を持たないため、作業 arena
  の由来ページと共有されることはなく、所有権のある作業 arena の
  ページは一意なまま適用される。計画複製は適用中も生存し、その
  文字列テーブルは作業 arena の文字列テーブルと Arc を共有するが、
  適用中の intern は作業側で COW 分離されるため計画側の名前は
  変わらない。
- `api_aliases.rs` の最終リネーム呼出だけを所有権版に切り替えた。
  他の `scope_rename_fast` 呼出側は変更していない。
- 回帰テスト `owned_fast_rename_matches_borrowed_fast_rename` を
  追加した。追跡あり・なしの両入力で、所有権版と借用版の
  ルート・ノード・文字列集合・シリアライズ済み AST（由来を含む）
  が完全一致することを検証する。

## 測定（directpass、11 反復・中央値。時間改善は主張しない）

- inline.lua：追跡差 1,286 回→1,158 回（約 −10.0%）。
- common.lua：追跡差 1,333 回→1,213 回（約 −9.0%）。
- dense-normal.lua：追跡差 2,685 回→2,497 回（約 −7.0%）。
- 無追跡側の確保回数・バイト数、saved・コード長・ノード数は
  3 入力すべてで変更前後完全同一。

## 検証

- 対象クレート syntax 126・minify 706（新規 1 件含む）・build 126
  とワークスペース全件 50 スイート・1,212 件全成功。
  `cargo xtask check`・fmt・clippy（syntax・minify lib、警告 0）
  通過。`--all-targets` の clippy 警告は search.rs テストコードの
  既存分のみ（変更ファイル外）。
- 実コーパス全件比較（Tier FULL。詳細は下記「コーパス比較」節）。

## コーパス比較

- ホスト：開発ホスト1。コーパス：ローカル複製
  コーパスのローカルミラー。実行前に SHA-256 manifest 照合
  4,155/4,155 OK（manifest ハッシュ
  abfec87ab0c29dc088d4e93f25cf563849b062d3eb15eeaa5d54f8d08b8341c3）。
- base：分岐元 b11fd50（第128ラウンド）、head：本変更。
  検証バイナリ SHA-256 は base
  01fdc2419b7f469a584696f6f02d4c29f6efaa21e9ccda997e6c18c9c877ed2f、
  head 8629476b7bbd5c9d79b79b451f70311e7aac2f7c254d690d78a94849b59baef1
  で相違を確認。
- 比較結果：全 4 条件で identical=4,155、hard・core・expl・missing
  すべて 0 件。成功／失敗の対は ok/ok 4,127・error/error 28 で両側
  同一。両 TSV はバイト同一（SHA-256
  304e2da92b0cdb8fcf0e4d73f14f577b6c42bba393c7e6241099663c9f6e1e27、
  第127・128ラウンドの head TSV と同一）。
- TSV・比較ログは 検証用保存先/corpus-{base,head}-c46.tsv・
  corpus-compare-c46.log に保存。ベース用 clone・ハーネス複製・
  ファイル別出力・コーパス用 target は用後削除済み。
