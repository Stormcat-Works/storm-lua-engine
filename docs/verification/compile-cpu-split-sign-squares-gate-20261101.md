# コンパイル CPU: split-sign 二乗段の必要条件ゲート（2026-11-01、w7 第89サイクル）

## 変更

- 実装: `859d640`（`crates/storm-lua-minify/src/passes/split_sign.rs` のみ。分岐元 `82cbe57` 時点の実装 `95339e3` を、統合先端の前進に合わせ `8a19ffe`（第244ラウンド）→`6671a01`（第245ラウンド）へ2回リベースしたもので、ファイル blob は同一 `fe666f8b…`）
- 内容: split-sign パスの二乗段（`math.max(x,0)^2*k - math.min(x,0)^2*k` の再合成）について、段入力（blocks 段の出力）に「指数が数値 2 で基底が `math.max` 呼び出しである `^` ノード」と「同じく `math.min` 版」の両方が存在することを必要条件とするゲート `squared_rewrite_possible` を追加した。条件が満たされない場合（および `aggressive` でない場合）は、二乗段・`math.abs` 別名走査・NumericProof 構築・候補 clone を丸ごと省略し、結果は blocks 段の出力そのものになる。blocks 段でも何も除去されなかった場合は入力 `Ast` に一切触れない（候補 clone の代入自体も省略）。回帰テスト1件追加（min 側のない単独の二乗 max 部品が書き換わらないこと）。

## レバー選定の根拠（第89サイクル計装）

第88サイクルで single-use-forwarding に行ったのと同じ内部計装を、finalize 上位の split-sign／conditionals／final-stores と ae-reuse に適用した（std-nomap 全 corpus、原子スロット式の低擾乱プローブ: 無計装 184.37s に対し計装 189.27s＝+2.7%。プローブ出力行は corpuscheck の std-nomap 行とソート後バイト一致を2回確認）。

- **split-sign**（50,129 回呼出、合計 約10.6s）: EffectAnalyzer 構築 2.60s、二乗段の変換走査 2.67s、blocks 段 1.80s、サイズ計測2回 1.66s、別名走査 0.75s、resolve 0.73s、NumericProof 0.39s。効果は corpus 全体で eliminated=0／squared=0（signed_part のペア通過は34件のみで全て後続条件で不成立）。
- **conditionals**: 割当形 transform 4.33s のうち refresh が 24,801 回（ref_analyzer 2.37s＋ref_resolve 0.75s）。ただし refresh を含むブロックは 23,267 件で 1ブロックあたり 1.07 回——ブロック単位の共有化では最大でも 1,534 回分（約0.19s）しか消えない。呼出形は refresh 1,348 回と小さい。
- **final-stores**（34,844 回）: 本体走査 2.33s、解析 1.76s、計測 1.10s、resolve 0.51s。走査内部では live 集合の clone が 646,696 回・総要素数 40,939,083 に達する（除去は 3,459 件）。
- **ae-reuse**: 本体走査 5.87s のうち式キー文字列の構築 `expression_key` が 2.59s（5,310,822 回、総 601,297,056 バイト）。走査した式ノード 16,075,876、ヒット 1,526、再利用 1,270。設定側は resolve 0.94s＋解析 1.34s＋組込み変異集合 0.34s。合成側（ae-carriers）走査 3.49s。

採用しなかったレバー:

- **conditionals の refresh 限定**: 鮮度証明が立たない。候補判定（`assignment_candidate`／`call_candidate`）は、子孫の低下で生成された式を含む文について effects を照会する（低下後の文の if 分岐本体がまさにそれである）。元の解析器は低下前の arena を所有し新規ノードを検査できない（コード内コメントおよび回帰テスト `nested_calls_refresh_analysis_after_child_lowering` がこの依存を固定している）。「候補判定が新規ノードの解析結果に依存しない」ことは一般には成り立たず、証明の不足箇所はここである。ブロック共有化も上記のとおり効果が小さいため、計装のみで止めた。
- **ae-reuse の式キー**: 構成的ハッシュ化（bottom-up のハッシュ＋ヒット時検証）で 2.59s の大半は消せる見込みだが、available 式マップのキー構造の作り替えを伴い、本ゲートより侵襲が大きい。残候補として記録する。
- **final-stores の live 集合表現**: clone 総量は大きいが、表現置換には反復順序の監査が要り、効果見積もりも走査残部 約1.8s が上限。残候補。
- **split-sign blocks 段のゲート化**: ペア走査自体が signed_part 呼び出しを変換と同数必要とし、変換本体（1.80s）と大差ないため見送り。

