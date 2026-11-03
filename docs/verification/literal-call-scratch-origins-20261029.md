# literal-call-folding の呼出単位スクラッチ化による追跡コスト削減（2026-10-29、未公開）

compile の追跡コストのうち、literal-call-folding パスに残っていた、
発火した呼出のコミット計算そのものの由来確保コストを除去した。
同パスはユーザ定義関数のリテラル呼出ごとに本体を複製して定数畳み込みし、
小さくなれば呼出点へ install するが、複製＋畳み込みを tracked な対象
アリーナ上で行っていたため、中間ノードの capture／finish／理由記録が
呼出ごとに積み上がっていた（inline.lua で単独追跡差 51,289 回）。
最終的に残るのは畳み込み結果のアトムだけで、中間ノードの由来は結果から
到達不能であることに着目し、複製＋畳み込みを由来なしスクラッチ上で行い、
結果がアトムの場合のみ tiny tracked arena でルート fold の由来
（本体複製由来＋constant-folding rewrite＋評価 evidence＋inline-site
relation／context）を再現して呼出点へ derive 転送する形に変更した。
実装は minify クレートの inline_functions.rs のみ＋回帰テスト 4 件。

等価性の根拠：スクラッチ fold は tracked fold と同一の値（本体の値と、
子再帰後の呼出引数の値）を見るため、値と採否判定（呼出点との measure
比較）は同一。最終アトムの由来は、到達可能なルート fold の由来だけから
決まる。tiny arena は本体複製由来を持つルートとスクラッチで畳み済みの
被演算子アトムからルート fold を再現するため、演算子・被演算子値・
サイズ・許容誤差が同一で、rewrite 理由と評価 evidence も同一になる。
転送は共有スロットの derive で行い、inline-site relation／context は
install と同一の手順で付与する。対象外は従来の tracked 経路へ fallback
する：非アトム結果、Paren／Call 等の本体ルート、非アトム被演算子、
tiny fold の値不一致、アトムのテキスト正規化。builtin 分岐は変更なし。

測定（開発ホスト1、directpass 11 反復・中央値、inline.lua、folded=500）：
無追跡 950,922 回→184,171 回、追跡 1,002,211 回→204,801 回で、
追跡差は 51,289 回→20,630 回（約 −59.8%、102.6 回／件→41.3 回／件）。
saved=3001・コード長 4443・ノード数 4022 は変更前後で同一。
時間改善は主張しない（決定的計数のみで判定）。

検証区分は Tier FULL。実コーパス全件比較（4,155 ファイル×4 条件、
開発ホスト1 のローカルコーパス、実行前に SHA-256 manifest 照合
4,155/4,155 OK、manifest ハッシュ
`abfec87ab0c29dc088d4e93f25cf563849b062d3eb15eeaa5d54f8d08b8341c3`）で、
base（分岐元）と head の TSV はバイト同一（SHA-256
`5210f6e1982483923446d683915522db9da582086e6d44d1eff6524ae71ecc84`、
16,620 行ずつ）。全条件で identical=4,155、hard・core・expl・missing
すべて 0 件。成功 4,127／失敗 28 は両側同一。
対象 3 クレート（syntax 124・minify 665＝新規 4 件含む・build 126）
全成功、ワークスペース全件 50 スイート・1,169 件全成功・失敗 0。
`cargo xtask check`・`cargo fmt --check`・clippy（minify lib、警告 0）通過。

なお、実装途中の測定で 2 つの罠を確認し、いずれも本変更で回避済み：
(1) scratch を inherit_ast で作ると origins=Some（スロット空）の
「追跡あり」arena になり、fold が追跡モードでのみ評価 evidence の
事実構築を行うため追跡差が悪化する。(2) scratch を
clone_without_origins で作ると全ノードを持つため、fold の resolve が
アリーナ全体に触れて絶対量が膨張する。採用した形は両者を合成した
empty かつ無追跡の小アリーナである。
