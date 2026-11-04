# リリース候補 0.4.0 再構築記録（第309ラウンド後、2026-11-04）

- 位置づけ: 第309ラウンド（Codex run8 所見 T01〜T10 の全件修正）の着地を受け、候補スカッシュ 0fb69571ff65fffd4838b431f84b53871ce33aa6 を破棄して候補を作り直した記録（再構築4回目）。タグ・GitHub Release・npm 公開・配備（工程4以降）は本記録の範囲外で未実施であり、最終承認待ちである。
- 入力: 統合先端 353160e（コード先端 083fc85、FULL-VERIFY-TIP 083fc85、独立回帰パック 166/166 PASS、Tier FULL はステータス全 16,620 行一致・出力変化 60 行は T03／T04／T05 へ全件帰属、ワークスペーステスト 1,828 件）。ただし後述の fmt 是正コミット c3c1b96 が統合へ着地したため、最終的な候補ツリーは c3c1b96 のツリーである。

## 最終候補

- スカッシュ f26c9b3d8f157948d1a28f5107213294a1ea82c4（親は公開 main 44376b89bbff6af1a8b98fcea3f4e399500b4bce、著者 makkiilikecat + Muse、ツリーが統合先端 c3c1b96 と完全一致することを diff で確認）を作成し、候補ブランチ muse/release-v0.4.0 へ force-push した。Draft PR #3 の head が新スカッシュになったことを確認し、他の GitHub ref（main／develop／release／muse/perf-optimization／タグ）は ls-remote の前後比較で不変である（PR に付随する head／merge ref の自動更新を除く）。
- CI run 41703481142 が初回試行で全ジョブ通過した（native ubuntu／macOS／Windows、wasm の全ステップ）。ubuntu ジョブではワークスペーステスト 1,828 件合格を再確認した。
- 候補ツリーは統合先端と同一であり、Tier FULL と独立回帰パックの証跡はそのまま有効である（再実施はしていない。fmt 是正が証跡へ影響しない根拠は次節）。
- 分類A再監査（最終スカッシュのツリー）: 開発ホスト名、ホームパス、ハードウェア型番、接続ツール名、内部基盤名、local-runs、私的ネットワークアドレスの具体値はいずれも 0 件（残る一致は検査器のパターン定義そのもの、テスト内の合成例示、過去の再構築記録におけるカテゴリ名の言及のみ）。

## CI 前の切り分け（1件）

- 統合先端 353160e のツリーへのローカル先行確認で、ピン留めツールチェーン 1.97.1 の `cargo fmt --all --check` が crates/storm-lua-minify/src/passes/root_globals.rs の5ハンクでのみ失敗した。fmt 以外の全ゲート（ワークスペーステスト全件、clippy 両条件、`cargo xtask check`、rustdoc、conformance 例題4件、release node テスト、tools Python テスト、npm テスト、screen fixtures、wasm32 の vm＋compiler-wasm check）は同ツリーで全て通過している。差分は全て行折り返しの判断差であり、第309ラウンドの変更がピン留め版と折り返し判断の異なる rustfmt で整形されていたことが原因で、製品バグではないと切り分けた。統合ブランチに style 是正（c3c1b96、対象は同ファイルのみ +10／−18）を着地させてから再スカッシュした。是正後の非空白トークン列は是正前と、呼出し引数境界の末尾カンマとクロージャ波括弧を除き完全一致し、minify テスト 1,167 件の再合格と合わせて製品挙動への影響がないことを確認した。CI は是正後のツリーで fmt ステップを含め全通過した。

## 工程2・3の成果物

producer は最終スカッシュのチェックアウト（~/sle-release/v0.4.0/src、HEAD f26c9b3）である。旧候補の成果物は ~/sle-release/v0.4.0/artifacts-0fb6957-superseded/ へ退避した（旧ハッシュは前回の再構築記録に保持されている）。

| 成果物 | bytes | SHA-256 |
| --- | ---: | --- |
| source-manifest.txt（839ファイル） | 96,446 | 4f881e5cc6398237a4ddd5ae7f53f11aaeef7b5f4cfcae5d414ebbe12bec583f |
| stormcat-works-storm-lua-engine-0.4.0.tgz（62ファイル、test-package 隔離検査通過） | 1,909,527 | c8c5116f98012db0c8e20c21c3a9e55f49fe3614ee2983cc329ed79f9920e651 |
| storm-lua-engine-playground-0.4.0.zip（31エントリ） | 3,337,686 | 4f06e3c5330251e243a4590ab6b784b98fed6b63860882bbf8845a4502e57d14 |
| release-candidate-0.4.0.json（状態 ci-passed-unpublished、CI run 41703481142 を参照） | 9,599 | 9f2d98ecddb171e642da5fa8a28c6a79a737393542c89e31e17e5abe60ff1160 |

- SHA256SUMS は上記4成果物を対象に再生成し、sha256sum -c で全件一致を確認した。source-manifest はチェックアウト上の実ファイルへ sha256sum を適用する方式（シンボリックリンクを辿る）で生成し、候補チェックアウトに対する自己照合も 839/839 一致した。
- 生成時の注記: wasm ビルドは CI と同じ Emscripten 6.0.6（emsdk）を有効化して実行した。Playground ZIP は組み立て先の app/dist-site から新規に作成し、エントリ構成が旧候補と一致することを確認した（差異は内容ハッシュ付きアセットファイル名のみ）。

## 統合先端の扱い

- 本記録のコミットにより統合先端は c3c1b96 から本記録分だけ前進する。候補スカッシュ f26c9b3 のツリーは c3c1b96 のツリーであり、本記録（文書のみ）は候補ツリーに含まれない（従来と同じ扱い）。
