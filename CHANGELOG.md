# Changelog

## 未公開

### 依存関係の更新

- `bincode`（RUSTSEC-2025-0141で保守終了が宣言された）を外し、minifyの候補探索がWorker境界で使う符号化を、serde駆動の自前バイナリコーデック（`crates/storm-lua-minify/src/job_codec.rs`）へ置き換えた。公開関数のシグネチャと符号化の意味は変えていない。
- `sha2`を0.11へ上げ、`Cargo.lock`全体を最新化した。推移依存の`rustix`は1.1.6となり、RUSTSEC-2026-0486の対象外になった。
- `mlua`の0.11/0.12化は見送った。どちらの版でも`Thread::set_hook`の内部実装変更により、addonの`synchronous_events`テスト（`nested_lua_error_restores_debugger_and_preserves_non_tick_callbacks`）がLuaのアサーションで異常終了する。切り分けは済んでおり、`=0.10.5`の固定を維持する。
- npm側は`vite` 8.3.8、`wrangler` 4.162.0、`playwright` 1.65.1、`@types/node` 26へ上げた。miniflareが厳密に固定する`undici` 7.28.0と`sharp` 0.35.2は、appの`overrides`で7.30.2と0.35.5へ寄せた。
- `typescript`は5.8.3を維持する。5.9.3と7.0.2では`WebAssembly.instantiate`の戻り値の型が変わり、`packages/lua-engine/src/raster.ts`がTS2339で失敗する。

### matrix の実ゲーム整合

- matrix 実装を `storm-lua-addon` から `storm-lua-vm` の共有モジュールへ移し、addon は re-export で使う。Component Mod と共用する土台になる。
- `matrix.rotationToFaceXZ` を追加した（実ゲームの静的解析で式を確定）。`matrix.scale` は Component Mod 用の集合にだけ追加し、addon には入れていない。
- 行列引数の検証とエラー文字列を実ゲームに合わせた（数値キー16件の完全一致、`bad matrix size`、`matrix was nil`、`bad matrix input type`、`matrix index out of range`、`bad value in matrix`）。有限性は要求しない。引数の個数も identity を除き完全一致を要求する（`Wrong number of args`）。
- multiply の合成順序は、実ゲームのバイナリと現行実装が一致することを乱数行列200ケースの数値比較で確認したため変更していない。
- `multiplyXYZW` のベクトル成分を、実ゲームどおり f32 へ丸めてから計算するようにした。
### VMコア意味論の実ゲーム仕様への整合

- MC APIの引数意味論を実ゲームへ合わせた。引数の個数が合わない呼び出しはエラーにならず戻り値0個で無言に戻り、値の型はLuaの変換規則で変換され、変換できない値は0として扱われる。検査の順序は個数→phaseフラグ→変換。トップレベルとhttpReplyでもI/O・描画はoutsideエラーになる。Compositeチャンネルはcvttsd2si相当の変換となり、範囲外の大きな値やNaNでもエラーにならない。
- エラー後の回復モデルを実ゲームのエラーステータスへ合わせた。通常のLuaエラーではVMは失敗状態にならず、Vehicleはステータス（onDraw=1、onTick=2、ロード=3、httpReply=5）と最大11件のエラーリングを保持し、エラー中はonTick/onDrawをスキップする一方でhttpReplyは配送を続ける。Addonはステータス（onTick=2、ロード=3、それ以外=5）を保持し、ステータス5は報告後のtickで自動復帰する。どちらも`error_status`/`error_log`で参照できる。
- 実行制限を再構成した。壁時計1001msの時間制限を既定で有効にし、超過は実ゲームと同じ文面のLuaエラーになる。命令数・Luaメモリ・ソース長の上限は既定で無効になり、ホストが明示した場合だけ効く（`ExecutionLimits`の各項目は`Option`になった）。
- Gameプロファイルの`math.random`の既定を、実ゲームと同じCRT互換のプロセス共有ストリームへ変えた。VMごとの決定的ストリームは`deterministic_random`オプションで選べる。Vehicleの`MicrocontrollerConfig`にもホストCRTコンテキスト（`random`）と`deterministic_random`を追加した。
- Vehicle/Addon環境から`select`を除き（baseは6関数のみ）、`string.dump`を戻した（stringライブラリは完全版）。
- 関数でないコールバックは、エラーではなく「不在」として扱う。Addonの`tick(game_ticks)`は符号付き整数を受け付け、0以下でも拒否しない。

