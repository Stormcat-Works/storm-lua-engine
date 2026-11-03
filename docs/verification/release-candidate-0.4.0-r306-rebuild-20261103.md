# リリース候補 0.4.0 再構築記録（第306ラウンド後、2026-11-03）

- 位置づけ: 第306ラウンド（Codex run5 所見 Q01〜Q16 の全件修正）の着地を受け、旧候補スカッシュ f957190e4fafc75e80728e929c30de9569a073df を破棄して候補を作り直した記録。タグ・GitHub Release・npm 公開・配備（工程4以降）は本記録の範囲外で未実施であり、最終承認待ちである。
- 入力: 統合先端 2e577d3fa5b458a4e95de7d1df49e76e26a43949（コード先端 1265a0badf84772c79c8a7b628ca93a3534cb393、FULL-VERIFY-TIP 1265a0b、独立回帰パック 134/134 PASS）。

## 初回スカッシュと CI の所見・処置

- スカッシュ f8b0e52d801cd3e2115e5ae585aac79de9157f55（親は公開 main 44376b89bbff6af1a8b98fcea3f4e399500b4bce、著者 makkiilikecat + Muse、ツリーが統合先端 2e577d3 と完全一致することを diff で確認）を作成し、候補ブランチ muse/release-v0.4.0 へ force-push した（Draft PR #3 の head が更新され、他の GitHub ref は ls-remote の前後比較で不変を確認）。
- CI run 41626679512: native 3 OS（ubuntu／macOS／Windows）は全ステップ成功。wasm ジョブが test-wasm（92件中1件）で失敗した。失敗は packages/lua-engine/tests/wasm/compiler-provenance.test.mjs の「non-minified map resolves UTF-16 columns on both sides of same-line linking」で、lib の function() の解決先が帰属なし（null）になった。
- 切り分け（実測）: 同一フィクスチャのリンク生成物を wasm コンパイラで再生成して全 function() 出現箇所を走査した。生成コードは1行目がレジストリ宣言、2行目がラッパ __stormmin_link_modules["lib"] = { function()（生成 glue で帰属なし）、3行目が lib 本体 return function() であり、本体の function() は lib.lua の1行7列へ正確に解決した。他の2プローブ（main の output.setNumber、lib の return 7）も期待どおり一致した。すなわち第306ラウンドのリンカ再設計でモジュール本体の前にラッパが生成されるようになり、テストの検索開始位置（先頭の改行の直後）がラッパ側の function() を指すようになった検索針の陳腐化であり、帰属の喪失（製品側の問題）ではない。第305ラウンド追補2の同種事例と同じ処置区分とした。
- 処置: 検索開始位置を本体の return の位置へ合わせ、理由をコメントで明記した。統合ブランチへ 1d9d0997ddc35171332ee272bc1c1e4dc2c48485 として着地した（変更は当該テストファイル1件のみ、製品コードの変更はない）。
- ローカル先行確認: 処置後のツリーで wasm ジョブの全段を .ud で実地実行し（Emscripten は CI と同じ 6.0.6 を有効化）、全て通過した。内訳は npm パッケージテスト 30/30、test:compiler 18/18、test-wasm 92/92、check-package、test-package 隔離検査、Playground の build／単体テスト 38/38／ブラウザ操作、test-compiler-browser。test-browser は初回にブラウザのスクリーンショット競合（Page.captureScreenshot のプロトコルエラー）で失敗したが、再実行で3エンジン全て通過したため環境側の揺らぎと判断した。
- Tier FULL について: 本処置で製品コード（Rust）は変更されていないため、FULL-VERIFY-TIP 1265a0b の証跡（ステータス全行一致、出力変化 2,024 行の全件帰属）はそのまま有効であり、再実施はしていない。

## 最終候補

- スカッシュ 5c03070a394fc183ab8b25397a686417802c34f6（親 44376b89、ツリーが統合先端 1d9d099 と完全一致することを diff で確認）を候補ブランチへ force-push し、Draft PR #3 の head が新スカッシュになったことを確認した。他の GitHub ref（main／develop／release／muse/perf-optimization／タグ）は不変である。
- CI run 41629598912 が全ジョブ通過した（native ubuntu／macOS／Windows、wasm の全ステップ）。同 commit を対象とする run はこの1件のみで、PR #3 のチェックロールアップも全て SUCCESS である。CI の ubuntu ジョブではワークスペーステスト 1,793 件合格（51 スイート）を再確認した。
- 分類A再監査（最終スカッシュのツリー）: 開発ホスト名、ホームパス、ハードウェア型番、接続ツール名、内部基盤名、local-runs、私的ネットワークアドレスのいずれも 0 件。

## 工程2・3の成果物

producer は最終スカッシュのチェックアウト（~/sle-release/v0.4.0/src、HEAD 5c03070）である。旧候補の成果物は ~/sle-release/v0.4.0/artifacts-f957190-superseded/ へ退避した（旧ハッシュは第305ラウンド記録に保持されている）。

| 成果物 | bytes | SHA-256 |
| --- | ---: | --- |
| source-manifest.txt（833ファイル） | 95,684 | c4f202df633aaca9ffc89d55ec4cb7b1514b4ccb13496ca573d6442ce3b609dd |
| stormcat-works-storm-lua-engine-0.4.0.tgz（62ファイル、test-package 隔離検査通過） | 1,891,095 | afa16bd4c9649c694da4518c9fc41886cad557f797cf185da01b32f773a39785 |
| storm-lua-engine-playground-0.4.0.zip（31ファイル） | 3,308,894 | f6ee3370a3e42e158ea8bce27dd0c38150f61af547c6ab58b23d565e202d9a09 |
| release-candidate-0.4.0.json（状態 ci-passed-unpublished、CI run 41629598912 を参照） | 4,325 | db24a2dc4a1d5871af87b31e06d9cf7c9653cecb5341f6c75534aae889e38427 |

- SHA256SUMS は上記4成果物を対象に再生成し、sha256sum -c で全件一致を確認した。source-manifest はチェックアウト上の実ファイルへ sha256sum を適用する方式（CLAUDE.md のシンボリックリンクを辿る）で生成し、候補チェックアウトに対する自己照合も 833/833 一致した。
- 生成時の注記: Playground ZIP は初回、既存アーカイブへの zip の更新モードで旧候補のハッシュ付き資産が混入したため、アーカイブを削除して作り直した（最終版は31ファイル全てが現行ビルドのもの）。

## 統合先端の扱い

- 本記録のコミットにより統合先端は 1d9d099 から本記録分だけ前進する。候補スカッシュ 5c03070 のツリーは 1d9d099 のツリーであり、本記録（文書のみ）は候補ツリーに含まれない（第305ラウンドと同じ扱い）。
