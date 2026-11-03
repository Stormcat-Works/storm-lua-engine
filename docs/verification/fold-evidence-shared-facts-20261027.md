# 検証記録：fold 証拠理由の共通ファクト共有化による追跡コスト削減（2026-10-27）

## 変更の概要

constant-folding の追跡コストのうち、fold 証拠理由（decision 理由）
の構築時にノードごとに作っていた共通ファクト文字列を、fold 全体で
1 個だけ作って使い回すようにした。記録内容は文字列内容で同一のため
変わらない。

- `crates/storm-lua-syntax/src/explanation.rs`：
  `OptimizationReason::decision_shared` と `DecisionFactValue`
  （Owned／Shared）を追加した。decision の綴り・値キャッシュを
  モジュールレベルの共有キャッシュに移し、decision と
  decision_shared が同じキャッシュを使うようにした。Owned 値は
  decision とまったく同じ経路（内容で照合し、ヒットすれば共有
  Arc を使い、未ヒットで上限内なら登録する）で処理し、Shared
  値は呼出側所有の Arc をそのまま使う（ヒットすればキャッシュの
  Arc を使い、未ヒットで上限内なら呼出側の Arc を登録する）。
  回帰テスト `decision_shared_matches_decision` を追加した
  （同一ファクトを両経路で記録して等価であること）。
- `crates/storm-lua-minify/src/passes/literal_folding.rs`：
  fold 全体で 1 個だけ作る `FoldSharedFacts`（aggressive フラグと
  許容誤差の絶対・相対ビットパターンの 16 進文字列）を追加し、
  `fold_node`／`fold_node_inner` に引数として通した。Un 折畳みと
  Bin 折畳みの証拠理由の構築を `decision` から `decision_shared`
  に変え、共通 3 ファクトを Shared、それ以外のノード固有ファクト
  を Owned で渡す。

## 内訳切り分け（一時パッチによる変種測定。すべて revert 済み）

合成入力 dense-normal、VM 上、計数アロケータ付きハーネス
（directpass は 11 反復、trackcost single は 5 反復の中央値）。
分岐元は統合先端 817b214ec（第47ラウンド）。

- 新先端での再棚卸し：全コンパイル追跡差 115,277 回のうち、
  constant-folding 単独オン差が 16,733 回で最大のパス単独差。
  directpass では constant-folding 11,758 回のうち
  fold_expressions が 10,797 回（約 92%）、
  simplify_control_flow が 961 回。
- fold_expressions 10,797 回の分解：走査前段
  （resolve＋analyzer＋clone_ast）は約 1 回、rewrite() は約 0 回、
  derive 遅延経路が 4,800 回、explain の記録が約 480 回、
  証拠理由の構築が 5,760 回、残り約 237 回。
  すなわち証拠理由の構築が全体の約 53% を占める。
- 証拠理由の構築コストの内訳は、decision に渡す前の
  caller 側の文字列整形（literal_fact・format!・to_string・
  op.clone）で、decision 内部の intern キャッシュはヒット時でも
  caller 側の確保を帳消しにできない。共通 3 ファクト
  （aggressive・absToleranceBits・relToleranceBits）は
  fold 全体で同一のため、共有化で 480 証拠ノード × 3 回
  ＝ 1,440 回の削減になることを、ファクト除去の診断変種で
  事前に確認した（診断のみで、実装では除去せず共有化した）。

## 実装上の注意（String から Arc への変換の罠）

初版では証拠サイトの全ファクトを `Arc::from(String)` で
Arc 化して decision_shared に渡したが、測定で逆に
約 1,440 回悪化した。原因は、`Arc::from(String)` が
String バッファとは別に Arc ボックスへ文字列を複写するため、
ノード固有ファクトごとに 1 回の余分な確保が生じること
（ヒット時でも caller 側の 2 確保は帳消しにならない）。
このため decision_shared のファクト値型を Owned／Shared の
enum に変え、ノード固有ファクトは Owned（decision と同一経路）、
共通ファクトだけ Shared で渡す形に修正した。修正後は予測どおり
1,440 回の削減になった。

## 測定結果（決定的な計数。時間差は主張しない）

- directpass constant-folding-expr 単独：無追跡 29,091 回→
  29,096 回（FoldSharedFacts 構築の 5 回分。fold 1 回あたりの
  固定費で、ノード数には依存しない）。追跡 39,888 回→
  38,453 回（追跡差 10,797 回→9,357 回で −1,440 回）。
  コード長 41,175・ノード数 6,883→7,363・changed 480 件は同一。
- directpass constant-folding 全体：無追跡 39,834 回→39,839 回、
  追跡 51,592 回→50,157 回（追跡差 11,758 回→10,318 回で
  −1,440 回）。コード長・ノード数は同一。
- trackcost single：全コンパイル追跡差 115,277 回→113,837 回
  （−1,440 回）。constant-folding 単独オン差 16,733 回→
  15,293 回（−1,440 回）。all-off 差（27,291 回）・他パス・
  無追跡側はすべて同一で、削減が folding 単独と完全一致する。

## 検証

- 対象 3 クレート（syntax 112＝新規 1 件含む・minify 516・
  build 126）とワークスペース全件（50 スイート・1008 件）が成功。
  `cargo xtask check` 通過。fmt クリーン。clippy（lib 対象、
  警告 0）を通過。
- 実コーパス全件（4,155 ファイル×4 条件、base 817b214ec 対 head、
  開発ホスト2 の読み取り専用コーパス、両バイナリは VM で
  Rust 1.97.1 ビルドし .ud へ送付して走行、検証バイナリの
  SHA-256 相違も確認：base 0ad426c2…、head 2d66f225…）の比較では、
  成功／失敗（4,127／28）・生成コード・Map 中核・正規化済み
  Map 全体のすべてが差分 0 件で、両 TSV はバイト同一だった
  （SHA-256 e1379f09…、16,620 行）。
  TSV は `workers/w1/corpus-verify-fold-evidence-20261027/` に保存し、
  開発ホスト2 の一時ファイルは用後削除済み。
  ベース用 worktree（sle-base-wt）も用後削除済み。