### フレームが変わったかどうかの確かめ方

- 描画の結果が前回と同じかどうかを確かめる例（`examples/consumer/frame-changes.mjs`）を追加した。`frame().pixels`の借用ビューを前回の写しと比べ、変わったときだけ`copy()`する。エンジンのAPIは変えていない。
- フレームのダイジェストを返すAPIと、エンジンが前回フレームを保持して比べるAPIは、試作して実WASMで測定し、上の手順より速くならなかったので追加を見送った。
### 必要なNodeのバージョン

- npmパッケージの`engines`を、`>=22.18`から`^18.19.0 || >=20.6.0`へ下げた。梱包したパッケージを独立consumerへ導入し、consumerを18.19.0、18.20.8、20.6.0、20.20.2、21.7.3、22.0.0、22.18.0で実行して確かめた。22.18以降という条件は、本リポジトリの開発とビルドだけに残る。
- `tools/test-package.mjs`に`--node`を追加した。梱包と導入は現在のNodeで行い、consumerのスクリプトだけを指定したNodeで実行する。CIは下限の2つの版でこの検査を実行する。
### 実行して確かめた例の追加

- CommonJSから使う例を追加した（`examples/consumer/commonjs.cjs`、`commonjs-types.cts`、`tsconfig.commonjs.json`）。動的`import()`で読み込み、WASMは`require.resolve`と`fs`で読んで`wasmBinary`へ渡す。TypeScriptは`module`と`moduleResolution`を`Node16`にし、型のimportに`resolution-mode`を付ける。
- ホスト関数に届くLuaの文字列が`Uint8Array`であることを示す例を追加した（`examples/consumer/host-bindings-bytes.mjs`）。文字列へ直す形と、バイト列のまま扱う形の両方を載せた。
- どちらの例も、`node tools/test-package.mjs`が独立consumerで実行する。パッケージのREADMEに2つの例への案内を足した。SDKのコードは変えていない。

### ホスト向けの小さな追加

- プロパティを1件だけ設定する`setProperty`と、1件だけ削除する`removeProperty`を追加した。Rustは`Microcontroller::set_property`と`remove_property`。全体を読んで書き戻す必要がなくなる。ラベルはバイト列のまま比較する。受け付ける時点は`setProperties`と同じで、実行中と中断中は拒否する。`setProperty`に`null`や`undefined`を渡しても削除にはならず、`TypeError`になる。
- パッケージの`exports`に`./compiler-wasm/*`を追加した。コンパイラのWASM（`compiler-wasm/compiler_bg.wasm`）の場所を、`require.resolve`と`import.meta.resolve`で求められる。
- `breakableLines`の構文エラーに位置情報が付くようになった。読み込みの構文エラーと同じく、`EngineError.detail`（Rustは`VmError.trace`）から省略されていないチャンク名と行を読める。
- runtimeのWASMは、既存の`sle_vehicle`に操作を足しただけで、exportの一覧とABI番号は変えていない。

### デバッガ

- ホストから実行を止める手段を追加した。`requestPause`は、実行していないVMに「次に実行する行で止まる」予約を置く。VM作成時の`pausePoll`は、実行中のVMが4096命令ごとに呼ぶ関数で、trueを返すと実行を中断する。無限ループに入ったスクリプトを、命令数の上限を待たずに止めて検査できる。
- `resume`に`carryToNextEntry`を追加した。ステップがコールバックの終わりを越えたとき、次に実行するコールバックの最初の行で止まる。既定では従来どおり、ステップはコールバックの終わりで消える。
- `breakableLines`を追加した。渡したソースについて、ブレークポイントを置ける行を入れ子の関数を含めて返す。ソースは実行しない。
- 行フックなしで始まった実行が中断した後に、ステップと後から置いたブレークポイントが働かない不具合を修正した。`resume`のたびにフックを付け直す。
- 中断したフレームの行を取得するとき、関数の最初の命令で止まった場合に行情報の手前を読んでいた不具合を修正した。フックが記録した行を返す。
- runtimeのWASMに`breakable_lines`を追加した。既存のexportとABI番号は変えていない。

### エラーの位置情報

