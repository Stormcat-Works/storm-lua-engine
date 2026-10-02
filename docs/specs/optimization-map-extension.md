# 最適化説明付きSource Map — x_storm schema 1

対象: v0.3.0。最適化器の詳細由来を正本に、標準Source Map v3と本拡張を一つのJSONとして返す。Source Map本体の`version`は3であり、`x_storm.schemaVersion`、`producer.version`とは独立である。[採用方針](../design/optimization-explanation-schema.md)。

## 公開API

`compiler.minify(source, {sourceMap:true, sourceName:'controller.lua'})`は、通常の結果に`map?:string`を追加する。`compiler.build(project,{sourceMap:true})`と`compiler.buildLifeboat(project,{sourceMap:true})`にも同じ指定が使える。Rustは`ApiCompileOptions.source_map/source_name`に対応する。sourceNameは表示用の非空文字列で、ファイルへのアクセス権やruntime chunk名ではない。

最適化mapは明示指定時だけ作る。指定しない既存minifyはmapを返さず、由来追跡・説明の収集を有効にしない。非短縮buildは従来から標準mapを返すため、その動作を維持し、sourceMap=trueで詳細拡張付きmapへ切り替える。

`compiler.validateSourceMap(code,map)`は、マップと対応する生成Luaを照合して`OptimizationMap`を返す。不正なJSON/スキーマ、異なるコードやsnapshot、不正範囲・参照・fingerprintは例外になる。`CompilerWorkerClient.validateSourceMap`も同じ検証をWorker側で実行する。Rustの対応は`storm_lua_build::optimized_source_map::validate`。

通常のSource Map readerは標準のsources/sourcesContent/names/mappingsを使用する。Storm拡張を理解しなくても標準の位置対応を解決できるが、理由や関連元・合成部分・削除情報の表示には本拡張を読む必要がある。

## トップレベル

| フィールド | 契約 |
| --- | --- |
| schemaVersion | 本拡張の形式版。現在は1。未知の版は拒否 |
| producer | name=storm-lua-engine、生成したCargo版、Git revision、dirty状態。Git情報がない配布ソースではrevision=unknown/dirty=nullを明示 |
| coordinateUnit | 拡張内の位置はutf8-bytes。標準mappingsの列はUTF-16 |
| generated | 生成Luaの正確なUTF-8バイト長とSHA-256 |
| sources | 標準sources/sourcesContentと同順の長さ・SHA-256。表示名が同じで内容が違うsnapshotも別ID |
| compilation | 記録した設定・実効pass設定・数値表現/許容誤差・採用結果 |
| origins | 主な元範囲、精度、名前、型付き関連元ID、理由ID |
| reasons | 重複排除した理由・観測事実の表 |
| relations | 重複排除したroleとSourceSpanの表 |
| contexts | inlineの定義側・呼び出し側の文脈 |
| mappings | 最終コード上の非重複区間、origin ID、正確なcopy、inline context ID |
| constructs | 式・文など、細かい区間を囲む構文範囲。区間は重なり得る |
| dispositions | 採用候補から明示的に外された元構文、置換記録と理由 |
| integrity | integrity自身を空文字列にしたcanonical JSON全体のSHA-256 |

Rustの公開構造とvalidatorが実装契約で、TypeScriptの型は`optimization-map-types.ts`に置く。unknownな追加フィールドの扱いは標準mapと拡張で異なる。標準の未知プロパティは通常readerが無視できるが、schema1の構造化x_stormには不正・未対応のフィールドやenumを黙って受理しない。

## 位置・copy・文脈

SourceSpanは`{source,start,end}`で、startを含みendを含まないUTF-8 byte範囲。sourceはsources配列のindexである。元スナップショット、境界、原文の名前との一致を検証する。

mappingsのorigin=nullはUnknown。Syntheticはoriginを持つがprimaryを持たず、生成理由を記録する。UnknownとSyntheticは同義ではない。Source/Derivedのprecisionはname/token/expression/statement/groupであり、Sourceでも文字対文字の一致を保証しない。

`copied`が存在する場合、その生成区間と対応する原文sliceはbyte-identicalである。区間内部の位置を差分だけ移すことができる。原文をそのまま返すtarget早期達成も明示的なidentity copyになる。`0x10`から`16`のような表記変更はcopyを主張しない。

保持された演算子、対応する始端keyword/end、ループのdo・最初のthen・repeatのuntilなどは、パースした構文の所有tokenへ結び付ける。任意の句読点やすべてのkeywordが常にtoken精度になるとは限らず、式・文精度の場所はそのまま示す。生成した演算子を原文内の同じ文字の検索で結び付けない。

細かいtokenが優先されても、その親の式に記録した変換や寄与元を捨てないため、constructsを併記する。constructsの存在はその範囲が実行可能・ブレークポイント可能であることを示さない。

inline contextはleafのprimaryと分離して保持する。たとえば展開後の`2`自身は定義側の元の`2`へ戻り、そのmappingに付いたcontextから呼び出し側を辿る。実際にinline化しなかった関数に、存在しない展開フレームを作らない。これは仮想的なソース文脈で、消えたVM frameや変数を復元したものではない。

## 関連元と理由

