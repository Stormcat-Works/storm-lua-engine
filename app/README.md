# Storm Lua Engine: Playground

SDKの解析・ビルド・最適化・実行・描画・デバッグ・ホスト接続を実際に試すCLI/Webです。Storm MinのCLI/Webとは併存し、置き換えません。ゲーム世界、仮物理、共同編集、クラウド保存は実装しません。

Playgroundはv0.2.1を公開しています。SDKのnpm registryとGitHub配布も0.2.1で、公開後の再取得・導入検証まで完了しています。[公開状況](../docs/verification/release-0.2.1.md)。[Webを開く](https://www.makkii.jp/tools/stormworks/storm-lua-engine/)。SDKのnpm版とGitタグは固定し、Webの配信設定はreleaseブランチから更新します。

## 準備と起動

リポジトリルートで`npm --prefix packages/lua-engine ci`、`node tools/build-wasm.mjs --with-tests`、`node tools/build-compiler.mjs`、`npm --prefix packages/lua-engine run build`を実行し、同じ版のSDKをビルドします。Rust/EmscriptenとCargo出力先は[配布ビルド](../docs/design/distribution.md)に従います。

続いて`npm --prefix app ci`を実行します。SDKのファイルを更新した場合も、このコマンドでローカルSDK依存を更新してください。

Webは`npm --prefix app run dev`（127.0.0.1:5178）で開きます。配布物は`npm --prefix app run build`で作り、`npm --prefix app run preview`（127.0.0.1:4178）で確認できます。

CLIのヘルプは`npm --prefix app run cli -- --help`、確認例の一覧は`npm --prefix app run cli -- --list`、実行は`npm --prefix app run cli -- --recipe vehicle`です。`--project FILE`はWebで書き出したJSONを読み込みます。`--jsonl`は同じSDKセッションへ1行ずつ操作JSONを渡します。`minify FILE`、`run FILE`にも対応し、拡張環境には`--extended`を明示します。

## 利用者向けの正本

通常の操作手順は[docs.makkii.jp](https://docs.makkii.jp/storm-lua-engine/playground)に移設しています。以下はアプリの開発・検証に必要な操作概要です。

## 使い方

左の確認例を選び、Luaソース・環境・操作列を確認して「最初から実行」を押します。「1操作進める」は同じセッションを保持しながら1件ずつ操作します。「中断」はWorker自体を終了し、次回は新しいセッションで始めます。

解析・ビルド・最適化はLuaを実行しません。`load`はトップレベルを実行します。VMとrasterの作成も明示操作です。ページを開いたり入力を編集したりしても自動実行しません。

「SDK操作列」ではJSONの入力を編集できます。`{"$source":true}`と`{"$environment":true}`は画面の入力を参照します。`as`で操作結果に名前を付け、`{"$ref":"compiled.code"}`で生成物を次の操作へ渡します。未知の操作、結果の不正参照、未実装APIはエラーとして表示します。

### 確認例

| 例 | 確認する機能 |
| --- | --- |
| Source | requireLoader、読み込み先ブレークポイント、前置き/本体/後置きのreset |
| Vehicle | Number/Boolean I/O、プロパティ、複数draw、reset、ログ |
| Compiler | analyze、非短縮リンク/map、minify、パス一覧、property走査 |
| 動的_ENV | 動的値・関数参照の名前保持、字句短縮、描画 |
| Game/Extended | script-visible関数、debug.logとprint、pcall、明示環境 |
| ホスト拡張 | 値・関数の追加、math.absの置換、bindingPaths、reset時の再適用 |
| Addon | load/start/tick/event/destroy、server、menu property、savedata/reload |
| Debugger | breakpoints、stack/locals/upvalues、table展開、watch、step/resume |
| Raster | Luaなしの描画、全基本DrawCommand、バイナリ命令列、所有frameとCanvas表示 |
| ホストサービス | 明示地図provider、palette、HTTP drain/reply/cancel、ログ |
| 値と保存 | i64、raw bytes、NaN/負のゼロ、matrix、保存codec、Lua値ヘルパー |
| エラー | 非対応関数、Addonコンパイラ、命令予算、未提供ホストの失敗 |
| APIカタログ | Vehicle/Addon/環境の公開カタログとイベント一覧 |

これはSDKの通常の公開機能を操作するための面です。低レベルRustのmlua escape hatchや、すべてのFFI不正ポインタをWeb入力として公開するものではありません。SDK内部の適合性・セキュリティ境界試験はconformanceが担当します。新しい公開機能を追加するときは、対応する操作・確認例・実テストを同時に見直します。

## ホストを明示する

Addonの`server`や任意の`bindings.functions`には、固定の戻り値を返す`return`、引数を返す`echo`、数値を加算する`sum`、失敗する`fail`という小さなテストホストを指定できます。これは実ゲームのAPI実装ではありません。未指定のAPIを成功値で埋めません。

`mapFixture`は指定RGBAの画像を返すテストproviderです。HTTPは要求を表示して、操作列から手入力の返信またはcancelを配送します。実ネットワークへ要求を送信するプロキシはありません。

## 保存と失敗

ソース、環境、操作列、選択中の例、タブ、直近64操作の結果、最新モニターを同じ端末の保存レコードに保持します。リロード後も入力と結果を復元しますが、Lua VMの継続を保存したとは扱いません。再実行は明示操作です。

version付きJSONのexport/importはソース・環境・操作列を持ち運びます。未知のversion、旧Addon Lab形式、不正JSONを読み込んでも現在の入力を置き換えません。ブラウザ内の保存が壊れていた場合も元データを上書きせず、救出操作と有効なJSONのimportを提示します。自動で旧形式を変換しません。

保存に失敗した場合はエラーを表示します。APIキー等の秘密をこのアプリへ保持する機能はありません。Luaコードの公開共有サービスでもありません。

## 検証と配備

`npm --prefix app test`は全確認例を実際のSDKで実行します。`npm --prefix app run test:browser`はChromium/Firefox/WebKitで、全確認例、モバイル表示、保存復元、import/export、実行中断、compiler-only/raster-onlyのWASMロードを確認します。ブラウザは`npm --prefix app exec playwright install chromium firefox webkit`等で用意してください。

静的配備用は`app/dist-site/tools/stormworks/storm-lua-engine/`へ出力します。makkii.jp向けWorker設定は`app/wrangler.jsonc`に置き、Storm Minのrouteを変更しません。`npm --prefix app run deploy:dry-run`は検査だけです。実際の`deploy`は管理者の明示指示で行います。

配布WebにはSDKの権利表示を含めます。アプリのVite等の開発依存はSDKのnpm依存へ追加しません。ブラウザWorker、公開用Cloudflare Worker、SDKの意味論は別の責務です。

## releaseブランチからの自動配備

Cloudflare Workers Buildsの本番トリガーは`release`ブランチだけを対象にします。リポジトリはStormcat-Works/storm-lua-engine、root directoryは`/app`、build commandは`bash scripts/cloudflare-build.sh`、deploy commandは`npm run deploy`です。

ビルド環境変数は`SKIP_DEPENDENCY_INSTALL=1`と`NODE_VERSION=22.22.1`です。SDKを生成してからappのローカル依存を取り込む必要があるため、依存の自動インストールを無効にし、スクリプトが順番を管理します。固定Rust/Emscripten/wasm-packで同じcheckoutのSDKを作り、SDK検証、app build/test、成果物検査後に配備します。

`main`/`develop`/作業ブランチのpushではこのWorkerを配備しません。公開するコードのCI成功を確認してから`release`へfast-forwardします。`version.json`でSDK版と配備したGit SHAを確認できます。npm公開はこのトリガーには含めず、検査済みtarballから別に行います。

## 本番のCSPとCloudflare

`worker.ts`はHTMLレスポンスのCSPへ毎回新しいnonceを追加する配信処理だけを行います。CloudflareのJavaScript Detectionsはこのnonceで動作し、Bot対策を停止したり、`unsafe-inline`を許可したりしません。HTMLはnonce再利用を防ぐためno-store、JS/WASM等は既存の静的配信です。Luaやコンパイラをサーバーで実行するAPIはありません。

ゾーンのConfiguration Ruleで、`www.makkii.jp/tools/stormworks/storm-lua-engine/`だけをZarazおよびRUMの自動挿入対象から除外しています。CSPの`connect-src 'self'`を緩めず、他のサイトやツールの解析設定は変更しません。このルールはゾーン設定であり、Workerの再配備とは独立に維持します。
