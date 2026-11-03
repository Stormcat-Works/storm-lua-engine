# トークン表の複数ソース対応と混在形（部分保存）— 検証記録（2026-10-30）

ユーザー決定（2026-10-30 16:34 JST）による schema-2 残件2件の実装記録。
第167ラウンドで残した既知の限界、(1) 複数モジュール project のトークン表化
（ゲートの単一ソース条件）と (2) lifeboat の部分的な単純形保存を、一つの
設計パスで扱った。変更は Map 表現のみで、生成コードとリネーム自体には
触れていない。validate／validate_for_code の検査は一切緩めていない
（混在形の読み側にはギャップ検査を追加した）。

## 設計

### 1. 複数ソースのレコードレイアウト

レコードレイアウトを source 数で切り替える。

- sources＝1：従来レイアウトのまま（完全 copy 4要素、リネーム5要素）。
  既存の単一ソース Map はバイト単位で不変であり、従来どおり読める。
- sources≧2：レコード第2要素に source index を挿入する
  （完全 copy 5要素 `[generatedStart, source, sourceStart, sourceEnd, nameId]`、
  リネーム6要素で末尾に generatedEnd）。

旧ゲートが「トークン表を持つ Map は sources＝1」を保証していたため、
source 数だけでレイアウトを判別でき曖昧さはない。schemaVersion は 2 の
まま据え置く（schema 2 は未リリース。書手・読手・TS 型・仕様書を同時に
更新する第167ラウンドと同じ互換性スタンス）。複数ソースのトークン表は、
単一ソース前提の旧読手では従来の「token table requires a single source」
エラーで棄却される（fail-closed）。

ゲート（`simple_copy_token_table`）は source＝0 の固定を外し、source の
範囲内であることと copied＝primary（source 一致を含む）を要求する。
一意性タプルに source を加える。展開（`expand_token_table`）はレイアウトを
source 数で選び、レコードごとに該当 source のスナップショットで名前照合し、
primary.source を復元する。生成順・範囲・生成端の検査は従来どおり。

### 2. compose の部分保存と混在表現

根本原因は2段ある。lifeboat の入力では約69件のテンプレート由来の非単純
origin が混ざるため compose の単純形 fast path が不成立となり、従来経路が
origin を mapping 部品ごとに複製してタプル一意性を壊していた。さらに
エンコーダ側にも「単純部分だけをトークン表にする」表現がなかった。

- compose（`try_compose_partial`）：origins／contexts／dispositions の変換は
  従来経路と同一に行い、変換後も単純形を保ち（simple-stable）、copied が
  単一のリンク範囲で完全に覆われ、その source span が変換後 primary と
  一致する mapping は origin を複製せず単一のまま保持する。従来経路の
  複製でも primary は同じ span になり名前 slice 検査も同じ結果になるため、
  その mapping の標準導出は同一で、origin 表だけがコンパクトになる。
  synthetic なギャップ origin は実際にギャップを出すときだけ遅延追加する。
  部分試行はクローン上で行い、fast 分岐が1件も発火しない場合や
  validate_for_code が失敗した場合は pristine な入力で従来経路をそのまま
  実行する（非単純な入力の出力は従来と完全一致）。
- 混在表現（`extract_token_table`）：一般形エンコーダの構築直後に抽出する。
  条件は contexts／constructs／dispositions が空、mapping が生成コードを
  連続して覆う、非単純な残余 origin が256件以下かつ単純 origin 件数以下、
  単純部分が純粋形と同じ一意性・初出順・名前 slice 条件を満たすこと。
  ワイヤでは tokens／tokenNames が単純部分を担い、origins／mappings は
  残余部分だけを（origin 参照は残余配列への index に振り直して）担う。
  reasons／relations プールは共有のまま残る（単純 origin はどちらも
  参照しないことが分類で保証される）。
- validate の混在展開：tokens と origins／mappings が両方とも非空なら
  混在形として扱う。トークン展開の未知ギャップの内部に残余 mapping が
  完全に収まり互いに重ならないことを検査し、ギャップを分割して差し込む。
  最終 origin 列は「展開された単純 origin＋残余 origin」で、残余 mapping の
  origin 参照には単純 origin 件数のオフセットが付く。ギャップを跨ぐ残余
  mapping は棄却する（検査の追加であり緩和ではない）。純粋形
  （ワイヤの origins／mappings が空）は従来どおり全体を置換する。
  返却する拡張は正規化済みの実体化形とし、tokens／tokenNames は消費済み
  として空にする。これにより返却値の再エンコードは従来レイアウトになり、
  再検証で同一の詳細へ展開される（第167ラウンドまでの「tokens を保持した
  まま返す」形では、再エンコードが不正な混在形になるため改めた）。

