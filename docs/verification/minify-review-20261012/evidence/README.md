# minify レビューの再現・検証証跡

結論、優先度、最小再現、修正方向は [統合報告書](../../minify-review-20261012.md) に記載した。このディレクトリは、実行ログと、再実行できる consumer・入力・generator を保存する。

対象は v0.3.1 commit `5a168d63eda82c898af4dcc72f8c2c3f9a178e51` に metadata / Source Map 修正を加えた作業ツリー。`--repo` にはその checkout、または比較したい修正版 checkout を明示する。リポジトリの旧ルート branch の検証結果と混同しないこと。監査対象の tracked diff は [review-target.patch](root/review-target.patch)、対象ソースの hash は [review-source-manifest.json](root/review-source-manifest.json)、当時 untracked だった追加回帰は [metadata_map_tests.rs](root/review-target-untracked-metadata_map_tests.rs) に保存した。

## 証跡の入口

| 分類 | 指摘と再現手順 | 主な結果 |
| --- | --- | --- |
| 構文・文字列・数値 reader・深さ | [root/findings.md](root/findings.md)、[run.py](root/run.py) | [設定別観測](root/root-option-matrix.jsonl)、[syntax 集計](root/syntax-corpus-summary.json)、[hex 集計](root/hex-corpus-summary.json) |
| 算術・数値変換・format | [numeric/README.md](numeric/README.md)、[reproduce.py](numeric/reproduce.py) | [重点再現](numeric/focused-results.jsonl)、[算術 matrix](numeric/matrix-summary.json)、[設定別集計](numeric/option-summary.json) |
| スコープ・closure・table・goto | [scope/scope-review.md](scope/scope-review.md)、[再実行手順](scope/README.md) | [構造化 corpus](scope/structured-corpus.jsonl)、[指摘別 matrix](scope/findings-matrix.jsonl)、[release 性能](scope/table-flatten-missing-release-timing.jsonl) |
| API・描画・探索 | [api/findings.md](api/findings.md)、[run.py](api/run.py) | [重点再現](api/results.jsonl)、[描画集計](api/draw-fuzz-summary.json)、[release 性能](api/perf-release-summary.json) |
| 実 Engine WASM | [game-runtime.mjs](root/game-runtime.mjs)、[game-api.mjs](root/game-api.mjs) | [値・callback 比較](root/wasm-game-runtime.jsonl)、[API・画素比較](root/wasm-game-api.jsonl)、[使用 asset の照合](root/existing-wasm-verification.json) |
| Source Map・metadata | [wasm.mjs](root/wasm.mjs)、[map_parity.py](root/map_parity.py) | [metadata 等の公開 API 観測](root/wasm-probes.jsonl)、[map ON/OFF 集計](root/map-parity-summary.json)、[API 設定との交差](api/map-matrix-summary.json) |

JSONL は pass/fail のみではなく、入力・設定・生成コード・元と生成後の観測値を含む。`ok:true` は compile 成功であり、意味の一致を示さない。panic や不一致を意図的に再現するケースもある。`seconds`、`elapsedMs` などの時間値は再実行時に変わる。

## Native の重点再現

以下はレビュー成果物を保存したリポジトリのルートから実行する例。`/path/to/audited-checkout` を対象へ置き換える。Rust/Cargo と依存取得済みの環境が必要。runner は独立 consumer を一時生成し、製品ソースへは書き込まない。Cargo cache は `/tmp/cargo-targets` 配下を使用する。結果出力は保存済み証跡を上書きしない別のファイルにする。

```bash
python3 docs/verification/minify-review-20261012/evidence/root/run.py \
  --repo /path/to/audited-checkout --output root-rerun.jsonl

python3 docs/verification/minify-review-20261012/evidence/numeric/reproduce.py \
  --repo /path/to/audited-checkout --output numeric-rerun.jsonl

python3 docs/verification/minify-review-20261012/evidence/api/run.py \
  --repo /path/to/audited-checkout --release > api-rerun.jsonl
```

scope の consumer は [専用 README](scope/README.md) を参照。WASM の再実行には対象ソースに対応する package dist が必要で、コマンドは [統合報告書](../../minify-review-20261012.md) に記載した。WASM と Native の比較は Engine 上の検証であり、Stormworks ゲーム本体での実測ではない。

## 既存ゲートと記録の区別

- 正しい監査対象の Native: [v031-workspace-tests.log](root/v031-workspace-tests.log)、[終了コード](root/v031-workspace-tests.rc)。875 passed / 0 failed / 0 ignored。
- 正しい監査対象の WASM: [wasm-all-tests.log](root/wasm-all-tests.log)、[終了コード](root/wasm-all-tests.rc)。91 passed / 0 failed / 0 skipped。
- `wasm-compiler-tests.log` は上記の部分集合を先行実行した記録。91 件に重複加算しない。
- `workspace-tests.log` は対象を切り替える前の旧 checkout の compile error 記録。テスト開始前の失敗であり、v0.3.1 の不具合数・成功件数には算入しない。
- `initial-*` は旧ルート、`review-target-*` は監査対象の開始時状態。既存の変更を保存したもので、このレビューで適用した patch ではない。

## 保存と整合性

全成果物のサイズと SHA-256 は [manifest.json](../manifest.json) に記載。`path` はその manifest のディレクトリを基準とする。各担当の manifest も保持した。最終確認は [completion-verification.json](root/completion-verification.json) を参照。

この証跡は `/tmp` 以外の永続領域に保存した。Cargo cache や一時実行 binary は再生成可能であり、永続的な依存先にはしていない。検証に使った WASM の由来と hash は保存しているが、package バイナリ一式をこのディレクトリへ重複コピーしてはいない。
