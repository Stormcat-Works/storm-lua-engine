# Distribution, targets and build

## Rust

RustクレートはGitタグまたは特定revisionへの依存で利用する。本体のローカル開発にはCargo path依存を使える。gitとcrates.ioの取得元の違いだけで実行性能は変わらない。必要な機能crateだけを参照し、描画専用consumerへLua backendを入れない。

RustクレートはGitで配布するため`publish = false`を維持する。npmパッケージは`publishConfig`でpublicなnpm registryを明示する。通常の開発CIは検証のみで、自動publishしない。版の更新、タグ、配布物、registry公開の手順は[リリース手順](../release.md)を参照。

ライブラリのCargo profile設定はconsumerのworkspace設定を上書きしない。[Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html)。LTOを使える構造と、常にすべてがinlineになるという保証を混同しない。

## One npm package, separate WASM builds

`@stormcat-works/storm-lua-engine`に型定義・JSラッパー・WASMを同梱する。現在の入口はmain、`/raw`、`/debug`、`/raster`、`/commands`、`/canvas`、`/compiler`、`/compiler-worker`とWASMファイル。runtimeのWorker構成はhost側のexampleで示す。compilerにはWorkerクライアントも提供し、汎用poolを必須化しない。

runtime用WASMはmicrocontrollerと必要なrasterを同じmoduleに含める。raster-only用WASMはLuaを含めない。両方をロードした場合のmemoryは別であり、pointerを相互に使わない。compiler-only用WASMも独立したmoduleとmemoryを持ち、Lua実行系を含めない。

## Pinned build baseline

Rust 1.97.1、Emscripten SDK **6.0.6**、TypeScript 5.8.3を使用する。Nodeは22.18以降。EmscriptenはSDKに付属するClang/LLVMとBinaryenを一組として使い、OSの別バージョンの`wasm-opt`を混ぜない。画面契約は同梱JSONだけで検査し、外部アプリや元ソースの取得・実行を必要としない。

