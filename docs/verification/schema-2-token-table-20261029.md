# schema-2 トークン表の実装（2026-10-29、未公開）

字句形の Source Map におけるトークンあたりの重複記録（mappings と
origins に原文 span が2箇所）を解消し、schemaVersion を 1 から 2 へ
上げた。互換断絶はユーザー了承済み（次回リリース v0.4.0）で、1 は
reader が拒否する。2経路のうち字句勝者経路と simple_copy 経路の
両方にトークン表を実装し、一般形の map は従来レイアウトのまま
tokenNames／tokens を空配列で出す。

## 変更内容

- `crates/storm-lua-build/src/optimized_source_map.rs`：
  SCHEMA_VERSION を 2 へ変更し、MapExtension に tokenNames
  （初出順の名前綴りプール）と tokens（トークンごとの
  [generatedStart, sourceStart, sourceEnd, nameId]、非名前は -1）
  を追加。validate() はワイヤ形のまま integrity を照合した後、
  tokens を mappings＋origins へ展開して返す。展開時の検査は
  生成順・非重複・範囲内・名前綴りと原文 slice の一致・単一
  source。未知ギャップは cursor 走査で未知 mapping として復元する。
- `crates/storm-lua-build/src/canonical_map.rs`：字句 copy
  エンコーダと simple_copy エンコーダをトークン表化。canonical
  writer のキー順は辞書順を維持し、新キーは sources の後、
  tokenNames → tokens の順に置く。integrity の計算方法は不変
  （integrity 自身を空文字列にした canonical JSON 全体の SHA-256）。
- simple_copy のトークン表は適用条件を厳格化した。単一 source、
  全 origin が1件以上の mapping から参照されていること、初出順が
  origin 順と一致すること、(primary, 名前, 精度) が一意であること、
  名前があるのは name 精度の場合に限ること、mapping が生成コード
  全体を連続して覆うこと。満たさない場合は schema 2 のまま従来
  レイアウトで出す。トークン0件の字句形（コメントのみ等）も
  従来レイアウトで出す。なぜなら空のトークン表では生成コード全体
  に広がる未知ギャップを運べないためである。
- `packages/lua-engine/src/optimization-map-types.ts`：OptimizationMap
  の schemaVersion を 2 へ変更し、tokenNames／tokens を追加。
  wasm テストの schemaVersion 期待値も 2 へ更新した。
- `docs/specs/optimization-map-extension.md`：schema 2 への改題と
  トークン表の節を追加した。compiler-sdk.md に map 関連節は
  なかったため併記はしていない。
- 回帰テスト1件を追加した。トークン表のワイヤ形（schemaVersion 2、
  mappings／origins 空、tokens 非空）と、validate() の展開結果が
  materialized 由来と一致すること、schema 1 のワイヤが拒否される
  ことを確認する。

## 検証（Tier FULL）

- 対象クレート（storm-lua-build 127 件）およびワークスペース全件の
  テストが成功した。xtask check・fmt を通過し、build クレートの
  clippy 警告は 0 件である。
- コーパス検証：base は分岐元 d44f1eaf、head は本変更込みの
  未コミット内容。開発ホスト1 のローカル複製を用い、実行前に
  SHA-256 manifest 照合を 4,155/4,155 OK（manifest のハッシュは
  abfec87a…）で確認した。4,155 件×4 条件で欠落 0 行、両 TSV は
  バイト同一である。成功 4,127 件／失敗 28 件、生成コード、Map
  中核、エラー文ハッシュは全条件で完全一致した。比較は schema
  対応の正規化（tokens を mappings＋origins へ展開し、schemaVersion・
  producer・integrity と、展開後に情報を持たない tokenNames／tokens
  キーを除去）で行った。なお正規化の初版では一般 map に付く空の
  tokenNames／tokens キーの除去が漏れ、全 ok ファイルが説明系のみ
  の差分に見えたため、正規化を修正して再実行し、バイト同一を
  確認した。検証バイナリの SHA-256 は base が
  c8b1de32c61177518a4b09c91caed8633f56fc86cc4a84850dbe1fb7aa808b5e、
  head が 8ab0e5f2a00dbd8487eb5ae956cd5f82f5dbae6c2052fe28e37f74d454453352
  である。両バイナリは生成した全 map を自前の validate() でも
  照合しており、validate 失敗は 0 件である。
- コーパス全体の map 容量（default-map／std-map は同値）：base
  1,136,655,511B に対し head 1,091,858,002B（-44,797,509B、
  -3.94%）。トークン表を使ったのは 5 件（字句勝者、トークン
  177,259 件）で、その削減は -44,908,803B である。simple_copy 経路
  のトークン表はコーパス中で1件も発火しなかった。トークン表を
  使わない map は空の tokenNames／tokens キー（27B／件）だけ増え、
  合計 +111,294B である。したがって schema-2 のコーパス全体への
  容量効果は約 -3.9% であり、字句勝者そのものの map は約9割縮む
  一方、一般経路の詳細記録には触れない。
- 計測専用マシン 開発ホスト3 での新旧交互測定（31 反復の中央値
  ×3 ペア、開始時 loadavg 0.09、MEASURE.lock 経由）：dense-normal
  は nomap 約226ms→約227ms、map 約284ms→約287ms で Map 比は
  約1.26 のまま不変。dense-lexical は nomap 約2.05ms→約2.05ms、
  map 約15.18ms→約6.14ms（約 -59.6%）で、Map 比は約7.4→約3.0
  へ改善した。生成コード長は両入力とも新旧で同一である。
