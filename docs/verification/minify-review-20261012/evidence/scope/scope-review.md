# Scope・table・state optimization review

対象は `5a168d6`（v0.3.1）を基点とする metadata-source-map-fix 作業ツリー。minify / analysis / syntax の製品コードは変更していない。別セッションの未コミット修正を保持したまま調査した。

## 検証方法と集計

独立 Cargo consumer から `storm_lua_minify::compile_code` と個別 pass を呼び出し、原文・生成コードを別々の vendored Lua 5.3 (`mlua 0.10.5`) VM で実行した。`input.getNumber(i)=3*i`、`input.getBool(i)=i%2==0`、`property.getNumber=3` 等の明示的な入力を使用し、`output.setNumber/setBool` 呼び出しを値・型・引数個数・順序まで記録。各原文・生成物について onTick→onDraw を 2 回実行した。これは実ゲームや実 Engine I/O 境界の検証ではなく、Lua 意味論の差分検証である。

`findings-matrix.jsonl` 以降の `default` は実際の `CompileOptions::default()`（Smallest / Tolerant / Exhaustive / zero_cost_newlines=true）。`exact` は NumericMode::Exact、`safe` は Safe + Exact。`target-probes*.jsonl` は zero_cost_newlines=false でも確認した初期調査。個別 pass 名を mode にした行は full pipeline ではない。

- 24 テンプレート × 5 値設定 × 4 モード = **480 件**の構造化差分検証。原文の構文・実行エラー 0 件。結果は **395 一致、70 出力不一致、15 コンパイル拒否**。不一致と拒否は以下の既知の再現クラスへ集約できる。
- 上記のうち scope-renaming 単独 120 件はすべて一致した。
- full pipeline の 360 件は 275 一致、70 不一致、15 拒否。Safe / Exact でも複数の不具合を防げない。
- 他に個別再現、pass ON/OFF、67 pass の切り分け検査を実施。詳細は各 JSONL。エラーを一致・成功として集計していない。
- 実行時間ではなく実行回数を持つ instruction hook により、各 Lua VM は約 100 万 instruction を上限とした。原文エラーとコンパイル拒否は区別した。
- 別担当の独立 runner でも S1–S8 を 30 比較し、input.getNumber を明示的に float `i*3.0` として全モードの再現性を確認した。S4 / S7 はさらに公開 build API consumer で 8 比較し、原因 pass 無効化による復旧も確認。数値 subtype や callback phase に起因する偽陽性は見つからなかった。重複検証なので新規件数へ加算していない（`../numeric/scope-cross-summary.json`、`../api/cross-scope-results.jsonl`）。

## S1 — [P1] table parameter scalarization が closure の capture を失う

箇所: `crates/storm-lua-minify/src/passes/tables.rs:1358-1360` および `rewrite_table_parameter_body` の Function skip。

```lua
local function f(t)
  local g=function()return t.x end
  return t.x+g()
end
function onTick()
  output.setNumber(1,f({x=input.getNumber(1)}))
end
```

原文は 6。default / exact / safe のすべてで `f(t)` の table 引数が scalar に置き換わり、closure 内の `t.x` だけが残る。生成コードは global `t` を参照して `attempt to index a nil value` になる。個別 pass でも再現し、`table-parameter-scalarization=false` で復旧。

ネスト関数を「解析しない」ことと「その関数による capture が存在しない」ことを同一扱いにしている。parameter を消す前に全 descendant の BindingId capture を検査するか、capture を含めて意味を保持する変換が必要。

証跡: `findings-matrix.jsonl` の `closure-table-param`、`target-probes.jsonl` の mode=`table-param`。

## S2 — [P1] immutable-table flattening が `function table.member` を書き込みと認識しない

箇所: `crates/storm-lua-minify/src/passes/tables.rs:493-510,554-565`。

```lua
local t={}
function t.f()return 3 end
function onTick()output.setNumber(1,t.f())end
```

原文は 3。default / exact / safe のすべてが `No semantically valid optimization candidate survived verification.` で失敗する。個別 pass の出力は次の構文不正コード。

```lua
function nil()return 3 end
function onTick()output.setNumber(1,nil())end
```

