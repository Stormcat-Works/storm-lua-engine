# Host debugger and execution control

## Three separate concerns

| Concern | Owner | Policy |
|---|---|---|
| Runtime errors, source identity, execution limits | vm | 通常実行でも必須。debug featureを切っても消えない |
| Breakpoints, stepping, stack/locals/upvalues/watch | vm `debug` feature | ホスト専用。UIやDAPへ依存しない |
| `print`/`debug.log`など開発支援Lua関数 | 明示的なホスト拡張 | ゲーム標準APIと混同せず、debug機構の有無から勝手に追加しない |

マイコンのtick/draw phaseと保留描画命令はmicrocontroller、VSCode/DAP/Monaco、ソースマップ表示、キー操作はホストが所有する。数行のコードのために別debug crateへは分割しない。

## Execution state

同じVMでIdle→Running→Completed/FailedまたはSuspendedへ遷移する。Suspendedでは同じcontinuationを保持し、continueで先頭から再実行しない。新しいtick/draw、resetなしの別チャンク実行、property変更は拒否する。reset/disposeはcontinuationと全debug handleを無効化する。

top-levelを含むすべての実行入口に制限を適用する。ブレークポイント、一時停止、命令予算は**1つのhook管理者**で合成し、debugを有効にして制限フックが上書きされないようにする。命令予算はresume間で累積する。現在はinstruction/heap制限であり、壁時計timeoutは未実装。停止中の待ち時間を命令予算として消費しない。

