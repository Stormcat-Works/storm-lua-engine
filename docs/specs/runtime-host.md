# Runtime and host services

## 共通VM

PUC-Lua 5.3/mlua 0.10.5のstateを所有thread/Worker内で生成・実行・破棄する。Nativeの通常APIにSend、Rayon、特定async runtimeを要求しない。既定では各VMの乱数列は独立だが、実ゲームの未公開乱数列との一致を保証しない。

許可environmentだけでsourceを実行し、io/os/package/load/raw debugをscriptに暗黙提供しない。requireも既定はnilであり、extendedの明示source loaderだけが提供する。string.dumpも隠す。全source/callbackにLua命令予算とheap上限を適用する。gameではpcall/xpcallはnilとし、extendedで提供する場合も命令予算の捕捉回避を防ぐ。長時間のホスト関数の途中停止を保証するwall-clock timeoutではない。

`backend-mlua`のconfigureは信頼された拡張用。通常consumerは`HostFunction`／`LuaValue`、MapProvider、公開profile APIを利用でき、mluaを直接扱う必要はない。

## ビークルとAddon

vehicleはinput/output/property/screenとComposite、draw commandの所有者。Addonはserver/matrix/menu property/g_savedataとevent lifecycleの所有者。片方の環境へもう片方のAPIを単に追加する構造にはしない。

vehicleでは入力f32→Lua f64、出力Lua f64→f32。outputは保持し、複数drawでもLua stateを共有する。draw中のCompositeアクセスの詳細や異常引数の規則はエンジン契約と実ゲーム実測を区別する。[数値仕様](numeric-io.md)

Addonはtop-level終了後に既存checkpointを復元し、その後にonCreateを呼ぶ。tickのgame_ticksは引数として1回配送する。停止中・異なるlifecycle段階のeventを拒否する。[Addon仕様](addon.md)

## 同期hostサービス

world queryは同期で戻る必要がある。Nativeのserver callbackはowned LuaValue結果、WASMのserver callbackは同じ値のtagged codecを通してJSへ接続する。JSのPromise結果・同一moduleへの再入は拒否し、host側のイベントはcallback復帰後に配送する。

mapは描画命令列の中で同期providerを呼び、正確な寸法のRGBAを受け取る。描画省略、空の地形、未提供関数の成功を暗黙に返さない。ゲームのworld/terrain/タイマー/HTTP clientの実体はhostが提供する。

## HTTPとログ

HTTPはvehicleのasync.httpGet、Addonのserver.httpGetからbounded request queueへ記録する。hostがdrainして通信し、idleでhttpReplyを配送する。VM世代付きtokenによりstale/foreign/duplicate replyをrejectする。cancelは失敗したtransportを明示破棄する操作で、架空のLua返信を作らない。

loggingはVM側が共通所有する。制限付きdebug.logとprintの公開はprofile/opt-inで区別する。レコードはsourceとbytes、および取得できた場合のlocation（chunkと1始まりのline）。TSのonLogはLuaから復帰後に呼び、失敗したcallbackが出したログも取り出す。配送先の例外とLuaエラーは両方保持する。ログcallbackも同期である。Promiseの戻り値はエラーとして観測し、別の未処理Promise rejectionを発生させないよう、そのPromiseは結果を採用せず監視する。

各サービスの上限、利用側のネットワークpolicy、手動配送と自動配送の選択は[host services guide](../guide/host-services.md)を参照。

## 環境プロファイル

Luaの公開集合は[環境仕様](environments.md)を正本とする。game/extended、Vehicle/Addon、ホストdebuggerを独立して選ぶ。両環境のdebugテーブルはlogのみ。printはextended専用で、onLogを設定するだけでは追加されない。明示的なホストbindingsは高レベル生成APIから渡し、reset/reloadで再適用する。


<a id="ログ発生位置未公開の後続版"></a>

## ログ発生位置

発生位置はログ関数を実行した瞬間のLuaスタックからVMが記録する。onLog配送後のスタックを位置の根拠にしない。ログcallbackの上から最大16フレームを調べ、Cフレームを飛ばした最も近いLuaフレームの完全なチャンク名と正の現在行を保持する。Luaフレームがない、または最も近いLuaフレームの行を取得できない場合は位置なし。内部のline hook追加やスクリプトへのdebug.getinfo公開は不要。

RustのLogRecord.locationはOption<LogLocation>、構造化WASMのlocationはobjectまたはnull、TSではoptionalなLogLocation。TSは位置のない旧レコードも受理するが、不正な型・ゼロ・負数・非整数の行は拒否する。従来のbytesだけをdrainする経路は変えない。RustでLogRecordをstruct literalで構成する利用者はlocationフィールドを追加する必要がある。公開済みv0.2.0のtarballは変更しない。