`Node::Assign` の target は AssignTarget context になるが、`Node::Funcstat` の target は一般の読み込みとして走査される。空 table の未存在キーとして f を nil に畳み、table 自体も削除する。member function 定義は Lua で普通に使う構文なので影響は限定的な edge case に留まらない。immutable-table-flattening=false で復旧する。

証跡: `immutable-funcstat.jsonl`、`namespace-toggle-isolation.jsonl`。

## S3 — [P1] immutable value reuse が挿入先の同名 local を捕捉する

箇所: `crates/storm-lua-minify/src/passes/immutable_values.rs:442-447`。

```lua
local tau=math.pi*2
function onTick()
  local tau=input.getNumber(1)
  output.setNumber(1,math.pi*2)
  output.setNumber(2,math.pi*2)
  output.setNumber(3,tau)
end
```

原文の値列は `2*pi, 2*pi, 3`。default / exact / safe では `3,3,3` へ変わる。式そのものの identity は BindingId を含めているが、再利用 carrier は元の SymbolId/spelling をそのまま各参照位置へ挿入する。その場所の tau が別 BindingId であることを確認していない。

個別 `reuse_immutable_values` でも再現。`immutable-global-expression-reuse=false` で復旧。候補の全利用位置で carrier binding が可視か検証するか、競合しない新名へ carrier と全正当な参照を先に rename する必要がある。

証跡: `findings-matrix.jsonl` の `carrier-shadow`、`target-probes2.jsonl` mode=`immutable-reuse`。

## S4 — [P1] immutable carrier synthesis が同じ字面の別 binding を統合する

箇所: `crates/storm-lua-minify/src/passes/immutable_synthesis.rs:47-48,82-96`。

```lua
local a
 a=123456
function f()return a+789012 end
local a
 a=222222
function g()return a+789012 end
function onTick()
  output.setNumber(1,f())
  output.setNumber(2,g())
end
```

原文は `912468,1011234`。default / exact は両方 `912468`。Safe はこの再現では一致した。expression_key が Printer の文字列 `a+789012` だけなので、別の a の BindingId を持つ式も同一グループになる。最初の a から算出する 1 個の carrier に統合される。

個別 pass および `only=[immutable-carrier-synthesis,scope-renaming]` で再現し、synthesis を無効化すると復旧。S3 とは別のアルゴリズム・別の欠陥であり、carrier 可視性チェックだけでは直らない。BindingId を含む式 identity と、全依存値が確定した挿入位置が必要。

証跡: `immutable-synthesis-shadow.jsonl`。

## S5 — [P1] root-local globalization が元からある global と local を統合する

箇所: `crates/storm-lua-minify/src/passes/root_globals.rs:26-48,54-59`。

```lua
x=0
function onDraw()
  x=x+1
end
local x=input.getNumber(1)
function onTick()output.setNumber(1,x)end
```

原文の onTick→onDraw×2 で output 1 は `3,3`。default / exact / safe は `3,4`。output は onTick 内だけで呼び、onDraw は状態のみを更新する。root_name_counts は Local / Localfunc / plain Funcstat だけを数え、ordinary Assign と global reference を除外している。その結果、onDraw が捕捉する既存 global x と onTick が捕捉する新しい local x を同一 global x にする。

同じ pass には固定名の別の公開範囲問題もある。次の原文にはホストが呼ぶ global onTick がないが、短縮後は公開 callback になり、毎 tick output へ 5 を書き込む。

```lua
local function onTick()output.setNumber(1,5)end
```

両方 default / exact / safe と個別 pass で再現。root-local-globalization=false で復旧。宣言の字面の数ではなく lexical binding と既存 globals を基準に、衝突しない名前へ変換し、fixed callback / API root 名を新規公開しないことが必要。

証跡: `root-collision-tick-only.jsonl`（主再現）、`findings-matrix.jsonl` の `root-local-callback`。初期の `root-global-collision` は synthetic host の onDraw output を含むので Engine 検証には主再現を使用する。

## S6 — [P1] goto/label を越える dataflow の変換が値を誤る

