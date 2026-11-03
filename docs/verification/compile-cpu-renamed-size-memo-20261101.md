# コンパイル CPU: measure_renamed_size のアリーナ世代鍵メモ化（第93サイクル、Tier INVARIANT）

対象: `crates/storm-lua-minify/src/scope_rename.rs` の
`measure_renamed_size`（rename plan の構築＋サイズ走査）。第92サイクルの
計装では全体で 406,543 回・13.40 秒（対象ノード総数 404.7M）で、同一
アリーナ版（values Arc ポインタ・root・ノード数で識別）への再計測が
157,719 回＝全体の 38.8%（同一キーの最大繰返し 42 回）に達していた。
本サイクルはその再計測を、アリーナ世代カウンタを鍵とするメモ化で
消滅させる。

## 前提監査（チョークポイント）

世代カウンタが全ての構文変更を捉えることを、コード読み＋grep で
確定した。

NodeArena（`crates/storm-lua-syntax/src/node_arena.rs`）:

- `values` フィールドは private でモジュール外から到達不能。
  `DerefMut` は無く、`self.values =` の直接代入も無い。構造体リテラル
  構築はモジュール内の 4 箇所（手書き Deserialize・Vec 構築子・
  `empty_like`・`clone_without_origins`）のみで、フィールド追加により
  コンパイラが全箇所の記入を強制する。
- values を書く経路は全 8 箇所: `push`、`retain_block_statements_with_code`
  （ブロック書換）、`truncate`、`reset_for_reuse`（clear）、`rewrite`、
  `rewrite_with_code`、`IndexMut<usize>`、`&mut` イテレータ。
  いずれも単一チョークポイント `values_mut()` 経由に統一し、そこで
  世代を進める。等値 rewrite・全保持 retain・範囲外 truncate の
  no-op 形は書換自体が発火しないため世代も進まない（単体テストで固定）。
- それ以外のメソッド群（origin 取得系、`finish_*`、`derive_*`、
  `relate_*`、`mark_synthetic`、`explain_*`、`attach_origins_like`、
  `set_provenance`）は起源のみを変更し values を書かない。計測は
  構文と文字列のみに依存する（`scope_rename_fast_owned` の
  「rename plan depends only on syntax and strings, never on provenance」
  の既存表明、および originless クローンで同一値を返す既存テスト群が
  裏付け）ため、これらで世代を進める必要はない。
- `Ast.nodes` の丸ごと差し替え（パス群に実在）は、差し替え先アリーナ
  自身の（バッファ, 世代）対で鍵付けされる。Node に interior
  mutability は無い。

Interner（`crates/storm-lua-syntax/src/ast.rs`）:

- 変更経路は `intern`／`fresh`（fresh は intern 経由）のみで
  append-only。既存 `SymbolId` の文字列は決して変わらない。
  `symbol_count()` がそのまま世代として使える。
- 計測側（`resolve`＋名前計画、`measure_size_with_names`）は文字列を
  部分木から到達可能な id に対する `get(id)` でのみ読む。ノードは
  生成時点で intern 済みの id しか参照できないため、intern の追加は
  既存部分木の計測値を変えられない。それでも保守のため、メモの
  スタンプには symbol count を含め、不一致なら再計算する（後述の
  プローブでは不一致 0 件で、実測上の損失は無かった）。

## 変更

- `NodeArena` に `generation: u64`（`#[serde(skip)]`、wire 形式は
  不変）を追加。全 values 書換を `values_mut()` に集約し世代を進める。
  アクセサ `generation()` と、バッファを保持するための `values_arc()`、
  Interner 側に `strings_arc()` を追加。
- `measure_renamed_size` をスレッド局所メモで包む。鍵は
  （values バッファポインタ, strings バッファポインタ）、エントリは
  両バッファの `Arc` を保持（保持中はアドレスが再利用されないため
  ABA の混同が起きない）＋スタンプ（世代, symbol count）＋
  root → size の表。スタンプ不一致でそのエントリの表を破棄する。
  上限はバッファ対 64 件・総サイズ数 32,768 件で、超過時は全消去
  （1 回のコンパイルで生きているアリーナは少数）。
- 世代は構造体側にあるため clone と正しく追随する: 共有中に変更
  すれば copy-on-write で detach してバッファが変わり、未 detach
  なら内容同一のまま世代も同一。

不変性: 世代が単調に進む限り旧スタンプのエントリは到達不能であり、
計測値は（部分木構文, 参照文字列）の純関数。したがってメモの
ヒットは再計算と常に同値で、出力不変が構築により言える。

## 決定論カウント（計装プローブ、std-nomap 全コーパス）

プローブ専用ドライバ（corpuscheck の std-nomap 行を逐次単一プロセスで
複製。行は分岐元 `aa81edd` のゲート TSV の std-nomap 行と
フィールド単位でバイト一致）でメモの出入りを数えた:

- 呼出 408,514 回／ヒット 155,539 回／初回（エントリ無し）252,973 回／
  世代不一致 0 回／symbol count 不一致 0 回／root 未登録 2 回
  （合計は呼出数と一致）。
- 第92計装の重複センサス 157,719 回に対し、ヒットで 155,539 回
  （98.6%）を回収した。残差はセンサス側の近似（ポインタ再利用による
  過大評価側）と、root 未登録 2 回・初回登録の境界分。
- 世代／symbol の不一致が 0 だったことは、この構成では再計測が
  常に未変更アリーナに対して起きることの実測であり、保守的な
  スタンプ（symbol count を含む）が実測上の損失を生んでいない。

## 検証

- Tier FULL 相当ゲート（分岐元 `aa81edd` との全コーパス比較）:
  16,620 行が両側とも SHA-256
  `4a98d0939f13cb4f3f888393087bbb64f8945cf3e5a9154e9bb7680ade733028`
  でバイト一致、fallback 0。
- ワークスペーステスト 1,668 件全通過（新規 3 件: 世代の全変更経路
  テスト、clone／detach 追随テスト、メモの変更・intern 後一致テスト）。
  clippy の警告集合は base／head で同一。変更 3 ファイル単体の
  `rustfmt --check` 無指摘。
- .us2（submit 1 回・全入力同梱、固定 208 ファイル std-nomap、
  交互 3 回、バイナリは .wsl で release ビルドして同梱）: base 中央値
  13,569,074µs（13,592,568／13,569,074／13,475,663）→ head 中央値
  13,115,108µs（13,115,108／13,225,697／13,099,470）で −3.35%。
  全 rep で head が base を下回り範囲は重ならない。status と
  コードハッシュのフィンガープリントは全 rep・両側一致
  （ok 206／err 2）。

## 残候補（未実装）

- api-alias 探索の試行構築＋計測の一般形（第92記録の順位 1、
  残り約 168,707 回の構築）。構築なし計測の一般化には rename plan の
  正確な増分計算と選択一致の証明が前提。
- split-sign の残部（解析 2.60s＋resolve 0.73s、段構成の再設計領域）。
- ae-reuse の残部: effects_for_statement 0.72s（2,404,818 回）と
  合成側キー構築 255,180 回／15.6MB。
- region 0 側の values 深コピー総量（219,789 回・267,690,260 要素、
  finalize／phase 側の clone 量）。
