# decision 証拠の fact リスト共有化による追跡コスト削減の検証（2026-10-28）

## 変更内容

compile の追跡コストのうち、constant-folding の証拠構築に対する共有化。
記録内容は内容比較で同一のため変わらない。

- `decision_shared` の facts リスト共有化
  （`crates/storm-lua-syntax/src/explanation.rs`）
  - 従来は証拠ノードごとに facts の `Vec`（1 確保）とその `Arc` 化
    （1 確保）を払っていた。同じ規則を同じリテラルへ適用する fold は
    事実上同じ fact リストを繰り返し作るため、内容キーで共有する
    キャッシュ（`DecisionIntern.fact_sets`、上限 4096 件）を追加した。
  - キーはスタック配列（`[Option<(Arc<str>, Arc<str>)>; 10]`、強参照保持）
    のため、キャッシュヒット時は確保 0 回である。キーに強参照を持たせる
    ことで、解放済み割当てを再利用した別文字列との混同は起きない。
  - 形状上限は 10 件（fold 証拠は Un 7 件・Bin 10 件）で、超過時は従来の
    無キャッシュ経路へ fallback する。fact リストが空の場合は従来の
    共有空リストを使う。公開 API は変わらない。

## 回帰テスト

- `explanation::tests::decision_shared_reuses_identical_fact_lists`
  （同一 fact リストは facts `Arc` がポインタ一致、1 値違いは不一致・
  内容不等であることを検証）

## 測定（合成入力 dense-normal、VM）

- 内訳切り分け（一時変種。診断後に revert し、repo 本体への変更は
  上記の実装のみ）：
  - fold_node の derive を外すと追跡差 4,657 回→1,777 回（derive 経路
    2,880 回）。
  - explain の記録を外すと追跡差 4,657 回→4,177 回（explain 記録
    480 回）。
  - 内側 rewrite を無記録の値代入へ変えても、collapsed-value ブロックを
    外しても追跡差は完全同一（33,753 回）で差なし。子 ID は折畳みで
    変わらないため内側 rewrite はほぼ常に no-op で、collapsed リテラルは
    ほぼ全て known アンカー付きである。よって残差 1,297 回は証拠構築が
    主因と確定した。
- directpass constant-folding-expr（11 反復中央値）：
  無追跡 29,096 回・コード長・ノード数は同一で、追跡 33,753 回→32,793 回
  （追跡差 4,657 回→3,697 回で −960 回＝証拠ノード 480 件×2 回）。
- trackcost single（5 反復中央値）：
  全コンパイル追跡差 66,084 回→65,124 回（−960 回）。
  constant-folding 単独オン差 8,265 回→7,305 回（−960 回）。
  api-alias・scope-renaming・root-local・all-off 差はすべて同一。
- 時間差は主張しない。
- 注：計数は同一プロセス内 11 反復の中央値で、プロセス全体の fact
  キャッシュが温まった状態の値である。初回コンパイル（未見のリスト）は
  従来と同コストで、繰り返しコンパイル（候補探索など）で共有が効く。

## 実コーパス全件比較（base `88236c6` と head、開発ホスト2）

- 4,155 ファイル×4 条件、欠落 0。両 TSV はバイト同一。
- 全条件 identical=4,155、hard・core・expl・missing すべて 0。
- 成功 4,127／失敗 28 は両側同一。
- すなわち本変更はコーパス範囲の出力に一切影響しない。
- 検証バイナリの SHA-256 相違も確認（base `48cc4ca7…`、
  head `befde96d…`）。コーパスは読み取り専用でコピーなし。
  開発ホスト2 の一時ファイルは用後削除済み。