チャンク名はソース識別子であり、ファイル読出しの許可ではない。ホストは実行時のソース世代に対応づける。未加工の名前付きチャンクには変換マップは不要。非短縮リンクは正確に同じcode/mapの組を使用し、最適化後の元ファイル位置をこの情報だけで復元できるとは扱わない。ラッパー関数は実際にログを呼ぶラッパー内の行を示す。列・同じ行の複数呼び出し・失われた末尾呼び出しの位置は推測しない。

## エラーの位置

スクリプトの読み込みと実行で失敗したエラーは、メッセージとは別に位置情報を持つ。ホストはメッセージ文字列を解析せずに、ファイルと行を特定できる。チャンク名と呼び出し履歴はスタックから取得する。構文エラーの行だけは、VMがLuaのメッセージから読む。debug featureを切っても位置情報は残る。

| 項目 | 内容 |
|---|---|
| stage | Luaエラーの段階。コンパイル中はsyntax、実行中はruntime。Luaエラー以外のカテゴリでは値なし |
| location | 省略されていないチャンク名と1始まりの行。ログのlocationと同じ形 |
| stack | 最大256件の呼び出し履歴。エラーを起こしたフレームが0。各フレームはチャンク名、行、取得できた関数名を持つ。関数名が不明な場合と、UTF-8でない場合は値なし |

VMは、失敗したコルーチンを破棄する前にそのスタックを読む。Luaはエラーで終了したコルーチンのスタックを巻き戻さないので、行フックやスクリプトへのdebug.getinfo公開は要らない。Luaがメッセージの中でチャンク名を60バイトに切り詰めても、位置情報のチャンク名は切り詰めない。

locationは、エラーを送出した最も近いLuaフレームを指す。Cの関数やホスト関数が送出した場合は、それを呼んだLuaの行になる。`error(message, 2)`のようにメッセージが呼び出し側の行を示す場合でも、locationは送出した行のままで、メッセージの行とは一致しない。呼び出し側の行はstackから読む。Cフレームの行は-1とする。末尾呼び出しで失われたフレームは推測しない。

構文エラーのstackは空で、locationのチャンク名にはホストが渡した名前をそのまま使う。行だけは、Luaがメッセージの先頭に付ける`チャンク表記:行:`から読む。メッセージがこの形式に一致しなければlocationは値なしとし、行を推測しない。requireが読んだチャンクの構文エラーでは、locationはそのチャンクを指し、stackはrequireを呼んだ側を指す。

命令数の上限とメモリ上限に達したエラーにも位置情報が付く。命令数の上限では、先頭フレームの行は上限を検出したフックが報告した実行行である。ホスト引数の誤り、失効したハンドル、failed状態のVMへの呼び出しのように、スクリプトの実行を始める前に拒否したエラーには位置情報が付かない。方言変換の段階で拒否したソースはLuaエラーだが、位置情報を持たない。位置情報の読み取りに失敗した場合も、元のエラーを別のエラーに置き換えず、位置情報なしで返す。

RustのVmError.traceはOption、WASMは`error_detail_ptr/len`のJSON、TSはEngineError.detailで、位置情報がなければundefinedとなる。RustでVmErrorをstruct literalで構成する利用者はtraceフィールドを追加する必要がある。Vehicle、Addon、同期コールバックのいずれのエラーにも付く。watch式の評価エラーには付けない。

結果コードの正本は`storm_lua_spec::abi::status`で、TSの`STATUS`はそこから生成する。stageがsyntaxとruntimeのどちらでも、結果コードはLuaエラーの1である。

ログ上限は本文1行16KiB、最大128件、未配送の本文とチャンク名の合計64KiB。位置メタデータもメモリ上限へ含める。上限超過時はエラーとし、取り出し済み/残存ログを成功時以外でも検査できる。Native/WASM、停止/失敗、include/reset、Addon、位置なし、バイト列、非短縮マップ、3ブラウザで検証する。


## 名前付きVehicle callbackと開発用状態操作

ホストはcall_tick/callTickまたはcall_draw/callDrawで、任意名のグローバル関数を通常のtick/drawフェーズと同じ命令予算・I/O・画面制約で実行できる。元のonTick/onDrawを置換せず、関数名をコードへ埋め込むloadもしない。名前付き描画の一時停止・再開では同一呼び出しのコマンド列を継続する。存在しないcallbackはMissingであり、成功した描画として偽装しない。

control_namespace/controlNamespaceはVehicleのextended限定・明示設定。未使用の単一ルート識別子を指定すると、そのテーブルへsetProperty、setInputNumber、setInputBool、getInputNumber、getInputBoolを登録する。既存の標準グローバルや重複/親子競合するホストbinding、requireLoaderとの競合は構築時に拒否する。設定しない場合、これらの関数は存在しない。

