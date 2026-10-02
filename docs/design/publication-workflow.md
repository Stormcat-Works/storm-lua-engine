# npm公開ワークフロー

2026-09-30。配布先はnpmjs.comを維持し、GitHub Packagesへ移さない。通常の開発pushは公開しない。

## 起動と成果物

`.github/workflows/publish.yml`は、GitHub Releaseのpublishedイベント、またはmainからのworkflow_dispatch（tag=vX.Y.Z、dry_run任意）で実行する。安定版だけを対象にし、prerelease、fork、draft、mainの履歴にないcommitを拒否する。

Releaseには検証済みSDK tarball、SHA256SUMS、release-verification.jsonを添付してから公開する。ワークフローはタグの実commit、同commitの成功済みEngine conformance（Linux/Windows/macOS/WASM）、receiptのsourceRevision/version、packageのname/version/repository/registry、ハッシュを確認する。既存のtarballを再ビルドせず、そのまま使う。

読み取り専用のverifyジョブで同じ版の隔離consumerを実行し、1日保持のartifactで次のジョブへ渡す。publishジョブは転送後のdigestを再確認して、`npm publish`を`--ignore-scripts`付きで実行する。書き込み認証は短命OIDCのみで、NPM_TOKEN/NODE_AUTH_TOKENをSecretsへ保存しない。

公開後は読み取り専用の別ジョブでregistryのversionとintegrity、再取得したtarballのSHA-256を確認し、registryから取得したbytesへ隔離consumerを再実行する。既存版とintegrityが同じならpublishを省略し、異なる場合は失敗する。未公開の古い版をlatestへ戻すことは拒否する。

公開全体は一つのconcurrency groupで直列化し、進行中のregistry書き込みを新しいrunで中断しない。CIの旧revisionキャンセルとは別扱いである。

## 初回だけ必要なnpm側の登録

package設定のTrusted Publisherで、次を登録する。

| 項目 | 値 |
| --- | --- |
| Provider | GitHub Actions |
| Organization | Stormcat-Works |
| Repository | storm-lua-engine |
| Workflow filename | publish.yml |
| Environment | 未指定 |
| Allowed actions | 直接のnpm publishを許可する |

GitHub-hosted UbuntuとNode 24.19.0を使用する。npm Trusted Publishingはnpm>=11.5.1、Node>=22.14.0を要する。初回の登録にはnpm側のパッケージ管理権限・認証が必要で、ワークフロー自身が自分を信頼済みとして登録するものではない。

CLIで登録する場合は、管理者が認証した環境で`npm trust github @stormcat-works/storm-lua-engine --repo Stormcat-Works/storm-lua-engine --file publish.yml --allow-publish --yes`を使える。設定値は大文字小文字も一致させる。秘密をチャット、ログ、ソースへ書かない。

既存v0.2.1は古いタグに本workflowがないため、mainからtag=v0.2.1を指定してdispatchする。`dry_run:true`は検証のみで、OIDC公開の成功を証明するものではない。認証が未設定なら実publishは失敗し、成功扱いしない。

## npmのprovenance attestationについて

Luaのソースマップとは異なる、配布物の出所証明である。releaseイベントのcommitとソースcommitが同じ場合はnpmのprovenanceを有効にする。過去タグのbootstrapのようにworkflowのcommitと配布物のsourceRevisionが異なる場合は、今日のcommitを元ソースとして誤って証明しないため、そのrunだけprovenanceを無効にする。OIDC認証、タグ/CI/ハッシュ検査は省略しない。既存の検査済みtarballも書き換えない。

## CI起動の節約

Engineの通常pushはmain/developだけ、作業branchはPRまたは明示dispatchで検証する。tag/releaseへのfast-forwardで同一commitの全CIを重複実行しない。PRのmerge結果に対する検証は残す。同一PR/branchの旧runはconcurrencyでキャンセルする。3 OSとWASMのrelease gateは維持する。

公開標準runnerの利用時間と、組織の非公開リポジトリの無料枠消費は別である。節約のためにprivateなソースやfixtureを公開リポジトリへ移したり、課金枠を増やしたりしない。

## 検証

`node --test tools/release/*.test.mjs`で不正tag、archive path、checksum重複、receipt/registry不一致、欠けたCI成功、latestの逆行を検査する。workflow YAMLはactionlintで確認する。実際の公開/registry再導入の状態はrelease verificationへ記録する。

参考: [npm Trusted Publishing](https://docs.npmjs.com/trusted-publishers/)、[npm trust](https://docs.npmjs.com/cli/v12/commands/npm-trust/)、[Actions billing](https://docs.github.com/en/billing/concepts/product-billing/github-actions)。


## 実行結果

[2026-09-30の検証記録](../verification/publication-workflow-20260930.md)に、Hosted dry-runの成功、実際のnpm認証エラー、registry確認スクリプトの試験を分離して記録する。
