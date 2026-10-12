# API・候補探索・描画系パスの追加レビュー

対象は `5a168d6`（v0.3.1）の当日 metadata/source-map 修正を含む作業ツリー。製品コードは変更していない。以下の行番号はその対象ツリーのもの。Lua 5.3（mlua 0.10.5、vendored）で元コードと公開 `storm_lua_build::public_api::compile` の結果を別々に実行し、input/property を固定供給して出力・描画呼び出し列を比較した。実ゲームの画素一致試験ではない。

証跡の `cases.jsonl` は完全な入力、options、ホスト設定、実行順を保持し、`results.jsonl` は生成コード・コンパイラ診断・両者の実行結果を保持する。以下はすべて公開 API から再現する。F1〜F6 は `ok:true` の生成コードの誤動作、F7 は正しい入力のコンパイル失敗である。

## F1 — [P1] 合成識別子が利用者の束縛と衝突する（既定設定）

対象:

- `crates/storm-lua-minify/src/passes/api_aliases.rs:174-177`
- `crates/storm-lua-minify/src/passes/screen_call_factoring.rs:107-109`
- `crates/storm-lua-minify/src/passes/output_loops.rs:141-146`
- `crates/storm-lua-minify/src/passes/screen_loops.rs:624-629`

合成名を `intern` するだけで、既存の名前・スコープとの衝突を避けていない。後段の scope rename はすでに合流した束縛を復元できない。

最小に近い公開 API 再現:

```lua
function onTick() __api0=input.getNumber(1) end
function onDraw()
 screen.drawText(0,0,__api0)
 screen.drawText(0,1,__api0)
 screen.drawText(0,2,__api0)
end
```

生成コードは `a=screen.drawText; function onTick()a=input.getNumber(1)end; ...a(0,0,a)...` となる。入力 1 を与えた後の onDraw が、元コードでは `1,1,1` を描画し、生成コードでは number の呼び出しエラーになる。

同じ欠陥を別々のパスでも確認した:

| 証跡 case | 既存の名前 | 元の結果 | 生成後 |
| --- | --- | --- | --- |
| F1-api-name-collision | `__api0` | `1,1,1` | number の呼び出しエラー |
| F1-color-name-collision | `__sc0` | 42 を 8 回描画 | number の呼び出しエラー |
| F1-output-name-collision | `__stormmin_output_values_0` | 最後の出力は 1 | 合成配列に上書きされる |
| F1-screen-name-collision | `__stormmin_screen_index_0` | 1 を 8 回描画 | `0,1,2,3,4,5,6,7` |

修正方向: 全合成名を既存の全識別子と照合する決定的な fresh-name allocator で生成する。`draw_sequences` などが既に行っている全インターナー名の収集も参考になる。ローカル捕捉も含め、全合成名の生成箇所を横断して回帰確認する。

## F2 — [P1] button helper の引数化が条件付き評価と副作用順序を変える（既定設定）

対象: `crates/storm-lua-minify/src/passes/screen_buttons.rs:425-430`、同 `323-329`。

もともと `hit(...)` → `active` → `pulse` の判定後に評価していた代入 RHS を、helper 呼び出しの第 4 引数 `site.value` に移す。このため hit が変えるグローバルを古い値で読み、実行しない分岐の式まで先に評価する。label/x/y の読み取りも元の色変更・hit との順序から動く。

F2-button-eager-value は 3 個のボタンを持ち、hit が `value=value+100` を行う。各ボタンの条件成立時に `value=value+1 / +7 / +13` を実行する。元の最終描画は **321**、既定 minify 後は **21**。x/y は onTick の入力から取得し、先行する定数畳み込みにより問題が隠れない通常の動的座標を使っている。

追加の単独パス・公開 passToggles 試験では、元は通らない `if pulse then value=bad+1 end`（bad は table）の RHS を先行評価し、生成後に初めて arithmetic error が発生する例も再現した。

修正方向: 条件下の RHS を無条件の関数引数へ移してよいという証明が必要。例外可能性、読み取り対象に対する hit/on/off の書き込み、読み取り回数・順序を検査する。証明できない場合は抽出を行わないか、元の評価位置で実行する構造を維持する。

## F3 — [P1] button helper の root 移動が lexical local を別の global に変える（既定設定）

対象: `crates/storm-lua-minify/src/passes/screen_buttons.rs:323-329`、同 `355-364`、同 `461-467`。

helper を root の先頭に挿入する一方、元の `active`、`pulse`、代入先、callee ノードをそのまま使用する。元の callback 内 local が、新しい helper 内では同名 global として解決される。すべての自由変数・代入先が root から参照できるという検査がない。

