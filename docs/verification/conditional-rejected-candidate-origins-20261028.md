# conditional 系の棄却候補への由来付け省略による追跡コスト削減（2026-10-28、未公開）

## 焦点

compile の追跡コストのうち、conditional 系 3 パス
（conditional-call-lowering／conditional-assignment-lowering／
conditional-return-lowering）に残っていた、棄却された候補ノードへの
由来付けコスト。合成入力 dense-normal では 3 パスとも発火 0 件
（生成コード長は all-off と同一）なのに、パス単独の追跡差が
約 800 回ずつ残っていた。

## 切り分け

3 パスの共通構造は「候補生成→由来付け→採用判定」の順で、
`attribute_conditional_nodes` が採用判定の前に走っていた。
候補生成は採用されなかった候補のノードもアリーナに残すため、
棄却候補のノード 1 件あたり finish_rewrite 1 回分のスロット確保が
追跡差に積み上がっていた。採用された候補の由来付け集合は、
候補生成直後の `[start, end)` で決まる。

## 採用した変更（minify クレートの conditionals.rs のみ＋回帰テスト 1 件）

- `attribute_conditional_nodes` に `end` 引数を追加し、由来付け範囲を
  `[start, end)` に限定した。`end` は候補生成直後に取得するため、
  採用判定のための比較ブロック（guard-return の old_block／new_block など）
  は従来どおり由来付けの対象外である。
- 3 呼出箇所（call／assignment の transform_block、return の
  full-return と guard-return）を、候補の採用が確定した後にだけ
  由来付けを行う形に変更した。採用された候補の由来付け集合・
  fallback との relate・transformation 名は従来と同一である。
- 回帰テスト `rejected_call_candidate_leaves_output_unchanged` を追加
  （多引数の同一関数 if／else で三項形が長くなり候補が棄却される
  入力で、出力が if 形のままであることを検証）。

## 等価性の根拠（Tier INVARIANT）

棄却された候補ノードはパス出力から到達不能である。生成由来は
印字出力（到達可能ノード）の emissions から作られる
（`GeneratedOrigins::from_print` が印字ルートと emissions のノードだけを
参照する）ため、棄却候補ノードの由来の有無は生成コード・Source Map・
由来監査のいずれにも観測されない。採用された候補については由来付け
集合が従来と同一のため、出力は構成上不変である。

## 測定（合成入力 dense-normal、VM、中央値。時間差は主張しない）

- trackcost single（5 反復）：conditional-call-lowering の単独オン差
  803 回→1 回、conditional-assignment-lowering 803 回→1 回、
  conditional-return-lowering 802 回→0 回。api-alias・scope-renaming・
  constant-folding・root-local・all-off 差は不変。
- trackcost default（5 反復）：全コンパイル追跡差 63,932 回→54,284 回
  （−9,648 回、約 −15%）。単独オン差の合計（約 2,408 回）を上回る
  削減は、探索中の候補評価で各パスが繰り返し走るためである。
  無追跡側・コード長・候補数はすべて同一。
- directpass（11 反復）：api-alias 追跡差 2,685 回・scope-renaming
  1,181 回・root-local 652 回・constant-folding 2,258 回はすべて不変。

## 検証

- 対象 3 クレート（syntax 123・minify 632＝新規 1 件含む・build 126）
  全成功、ワークスペース全件 50 スイート・1,135 件全成功。
  `cargo xtask check`・`cargo fmt --check`・clippy（minify lib、警告 0）通過。
- 検証区分は Tier INVARIANT（構成上出力不変の、到達不能ノードへの
  由来付け省略）のため、全件コーパス（4,155 ファイル×4条件）は
  次回 Tier FULL の累積比較（起点 882169d0）へ繰り延べる。
  INVARIANT 連続は本サイクルで 3 件目となり上限に達するため、
  次回マージは Tier FULL とする。
- 注：本サイクル時点では 開発ホスト2（コーパス実行ホスト）への
  接続ができず（SSH 接続時にプロキシが CONNECT を閉じたことを
  実測）、仮に Tier FULL を選んでも全件コーパスは実行できなかった。
  INVARIANT への繰り延べはその点でも整合する。.ud 側への介入は
  一切行っていない（接続確認のみ）。
