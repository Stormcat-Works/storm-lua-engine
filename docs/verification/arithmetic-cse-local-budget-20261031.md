# 逐次算術 CSE のキャリア挿入にローカル予算チェックが無く、safe 構成で読込不能コードを生成した（2026-10-31、未公開）

## バグ

w6 第52サイクル（Codex 指摘 #7 系「ローカル挿入パスに予算チェックなし」クラスの完結監査）で発見。逐次算術 CSE（`straightline-arithmetic-cse`）は、共通部分式を `local c = …` のキャリアへ巻き上げる際、囲み関数の同時有効ローカル上限を確認していなかった。再現（c52-probes/d3-cse-ontick-198）: 引数 198 の onTick 内で CSE が発火すると、198（引数）＋2（既存ブール）＋1（キャリア）＝ 201 になり、safe 構成でのみ vm-error になった（opt は後段の function-globalization がキャリアを救済、plain／target は一致）。切り分け: CSE を単独無効化すると safe が正常化し、function-globalization を単独無効化すると opt にも同じ vm-error が現れた（潜伏形）。

## 原因箇所

`crates/storm-lua-minify/src/passes/arithmetic_cse.rs`。第212ラウンドで LICM へ入った予算チェック（関数スタック＋メモ化した最大値＋挿入数カウント）が、このパスには入っていなかった。同クラスの 9 例目。

## 修正

- LICM と同型の会計を導入: 変換走行で関数スタックを維持し（Funcstat のメソッド定義では暗黙の `self` を加算）、関数ごとに共有ヘルパー `max_active_locals` の結果をメモ化、グループごとのキャリア挿入数を数え、`max + inserted + 1 > MAX_LOCALS`（200）のグループは変換をスキップする。
- 回帰テスト 2 件を追加: 198 引数でスキップ／197 引数で発火。

## 検証

- ワークスペーステスト全通過（1,538 件、本ラウンド分は arithmetic_cse の 2 件）。
- 再現 d3-cse-ontick-198 は全 5 構成で正常・トレース一致に転じた。なお同形状の c1-cse-198／d3 を function-local-globalization 無効化で検証すると、符号因数分解の合成キャリアによる別の溢れが残る（切り分けで確定）。これは第219ラウンドで処置した。
- Tier FULL（base `2c26b9ea9` = FULL-VERIFY-TIP → head、実世界コーパス 4,155 件 × 4 条件 = 16,620 行、開発ホスト1）: base／head の corpuscheck TSV がバイト単位で完全一致（SHA-256 `e88ee2b4…`、両側とも success 4,128／error 27 × 4 条件）。上限近傍での発火抑制のみのため、コーパスへの影響ゼロは設計どおり。回帰 0 件。
