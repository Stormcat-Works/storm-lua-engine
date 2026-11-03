# Block rewrite の分離走査の小規模無確保化（2026-10-30）

## 焦点
compile 追跡コストのうち、immutable-carrier-synthesis パス
の残差 760 回（immutable.lua、directpass 11 反復・中央値）の
うち挿入 rewrite 16 回の内訳確認と削減。第62サイクルで
残差は derive フロア 740 回＋挿入 16 回＋残り約 4 回に
集約されており、derive は第61サイクルで表現フロアと
確定済みのため、本サイクルでは挿入 16 回に着手する。
追跡専用・出力不変の変更に限る。

## 挿入 rewrite の内訳と除去の可否
診断変種（すべて復元済み）により、挿入 rewrite 1 件
あたりの追跡差は 2 回で、8 ラウンド分で 16 回と確定した。
内訳は、由来 Arc 1 回と、分離走査（detachment 計算）の
ための retained HashSet 1 回である。追跡なしモードでは
`finish_rewrite_with_code` が早期復帰するため、この
2 回は追跡モードにのみ発生する。
挿入 rewrite 自体の除去は不採用とした。値書込みのみ
（IndexMut 代入）に置き換える診断変種では追跡差は
760 回→744 回に減り、生成コード・saved・ノード数は
同一だったが、対象テスト 3 件が失敗した。ルート Block
の由来は、監査プリンタにおける文間セパレータの
mapping の被覆に読まれており、由来を消すと
unknown bytes が発生する。したがって由来 Arc 側は
記録そのもので削れず、削減対象は分離走査の
HashSet のみに絞られる。

## 採用した変更
`crates/storm-lua-syntax/src/node_arena.rs` の
`finish_rewrite_borrowed` における Block 分離走査を、
小さい Block では線形走査に変更した。
旧ソース順を保ちながら分離集合を求める計算で、
両経路とも同一の集合を同一の順序で返すため、
記録内容は変わらず、変わるのは計算方法だけである。
しきい値は旧・新いずれも 512 文以下で線形走査、
それを超える広い Block では従来の HashSet 経路を
維持する。広い Block での二次走査化を避けるための
従来の配慮はそのまま残る。
回帰テストとして、600 文の Block を 300 文へ書き換える
広い Block 経路のテストを追加した（文数・印字コード・
クローン間のシリアライズ同一性を確認）。

## 測定
開発ホスト1、directpass 11 反復・中央値、2 回の
再測定で完全再現。
- immutable.lua：追跡差 760 回→752 回（-8 回、挿入
  rewrite 8 件分）。無追跡側 32,229,247 回・saved=1491・
  コード長 37040・ノード数 10499→10539 は同一。
- immutable-small.lua：追跡差 103 回→102 回（-1 回）。
  saved・コード長・ノード数は同一。
- 時間差は主張しない。
残差 752 回は derive フロア 740 回＋挿入由来 Arc 8 回
＋残り約 4 回に集約され、immutable-carrier-synthesis
パスの追跡コストは表現フロアに到達した。

## 検証
検証区分は Tier FULL（共有経路の変更のため）。
- 対象 3 クレート（syntax 134＝新規 1 件含む・
  minify 765・build 127）全成功、ワークスペース全件
  50 スイート・1,280 件全成功、xtask check・fmt・
  clippy（syntax・minify lib、警告 0）通過。
- 全件コーパス比較（開発ホスト1 ローカルコーパス、
  manifest 照合 4,155/4,155 OK、manifest ハッシュ
  abfec87a…、内部並列 -P 4）：分岐元 e441027 対 head。
  検証バイナリ SHA-256 は base dcd1c1c4…・head
  489058ed… で相違を確認。4,155 ファイル×4 条件、
  欠落 0。両 TSV はバイト同一
  （SHA-256 98f09177…、16,620 行ずつ）。全条件
  identical=4,155、hard・core・expl・missing すべて 0。
  成功 4,128／失敗 27 は両側同一。
