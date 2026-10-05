# v0.3.1 子ノード走査とmap辞書構築の追加改善

2026-10-05。`feat/minify-performance-v0.3.1`で[全経路の条件](compile-all-scenes-v031.md)を継続する。直前の基準は`f3e5c87`、[印字・名前保存の計測](../verification/compile-emission-20261005.md)が対応する。Mapなし/ありの絶対時間をともに減らし、同じ新実装で最大1.2倍、可能なら1.1倍を目指す。平均値と個別条件を混同しない。

## 限定して試す箇所

1. ASTの再帰書換えが短いcall/return/blockの子IDリストを毎回heapへ複製する処理。小さい固定長のスナップショットを用い、実際に子IDが変化するときだけNodeのVecを作る。子の訪問順、親を書き換えるcallbackの挙動、NodeIdと由来の付け方を変えない。公開ASTやVec形式は維持する。
2. Source Map拡張の辞書登録で、既に所有されているreason/typed relationを一時キーへ複製する処理。参照可能な期間が明確な理由キーを借用し、関連元の中間Vecを避ける。先頭出現順・重複した役割・reasonのbasis/factsが異なる項目は維持する。出力schemaや情報量は変更しない。

既存profileは候補でのallocation・cloneが共通費用であることを示すが、古いprofileを効果の証明には使わない。両試案はWASMで測り、無効時を悪化させる／有効時にも効果がない場合は採用しない。テスト専用の従来処理で、callback列・生成物・完全なmapとJSONの比較を行う。

## 検証

同じ基準WASMを固定し、単体・全workspace・SDK・実WASM・3ブラウザー・独立consumerを検証する。44シーンと固定240設定、実コーパス4,155原文を利用する。比較から除くのはpassの実測時間、map生成者情報とそれに依存するintegrityだけ。既知失敗を成功へ変換しない。

実装・試行・負の観測・不採用案を保存し、意味のある単位でcommitする。push・公開・main/release・版番号の更新は行わない。


## 試案の絞り込み

小さい子配列snapshotとmap辞書借用は回帰テストで同一結果を確認したが、WASM pilotの改善が一貫せず不採用とした。patchと負の観測を検証領域に保存し、製品コードは元へ戻す。

次は、既存の由来tail共有を利用し、typed Contributionがすでに保持されていることを順序付きの線形比較で証明する。成功時だけ繰り返していたmembership検索を省く。欠落した役割・並べ替え・特殊な重複は従来の一般経路を使用し、Map内容を変えない。汎用hash cacheは追加しない。


通常経路の追加候補として、字句解析と印字のキーワード判定を同じ定数定義から生成したmatchへ変更し、識別子ごとの語彙全走査を避ける。公開キーワード配列の順序、厳密な大文字小文字、Unicodeを識別子として受け付けない条件を維持する。これはMap生成とは独立にparse・lint・build・minifyへ適用される。


順序付きContribution証明も独立参照とは一致したが、大きな描画や全体のpilotで実測上の短縮を確認できず、不採用にした。理論的に検索回数が減るだけで採用せず、元の製品処理へ戻した。計測データとpatchは保持する。


共通解析側では、解決済みbuiltinのpure/total/effect判定で、全API語彙への繰り返しprefix比較を避ける。公開API一覧とprivate matchを同じ宣言から生成し、一覧順・memberの文字列意味・alias・host override・effect flagsを維持する。識別子だけで未解決builtinを決め打ちする変更ではない。
