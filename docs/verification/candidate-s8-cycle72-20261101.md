# 検証記録: 候補S8（immutable-global-expression-reuse の tick 跨ぎ input.* carrier）

- 実施: w9 第72サイクル（2026-11-01）
- 対象コミット: `8e9fefc`（分岐元 `d8a3ee5`＝FULL-VERIFY-TIP／第295ラウンド先端。w7 第107サイクルの統合が先行したため、本修正は第296ラウンド。rebase 前の原コミットは `26d6c8d`、分岐元は `cb77237`＝第294ラウンド）
- 対象ファイル: `crates/storm-lua-minify/src/passes/immutable_values.rs` のみ（+257/-0、うち回帰テスト 8 件）
- ティア: FULL（生成コードが変わる正しさ修正）

## 問題

`immutable-global-expression-reuse`（pass id 18）は、チャンクレベル文に束縛された
式（carrier）と一致する式を関数本体内で carrier への参照に置換する。carrier は
チャンクのロード時に一度だけ評価されるのに対し、置換先は任意の後時点で実行され
うる関数本体である。pass の可否判定は束縛レベルの effects に依っており、
`EffectAnalyzer` を `stable_reads=true` で構築するため、`input.*` の読みは
`stable` のまま carrier 式に残る。しかしユーザー認定のホスト意味論では
`input.*` の安定性は **1 ティック内** に限られる。したがって `input.*` を観測
する carrier は、ロード時のスナップショットをティックを跨いだ新鮮な読みの代わり
に差し込む無言の誤コンパイルになる（第71サイクルの再検証で camp7001
f001173／f002415 が確定、w6 第74サイクルがティック可変スタブ c74tickexec で
境界を確定）。

w6 の最小再現（s8-min.lua）:

```lua
G1 = input.getNumber(3) + 5
function onTick() output.setNumber(2, input.getNumber(3) + 5) end
```

plain はティック毎に 1015／2015／3015 を出すが、生成コードはチャンクレベルで
1 回だけ評価した carrier を onTick が参照し、ロード時の値 15 を出し続ける。
同パス無効化で全形が復元する。

## 到達可能性（w9 実装側解析＋プローブ実測）

- carrier の束縛先はチャンクレベル文のみ（`collect_carriers` は root ブロックの
  `Local`（書込み回数 0）と `scope 0` の大域 `Assign`（書込み回数ちょうど 1）
  だけを走査する）。束縛はロード時に無条件で 1 回だけ実行され、置換は関数本体
  内でのみ起きる。**ティック跨ぎの参照は到達可能**であり、それも onTick の
  ようなホスト呼出し関数では常態である。
- 直接形: `input.getNumber`／`input.getBool` を含む任意の carrier 式（素の呼出し、
  `math.floor(...)` 等の複合、`+` の両辺、複数読み）。pass 単体プローブで
  12 形中 10 形が発火した（残りはエイリアス経由 `g(1)` と関数値そのもので、
  いずれも現行では carrier が形成されない）。
- 推移形: 本体が `input.*` を読むチャンク関数への呼出しも、サマリが
  `stable_reads` の下で pure になるため carrier になりうる（`local a = rd()`、
  引数付き・二段経由も実測で発火）。
- `property.*` は認定意味論が「実行中安定」であり、同じ carrier 構造でも
  スナップショットが全実行で有効なため欠陥ではない（温存する）。
- 副次所見: 大域 carrier は「carrier 文より前に呼ばれうる関数」への置換も許す
  ため、ロード内順序の問題が決定論的式でも別途存在する（例:
  `function f() return math.max(1,2) end` を呼び出してから `G = math.max(1,2)`
  を束縛すると、生成後の `f()` は nil を返す）。これは S8 とは別機構であり、
  本サイクルでは候補として記録のみ（親の改番待ち）。

## 修正

`TickScopedReads` を pass 内に追加し、carrier 収集直後に落とす:

- 直接観測: carrier 式内の各 `Call` について、effects の
  `resolve_builtin_reference`（証明済み single-write エイリアス経由の解決を含む）
  が `input.` で始まる参照へ解決されれば観測あり。
- 推移観測: 関数リテラルで定義される全束縛について、本体（入れ子関数本体は
  除く）が `input.*` を直接観測するか、観測する関数を呼ぶ（generic-for の
  イテレータとしての使用を含む）かを不動点で計算し、carrier 式がその束縛を
  呼べば観測あり。
- `property.*`、決定論的組込み、閉じたテーブル等の従来の再利用は不変。

単体テスト 8 件追加（input 直接／大域 carrier／複合／getBool／チャンク関数
経由／二段経由の拒否 6 件、property 直接／チャンク関数経由の温存 2 件）。
storm-lua-minify 全 1,140 件合格、変更ファイルは rustfmt・clippy 無指摘。

## 検証

- tickexec 回帰（w6 の make-tickexec.py を本ツリーへ移植した c72tickexec、
  ティック内安定・ティック毎変化の認定意味論スタブ、3 ティック）:
  修正前バイナリでは w6 確定の発散 7 形（s8-min、ヘルパー経由、チャンク局所、
  `*2`、`not getBool`、`>500`、鏡像）＋素のアンカー形が発散し、対照 4 形は一致。
  修正後バイナリでは **全 12 形が一致**（発散形の閉鎖と、property 複合・
  アンカー無しペア等の対照の不変を両方確認）。
- Tier FULL（corpuscheck 正本 SHA-256 `3c07a763…` で照合）: rebase 前の累積 base `cb77237` → head `26d6c8d` と、rebase 後の累積 base `d8a3ee5`（現行 FULL-VERIFY-TIP）→ head `8e9fefc` の **2 回とも**、全 4 構成で identical=4,155、hard=0、map-core=0、expl-only=0、missing=0、status 不変（ok 4,128／error 27）、総バイト変化 0。input 観測 carrier は実コーパスの勝者出力に現れなかった（実スクリプトではチャンクレベルの input 読み束縛が関数内複合式と一致する形が無いか、他 pass が先に処理する）。
- ワークスペース全テスト: 1,758 件合格（第294ラウンド時点の 1,750 件＋本修正の新規 8 件）。rebase 後の統合ツリーでも再実行し 1,758 件合格を確認。
