# root-local-globalization の capture＋finish から rewrite() への置換による追跡コスト削減（2026-10-27、未公開）

ルート local のグローバル化パス（`root-local-globalization`）で、変換文ごとに
`capture_origin`＋新規ノード作成＋`finish_rewrite` を行っていた箇所を、
文スロットに対する `NodeArena::rewrite()` に置き換えた。公開 API のシグネチャは
変えておらず、生成 Lua・候補選択は維持する。Map 比 1.2 の目標は未達のままであり、
この変更はその達成を主張しない。

## 原因

追跡差の内訳切り分け（合成入力 dense-normal、計数アロケータ付きハーネス、
11 反復中央値）では、root-local-globalization 単独の追跡差は無追跡 2,002 回→
追跡 6,849 回（＋4,847 回）だった。一時パッチによる変種測定で次の内訳を得た
（各変種は測定後に revert した）。

- 文ごとの capture＋finish：3,840 回（320 文で約 12 回／文）。
  capture 時の Local 値 clone（名前・式 Vec のバッファ）と、finish 時の
  由来構造体 clone、理由構築（code・before・after・空 facts の各 Arc）が主因。
- 名前由来の `copy_name_within`：640 回（320 名で 2 回／名。
  新規ノードの由来スロット確保と名前列バッファで、 fresh ノードには不可避）。
- ルート Block の rewrite：345 回。変換文の旧 Local ID が新 Block から外れるため、
  320 件の除去 dispositions が理由付きで記録されていた。
- 残差は約 22 回で、resolve の追跡上乗せ・新規ノードのスロット確保・
  synthetic 継承はいずれも無視できる規模だった。

## 変更

- `crates/storm-lua-minify/src/passes/root_globals.rs` のみ（パス側）。
  Local→Assign、Localfunc→Funcstat の変換を、新規ノードの追加＋finish から、
  文スロットへの `rewrite(statement, Node::Assign/Funcstat, "root-local-globalization")`
  に変えた。名前ターゲットの作成・synthetic 継承・名前由来コピーの順序は従来通り
  で、rewrite の前に行う。`rewrite()` は旧値を `mem::replace` で取り出すため値
  clone がなく、スロットの由来を take するため由来構造体の clone もなくなる。
  変更時は従来と同じ finish 経路（同一理由・同一記録）を通る。文 ID が再利用
  されるため最終 Block の rewrite では変換文が除去扱いにならず、ルート由来の
  除去 dispositions は付かなくなる（後述の説明系差分）。
- `crates/storm-lua-syntax/src/node_arena.rs` のみ（共有部）。
  Block rewrite 時の除去 dispositions で、記録済み Remove 理由がない場合の
  フォールバック理由をループ全体で1個共有するようにした（従来は除去文ごとに
  code 文字列を新規確保していた）。記録される理由の内容は同一で、
  記録済み理由がある文は従来通り自分の理由を clone する。
- `crates/storm-lua-syntax/src/explanation.rs` のみ（共有部）。
  `OptimizationReason::rewrite` の空 facts を、既存の共有 static（`action_shared`
  と同じもの）から取るようにした（従来は理由ごとに空ボックスを新規確保していた）。
  空リスト同士で内容は同一である。

## 測定

合成入力 dense-normal、11 反復中央値（直接パス）。無追跡側は変更前後で完全同一。

- root-local-globalization 単独：無追跡 2,002 回・471,775 B→追跡 6,849 回・
  1,126,759 B（差 ＋4,847 回）から、追跡 4,574 回・855,703 B（差 ＋2,572 回・
  383,928 B）へ。追跡差は約 −47%、バイト差は約 −41%。
  ノード数は 6,883→7,523（新規 Assign ノード分）から 6,883→7,203（名前分のみ）へ。
  saved=1,920・コード長 40,419 は同一。時間差は負荷変動が大きく主張しない。
- 全コンパイル（同一ヘッドでの前後比較、trackcost single、5 反復中央値）：
  全コンパイル追跡差 133,644 回→130,353 回（約 −2.5%）、
  root-local-globalization 単独オン差 19,845 回→18,728 回（約 −5.6%）。
  無追跡側の確保回数も 1,839,009 回→1,838,369 回（−640 回。
  新規ノードが減ったことによる下流の削減）。
- 残りのパス単独追跡差 2,572 回の主因は、文ごとの理由構築と名前由来コピーで、
  理由の code・before・after 文字列の共有化が次の候補として残る。

## 検証

- 回帰テスト：`globalizes_mixed_root_statements_in_place` を追加し、
  変換文（通常 local・local function）と非変換文（未初期化 local）の混在を
  origin 監査付きプリンタ（unknown 0 バイト）で検証した。
  既存の同パス 4 テストも監査付きで通過した。
- 対象 3 クレート（syntax 107・minify 454＝新規 1 件含む・build 126）と
  ワークスペース全件（50 スイート・941 件）が成功。xtask check・
  fmt・clippy（警告 0）を通過。なお `--all-targets` での clippy は、
  本サイクル外の search.rs テストコードに既存警告 4 件がある（第30ラウンド由来。
  本変更のファイルには警告なし）。
- 実コーパス全件（4,155 ファイル×4 条件）の比較では、成功／失敗
  （4,127／28）・生成コード・Map 中核は全件一致で、hard・core・missing 差分は
  0 件。Map 条件でのみ説明系差分 592 ファイルが出た（下記）。
  TSV は `hidden_files/workers/w1/corpus-verify-rootglob-20261027/` に保存し、
  開発ホスト2 の一時ファイルは用後削除済み。

## 実コーパス比較での説明系差分について

Map 条件（default-map・std-map）で 592 ファイルに説明系のみの差分が出た。
nomap 条件では差分 0 件。生成コード・Map 中核・成功／失敗は全件一致する。
標本 3 ファイルの Map 全体を解決して調べたところ、差分は次の想定内の変化に
限られることを確認した。

- 変換されたルート local について、従来はルート Block の rewrite が
  「除去」disposition（理由 code `root-local-globalization`、operation `remove`）
  をルート由来に記録していたが、in-place 化で文が除去扱いにならなくなったため、
  その disposition が付かなくなった。標本では head の dispositions が base から
  当該 1 件を除いたものと一致した（1 標本では、後段の api-alias パスによる
  部分範囲の除去記録がより精細な形で残る変化もあった）。
- 理由テーブルが縮小し（標本で 51→49 件、38→36 件）、理由インデックスの
  振り直しに伴って origins の理由列の見かけが変わるが、解決後の理由内容は
  上記の除去記録の変化に帰着する。origins の理由列が1段短くなる箇所は、
  detach＋再作成経路で付いていた余分な段が消えたものである。
- 変換文そのものの由来（改名・名前由来・生成コードへの対応）は維持され、
  Map 中核の一致がそれを裏付ける。変換された構文が「除去された」のではなく
  「書き換えられた」と記録されるようになった、という表現上の改善である。