箇所: `final_stores.rs:151-154,243-245`、`single_use_forwarding.rs:237-243`。関連する function-local slot coloring にも別の個別 pass 再現あり。

```lua
function onTick()
  local x
  x=1
  goto done
  x=2
  ::done::
  output.setNumber(1,x)
end
```

原文は 1。default / exact / safe は 2。final-dead-store-elimination 単独では x=1 が削除され、nil を出力する。goto/label を通常の空 effect として逆向き live-set 計算へ通すので、実行されない x=2 が x=1 の上書きであると誤認する。

また `local x` を取り除いた global 版では single-use-global-forwarding 単独でも x=2 を label の先へ転送し、実行されないはずの値 2 を出力する。中間 statement の call/dependency write だけを障壁にしており label/goto の到達関係を扱っていない。final-dead-store-elimination だけを OFF にしても full pipeline の誤りは解消しない。

CFG による到達・支配・liveness がない間は、goto/label を含む対象関数でこれらの変換を保守的に止める必要がある。function_globalization の直線的 lifetime coloring も backward goto で global slot を再利用してしまうため、同じ対象検査に含めるべき。

証跡: `findings-matrix.jsonl` の `goto-forward` / `goto-global-forward`、`target-probes2.jsonl` の `back-goto` mode=`function`。

## S7 — [P1] destructive result globalization が alias からの呼び出しを破壊する

箇所: `crates/storm-lua-minify/src/passes/destructive_results.rs:82-119,185-196`。

```lua
v=100
function f(x)x=x+1 return x end
local saved=f
function onTick()
  v=f(1)
  v=f(2)
  output.setNumber(1,saved(10))
end
```

原文は 11。default / exact では f が引数・戻り値なしの関数になり、saved(10) の戻り値がなくなる。Safe はこの再現では一致する。変換条件は直接の `f(...)` 呼び出ししか調べず、f の値が saved に escape したことを調べない。

個別 pass と only=destructive-result-globalization で再現し、pass OFF で復旧。すべての参照が検証済み直接 call であることを BindingId 単位で証明する必要がある。追加検査として、callee が別関数経由で destination global を読むケースでも個別 pass の意味差を確認した（full default では別の変換により当該再現の誤差は出なかったため、独立の本番再現としては数えていない）。

証跡: `findings-matrix.jsonl` の `escaped-destructive-function`、`target-probes2.jsonl` の `destructive-side`。

## S8 — [P1] table の integer64 キーを f64 に変換して別キーを同一視する

箇所: `crates/storm-lua-minify/src/passes/tables.rs:316-325,366-368`。

```lua
local t={[9007199254740992]=10,[9007199254740993]=20}
function onTick()output.setNumber(1,t[9007199254740993])end
```

Lua 5.3 の原文は 20。default / exact / safe は 10 になる。StaticTableKey::Num(f64) / num_val によって隣接する 64-bit integer がともに 9007199254740992 へ丸められ、table_get が最初のフィールドを返す。NumericMode::Exact でも止まらない。

immutable-table-flattening 単独で再現し、pass OFF で復旧した。integer と float の Lua key 正規化を保った表現にして、integer の識別へ f64 を使わないこと。通常の数値定数畳み込みとは異なる table lookup 層の問題である。

証跡: `findings-matrix.jsonl` の `int64-table-key`、`target-probes.jsonl` mode=`immutable-table`。

## S9 — [P1] local sinking 後の table を prefix expression として構文不正に出力する

箇所: `crates/storm-lua-syntax/src/print.rs:464-494`、`crates/storm-lua-minify/src/passes/locals.rs:734-767`。

```lua
local t={{}}
local a=t[1]
function onTick()output.setBool(1,a==a)end
```

原文は毎 tick true。default / exact / safe のすべてが `No semantically valid optimization candidate survived verification.` で失敗する。single-use-local-sinking 単独の生成物は次の構文不正コード。

```lua
local a={{}}[1]
function onTick()output.setBool(1,a==a)end
```

Lua の table constructor は、index / call の receiver となるには `({{}})[1]` のように括弧が必要。Printer の Index は `expr(obj,15)` を呼ぶが、Table 分岐は parent precedence を考慮せず早期 return する。Function 分岐にも同様の形があるため、prefix-expression grammar を共通処理する修正が適切。`single-use-local-sinking=false` で復旧し、only=[single-use-local-sinking,scope-renaming] で再現する。

