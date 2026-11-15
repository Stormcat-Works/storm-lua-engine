# npmパッケージが求めるNodeの版（2026-11-15）

公開パッケージの`engines`を決めるために、梱包済みパッケージを古いNodeで動かした記録である。この結果を根拠に、`engines`を`>=22.18`から`^18.19.0 || >=20.6.0`へ変更した。開発とビルドの条件（22.18以降）は変えていない。

## 条件

| 項目 | 内容 |
|---|---|
| 対象 | `develop`の`6069666`からビルドしたtarball（版番号0.4.1、未公開の変更を含む） |
| ビルド | Rust 1.97.1、Emscripten 6.0.6、wasm-pack 0.13.1、Node 26.8.2 |
| 実行環境 | macOS、Apple Silicon |
| 古いNode | nodejs.orgの公式`darwin-arm64`のtarball。`SHASUMS256.txt`のSHA-256と照合した |
| コマンド | `node tools/test-package.mjs --node <Nodeの実行ファイル> <tarball>` |

梱包、独立consumerへのoffline install、配布物の検査は、Node 26.8.2とそのnpmで行った。古いNodeで実行したのは、導入したパッケージを使うconsumerのスクリプトだけである。対象は、`tools/test-package.mjs`が書き出すsmoke、`examples/consumer`の`node.mjs`、`source-loading.mjs`、`log-locations.mjs`、`source-map.mjs`、`optimized-source-map.mjs`で、runtime、raster、compilerの3つのWASMを読み込んで実行する。

## 結果

| Node | 結果 | 失敗の内容 |
|---|---|---|
| 18.0.0 | 失敗 | `import.meta.resolve`が関数でない |
| 18.3.0 | 失敗 | 同上 |
| 18.18.2 | 失敗 | 同上 |
| 18.19.0 | 成功 | |
| 18.20.8 | 成功 | |
| 19.9.0 | 失敗 | 同上 |
| 20.0.0 | 失敗 | 同上 |
| 20.5.1 | 失敗 | 同上 |
| 20.6.0 | 成功 | |
| 20.20.2 | 成功 | |
| 21.7.3 | 成功 | |
| 22.0.0 | 成功 | |
| 22.18.0 | 成功 | |
| 26.8.2 | 成功 | |

失敗はすべて同じ原因で、最初に実行するsmokeの`import.meta.resolve`の呼び出しで止まった。`examples/consumer`の例も同じ関数でWASMの場所を求めている。`engines`の`^18.19.0 || >=20.6.0`は、この例一式が通った版の範囲である。

## 22.18が必要だった理由

実行時に22.18を必要とする箇所は見つからなかった。確かめた内容は次のとおり。

- Emscripten 6.0.6の既定値は、従来方式のWASM例外処理（`WASM_LEGACY_EXCEPTIONS`）、`WASM_BIGINT`、最小のNode 18.3である。ビルドはこれらを上書きしていない。
- TypeScriptの出力は`ES2022`で、SDKが使う組み込みのうち新しいものは`Object.hasOwn`と`AggregateError`である。どちらもNode 18.0.0にある。
- SDKのJSは`import.meta.resolve`を使わない。使うのはconsumerの例と検査用のスクリプトである。

## 対応とは書かない範囲

`import.meta.resolve`を使わず、`createRequire`でWASMの場所を求める最小のスクリプトも別に実行した。内容は、runtimeのload、tick、draw、Luaエラーの受け取り、rasterの作成、compilerの読み込みである。これは18.0.0、18.3.0、18.18.2、19.9.0、20.0.0のすべてで動いた。

ただし、この最小のスクリプトが確かめるのは検査一式の一部だけなので、`engines`の根拠には使っていない。これらの版を対象に入れるには、例を`createRequire`で書き直し、例一式を通す必要がある。[TASKS](../../TASKS.md)に残した。

## 実行していないもの

- Linux、Windows、x64での古いNodeの実行。CIのwasmジョブにLinux x64の18.19.0と20.6.0を足したが、このジョブはまだ走らせていない。
- 18.19.0と18.20.8の間、20.6.0と20.20.2の間、23系、24系、25系の各版。
- 古いNodeでの3ブラウザ検査、Playground、開発用の道具。これらは開発の条件（22.18以降）で動かす。
- VSCodeなど、Nodeを組み込んだ製品の中での実行。
