# ADR 0001: Modular foundation

Status: accepted for the foundation, 2026-09-24. Implementation completion is tracked separately.

## Decision

機能所有者として4crate（spec/raster/vm/microcontroller）、WASM境界として2crateを採用する。test-only conformanceとxtaskを製品DAGから分け、依存を機械検査する。詳細な現行責務は[architecture](../design/architecture.md)に集約する。

NativeはCargo source依存、TSは1つのnpm packageと2つのWASM成果物を使う。高レベルAPIと低レベルの借用/batch APIを両方サポートする。Linear Memoryはコピー回避の手段であり、Worker/表示/関数呼び出しまで0µsになるという主張は採用しない。

描画の契約はこのリポジトリが所有し、フォントデータを必ず同梱する。Composite信号はf32、Lua内部はf64/i64として境界でのみ変換する。描画規則と式は[screen](../specs/screen.md)、数値契約は[numeric I/O](../specs/numeric-io.md)を正本にする。

Propertiesを信号へ混ぜず、host debuggerをLua標準APIへ露出させない。プロパティの実ゲーム保存精度・欠損getterの未確認挙動は確認済みと装わず、実装時にfixtureで確認することを条件とする。

ゲーム互換の最適化テストはエンジンを使い、汎用Luaの独立oracleは直接backend利用を許容する。コンパイラ製品へVMの強制依存は追加しない。

## Alternatives and consequences

単一crateへの集約はraster-only利用者へC Lua/Emscriptenを強制しやすいため採用しない。一方、巨大アプリのレイヤー数・移行台帳をそのまま写した構成も採用しない。crate追加は意味論と独立した利用条件で決める。

raw pointerだけのAPIは安全な導入を難しくし、全APIのJSON化はhot pathの変換と情報損失を増やすため、両極端を避ける。cold pathの構造化変換は許容する。

標準のpremultiplied alphaへ統一する案、全領域f64のComposite、必須Send/Rayon、必須no_std、全処理ゼロ割り当て、未測定の固定性能SLAは採用しない。SIMDやshared memoryは実測と適合テストを伴う後続判断。

## Implementation scope

このADRは共通化の基本方針を扱う。Addon、ホスト接続、回帰契約の具体化は後続ADRと現行仕様を参照する。実装済み/未実装と今回の検証結果は[STATUS](../../STATUS.md)を正本とし、決定本文へ重複して記載しない。
