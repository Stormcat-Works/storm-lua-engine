# v0.3.1 公開候補の検証

2026-10-12。**公開commit確定前のローカル候補を統合・検証・梱包した時点の記録。** [機械記録](release-candidate-0.3.1.json)と[公開手順](../release.md)を参照する。

最終commit・CI・配布物・registryと本番の確認は[GitHub Release](https://github.com/Stormcat-Works/storm-lua-engine/releases/tag/v0.3.1)の`release-verification.json`と公開後の検証記録を正本とする。以下のhash・dirty=trueはローカル候補だけに対応し、正式配布物とは区別する。

## 統合と修正

性能ブランチ`feat/minify-performance-v0.3.1`の`88a9e53`と未コミットのコンパクトな字句由来生成を基準にした。別作業のAddon非短縮build（`7834de8`）、metadata除去、Addon環境・エラー後配送（`da32aa3`）、`origin/develop`のレビュー済み描画修正（`ec7fa7f`）を統合した。未公開SHAはローカル開発IDであり、公開履歴には含めない。既公開main/developを親とする新規1コミットへ統合し、以前の作業ツリーを保全する。追加性能研究はv0.3.2以降へ送る。

公開前に次の実際の問題を修正した。

- 子の条件分岐を最適化した後、古い解析snapshotで新しいノードを参照するpanic/WASM trap。解析を更新し、独立した合成例の全分岐を実VMで比較した。既知7原文はMap有無と全探索/8192目標の28設定、Native7設定で成功し、生成Luaが一致した。
- 字句短縮の高速mapがUnknown内のtokenアンカーを省く問題と、JSON読取り後に失われる名前のポインタ共有への依存。必要なアンカーと文字列による名前同一性を維持し、部分map取り込みと大量の反復名を検証した。
- コンパクト由来の範囲・UTF-8境界・順序・コピー内容の検証不足。破損したcandidate/mapデータを結果の生成前に拒否する。
- Addonのgameでのselect可否をruntime/静的検査で一致させ、影になる自前定義とnil/typeによる存在検査を維持した。JS側でもAddonにVehicleのminify/targetSize等を渡すと明示拒否する。WorkerのbuildAddonも接続した。

描画修正はStormworks v1.15.23の既存撮影に基づく採用済み契約をそのまま取り込んだ。5ケースの更新と12ケースの追加で743件となる。今回の実装から期待RGBAを再生成していない。新しいゲーム測定を行ったという意味ではない。

## 同一候補の品質ゲート

| 検証 | 結果 |
| --- | --- |
| Native全workspace/all-features | 871成功、失敗・ignored 0、exit 0 |
| SDK型/独立consumer型/JS | 30成功、exit 0 |
| 実WASMと独立Lua backend probe | 87成功、exit 0 |
| Playground/CLI | 38成功、exit 0 |
| Chromium・Firefox・WebKit | 各16例、Worker、743 RGBA×直接/実Lua、保存復元・移行・中断・デバッグ・部分map/大容量map成功 |
| fmt、default check、all-target/all-feature clippy、rustdoc、architecture、Python、font/画面契約、権利表示、公開workflow unit | exit 0 |
| 梱包SDKの独立導入・実Lua・標準map reader | 固定したtarballでexit 0 |
| Worker | Wrangler 4.118.0のdeploy --dry-run、exit 0。本番配備ではない |

Browser pluginは利用できず、既存Playwright 1.62.1を使用した。ルート配下のローカル静的サイトで、desktop 1440×1100とmobileを検証。ページ識別・非空画面・console/page error・実操作・画面証跡を確認した。SDK埋込み例は初回描画をscreenshotで確認してから実クリックする。Playgroundのコピー済みローカルSDKは再導入し、compiler WASMのhashが本SDKと同一であることを確認した。

キャッシュ不在によるoffline導入失敗は、同じlock/integrityのtarball再取得で解消した。これらを製品テストの成功へ混ぜず、再実行のexit codeを保存した。過去のテスト件数を今回の件数として流用していない。

## 正確性と性能

検証済み最終開発baseline（`5f1f58f`）との固定30原文×8設定=240構成、240未達target、480旧新reader、同一compilerのcandidate経路3本、通常/LifeBoat build 8組で、生成Lua・全map・診断・探索を一致させた。内部candidateの形式変更に伴い、Worker・JS glue・WASMは同じcompiler版を使用する。

実コーパス4,155原文は、既知7trapの修正検証と残り4,148原文の旧新比較に分けた。残りでは全探索4,054成功/94診断、8192目標4,121成功/27診断となり、成功8,175組の生成Lua・map全体・診断・探索・pass記録が一致した。比較から除くのはpassの実時間、mapのproducerとそれに依存するintegrityだけ。失敗を成功分布へ入れていない。

速度比較の基準は**実公開v0.3.0のregistry tarball**で、compiler WASM hashを公開記録と照合した。同じNode v24.19.0、入力・設定で、Map生成を含むAPI完了までを計測する。原文I/O・初期化・事前GC・外部validate・結果比較は時間外。全コーパスは1組ずつ、代表23原文×2設定は7回または21回の反復中央値を別記録した。共有VMのばらつきと負の観測を保持し、全入力・全OSの速度保証にはしない。

| 全コーパス成功組の平均、ms/原文 | v0.3.0 Mapなし | 候補 Mapなし | 短縮 | v0.3.0 Mapあり | 候補 Mapあり | 短縮 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 全探索、4,054原文 | 88.001 | 44.993 | 48.9% | 130.764 | 51.217 | 60.8% |
| 8192目標、4,121原文 | 21.151 | 5.258 | 75.1% | 29.135 | 6.080 | 79.1% |

反復条件は46、うち42成功・4つの構文エラーを維持。成功条件の両モードはすべて中央値が短縮した。制御例の全探索は58.4%/70.2%、4,096描画例は65.3%/66.4%短縮した。一方、無効入力には約0.5µsの負の観測があり、削除していない。**同じ候補のMap有無最大比は7.019で、全入力1.2倍目標は未達**。空・コメント・字句短縮・原文返却を分布から除かず、追加研究はv0.3.2以降へ送る。

Runtimeは100 warm-up後の9batch平均の中央値を記録。Native tick 0.836µs/draw96 110.419µs/create+load 56.021µs、Node/WASM tick 1.329µs/draw96 130.385µs/create+load 72.285µs。過去の別日に採った測定を厳密な前後比較にはしない。CPU描画/実Luaのみで、Canvas/GPU upload、Worker転送、cold compile/downloadは除外した。

## 反復時のメモリ観測

初期化と外部validatorを含む別processで、公開0.3.0/候補、Map OFF/ON、描画4,096件/字句反復の8条件を各6回コンパイルした。明示JS GC後の観測を全件保存し、末尾3回でexternal容量が安定した。LuaやJS/WASMの割り当て・GCが消えたという意味ではなく、任意入力の漏れを証明するものでもない。

| peak RSS、MiB | v0.3.0 Mapなし | 候補 Mapなし | v0.3.0 Mapあり | 候補 Mapあり |
| --- | ---: | ---: | ---: | ---: |
| 描画4,096件 | 320.5 | 261.2 | 481.0 | 408.4 |
| 字句反復 | 98.7 | 96.5 | 117.9 | 114.4 |

最初の観測では3回目以降にもbaselineの容量拡張があった。初回拡張と漏れを混同せず、最終の連続観測を同条件で確認した。旧新の初期化/JIT・validatorを含むため、差をRust compilerだけのpeakとして説明しない。

## 固定した配布候補と再開情報

`local-release/v0.3.1/`の永続領域に、source manifest、ソースpatch/追加ファイル、コマンド・ログ・rc、コーパス/反復/メモリの記録、画面証跡、検査済みtarball、静的サイトZIP、SHA256SUMS、release notes、公開ガイドの適用検査済みpatchを保持する。Cargo cacheはtmpfsであり、再開の唯一の依存にしない。ソース・配布物の実在、サイズとhashを再照合する。

ソースmanifest SHA-256: `b379ab4d62b1606f89853677da6e3a7f6f8e223ec28b7e5b81670f7dd8ac2821`。基準HEADは`88a9e531faf7b4ed787c41e784f58376d5b48372`、候補は未コミットなのでproducer.dirty=trueを維持する。

| 成果物 | bytes | SHA-256 |
| --- | ---: | --- |
| stormcat-works-storm-lua-engine-0.3.1.tgz | 1,542,478 | bd40119804e3be3da9229868321c8d026cf768fc6adb8ae3144a42ffa91bdec4 |
| storm-lua-engine-playground-0.3.1.zip | 2,647,514 | 357b92dc8d0f7d8dc196b70f02e2d4346147f5b4bdc27fecf1ae04e5198736d2 |

## 公開時に残る工程

このローカル候補の記録時点では、最終commitとその3 OS/WASM CIは未実施。commit確定後はproducer/revisionの変わる配布物を再生成・再検査し、その同一commit/成果物でタグ、GitHub Release、npm OIDC公開、releaseブランチの本番配備、registry再導入と本番3ブラウザを確認する。検査済みローカル候補を公開済みとして扱わない。

Addon最適化・完全なserver analyzer、消えた変数/storage/lifetime/bytecode PCの動的復元、全OS/全CPUの性能評価、任意Lua/実ゲーム全版の適合証明はこの有限検証の対象外。

## 公開文面の監査

Astraによる全119コミット（未公開64を含む）のメッセージと追加差分、公開対象499ファイルの監査で、資格情報・内部解析データ・私有Lua原文の漏えいは検出されなかった。過去資料のconsumer運用事情と他製品開発状況は、検証範囲を維持した一般的な表現へ整理した。既公開履歴は維持し、未公開作業履歴を公開commitの祖先に含めない。
