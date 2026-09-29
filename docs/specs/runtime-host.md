# Runtime and host services

## 共通VM

PUC-Lua 5.3/mlua 0.10.5のstateを所有thread/Worker内で生成・実行・破棄する。Nativeの通常APIにSend、Rayon、特定async runtimeを要求しない。各VMの乱数列は独立だが、実ゲームの未公開乱数列との一致を保証しない。

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


## ログ発生位置（未公開の後続版）

発生位置はログ関数を実行した瞬間のLuaスタックからVMが記録する。onLog配送後のスタックを位置の根拠にしない。ログcallbackの上から最大16フレームを調べ、Cフレームを飛ばした最も近いLuaフレームの完全なチャンク名と正の現在行を保持する。Luaフレームがない、または最も近いLuaフレームの行を取得できない場合は位置なし。内部のline hook追加やスクリプトへのdebug.getinfo公開は不要。

RustのLogRecord.locationはOption<LogLocation>、構造化WASMのlocationはobjectまたはnull、TSではoptionalなLogLocation。TSは位置のない旧レコードも受理するが、不正な型・ゼロ・負数・非整数の行は拒否する。従来のbytesだけをdrainする経路は変えない。RustでLogRecordをstruct literalで構成する利用者はlocationフィールドを追加する必要がある。公開済みv0.2.0のtarballは変更しない。

チャンク名はソース識別子であり、ファイル読出しの許可ではない。ホストは実行時のソース世代に対応づける。未加工の名前付きチャンクには変換マップは不要。非短縮リンクは正確に同じcode/mapの組を使用し、最適化後の元ファイル位置をこの情報だけで復元できるとは扱わない。ラッパー関数は実際にログを呼ぶラッパー内の行を示す。列・同じ行の複数呼び出し・失われた末尾呼び出しの位置は推測しない。

ログ上限は本文1行16KiB、最大128件、未配送の本文とチャンク名の合計64KiB。位置メタデータもメモリ上限へ含める。上限超過時はエラーとし、取り出し済み/残存ログを成功時以外でも検査できる。Native/WASM、停止/失敗、include/reset、Addon、位置なし、バイト列、非短縮マップ、3ブラウザで検証する。
