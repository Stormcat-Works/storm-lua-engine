# SMC 複合レイアウト（S1＋SMC・MR＋SMC）の追加（2026-10-30）

## 変更

target モード限定レイアウトはこれまで、api エイリアス配置に対して
S1・MR・SMC をそれぞれ単独で適用したものだけだった。両方の機会を
併せ持つファイルでは上位レイアウトのみが勝ち、下位分は拾われなかった。
本変更は SMC を S1／MR レイアウトの上に重ねた複合レイアウトを追加する
（S1／MR の挿入コードは SMC のセグメント証明を壊さない。SMC の証明は
基底の非再代入と書込み回数で決まり、S1／MR のキャッシュ局所は一度だけ
代入される fresh な局所であるため）。

- `search.rs` に target モード限定レイアウト
  `joint-api-local-s1-smc`／`joint-api-local-mr-smc` と licm 側の
  `joint-api-local-licm-s1-smc`／`joint-api-local-licm-mr-smc` を追加
  （両パスが発火したときにのみ提供するフル変種のみ。部分変種の複合は
  将来項目）。S1／MR レイアウトの AST を複製して SMC を適用する。
- 速度順位は両構成要素より上に置く（licm-s1-smc 0、licm-mr-smc 1、
  licm-s1 2、licm-s1-partial 3、licm-mr 4、licm-smc 5、
  licm-smc-partial 6、licm 7、local-s1-smc 8、local-mr-smc 9、
  local-s1 10、local-s1-partial 11、local-mr 12、local-smc 13、
  local-smc-partial 14、local 15、joint-api 16、キャッチオール 17）。
  既存レイアウト間の相対順序は不変である。
- root-local-globalization polish の適用除外に複合 4 レイアウトを追加した。
- `layouts_per_job` を 12 から 16 へ引き上げた（1 ジョブあたり最大
  local 側 8＋licm 側 8＝16）。
- `straightline_member_reads.rs` のモジュール文書に複合の順位を追記した。

## 単体テスト

複合の逐次適用テスト 2 件を追加（SMC テスト計 26 件が成功）。

- MR の後に SMC を適用すると両方が発火し、生成コード中の `t.x` が
  1 箇所（SMC キャッシュ定義）のみになること。
- S1 の後に SMC を適用すると両方が発火し、S1 キャッシュ定義の
  callee 読取（非証明ホスト基底）は SMC に拾われず、`t.x` が 1 箇所に
  なること。複合出力は再パースできること。

## 検証区分

Tier FULL（勝者選択が変わるため）。

- ワークスペース全件 1,348 件成功・失敗 0（minify 828 件）。
  `cargo xtask check`・fmt・clippy（lib 警告 0。`--all-targets` の
  loop_member_reads.rs テスト由来の既存エラー 3 件と search.rs
  テストモジュール由来の既存警告 8 件のみで、新規分 0）を通過。
- 目標なしコーパス比較（4,155 件×4 条件、base 115e1fc6 対 head）：
  16,620 行、TSV バイト同一（SHA-256 cad1d4e5…）。目標なしコンパイルは不変。
- target モード差分（目標＝目標なし勝者サイズ＋32、4,155 件）：
  勝者変更 3 件（local-s1 → local-s1-smc 2 件：idx 626・1579、
  licm-s1 → licm-mr-smc 1 件：idx 645）、合計 +20 文字、目標超過 0、
  target_met 反転 0。遷移はすべて複合レイアウトへのもので、既存
  レイアウト間の後退はない。
- 遷移ペアの実行同一性（vmbench、200 ticks／20 draws）：3 件とも
  両側が同一種のホスト状態依存エラー（nil 値の index）で失敗し、
  スレッド ID を正規化すると出力は同一。実行可能ペアはなかったが、
  失敗の種類・位置・出力は両側で対応する。
- 開発ホスト3 採用後測定（MEASURE.lock 経由、単一 submit ジョブ、
  負荷 0.21→0.21）：合成入力 2 件（tick：base mr 131→head mr-smc 137、
  目標 137。draw：base s1 161→head s1-smc 189、目標 210）。
  target コンパイル壁時間は tick・draw とも中央値 16→16／18ms
  （プロセス起動込み、n=5 対、対差は ±3ms 以内）。
  変更勝者の実行時間（21 対の交互測定ペア中央値）は
  draw −5.24%（15/21 対で高速）・tick +35.39%（6/21 対で高速）。
  tick の .us 結果はサンプルが二峰性（head 側に約 10.5µs の群）で
  干渉ノイズの疑いが強いため、開発ホスト2 で同一ペアを再測定した
  ところ tick +0.83%（6/21 対）・draw −1.26%（11/21 対）でほぼ同等
  であり、減速の証拠はないと判断した。スモーク出力は両側同一。
  合成入力による結果。

## 将来項目

- 部分変種の複合（s1-smc-partial／mr-smc-partial）：フル複合が目標を
  超過するファイルの回収策として未対応。
- S1／MR レイアウトとの合成は本変更で対応済みのため、第76サイクル
  申し送りの同項目は完了とする。
