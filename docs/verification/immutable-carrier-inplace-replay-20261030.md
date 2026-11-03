# immutable-carrier-synthesis のリプレイのインプレース化（2026-10-30）

## 焦点
compile 追跡コストのうち、immutable-carrier-synthesis パスの
残差 2,109 回（immutable.lua、directpass 11 反復・中央値）の
うちページ COW フロア約 1,353 回の回避。第60サイクルで
確定した内訳は derive フロア 740 回＋挿入 16 回＋ページ
COW 約 1,353 回で、derive は第61サイクルで表現フロアと
確定済みのため、本サイクルでは COW 側に着手する。
追跡専用・出力不変の変更に限る。

## 機構（第60サイクルの再確認）
`NodeArena` の由来スロットは 64 スロット／ページの
`OriginSlots` で、ページ単位の `Arc` 共有＋COW である。
従来のリプレイはラウンドごとに `clone_ast` で源を複製し、
さらに候補を複製してから occurrence 書き込みを行うため、
書き込みに触れたページをラウンドごとに切り離し直す。
immutable.lua では 8 ラウンドで約 656 ページコピー
（1 コピーあたりアロケータ計数 2 回＝Arc ブロック＋
バッファ）が観測され、残差の約 1,353 回に相当する。
クローン自体は `Arc` 複写が中心でほぼ無コストであり、
コストの本体はページの再共有→再切り離しである。

## 採用した変更
- `crates/storm-lua-syntax/src/node_arena.rs` に 3 API を追加：
  - `derive_from_borrowed_with_code`：変異前に捕捉した
    contributor（`capture_origin_ref`）から derive する。
    `derive_from_with_code` の低速路と同一の
    `finish_rewrite_borrowed` を通るため記録内容は同一。
  - `relate_from_borrowed_role_code`：変異前に捕捉した
    contributor から relate する。`relate_from_role_code`
    と同一の `relate_snapshot` を通るため記録内容は同一。
    contributor が `None` の場合は何も記録しない。
  - `derive_from_clone_self`：同一アリーナ内での
    `derive_from_clone` 相当。子 ID を付け替えた複製は
    種類を保つためスロット共有路を通り、種類不一致の
    フォールバックは捕捉起点からの derive で内容同一。
  - 等価性テスト 3 件を追加（各新 API と従来 API の
    origin 一致、および `None` contributor の無作用）。
- `crates/storm-lua-minify/src/passes/immutable_synthesis.rs`：
  - `replay_group_on` を新設し、勝者候補のリプレイを
    作業中 AST へのインプレース適用に変更した。derive
    入力と定義 contributor は変異前にスナップショット
    する（値書き込みはスロットをクリアし、expression
    ノード自体が最初の occurrence であるため、変異後の
    読み直しでは pristine な由来が得られない）。
    部分木複製は `clone_subtree_in_place` で同一アリーナ
    内に行い、スロット共有は `derive_from_clone_self`
    で行う。値置換・derive・relate・synthetic 付与・
    挿入 rewrite の順序と理由コードは従来通りで、
    記録内容は同一である。試行候補は従来通り
    `candidate_for_group` を使う。
  - ラウンドループの `clone_ast` を廃し、inspect・
    resolve・試行を作業中 AST 上で直接行う。ラウンド
    開始時の AST は前ラウンドのコミット結果そのもの
    であり、従来の複製と内容は同一である。
  - 効果の所在：ページ切り離しはパスあたり最大 1 回
    （共有されたページに触れた初回のみ）になり、
    ラウンドごとの再切り離しが消える。パスが
    アリーナを一意に所有する場合（directpass）は
    切り離し自体が 0 回になる。パイプライン内で探索
    基底と共有されている場合でも、初回ラウンド以降の
    ラウンドは切り離しを払わないため、従来より常に
    少ない。

## 測定（開発ホスト1、directpass 11 反復・中央値、2 回の再測定で完全再現）
- immutable.lua：無追跡 32,228,515→32,229,247 回
  （+732 回。インプレース化に伴う試行・構築側の微増）、
  追跡 32,230,624→32,230,007 回、追跡差 2,109→760 回
  （約 -64.0%）。残差 760 回は derive フロア 740 回＋
  挿入 16 回＋残り約 4 回と一致し、ページ COW 分は
  ほぼ全額消えた。saved=1491・コード長 37040・
  ノード数 10499→10539 は変更前後で同一。
- immutable-small.lua：無追跡 970,523→970,621 回、
  追跡 970,711→970,724 回、追跡差 188→103 回
  （-85 回、約 -45.2%）。saved=1381・コード長・
  ノード数は同一。
- 時間差は主張しない（試行支配で中央値の変動が大きい）。

## 検証
- 対象 3 クレート：syntax 133（新規 3 件含む）・
  minify 765・build 127、全成功。ワークスペース全件
  50 スイート・1,279 件全成功。xtask check・fmt・
  clippy（syntax・minify lib、警告 0）通過。
- 既存の追跡付きテスト
  `tracked_trial_winner_matches_untracked_output` が
  追跡／非追跡の出力一致と由来監査（unknown 0）を
  検証しており、本変更の回帰網羅に含まれる。
- 検証区分は Tier FULL のため、分岐元との全件
  コーパス比較を実施した（下記）。インプレース化は
  構築経路の変更であり、共有下のパイプライン条件も
  含む全件比較で同一性を確認した。

## コーパス検証（Tier FULL、開発ホスト1 ローカルコーパス、内部並列 -P 4）
- manifest 照合：4,155/4,155 OK、manifest ハッシュ
  abfec87a…（VM 正本と一致）。
- base 分岐元 3354f3b00 対 head。検証バイナリ
  SHA-256 は base dea9bed6…・head 25cd019f… で
  相違を確認。4,155 ファイル×4 条件、欠落 0。
- 両 TSV はバイト同一（SHA-256 98f09177…、16,620 行
  ずつ）。全条件 identical=4,155、hard・core・expl・
  missing すべて 0。成功 4,128／失敗 27 は両側同一。