- スクリプトの読み込みと実行で失敗したエラーが、メッセージとは別に位置情報を持つようになった。省略されていないチャンク名と行、呼び出し履歴、構文エラーと実行時エラーの区別を返す。Luaはメッセージの中のチャンク名を60バイトに切り詰めるため、従来は長いパスのファイルをメッセージから特定できなかった。
- Rustは`VmError.trace`、TypeScriptは`EngineError.detail`で受け取る。命令数の上限に達したエラーにも、実行していた行と呼び出し履歴が付く。debug featureは要らない。
- 結果コードの名前付き定数`STATUS`をTypeScriptへ公開した。`EngineError.code`と比較する数値を直接書かずに済む。
- runtimeのWASMに`error_detail_ptr`と`error_detail_len`を追加した。既存のexportとABI番号は変えていない。
- Rustで`VmError`または`BridgeError`をstruct literalで構成している利用者は、追加したフィールド（`trace`、`detail`）を指定する必要がある。`VmError::new`と`BridgeError::new`の呼び出しは変更不要。

### 実ゲームに合わせた挙動の変更（非互換）

- 呼べないphaseで呼んだ`input`、`output`、`screen`の関数が、実ゲームと同じ文言のLuaエラーになった。対象は、`onDraw`の実行中の`input.*`と`output.*`、`onTick`の実行中の`screen.*`である。従来は読み出しが0または`false`を返し、書き込みと描画は無視されていた。Stormworks v1.15.26の実機ではどれもスクリプトが停止するため、ゲームで止まるスクリプトがエンジンでは動き続けていた。
- トップレベルと`httpReply`での呼び出しは実機で測定していないので、従来の挙動のままにした。
- この挙動に依存して`onDraw`から`output`を呼んでいたスクリプトは、エラーで停止するようになる。コンパイラの診断`input-outside-ontick`と`output-outside-ontick`が、該当する呼び出しを事前に報告する。

## 0.4.1 — 2026-11-04

### 正確性の修正

- 目標文字数（targetSize）を指定した最短化探索で、探索の途中経過を保存して再開する経路（チェックポイント再開）と一括実行の経路で異なる結果が選ばれる不具合を修正した。どちらの経路でも、同じ入力には同じ出力が得られる。再開経路側の結果に合わせて一括実行側の出力を変えることはせず、公開済みの一括実行の結果を維持した。
- サイズ計測が印字結果より大きくなる不具合を修正した。印字時に1文へ統合される連続したlocal宣言と、印字時に除去される空のif文を、計測でも印字と同じ扱いにし、計測値が実際の出力長と一致するようにした。これに伴い、一部の入力では最適化パスの採否や候補の選択が変わり、出力が変わる場合がある（意味は保存される）。
- 計測の修正に合わせ、正確モード（numericMode: exact）で定数畳み込みが印字上は消える文に適用された場合でも、パス記録が正しく報告されるようになった。

## 0.4.0 — 2026-11-04

### Source Map schema 2（非互換変更）

- 最適化後Source Mapのx_storm形式を`schemaVersion: 2`へ更新した。字句短縮mapで重複していたmappings／originsの二重記録（約282B/token）をトークン表へ置き換えてmap容量を削減し、複数ソース・リネーム済みトークン・プロジェクト合成mapでもトークン表を適用する。schema 1のreaderでは読めないため、生成側と利用側の版を揃えること。詳細は[検証記録](docs/verification/schema-2-token-table-20261029.md)と[複数ソース対応の検証記録](docs/verification/token-table-multisource-mixed-20261030.md)。

### 正確性の修正

