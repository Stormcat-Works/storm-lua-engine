# v0.5.0 公開記録

2026-11-27。候補コミット `7866b3c1083b5607ecb828c988187458a21cb0f7` に CHANGELOG の公開日更新（2026-11-16 から 2026-11-27）だけを加えたリリースコミット `e7f0e0e179aa0c3499a68e196e5d319d39140dcd`（ツリー `5f5022712041d597372f24c1da0313b9706add3d`）を公開した記録。変更内容は CHANGELOG の 0.5.0 節を参照する。

## 承認経緯

- 管理者が 2026-11-27 に v0.5.0 の公開作業の開始を指示した。
- 公開に先立ち差分レビューを実施し、挙がった指摘は全て解消した（文書の事実誤りの修正、最適化差分に対するコーパスでの Source Map 不変性の検証、スナップショットと継続の CHANGELOG 記載、`httpGet` の引数個数の扱いの実ゲーム確定と修正）。

## プレフライト照合

- GitHub ls-remote: develop `7866b3c`、main `88ec6f0`、release `88ec6f0`（想定どおり）。タグ `v0.5.0` と GitHub Release は未存在だった。
- コーパスの定義マニフェスト（SHA-256 清算）で 4,155 / 4,155 件の一致を再確認した。

## 最終ゲート（候補ツリーで実施）

- `cargo test --workspace --all-features --locked`: **2,047 passed / 0 failed**。
- `cargo fmt --all --check`、`cargo xtask check`、`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`、`cargo doc --workspace --all-features --no-deps --locked`、`cargo check --workspace --locked`: 全て成功。
- `tools/check_screen_fixtures.py`、`tools/generate-toolchain-licenses.py --check`（Emscripten 6.0.6）、Python ツールテスト 19 件、`tools/release` のテスト: 成功。
- コーパス FULL（4,155 ファイル × 4 構成 = 16,620 行）: 集約 TSV の SHA-256 が公開前検証の基準値 `ac107845afe13a0a26e53643bad37cdce72854671237c355396685eb54a635e2` と一致した。生の Source Map の内容差分も 0 で、差は生成元リビジョンの印字とそれを覆う完全性ダイジェストだけだった。
- TypeScript SDK: 単体テスト 37 / 37、consumer 検査成功。コンパイラテスト 18 / 18。
- 実 WASM テスト: 113 / 113。ブラウザテスト（`tools/test-browser.mjs`、`tools/test-compiler-browser.mjs`）: 成功。
- Playground: 単体テスト 38 / 38、ビルド、ブラウザテスト: 成功。
- `tools/check-package.mjs`、`tools/check-artifacts.mjs`（78 ファイル、WASM 8 件）: 成功。
- 配布物はリリースコミット `e7f0e0e` から構築し（Playground の `version.json` の revision で確認）、完成した npm tarball に対する隔離 consumer 検査（`tools/test-package.mjs`）も成功した。

## ブランチ反映（fast-forward のみ）

リリースブランチ `muse/release-v0.5.0` を候補から切り、CHANGELOG の公開日更新の 1 コミットを載せて push した。同名の旧候補ブランチ（2026-11-16）の先端コミットは、作り直しに先立って別名で保存した。

| ブランチ | 変更前 | 変更後 |
| --- | --- | --- |
| main | `88ec6f0` | `e7f0e0e` |
| develop | `7866b3c` | `e7f0e0e` |
| release | `88ec6f0` | `e7f0e0e` |

PR #14（main 向け）は、main が PR の head コミットへ到達したことで自動的に MERGED となった（mergedAt 2026-11-27T10:22:36Z）。手動のマージ・クローズ操作は行っていない。force push は用いていない。

## CI（Engine conformance）

- PR #14 の run `45979600506`（pull_request）: **success**。ジョブは native (ubuntu-latest) / native (windows-latest) / native (macos-latest) / wasm の全て success。
- main push の run `45981575697`（push）: **success**（同 4 ジョブ success）。publish.yml の事前検証が要求する「対象 commit の push イベント成功 run」を充足した。

