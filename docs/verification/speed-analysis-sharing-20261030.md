# 速度パス（S1／MR／SMC）の初回解析共有（2026-10-30）

## 変更

target モードのレイアウト構築段では、1 つの基底 AST（localized 基底と
LICM 基底）に対して S1 full／S1 partial／MR／SMC full／SMC partial の
最大 5 回の速度パスが走り、各回が同一内容の arena に対して
`resolve()` と `EffectAnalyzer` 構築（内部で `mutated_builtin_roots` と
別名・関数要約の fixpoint を全事前計算）を個別に再構築していた。S1 は
追加で `mutated_builtin_roots` を自前にもう 1 回計算していた。

- 新規モジュール `passes::speed_analysis`：基底ごとに 1 回だけ構築した
  解析三点（Resolution／EffectAnalyzer／mutated 集合）の借用ビュー
  `SpeedAnalysis` を定義し、等価性の根拠をモジュールドキュメントに明記。
- S1／MR は Transformer が EffectAnalyzer を所有せず借用するよう変更し、
  `*_with_analysis` 入口を追加。SMC は EffectAnalyzer を使わないため
  Resolution のみを共有する `*_with_resolution` 入口を追加。従来入口は
  従来どおり自前構築で、他の呼び出し元への影響はない。
- `search.rs` は両基底で三点を 1 回構築し、5 つの共有対象呼び出しへ
  渡す。合成実行（S1+SMC／MR+SMC）は変換後 AST に対する実行のため
  従来入口のまま。基底あたり resolve 2 回・EffectAnalyzer 構築 2 回・
  mutated 計算 3 回分の重複を回避する。

## 等価性（勝者不変）

`resolve`／`EffectAnalyzer::new`／`mutated_builtin_roots` はいずれも
AST arena 内容に対する決定的純粋関数であり、EffectAnalyzer の照会は
全て `&self` で内側可変性はない。各パス内部の作業 AST は基底の
逐語クローン（nodes／strings arena の完全コピー）であるため、基底上で
構築した解析はパス私製クローン上で構築した解析と全照会で同一の値を
返す。生成コード・勝者・Source Map は構成上不変。

## 検証（Tier FULL で実施。設計上は INVARIANT 相当だが裏付けとして FULL を実行）

- 等価性ユニットテスト 3 件を各パスに追加：発火入力・不発火入力で
  従来入口と共有入口の（出力コード、cached 数）が完全一致。
- コーパス FULL 比較（開発ホスト1、manifest 照合済み 4,155 ファイル）：
  base（`07fad008f`）と head の TSV が 16,620 行バイト同一
  （SHA-256 `154d7ea2e6114a2907b0bde26316aa9c4114bd07e3af86af36e2b31beb874edf`）。
- ワークスペース全件テスト成功・失敗 0（minify lib 834 件、新規 3 件含む）。
  xtask check 通過、fmt clean、clippy（minify lib）警告 0。
- 決定的計数：代表入力 6 件（dense-normal／common／scale-320／draw／
  S1 発火合成 t3／t4）の全モードで candidates・attempted・勝者・
  バイト数が新旧完全一致。

## 効果測定（開発ホスト3、MEASURE.lock、3 ラウンド交互・各値は複数反復の中央値）

- 小入力 t3／t4（S1 発火、target コンパイル約 6ms）：全 8 行で
  −0.3〜−1.0%。
- 構築比率の大きい dense-normal（約 335ms）／scale-320（約 753ms）：
  −0.6〜+0.4%（中央値約 −0.2%）。勝者・バイト数・計数は全行一致。
- 実現削減は target コンパイル時間の約 1% 以下と小さい。事前の上限
  見積もり（構築コストの 5〜9%）は、速度パス走査内部で解析構築が
  占める割合を過大に見ていた（走査自体の clone＋変換が支配的）。
  ただし全 16 行中 14 行が非負方向で、回帰の証拠はない。重複解析の
  除去自体は決定的に成立している。
