# v0.3.0 — 最適化理由付きSource Mapと精度改善

2026-10-01。合意した設計を[確定文書](../design/optimization-explanation-schema.md)と[公開スキーマ契約](../specs/optimization-map-extension.md)へ保存し、未公開のv0.3.0開発ブランチで実装した。実装commitは`c9855a3937518f7b2f25b2dd0bd929e5e8cf8c41`。v0.2.1のタグ・npm版・本番サービスを更新する工程ではない。

**Rust/WASM/TSから、最適化後Source Map v3と`x_storm`の理由・詳細範囲を取得し、同じ成果物かを検証できるようになった。** 単なる最終mapの器だけでなく、元トークン・正確なコピー、型付き寄与元、インライン化した子の文脈、消えた構文の理由を接続した。[実測例の更新版](provenance-examples-20261001.md)と[機械検証記録](optimization-explanations-20261001.json)。

## 1. 公開した開発API

TSの`compiler.minify(source, {sourceMap: true, sourceName: 'controller.lua'})`は、成功時に`code`とJSON文字列の`map`を返す。Rustの`ApiCompileOptions.source_map`も同じ契約。通常の`sourceMap`未指定/falseでは追加mapを返さず、従来の最適化を行う。

`compiler.validateSourceMap(code, map)`は、標準部分と拡張の構造・内容指紋を検査して型付き`OptimizationMap`を返す。不正な結果を空mapへ変換せずエラーにする。`CompilerWorkerClient`と実Worker RPC、バイナリ候補継続でも利用できる。

`build`と`buildLifeboat`も対応し、最適化前のリンク済み1ファイルで位置を止めず、元の各moduleへspanを合成する。リンク時の生成prefixや開発専用部分は誤って原文へ割り当てない。非短縮buildの従来の標準mapは維持し、`sourceMap:true`で詳細拡張を付けられる。

これはAPIとしての提供であり、npmの0.3.0公開を意味しない。標準リーダーはSource Map v3部分を読めるが、理由や複数寄与元のUIには`x_storm`対応が必要。

## 2. スキーマと成果物の識別

標準の`version`は3、拡張の`x_storm.schemaVersion`は1。producerのname/version/revision/dirty、生成コードと各元snapshotのbyte数/SHA-256、実効設定と採用候補の識別を持つ。エンジン版とスキーマ版は別に管理する。

絶対誤差・相対誤差の設定、propertyの数値値はf64 bit列として保存し、JSONで負のゼロや非有限数を暗黙に失わない。文字列は元の設定値を保持するので、原文とともにマップの公開範囲を管理する必要がある。エンジンの実バイナリhashを生成前に自己参照で捏造せず、検証レポートに配布WASMのhashを保存する。

マップ全体のintegrityはcanonical JSONのSHA-256。整形・キー順変更で同じ内容を拒否しない。**署名や発行者の認証ではない。** さらに、ハッシュを再計算した壊れた入力でも、未参照の理由/関連元、ID、範囲、copyの実bytes、標準mappings/namesと詳細情報の一致を検査する。合成された別ファイルの内容や同長の別Luaとの組み違いを拒否する。

## 3. 以前の実測例から改善した点

| 以前 | 今回 |
| --- | --- |
| `+`や`*`、endが式・関数全体に帰属 | 保持された演算子と所有keywordをTokenとして記録。親式の理由は別constructとして残る |
| 原文早期返却はGroupが1個だけ | `copied`で原文と生成物の一致した区間を明示。identity内の任意のUTF-8境界から元位置へ写せる |
| 子の`2`だけを選ぶとインライン呼び出し位置が消える | leafのprimaryと別にinlineContextsを保持し、定義位置と展開元を両方取得できる |
| relatedは役割なしの範囲配列 | contribution/definition/callSite/argument/parameterUse/useSiteを記録。呼び出しや引数への関係は実変換位置で指定 |
| 最後のtransformation名だけ | 選択された由来に属する構造化reasonを保持。同じruleでも判断値が違えば別記録 |
| 削除された行の理由がない | 元の範囲・除去/置換操作・収集できた根拠を保持。全生成コードが空になってもrootの記録を残す |
| 生成長だけが一致する別コードとの混同 | コード・原文・設定・mapの内容指紋と構造を検証 |

Tokenは原文に実在するそのtokenを指す場合だけ使う。例えば生成の`do`を元の`while`へToken精度で誤帰属する経路を検出して修正し、空bodyを含むfor/while/if/repeatの回帰を追加した。新しく作った演算子へ同じ字の近い原文を探して割り当てる方式ではない。

## 4. 最適化の理由

`code`と`operation`は観測した変換、`before`/`after`は構文形、`basis`/`facts`は実際に判断した場で記録した確認事項である。例えば`2*3`の評価ではoperator、i64の2/3/6、出力表現、前後サイズ、許容誤差を記録する。数値近似は値・置換値・誤差・予算をf64 bitsで区別する。未使用localの削除はread/write数とEffectAnalyzerのmovable判定、既知条件の削除は子の評価後のLua truthinessを保持する。

**すべての67パスの適用条件を形式的な証明として記録したわけではない。** 操作しか観測していないreasonはbasis=null/facts=[]を返し、正当化を生成後のコードから推測して埋めない。最終候補の選択理由と個々の変換の適用理由も別。破棄した試行の削除記録や理由を採用成果物に漏らさない。

