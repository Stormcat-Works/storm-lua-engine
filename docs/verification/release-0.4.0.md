# v0.4.0 公開記録（工程4以降）

2026-11-04。候補スカッシュ `f26c9b3d8f157948d1a28f5107213294a1ea82c4`（単独親＝公開 main `44376b89bbff6af1a8b98fcea3f4e399500b4bce`、ツリー `784c913777171ba7ec7dedda53eb9c2c80b8bc84`）を公開した記録。候補の検証経緯は `release-candidate-0.4.0-r309-rebuild-20261104.md`（再構築4回目）までの各記録を参照。

## 承認経緯

- ユーザーは 2026-11-04 06:38 JST に Codex 再レビュー（run9）の見送りと公開工程への移行を決定し、06:42 JST に公開の再試行を明示した。これを公開の最終承認として実施した。
- 初回の実行委任は安全ポリシーにより拒否され未実行だった。GitHub の状態変更がないことを ls-remote で確認したうえで、再指示を受けて本実施に至った。

## プレフライト照合

- GitHub ls-remote: main `44376b89`、develop `5a168d63`、release `5a168d63`、muse/release-v0.4.0 `f26c9b3`、タグ v0.4.0 と Release v0.4.0 は未存在（期待どおり）。
- 配布物（`~/sle-release/v0.4.0/artifacts/`）の `sha256sum -c` は全件 OK。リリース作業リポは `f26c9b3` で clean。ホストの root 空きは 18 GB（下限 4 GB を上回る）。

## ブランチ反映（fast-forward のみ）

各 push の前に `git merge-base --is-ancestor` で旧先端が `f26c9b3` の祖先であることを確認し、force push は用いていない。push 後の ls-remote で3ブランチとも `f26c9b3` を確認した。

| ブランチ | 変更前 | 変更後 |
| --- | --- | --- |
| main | `44376b89` | `f26c9b3` |
| develop | `5a168d63` | `f26c9b3` |
| release | `5a168d63` | `f26c9b3` |

Draft PR #3 は、main が PR の head コミットへ到達したことで自動的に MERGED となった（mergedAt 2026-11-03T21:43:47Z、mergeCommit `f26c9b3`）。手動のマージ・クローズ操作は行っていない。

## CI（Engine conformance）

- PR #3 の run `41703481142`（pull_request）: success（候補段階で確認済み）。
- main push の run `41724411127`（push）: **success**。ジョブは native (ubuntu-latest) / native (windows-latest) / native (macos-latest) / wasm の全て success。
- develop push の run `41724419979`（push）: **success**（同4ジョブ success）。
- publish.yml の事前検証（verify-publication.mjs `prepare`）が要求する「対象 commit の push イベント成功 run」は main / develop の両 run で充足された。

## タグ

- 注釈付きタグ `v0.4.0`: tag object `b9112ab7348ade7711875b68d1a59f88e18206a9` → commit `f26c9b3`。メッセージは「Storm Lua Engine v0.4.0: Source Map schema 2・正確性修正群・コンパイル性能改善」（v0.3.1 の形式に倣った）。

## GitHub Release

Draft で作成し、資産 digest をローカルハッシュと照合してから公開した。

- URL: https://github.com/Stormcat-Works/storm-lua-engine/releases/tag/v0.4.0
- publishedAt: 2026-11-03T22:03:30Z、targetCommitish: main、draft / prerelease とも false。
- Release ノートは CHANGELOG の 0.4.0 節を用いた。
- 資産（GitHub 側 digest とローカル実測が一致）:

| 名前 | バイト数 | SHA-256 |
| --- | --- | --- |
| stormcat-works-storm-lua-engine-0.4.0.tgz | 1,909,527 | `c8c5116f98012db0c8e20c21c3a9e55f49fe3614ee2983cc329ed79f9920e651` |
| storm-lua-engine-playground-0.4.0.zip | 3,337,686 | `4f06e3c5330251e243a4590ab6b784b98fed6b63860882bbf8845a4502e57d14` |
| SHA256SUMS | 304 | `13aa8c122d51436f0b014f1934d98849876b0dc42a6fbca7324d426555577579` |
| release-verification.json | 2,499 | `1d139d6f46edf02bafba6c44e1ed5c6ceb3dcd27b65bf898209ca086d102d1ad` |
| publication-verification.json | 3,825 | `5425815a10230fb3fbc8fe04b0fe06044cd37bcd734e880144787efcf1858bb6`（公開後検証の完了後に追加） |

- release-verification.json は候補 JSON（`release-candidate-0.4.0.json`、SHA-256 `9f2d98ecddb171e642da5fa8a28c6a79a737393542c89e31e17e5abe60ff1160`）を基に v0.3.1 の receipt 形式で構成し、添付前に `tools/release/verify-publication.mjs` の検証関数（checksums / validateReceipt）で版・sourceRevision・tarball ハッシュの一致を確認した。SHA256SUMS は実際に添付する集合（receipt・Playground ZIP・npm tarball）に対して再生成した。

## npm 公開（publish.yml）

Release 公開で起動した run `41727360515`（release イベント、head_sha `f26c9b3`）の結果:

| ジョブ | 結果 |
| --- | --- |
| verify | success |
| publish | success |
| verify-registry | **failure** |
| 全体 | failure |

- verify-registry の失敗は、実行時点（22:04–22:05Z）で registry が新版を処理中で per-version メタデータが 404 を返し続け、検証ツールのリトライ窓（6回×5秒）を超えたことによる（assertion: `Version not found after publication`）。v0.3.1 の初回試行と同型の伝播遅延であり、publish ジョブ自体は成功している。
- その後の実測: packument に 0.4.0 が現れ、`dist-tags.latest` は `0.4.0`。`dist.integrity` は `sha512-pJ76vUWTTEDAoYYl5bsoEd6rM8w5fPqoIx0FDFlTbjZ6lqlg4eTlxPLR9HXnqU3y90bGOC3WkecE8Zbrb1PFqg==` で配布 tarball と一致する。attestations には SLSA provenance（`https://slsa.dev/provenance/v1`）が付いている。npm への公開は1回だけである。
- 同一ツールによるローカル再検証（公開後検証として実施）: `prepare` は success（対象 CI として develop push run `41724419979` を採用し、registry の既存版が同一バイトであることを確認）、`verify-registry` は success（registry から再取得した tarball がバイト同一、latest 0.4.0）、consumer テスト（`tools/test-package.mjs`）は Release tarball・registry tarball の双方で exit 0。
- **残件**: GitHub 上の publish.yml run `41727360515` は failure 表示のままである。再実行（workflow_dispatch、tag=v0.4.0）すれば既存の同一バイトを検出して publish はスキップされ verify-registry のみが再検証される見込みだが、本工程では再実行しておらず、親判断事項として残す。

## Playground 配備

- release ブランチの push により Cloudflare Workers Builds の配備が走る構成である。本番 `https://www.makkii.jp/tools/stormworks/storm-lua-engine/version.json` は `{"version":"0.4.0","revision":"f26c9b3d8f157948d1a28f5107213294a1ea82c4"}` を返し、配布 ZIP 内の version.json と一致した。
- Workers Builds のダッシュボード自体はリリースホストから観測できない。なお本番 index のバンドルハッシュはローカルビルドの ZIP と異なっていた（version.json は一致）。

## 記録・証跡

- 実施ログはリリースホスト `~/sle-release/v0.4.0/logs/` の `publish-*20261104.log` に保存した。公開後検証の作業物は `~/sle-release/v0.4.0/verify-work/` に残した。
- ガイド（docs.makkii.jp）の版対応確認は本工程の対象外である。
