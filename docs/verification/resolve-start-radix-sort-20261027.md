# resolve の start イベント列の基数ソート化（2026-10-27）

## 変更

`GeneratedOrigins::from_arena`（`crates/storm-lua-syntax/src/provenance.rs`
のみ）で、start イベント列の順序付けをタプルに対する
`sort_unstable` から、start offset に対する安定な 8 ビット桁の基数
ソートへ変えた。end イベント列は従来どおり `sort_unstable` のまま。
公開 API・生成コード・解決結果の形は変わらない。

## 正しさの根拠

- マージ判断は offset の比較のみで決まる。同 offset では end イベント
  が start イベントに常に先行し、同 offset の starts 同士は active が
  集合であるため順序不問である。
- さらに強い同一性として、基数ソートの入力順は emission index 順で、
  各桁の計数ソートは安定なため、得られる starts 列は従来のタプル
  ソートが作る (offset, index) 順と完全一致する。順序そのものが
  変わらないため、解決結果は構成上同一である。
- 8 ビット桁を選んだ理由：16 ビット桁版では 65,536 バケットの固定
  コストが小入力で逆転した（draw 入力で約 +0.08ms）。256 バケット版
  では小入力でも悪化しないことを確認した。桁数は max offset から
  決まるため、大きな入力でも桁数が対数的にしか増えない。

## 測定（resolve 単体、31 反復中央値、旧・新バイナリ交互）

| 入力 | 変更前 | 変更後 | 差 |
| --- | --- | --- | --- |
| dense-normal | 約 2.75ms | 約 2.62ms | 約 −0.13ms（約 −5%） |
| dense-lexical | 約 2.73ms | 約 2.61ms | 約 −0.13ms（約 −5%） |
| scale-80 | 約 1.35ms | 約 1.30ms | 約 −0.05ms（約 −4%） |
| draw | 約 0.42ms | 約 0.42ms | ±0 |

件数（mappings・origins・constructs・dispositions）は 4 入力すべてで
新旧完全一致した。

なお同サイクルの分解測定では、dispatch（イベント振り分け＋active
維持）が約 0.28ms、gap ブロックが約 0.8ms＋αと測れた。sort 単独の
寄与は、未ソート変種の差（約 0.40ms）に gap 計算の減少分が混ざる
ため上限としてのみ読むこと。

## テスト

- 対象 3 クレート全件成功：syntax 106 件・minify 452 件・build 126 件。
- ワークスペース全件 50 スイート成功。
- xtask check 通過、cargo fmt --check 通過、syntax クレートの clippy
  は警告なし。ワークスペース全体の clippy の警告 4 件は minify の
  テストコード由来で、本変更のファイル外（統合先端由来）。

## 実コーパス全件比較

- base：統合先端（第 30 ラウンド相当）。
- head：本変更を含む作業ツリー（コミット前の内容で検証し、検証後に
  コミットした）。
- 方法：公開 API compile をファイルごとに独立プロセスで実行、
  300 秒タイムアウト、4 並列。条件は default（Map なし／あり）と
  std（Map なし／あり。mode=smallest、numericMode=tolerant、
  searchMode=exhaustive、zeroCostNewlines=true）の 4 系統。
- 比較項目：成功／失敗、エラー文ハッシュ、生成コードの長さと
  SHA-256、Source Map の中核、正規化済み Map 全体。
- 結果：4,155 ファイル × 4 条件＝16,620 行、欠落 0 行。両 TSV は
  バイト同一。全条件とも成功 4,127 ファイル／失敗 28 ファイルで
  完全一致し、生成コード・Map 中核・正規化済み Map 全体・エラー文
  ハッシュの差分はすべて 0 件。
- コーパスの内容は本記録にも結果 TSV にも含まれていない
  （ファイル ID・ハッシュ・集計値のみ）。