## タグ

- 注釈付きタグ `v0.5.0`: tag object `121bb2433ee590f84c3ece9e21779c00a0fd9167` → commit `e7f0e0e`。メッセージは「Storm Lua Engine v0.5.0: Component Mod の追加と Vehicle／Addon 意味論の実ゲーム整合、スナップショットと継続」。

## GitHub Release

Draft で作成し、資産の digest をローカルハッシュと照合してから公開した。

- URL: https://github.com/Stormcat-Works/storm-lua-engine/releases/tag/v0.5.0
- publishedAt: 2026-11-27T10:33:37Z、targetCommitish: main、draft / prerelease とも false。
- Release ノートは CHANGELOG の 0.5.0 節を用いた。
- 資産（GitHub 側 digest とローカル実測が一致）:

| 名前 | バイト数 | SHA-256 |
| --- | --- | --- |
| stormcat-works-storm-lua-engine-0.5.0.tgz | 2,072,638 | `40ba140152cb0b24d27c66eaed19852a30cbae65d523d2decf0d7750ce8908a7` |
| storm-lua-engine-playground-0.5.0.zip | 3,520,947 | `407ddc89b3df0245cd6f30093ddf7f5a984503ece2ed372e2dc9fa9b3d4e31b3` |
| SHA256SUMS | 304 | `ec29c2f3100febb661bbb2285ee74172192cc898d89098be415d8a78d4733138` |
| release-verification.json | 1,667 | `2bc793eed2230fc9a51737e4d0dbef9d6ea067ba3aff54dae4cb367697d63607` |
| publication-verification.json | 3,156 | `c44dfd7f8df4035f1a7899cb52f53d8f4c4a6c1fc1614c9cc36bdd4162561fb3`（公開後検証の完了後に追加） |

## npm 公開（publish.yml）

- 試行 1: Release 公開で起動した run `45983125638`（release イベント、head_sha `e7f0e0e`）。verify **success**、publish **success**、verify-registry **failure**。失敗はレジストリの処理ウィンドウ内で新しい版がまだ 404 だったことによるもの（`AssertionError: Version not found after publication`）で、公開そのものは完了していた。
- レジストリを直接確認したところ、版 0.5.0 が存在して `latest` になっており、integrity `sha512-n6XBzA71giIq2X6tUHy7+xpMJrMwylgoWbcgQnwb2VMSpB3gKcegmGgBzrS8+ZyjJdggC7PqTiox0QuB9Hn5yg==` がローカル tarball と一致した。provenance の attestations（publish/v0.1、SLSA provenance v1）も付いている。
- 試行 2: `workflow_dispatch` で main から再実行した run `45983773980`（tag `v0.5.0`）。verify **success**、publish はレジストリに同一バイトが既にあったため **skipped**、verify-registry **success**。全体の conclusion は **success**。
- npm への公開はちょうど 1 回だけ行われた。

## Playground

- `release` ブランチの前進で Cloudflare Workers Builds がデプロイした。
- 公開後の https://www.makkii.jp/tools/stormworks/storm-lua-engine/version.json が `{"version":"0.5.0","revision":"e7f0e0e179aa0c3499a68e196e5d319d39140dcd"}` を返し、ページは HTTP 200 だった。

## 手順との差異・備考

- `docs/release.md` は公開記録の置き場と形式を指定していないため、本記録は `release-0.4.0.md` の形式に倣った。なお v0.4.1 の公開記録は、ツリー内のファイルとしては存在せず、GitHub Release の資産 `publication-verification.json` としてのみ残っている。今回も同名の資産を添付し、併せて本記録をツリーに残した。
- リリース作業環境には Emscripten 6.0.6（emsdk）と Playwright のブラウザが不足していたため、本工程で導入した。ツリー側の変更ではない。
- 本記録のコミットで STATUS.md と TASKS.md の公開状態の記述も v0.5.0 に揃えた。
