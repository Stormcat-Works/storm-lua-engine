# リネーム済みトークンの Map 表現の統一と compose の eligibility 保存（2026-10-30、未公開）

## 背景

R137 の字句勝者ローカル限定リネームは、生成コード上のローカル名を
短縮するが、Source Map 側の表現が統一されていなかった。字句層
（`CopiedToken`）は `renamed` と生成端を持ち、字句層の validate は
リネーム済みトークンを綴り不一致として拒否しない。一方でワイヤ形
（materialize 済みレイアウトと schema-2 トークン表）はリネームを
表現できず、リネーム済みトークンを exact-copy として記録していたため、
エンジン自身の `validate` が自身の出力を拒否していた（字句勝者 5 件
の単一コンパイルで全件失敗。失敗クラスは展開時の生成順序検査で、
元綴り長から算出した生成端が次レコードを追い越すために起きる）。
materialize 経路ではリネーム済みトークンが `copied: None` になるため
simple_copy ゲートが不成立だった。project 経路では compose が
mapping 部分ごとに origin を複製し、合成 origin を無条件で追加する
ため、composed 形はゲートの未参照・順序・種別の各条件を満たさず、
従来レイアウトになっていた（成功 3 件で計約 49MB）。

## 設計（リネーム済みトークンの表現を全層で統一する）

- リネーム済みトークンは exact-copy ではない。materialize 済み
  レイアウトでは `copied` を null にする（`copied_tokens` と同一の扱い）。
- トークン表レコードは、未リネームは従来の 4 要素
  `[generated_start, source_start, source_end, name_id]`、リネーム済みは
  第 5 要素 `generated_end` を追加した 5 要素とする。旧 4 要素マップは
  従来意味（exact-copy）で読み続けられ、展開はそれ以外の要素数を拒否する。
  展開したリネーム済み mapping は `copied: None` になる。
- ゲート（`simple_copy_token_table` と `lexical_copy_only`）は、origin が
  Source・Name 精度・name が元綴りスライスと一致という条件下で
  `copied: None` の mapping をリネーム形として受理する。展開の可逆性
  （origin の一意タプル・first-appearance 順序・被覆）は従来条件のまま担保する。
- compose には単純形保存の高速路を加えた。入力が単純形（全 origin が
  Source・Name／Token 精度・関連／関係／理由／変換なし、contexts・
  constructs・dispositions 空、mappings は exact-copy・リネーム形・
  未知ギャップのみ）の場合に限り、origin をインプレースで再マッピング
  し、部分ごとの origin 複製も合成 origin の無条件追加も行わない。
  primary や copied が範囲を跨ぐ・範囲外になる・非単純形が現れた場合は
  従来の compose へフォールバックし、非単純入力の composed 形は従来と
  同一に保つ。
- `validate_for_code` と `validate` の検査内容は変更していない。
  リネーム形は `copied: None` のため exact-copy 検査の対象外になるのが
  正しい動作で、検査の緩和ではない。schema バージョンは 2 のまま。
- 既知の限界：複数モジュール project はゲートの単一ソース条件により
  引き続き従来レイアウトになる。lifeboat は 69 件の非単純 origin
  （変換・理由を持つテンプレート由来の origin。トークン表は理由・変換を
  表現できないため、token 表化は情報の欠落になる）を保持するため、
  正しく従来レイアウトに残る。部分的な単純形保存（非単純 origin を含む
  入力でも単純部分だけを共有化する）は、一般の compose 出力を変えるため
  本変更では行わず将来項目とする。

## 変更

- `crates/storm-lua-build/src/canonical_map.rs`：トークン表レコード型の
  可変化、ゲートのリネーム形受理、字句ライタの 5 要素レコードと
  materialize 済みレイアウトでのリネーム済み `copied: null`。
- `crates/storm-lua-build/src/optimized_source_map.rs`：`MapExtension.tokens`
  の可変長レコード化、展開の 4／5 要素対応とリネーム済み `copied: None`、
  `lexical_copy_only` のリネーム形受理。
- `crates/storm-lua-build/src/map_composition.rs`：単純形判定と
  インプレース再マッピングの高速路（フォールバック付き）。
- 回帰テスト 5 件（リネーム済み字句 encode→validate ラウンドトリップと
  compact／materialize バイト一致、未リネーム 4 要素維持、compose 保存
  2 件、非単純入力のフォールバック 1 件）。

## 測定と検証

- 対象クレート（build 132 件）・ワークスペース全件 1,340 件が成功。
  `cargo xtask check`・fmt・clippy（build クレート警告 0。minify の
  loop_member_reads.rs テスト由来の既存エラー 3 件は本変更のファイル外
  のため従来どおり）を通過。
- 字句勝者 5 件の probe（単一／project／lifeboat、計数のみ）：
  単一 validate は全件失敗から全件通過へ。project 成功 3 件は全件
  トークン表化し、map 合計は約 49,341,846B から約 4,581,213B へ
  （約 44.8MB 削減）。lifeboat 成功 3 件は上記の理由により従来
  レイアウトに残るが validate は通過する。生成コードは全経路で不変。
- 検証区分は Tier FULL：コーパス比較（4,155 ファイル×4 条件、base f0f67098 対 head、開発ホスト1 ローカル、manifest 照合 4,155/4,155 OK）は 16,620 行で欠落 0 件、成功／失敗（4,128／27）・生成コード・Map 中核すべて完全一致。差分は字句勝者 5 件の map 構成 2 条件のみ（計 10 行）の説明系差分で、生成コードと標準 Map は不変、x_storm トークン表のワイヤ形（5 要素レコード化）のみの変化である。
