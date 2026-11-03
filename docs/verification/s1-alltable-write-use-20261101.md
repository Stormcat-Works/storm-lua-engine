# 第98サイクル検証記録: LICM 型保証部分集合の見切り／gd-wc-alltable 書込み用途強化／clippy 債務解消

担当: w4 第98サイクル。分岐元 `295e52c`（第234ラウンド）、第235ラウンド先端
`a06a994` 、さらに第236ラウンド先端 `0a4f228` へ rebase 済み。実装 `d690a3a`（alltable）＋ `63662f3`（clippy）。

## 1. 主課題: LICM 非リテラル巻き上げ（型保証つき部分集合）— 到達量ゼロで見切り

第95サイクル設計の証明義務（i〜v）に基づく専用センサス（証明・失格・ヘッダ
判定を忠実に複製し、提案する型証明だけを拡張した計測用バイナリ。合成入力
4 件で証明成立と機会検出を事前確認）:

- コーパス 4,155 ファイル（parse 成功 4,137）、対象ループ 11,588 件
  （資格あり 5,117 件）。
- ループ内対象コール（input.getNumber/getBool、property.getNumber/getBool）:
  1,734 件 = リテラル引数 109（既存枠組みで処理済み）、**Name 引数 625、
  そのうち定義式で型が静的に証明できるもの 0、機会 0**、その他引数形 1,000。
- 未証明 Name 引数 625 件の内訳: **当該ループ自身の for 変数（同一ループ
  レベル）604**、グローバル 16、証明域（祖先ブロック）外ローカルの Bin 初期化
  4、再代入ありローカル 1。API 別は input.getBool 329 / input.getNumber 295 /
  property.getNumber 1。

結論: 親決定の部分集合（定義式が型を保証する不変引数）の到達量は実測ゼロ。
for 変数は反復ごとに変わるため不変量ではなく、全面形でも対象外。よって
本サイクルでは LICM 本体を変更せず、申し送りの pivot へ移行した。全面形は
引き続き保留（本センサスでも裏付けなし）。

## 2. pivot: gd-wc-alltable の証明強化 — 書込み用途に限り実装

第96サイクルの上限診断（sole 87 件）は zip 位置対応の診断近似だったため、
厳密センサス（全書込みの位置 zip 列挙。末尾多重代入先は nil／展開として
非 Table 扱い、Funcstat は失敗、列挙件数と resolver 書込み件数の一致を必須）
で切り分けた:

- 書込み 2 件以上のグローバル束縛 21,200 件中、厳密な全 Table 定義は
  449 件（Funcstat 含み 125、非 Table 含み 20,650、件数不一致 0）。
- 全定義がエスケープしないもの 116 件。その base へのフィールド書込みを
  含むループは **18 件／12 ファイル／書込み 18 件**。

実装（`loop_member_reads.rs` / `single_member_cache.rs`）:
`global_all_table_def_targets` が上記の厳密列挙を行い、`base_proven_write_use`
が「単一定義の従来証明 OR 複数定義の全 Table＋全定義ターゲットを免除した
エスケープなし」の場合に S1 の書込み base（`write_base_harmless`）を認める。
健全性: どの定義が最後に実行されても書込みは新規の plain Table に落ち、
定義前の書込みは nil エラーで同一（S1 は書込みを移動・追加・削除しない）。
**限定: 読み用途には使わない。** MR（`loop_member_reads::proven`）と SMC は
従来どおり単一定義証明のみ——定義間で denotation が変わりうるため。
この限定はコードのドキュメントコメントにも明記した。

テスト: S1 単体テスト +7 件（肯定 3: 複数定義／多重代入 zip／他関数内の
定義、否定 4: 非 Table 定義／Funcstat／末尾 nil 多重代入／エスケープ）。
非空虚性も確認: 呼出しを従来証明へ戻すと肯定 3 件だけが失敗する。

## 3. clippy 債務（w7 第83サイクル記録分、親からの追加小課題）

`single_member_cache.rs` の alias chase の `next.unwrap()` が workspace の
clippy deny（`unwrap_used`／`expect_used` の両方）に違反し、基点時点で lib の
clippy が落ちていた。直前の両分岐は定義を記録するか早期 return するため
到達不能な None だが、expect も deny のため、コードベース標準の let-else
形式（不変条件が将来崩れても panic せず保守側に倒れる）に置換した
（実装 `63662f3`）。到達可能な経路の意味論は不変。

## 検証

- ワークスペーステスト: **1,611 件全通過**（rebase 後先端。第234ラウンド
  時点 1,604 + 新規 7）。
- clippy: `cargo clippy -p storm-lua-minify --lib` および
  `cargo clippy --workspace` がエラーなしで完走（既存警告のみ）。
- Tier FULL: base（`295e52c`）vs head の corpuscheck（4 構成 × 4,155
  ファイル、16,620 行／側、fallback 0）は **TSV バイト一致**
  （SHA-256 `743431aa4bb3142c3661c78dfdbf63219052704c8d763fbd35cda85d73279be7`）。
  rebase 後先端でも head 側を再実測し同一ハッシュを確認（第235・236ラウンドは
  いずれも Tier INVARIANT で同ハッシュが記録済みのため累積合成が成立）。
- target-delta（targetbench、全コーパス）: slack 32 は勝者遷移 0 件。
  slack 128 は **遷移 1 件**（ファイル idx 3784、joint-api-local →
  joint-api-local-s1、2,147 → 2,181 文字、+34）。同一レイアウト内の
  サイズ差は 0 件。
- diffexec（遷移ファイル、head）: plain／opt／safe／target32／target128 の
  全構成でトレース sha 一致（`51167b7e…`）。
- .us2 測定（MEASURE.lock、同一機交互 31 rep、onTick 1,000 回／rep）:
  base（joint-api-local）中央値 1,058.165 µs/tick vs head
  （joint-api-local-s1）1,052.342 µs/tick（−0.55%、単一ファイルでは
  ノイズ範囲）。出力スナップショットは両側一致
  （numbers[0]=-0.5367088、numbers[1]=1.3898737、booleans[0]=false）。

## 申し送り

- LICM 非リテラル巻き上げは型保証部分集合・全面形とも実測裏付けなし。
  再開するなら for 変数以外の新しい事実（例: 実行時型情報）が要る。
- gd-wc の残り（sole 87 上限の残部）は zip 近似と Funcstat／非 Table 定義が
  大半であり、本件で証明可能な部分は回収済み。
