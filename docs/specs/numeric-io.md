# Numeric and Composite I/O contract

## Domains

| Domain | Representation | Conversion |
|---|---|---|
| Composite Number | `[f32; 32]`, WASM `Float32Array` | wire/buffer domain |
| `input.getNumber` return | Lua Number (f64) | exact widening from stored f32 |
| Lua arithmetic/registers | f64 Number or i64 Integer | no global f32 quantization |
| `output.setNumber` argument | Lua numeric value | convert to Number, narrow to f32 when written |
| Property Number | f64 host storage | independent contract; see [properties](properties.md) |
| Debug value | i64/f64/bytes | lossless tagged representation, never Composite quantization |

Lua 5.3の標準構成は64bit整数と倍精度浮動小数点を使用する。[Lua manual §2.1](https://www.lua.org/manual/5.3/manual.html#2.1)。本workspaceは選択backendの実サイズをcompile-time assertする。

入出力はホストのf32信号を正本とする。例えばLua内部の`16777217.0 - 16777216.0`は1だが、`output.setNumber(1,16777217.0)`が保存する信号値は16777216。`input.getNumber`はその保存済みf32をf64として返す。f64の入力ビューを作って精度があるように見せない。

Rustの`CompositeSignal`はドメイン型であり、ABIへのメモリcastを許可しない。WASM用の[IoBuffer](wasm-abi.md)は別の`repr(C)`型で、Booleanには0/1のu8を使う。

## Channels and lifecycle

ホストAPIでは1〜32を`Channel`へ検証する。Lua側の引数意味論は実ゲームの静的解析で確定した契約に従う。検査の順序は（1）引数の個数、（2）phaseフラグ、（3）値の変換である。個数が合わない呼び出しはエラーにならず、戻り値0個で無言に戻る（`screen.getWidth`/`getHeight`/`drawClear`は個数の検査自体がない）。値はLuaの変換規則で変換され、変換できない値は0として扱われる（数値文字列は変換され、真偽値は0になる。`output.setBool`の値だけは真偽値として解釈される）。チャンネルはcvttsd2si相当でi32へ切り捨て変換され、1〜32の範囲外は読み出し0/false・書き込み無効となる。これはゲーム互換の既定値として確定した契約であり、ホストAPI側の不正入力を隠すものではない。

呼べないphaseでの呼び出しは、実ゲームの測定に合わせてLuaエラーとする。スクリプトは通常の実行時エラーと同じく停止する。

| 呼び出し | 実行中のphase | 結果 |
|---|---|---|
| `input.*`、`output.*` | onDraw（callDrawを含む） | `attempting to use input function outside onTick`、`attempting to use output function outside onTick` |
| `screen.*` | onTick（callTickを含む） | `attempting to use screen function outside onDraw` |
| `input.*`、`output.*`、`screen.*` | トップレベル、httpReply | 個数が正しければonDraw/onTick中と同じ文言のoutsideエラー（I/O・描画フラグは0） |

文言の先頭には、Luaの実行時エラーと同じ`チャンク表記:行:`が付く。行は、そのAPIを呼んだLuaの行である。ゲームが調べるのは関数が呼ばれた時点のphaseで、トップレベルで変数に取っておいた関数を別のphaseで呼んでも同じエラーになる。引数の個数が合わない呼び出しはphaseフラグより先に無言で戻り、型の変換はphase検査の後に行われる。game環境とextended環境で挙動は同じである。測定の条件と写真の出典は[検証記録](../verification/phase-errors-ingame-20261115.md)に残す。トップレベル・httpReplyの扱いはdocs-internalのLua解析（静的確定）に従う。

出力は毎tickゼロクリアせず、次の書き込みまで保持する。入力はホストが次のcallback開始前に設定する。tick/drawを同じVM上で並列実行せず、drawのために入力APIを暗黙に有効化しない。draw中の入出力APIはエラーとなるため、drawが出力を書き換えることはない。実行失敗時の部分出力は完成したtickと区別する。未確認のエラー時出力をロールバックする実装を先に足さない。

## Non-finite and reproducibility

基盤のf32/f64バッファ変換はNaN、±Infinity、-0をJSONや有限値への変換で失わない。NaN payloadのbit一致やすべての数学関数の全CPU bit一致は保証しない。実ゲームの非有限値フィルタが追加で確認された場合は、対応するAPI境界だけに明示する。

fast-math、浮動小数点式の勝手な再結合、描画への一律floor適用を禁止する。境界fixtureとNative/WASMテストは別々に維持する。
