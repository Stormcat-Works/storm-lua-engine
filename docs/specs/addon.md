# Addon profile contract

## 責務と対応範囲

`storm-lua-addon`がAddonの名前空間、初期化順、イベント、menu property、HTTPキューを所有する。`storm-lua-vm`の制限・debugger・owned values・loggingを共有し、microcontrollerやrasterへ依存しない。

Addonの通常環境には`server`、`matrix`、Addon用`property`、制限付き`debug.log`、`g_savedata`がある。`input`、`output`、`screen`、vehicleの`async`と`property.get*`は登録しない。base関数は`ipairs`、`next`、`pairs`、`tonumber`、`tostring`、`type`。`select`は通常のAddon環境へ登録しない。`print`は開発支援のopt-in。OS・ファイル・package loader・raw debugを公開しない。

server関数はホストが名前と同期実装を明示登録する。エンジン自身が登録するserver関数は`server.httpGet`。他の名前に固定値・成功stubを作らない。登録名は識別子、最大512個、1名128bytes以下。`httpGet`は予約され上書き不可。

これは汎用的なホスト接続面であり、すべてのゲームserver APIについて引数・戻り値を検証済みにしたという意味ではない。ホストが各関数の署名・世界に対する副作用・権限を実装する。

## Lifecycle

Empty → Loading → Loaded → Creating → Ready → Callback → Ready、終了はReady → Destroying → Destroyed。Luaが停止した場合はその段階を保持してresumeする。別のcallbackやstartは重ねない。通常のLuaエラーは呼び出し元へ返して保持し、以後のonTickを抑止するが、同じVMの通常イベント・onCreate・onDestroyは実行できる。sourceの実行がLuaエラーになった場合もcheckpointの復元とonCreateへ進める。命令数・メモリ制限やホスト境界の失敗は再利用せず、明示的にreload／recreateする。callbackのglobalがfunction以外なら未定義として扱い、Luaの返値は捨てる。errorのpeerへの報告と、通常callbackエラーの報告後のtick再開は現在のホストAPIでは未対応。

新規ワールドではtop-levelの初期g_savedataを保持する。既存ワールドではcheckpointを事前検証し、top-levelが完了した後にg_savedataへ適用してからonCreate(false)を実行する。checkpointの適用で古いテーブル参照・upvalueを自動書き換えしない。

`reload(checkpoint)`は同じsourceを既存ワールドとして読み直す。新VMの構築・loadが成功した場合に置換し、startは別に呼ぶ。external hostの副作用はrollbackできない。VM構築・ロード前の設定失敗と、外部サービスが既に実行された後の失敗を同一視しない。

`onDestroy`はホストが明示駆動する。disposeで勝手に実行しない。HTTPの返信はReady時のみ配送し、busyの拒否ではtokenを消費しない。再作成で旧HTTP generationとdebug handlesは失効する。

## APIと精度

実装済みmatrix関数はidentity、translation、position、multiply、transpose、invert、distance、multiplyXYZW、rotationX/Y/Z。列優先の16要素table、移動成分はLua index 13〜15、計算はf64。逆行列の特異行列は明示エラー。行列恒等式・典型入力・Native/WASMを検証するが、実ゲーム全入力のbit一致は保証しない。`rotationToFaceXZ`はまだ未実装で、一般的な式を推測して対応済みにしない。

menu propertyはcheckboxとslider。新規ワールド時はhost overrideまたはdefault、既存ワールド時はnil。宣言は`menuProperties`／`property_definitions`から取得できる。矛盾する重複宣言、不正な範囲・型はエラー。自動clampや刻みへの丸めはしない。

一般イベントは[callback catalog](../../fixtures/addon-api-catalog.json)に列挙する。onCreate/onTick/onDestroy/httpReplyは専用操作。可変長の引数列と末尾nilを保持し、不要なf32量子化を行わない。

## Owned value boundary

Lua値はnil／bool／i64／f64／bytes／tableの所有データに変換する。tableはscalar keyのentry listで、疎配列と混在キーを保持する。nil値のentry、重複キー、NaN key、table key、循環、metatable、function/thread/userdataは拒否する。共有subtreeのidentityは保持せず、値として複製する。

1転送につき最大深さ32、65,536 nodes、合計string bytes 1MiB。control JSONは4MiB、host responseは16MiBなど、adapter側の追加上限も適用される。複雑なLua heap全体のシリアライズではない。

JSの携帯形式は`format: storm-lua-addon-savedata`、`version: 1`。整数は10進文字列、浮動小数点はIEEE754 bit列、文字列はbytesのタグ付き表現とする。旧形式を黙って変換せず、不一致はrejectする。ゲームXMLとの直接互換形式ではない。

## 参照した公開資料と確度

調査日: 2026-09-25。WikiWikiに掲載された**ゲーム内ヘルプの英語原文**を参照し、訳注は原文と分けて扱った。

| 原文掲載ページ | 表示されている対象版 | 確認した範囲 |
|---|---|---|
| [General](https://wikiwiki.jp/sbarjp/アドオンLua/General) | v1.15.23 | savedata、座標軸、標準ライブラリ |
| [Callbacks](https://wikiwiki.jp/sbarjp/アドオンLua/Callbacks) | v1.15.20 | lifecycle・event名・経過tick引数・HTTP返信 |
| [Matrices](https://wikiwiki.jp/sbarjp/アドオンLua/Matrices) | v1.15.23 | 公開された関数署名 |
| [Misc](https://wikiwiki.jp/sbarjp/アドオンLua/Misc) | v1.15.20 | menu propertyと既存world時のnil、HTTP API |

公開文書は引数・戻り値・ライフサイクルの根拠であり、実ゲーム内の新規実行測定ではない。復元の全edge case、数値の保存精度、行列の極端な入力などは独立実測が必要。描画の契約はこのリポジトリのscreen仕様と採用済みケースが所有する。
