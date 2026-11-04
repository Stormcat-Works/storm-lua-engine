# Security and untrusted Lua

エンジンはallowlistとinstruction/heap制限を持つが、OS process分離の代わりになる保証ではない。`backend-mlua` escape hatchはホスト自身が責任を持って使うmluaの直接アクセスであり、ゲーム互換環境や実行予算を自動付与しない。

製品ランタイムは許可APIだけを登録した環境で実行し、OS、ファイル、package loader、raw debug、任意ネットワークアクセスを暗黙に提供しない。ホストのデバッガが持つ検査能力をLuaコードへ公開しない。

命令数制限はLua実行を止めるための機構であり、長時間のC/Rustコールバックを途中で安全に止める保証ではない。描画サイズ・命令数・文字列長・HTTP要求数・メモリを各所有者が制限する。Worker隔離とOSプロセス隔離は別物であり、Native組み込み時の防御境界を誇張しない。

公開WASM APIは無効な長さ・版・ハンドルをエラーとし、所有権と寿命を検証する。同一インスタンスへの同時実行・再入は禁止する。生ビューを渡す低レイヤーAPIでは、ホストがメモリを任意に変更できることを明記する。

脆弱性を公開issueへ報告するときは、認証情報・私有コード・ゲーム資産を添付しない。公開リポジトリでprivate security reportingが有効な場合はその窓口を優先する。応答時間の保証は設けていない。

## Host services and persistence

hostのserver関数・地図provider・ログ配送先は信頼されたアプリケーションコードであり、Luaのsandboxがそのコード自体の権限を制限するものではない。同じWASM moduleへの再入を拒否し、fresh I/O viewもcallback中に取得させない。すでに渡したraw viewや生exportsを直接扱うhostは、実行中の読み書き禁止と所有権契約を守る。

HTTPはデータの要求キューであり、Luaから任意ネットワークへ接続しない。ホストはport/path/権限/redirect/timeout/sizeを制御して、配送可能な時点でのみreplyする。Promiseを返す同期サービス・ログcallbackは明示エラーにする。

owned valuesとsavedataはdepth/node/byte上限を持ち、循環・metatable・関数・不正キー・semantic duplicate keysを拒否する。携帯可能なcheckpointはVMやworld全体の保存ではなく、host側のworld状態/source/設定の保存責任を置き換えない。

## Development source loader

extendedのrequireLoaderは信頼されたホストからテキストと名前を供給する明示機能です。SDKはファイル検索・HTTP取得をしません。resolverは同じVMへ再入せずソースを返し、読み込み先はLuaの同じ継続・予算で実行します。1件と合計のソースサイズ・件数を制限し、bytecodeを拒否します。許可名、パスの対応、ソース世代はホストが管理します。[契約](docs/specs/source-loading.md)。