ステップはinto/over/outを設計対象とするが、未実装の種類をintoで代用して成功扱いしない。call/returnイベントは深さの追跡、停止はyield可能な境界で行う。外部C/Rust処理を安全に途中停止できるとは保証しない。[Lua manual §4.7](https://www.lua.org/manual/5.3/manual.html#4.7)。

## Pause requests

ホストが実行を止める手段は、ブレークポイントとステップのほかに2つある。どちらも止まった結果はブレークポイントと同じSuspendedで、同じcontinuationを保持する。

| 手段 | 呼べる時点 | 止まる位置 |
|---|---|---|
| 一時停止の予約（request_pause） | 実行していないとき。アイドルまたは中断中 | アイドル中の予約は、次に実行する入口の最初の行。中断中の予約は、再開後の次の行 |
| 実行中の問い合わせ（pause poll） | VMが実行中に自分で呼ぶ | 問い合わせにtrueが返った後、最初にyieldできる命令 |

予約は1つだけで、止まった時点で消える。存在しないcallbackの呼び出しでは消えず、次に実行する入口まで残る。cancel_pauseで取り消せる。reset/disposeは予約を破棄する。ステップが入口の終わりを越えると、ステップの状態は消える。resumeに持ち越しを指定した場合だけ、同じ予約へ引き継ぎ、次に実行する入口の最初の行で止まる。Continueには持ち越しが働かない。入口はtick/drawに限らず、load、httpReply、任意名のcallTick/callDraw、Addonのcallbackを含む。同期callbackの中では止まらない。

実行中のVMには、ホストから呼び出しを届けられない。WASMは同じmoduleへの再入を拒否し、共有memoryを前提にしないためである。そこでホストは、VMの作成時にpause pollを渡す。VMは4096命令ごとにそれを同期で呼び、trueなら止まる。別スレッドからの要求は、ホストがSharedArrayBufferなどでこの関数へ伝える。渡さないVMは問い合わせをしない。この間隔はVMの定数である。実WASM（Node.js 26、Apple M5）で、1回のtickが約600万命令を実行するループを測った。問い合わせは1 tickあたり1464回で、tickの所要時間の中央値は問い合わせなしが134〜140ms、ありが136〜144msだった。差は測定のばらつきの範囲に収まり、3回の測定で1回あたり7µsを超えなかった。

pause pollがtrueを返した時点でyieldできない場所（table.sortの比較関数の中など）にいた場合は、yieldできる命令に戻るまで実行を続けてから止まる。pollで止まる位置は行の途中になり得る。表示する行は、止めたフックが報告した実行行である。pollがエラーを返すと、続行にも停止にも読み替えず、実行中のスクリプトへホストエラーとして送出する。pause pollはVehicleだけが受け取り、resetをまたいで保持する。resetが保持済みのソースを再生している間は問い合わせない。

行フックは、ブレークポイント、ステップ、予約のいずれかがあるときだけ付ける。行フックなしで始めた実行がpollで止まった後でもステップと後から置いたブレークポイントが働くよう、resumeのたびにフックを付け直す。

## Breakable lines

breakable_linesは、渡したソースについてブレークポイントを置ける行を昇順で返す。行フックが発火し得る行、つまりコンパイル結果のいずれかの命令に対応する行である。空行、コメントだけの行、命令を持たない宣言の行（`local function f()`の開始行など）は含まない。

VMは実行と同じ方言変換のあとでソースをコンパイルし、非stripのバイトコードダンプから、入れ子の関数を含む全関数の行情報を読む。`debug.getinfo`のactivelinesは関数1つぶんしか返さないため使わない。方言変換は改行を増減しないので、行番号は元のソースと一致する。ソースは実行せず、VMにも保持しない。アイドル中と中断中のどちらでも呼べる。構文エラーはLuaエラーで返す。ダンプの形式が想定と違う場合は、空の一覧を返さずUnsupportedとする。

返した行は「置ける」行であり、必ず止まる行ではない。その行の命令に到達しない実行では止まらない。関数を作る命令はその関数のendの行に付くなど、行の割り当てはLuaのコンパイラに従う。返さなかった行のブレークポイントは止まらない。この対応は、各行に1つずつブレークポイントを置いて実際に止まった行と突き合わせるテストで確かめる。

## Inspection and values

source IDと行番号を保持し、名前が不明な関数は不明と返す。内部変数名をUIの都合だけで捨てず、raw inspectionで確認可能にする。tablesは停止中のhandleとpage単位の列挙にし、循環するLua heapを全体JSON化しない。handleはVM identity、pause epoch、slotで識別し、resume後に他のオブジェクトへ誤接続しない。

整数はi64、数値はf64、文字列はbytes。JS APIはbigint/number/Uint8Arrayを用いる。JSON経路では整数をタグ付き10進文字列、bytesを明示エンコード、f64の非有限値/-0をbit表現などのタグ付き形式にする。`JSON.stringify`をLuaの値へ無条件に適用しない。[Lua values](https://www.lua.org/manual/5.3/manual.html#2.1)。

通常の検査はraw readとし、`__index`/`__pairs`/`__tostring`を実行して副作用を起こさない。任意watch式の評価は別の明示操作とし、副作用を起こし得ることをホストへ示す。別の環境テーブルに式を置くだけでは、共有tableや関数の変更を防げないため、read-onlyだと主張しない。watch自身にも命令・メモリ・出力量の上限を設ける。

## Isolation and frames

本物のLua debug libraryやraw VMアクセスをスクリプトへ渡さない。実装時にunsafeなbackend初期化が必要な場合は、専用moduleに閉じ、スクリプトから到達しないことをテストする。

停止中の描画命令は未完了のprefixであり、完成フレームと区別する。continue時にprefixを再発行して二重alpha blendしない。部分出力・部分描画を表示する場合はデバッグ用snapshotとして明示する。

## Implemented status

`debug` featureにbreakpoints、into/over/out、一時停止の予約と実行中の問い合わせ、入口をまたぐステップの持ち越し、置ける行の一覧、stack/locals/upvalues、世代付きtable展開、制限付きwatch評価を実装した。Native、実WASM、Chromium/Firefox/WebKitで停止・再開とi64を検証する。

通常実行ではdebugをcompileしただけでline/call hooksを付けない。breakpoint/step使用時にのみ追加し、instruction budget hookを維持する。watchによるhookの置換後はresume時に制限を保守的に継続する。

中断したフレームの行は、止めたフックが記録した値を返す。フックからyieldしたフレームに対して`debug.getinfo`の現在行を照会しない。Luaがそのフレームのプログラムカウンタを1つ巻き戻しており、関数の最初の命令で止まった場合に行情報の手前を読むためである。

line breakpointはsource名と行番号、空sourceは全chunk一致。Cの非yieldable領域で無理に停止しない。raw inspectionは上限付き、完全なDAP serverやtime-travel debuggerではない。
