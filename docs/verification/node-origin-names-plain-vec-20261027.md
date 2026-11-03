# NodeOrigin.names の素の Vec 化による追跡コスト削減（2026-10-27、未公開）

`NodeOrigin.names` を `Arc<Vec<(NameSite, StoredOrigin)>>` から素の
`Vec<(NameSite, StoredOrigin)>` に変え、改名時の名前 Vec 複写に伴う
Arc ボックス確保をなくした。シリアライズ形は配列のままで同一で、
記録内容・生成 Lua・候補選択・schema1 のマップ本体は維持する。
Map 比 1.2 の目標は未達のままであり、この変更はその達成を主張しない。

## 原因

追跡差の内訳切り分け（合成入力 dense-normal、計数アロケータ付き
ハーネス、11 反復中央値）では、scope-renaming 単独の追跡差
3,745 回（無追跡 5,498 回→追跡 9,243 回）のうち、改名ノード
961 件に対する名前側の残りは、名前 Vec の `Arc::make_mut` による
複写 1,922 回（Vec バッファ＋新 Arc ボックスの 2 回／ノード）だった。

複写が避けられない理由は、源 AST が存続する限り finish 側の小手先では
解消できない。`finish_rename_inner` は snapshot 由来を複写するが、
snapshot の `NodeOrigin` は源 AST のスロットとも共有されているため
`Arc::unwrap_or_clone` できず構造体複写となり、共有された names Vec
への `Arc::make_mut` が必ず新しい Vec と Arc ボックスを作る。
名前項目への理由記録そのものは共有テール化済みで確保を生まないため、
残るコストは Vec 複写の枠組み自体にあった。

## 変更

- `crates/storm-lua-syntax/src/provenance.rs` の `NodeOrigin.names` を
  素の `Vec` に変えた。空 Vec は確保しないため、名前を持たない
  ノードの由来複写コストは増えない。`empty_names` の共有シングルトンは
  不要になったため削除した。seed 時の push は直接 push になった。
- `crates/storm-lua-syntax/src/node_arena.rs` の names に対する
  `Arc::make_mut` 呼出（import・finish_rewrite・finish_rename・
  copy_name 系）を素の Vec 操作に変えた。
- 変更範囲は storm-lua-syntax クレート内のみで、names を読む他クレートは
  ない。公開 API のシグネチャは変えていない。`NodeOrigin` の
  シリアライズは `Arc<Vec>` でも `Vec` でも JSON 配列であり、形は同一。

## 測定

合成入力 dense-normal、11 反復中央値。無追跡側は変更前後で完全同一。

- scope-renaming 単独：追跡側 9,243 回→8,282 回、追跡差
  3,745 回→2,784 回（−961 回、約 −26%）。コード長・ノード数は同一。
- api-alias 全体：追跡側 126,690 回→124,925 回、追跡差
  11,159 回→9,394 回（−1,765 回、約 −16%）。saved=7127・コード長・
  ノード数は同一。
- 全コンパイル（trackcost、既定条件）：追跡差 170,832 回→160,576 回
  （−10,256 回）。無追跡側は確保回数・バイト数とも完全同一で、
  削減が追跡経路だけのものであることを確認した。
- 時間差は負荷変動が大きく、時間改善は主張しない。

## 検証

- 回帰テスト：`names_serialize_as_a_json_array_after_the_vec_representation_change`
  を追加し、追跡付き AST のシリアライズで names が JSON 配列であることと
  ラウンドトリップ後の AST 一致を検証した。
- 対象 3 クレート（syntax 106・minify 441・build 126）とワークスペース
  全件（50 スイート）が成功。xtask check・fmt・clippy（警告 0）を通過。
- 実コーパス全件（4,155 ファイル×4 条件）の比較では、成功／失敗
  （4,127／28）・生成コード・Map 中核・正規化済み Map 全体のすべてが
  差分 0 件で、両 TSV はバイト同一だった。
