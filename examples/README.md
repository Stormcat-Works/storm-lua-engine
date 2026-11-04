# Embedding examples

## Playground

SDK全体の対話的な確認には[Storm Lua Engine: Playground](../app/README.md)を利用してください。Addon Labのホスト・イベント・保存・エラー確認はappにまとめました。旧Labと仮物理・3Dワールドは削除しました。以下の小さな組み込み例は維持しています。

## Native

`cargo run -p storm-lua-conformance --example native-foundation --locked`は数値境界の小さな例です。`cargo run -p storm-lua-conformance --example microcontroller --locked`では実際のload/tick/drawを実行し、CPU framebufferを使います。`--example differential`は最適化前後の同じI/O/画素比較を行う。

例はtest-only memberへ配置し、製品側へexamples/テスト用依存を混入させない。

## Browser and Worker

TSとWASMをbuildした後、リポジトリルートから`python3 -m http.server 8080`で配信し、[browser example](browser/index.html)を開く。Runボタンで明示的にsourceをloadし、1tick/1drawを実行する。CanvasPresenterはraw RGBAをImageDataへcopyする。

[module Worker example](browser/runtime-worker.js)は1jobごとにVMを生成し、owned frame copyだけをtransferする。Workerの生成/終了、job schedulingはアプリ側が所有する。`node tools/test-browser.mjs`は3ブラウザそれぞれで2 Workerの独立実行も検証する。

## Performance

`cargo run --release -p storm-lua-conformance --example benchmark --locked`と`node tools/bench-wasm.mjs`は同じLua fixtureを測定します。測定範囲と注意点は[testing/performance](../docs/design/testing-performance.md)。

## 利用アプリ側から実行する例

[Node consumer](consumer/node.mjs)は公開package名だけをimportし、vehicle制御・ホスト地図・Addon lifecycle/server・ログ・HTTP・checkpoint roundtripを実行します。ローカルでpacked packageをインストールしたアプリにこのファイルをコピーして実行します。`tools/test-package.mjs`はその手順を独立したofflineインストール環境で自動検証します。

[Native addon host](../conformance/examples/addon_host.rs)はRustのserver callback、ログ、保存・復元、map providerを示す。`cargo run -p storm-lua-conformance --example addon_host --locked`で実行できる。conformanceは例の起動用targetであり、製品側から依存するcrateではない。

例に含まれる地図・プレイヤー・HTTP replyはテストホストが明示的に用意したデータであり、ゲーム環境が無い場合のengine fallbackではありません。詳細は[consumer guides](../docs/guide/getting-started.md)。

## 開発用の名前付きソース

[source-loading.mjs](consumer/source-loading.mjs)はインストール済みSDKだけを利用し、require先のブレークポイントと前置き・本体・後置きのresetを確認します。`node tools/test-package.mjs`も同じ例を隔離consumerで実行します。利用手順の参照先は[ソースガイド](https://docs.makkii.jp/storm-lua-engine/source-loading)です。

## 非短縮ビルドのソースマップ

`consumer/source-map.mjs`は、インストールしたSDKと`@jridgewell/trace-mapping`を使い、game環境でbreakpoint・step・caller・runtime errorの位置を元ファイルとの間で相互に対応付けます。対応したSDKとtrace-mappingを利用先へ導入して実行してください。SDKはこのconsumerを通常のパッケージ検査でも実行します。[利用ガイド](https://docs.makkii.jp/storm-lua-engine/source-maps)。