- 敵対的レビューで確認された一連の正確性問題（S1〜S11・S14）を修正した。対象はminifyの意味保存（数値リテラルの厳密さの向き、評価順序の保存、goto/labelを跨ぐ利用可能式の再利用、tickを跨ぐinput.*の安定性の前提ほか）、VM sandboxの整数強制、リンク時のロードエラー帰属など。詳細は`docs/verification/`の各検証記録。
- 方言としてロード違法なソースを、最適化で無言に修復せずコンパイル時に拒否するようになった（非vararg関数内の`...`など）。従来は受理されていた違法ソースがエラーになる。実世界コーパスでは2件が該当した。詳細は[検証記録](docs/verification/candidate-s10-s14-cycle73-20261102.md)。
- minifyのLua byte文字列・hex/64bit数値・型/overflow・スコープ/capture・API差し替え・描画helperの意味保存を修正。深すぎる構文はNative/WASMとも診断を返す。未存在tableキー、色候補探索、format候補割り当ての無駄を削減。安全な適用条件への限定で文字数が増える場合がある。詳細は[実装検証](docs/verification/minify-review-fixes-20261013.md)。
- Rust syntax utilityの`decode_lua_string`はバイナリ値を失わないよう`Option<String>`を返す。byte列は`source_tools::string_bytes`を使用する。
- `metadataTag`・`minify:false`・`sourceMap:true`を同時指定した通常/LifeBoatビルドがexact-copy検査に失敗する不具合を修正。空白化した範囲をコピー区間から除き、元のsourcesContentと残存コードの位置を保持する。Addonと標準mapも同じ除去範囲を使用し、metadataだけのmoduleでも元snapshotを保持する。

### 性能

- コンパイル時の候補試行と由来構築の割り当てを削減した（pool実体化の削減、fusion述語の試行前評価、tiny-literal試行のin-place化ほか）。決定的計数で試行のdetach総量は約2.59億要素から約2.02億要素へ減少した。これらの変更では生成Lua・Source Map・診断は不変であり、実世界コーパス全件（4,155ファイル×4条件）でバイト同一を検証した。
- targetモードでは、目標内に収まる候補が現れても速度レイアウトまで探索を続ける（部分複合レイアウトを含む）。targetなしの最高圧縮の挙動は変えていない。

## 0.3.1 — 2026-10-12

- コンパイルの解析・字句処理・候補評価・印字・Source Map生成で重複走査と割り当てを削減。Map有無で生成Lua、探索順、診断、Source Map v3とx_storm schema1の精度・理由を維持する。
- 字句短縮mapの標準アンカーと名前の重複排除を修正し、由来未取得mapと多数の反復識別子を含むmapの生成・読み込みを維持する。
- 入れ子の条件分岐を最適化する際、子の変換後に解析を更新し、NativeのpanicとWASM trapを修正。
- 描画の1/256px格子の中間座標とdrawCircleFの境界を、Stormworks v1.15.23の撮影に基づく契約へ合わせる。レビュー済み5ケースを修正し、12ケースを追加して743件へ拡張。
- AddonのGame環境からselectを除き、通常のLuaエラー後も保存状態とイベント配送を維持する。onTickは停止し、資源制限やホスト境界の失敗は引き続きVM再作成を要求する。
- 非短縮のAddon include-onceビルドを`buildAddon`として追加。Vehicleの最適化・8192文字制限を適用せず、元ソース入りSource Mapを返す。
- Vehicleの通常/LifeBoatビルドへ`metadataTag`を追加。明示した先頭metadata commentだけを除去し、元位置とsourcesContentを保つ。
- Rustの`ApiProjectCompileOptions`に`metadata_tag`が追加された。構造体を全フィールド指定する利用者は`None`または既定値を指定する。内部candidateのバイナリ/JSON、JS glueとWASMは同じcompiler版で利用する。
- 全入力でMapあり/なし1.2倍以内にする研究目標、map容量・追加キャッシュ等の改善はv0.3.2以降へ継続する。実測した条件と限界は公開候補の検証記録に示す。

## 0.3.0 — 2026-10-02

### 最適化後Source Mapと説明情報

- `minify` / `build` / `buildLifeboat`の`sourceMap:true`で、生成Luaに対応するSource Map v3と`x_storm.schemaVersion:1`を返す。通常のminifyはopt-inで、非短縮buildの標準mapは従来どおり返す。
- Rust、compiler専用WASM、TypeScript型、CompilerWorkerClient/serveCompilerを同じ版へ更新。`validateSourceMap(code,map)`で生成コード・原文snapshot・設定の指紋、範囲・参照、標準mapとの整合性を検証する。
- 全67最適化パスで元の名前・トークン・式・文・複数の寄与元を保持。元情報が不足する場合はUnknown、制御/保存の補助コードはSyntheticとして区別する。原文をそのまま返す経路は検証済みcopyとして区間内部も正確に対応する。
- 変換理由、実際に記録したbasis/facts、型付き関連元、インライン文脈、除去/置換記録、producerの版/revision/dirtyを出力する。形式的な正当性証明、元変数値や消えたフレームの復元は含まない。
- 複数moduleの元位置をリンク前のファイルへ合成する。標準mapの列はUTF-16、詳細範囲は半開UTF-8バイト区間である。