## 出力不変性（Tier INVARIANT）

- 二乗段が書き換えるのは、`squared_part` が両辺で成功した `Bin("-")` ノードに限る。成功した `squared_part` はすべて、段入力に存在する元の `^` ノード（指数=数値2、基底=`math.max`／`math.min` 呼び出し）へ遡れる: 変換の置換結果には `^` ノードが含まれないため、早期の置換で新たに露出した部品も、元の `^` ノードそのもの（内側パターンから抽出された value／factor）である。基底呼び出しの判定（`call_operation`）が読むのは変換が書き換えないノードのみである。したがって片方の演算でもそのような `^` ノードが段入力に無ければ、`squared_part` は決して成功せず、二乗段は恒等変換である。`aggressive` でない場合は段自体が無条件に恒等である。
- blocks 段で除去 0 の場合、段出力は入力の値・origin とも同一の複製である（等しい値での `rewrite` は origin を保持する no-op であり、これは node_arena の明示仕様）。この場合に入力 `Ast` を触らないことは観測可能な差を生まない。除去があった場合の結果は blocks 段出力そのもので、現行コードの候補（同一内容の clone）と一致する。`PassResult` の `saved`／`details` も現行と同一の値を返す（squared=0、saved は実測サイズ差）。

## 検証

- 単体テスト: split-sign 8 件全通過（新規1件＋provenance 系3件を含む）。ワークスペーステストは最終 head（`6671a01`＋本変更）で 1,631 件全通過（第245ラウンド時点の 1,630＋新規テスト1）。
- clippy: `storm-lua-minify` で base（`8a19ffe`）／head の警告集合が同一（既存分のみ、本変更ファイルに無し）。対象ファイル単体の `rustfmt --check` 無指摘。
- 合格 gate として Tier FULL 相当の base／head 全 corpus 比較を、統合先端が進むたびに計3回実施: (1) base=`82cbe57`、(2) base=`8a19ffe`（第244ラウンド先端）、(3) base=`6671a01`（第245ラウンド先端＝最終統合先端）。いずれも 16,620 行（4,155 ファイル×4条件）が両側とも SHA-256 `743431aa4bb3142c3661c78dfdbf63219052704c8d763fbd35cda85d73279be7` でバイト一致、fallback 0。
- .us2 測定（submit 1 回・全入力同梱、208 ファイル std-nomap、交互 3 rep、per-rep 合計 µs）: base 中央値 13,715,990（13,714,693／13,715,990／13,795,756）→ head 中央値 13,609,459（13,558,044／13,623,987／13,609,459）で **−0.78%**。3 rep 全てで head が base を下回り、rep 範囲も重ならない。status は全 rep・両側一致（ok 206／err 2）。.us2 の負荷は 0.35／0.16／0.11 と低かった。計装からの期待（消滅する二乗段走査＋証明＋別名走査＋最終計測 ≈ 4.6s gross、ゲート走査 ≈0.75s を差し引いた正味 約3.9s、std-nomap 全体の約2%）とは桁が合う（208 ファイル部分集合での実測は −0.78%）。

## FULL-VERIFY-TIP について

本ラウンドは Tier INVARIANT のため、FULL-VERIFY-TIP（`6671a01`、第245ラウンドの Tier FULL）は前進させない。第245の後の INVARIANT は本ラウンドが1件目（上限3以内）。ただし上記のとおり最終統合先端を base とする FULL 比較自体を実施し、累積ハッシュ `743431aa…` のバイト一致を確認済みである。
