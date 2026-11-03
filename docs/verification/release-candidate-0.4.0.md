# v0.4.0 公開候補の検証（リリース準備）

2026-11-02。**公開commit確定前のローカル候補の準備・検証記録。** [公開手順](../release.md)のv0.4.0節と対応する。公開（push・タグ・GitHub Release・npm・本番配備）は管理者の明示承認（go/no-go）待ちであり、本記録は公開済みを意味しない。

## 候補の構成

- 統合ブランチ`muse/perf-optimization`先端：第301ラウンド検証記録`fbf5742`（コード`4481b5e`）へ、リリース準備コミット`2739c0aa0ecf95cf0b9a6a88d6262db80694646b`（版番号0.4.0・CHANGELOG確定・README／app／packagesの版参照・TASKS.md・docs/release.md）と本記録を追加したもの。
- 版管理： ワークスペース版はルート`Cargo.toml`の`[workspace.package]`が正本（0.3.1→0.4.0、全15クレートが`version.workspace`で継承）。`Cargo.lock`の変化はworkspace内15件の版のみ。`app/package.json`・`packages/lua-engine/package.json`と各lockfileも同じ版へ揃えた。

## go/no-go条件の事前確認（準備着手時点の記録）

- リリース範囲の修正（S1〜S11・S14）は全てマージ済み（第285〜第299ラウンド）。S12は承認済み方言差異としてCLOSED、S13は帰属のみで範囲外（いずれも2026-11-02のユーザー決定）。
- 独立回帰： 回帰パックcheck-postは先端`fbf5742`で114/114 PASS（期待表更新後）。製品側の退行・未着地は0件。
- FULL-VERIFY-TIP `4481b5efb7df855d79c75442549a7a6ed036d5f6`は準備着手時点のコード先端`4481b5e`と一致。
- コーパス（現行版）のマニフェスト照合： 4,155/4,155 OK。

## 最終FULL検証（準備後先端・現行コーパス版）

- corpus： 実世界コーパス4,155ファイル×4条件＝16,620行。corpuscheckはbench-harnessの同一ソースから準備後先端でビルド（バイナリSHA-256 `f8ea95c903383ee9c6969db9aaaa45e1ee1451e7ff8b3fcf511ab79087a2f74f`）。整列TSVのSHA-256は`42ce28cacbfec693acb1c1dc3b338251ebe46e5a1f09dab284c4bd393e9b4e28`で、第300・第301ラウンドのTSVとバイト同一。構成ごとのstatusはok 4,126／error 29で不変。
- テスト： ワークスペース全体50スイート・1,775 passed / 0 failed（exit 0）。
- fmt： `cargo fmt --all -- --check`の差分は従来から存在する10箇所のみで、本準備の変更ファイルへの指摘はない（第301ラウンド記録と同一集合）。
- clippy： `cargo clippy --workspace`（lib／bin対象）はexit 0。storm-lua-minify libの警告8件はベースと同一集合。
- FULL-VERIFY-TIPを準備コミット`2739c0aa0ecf95cf0b9a6a88d6262db80694646b`へ前進させた。

## 既存不整合（本準備の退行ではない。公開判定の入力）

- `cargo xtask check`が失敗する。`tools/check_repository.py`のアーキテクチャ規則が`storm-lua-build`→`storm-lua-vm`の通常依存を未許可とするためで、第299ラウンド`c65a629`（S10/S14ゲート）が当該依存を導入して以降の状態である。ベース`fbf5742`でも同一に失敗することを確認した。解消には規則側の許可追加か依存の再構成が必要で、リリース準備の範囲では変更していない。
- `cargo clippy --workspace --all-targets`は`storm-lua-vm`のlib test（`src/dialect.rs`のテストコードの`expect`／`unwrap_err`）がdeny lint（`expect_used`／`unwrap_used`）に触れて失敗する。ベース`fbf5742`でも`-p storm-lua-vm --all-targets`で同一に再現する。ラウンドの合否に使ってきたlib対象のclippyは上記のとおり通過している。

