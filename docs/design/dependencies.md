# External dependency policy

## Selection

実装可能性ではなく、仕様の難しさ・保守責任・ライセンス・更新状況・バイナリ/ビルドコストで採否を決める。小さなcrate数を目標に自前のLua VM、JSON、汎用画像形式parserを作らない。一方で、ゲーム特有の丸め・pixel blend・I/O境界はこのプロジェクトが所有する。

| Dependency class | Policy |
|---|---|
| mlua / vendored Lua | vmだけが直接所有（任意依存であり、`backend-mlua` feature が有効な場合のみ実体化する）。現行baselineはmlua 0.10.5 + lua53 |
| Serde/JSON | cold-path adapterや開発ツールでは許可。spec/rasterには必須にしない |
| Error helpers | 標準Errorか小さなhelper。必要になるまで依存を追加しない |
| Image decoding | サンプル/host/toolを基本とし、raw pixel rasterの必須依存にしない |
| HTTP/async/GPU/UI | host/専用adapterのみ |
| Parallel scheduling | host。Rayonやpthreadsを基本featureへ入れない |
| Property tests/fuzz/bench | dev-onlyとして必要に応じ追加 |

`mlua`は最初の切り出しと更新を混ぜないため0.10.5に固定した。最新だという意味ではない。LuaJIT/Luau、send、asyncは有効にしない。vendored build依存のlockfileに別backend用のビルド補助crateが現れることと、LuaJITを実行backendとして選んだことは別である。

Cargo featuresは同じdependency上で加算されるため、別の利用者が`send`等を有効化した場合の影響を無視しない。[Cargo features](https://doc.rust-lang.org/cargo/reference/features.html)。当初は所有スレッド固定であり、backend featureを切り替える場合はcallbackのSend制約・性能・全consumerを確認する。

## Public types and raw backend

通常の公開APIはmlua型を隠す。`backend-mlua`featureはmicrocontrollerのAPI登録と独立backendテストに必要なLua/Table/Function/Value/String/Variadic/Result/Error等だけを明示的に公開する。これはmluaを何でも再exportするfacadeではなく、backend版と結びつく逃げ道である。API登録で追加型が必要になった際は、そのconsumerと共に追加する。

`backend-mlua`は第305ラウンド以降、型の再公開ゲートであると同時に**依存ゲート**でもある: mluaはvmの任意依存であり、このfeatureを有効にしたconsumerのグラフにだけ現れる。有効化するのは実行backendを必要とするconsumer（addon、microcontroller、wasm runtime、conformance、検証ハーネス）であり、コンパイラ専用consumer（build、minify、compiler-wasm）は有効化しない。feature無効時のvmは実行backendを持たず、ロード合法性判定は自前パーサとPUCロード規則チェッカによる代替判定になる（意味論と参照判定との残差は`load_legality`のモジュール文書が正本）。

## Update gate

バージョン変更時はNative/WASMビルド、整数/数値/bytes、エラー、サンドボックス、limits/debug、callback、描画パリティを確認する。Cargo.lockは開発・CIの再現性のため保持するが、利用アプリの依存解決をライブラリのlockfileだけで支配できるとは主張しない。

新しい直接依存は[architecture.json](../../tools/architecture.json)の所有者に追加し、機械検査を通す。transitiveな配布ライセンスはrelease前にCargo/npmの実際の解決結果から生成して確認する。

Playwrightは3browserの実テスト、TypeScriptはSDKの型検査/生成に必要なdevDependencies。runtime packageにその依存を持ち込まない。
