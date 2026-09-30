# npm公開自動化とCI起動の検証

2026-09-30。**v0.2.1のOIDC実公開とregistry再取得・consumer検証は完了した。** 公開先はnpmjs.comを維持する。実装はmainの`91afd9f`、registryのlatest確認は`e2b1f24`。v0.2.1のtagと配布tarballは変更しない。

## 実装

[公開workflowの仕様](../design/publication-workflow.md)を参照。GitHub Release公開時、またはmainから既存tagを指定したdispatchで動作する。検査済みtarballのsourceRevision/version/checksumと、同じcommitの実CI成功を確認し、その成果物を隔離consumerで検証する。

公開専用jobだけがOIDCのid-token:writeを持つ。package scriptsや長期npmトークンを使わずに同じtarballを送信する。公開後はregistryから再取得してintegrityとSHA-256、latestが当該版以上であること、consumerの動作を確認する。同じ内容の再実行は許容し、異なる内容での同一版の上書きやlatestの巻き戻しは拒否する。

## 実行結果

| 検査 | 結果 |
| --- | --- |
| 入力/receipt/archive/registry/CI成功検証のunit tests | 9件成功 |
| actionlint | ci.yml / publish.yml成功 |
| 実際のv0.2.1 Release preflightとtarball consumer | 成功 |
| [Hosted dry-run](https://github.com/Stormcat-Works/storm-lua-engine/actions/runs/36655901803) | verify成功、約21秒。公開/registry確認はdry-runにより実行しない |
| [Hosted通常CI](https://github.com/Stormcat-Works/storm-lua-engine/actions/runs/36655898394) | 91afd9fの3 OS NativeとWASMが成功 |
| [実際のOIDC公開](https://github.com/Stormcat-Works/storm-lua-engine/actions/runs/36656237821) | verify成功、npm publishがENEEDAUTH、公開後確認は未実行 |
| 既公開0.2.0に対するregistry検証スクリプト | version/integrity/ダウンロードhash/latest検証が成功。0.2.1公開と混同しない |

npm Trusted Publisher登録の成立をこちらから確認できる認証はなく、既存npm trust listも401だった。ENEEDAUTHを公開成功として扱わない。初回だけ、npm側でOrganization=Stormcat-Works、Repository=storm-lua-engine、Workflow=publish.yml、Environment未指定、直接のnpm publishを許可する設定が必要である。手順は上記仕様に記録した。

設定完了後はPublish npm releaseをmainからtag=v0.2.1で再実行できる。通常の今後の公開はReleaseイベントを起点とし、管理者の端末で毎回npm publishする必要はない。

## 起動量の抑制

通常CIはmain/developへのpush、PR、明示dispatchで実行する。作業branchのpushとPR、公開時のmain/tag/releaseの同一commit実行を重ねない。同一PR/branchの旧runはcancelする。release品質の3 OS/WASM試験そのものは維持する。

公開標準runnerの使用時間と、組織のprivate向け無料分数は別である。組織の実利用量・privateリポジトリの調査結果はローカル検証領域と該当privateリポジトリに記録し、ここへ内部情報を公開しない。

## 開発との分離

npmの初回登録は公開運用の残件として扱う。v0.3.0のParser/Printer位置記録は専用branch `feat/minify-source-provenance-v0.3`の`47436c6`で着手・検証している。最適化後minify mapの完成やv0.3.0の公開を意味しない。


## 最終mainのCI

公開後latest確認も含むcommit `7caef992e2a685a4f01b2a03001baf1861d8c644`は[CI36660638197](https://github.com/Stormcat-Works/storm-lua-engine/actions/runs/36660638197)で3 OS Native/WASM全成功。開発branchの実装47436c6も別のCI36660638108で成功した。今回の追記は検証結果だけであり、同じ製品コードのCIを再起動しない。


## Trusted Publisher設定後の公開完了

[run 36667537171](https://github.com/Stormcat-Works/storm-lua-engine/actions/runs/36667537171)のattempt 1でnpm publishが成功した。公開直後のversion取得は404を返し、30秒の確認期間では完了しなかった。取得可能になった後のattempt 2は同一内容の公開済み版を検出してpublishを省略し、verify/verify-registryが成功した。npm versionとlatestは0.2.1、再取得したtarballのhashも元の検査済み配布物と一致した。空のディレクトリから通常のnpm installとLua/描画/compilerのsmokeも成功した。詳細は[release-0.2.1](release-0.2.1.md)へ記録する。上のENEEDAUTHは設定前の履歴であり、現状の公開障害ではない。
