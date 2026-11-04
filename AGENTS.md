# Development rules

このリポジトリ単体で開発・検証できる状態を維持する。現在地は[STATUS.md](STATUS.md)、未完了作業は[TASKS.md](TASKS.md)、仕様の入口は[docs/README.md](docs/README.md)。

## Ownership and boundaries

- 意味論の所有者は[architecture](docs/design/architecture.md)。1つの処理に1つの所有者を置く。
- 公式アプリは[Playground設計](docs/design/playground.md)に従い`app/`で管理する。Storm MinのCLI/Webとは併存し、SDKを試す範囲を超えるアプリ機能を増やさない。
- Cargoの依存宣言はすべて[architecture.json](tools/architecture.json)で管理し、`cargo xtask check`を通す。optional依存、target固有依存、dev依存も対象とする。
- 製品からconformance/xtaskへ依存しない。描画専用経路はmlua、Lua処理系、DOM、GPU、HTTPへ依存しない。
- WASM境界は変換・所有権・呼び出しのみを担当し、ゲームの描画式やLua API規則を再実装しない。
- vehicleとaddonは別profile・別API型。server関数や地形データはhostが明示提供し、未提供を成功stubで埋めない。
- 利用者向けガイドの本文はdocs.makkii.jpのstorm-lua-engine配下で管理する。docs/guideは移設先への案内だけを保持し、契約・設計・検証・release手順は本リポに残す。公開例は本リポの独立consumerで実行検証する。
- 公開APIを一括re-exportしない。mlua型へのアクセスはVMの明示的な`backend-mlua`機能に限定し、通常のAPIとは分ける。
- クレート数、LOC、関数数を目的に分割しない。利用条件と独立検証可能な意味論で決める。

## Semantics and safety

- 描画は[screen仕様](docs/specs/screen.md)の参照契約に従う。一般的なpremultiplied alphaへ置換しない。
- Composite Numberはf32、Lua Numberはf64。入出力境界以外のLua計算をf32へ落とさない。
- フォントは同梱する。実行・ビルド時にゲーム資産の探索やダウンロードをしない。
- 不正なホスト入力・UTF-8・ABI・サイズ・ハンドルを空値へ隠蔽しない。ゲーム互換の既定値は別の明示契約とする。
- Lua文字列はバイト列として保持する。整数64bit、非有限数、負のゼロをJSONで暗黙に破壊しない。
- workspace lintを継承する。unsafe例外はFFI境界、または将来の明示的なdebug backend初期化に局所化し、安全条件と専用テストを付ける。unsafe impl Sendを追加しない。
- hot pathの不要な割り当て・JSON変換を避けるが、Lua自身の割り当てやGCが消えると主張しない。
- 未実装機能を成功・空フレーム・ゼロ値で模倣しない。実装済みAPIだけをexportする。

## Documentation and validation

- 規約はAGENTS、挙動はspecs、構造はdesign、判断理由はADR、現在地はSTATUSへ書く。同じ決定本文を複製しない。
- 私有リポジトリ、個人の絶対パス、固有マシンの設定を公開資料の前提にしない。第三者の著作権・出典表示は維持する。
- 基本ゲートは[CONTRIBUTING.md](CONTRIBUTING.md)。変更した担当者がテスト・ビルドを実行する。
- Native/WASMの一致と実ゲームへの適合を区別する。実行0件、fixture不在、未実装をpassとして数えない。
- commit・push・publishは明示指示なしに実行しない。Gitの破壊的操作や他作業の変更破棄をしない。