追加の 3 原文 × Default / Safe / Exact / Safe+Exact × origin_source の有無 = 24 検査でも全て同じ compile rejection。これは内部 provenance の有無の検査であり、公開 SDK の sourceMap API 全体を検証したという意味ではない。この追加 matrix は同じチームの `numeric/harness.rs` を使用した。

証跡: `local-sink-minimal.jsonl`、`local-sink-table-index.jsonl`、`local-sink-origin-matrix.jsonl`。

## 未採用・限定した観察

- 同じ table constructor key の重複で first/last の差を確認したが、Lua 言語の constructor assignment order の扱いを精査する必要があるため、確定 finding に数えていない。
- scope_rename 単独では 120 件すべて一致し、repeated local、nested closure、loop capture、repeat-until、shadowing に新しい問題を確認できなかった。全入力の正しさを保証する意味ではない。
- 現行 NodeArena は Arc/COW なので、単なる clone 呼び出しを見ただけで全 AST の複製と断定しなかった。既存キーを読む table flattening の測定は概ね線形だった。欠損キーについては別途 COW 発生を測定した。

## PERF1 — [P2 / performance] 未存在 table key の flattening が lookup ごとに全 AST を COW 複製する

箇所: `crates/storm-lua-minify/src/passes/tables.rs:591-593,373`、`crates/storm-lua-syntax/src/node_arena.rs:149-150`。

`local t={x=123}` と N 個の `output.setBool(1,t.missing==nil)` を持つ callback を、個別 `flatten_immutable_tables` で処理した。計測対象は parse + pass + print、Lua 実行を含まない。CARGO_PROFILE_DEV_DEBUG=0、CARGO_INCREMENTAL=0 の **非最適化 dev build**、各 5 回の中央値。wall-clock 値そのものを release 性能として扱わない。

| アクセス数 | 欠損 key の flattening | parse + print だけ |
|---:|---:|---:|
| 50 | 10.4 ms | 0.44 ms |
| 100 | 37.0 ms | 0.82 ms |
| 200 | 138 ms | 1.67 ms |
| 400 | 536 ms | 3.31 ms |
| 800 | 2,117 ms | 6.55 ms |
| 1,600 | 8,413 ms | 12.9 ms |

N を倍にすると pass 全体が約 4 倍になる。既存 key の場合は 1,600 件でも 46.0 ms で概ね線形だった。`scratch=clone_table_ast(source)` 自体は Arc clone だが、`table_get` が absent を返すために scratch へ Nil を push すると `Arc::make_mut` により AST が全複製される。それを各 lookup ごとに破棄して再実行している。

読み取り専用 lookup を `Option<NodeId>` などに変更し、Nil を生成する必要がある場合も最終出力 arena にだけ作ると、この繰り返しを避けられる。性能改善後は missing nested key の runtime error 保持も合わせて検証が必要。

証跡: `table-flatten-missing-timing.jsonl` / `table-flatten-timing.jsonl`、`benchmark.py` / `benchmark_missing.py`。

最適化 release build でも同じ測定を実施した。各 5 回の中央値は以下。`CARGO_INCREMENTAL=0 cargo build --release`、他の release profile override なし。

| アクセス数 | 欠損 key の flattening (release) | parse + print だけ (release) |
|---:|---:|---:|
| 50 | 2.66 ms | 0.075 ms |
| 100 | 10.0 ms | 0.126 ms |
| 200 | 37.8 ms | 0.298 ms |
| 400 | 148 ms | 0.539 ms |
| 800 | 589 ms | 1.16 ms |
| 1,600 | 2,398 ms | 2.19 ms |

既存 key の 1,600 件は release で 8.02 ms。欠損 key では最適化ビルドでも約 300 倍遅く、二次的な増加傾向が残る。入力サイズやテーブル内容の違いもある比較なので比率そのものを製品全体の速度差とは解釈しない。証跡は `table-flatten-missing-release-timing.jsonl` / `table-flatten-release-timing.jsonl`。
