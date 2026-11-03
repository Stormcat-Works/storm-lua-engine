# literal-call-folding の relate における変換コード共有化による追跡コスト削減（2026-10-29、未公開）

compile の追跡コストのうち、literal-call-folding パスに残っていた
relate のコストを削減した。

## 切り分け

第38サイクルまでの残差（17.3 回／件）のうち、relate 5.0 回／件を
診断変種で切り分けた（診断は全て一時変種で、測定後に元の実装へ
復元した）。診断変種は directpass（inline.lua、11 反復・中央値、
folded=500）で測った。

- relate のみ外すと 2,500 回減（5.0 回／件）、context のみ外すと
  1,000 回減（2.0 回／件）で、第38サイクル値と一致する。
- relate の spans 側だけを外すと 2,000 回減（4.0 回／件）、
  derive_action 側だけを外すと 500 回減（1.0 回／件）で、
  合計 2,500 回と閉じる。すなわち relate 5 回のうち 4 回が
  related／relations ベクタへの追加、1 回が理由の変換コード確保である。
- spans 側の 4 回は、共有された related／relations ベクタに対する
  Arc の置き換えに伴うものである。置き換えではベクタバッファと
  Arc ボックスの 2 確保が必ず発生するため、追加分を事前に確保する
  変種はバイト数は減らすものの件数は変わらなかった（件数で無益な
  ため採用せず revert した）。spans 側と context 側は、現行の
  Arc ベクタ表現のままではこれ以上削れないフロアにあり、表現の
  変更は親確認が必要なため本サイクルでは着手しない。

## 採用した変更

relate の理由コードをパス全体で 1 個だけ作り、使い回す形に変更した。
記録されるコード文字列の内容は同一で、変わるのは確保回数だけである。

- crates/storm-lua-syntax/src/node_arena.rs：relate_snapshot に
  共有コード経路を追加し（derive_action_code を使う。従来経路は
  None として残し、他の呼出側の挙動は変わらない）、
  relate_within_role_code を追加した。
- crates/storm-lua-minify/src/passes/inline_functions.rs：
  fold_literal_function_calls が function-inline-site の Arc を 1 個だけ
  作り、rewrite_literal_calls 経由で try_install_scratch_folded_call
  へ渡し、relate を共有コード版に変更した。
- 回帰テスト relate_within_role_code_matches_relate_within_role を
  追加し、新旧両経路の由来が一致することと、relate が実際に
  related へ追加されていることを検証する。

等価性の根拠：共有コード版は呼出側所有の Arc を文字列内容で比較・
格納するため（transform_code の既定動作）、シリアライズされる
由来・理由は従来と同一である。既存の relations テストが
relate_snapshot と参照実装の内容一致を幅・役割ごとに検証しており、
本変更後も全て通過する。

## 測定

開発ホスト1、directpass 11 反復・中央値、inline.lua、folded=500。
無追跡 180,179 回から 180,180 回へ（共有コード Arc 自体の 1 回増）、
追跡 188,825 回から 188,326 回へで、追跡差は 8,646 回から 8,146 回へ
（約 -5.8%、17.3 回／件から 16.3 回／件）。saved=3001・コード長
4443・ノード数 4022→4022 は変更前後で同一。時間改善は主張しない。

## 検証

- 対象 3 クレート（syntax 125＝新規 1 件含む・minify 683・build 126）
  全成功、ワークスペース全件 50 スイート・1,188 件全成功・失敗 0。
  cargo xtask check・cargo fmt --check・clippy（syntax・minify lib、
  警告 0）通過。
- 実コーパス全件比較（Tier FULL、開発ホスト1 のローカルコーパス
  コーパスのローカルミラー、実行前に SHA-256 manifest 照合 4,155/4,155 OK、
  manifest ハッシュ abfec87ab0c29dc088d4e93f25cf563849b062d3eb15eeaa5d54f8d08b8341c3）：
  base は分岐元 16358ff21441ed92f866cc2afbb1152b99828944、head は
  本変更。検証バイナリの SHA-256 は base 49b51dc5…・head e2e63b4c…
  で相違を確認。4,155 ファイル×4 条件、欠落 0。両 TSV はバイト同一
  （SHA-256 7411b733a1da8a679c41b60925a81d2e6f5f3bdcc74dceae4283f139ab303972、
  16,620 行ずつ）。全条件 identical=4,155、hard・core・expl・missing
  すべて 0。成功 4,127／失敗 28 は両側同一。
