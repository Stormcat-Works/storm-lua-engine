# Component Mod profile contract

## 責務と対応範囲

`storm-lua-component`がComponent Mod（コンポーネント埋め込みLua）の名前空間、ライフサイクル、エラーステータスを所有する。第3のプロファイルであり、Vehicle/Addonとは別のスクリプトクラスとして扱う。`storm-lua-vm`の制限・debugger・owned values・loggingを共有し、microcontrollerやaddonへ依存しない。

通常環境には`component`、`parser`、`matrix`、`debug.log`と、3系統共通のbase subset（`ipairs`、`next`、`pairs`、`tonumber`、`tostring`、`type`と完全な`math`/`table`/`string`。`string.dump`を含む）がある。`server`、`property`、`input`、`output`、`screen`は登録しない。`select`は登録しない。`print`は開発支援のopt-in（extended環境）。OS・ファイル・package loader・raw debugを公開しない。

APIは64関数。`matrix`の13関数（addonの12関数に`scale`を加えたComponent集合）と`debug.log`はエンジンが実装し、`storm-lua-vm`の共有モジュールを使う。残りの50関数（`component.*` 47、`parser.*` 3）はホスト提供であり、`ComponentProviders`のsub-trait（LogicSlots、TorqueSlots、FluidSlots、ElectricSlots、Sfx、FluidContents、Heater、Render、Physics、EnvironmentQuery、ParseHandlers）でドメインごとに渡す。提供されたドメインの関数だけが登録され、未提供を成功stubで埋めることはしない。カタログの正本は`storm-lua-spec::component::FUNCTIONS`であり、availability（implemented / requires-provider）と登録状態の一致をconformanceで照合する。

## Lifecycle

Empty → Loading → Loaded → Parsing → Parsed → Adding → InSimulation、tick/renderはInSimulation内の遷移、終了はInSimulation → Removing → Removed。順序外の呼び出しはエラーにする。Luaが停止した場合はその段階を保持してresumeする。

駆動は`load` → `parse`（`onParse`）→ `add_to_simulation`（`onAddToSimulation`）→ `tick(game_ticks)`（`onTick`）/ `render`（`onRender`）→ `remove_from_simulation`（`onRemoveFromSimulation`）。`game_ticks`は整数で渡し、0も拒否しない。コールバックが未定義、または関数でない場合は「不在」として扱い、ステータスは変わらない。

通常のLuaエラーは呼び出し元へ返し、実ゲームのエラーステータスを`ComponentStatus`に記録する（onTick失敗=2、ロード失敗=3、それ以外のコールバック失敗=5）。実行時間制限の超過のうち、tick（`onTick`）の実行中に起きたものだけは数値を主張しない`TimedOut`として表し、load・parse・add_to_simulation・render・remove_from_simulationの実行中に起きたものは各段階の失敗ステータス（ロード=3、その他=5）で表す。Addonと違いステータスの自動復帰はない。失敗したインスタンスはホストが作り直す。tickだけがステータスゲートを持ち、ステータスが正常でない間は`onTick`を実行せずMissingを返す。renderとremove_from_simulationにゲートはなく、tick失敗後も`onRemoveFromSimulation`は配送される。命令数・メモリ制限やホスト境界の失敗はVMを失敗状態にし、そのインスタンスは再利用しない。

## lua_filename のホスト契約

スクリプトの読み込み先を決めるのはホストであり、エンジンに新しいAPIは作らない。ホストはコンポーネント定義XMLから`lua_filename`属性を抽出し、(1) パッケージ内アセット、(2) 実ファイルの順で解決し（実ゲームのcreate_luaの二段引きと同じ順序）、得たバイト列を属性値そのままのチャンク名で`load(bytes, lua_filename)`へ渡す。チャンク名はエラーの発生位置にそのまま現れる。ファイル読みで得たソースは、定義XML経由の切り詰め（0x2000バイト）を受けない。コンポーネント1個につき`Component`インスタンスを1個作る。

## APIと引数意味論

引数の個数が合わない呼び出しは、Luaエラー（`Wrong number of args`）になる。唯一の例外は共有matrixの`matrix.identity`で、実ゲームの実装に個数検査が無いため、余分な引数を渡してもエラーにならない（Componentでも共有モジュールの挙動をそのまま使う）。値の型は無言で変換され、整数・数値は変換できない場合に0、boolは`lua_toboolean`相当（falseとnilだけが偽）になる。table必須の引数（matrixとComposite）だけは型不一致がLuaエラーになる（matrixは共有モジュールの文面、Compositeは`lua_table_to_composite: invalid table`）。