F3-button-local-capture の onDraw は `local value=0` を持ち、`function kept() return value end` にも capture させている。このため別の function-globalization が誤変換を偶然隠せない。3 ボタン操作後、元の最終描画 **21** に対し既定生成コードは **0**。onDraw のパラメータが value/active/pulse である例でも再現した。

修正方向: 抽出先のスコープから全 free binding が同じ binding に解決されることを確認する。代入先 local の capture を保持できない場合は適用不可にする。F2 の評価順検査とは独立の修正が必要。

## F4 — [P1] callback slotting が初期化時に呼ばれる関数を削除する（既定設定）

対象: `crates/storm-lua-minify/src/passes/callback_slots.rs:352-378`。

root の呼び出しは owner `0` として calls に収集されるが、採用判定は `reachable(onTick)` と `reachable(onDraw)` しか参照しない。初期化時にも呼ばれる helper を callback 専用と誤判定し、root の定義を削除して callback 内へ移す。

F4-callback-init-call は通常の property 初期化値を 55 個保持し、top-level で `tickHelper(10)` を一度呼んで状態を初期化する。onTick は同 helper を使い、onDraw は別 helper を使う。55 個の live binding は、名前を共有する候補がサイズ上で勝つ条件を作る。元コードは正常に初期化・tick・draw を実行するが、生成コードは初期化中の **nil 関数呼び出し**で停止する。

修正方向: root 初期化、他の entrypoint、escape した closure などからの到達を除外して初めて callback 専用と判定する。callback 同士の再入・直接呼び出しも共有可能性の前提として別途点検する必要がある。現時点の実行再現は root 初期化の欠落についてのもの。

## F5 — [P1] screen loop の f64 等差判定が exact モードでも 64-bit 整数を破壊する

対象: `crates/storm-lua-minify/src/passes/screen_loops.rs:72-89`。

すべての `Node::Num` を `num_val` で f64 化し、その値だけで等差列・同値を判定する。`9007199254740992` と `9007199254740993` は Lua 5.3 では異なる整数だが、この判定では同値になる。

```lua
function onDraw()
 screen.drawText(0,0,9007199254740992)
 screen.drawText(0,5,9007199254740993)
 screen.drawText(0,10,9007199254740992)
 screen.drawText(0,15,9007199254740993)
 -- 証跡は同じ交互パターンを計 8 呼び出し使用
end
```

生成コードは `for a=0,7 do screen.drawText(0,a*5,9007199254740992) end` となる。F5-screen-i64-default と F5-screen-i64-safe-exact の両方で確認。`mode:"safe", numericMode:"exact"` でも発生する。整数を text 引数として使うため、描画座標の f32 丸めの問題ではない。

修正方向: Lua の integer/float を区別した数値表現で証明する。最低限、f64 で正確に表現できない integer literal はこの affine 変換から除外する。`draw_records::Atom` の整数を保つ処理は同じ境界ケースを保持できた。

## F6 — [P1] API member の書き換えを builtin の証明が無視する（既定設定・safe/exact）

対象: `crates/storm-lua-minify/src/passes/api_aliases.rs:40-45`、同 `99-109`、`crates/storm-lua-analysis/src/effects.rs:401-421`、`crates/storm-lua-minify/src/passes/property_reads.rs:50-60`。

`node_write` は member 代入の Index ノードをここで期待する write として扱わないため、`screen.drawText = function...` の左辺まで API alias の bare name に書き換わる。また EffectAnalyzer の builtin root invalidation は root 変数への write だけを調べるため、member を置き換えた後も元の builtin として分類する。

F6-api-table-mutation は drawText を自作関数に差し替え、`screen[property.getText("fn")]()`（fn は drawText）を 1 回、静的な drawText を 3 回呼ぶ。その回数を次の onTick で output に送る。元は **4**。生成後は動的呼び出しが古いホスト API に流れ、**3** になる（記録 stub では余分な `dynamic` 描画も観測）。既定設定と safe/exact の両方で確認。

F6-property-function-mutation では `property.getNumber=function(k)return 7 end` があるのに、hardcode option `numbers.X=42` がその後の呼び出しを 42 に置き換える。元出力 **7** に対し **42**。

修正方向: 代入/関数宣言の左辺を文脈付きで扱い、Index の代入先を値読み取りとして alias 化しない。root/member の書き換えやエイリアス経由の mutation がある場合は、その builtin の不変性・純粋性・hardcode 可否を無効化する。この問題は F1 の fresh name 修正では解決しない。

## F7 — [P2] 結果を捨てる property 呼び出しを hardcode すると不正な文になる

対象: `crates/storm-lua-minify/src/passes/property_reads.rs:33-41`、同 `55-74`。

```lua
property.getNumber("X")
function onTick() output.setNumber(1,1) end
```

