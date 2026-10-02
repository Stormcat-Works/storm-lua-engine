# Storm Lua Engine v0.3.0 公開完了と利用確認

2026-10-02。**v0.3.0をnpm・GitHub Release・タグ・Playgroundへ公開し、registryからの再取得・独立導入、公開サイトの3ブラウザー、公開ガイドの更新まで完了した。** npmのlatestは0.3.0。Source Mapに関するSDKのJavaScript・型・WASM・Workerは同じ版へ揃えている。

[GitHub Release](https://github.com/Stormcat-Works/storm-lua-engine/releases/tag/v0.3.0) / [公開前CI](https://github.com/Stormcat-Works/storm-lua-engine/actions/runs/36946189312) / [npm公開workflow](https://github.com/Stormcat-Works/storm-lua-engine/actions/runs/36947042993) / [Playground](https://www.makkii.jp/tools/stormworks/storm-lua-engine/) / [利用ガイド](https://docs.makkii.jp/storm-lua-engine/source-maps) / [機械記録](release-0.3.0.json)。

## 1. 公開したもの

対象commitは`1e5d2efc4e05af685348d0eb369ce6199f17128f`。v0.2.1公開後のmainの文書変更を統合し、0.3.0の変更点を確定した。既存v0.2.1のタグ・配布物は変更せず、mainとreleaseを新しい版へ進めた。

最適化後Source Map v3＋x_storm schema1、全67最適化パスの由来、型付きの関連元、copy、インライン文脈、理由・観測事実、除去/置換、通常/LifeBoatの元ファイル合成、Rust/WASM/TSとWorker、Playground/CLIの静的・実runtime接続を含む。追加の性能改善はv0.3.1以降とし、リリース準備で最適化結果やマップ精度は変更していない。

## 2. npmと配布物

導入は`npm install @stormcat-works/storm-lua-engine@0.3.0`。registryのversion/latestが両方0.3.0であること、通常のnpm installで導入できることを確認した。

SDK tarballのSHA-256は`27145ec5accc8c9f5c84261de71c77d096577a3564094396a7521c36808d2de4`。registryから再取得したbytesは、検査済みGitHub添付tarballと完全一致した。dist.integrityのSHA-512も一致した。公開者はGitHub Actions、Trusted Publisherはgithubとして記録され、npmのprovenance attestationも提供されている。map内のproducerとは別の配布証明である。

| 配布物 | SHA-256 |
| --- | --- |
| SDK tarball | `27145ec5accc8c9f5c84261de71c77d096577a3564094396a7521c36808d2de4` |
| Playground静的ZIP | `44485ef3cb2979aa910787185629ddf1b0b191239a4788e193620ea03f242e66` |

検査時点のrelease-verification.jsonとSHA256SUMSは固定したまま、公開後の結果は別添えのpublication-verification.jsonへ記録した。

### 自動公開の実行経過

release publishedイベントでworkflowを起動し、attempt1の事前検証とnpm publishは成功した。公開直後のversion取得は30秒間の確認期間に404となり、verify-registryだけ失敗した。後からversion/latestの取得を確認し、workflowを再実行した。

attempt2では同一内容がすでに公開されていることを検出してpublishをskipし、registry再取得・ハッシュ・独立consumerが成功した。既存0.3.0を上書き・再公開していない。最初の確認失敗を最終成功に隠していない。

## 3. SDKと利用側の接続

SDKは`compiler.minify(source,{sourceMap:true,sourceName:'controller.lua'})`で`code`と`map`を返す。両方を同じビルドの組で保存し、`compiler.validateSourceMap(code,map)`で検証済み`OptimizationMap`を取得する。`build`、`buildLifeboat`、CompilerWorkerClientも対応済み。低レベルの公開Rust APIとcompiler WASMも同じ仕様を使用する。

マップ本体はJSON文字列。標準Source Map v3を読む場合は、利用側でtrace-mappingなどのreaderを使用できる。詳細な理由・関係・文脈はSDKの検証結果から取得する。位置の名前・内容は標準sources/sourcesContentと対応し、details.sourcesは内容指紋の表である。

`examples/consumer/optimized-source-map.mjs`を追加し、空のNodeプロジェクトへregistry版SDKとtrace-mappingを通常導入して実行した。生成された`6`から、controller.luaの6行目、UTF-16列47（0始まり）の`2*3`へ戻り、定数評価理由・インライン文脈・古いmapの拒否を確認した。この例はtolerantの数値変換を明示し、exactでは同じ変換が選ばれるとは説明しない。

インストール済みSDKのTypeScript型でも、Compiler、CompileOptions、OptimizationMap、OptimizationReason、CompilerWorkerClient/serveCompilerが利用できることをstrict/NodeNextで検証した。既存の非短縮moduleのbreakpoint/step/error例と、最適化後の実VM出力もregistry版で成功した。

**SDK更新だけで他のエディタに表示が自動追加されるわけではない。** 利用アプリは、生成物の保存・選択位置をUTF-8 byteへ変換する処理、標準のUTF-16列、表示・逆引き・候補選択、実VMのチャンク名/実行世代と生成行の対応を接続する。生成行しか分からない場合は列を推測しない。元変数値・寿命・消えたframe・特定loop反復の復元は別機能である。

mapはゲームへ渡すLuaとは別であり、8192文字制限のソースへ埋め込む必要はない。原文全文と固定化した設定を含むので共有先を確認する。生成後のLuaを変更した場合は古いmapを使わない。

## 4. Playgroundとガイドの公開後確認

releaseブランチのpushから本番配備が反映された。Cloudflare deployments APIでも2026-10-02の新versionへの100%配備を確認した。version.jsonのversion=0.3.0、revision=`1e5d2efc4e05af685348d0eb369ce6199f17128f`を確認した。CSP・既存route・Bot対策を緩める変更はしていない。GitHub上のCloudflare checkは最終確認時にin_progressのままで、完了通知は未確認である。本番配備の確認と、この通知の状態を分離して記録する。

本番専用routeでChromium・Firefox・WebKitを使用し、各16例、Source Mapの双方向選択、理由とinline文脈、元module、identity/copy・Unicode、保存復元・workspace持ち運び、実VMのpause/step/log/errorを実行した。全ブラウザーでconsole/page errorは0件、初期画面でWASMを起動しないこと、compiler/rasterの独立ロード、中断後の再実行も確認した。desktop1440×1100とmobile390×844を検証した。

Browser pluginは利用できないため既存のPlaywrightを使用した。公開ページが意味のあるUIを表示し、エラーoverlayや横overflowがないことをスクリーンショットでも確認した。エラー確認例の意図したLua失敗は、ブラウザーの故障とは区別している。

公開ガイドはdocs-siteの`c125123918a1b46516888bc8482b5fe177af4882`で更新した。Mintlifyのstrict build validationとbroken-linksは成功し、本番のSource Mapページも0.3.0の生成・検証・標準reader・理由表示・Worker・実行精度の説明へ更新されたことを取得して確認した。Chromiumで実ページが表示され、console/page errorが0件であることも確認した。既存の0.3.0記事ドラフトは見つからず、新規ブログ記事はこの公開へ追加していない。

## 5. 検証と互換性

| 対象 | 今回の確認 |
| --- | --- |
| Native workspace | 764件成功 |
| SDK型/JavaScript | 30件成功 |
| 実WASM | 81件成功 |
| Playground/CLI | 38件成功 |
| Local/本番の3ブラウザー | 各16例とSource Map実操作を成功 |
| GitHub CI | 公開commitのLinux・Windows・macOS NativeとWASM全job成功 |
| 梱包済み/registry SDK | 同一bytesの独立consumer、追加の利用例、公開型を成功 |
| 静的ゲート | fmt、architecture、default、clippy、rustdoc、Python、権利表示、画面fixture、配布物検査を成功 |

低レベルAst.nodesのNodeArena、opaque candidateのJSON/binaryは同一compiler版を前提にする。Playground保存はIndexedDB state v2、portable workspace v1で、旧localStorageは救出可能な明示拒否とし自動変換しない。入力project JSON v1、描画命令ABI、Composite I/O、Addon savedata、既定のgame/extended契約は維持する。

代表240設定と全67パスの由来検証は直前の[性能・精度記録](source-map-performance-20261002.md)にも残る。リリース準備でcompiler実装は変更しておらず、今回は全workspaceと実WASM/配布物を再検証した。全収集Luaやすべてのブラウザー環境を検証済みとは表現しない。

## 6. 完了と後続

v0.3.0の公開工程は完了。さらなるmap生成時間・容量・validatorメモリの改善はv0.3.1以降。Storm Code/Editor/Min等のconsumer全体の導入・UI変更は各製品の作業であり、今回それらのソースや未コミット変更は更新していない。