`rustup target add --toolchain 1.97.1 wasm32-unknown-unknown wasm32-unknown-emscripten`でtargetを入れる。Emscripten SDKの6.0.6をactivateし、そのSDKの`emcc`と付属ツールがPATHから解決される状態にする。[SDK installation](https://emscripten.org/docs/getting_started/downloads.html)。

開発時の実行順は`npm --prefix packages/lua-engine ci`、`cargo xtask check`、`npm --prefix packages/lua-engine test`、`node tools/build-wasm.mjs --with-tests`、`node tools/build-compiler.mjs`、`node tools/test-wasm.mjs --with-tests`。ブラウザ検証はpackageで`npx playwright install chromium firefox webkit`を行い、rootから`node tools/test-browser.mjs`を実行する。CI/Linuxでシステム依存も必要な場合はPlaywrightの`install --with-deps`を使う。

[build-wasm.mjs](../../tools/build-wasm.mjs)は`cargo metadata`からtarget directoryを取得し、rasterを`wasm32-unknown-unknown`、runtimeを`wasm32-unknown-emscripten`でrelease buildする。SDK固有の例外ABIとexport指定はそのbuildの環境だけに渡す。グローバルCargo設定やshell設定を書き換えない。

成果物は`packages/lua-engine/dist/wasm/`へコピーされ、Gitでは管理しない。テスト専用のLua backend executableは`artifacts/wasm-tests/`へ分離し、npmには同梱しない。`--with-tests`を付けないbootstrapの成功だけでLua backendの実行まで検証したと主張しない。

`packages/lua-engine/`で`npm pack --dry-run`を実行し、JS、型定義、WASM、文書とライセンスを検査する。テスト専用fixtureや元のゲーム資産一式をpackageへ含めない。`node tools/test-package.mjs`は作成したtarballを独立consumerへoffline installし、bare package importから実際のLua/rasterを検証する。

コンパイラ資産は[build-compiler.mjs](../../tools/build-compiler.mjs)で別に生成する。CIで固定しているwasm-pack 0.13.1を使い、生成先は`packages/lua-engine/dist/compiler-wasm/`とする。runtimeやrasterのビルドだけではコンパイラ資産は生成されない。

## Targets and verification levels

| Platform | Intended native targets | Evidence policy |
|---|---|---|
| Windows | x86_64-pc-windows-msvc, i686-pc-windows-msvc, aarch64-pc-windows-msvc | CIのx64実行と他architectureのcross-buildを区別 |
| Linux | x86_64-unknown-linux-gnu, i686-unknown-linux-gnu, aarch64-unknown-linux-gnu, armv7-unknown-linux-gnueabihf | glibc baseline。muslを暗黙に含めない |
| macOS | x86_64-apple-darwin, aarch64-apple-darwin | x86 32bit/armv7 macOSを対応表へ足さない |
| Web raster | wasm32-unknown-unknown | Native/WASM bootstrapと描画適合は別 |
| Web runtime | wasm32-unknown-emscripten | C Lua backendの実行を含む別probeを実施 |
| Web compiler | wasm32-unknown-unknown | compiler専用WASMと独立consumerを検証 |

意図したtargetがすべて現在検証済みという意味ではない。実際の実行環境は[verification](../verification/repository.md)に記録する。Native ARM/x86やブラウザごとの本番対応には専用CI/実機実行を追加する。

## Browser example

WASMを作成後、リポジトリルートで`python3 -m http.server 8080`を実行し、`examples/browser/`を開く。source編集・load/tick/draw・Canvas表示ができる小さな組み込み例であり、IDE製品ではない。ブラウザ配信、WebViewのCSP、asset URL、Worker構成はconsumer側で設定する。

## License generation

`cargo about generate --workspace --all-features --locked about.hbs -o packages/lua-engine/THIRD_PARTY_LICENSES.txt`で解決済みRust依存のライセンス全文を生成する。ツールbaselineはcargo-about0.9.2。描画構成要素のMIT許諾文はSCREEN_COMPONENTS_LICENSEとして別途同梱し、移植部分の権利者表示を保持する。

Cargo依存とは別に、`TOOLCHAIN_LICENSES.txt`へEmscripten、muslの総合・ファイル別許諾、compiler-rt、libc++／libc++abi、libunwind、LLVM libc、allocatorの通知を収録する。`RUST_STD_LICENSES.html`は固定Rust toolchainに付属する標準ライブラリの著作権・許諾一覧を原文のまま同梱する。通知はSDKの保守的な上位集合であり、列挙された全ライブラリがすべての成果物にリンクされるという意味ではない。

固定SDKをPATHへ設定して`python3 tools/generate-toolchain-licenses.py`で更新し、`--check`で同一性を検査する。元ファイルの相対パス・SHA256と生成物のSHA256は`tools/toolchain-licenses.json`に記録する。SDK変更時は通知も更新する。Playwright/TypeScriptは開発用で、npm packageのruntime dependencyにはしない。

## Host integration build

標準runtimeはvehicle、addon、debug、同期JS host importを含む。`tools/host-library.js`をEmscriptenのjs-libraryとしてリンクし、crateのbuild scriptで内容変更を追跡する。ソースを編集したのに古いWASMがキャッシュから再利用されることを避ける。raster-only moduleにはこのhost importやLuaをリンクしない。

consumer向け入口はcreateVehicle/createAddonを明示する。`packages/lua-engine/tsconfig.consumer.json`はmode混在・Promise型のserver/map callbackをcompile-timeで検査する。`tools/test-package.mjs`は梱包後の独立インストール先へ[利用例](../../examples/consumer/node.mjs)をそのままコピーし、host地図・server・logs・HTTP/checkpointを実行する。


## 配布ビルドのパス正規化

`tools/build-wasm.mjs`は両方のWASM targetに`--remap-path-prefix`を適用する。ホームは`/build-home`、Cargo依存は`/dependencies/cargo`、ビルド出力は`/build/target`、本体は`/workspace/storm-lua-engine`へ正規化する。後に一致した規則が適用されるため、具体的なパスは後に指定する。ユーザー全体のCargo設定は変更しない。

これは依存ライブラリのソースコードを隠す処理ではなく、panic位置等に埋め込まれるビルド時の絶対パスを正規化する処理である。コンパイラ外のツールが埋め込む文字列まで自動で消えるとは限らないため、完成後のJS/WASMを`node tools/check-artifacts.mjs`で別に検査する。既知のローカルprefixとUTF-8/UTF-16のホームパスを検出し、失敗時は配布を止める。仮想FSの標準パスは個人情報と区別する。

npmの独立installテストでも、展開した`dist`を再検査します。ブラウザサンプルのbuildは静的配布ディレクトリを検査し、元のWASMから古いパスをコピーしていないことを確認します。これは完全な秘密情報スキャナや、すべてのOS/linkerに対するパス除去の保証ではありません。

仕様の根拠: [rustcのパス正規化](https://doc.rust-lang.org/rustc/remap-source-paths.html)。Nativeを利用者の環境でビルドする場合のパス管理は、そのアプリのビルド契約に従います。