`debug.log`は関数単位の例外で、引数の個数が合わない場合はエラーになるが、型は無言であり、文字列と数値以外は記録されない（上限・件数の制限はない）。これは他の2系統の`debug.log`（個数を検査しない）とは異なる、Component系統だけの契約である。

phase制限は入れない。実ゲームにphase制限の記述は確認されておらず、カタログの`phase`は情報目的の記録である。

Composite信号のLua表現は`number_values`と`bool_values`のサブテーブル（キー1〜32、欠けた要素は0/false）であり、Rust側は`CompositeSignal`（32 number + 32 bool）と相互変換する。`bool_values`の読みは実ゲームで確定している。`number_values`は対称形の想定である。

`parser.parseBool/parseNumber/parseString`は純粋な変換ではなく、ホストのparseハンドラ越しの定義データアクセスである。第1引数のkeyに対応する値を型別に読み書きし、方向（読み/書き）はホスト側のパース状態で決まる。返値は（値、成功bool）の2個で、パース期間外は`false`が1個だけ返る。keyは無言で文字列化され、値は型ごとに無言変換される（boolはtoboolean、numberは変換失敗で0、stringは文字列化）。parseNumberの値は定義データ側の精度に合わせてf32で受け渡す。

返値の形がまだ確定していないAPIは、trait署名に想定を置く。対象は`physicsRaycast`（ホスト所有の値をそのまま返す）、`physicsGetLinearVelocity`系（3数値）、`getWindVelocity`（3数値）、`getSubmergenceFactor`（1数値）、`spawnProjectile`（返値なし）、流体スロットの流量解決（引数の意味と返値）、`heaterSetOrientedBox`と`renderLaser`のmatrix引数の位置である。これらは実ゲームでの追加測定が済むまで確定契約とはしない。

## TypeScript SDK（WASM）

TypeScript SDK では `LuaEngine.createComponent({providers})` が WASM セッションの Component モード（mode 3）を作り、`ComponentVm` が `load` → `parse()` → `addToSimulation()` → `tick(gameTicks)` / `render()` → `removeFromSimulation()` と `status()` を駆動する。`tick` の `gameTicks` は Rust 側 `Component::tick` に合わせて符号なし整数である（Addon の符号付きとは異なる）。`status()` は `{status, code}` を返し、tick の timeout（`TimedOut`）のときだけ `code` が null になる。

ホスト提供ドメインは、addon の `server.*` のような名前登録ではなく、ドメイン単位の型付きプロバイダとして供給する。セッション設定にはドメイン名（`logic`、`torque`、`fluid`、`electric`、`sfx`、`fluidContents`、`heater`、`render`、`physics`、`environment`、`parse`）だけを渡し、実体はホスト登録側に置く。WASM 境界では固定署名の同期コールバックとして転送され、値の転送は `value_codec` の型付き形式に揃える。Composite と matrix は Lua テーブルと同じ形（`number_values` / `bool_values` サブテーブル、キー 1〜16）で運び、TS 側では `CompositeSignal`（32 number + 32 bool）と 16 数値の配列に変換する。`physicsRaycast` の返値はホスト所有の値をそのまま運ぶ。

parser は専用の転送で、方向は Rust 側のパース状態が決める。TS 側の parse ハンドラは key と現在値を受けて（値, 成功）の組を返し、パース期間外は null を返す（Lua へは `false` が 1 個だけ返る）。

Component には HTTP API が無いため、`drainHttpRequests()` は常に空を返し、`httpReply` / `cancelHttp` は Unsupported（code 6）で失敗する。Rust の `ComponentConfig` に require loader の入口が無いため、`createComponent` の `requireLoader` は黙って無視せず明示的に拒否する。

生成カタログには `COMPONENT_API_CATALOG`（64 関数とコールバック 5 件）と、environment カタログの `componentRoots` / `componentCallbacks` が含まれる。build/minify パイプラインの Component 入口は `buildComponent`（Rust の `storm_lua_build::component::build_component`）で、addon と同じく include-once リンクのみを行い、最適化（短縮化）は行わない。

## 参照した公開資料と確度

64関数の名前・引数の個数と型、コールバック5件、エラーステータスの値、`debug.log`の例外、parserの方向性と返値の個数は、実ゲーム v1.15.26 の挙動で確認した内容に基づく。公開文書ではなく、実ゲームの挙動を正本とする。未確定のまま残る項目は上記の想定一覧と、Componentのtimeoutステータスの数値、Compositeの`number_values`キー名であり、推測で確定扱いにしない。