`{property:{mode:"hardcode",numbers:{X:42}}}` で `Callstat(Call(...))` の子だけを数値に置き換え、`Callstat(Num(42))` を残す。Lua の文として 42 は無効なので既定探索の全候補が再パースで失敗し、`ok:false` / `No semantically valid optimization candidate survived verification.` を返す。getBool/getText でも同様。targetSize 0/8192 では `satisficing checkpoint failed reparse validation` になる。

修正方向: 文脈を保持して置換し、結果が不要な pure property 呼び出しは文ごと削除する。呼び出しの副作用・エラーを消せることは F6 の builtin 証明と合わせて確認する。

## 性能 — color subset 探索の 12 group 境界

対象: `crates/storm-lua-minify/src/passes/screen_call_factoring.rs:214-241`。

eligible group が 12 個以下の場合、全 `2^N-1` 部分集合について AST 複製とプログラム全体の renamed size 計測を行う。13 個になると greedy 探索に切り替わり、入力が増えたのに大幅に速くなる。

Rust release consumer、sourceMap 無効、各色 8 回、初回ウォームアップを捨てた 6 サンプルの中央値:

| 色 group 数 | Lua サイズ | 単独 pass の中央値 |
| --- | --- | --- |
| 9 | 2036 文字 | 9.602 ms |
| 10 | 2260 文字 | 20.076 ms |
| 11 | 2484 文字 | 43.972 ms |
| 12 | 2708 文字 | 95.144 ms |
| 13 | 2932 文字 | 2.777 ms |

12 group は 13 group の約 **34.3 倍**。`perf-release-summary.json` に全サンプルと min/max を保存した。これは単独 pass の計測であり、WASM 全体レイテンシや実ゲームの FPS ではない。全体 AST の clone/resolve を削減する、同じコストモデルの共有部分を再利用する、決定性と最終サイズ契約を維持できる pruning を検討する価値がある。単に探索候補を捨てる変更はサイズ最小化契約との比較が必要。

## 追加の検証範囲

- 描画 differential generator: **627 実行**。120 種の線形/周期/乱数/反復/平行移動パターン × record packing・screen loop・periodic loop・motif outlining・公開 compile の 600 実行と、64-bit 整数境界の 27 実行。
- 501 実行は呼び出し列一致。126 差分の内訳は、別担当で扱う Lua バイト文字列 printer 破損に起因する 120 件、F5 に起因する 6 件。多数の差分を独立したバグ数として数えていない。
- sourceMap/options 交差試験: **210 compile 実行 / 105 ON/OFF 組**。既定・fast beam 1/16・target 0/8192・newline 無効・safe/exact と、Unicode sourceName、extended/_ENV の lexical 経路を含む。成功した **98 組すべて**で map の公開 validator が成功し、ON/OFF のコード/成否が一致。7 組は F7 の入力で両方失敗。今回の交差試験から新しい map 不整合は検出しなかった。
- ソースコードの再パースおよび Source Map validator が成功しても、上記 wrong-code は検出されない。`semanticRejected:0` は実行意味論の合格を表すものではない。公開 compile は外部の semantic verifier を使わない。

## 再実行

監査対象 checkout を `--repo` に渡す。harness 自体は一時 consumer として作り、製品ファイルには書き込まない。Cargo cache は必ず既存規約どおり `/tmp/cargo-targets` 配下。

```bash
python3 docs/verification/minify-review-20261012/evidence/api/run.py \
  --repo /path/to/audited-checkout --release > api-results.jsonl
```

追加入力は `--cases FILE`。`draw-fuzz.py /path/to/audited-checkout` と `map-matrix.py` は JSON Lines を標準出力へ生成する。上の各 finding は `cases.jsonl` の同じ case 名で確認できる。証跡一覧・サイズ・SHA-256 は `manifest.json` を参照。

## 独立反証確認と実 Engine 追試

別担当 S4（immutable carrier の同名 binding 混同）と S7（escaped destructive function）を、この担当の公開 build API consumer でも 8 ケース再実行した。Default/Exact の不一致、Safe+Exact の一致、原因 pass を無効化した場合の復旧がすべて一致した。全式は有限値であり、許容された subtype 変更では説明できない。証跡は `cross-scope-cases.jsonl` / `cross-scope-results.jsonl`。新しい finding 数には加算していない。

root 担当は本ファイルの 12 再現例を実 WASM Engine Vehicle（float input、property、128×64 raster）でも SourceMap OFF/ON の **24 比較**として独立追試し、元コードのエラー 0 件、生成後の差分 24 件を確認した。詳しくは `../root/wasm-game-api.jsonl`。本担当の呼び出し列 mock と実 Engine の追試は区別して記録している。

保存した `run.py` の release 再実行も成功し、12 件すべての生成コード・元の観測値・compile 成否が保存記録と一致した。
