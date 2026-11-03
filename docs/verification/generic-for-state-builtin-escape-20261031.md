# ジェネリック for の state／control 値がイテレータへの escape として数えられず、組み込みルートの変異が見落とされる（2026-10-31、未公開）

## バグ

w6 第50サイクルの横断点検（第49サイクル候補 C と同型の「Call のみの呼出・escape 概念」の点検）で発見。`mutated_builtin_roots`（`crates/storm-lua-analysis/src/effects.rs`）の値フローは、呼出引数・return 値・テーブルフィールド値として未知コードへ渡る組み込みルート名を escape として数えるが、ジェネリック for のヘッダには腕が無く、state 式と control 式がイテレータ関数の第1・第2パラメータへ流れる経路が欠落していた。

そのため、state に組み込みテーブル自体（またはその別名）を渡し、イテレータがパラメータ経由でメンバを書き換える形状で、ルートが「プログラム全体で未変異」と誤判定され、変異に依存する最適化が書換え前の値で評価された。再現（境界スイート v15 探査 f50-s1-state）: ループ前に `math.floor(1.5)` を、イテレータが `st.floor` を書き換えた後に再評価する形状で、原典 101 に対し opt / safe が 2（`constant-folding` と `literal-call-folding` の両パスが独立に誤って畳み込むため、片方の無効化では復旧しない）。同じ欠落は LICM の純粋性ベトーにも効き、state 経由で `input.getNumber` の結果が変わる形状では target のみ乖離した（f50-licm-statewrite、原典 103 に対し target 8）。control 位置の変種（f50v-control2、原典 100 に対し opt 1）、別名経由の変種（f50v-statealias）でも発現した。通常の呼出引数経由（f50v-callarg）では発現せず、Forin ヘッダ経路に固有の欠落であることを確認した。

## 原因箇所

`crates/storm-lua-analysis/src/effects.rs` の `mutated_builtin_roots` が構築する値フロー（`named_value_bases` の走査）。`Node::Forin` の腕自体が存在せず、ヘッダ式が走査されていなかった。なお効果解析本体の Forin 暗黙呼出のモデル化（Codex 指摘 6 の修正）とは別の、変異ルート集合構築側の欠落である。

## 修正

Call 引数と同じ規約で、ジェネリック for のヘッダ式のうちイテレータ式（第1式）を除く state／control 式について、その名前値（別名解決済み）を escape として登録する腕を `mutated_builtin_roots` の走査に追加した。イテレータ式自体は関数位置であり引数ではないため、Call の関数位置と同様に数えない。回帰テスト4件を追加（state 経由の escape、control 経由の escape、別名経由の escape、ユーザーテーブルに対する過剰な変異判定の否定）。escape 側3件は修正前のコードで失敗することを確認済み（腕を無効化して再実行）。

## 検証

- ワークスペーステスト全通過（本修正時点で 1,478 件 = 修正前 1,474 + 新規 4、統合先端へリベース後のツリーでは 1,496 件）。fmt / clippy（自変更への新規警告なし）/ xtask check 通過。
- 再現集合 f50-s1-state / f50v-statealias / f50v-control2 / f50-licm-statewrite は全5構成で原典と一致に転じた。対照（f50v-callarg、f50v-retiter、直接・別名書込みで捕捉される f50-s1-direct / f50-s1-alias）は不変。
- Tier FULL（実世界コーパス 4,155 件 × 4 条件 = 16,620 行、マニフェスト 4,155/4,155 照合済み、開発ホスト1）: 着手時基点 `72f684e46` に対しても、統合直前基点 `48ae6334d`（= FULL-VERIFY-TIP、w4 第205〜208ラウンド統合後）に対しても、全4条件で完全同一（identical 4,155、expl-only 0、map-core 0、hard 0、状態ペア ok 4,128 / error 27、反転なし）。base / head の corpuscheck バイナリは f50-s1-state の生成コードハッシュで機能的に同定済み（base は畳み込み形 200 文字、head は非畳み込み形 219 文字）。回帰 0 件。