### Playgroundと利用例

- 原文/生成コードの双方向選択、理由・関連元・削除記録、通常/LifeBoat module対応、実VMのpause/step/log/errorの生成行との接続を追加。列が分からない実行位置は複数候補として示し、推測しない。
- CLIの`--source-map`と`map-inspect`、成果物JSONの検査を追加。閲覧だけではLuaを実行しない。
- 選択・生成物・結果をIndexedDBの単一workspaceへ保存し、portable workspace JSONで移せる。旧localStorageの端末保存は自動変換せず救出可能な明示拒否とする。入力project JSON v1は継続受理する。
- identity/copyの選択が元ファイル全体へ広がる問題を修正。Unicode/CRLF、カーソル、Shift・方向キー・Home・End、再読み込みを検証した。
- `examples/consumer/optimized-source-map.mjs`に、公開SDKからのmap生成・検証、標準reader、詳細な理由・関連元の参照例を追加。

### 性能・互換性

- サイズ比較だけのscope rename、画面呼び出し共有の試行、translated/affine wrapper試行で、捨てる候補の由来構築を削減。採用した変換のみを元の由来付き入力へ適用する。短縮結果、候補順、マップ内容を維持する。
- 代表30入力・240設定でUnknown 0%、map有無の生成Lua一致を確認。性能改善前後もproducer/integrityを除くmap全体が一致した。これは任意入力の完全性や実行時間の上限を保証しない。
- 低レベルRustの`Ast.nodes`は由来失効を管理する`NodeArena`へ変更。内部candidateのJSON/binary context、JS glueとWASMは同じcompiler版で使用する。
- 描画命令ABI、Composite I/O、Addon savedataの形式と既定のgame/extended契約は維持する。
- 追加の性能・map容量改善はv0.3.1以降。schema1を今回変更せず、位置・理由の精度も落とさない。

## 0.2.1 — 2026-09-30

- `targetSize`が未達の場合、同じ意味論・pass設定の最大探索（`searchMode:exhaustive`、targetなし）と同一の生成コードを返す。`searchMode:fast`やbeam幅の指定で未達時の品質を落とさない。targetなしのfastは従来どおり候補を限定する。
- 低レベルRustの`LinkedRange`にbyte範囲を追加。opaqueなWorker search contextは同じ版のcompiler間で使用する。描画命令ABI、Composite I/O、savedata形式は維持する。


- `targetSize`未達時の探索を全探索と共通化し、実行済みのコア最適化・候補評価を繰り返さない。Worker継続でも評価済み候補を保持する。時間による探索打ち切りは導入しない。
- 非短縮の通常/LifeBoatビルドのSource Mapをトークン・列単位へ詳細化し、UTF-16座標へ変換する。診断の元モジュール・列・終端も実際のコピー範囲から求め、合成コードと削除済み開発区間は未対応位置として明示する。最適化後mapはまだ返さない。

- IDE用のトークン/AST宣言検査、明示的LB include-onceビルドと開発専用区間除去、描画の開発用overflow viewportを追加。このIDE向け追加は通常game描画を変更せず、minify後ソースマップの実装とは分離する。

- Lua実行で利用できる可変長引数のみの`function(...)`をCompilerも正しく受理する。旧TypeScriptパーサー由来の閉じ括弧の消費漏れを修正。

- 識別子位置で記号や予約語を受理していた構文解析を修正し、`local =`等を構文エラーとして報告する。

- Vehicleの名前付きtick/draw callback、extended限定の明示controlNamespace、更新済みpropertyスナップショットを追加。同一Lua呼び出し内の状態変更をWASM再入なしに標準APIへ反映する。

- ログ生成時のチャンク名と実行行をRust LogRecord、構造化WASM、TS LogRecord.locationへ追加。Luaへのdebug API公開や常時line hookを追加しない。
- 取得不能な位置は省略し、旧ランタイムの位置なしログもTSで受理する。最適化後ソースマップを提供する変更ではない。
- ログの64KiB蓄積上限へチャンク名のbytesも含める。RustでLogRecordを直接構成する利用者はlocationフィールドが必要。