origin.relatedはrelations表へのID。roleはcontribution/definition/callSite/argument/parameterUse/useSite。関係の役割を明示したパスだけが具体的roleを記録し、その他の寄与元はcontributionとして扱う。型を付けていない過去の範囲を、表示時の推測でcallSite等に変えない。

origin.reasonsはreasons表へのIDを記録順に持つ。reasonのフィールドは次の通り。

| フィールド | 内容 |
| --- | --- |
| code | 記録した変換・操作の安定した識別子 |
| operation | rewrite / rename / relate / synthesize / remove |
| before / after | 取得できた場合の変更直前/直後の構文型・数値リテラル表記。長い文字列payloadは長さだけを記録する |
| basis | 実際にその判断箇所で記録した成立条件のcode。記録がなければnull |
| facts | 判断時に得たkey/value。数値のbit表現・単位はkeyまたはtagで明示 |

**全reasonに完全な適用条件の証明があるわけではない。** 全パスの観測された由来操作と構文変化は保持し、定数式の評価、数値近似、共通定数引数の特殊化、未使用localと既知条件の削除では、判断時の値・比較・設定も記録する。個別根拠を記録していない操作はbasis=null/facts=[]のままで、最終コードや現在のエンジンから根拠を捏造しない。

定数畳み込みは、演算子・実オペランド・評価結果・採用した表現の値・サイズ比較・許容誤差を記録する。i64は10進文字列、f64はIEEE754 bit文字列とし、Luaの整数精度や-0/非有限数をJSONの数値へ暗黙変換しない。数値近似は誤差と適用された予算を別に保存し、完全同値と表示しない。

定数引数の特殊化は、調べたcall数、パラメーターindex、採用値、引数削除の判断を記録する。実引数と仮引数の使用位置は異なるroleを持つ。

copy、構文整理、名前付けも記録された操作であり、すべてが局所的な短縮を意味するわけではない。局所サイズ差の合計を全体の貢献度として扱わない。全探索の不採用候補の理由や「なぜ変換できなかったか」の全ログは本mapの対象外である。

## 採用候補と削除記録

理由・文脈・dispositionは候補と同じ寿命を持つ。trialのclone、変更、復元とともに戻り、採用されなかった候補の記録を最終mapへ漏らさない。削除/置換情報は生き残る所有nodeのsnapshotに保存し、最終出力にある構文から収集する。全コードが削除されて出力が空の場合も、選択されたrootの削除記録は保持する。

dispositionのremoveは、元の構文がその候補のその位置から外されたという記録である。コピーや変換結果が別の場所に残らないという主張ではない。`replacementSources`は元範囲への明示的な関連であり、実行PCや停止可能地点ではない。原文のすべての失われた文字について詳細な消失理由を保証するものでもない。

## 複数ファイルと標準map

minifyの由来は、リンク時にコピーした正確なbyte区間を通して各モジュールの原文へ合成する。複数ファイルへまたがる範囲は別々のspanにし、架空の連続範囲を作らない。リンク用の宣言・接続コードはSynthetic、もともとUnknownだった区間はUnknownを維持する。LifeBoatの開発専用区間の除去も、削除された部分へ元位置を戻さない。

標準mappingsには区間境界・token開始・行頭・EOFを出す。copy区間の内部anchorは正確に変換できるが、構文由来の区間内部を文字差分で補間しない。Synthetic/Unknownには未対応segmentを出して、直前の別の原文位置を引き継がせない。

標準mapだけを再生成する外部ツールがx_stormを保持する保証はない。拡張を失ったmapを理由表示付き成果物として扱わない。

## 検証とセキュリティ境界

validatorは生成コード、原文snapshot、標準mapと拡張のハッシュ・内部範囲・ID参照・copy内容を検証する。未参照の理由・関連元も検査する。再計算したハッシュが付いていても、標準mappings/namesと詳細範囲の投影が異なる場合は拒否する。本拡張では空でないsourceRootとindexed sectionsをサポートしない。canonical JSONの同じ内容なら、外側の整形・object property順が違っても読める。一方、同じ文字数でも内容が異なるコードには使えない。

SHA-256は組み違い・破損の検出用で、作成者の署名ではない。攻撃者が別のmapとhashをまとめて作ることを防ぐ認証機能ではない。producer情報も、未知の外部mapの出所を暗号学的に保証しない。

原文全文とpropertyの固定値等を含み得るため、mapは元ソースに相当する開発用データとして扱う。公開するLuaとは別に配布範囲を判断する。sourceNameをファイル読み込み命令として扱わず、説明の文字列をHTMLやLuaとして実行しない。

## 未提供のデバッグ機能

元の変数の保存先・生存期間・実行時の値、消えたframe、bytecode命令→生成列の対応、録画/逆実行、描画loopの特定反復→元命令の動的復元は別工程。行番号しか得られない実行環境から、mapだけで実際の列を確定しない。Playgroundの詳細UIはP5として管理する。


## 同じルールの複数適用

同じcodeでも値・根拠が異なるreasonを同一視しない。完全に等しい連続した操作のみ重複排除する。記録されたfactのkeyは理由内で一意かつ非空で、空のbasisや不正なnestedフィールドは拒否する。最後のtransformationラベルを過去の説明へ置き換えて表示しない。
