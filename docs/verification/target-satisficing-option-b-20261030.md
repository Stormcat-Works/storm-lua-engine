# target モード satisficing の継続探索化（選択肢 B、2026-10-30）

## 決定
2026-10-30 のユーザー決定（第68サイクルの選択肢 B、source 適合を含む）：
target モード（OBJ-2）では、source／base／structural の各チェックポイントで
適合候補が現れても探索を打ち切らず、速度レイアウトまで評価して既存の
順位付け（目標適合→速度順位→サイズ→順序）で最速の適合候補を選ぶ。
適合する最適化候補がない場合のフォールバックは従来通り。目標なし
（OBJ-1）は不変。コンパイル作業量の増加（第68サイクル probe：候補数
中央値 2→5）は target モードでは compile 速度が最下位優先のため許容。

## 変更（`crates/storm-lua-minify/src/search.rs` 中心）
- `try_satisficing_from_parsed` の checkpoint マクロから適合時早期復帰を
除去し、チェックポイントは進捗計数と呼出側の checkpoint budget
（max_checkpoints）のみにした。未使用になった `target_is_met` を除去。
- `try_satisficing_compact` の identity 短絡（source 適合時の origin 表省略
パース）を除去し、常に `parse_for_search` する。source 適合が identity
返却を保証しなくなったため、勝ち得る最適化候補の tracked origins が
必要になる。`compile_code_with_source_inspection_compact` の同型の
短絡も除去し、検査経路と通常経路の結果一致を保った。
- 旧契約を符号化していたテスト 7 件を新契約に更新（minify 6 件＋build 1 件）：
適合 source の identity 返却・早期停止・source チェックポイント段階の
主張を、継続探索・最速適合勝者・full-search 終了の主張に変更。
identity 比較は勝者構造が `target:source` の場合に限定（コード同一の
印刷勝者・空入力の edge を含む）。

## 検証（Tier FULL。勝者選択が変わるため）
- ワークスペース全件：1,296 件成功・失敗 0（minify 783・build 127 ほか）。
xtask check 通過、fmt clean、clippy は lib 警告 0（minify テストモジュールの
既知 8 警告のみ）。
- 目標なしコーパス比較（4,155 件×4 条件、base 8adffd3b 対 head）：
16,620 行、TSV SHA-256 一致（6f47cfcc1965ce16632316cd650e77329899a0ec59f7cb5b5941491831c2a0b5）。
4 条件すべて identical 4,155、成功 4,128／失敗 27 が両側同一、
hard／core／expl／missing 0。目標なし動作は完全に不変。
- 累積比較（マーカー ac670ed 対 head）：4 条件すべて identical 4,148・
hard 7（全条件で同一の 7 件、文字数差 +3〜+23）。標準 FULL が本変更の
寄与 0 件を証明するため、7 件は先行ラウンド（展開位置ガード）の効果と
帰属する。FULL-VERIFY-TIP は本マージで新先端へ進める。
- target モード差分（目標＝目標なし勝者サイズ＋32、4,155 件）：
勝者変更 1,098 件（第68サイクル probe の予測と一致）、合計 −6,721 文字
（縮小 732 件・不変 3,156 件・増大 267 件、増大分もすべて目標内）。
目標超過 0、target_met 反転 0、成功／失敗の不一致 0。
主遷移：none→joint-api-local 610、source→joint-api-local-s1 264、
source→joint-api-local 76、none→joint-api-local-s1 46 ほか。
- 開発ホスト3 での採用後測定（MEASURE.lock 経由、単一 submit ジョブ、
負荷 0.35 と静穏）：
(a) 小規模合成入力の target モードコンパイル壁時間（中央値、n=5 対、
プロセス起動込み）：draw 6→9ms、tick 6→9ms、srcfit 6→7ms。
絶対値は小さいが方向は増加で、候補数増（中央値 2→5）に対応する。
コーパス規模ファイルの compile 時間は、コーパスを .us へ送れないため
未測定であり、ここでは主張しない。
(b) 変更勝者の実行時間（21 対の交互測定、中央値）：
draw −9.37%（20/21 対で head が高速、draw_commands 129 が両側一致、
出力同一）、tick −50.25%（21/21 対、出力 numbers[0]=67 が両側一致）。
いずれも合成類似入力による結果であり、コーパス実ファイルの実行時間
そのものではない。
- 実行同一性スポットチェック：遷移 3 件の base／head 勝者ペアを
vmbench で比較し、outcome・出力・draw_commands が一致した
（サイズは差分 TSV と一致）。
