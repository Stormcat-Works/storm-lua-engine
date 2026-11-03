# 理由テールの一般共有による追跡コスト削減（2026-10-28、未公開）

## 焦点

compile の追跡コストのうち、api-alias パス内の後段改名 finish に
残っていた、置換ノード 640 件分の fresh 理由テール確保。

## 切り分け

第22サイクルの棚卸し（分岐元 960a4940＝第96ラウンド、合成入力
dense-normal、VM、trackcost single 5 反復・directpass 11 反復の中央値）では
全コンパイル追跡差 66,303 回、api-alias 単独オン差 13,072 回だった。
api-alias 直接実行 3,405 回に対する一時変種（すべて退避からの cp 復元で
除去し、git の破壊的操作は使っていない）で次の分解を得た。

- 後段 scope_rename_fast を外すと 970 回：転送 970 回＋後段改名 2,435 回。
- 転送のみで置換導出を外すと 330 回：置換導出 640 回（置換 640 ノード×
  スロット Arc 1 回）と固定費 330 回（定義マーキング 106 回＋歩行約 224 回）。
- 改名で finish_rename_changed を外すと転送込みで 1,160 回：
  改名 2,435 回＝finish 2,245 回＋finish 以外 190 回。
- finish 2,245 回の内訳は、スロット Arc 1,605 回（改名ノード数と一致）
  に加え、置換 640 ノード分の fresh テール 640 回。置換ノードは転送で
  api-alias 理由の共有テールを既に持つため、改名 finish で同じ改名理由を
  push する際に毎回 fresh テールを作っていた。

## 採用した変更

`crates/storm-lua-syntax/src/deferred_origin.rs` のみ（＋回帰テスト 1 件）。

- push_reason の深いテール生成を、（previous ポインタ, reason）をキーと
  する共有テーブルによる共有に変更した（上限 4096 件、超過時は従来の
  fresh 生成へ fallback）。初回理由の共有テーブル（shared_first_tail）は
  従来のまま。
- 等価性の根拠：テールノードの記録する理由列は previous の列＋reason で
  決まるため、同一 previous・同一 reason の共有は記録内容が同一である。
  共有ノードはテーブルが参照を保持するため決して in-place 変更されず、
  共有元への後続 push は copy-on-write で複写される（初回共有と同一の
  安全性）。理由の同一性は OptimizationReason の Eq・Hash（code・
  operation・before・after・basis・facts を含む）で判定する。
- 回帰テスト `identical_second_reasons_share_one_tail_node` を追加した
  （2 由来が同じ第1・第2理由で同一テールノードを共有し、一方への第3理由が
  他方の記録を変えないことを検証）。

## 測定

合成入力 dense-normal、VM、中央値。時間差は主張しない。

- directpass（11 反復）：api-alias の追跡差 3,405 回→2,765 回（−640 回、
  置換 640 ノード分と一致）、constant-folding 2,738 回→2,258 回（−480 回、
  変更ノード 480 件分と一致）、scope-renaming 1,181 回・root-local 652 回は
  不変。無追跡側・saved・コード長・ノード数はすべて同一。
- trackcost single（5 反復）：全コンパイル追跡差 66,303 回→64,120 回
  （−2,183 回、約 −3.3%）。単独オン差は api-alias 13,072 回→12,432 回
  （−640 回）、constant-folding 6,345 回→5,865 回（−480 回）、
  scope-renaming 8,861 回・root-local 3,458 回・all-off 差 15,869 回は不変。
  単独オン削減の合計（1,120 回）を上回る全コンパイル削減の残りは、
  候補評価など全コンパイル内の他の経路での同型の共有によるもので、
  厳密な内訳までは主張しない。追跡バイト数も減少した。
  無追跡側・コード長・候補数はすべて同一。

## 検証

- 対象 3 クレート：syntax 122 件（新規 1 件含む）・minify 619 件・
  build 126 件、全成功。ワークスペース全件：50 スイート・1,121 件全成功
  （前回比＋3 件は w3 の新規 2 件と本サイクルの新規 1 件で一致）。
  `cargo xtask check`・`cargo fmt --check`・clippy（対象 3 クレート、警告 0）
  通過。--all-targets の clippy は本サイクルでは未実行（既存警告は
  本変更のファイル外のため対応しない）。
- 検証区分は Tier INVARIANT（構成上出力不変の共有化。対象テスト＋
  ワークスペーステスト＋代表入力の決定的計数で判定）。全件コーパス
  （4,155 ファイル×4条件）は、GOAL.md の効率化プロトコルに従い、次回
  Tier FULL の累積比較（起点 882169d0）へ繰り延べる。INVARIANT 連続は
  本サイクルで 1 件目。次回 FULL 時は起点からの累積比較を行うこと。
