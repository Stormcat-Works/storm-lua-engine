# Storm Lua Engine v0.2.1 公開状況と検証

2026-09-30。**v0.2.1はnpm・GitHub Release・Gitタグ・配布物・Playgroundへの公開と、registryからの再取得・導入確認まで完了。** npmのlatestは0.2.1。[公開workflow](publication-workflow-20260930.md)のTrusted Publishingを実際に使用した。最適化後ソースマップはv0.3.0の別開発である。

[GitHub Release](https://github.com/Stormcat-Works/storm-lua-engine/releases/tag/v0.2.1) / [公開前CI](https://github.com/Stormcat-Works/storm-lua-engine/actions/runs/36650424343) / [Playground](https://www.makkii.jp/tools/stormworks/storm-lua-engine/) / [機械記録](release-0.2.1.json)

## 1. 目標未達時の文字数・生成コード

**同じ入力と意味論・最適化設定に対して、目標未達で完了した結果は、targetなしの最大探索と文字数も生成Luaも一致する。** 比較する最大探索は`searchMode:exhaustive`、target指定なしである。numeric mode、pass設定、property値、環境、出力形式を同一にする。

公開前に追加の抜けを修正した。従来の実装はtarget指定時にもfast/beamで候補を限定し得たため、target探索では最大探索の候補集合を保持するよう統一した。評価済み候補は継続contextへ保存し、Workerで再実行しない。canonicalなsize/orderの選択規則は維持する。

目標を達成して早期終了した結果、またはtargetを指定せず明示的にfastを選んだ結果まで、最大短縮との一致を要求するものではない。内部テスト用checkpointによる途中停止も、完了した未達結果とは区別する。wall-clockによる探索打ち切りは存在せず、今回も追加していない。

### 検証範囲

代表30入力に対し、safe/smallest、exact/tolerant、改行設定2種の240組を最大探索の基準にした。それぞれtarget=0のexhaustive、target=0のfast/beam1、最大探索の出力より1文字小さいtargetでfast/beam4を比較した。

SDK配布物と本番compiler WASMのそれぞれで、**716件の未達比較すべてが生成Lua、UTF-16文字数、候補集合で一致**した。別の4件は原文候補で実際に目標達成しており、上限以下であることを確認し、未達の件数へ含めていない。SDKと本番の240基準出力もhash一致した。

通常のRust/WASM回帰には、beam1/4/16、property固定化、実際のWorker継続・逆順完了・シリアライズ往復・候補重複なしの試験を含む。

## 2. 含まれる変更と含まれないもの

P0/P1（共通のtarget探索・詳細な非短縮map）、ログ位置、Vehicleの名前付きcallbackとホスト操作、構造化source inspection、LifeBoatビルド、可変長引数のみのfunction構文修正を含む。

**最適化後minify mapは含まない。** その作業はv0.3.0に確定した。独立したdevelopブランチの描画修正は今回の候補へ混ぜていない。

一般的なTS成果物型、Source Map v3、savedata、描画命令ABI、Composite I/O形式は維持する。低レベルRustのLinkedRange初期化にはbyte範囲が必要。Workerのopaque contextは同じcompiler版の一時データとして使用する。

## 3. 固定した版と配布物

タグ`v0.2.1`は`5724cb054816dd330f73fd5a114be967b6c96095`を指す。mainとreleaseをその検証済みcommitへfast-forwardした。公開済みv0.2.0のタグ・npmパッケージは変更していない。

GitHub Releaseには検査した同じSDK tarball、Playgroundの静的ZIP、release-verification.json、SHA256SUMSを添付した。公開後に全添付物を再ダウンロードし、チェックサム検証が成功した。

SDK tarball: `stormcat-works-storm-lua-engine-0.2.1.tgz`、SHA-256 `3826946bbe2117bf7cc4d2ea7745f20ecaffb0927304af6540bd781f7354ad19`。

Playground ZIP: `storm-lua-engine-playground-0.2.1.zip`、SHA-256 `13a8d4002185a91eb196f1ceeba3aa5dab4ad50e65153550869cc5ce23bfd3f0`。

添付tarballは隔離環境へ導入し、Lua実行、描画、compiler、property保持、source loader、ログ位置、マップ付きdebuggerと実行エラーを確認した。この検査済みtarballをそのままnpmへ公開し、registryから取得したbytesもSHA-256とintegrityが一致した。

## 4. 実行したゲート

| 対象 | 結果 |
| --- | --- |
| GitHub CI | 上記commitのLinux/Windows/macOS Native、WASM、3ブラウザ、Playground、隔離consumerが成功 |
| ローカルNative | 513件成功、失敗なし |
| 実WASM全体 | 69件成功。別途compiler関連17件も再実行（件数は重複するため加算しない） |
| SDK | 型consumerと30件成功 |
| Playground | 17件と3ブラウザの13確認例・保存・中断・モバイル・独立WASMロードが成功 |
| 静的ゲート | fmt、workspace check、clippy、rustdoc、architecture、Python、権利表示検査が成功 |
| 検査したtarballの独立導入 | 成功 |

初回のローカルSDK依存不足と、以前の検証用dist symlinkによるnpm梱包漏れは環境を修正して再実行した。release用distは独立した実ファイルである。既存primary checkoutの生成runtimeも同checkoutのソースから元のhashへ戻し、ソース変更を残していない。これらの失敗を成功として集計していない。

## 5. Playground本番配備

release pushによるCloudflare Workers Buildsが成功し、version.jsonでversion=0.2.1、revision=5724cb054816dd330f73fd5a114be967b6c96095を確認した。本番URLでChromium/Firefox/WebKitの各13確認例に加え、リロード、import/export、step/cancel、モバイル表示、WASM分離を実行した。console/page errorはなかった。

本番runtime/raster WASMはSDK添付物とbyte-identicalだった。compiler WASMだけは異なるため、同一成果物と偽っていない。原因はローカルのwasm-opt 120と、CloudflareのEmscripten 6.0.6に含まれるwasm-opt 131の差である。同じソースを131で再ビルドすると、本番compilerのSHA-256 `47426d8405716db04dbf39f85c02ef968df640244058f5cf7268fba8523de9dd`と完全一致した。

SDK側compilerのSHA-256は`965aaa5fad24df787c4254c0d07c52aebe48d8834956c5968b0567d672f5a3ae`。それぞれに前述の716未達比較を実行し、240基準出力も両版で一致した。公開済みSDK tarballは書き換えていない。

## 6. npm公開完了とv0.3.0

npm管理者によるTrusted Publisherの設定後、[Publish npm release run 36667537171](https://github.com/Stormcat-Works/storm-lua-engine/actions/runs/36667537171)をmainからtag=v0.2.1で実行した。attempt 1の事前検証とnpm publishが成功し、registry上の公開者はGitHub Actions、trustedPublisherはgithubと記録された。長期npmトークンの追加や、端末からの手動publishは行っていない。

公開直後の確認では30秒の再試行期間にversion endpointが404を返し、verify-registryだけ失敗した。その後version/latestが取得可能になったことを確認し、workflow全体を再実行した。**attempt 2は既存の同一内容を検出してpublishをskipし、verifyとverify-registryが成功した。** 既存版を再公開・上書きしていない。最初の確認失敗は隠さず[機械記録](release-0.2.1.json)へ残した。

| 公開後確認 | 結果 |
| --- | --- |
| npm version / latest | 0.2.1 / 0.2.1 |
| registry tarball SHA-256 | `3826946bbe2117bf7cc4d2ea7745f20ecaffb0927304af6540bd781f7354ad19`、GitHubの検査済み配布物と一致 |
| dist.integrity | SHA-512一致 |
| Hosted registry consumer | Vehicle/Addon、Lua実行、描画、compiler、host、保存/reload、source loading、mapped debugger/errorを確認 |
| Dev-Serverの空ディレクトリから通常のnpm install | `@stormcat-works/storm-lua-engine@0.2.1`の導入成功 |
| 通常導入したSDKの追加smoke | 入力3.5→出力7、描画画素、minify成功、fast/beam1の目標未達と最大探索のbyte一致 |

導入コマンド: `npm install @stormcat-works/storm-lua-engine@0.2.1`。

GitHub Releaseの既存tarball/ZIP/SHA256SUMSは変更せず、公開後の検証結果を別の`npm-publication-verification.json`として添付する。既存のrelease-verification.jsonは公開準備時点の記録で、現在の公開状態は本書と追加した検証記録を参照する。

v0.3.0は[由来情報計画](../design/source-provenance.md)のP2〜P5を実装する専用branchで開発する。今回の公開処理で同branchのソース変更、マージ、タグ作成、v0.3.0の公開は行っていない。

原ログと再取得した配布物はlocal-validation/release-0.2.1-publishedへ保存した。公開文書へ秘密・非公開ソースを持ち込んでいない。