状態操作はEngineが所有する同じproperty/input状態を変更する。Luaから戻るまで更新を遅延せず、同じチャンク・callback内の後続の標準API読み取りが更新を見る。JSホストcallbackからWASMへ再入する操作ではない。setPropertyはバイト列ラベルとnumber/Boolean/byte string/nilを受け、nilは削除。プロパティ数4096・ラベルと値の合計1MiB以内。入力channelは整数1..32、numberの転送時のみf32化し、propertyのf64は縮小しない。getInput系は開発用入力状態の取得であり、ゲームAPIのフェーズ制約を変更しない。

実行後のWASM固定I/Oブロックは状態操作後の入力も反映する。次のtickへホストが別の入力を書けばその入力が新しい開始値になる。properties()は更新済みの損失のない所有スナップショットを返す。resetは現在のpropertyを引き継いでnamespaceを新しい状態へ結び直し、完了済みloadを再実行する。古いVMのStateを捕捉したクロージャを再利用しない。

アプリ固有のsim/LB API、GUIメタデータ、登録tick hook、パス解決、任意のtick/drawスケジュールはこのAPIの利用者が所有する。ゲーム環境の関数集合やデバッグAPIは追加しない。新しいsle_vehicle exportはABIの固定I/Oレイアウトを変更せず、古いSDK操作もそのまま利用できる。


## 開発実行向けの静的診断

Compilerのanalyze(project,{mode:'runtime'})は、Engineで直接実行する名前付きチャンク向けの解析である。構文・グローバル名・環境・既存lintは維持するが、ビルド用のrequire配置・静的依存グラフ・戻り値付きモジュール規則を適用しない。requireの文字列や動的式は実行ホストのloaderが実行時に解決し、その段階で未存在等のエラーを返す。runtime modeはbuild-time ambientを拒否し、開発用の名前はextendedとhostBindingsで明示する。

省略時のmode:'build'は既存のリンク前解析のままであり、旧consumerのビルド契約を緩めない。runtime解析成功はゲーム向けビルド成功を意味しない。Parserは共有であり、runtime診断のために別Luaパーサーや文面フィルターを作らない。


## Development overflow viewport

Vehicle callDraw accepts an optional integer margin. The raster remains Engine-owned, translates geometry into an expanded viewport, and keeps screen.getWidth/getHeight and host map requests at their logical sizes. Zero margin is the unchanged game frame contract. The output is GameRGBA, not premultiplied RGBA, and the frame remains subject to the same allocation limits. Normal draw delegates to zero margin; debugger continuation replays only new commands into the same viewport. Host rotation is a presentation transform of rendered pixels, not another primitive rasterizer.
## Execution limits

`ExecutionLimits` follows the actual game's model. The only limit enabled by default is the wall-clock time limit of 1001 ms per entry; exceeding it raises a Lua error with the game's own wording (`max execution time exceeded (1000ms). stopped at line <N>`), does not fail the VM, and cannot be caught forever by pcall. The instruction budget, the Lua memory limit and the source-length limit are disabled by default (the game has no such hard limits) and apply only when the host sets them explicitly. Enabling the source-length limit also enables the retained-program caps (128 chunks / 8 MiB) described in [source loading](source-loading.md).

## Explicit shared CRT random context

`Vm::install_crt_random` and `AddonConfig.random` accept a `SharedCrtRandom` owned by the host. Its `CrtRandomSource` supplies raw values in 0..=32767 and an exact `srand(u32)` operation. A value above that range or a provider error is reported as an execution error. The provider captures its own shared state; it must not re-enter the VM. The host checkpoints this state together with its scene consumers.

This optional mode applies Windows CRT Lua 5.3 sampling in the Engine: each `math.random` consumes one draw before validating its argument count and exact integer bounds. Zero arguments return a Number in [0,1); integer sampling scales that value once and truncates without rejection sampling. Invalid intervals still consume the draw. `math.randomseed` coerces its first argument to Number, truncates through signed 64-bit conversion (the native indefinite integer for nonfinite/out-of-range input), writes its low 32 bits, then discards exactly one draw. Additional seed arguments are ignored and the call returns no values.

Creating/reloading an addon does not implicitly seed or draw from the supplied context. Top-level script calls still execute normally. Cloning the config retains the shared resource. Omitting the provider selects the engine's built-in MSVC CRT stream, shared per host thread, which is the game-profile default (the actual game shares one CRT sequence per process). The isolated deterministic generator remains available as an explicit host option (`deterministic_random` on the Vehicle/Addon configs, or `Vm::use_deterministic_random`); the Extended profile always uses it. This Rust host API adds no WASM ABI provider.
