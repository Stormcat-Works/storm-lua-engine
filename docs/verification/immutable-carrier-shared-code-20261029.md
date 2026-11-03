# immutable-carrier-synthesis の理由コード共有化（2026-10-29）

## 焦点
compile 追跡コストのうち、immutable-carrier-synthesis パスの
追跡差 7,273 回（immutable.lua、directpass 11 反復・中央値）の
切り分けと削減。追跡専用・出力不変の変更に限る。

## 切り分け（分岐元 247036c、開発ホスト1）
一時診断変種（すべて cp 復元で除去）による内訳：
- relate_from_role 除去で差 7,273→3,589 回（relate 分 3,684 回）
- origins::derive 除去で差 7,273→5,793 回（derive 分 1,480 回）
- mark_synthetic 除去で差不変（0 回）、clone_subtree の
  derive_from_clone 除去で差不変（0 回）
- derive＋relate 除去で差 2,109 回（occurrence rewrite＋
  挿入 rewrite＋ラウンド側残差）。第1ラウンドのみの残差は
  約 1 回のため、ラウンド側（clone_ast／resolve／tracked 計測）
  の寄与は無視できる規模で、残差はほぼ occurrence rewrite 側。

## 採用した変更
`candidate_for_group` で、候補ごとに 1 個ずつの共有コード文字列
（`immutable-carrier-use`／`immutable-carrier-definition`／
`immutable-carrier-insertion`）を作り、以下へ渡す：
- occurrence の書き換えを `rewrite` から `rewrite_with_code` へ
- 由来付与を新設の `origins::derive_with_code` へ
  （`derive_from_with_code`＋`relate_many_from`。単一入力の
  本呼出では後者は空列で無作用。未知入力時の IndexMut
  フォールバックは従来の `derive` と同一）
- 定義関連付けを `relate_from_role_code` へ
- 挿入 rewrite を `rewrite_with_code` へ
記録される文字列内容は変わらないため生成コード・由来・Map は
同一で、変わるのは確保回数だけである。`rewrite` は由来なしの
試行アリーナでもコード文字列を fresh 確保するため、試行側の
絶対確保回数も減る。

## 測定（開発ホスト1、directpass 11 反復・中央値、2 回再現）
- immutable.lua：無追跡 32,513,605→32,228,515 回（-285,090 回）、
  追跡 32,520,878→32,234,308 回（-286,570 回）、
  追跡差 7,273→5,793 回（-1,480 回、約 -20.3%）。
  saved=1491・コード長 37040・ノード数 10499→10539 は同一。
- immutable-small.lua：無追跡 1,004,578→970,523 回、
  追跡 1,005,464→971,209 回、追跡差 886→686 回（-200 回、
  約 -22.6%）。saved・コード長・ノード数は同一。
時間差は主張しない。

## 検証
- 対象 3 クレート（syntax 129・minify 750・build 127）全成功、
  ワークスペース全件 1,260 件全成功、xtask check・fmt・clippy
  （syntax・minify lib、警告 0）通過。
- Tier FULL コーパス検証（開発ホスト1 ローカルコーパス、
  manifest 照合 4,155/4,155 OK、manifest ハッシュ abfec87a…、
  内部並列 -P 4）：base 分岐元 247036c 対 head。
  検証バイナリ SHA-256 は base b3ff4db2…・head ba6ce482… で
  相違を確認。4,155 ファイル×4 条件、欠落 0。
  両 TSV はバイト同一（SHA-256 80ef8b65…、16,620 行ずつ）。
  全条件 identical=4,155、hard・core・expl・missing すべて 0。
  成功 4,127／失敗 28 は両側同一。
