# phase違反の呼び出しに対する実ゲームの挙動（2026-11-15）

`onDraw`の中の`input`と`output`、`onTick`の中の`screen`を呼んだときに、実ゲームがどう振る舞うかを測定した記録である。この結果を根拠に、エンジンの[契約](../specs/numeric-io.md)を「0/falseを返す、書き込みを無視する」から「Luaエラー」へ変更した。

## 測定の条件

| 項目 | 内容 |
|---|---|
| ゲーム | Stormworks v1.15.26 64Bit |
| 環境 | macOS、Apple M5 |
| 測定日 | 2026-11-13（input、screen）、2026-11-15（output） |
| 測定者 | エンジンを利用する側のプロジェクトの開発者 |
| 構成 | マイコンの映像出力を3x3のモニターへ接続し、モニターのコンポジット出力をマイコンへ戻す |

検証用のスクリプトは、ページごとに1種類の呼び出しだけを実行する。画面の上半分に触れるまで問題の呼び出しを実行しないので、スクリプトが停止するページも1つずつ確かめられる。`onTick`で読んだ入力値を比較用に常に表示する。`screen`の関数はトップレベルで`S=screen`と変数に取り、その変数を通して呼んだ。スクリプトと写真は利用側プロジェクトが保管しており、本リポジトリには含めない。

## 結果

7種類の呼び出しのうち、対照を除く6種類すべてでスクリプトが停止した。モニターは青いエラー画面になり、`DRAW_FAILURE`または`TICK_FAILURE`と、行番号つきのエラー文を表示した。

| 呼び出し | 実行した場所 | 結果 | 撮影できたエラー文 |
|---|---|---|---|
| 対照（何も呼ばない） | onDraw | 停止しない | なし |
| `input.getNumber(1)` | onDraw | 停止、`DRAW_FAILURE` | `80: attempting to use input function outside onTick` |
| `input.getBool(1)` | onDraw | 停止 | 撮影していない |
| `output.setNumber(1,123)` | onDraw | 停止、`DRAW_FAILURE` | `83: attempting to use output function outside onTick` |
| `output.setBool(1,true)` | onDraw | 停止、`DRAW_FAILURE` | `84: attempting to use output function outside onTick` |
| `screen.getWidth()` | onTick | 停止 | 撮影していない |
| `screen.drawRectF(40,60,20,20)` | onTick | 停止、`TICK_FAILURE` | `53: attempting to use screen function outside onDraw` |

先頭の数字は、検証用スクリプトの中でその関数を呼んだ行である。マイコンにカーソルを合わせたときのツールチップにも、`draw error`の見出しと同じエラー文が表示された。

## 分かったこと

- 呼べないphaseでの呼び出しは、既定値を返すのではなく、スクリプトを停止させるエラーである。
- 文言は、関数の属する表ごとに決まる。`input`と`output`は`outside onTick`、`screen`は`outside onDraw`となる。同じ表の別の関数（`setNumber`と`setBool`）は同じ文言だった。
- ゲームは表を消しているのではなく、関数が呼ばれた時点のphaseを調べている。トップレベルで変数に取った`screen`を`onTick`から呼んでもエラーになった。

## 測定していないこと

- トップレベル（`onTick`と`onDraw`のどちらの外でもない場所）で呼んだときの挙動。
- `httpReply`の中で呼んだときの挙動。
- `input.getBool`、`screen.getWidth`、および測定していない他の`screen`の関数のエラー文。停止することは確かめたが、文言は撮影していない。エンジンは、同じ表の測定済みの関数と同じ文言を使う。
- 引数の型が不正な呼び出しで、型のエラーとphaseのエラーのどちらが先に出るか。
- `output`の書き込みが、エラーで停止する前に信号へ反映されるか。エンジンは反映しない。
- WindowsなどmacOS以外の環境。

## エンジンでの検証

`conformance/tests/phase_errors.rs`（Native）と`packages/lua-engine/tests/wasm/phase-errors.test.mjs`（実WASM）が、上の表の呼び出しについてエラーの文言と行、スクリプトの停止、書き込みが起きないことを確かめる。未測定のトップレベルと`httpReply`については、従来の挙動が変わっていないことを確かめる。