## 測定

測定は bench の c76probe／c79probe（public_api 経由）で、base＝07fad008f、
head＝本変更。生成コードは全経路で base／head 同一（code_len 一致）で、
全 Map が validate を通過した。

### (a) 複数モジュール project（コーパス由来モジュールから構成）

字句勝者3件（3bedf3f1／4067b474／a37299b7）をモジュール wa／wb／wc とし、
entry が3件を require する project（sources＝4）を構成した。

| 経路 | base | head | 削減 |
| --- | ---: | ---: | ---: |
| project | 49,485,112 B（origins 223,156・tokens 0） | 6,594,895 B（tokens 118,761・残余 origins 34） | −42,890,217 B（−86.7%） |
| lifeboat | 49,510,473 B | 6,613,322 B | −42,897,151 B（−86.6%） |

最大の勝者1件＋小さな helper モジュールの2モジュール構成（sources＝2）でも
同型：project 32,312,242 B → 4,796,375 B（−85.2%）、lifeboat 32,330,451 B →
4,810,953 B（−85.1%）。トークン記録は5／6要素（複数ソースレイアウト）で
あることを確認した。

### (b) lifeboat 成功3件（単一モジュール）

| ファイル | base | head | 削減 |
| --- | ---: | ---: | ---: |
| 3bedf3f1 | 15,608,018 B | 1,499,929 B | −90.4% |
| 4067b474 | 32,317,810 B | 4,652,429 B | −85.6% |
| a37299b7 | 1,463,561 B | 190,763 B | −87.0% |
| 合計 | 49,389,389 B | 6,343,121 B | −87.2% |

残余 origins は3件とも正確に69件（テンプレート由来の非単純 origin）で、
トークン数は単一 compile と同じ 38,353／76,665／3,743 件になった。

### (c) Tier FULL でのコーパス全体の Map サイズ効果

base 07fad008f との全コーパス比較（4,155ファイル×4条件＝16,620行）は
**全行 identical**（expl／core／hard／missing ともに 0、成否は ok 4,128／
error 27 で一致）。マージ準備の過程で当時の統合先端（第172ラウンド f5fcf8714、
第174ラウンド 439aced2f、第176ラウンド 72f604286）を base として
再比較し、いずれも全行 identical を再確認した。単一 compile 経路の
コーパス Map 合計は不変である。
理由：コーパス harness が対象とするのは単一 compile のみであり、一般勝者は
全件が補助表（contexts／constructs／dispositions）を持つため設計どおり
抽出対象外で、字句勝者は既に純粋形だった。サイズ効果は (a)(b) の
project／lifeboat 経路に現れる。

### (d) 単一 compile／単一モジュール経路への影響

なし。単一 compile の Map は、producer の revision／integrity（ビルド
メタデータ）を除いてバイト同一であることを直接 diff で確認した。
単一モジュール project の3件合計も第167ラウンド記録と同一の
4,581,213 B（1,199,637＋3,267,018＋114,558）だった。

## テスト

- 対象クレート（storm-lua-build）134件成功（新規：部分保存の混在入力
  テスト、複数モジュール compose のトークン表テスト。既存の
  「非単純入力は従来経路」テストは、前提を「補助表だけでは部分保存を
  妨げない」形に合わせて更新した）。
- ワークスペース全件 1,370 件成功・失敗 0（最終 rebase 後のツリー）。`cargo xtask check` 通過、
  `cargo fmt --check` 通過、対象クレートの clippy 警告 0。

## 残るコストと申し送り

混在形では、トークン間の未知ギャップにあたる mapping が残余として
明示的に残る（(a) の構成では残余 mapping 22,391件）。未知 mapping を
省略すると validate の標準フィールド照合（マージ結果と標準導出の一致）
に反するため、現設計では残す。ギャップと正確に一致する未知 mapping の
省略は、標準導出側の扱いとセットで別途設計が要る。