## 5. 検証

| 対象 | 結果 |
| --- | --- |
| Native workspace | **761件成功** |
| SDK型/JS consumer | **30件成功** |
| 実WASM | **81件成功**、独立Lua backendも成功 |
| 代表30入力・240設定 Native | map ON/OFFと前commitの生成Luaが一致、全240 Unknown 0 |
| 未達target Native | 240比較で最大探索と生成Lua・由来が一致 |
| 実WASM 240設定＋未達30件 | 旧WASM/通常/理由付きmapの生成Luaと元snapshot hashの一致、実validator成功 |
| 標準map consumer | 独立したtrace-mappingで実際の元行/UTF-16列を取得 |
| 複数ファイル | 通常/LifeBoat、minify有無、Unicode/CRLF、元module・生成glueの区別 |
| Worker | バイナリ候補継続の同一mapとホスト所有RPCでの検証 |
| Browser / installed package | Chromium・Firefox・WebKitと隔離npm consumer成功 |
| fmt/default/clippy/rustdoc/architecture/licenses | 全成功 |

GitHub CI run [36781676547](https://github.com/Stormcat-Works/storm-lua-engine/actions/runs/36781676547)は、上記実装commitのLinux・Windows・macOS・WASM全jobで成功した。WASM jobはPlaygroundの既存機能・ブラウザー・梱包consumerも検証する。v0.3.0の新しい理由パネルを実装したという意味ではない。

ローカル初回のlicense検査は、シェルの古いEmscripten 3.1.69を拾って実行前条件で失敗した。対応する6.0.6をコマンド単位で選択し、最終ライセンスゲートは成功した。製品テスト失敗と環境不一致を混同していない。

## 6. 性能と実施した最適化

追加メタデータが多いため、生成物ごとにreason/related/context/削除記録を大きくコピーする実装は避け、Arc共有とコピー時変更、出力時のreason/relationプールを採用した。ブロックのdetachment検出も集合を使い二乗走査を除去。UTF-16列変換は行ごとに何度も先頭から再走査せず、索引で変換する。

初期実装の矩形4,096個のmapは約13.68MBだった。最終値は下表のとおり。**情報を落とす、Unknownで埋める、最適化を止めることで軽くしたわけではない。** 理由の異なる二つの判断を同じcodeだけで重複排除する問題も直した。

Nativeは同じrelease binaryの3回中央値。mapありは追跡からv3/x_storm符号化まで含む。validateは別測定。ファイルI/Oと検証自体はcompile時間に含めない。

| 入力・設定 | mapなし | mapあり | mapサイズ | validate |
| --- | ---: | ---: | ---: | ---: |
| 制御例・全探索 | 2.273秒 | 5.411秒 | 1.37MB | 24.3ms |
| 制御例・8192目標 | 0.269秒 | 0.539秒 | 1.52MB | 26.8ms |
| 矩形4,096個・全探索 | 3.241秒 | 7.233秒 | 8.23MB | 152.6ms |
| 矩形4,096個・8192未達 | 3.159秒 | 7.510秒 | 8.23MB | 150.8ms |

Node上のWASMも同じ入力、独立した旧/新compiler instanceを交互に実行した3回中央値。mapなしの新旧一致と、新しいmap生成の増分を分ける。

| 入力・設定 | 旧mapなし | 新mapなし | 新mapあり | validate |
| --- | ---: | ---: | ---: | ---: |
| 制御例・全探索 | 2.830秒 | 2.805秒 | 4.732秒 | 40.4ms |
| 制御例・8192目標 | 0.311秒 | 0.305秒 | 0.501秒 | 39.4ms |
| 矩形4,096個・全探索 | 2.990秒 | 2.957秒 | 6.258秒 | 235.7ms |
| 矩形4,096個・8192未達 | 2.945秒 | 3.038秒 | 6.496秒 | 259.1ms |

追跡ONは依然として時間・メモリ・map容量の増加を伴う。すべてが4秒以内と保証せず、通常WASMの小さな差を恒常的な改善/悪化と断定しない。特に詳細mapを不要な通常ビルドでは、既定のsourceMap=falseを使う。実行Luaの文字数やruntimeの負荷を増やす計測コードは挿入していない。

## 7. 残る境界

位置マップは実行中の元変数値・寿命・保存先、消えた実stack frame、bytecode PC、生成loopの特定反復を自動復元しない。VMから行しか得られなければ、単一生成行内の位置をmapだけでは特定できない。

SourceDispositionは観測した除去/置換の記録であり、「生成位置がないなら削除」と推測しない。一部の移動・統合は構造的なdetachment記録のみで、実行可能な置換先を網羅したデータベースではない。replacementSourcesの値は元範囲であってbreakpoint候補ではない。

今回の取得/保存/検証APIを用いたPlayground全体の理由表示、双方向選択、breakpoint UIはP5として残る。公開していない高度なデバッグ能力を成功扱いしない。全最適化の適用根拠をさらに詳細化する作業も、basis/factsの記録点を増やす独立改善である。

ソース・検証文書は既存の開発branchに保存し、main/release/公開済みv0.2.1は変更しない。私有コーパスの原文や生成Luaはlocal-validationに留め、公開文書には集計と新しく作った例だけを含める。
