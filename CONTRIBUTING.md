# Contributing

## Prerequisites

Rustは[rust-toolchain.toml](rust-toolchain.toml)のtoolchain、Python 3.10以降、Node.js 22.18以降、npmを使用する。Nativeのvendored LuaにはCコンパイラが必要。WASMは[配布・ビルド](docs/design/distribution.md)のtargetとEmscriptenを追加する。
Cargoの生成物は利用環境のポリシーに従って`CARGO_TARGET_DIR`で指定できる。個人の絶対パスをリポジトリ設定へ入れない。

## Gates

| Purpose | Command |
|---|---|
| Release workflow input validation | `node --test tools/release/*.test.mjs` |
| Format | `cargo fmt --all --check` |
| Dependencies, generated data, docs | `cargo xtask check` |
| Native tests and backend features | `cargo test --workspace --all-features --locked` |
| Default configuration | `cargo check --workspace --locked` |
| Lints | `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` |
| Rust documentation | `cargo doc --workspace --all-features --no-deps --locked` |
| 採用済み画面契約の検査 | `python3 tools/check_screen_fixtures.py` |
| Isolated npm consumer | `node tools/test-package.mjs` (build TS/WASM first) |
| Three browser engines / Workers | `node tools/test-browser.mjs` (install Playwright browsers first) |
| Python tool tests | `python3 -m unittest discover -s tools/tests -v` |
| Consumer mode/type contracts | `npm --prefix packages/lua-engine run check:consumer` |
| Native host example | `cargo run -p storm-lua-conformance --example addon_host --locked` |
| TypeScript and JS memory tests | `npm --prefix packages/lua-engine ci` then `npm --prefix packages/lua-engine test` |
| WASM runtime/raster execution | `node tools/build-wasm.mjs` then `node tools/test-wasm.mjs` |

Rustdoc broken intra-doc linksはCIでエラーにする。新しい依存・public API・互換挙動の変更は対応する仕様とテストを同時に変更する。ABI constantsはRustが正本であり、`cargo xtask generate`でTSとfixtureを更新する。フォントJSONからRustを生成する手順も同じコマンドに含まれる。

## Tests and changes

単一moduleのunit testは近傍、公開契約はcrateの`tests/`、複数crateを跨ぐ契約は`conformance/tests/`へ置く。fixtureは読み取り専用とし、ゲームや他リポジトリへのアクセスを通常CIの必須条件にしない。未実装機能のfixtureが存在することと、適合検証が実行されたことを区別する。

設計・レビュー・ベンチ報告では、確認した版と実際に走らせたコマンドを記録する。速度・対応環境・互換性を未測定のまま保証しない。以前のWASMサイズをAddon等の追加後のサイズ保証に使わない。

コミット・push・公開は管理者の明示指示に従う。自動publishのworkflowは設けない。


## 画面契約と公開前の確認

画面の期待値を現行実装で再生成して回帰テストを通す運用は禁止します。[画面fixture](fixtures/screen/README.md)の更新手順に従い、独立した根拠とレビューを付けてmanifestを更新してください。フォント定数の生成と期待RGBAの更新は別です。

`node tools/check-artifacts.mjs`はビルド後の配布パスを検査します。packageの梱包内容、権利表示、SDK/system library、公開対象のGit履歴は別の確認項目です。CIを通しただけで、未公開の過去履歴を自動的に公開可能と判断しません。

## Playground consumer

SDKの全WASMとTypeScriptを先にビルドし、`npm --prefix app ci`、`npm --prefix app run build`、`npm --prefix app test`、`npm --prefix app run test:browser`を実行します。SDKからappへの逆依存は作りません。配布物は`node tools/check-artifacts.mjs app/dist`で検査します。公開用Workerのdry-runは本番配備とは区別します。

## v0.2.0の非短縮Source Map

`conformance/tests/source_maps.rs`は実Native VM、`packages/lua-engine/tests/wasm/source-map.test.mjs`は実WASMで原文位置を確認します。`node tools/test-package.mjs`は梱包済みSDKへ独立consumerを導入し、`examples/consumer/source-map.mjs`を実行します。trace-mappingはそのconsumerだけの依存で、SDKの実行時依存には入りません。
