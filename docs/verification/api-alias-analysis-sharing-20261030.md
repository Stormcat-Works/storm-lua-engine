# API エイリアス解析の global／localized 間共有（2026-10-30）

## 変更概要

target モードでは、joint-api レイアウト（global 形）と localized
レイアウトが同じ phase ツリーに対して API エイリアスパスを2回実行する。
両入口（`optimize_api_aliases` / `optimize_api_aliases_localized`）は
単一の内部実装の `localize` フラグ違いであり、各呼び出しが
origin なし clone・`resolve`・`EffectAnalyzer` 構築・builtin 使用回数の
scan という同一の「第一解析」を自前で構築していた。

本変更は第一解析を `passes::api_aliases::ApiAliasAnalysis` として切り出し、
search のレイアウト構築（target モードのみ）で phase ツリーに対して
1回だけ構築して両呼び出しに共有する。localized 基底が relaxed carrier
synthesis で差し替わった場合は従来どおり自前構築の入口を使う。
target なし経路・LICM 基底・選択探索（候補構築と remove/add 探索）には
一切触れていない。

## 等価性（勝者不変の構成的立証）

- 解析の全構成要素は `(source, root)` の決定的純粋関数である:
  `clone_without_origins` は arena／文字列表の構造コピー、
  `resolve` と `EffectAnalyzer::new` はツリーの読み取りのみ、
  scan は使用回数の集計のみで、照会は全て `&self`。
- search 側の各呼び出しは従来 `phase_ast.clone()` を入力にしていた。
  Ast の clone は node／symbol の ID と順序を保存する深いコピーのため、
  `clone_without_origins(phase_ast.clone())` と
  `clone_without_origins(phase_ast)` は同一の値になる。
- 自己完結入口は「source／resolution／解析を自前構築してから共通実装
  `run_with_analysis` を呼ぶ」形に再構成され、演算と順序は従来と同一。
  共有入口は同じ共通実装に外製の解析を渡すだけである。
- 等価性ユニットテスト（新規1件）: 複数入力 × global／localized で、
  自己完結入口と共有入口（解析は入力の深い clone から構築し、実行は
  さらに別の clone に対して行う、search の共有形そのもの）の
  出力コード・選択エイリアス・saved が完全一致することを固定した。

## 重複の実測（実装前）

一時計装（環境変数ゲート、未コミット）でコーパス target 実行
（slack32）を測った。target ジョブ 8,825 件中、localized 基底が
差し替わらず解析を共有できるのは 8,371 件（94.9%）。
解析構築は pass 呼び出し 26,781 回すべてで各1回走り、pass 全体時間の
約 2.0% を占めた（最大級ファイル6件でも約 1.7%）。支配的なのは
選択探索側であり、当初見積もり（構築コストの 15〜20%）は過大だった。

## 測定結果

- 決定的計数: 実装後の同一計装で解析構築回数は
  **26,781 → 18,410（−8,371）** と、共有可能な組の実測値と正確に一致。
  pass 呼び出し回数・localized 呼び出し回数は不変。
- 開発ホスト3 確認（MEASURE.lock 単一ジョブ、21回交互反復の中央値、
  勝者は全仕様で同一）: 分岐元 6172d0c79 に対しては代表入力9仕様すべてで
  −0.25%〜−0.91%（draw −0.47% / −0.81%、dense-normal −0.72% / −0.25%、
  scale-320 −0.79% / −0.91%、common −0.83% / −0.77%、目標なし対照
  −0.38%）。マージ準備中に統合先端が第183ラウンド（5865f5eb）へ進んだ
  ため rebase し、新基底でも再測定したところ差は ±0.5% のノイズ域内に
  収まった（−0.54%〜+0.36%）。壁時計効果は入力あたり 1% 未満で、
  第2回測定ではノイズと分離できない。効果の確実な証拠は決定的計数
  （コーパス target 全周あたり解析構築 8,371 回の除去）である。

## 検証（Tier FULL）

- ワークスペース全件テスト 1,382 件成功（minify lib は等価性テスト
  追加分を含む）。xtask check・fmt 通過。clippy（lib）は変更域クリーン
  （既存の literal_folding 警告1件のみ、分岐元でも同一）。
- 目標なしコーパス FULL: 分岐元 6172d0c79 対 head で 16,620 行
  **バイト同一**。rebase 後は新基底 5865f5eb 対 head でも 16,620 行
  **バイト同一**（FULL-VERIFY-TIP = 5865f5eb からの累積も兼ねる）。
- target-delta（slack32、target = 目標なし勝者 + 32）: 両基底とも
  勝者・サイズ・レイアウトの全 4,155 行が **バイト同一**（遷移 0 件）。
- ワークスペース全件テストは rebase 後のツリーでも 1,382 件成功。
