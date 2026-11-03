# immutable-carrier-synthesis の occurrence rewrite 由来の除去（2026-10-29）

## 焦点
compile 追跡コストのうち、immutable-carrier-synthesis パスの
残差 2,849 回（immutable.lua、directpass 11 反復・中央値）の
うち occurrence rewrite 側の再確認。追跡専用・出力不変の
変更に限る。

## 切り分け（分岐元 4e0f40c、開発ホスト1）
一時診断変種（すべて cp 復元で除去）による内訳。baseline の
追跡差 2,849 回は次で完全に閉じる：
- occurrence の rewrite 由来除去で差 2,849→2,109 回
  （rewrite 分 740 回）。
- occurrence の derive 除去で差 2,849→2,109 回
  （derive 分 740 回）。
- rewrite・derive 両除去で差 1,369 回。
  挿入 rewrite 除去でさらに 16 回減り 1,353 回。
  relate 除去では差は不変（relate 分 0 回）。
- すなわち残差 2,849 回＝rewrite 740 回＋derive 740 回＋
  挿入 16 回＋その他 1,353 回である。
- その他 1,353 回の内訳：コミットなし変種では追跡差が
  8 回まで落ちるため、ラウンド構造（resolve・inspect・
  試行・計測）ではなくリプレイ候補側にある。ページ
  コピー計数では、1 反復あたり 656 回のページコピー
  （64 スロット／ページの COW）が観測され、1 ページ
  コピーあたりアロケータ計数 2 回（Arc ブロック＋
  バッファ）として約 1,312 回に相当し、残差の大半を
  占める。すなわちその他は、リプレイ候補が源と共有する
  スロットページを occurrence 書き込みで切り離す際の
  ページ COW フロアであり、occurrence 単位の操作除去
  では削減できない。値だけ置換しスロットに触れない
  変種では追跡差が 57 回まで落ちることも、この解釈と
  一致する。
- なお、スロットを共有したまま relate だけを行う変種では
  追跡差が増える（共有スロットの NodeOrigin 複写）が、
  これは診断変種の人工物である。実装経路では derive が
  fresh なスロットを作るため relate は無コストであり、
  relate 除去の測定（差不変）と一致する。

## 採用した変更
`crates/storm-lua-minify/src/passes/immutable_synthesis.rs` のみ：
- リプレイの occurrence ループで、従来の
  `rewrite_with_code` による値置換＋rewrite 由来記録を、
  `IndexMut` 代入による値置換のみに変更した。
- 直後の `derive_with_code` が当該スロットを源 occurrence
  から作り直す（未知入力時はフォールバックでクリアする）
  ため、rewrite 由来は読み取られることなく上書きされる。
  値が等しい場合の早期復帰も該当しない（occurrence は
  Bin／Un、置換後は Name）。試行候補は由来なしのため
  値置換の結果は従来と同一で、勝者選択は変わらない。
  derive・relate・挿入 rewrite は従来通りである。
- したがって最終 AST・由来・生成コード・Map は同一で、
  変わるのは確保回数だけである。既存の追跡付きテスト
  `tracked_trial_winner_matches_untracked_output` が
  追跡／非追跡の出力一致と由来監査（unknown 0）を
  検証しており、本変更の回帰網羅に含まれる。

## 測定（開発ホスト1、directpass 11 反復・中央値）
- immutable.lua：無追跡 32,228,515 回で不変、追跡
  32,231,364→32,230,624 回、追跡差 2,849→2,109 回
  （-740 回、約 -26.0%）。saved=1491・コード長 37040・
  ノード数 10499→10539 は同一。
- immutable-small.lua：無追跡 970,523 回で不変、追跡
  970,811→970,711 回、追跡差 288→188 回（-100 回）。
  saved・コード長・ノード数は同一。
時間差は主張しない。

## 検証
- 対象 3 クレート（syntax 130・minify 759・build 127）
  全成功、ワークスペース全件 50 スイート・1,270 件成功、
  xtask check・fmt・clippy（syntax・minify lib、警告 0）通過。
- Tier FULL コーパス検証（開発ホスト1 ローカルコーパス、
  manifest 照合 4,155/4,155 OK、manifest ハッシュ abfec87a…、
  内部並列 -P 4）：base 分岐元 4e0f40c 対 head。
  検証バイナリ SHA-256 は base 28e99e21…・head a8f5f853… で
  相違を確認。4,155 ファイル×4 条件、欠落 0。両 TSV はバイト同一
  （SHA-256 40d852a9…、16,620 行ずつ）。全条件
  identical=4,155、hard・core・expl・missing すべて 0。
  成功 4,127／失敗 28 は両側同一。TSV・比較ログは
  検証用保存先/corpus-{base,head}-c60.tsv・
  corpus-compare-c60.log に保存。コーパス用ビルド・
  出力は一時領域に置き用後削除済み。