## 0.2.0 — 2026-09-27

Compiler SDK、実行環境の選択、開発用ソース読み込み、Playgroundをまとめたリリースです。

### 追加

- Vehicle向けCompiler SDKを責務別クレートと`/compiler`へ統合。解析、lint、静的リンク、minify、プロパティ走査、独立したコンパイラWASMを提供。
- ホスト所有のWorkerへ接続する`/compiler-worker`アダプタ。
- 任意のホスト値・同期関数を高レベルAPIから追加・置換する`bindings`。
- extended専用の`requireLoader`。ホストが供給するテキストを別名付きチャンクとして共有環境で実行し、include-once・戻り値破棄・入れ子・ファイル単位のデバッグに対応。
- `app/`にStorm Lua Engine: PlaygroundのCLI/Webと静的配信Worker設定を追加。Storm MinのCLI/Webと併存。

### 非互換・挙動の変更

- 既定をgame環境へ変更。debug.logは利用可能で、pcall/xpcall/error/print/assert/グローバルunpackはextendedへ分離。onLogは公開関数を増やさない。
- Vehicleのresetは最後のソースだけでなく、正常完了した全loadを順に再実行。失敗・未完了のloadは履歴に入れず、履歴を128件/8MiBに制限。
- 単一ソースとプロジェクトの環境診断を統一。未知・廃止のパスIDはfalse指定でも拒否。
- general-expression-factoring、repeated-expression-factoring、scalar-vector-loop-synthesis、redundant-nil-fallback-eliminationを撤去。
- 独立Addon Labを削除し、SDK確認をPlaygroundへ移行。仮物理/3Dワールドは移さない。

### 修正

- ソースに代入のない外部グローバルを短名またはnilへ変換しない。
- 動的_ENV・拡張環境・ホスト置換は、トークンと行位置を保持する字句短縮へ切り替え、conservative-minificationで通知。
- クロージャに捕捉されたproperty/inputの値を後から再取得する変換を防止。
- 不正な元ソースを公開finalizerへ渡した場合のpanicを診断へ変更。
- 非公開fixtureの生成を通常コンパイラビルドから分離。

- 非短縮リンクの合成行を架空の元行へ割り当てない。元ファイルのbreakpoint・step・エラー位置を往復するconsumer例とNative/WASM回帰を追加。

### ドキュメントと配布

- PlaygroundはCloudflare Workers Buildsからreleaseブランチpushでビルド・自動更新。設定とビルド処理はapp/で管理。npm公開は独立した明示操作。
- 隔離パッケージのSource Map検証はlockfileのtarballを再利用し、新規CIのnpm metadata cacheに依存しない。

- 利用者向けガイド本文を[docs.makkii.jp](https://docs.makkii.jp/storm-lua-engine/index)へ移設。リポジトリには契約・設計・検証・公開手順と実行可能な例を維持。
- Rust workspace、npm SDK、Playgroundを0.2.0に統一。savedata形式、描画命令ABI、Composite I/Oレイアウトは変更しない。


## 0.1.0 — 2026-09-26

Storm Lua Engineの初回リリース。

- RustとTypeScript／WebAssembly向けに、Lua 5.3の実行基盤を提供。
- ビークルとAddonを独立したAPI・実行環境として提供。Composite I/O、プロパティ、コールバック、イベント、savedataに対応。
- CPUラスタライザ、ビットマップフォント、Luaを含まない描画専用WASMを同梱。731件の固定RGBAケースを直接描画と実Lua経路で検証。
- ブレークポイント、ステップ実行、変数・テーブル検査、watch、ログのホスト接続を提供。
- Addonの同期server関数、地図プロバイダー、HTTP要求・返信を利用側の実装へ接続。
- npm SDK、RustのGitタグ、Node／Rust／ブラウザの利用例、Three.jsとCodeMirrorによるAddon Labを提供。
- 配布用の権利表示、toolchain通知、WASM・export・パス検査と独立したnpm consumer検証を整備。

ゲームのワールド、物理、地形、実際のネットワーク通信はホスト側の責務です。対応APIと制約は[利用ガイド](docs/guide/getting-started.md)、追加の検証・実装項目は[TASKS](TASKS.md)、未信頼コードの実行条件は[SECURITY](SECURITY.md)を参照してください。
