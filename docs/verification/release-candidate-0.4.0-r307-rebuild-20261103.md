# リリース候補 0.4.0 再構築記録（第307ラウンド後、2026-11-03）

- 位置づけ: 第307ラウンド（Codex run6 所見 R01〜R12 の全件修正）の着地を受け、候補スカッシュ 5c03070a394fc183ab8b25397a686417802c34f6 を破棄して候補を作り直した記録（再構築2回目）。タグ・GitHub Release・npm 公開・配備（工程4以降）は本記録の範囲外で未実施であり、最終承認待ちである。
- 入力: 統合先端 eb13e66f33cf9edfe98c60f52f31daeaabc4beab（コード先端 72979ec533150f58b469aa03efa20b7ceb5aaff4、FULL-VERIFY-TIP 72979ec、独立回帰パック 145/145 PASS、Tier FULL はステータス全 16,620 行一致・出力変化 0 行）。

## 最終候補

- スカッシュ d83577768434fabb889c715df00c16508f593bbd（親は公開 main 44376b89bbff6af1a8b98fcea3f4e399500b4bce、著者 makkiilikecat + Muse、ツリーが統合先端 eb13e66 と完全一致することを diff で確認）を作成し、候補ブランチ muse/release-v0.4.0 へ force-push した。Draft PR #3 の head が新スカッシュになったことを確認し、他の GitHub ref（main／develop／release／muse/perf-optimization／タグ）は ls-remote の前後比較で不変である。
- CI run 41650303143 が全ジョブ通過した（native ubuntu／macOS／Windows、wasm の全ステップ）。同 commit を対象とする run はこの1件のみで、PR #3 のチェックロールアップも全て pass である。CI の ubuntu ジョブではワークスペーステスト 1,807 件合格（51 スイート）を再確認した。今回のスカッシュでは CI 失敗の切り分けや期待値の是正は発生しなかった。
- 候補ツリーは統合先端と同一であり、Tier FULL と独立回帰パックの証跡はそのまま有効である（再実施はしていない）。
- 分類A再監査（最終スカッシュのツリー）: 開発ホスト名、ホームパス、ハードウェア型番、接続ツール名、内部基盤名、local-runs、私的ネットワークアドレスのいずれも 0 件（残る一致は監査カテゴリ名そのものの言及のみ）。

## 工程2・3の成果物

producer は最終スカッシュのチェックアウト（~/sle-release/v0.4.0/src、HEAD d835777）である。旧候補の成果物は ~/sle-release/v0.4.0/artifacts-5c03070-superseded/ へ退避した（旧ハッシュは前回の再構築記録に保持されている）。

| 成果物 | bytes | SHA-256 |
| --- | ---: | --- |
| source-manifest.txt（835ファイル） | 95,938 | c2ebeb0d068968bdc6d18fc0a0498053103f35f95983c3df79f7ea3de87ea33b |
| stormcat-works-storm-lua-engine-0.4.0.tgz（62ファイル、test-package 隔離検査通過） | 1,904,898 | c3722e6c4a0c7b8c679246e53bebb5b99c82addccc924001045adec136d52dab |
| storm-lua-engine-playground-0.4.0.zip（31ファイル） | 3,329,483 | 93129f2a3470917a7d049e36750f6163afb087a4e09fc063a91605e239107b96 |
| release-candidate-0.4.0.json（状態 ci-passed-unpublished、CI run 41650303143 を参照） | 5,825 | 9ccde352afee0d7971f0b7b4f57136ab718be8fa9aeef2233b6caac7b2b597da |

- SHA256SUMS は上記4成果物を対象に再生成し、sha256sum -c で全件一致を確認した。source-manifest はチェックアウト上の実ファイルへ sha256sum を適用する方式（シンボリックリンクを辿る）で生成し、候補チェックアウトに対する自己照合も 835/835 一致した。
- 生成時の注記: (1) wasm ビルドは CI と同じ Emscripten 6.0.6（emsdk）を有効化して実行した。既定のシステム側ツールチェーンでは wasm-opt の未知オプションにより emscripten リンクが失敗する（環境側の版不整合であり、製品側の問題ではない）。(2) Playground ZIP は組み立て先の app/dist-site から新規に作成し、エントリ構成が旧候補と一致することを確認した。dist 直下から作成するとサイトツリーの階層と _headers／version.json が欠ける。

## 統合先端の扱い

- 本記録のコミットにより統合先端は eb13e66 から本記録分だけ前進する。候補スカッシュ d835777 のツリーは eb13e66 のツリーであり、本記録（文書のみ）は候補ツリーに含まれない（従来と同じ扱い）。
