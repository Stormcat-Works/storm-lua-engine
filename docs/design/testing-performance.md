# テストと性能計測

## 契約と実装の独立性

画面の仕様と採用済み入力・期待値は本リポジトリに置きます。下流の利用アプリを正解判定に使わず、CIが他アプリをcloneすることもありません。現在の実装から期待値を再生成してテストを通す仕組みは設けません。

[743件のRGBA契約](../../fixtures/screen/README.md)は直接の描画命令と、同じ入力から組み立てたLuaのscreen呼び出しの2経路で比較します。Native、WASM、3ブラウザで、透明背景を含む全成分を検証します。入力をLuaへ変換する補助は期待値を生成しません。

元の観察資料を同梱する検証と、この固定契約への回帰検証は異なります。回帰テストを新たな実ゲーム測定と表現しません。新しい互換性の根拠は、適用版・入力・観測または導出・制限を明確にして、最小の自己完結したケースとして追加します。

## 検証の層

| 対象 | 検査 |
|---|---|
| Contract | schema、ケース数、ID、有限値、RGBA範囲と全画素、採用済みハッシュ |
| Numeric / property | f32境界、i64、bytes、非有限値、初期化とreset、停止中の更新 |
| Drawing | subpixel、clip、空形状、alpha、文字、巨大座標、直接経路とLua経路 |
| Runtime / debugger | sandbox、命令・heap制限、pause/step、stale handle、watch |
| Addon / host | mode分離、同期server、保存、event、map、HTTP、ログ配送 |
| Ownership | 不正ABI・upload・長さ、memory growth、borrowとcopy、再入 |
| Distribution | 独立npm install、権利表示、成果物の絶対パス検査 |
| Application example | 仮ワールドの決定性、コード編集、エラー、reload、import/export、実ブラウザ |

空のtest runnerや未取得のfixtureをPASS件数に加えません。生成物がなければ必要なテストは失敗させます。通常のPython/Cargoテストはローカルの契約だけで完結し、WASM/ブラウザテストはCIの専用jobで実行します。

## 期待値の変更手順

不一致の原因を調べ、独立した根拠を示し、変更する規則とケースを限定します。レビューした上で期待値とmanifestのハッシュを同じ変更で更新します。ハッシュ一致は改変の検出であり、意味論の正しさを証明するものではありません。手順を迂回する自動bless機能は提供しません。

## 性能

入力は[control.lua](../../fixtures/bench/control.lua)と[draw.lua](../../fixtures/bench/draw.lua)。Nativeは`cargo run --release -p storm-lua-conformance --example benchmark --locked`、WASMは`node tools/bench-wasm.mjs`を使用します。100回warm-up後の9batch平均の中央値と最大値で、個々のcallbackのp95ではありません。

Lua生成・load、tick、96×96の描画を分けます。Canvas/GPU upload、Worker通信、初回module compile/downloadは別です。既存の[測定値](../verification/performance.json)は記録当時のbaselineであり、最新コードの性能保証ではありません。

正確性を優先し、SIMDやshared memoryの導入は実測と互換性の検証を伴う場合に限ります。Lua自身のallocation/GCまでゼロコストと主張しません。[実行記録](../verification/repository.md)を参照してください。
