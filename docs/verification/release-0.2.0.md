# Storm Lua Engine v0.2.0 公開・配備確認

2026-09-27。SDK、Playground、利用ガイド、初回版・次版の2記事を公開しました。ローカル候補の検証と実際の公開を分離し、公開後のregistry導入と本番URLでの操作を確認しています。[機械記録](release-0.2.0.json)にcommit、配布物hash、CI、URLを記録します。

## SDKと公開版の固定

Gitタグ`v0.2.0`とnpmの`@stormcat-works/storm-lua-engine@0.2.0`は`22592de08cfaadfc5696da001add1634149ba2cc`を基点とします。公開した版・タグを後から差し替えていません。GitHub Releaseには検査した同じSDK tarball、Playgroundの静的ZIP、release-verification.json、SHA256SUMSを添付しました。公開後にダウンロードし直してhashを確認しました。

npmのlatestは0.2.0です。空のcacheと隔離ディレクトリから公開registryを使って導入し、Vehicle/Addon、runtime/raster/compiler、require、mapped debuggingのconsumerを実行しました。registryのtarballは事前検査したものとbyte-identicalです。

[SDK Release](https://github.com/Stormcat-Works/storm-lua-engine/releases/tag/v0.2.0)。GitHub Actionsの[SDK公開前CI](https://github.com/Stormcat-Works/storm-lua-engine/actions/runs/36296691649)と[配信修正CI](https://github.com/Stormcat-Works/storm-lua-engine/actions/runs/36298165552)は、Linux/Windows/macOS NativeおよびWASM・ブラウザを含めて成功しました。

## Cloudflareの自動配備

Worker名は`storm-lua-engine-playground`、本番routeは`www.makkii.jp/tools/stormworks/storm-lua-engine/*`です。既存のPhysics Codegen／Codec CompilerのWorkers Builds構成を確認し、新規のGitHub接続とrelease専用トリガーを設定しました。main/develop/作業ブランチではこのWorkerを配備しません。

Cloudflareのroot directoryは`/app`、buildは`bash scripts/cloudflare-build.sh`、deployは`npm run deploy`です。Node 22.22.1、Rust 1.97.1、Emscripten 6.0.6、wasm-pack 0.13.1から同じcheckoutのSDKをビルドし、SDK/appテストと成果物検査後に配備します。依存の自動導入を停止し、SDK生成後にappのローカル依存を取り込む順序を明示します。設定と公開処理はapp/内です。

releaseへの実pushで、`22592de`の初回自動配備が成功しました。その後のCSP配信修正`48bff8fa2ac3a582d606762f0f6d51e8b56005da`も、CI確認後にreleaseへfast-forwardし、ソースビルド・テスト・自動配備の成功を確認しました。最新のWeb配備commitはversion.jsonで確認できます。Web配信修正は公開済みSDKパッケージの差し替えではありません。

## 公開時に見つけた問題と対応

1. 新規GitHub runnerのnpmキャッシュにはtarballがあってもregistry metadataがなく、Source Map consumerのoffline installが失敗しました。明示的なlockfileでnpm ciする形に修正し、空cacheからのci→隔離consumerとremote CIで確認しました。SDKのruntime依存は増やしていません。
2. ゾーン全体のZaraz/RUM自動挿入が、Playgroundの厳密なCSPへ衝突しました。Playgroundのhost/pathだけを自動解析挿入から除外しています。他のツール・ブログの設定には適用しません。
3. Bot用JavaScript Detectionsのinline初期化も同じCSPへ衝突しました。Bot保護を無効化したりunsafe-inlineを許可したりせず、HTMLへリクエストごとのnonceを追加する配信処理で対応しました。HTMLはno-store、SDK資産は静的配信です。Luaのserver-side実行は追加していません。

## 本番の実操作確認

[Playground](https://www.makkii.jp/tools/stormworks/storm-lua-engine/)でChromium/Firefox/WebKitの各13確認例、計39実行が成功しました。実際のモニター、リロードでの入力・結果保持、390×844のviewportでの横はみ出しなし、CSP、console errorなしを確認しています。初期環境を作った独立ブラウザcontextを使用しました。

本番配信のruntime/raster/compilerの3WASMは、公開SDKの対応物とbyte-identicalで、application/wasmで配信されています。アプリのネットワーク要求は静的GETのみです。Cloudflare自身のBot検出通信は別に分類し、Luaソースを送るアプリAPIとして扱っていません。

デスクトップ/モバイルのスクリーンショットを直接確認しました。デバッグ・描画・コンパイル処理をサーバーへ置き換えることなく、公開URLからSDKを操作できる状態です。保存・中断等の詳細なローカル検証は[候補記録](release-candidate-0.2.0.md)と[Playground検証](playground-20260926.md)も参照してください。

## ガイドとブログ

利用ガイドはdocs.makkii.jpのstorm-lua-engine配下へ公開し、11ページの200応答と0.2.0向け内容を確認しました。Mintlifyの本番deployment checkも成功しています。仕様・設計・検証・実行コードは本リポジトリへ残します。

- [v0.1.0公開記事](https://blog.makkii.jp/posts/storm-lua-engine-v0-1)
- [v0.2.0紹介記事](https://blog.makkii.jp/posts/storm-lua-engine-v0-2)

2記事だけdraftを解除して公開し、HTMLとfeedへの掲載を確認しました。初回版記事はv0.1.0に含まれない機能を混ぜず、次版とは分けています。無関係の下書きは公開していません。

## 下流と残る境界

Storm Minの専用統合ブランチを、公開タグの対象commitへ固定してpushしました。CLI/Web、製品版番号、既存の公開routeは変更していません。独立JS回帰25件を再実行して成功しました。

consumer側のremote CIは未実行です。ワークフロー構文はactionlintで検査しましたが、未実行のremoteテストを成功と表現しません。これは成功済みの公開Engine CIとは別の検証範囲です。

最適化後Source Mapはv0.2.5/v0.3.0、Addonコンパイラや全consumerの移行は後続工程です。実ゲーム全版・全入力の互換性を証明したという意味ではありません。
