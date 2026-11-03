# literal-call-folding の tiny fold における被演算子由来の合成共有化による追跡コスト削減（2026-10-29、未公開）

compile の追跡コストのうち、literal-call-folding パスに残っていた
tiny fold の被演算子由来付けコストを除去した。同パスは Bin／Un ルートの
tiny fold に先立ち、折り畳み済み被演算子アトムへ本体ルート由来を
derive_from で付けていたが、被演算子アトムの値は本体ルート（Bin／Un）と
種類が異なるため、derive は毎回フル rewrite 経路（新規由来 Arc・理由・
dispositions 構築）を通っていた。被演算子の由来は折り畳み結果アトムから
参照されず tiny arena とともに破棄され、tiny fold 内で使われるのは
known 判定の真偽だけであるため、共有合成由来（mark_synthetic）へ置換した。
共有合成由来は has_origin を真にし、known 判定の結果を変えない。

等価性の根拠：折り畳み結果アトムの由来は本体ルート由来からの derive と
constant-folding の rewrite 理由・評価 evidence で決まり、被演算子由来は
その内容に寄与しない。被演算子由来が担うのは、fold_node の known 判定
（変更ノードの部分木が全て由来付きか）における真偽のみである。本体ルート
由来が存在する場合は旧 derive も新合成由来も has_origin を真にし、判定は
真のままである。本体ルート由来が存在しない場合は、旧経路では known が偽に
なって折り畳み結果の由来が消去され、新経路では known は真になるが
derive 元の本体ルート由来自体が無いため結果由来はやはり無しとなり、
最終的な由来は両経路で同一である。無追跡時は mark_synthetic も旧 derive
も何もしない。回帰テストは、Bin 形状の既存 provenance テストに加え、
Un 形状の新規テスト（scratch_folded_unary_literal_call_keeps_body_origin）
で、生成コード・unknown 0・本体由来の保持を検証する。

測定（開発ホスト1、directpass 11 反復・中央値、inline.lua、folded=500）：
無追跡 181,676 回で不変、追跡 196,318 回から 192,318 回へで、
追跡差は 14,642 回から 10,642 回へ（約 -27.3%、29.3 回／件から 21.3 回／件）。
saved=3001・コード長 4443・ノード数は変更前後で同一。
時間改善は主張しない（決定的計数のみで判定）。

検証区分は Tier FULL。実コーパス全件比較（4,155 ファイル×4 条件、
開発ホスト1 のローカルコーパス、実行前に SHA-256 manifest 照合
4,155/4,155 OK、manifest ハッシュ
abfec87ab0c29dc088d4e93f25cf563849b062d3eb15eeaa5d54f8d08b8341c3）で、
base（分岐元 a2d9112）と head の比較結果は下記のとおり。
base（分岐元 a2d9112）と head の TSV はバイト同一（SHA-256 5210f6e1982483923446d683915522db9da582086e6d44d1eff6524ae71ecc84、16,620 行ずつ）。全条件で identical=4,155、hard・core・expl・missing すべて 0 件。成功 4,127／失敗 28 は両側同一。
