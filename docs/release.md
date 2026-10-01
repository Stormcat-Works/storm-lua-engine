# リリース手順

リリースは管理者の明示指示で行います。GitHub CIは検証を担当し、通常の開発pushからregistryへ公開しません。検査済みGitHub Releaseの公開後、[publish.yml](design/publication-workflow.md)がOIDCで同じtarballをnpmへ自動公開・再検証します。Cloudflare Workers BuildsはreleaseブランチへのpushだけでPlaygroundを更新します。RustクレートはGitタグ、JavaScript／TypeScript SDKはnpm、ビルド済み配布物はGitHub Releasesで提供します。

## v0.3.0の公開前準備

v0.3.0のP0〜P5と、SDK/Playground/CLIの検証・梱包は[P5完成記録](verification/p5-playground-20261001.md)に集約した。 identity/選択の最終修正後の検査済みrevisionとtarball/ZIPは[仕上げ記録](verification/p5-final-selection-20261001.md)を使う。Source Map v3＋x_storm schema1、producerと指紋、内部candidate ABI、低レベルNodeArena、PlaygroundのIndexedDB/workspace形式を非互換棚卸しに含める。現在のnpm/GitHub/本番配備は0.2.1で、0.3.0のタグや公開はまだ実施していない。

公開を実施する際は、検査済みcommitとtarball/ZIPを固定し、CHANGELOG日付・タグ・GitHub Release/npm OIDC・releaseブランチの配備・公開ガイドを同じ版で揃える。詳細の性能改善は保留されており、4秒を理由付きmap生成の保証値として案内しない。古い端末の保存状態を暗黙変換しない旨も公開ノートへ含める。

以下の0.2.0/0.2.1固有範囲は過去版の条件であり、今回の完成状態とは区別する。

## v0.2.0の完成範囲

v0.2.0は既存runtime/raster/debug、Vehicle Compiler SDK、game/extendedとbindings、開発require、Vehicle load履歴、Playgroundを一つの版として仕上げます。ゲーム向けのmulti-fileは`build({minify:false,environment:"game"})`で単一ソースにして実行し、LB式の実行時includeはextendedの`requireLoader`を維持します。両者の意味を自動推測しません。

Source Mapは**非短縮リンク結果の行単位対応だけ**を今回の正式範囲とします。元ファイルのbreakpoint・停止行・runtime errorへの接続を実行例と回帰で確認します。最適化後マップはv0.2.5またはv0.3.0の別改修であり、現在のminify結果へ古いmapを付けません。元変数の復元や新しいデバッガ抽象は今回の条件に含めません。

ローカル完成確認は[TASKS](../TASKS.md)の5項目と[候補記録](verification/release-candidate-0.2.0.md)。この段階でpush・CI実行・タグ・公開・本番配備は行いません。公開時のCIと正式配布物確定は、以下の別工程です。

## 版と契約の確認

現在の対象版は**0.2.1**です。target未達時の最大探索との同一出力、重複探索排除、詳細な非短縮map、ログ位置・ホスト操作・LifeBoatビルドを含みます。最適化後minify mapは**0.3.0**の別工程です。公開実行前に[0.2.1確認記録](verification/release-0.2.1.md)とCHANGELOGを確認します。

Cargo workspaceと`packages/lua-engine/package.json`の版を揃え、`CHANGELOG.md`へ利用者に影響する変更を書きます。タグは`v<version>`とします。公開済みのタグやnpmの同じ版を差し替えず、修正は新しい版として出します。

公開API、WASM ABI、描画命令、savedata/checkpoint、Playgroundのproject形式に非互換変更があるか確認します。形式を変える場合は版とreject条件、往復テストを同時に更新し、予定している非互換変更を分散したリリースへ持ち越しません。0.2.0でも既存savedata/project形式、描画命令ABI、Composite I/Oレイアウトの維持を照合します。環境の既定値やload/resetの挙動変更はCHANGELOGで明示します。

## 検証とビルド

[CONTRIBUTING](../CONTRIBUTING.md)のNative、型、Python、WASM、ブラウザのゲートを実行します。固定toolchainを用意し、`python3 tools/generate-toolchain-licenses.py --check`で配布通知を照合します。toolchain更新時は先に同じコマンドの`--check`なしで再生成し、原文とmanifestの変更をレビューします。

`node tools/build-wasm.mjs --with-tests`と`node tools/test-wasm.mjs --with-tests`では、製品WASMだけでなく独立したLua backend probeも実行します。`node tools/test-browser.mjs`では3エンジンで直接描画・実Lua描画・ホスト機能を検査します。