## 残る公開工程

- ユーザーのgo/no-go確認とpush承認（初回pushでは統合ブランチのベース`64c2d88`も公開される形になることの最終確認を含む）。
- 検査済みcommitの3 OS/WASM CI、producer/revision確定後の配布物再検証、公開候補JSON・梱包（source manifest・npm tarball・静的サイトZIP・SHA256SUMS）、タグ・GitHub Release・npm・本番配備。
- 上記xtaskアーキテクチャ検査の不整合の処置。

## 第304ラウンドでの修正と再検証（2026-11-03）

第303ラウンドのスカッシュ公開で回した CI（Draft PR #3、3 OS／WASM）が、想定外の実バグ1件と従来分のゲート不整合を検出したため、工程2・3を停止して先に修正した。本節が現在の候補の構成であり、前節までの「既存不整合」は本節で解消済みとして読み替える。

### 修正

- **wasm32 コンパイル不能（実バグ、`d0e2865bcd235b23e7718e3ed981dc823fcce181`）**: 内部ハッシュ（FxHash 型）の状態を `usize` で保持し、64bit のシードリテラルが 32bit ターゲットで `overflowing_literals` になっていた。状態・シード・混合を `u64` 固定幅へ変更し、64bit 環境でのハッシュ値の列が従来とビット単位で一致するようにした。対象は `storm-lua-syntax` の provenance と `storm-lua-minify` の file-local コピー2件。あわせてコンパイラ系クレートを wasm32 の両ターゲット（unknown-unknown／emscripten）で洗い出し、同種の幅依存エラーが他にないことを確認した。
- **ゲート整合（`6118faf6a7688738933ca0769806d10a7af98f8f`）**: `cargo fmt --all` を適用し従来分の差分10箇所を解消した。`tools/architecture.json` に第299ラウンドの正当な依存（storm-lua-build／storm-lua-minify → storm-lua-vm）を許可し、`cargo xtask check` を通過させた。minify lib の警告8件、all-targets のテストコード lint（vm dialect・analysis effects・minify 各所）、`storm-lua-syntax` の doc リンク切れを、いずれも最小変更（コードの書き換えと、既存慣行に沿ったスコープ付き allow）で解消した。

### 再検証（コード先端 `6118faf6a7688738933ca0769806d10a7af98f8f`）

- corpus FULL: 実世界コーパス4,155ファイル×4条件＝16,620行。整列TSVのSHA-256は `42ce28cacbfec693acb1c1dc3b338251ebe46e5a1f09dab284c4bd393e9b4e28` で、第300〜302ラウンドと**バイト同一**。構成ごとの status は ok 4,126／error 29 で不変。FULL-VERIFY-TIP は本先端へ前進した。
- ワークスペーステスト: 50スイート・1,775 passed / 0 failed。
- CI と同一条件のローカルゲート: fmt check、`cargo xtask check`、clippy（`--workspace` と `--workspace --all-targets --all-features`、いずれも `-D warnings`）、`cargo doc`（壊れた intra-doc link を deny）、conformance の4 example、Python／Node のツールテストが全て通過した。

### 新たに判明した制約（処置は設計判断待ち）

- コンパイラ系 WASM（`storm-lua-compiler-wasm`、wasm32-unknown-unknown）は現行の依存構成ではビルドできない。`storm-lua-vm` が vendored Lua（mlua）に無条件で依存し、そのビルド（lua-src）が wasm32-unknown-unknown をサポートしないためである。第299ラウンドでロード合法性ゲートが build／minify から vm への依存を加えたことに由来する。emscripten 側のランタイム系 WASM（storm-lua-wasm、conformance の lua_backend）はビルド可能で、上記の洗い出しで確認済みである。ゲートの判定を wasm でどう提供するか（依存の分離か判定経路の差し替えか）は設計判断を要するため、本ラウンドでは修正していない。