Playgroundは`npm --prefix app ci`でローカルSDK依存を更新した後、`npm --prefix app run build`、`npm --prefix app test`を実行します。全ブラウザの操作試験は`npm --prefix app run test:browser`です。

画面のないLinuxランナーでも、Playgroundはheadlessブラウザ試験を`npm --prefix app run test:browser`で実行します。ブラウザ依存はPlaywrightの`install --with-deps`で用意します。

`node tools/check-package.mjs`はJS export、WASM、必須の通知、公開先設定を確認します。`npm pack`のprepackにも組み込まれており、TypeScriptだけをビルドした不完全な配布物を拒否します。パス検査は`node tools/check-artifacts.mjs packages/lua-engine/dist app/dist`で実行します。

## 同じ成果物を検査して公開する

`packages/lua-engine/`で`npm pack --json --pack-destination <出力先>`を実行し、生成したtarballを`node tools/test-package.mjs <tarballのパス>`で検査します。独立環境へoffline installし、Lua・描画・公開exportとドキュメントのconsumer例を実行します。引数なしの場合は検査用tarballを一時生成します。

GitHub Releasesには検査したnpm tarball、Playgroundの静的サイトZIP、`SHA256SUMS`を添付します。静的サイトZIPには`dist/`の内容と必要な権利表示を含めます。個人パス、調査資料、内部履歴のバックアップ、node_modules、Cargo target、テスト専用WASMは配布しません。Rust用ソースはタグから取得します。

公開するcommitのCI成功を確認し、同じcommitへタグを付けます。自動公開は[公開workflow](design/publication-workflow.md)を使用します。認証済み管理者による手動の復旧操作が必要な場合に限り、npmは検査済みtarballを`npm publish <tarballのパス> --access public --tag latest --registry=https://registry.npmjs.org/ --ignore-scripts`で公開します。認証や二要素認証が必要な場合は管理者の認証手順を使い、トークンをコード、ログ、チャットへ記録しません。

公開後、registryのversion・dist-tag・integrityとGitHub Releaseのassetを確認し、registryから新しくインストールしたconsumerを実行します。dry-run、タグ作成、tarballの添付だけでnpm公開済みとは扱いません。

## ブランチ

`main`は公開する版、`develop`は次の変更、`release`はPlaygroundの本番配備対象を管理します。`release`はCI確認済みcommitへfast-forwardし、Cloudflare Workers Buildsでそのcheckoutをビルド・配備します。設定と手順は[app guide](../app/README.md)のrelease節を参照してください。初回公開ではレビュー済みtreeを親なしの1commitにまとめ、そのcommitから`develop`を作成します。公開対象でない開発履歴の復旧用bundleはローカルだけに保存し、公開refやReleaseには含めません。以後の通常リリースで初期化や履歴の作り直しを繰り返しません。

## Compiler assets on the integration branch

A release containing the compiler subpath must additionally run `node tools/build-compiler.mjs`, `npm --prefix packages/lua-engine run test:compiler` and `node tools/test-compiler-browser.mjs`. Build compiler assets before the full package gate. The isolated installed consumer exercises both compiler and runtime together, including property snapshot preservation. The expanded SDK is prepared as 0.2.1; publishing still requires an explicit release action.

## Playgroundの配布

`app/`にはCLI/Webと公開用Worker設定を置きます。`npm --prefix app run build`は`app/dist`と専用route向け`app/dist-site`を生成します。`npm --prefix app run deploy:dry-run`は梱包検査であり、本番配備ではありません。Storm Minの製品・Worker・routeは維持します。

Addon Labの現在のソース・CI参照は撤去済みです。過去版ReleaseのLab資産は書き換えません。配布物のライセンスとsource/WASM版の一致を確認し、実配備は明示的に許可したreleaseブランチへのpushで実施します。

## 環境契約変更の公開ゲート

環境プロファイル対応版は、既定のpcall/error/print等、onLogの副作用、コンパイラ診断、外部名と_ENVの扱いを変更します。新しい版で公開し、0.1.0を差し替えません。gameとextendedの同じ条件でNative/WASMを検査し、compiler-workerとホストbindingsを梱包済みconsumerから実行します。既存利用者にはextendedの明示指定と、必要なcompiler側hostBindingsの指定を案内します。

## 公開資料の正本

利用ガイドはdocs.makkii.jpのstorm-lua-engine配下に移しました。本リポジトリのdocs/guideは移設先だけを示し、本文を複製しません。契約・設計・検証・リリース手順とconsumerコードは本リポで検査します。

公開時はSDKパッケージ、ガイド、Playgroundを対応版で配備し、ブログ下書きのdraft解除はその公開状態を確認してから行います。npm publish・Worker配備・ブログ公開を、通常のコードcommitと同一視しません。
