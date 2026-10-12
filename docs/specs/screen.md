# 画面描画と同梱フォントの契約

## 仕様の所有者

**この仕様、採用済みの入出力ケース、実装を所有するのはstorm-lua-engineです。** 利用アプリの実装を実行時・ビルド時・CIの正解判定へ逆参照しません。[画面契約](../../fixtures/screen/README.md)は通常のcheckoutだけで利用できます。

[743ケースのRGBA](../../fixtures/screen/cases-v1.json)は、レビューで採用した固定値です。直接ラスタライズと実Luaコード実行の両経路を同じ期待値で検証します。期待値を候補実装から生成し直して一致させる仕組みはありません。規則とケースが矛盾した場合は原因を調べ、別の観察または検証可能な導出を示してから契約を変更します。

この比較は契約への回帰検証です。今回、新しい実ゲーム測定を行ったことや、ゲーム全体・全版の動作を保証することとは異なります。検証条件は[記録](../verification/repository.md)、更新手順は[テスト設計](../design/testing-performance.md)を参照してください。

## RGBAとフレームの寿命

各成分は`floor((source*alpha + destination*(255-alpha) + 127)/255)`。A成分にも同じ係数を用いるので、正規化すると`Aout=As*As+Ad*(1-As)`です。`rgb<=alpha`は不変条件ではありません。

透明黒へ`[255,0,0,128]`を1回描くと`[128,0,0,64]`、2回で`[192,0,0,96]`になります。alpha0は既存画素を保持し、255は置換します。`drawClear`はblendではなく、現在色のRGBAによる画面全体の置換です。

`begin_frame`は透明黒へ消去し、現在色を白`[255,255,255,255]`へ戻します。Nativeの`submit`と`draw_batch`はフレームを暗黙に開始しません。WASMの`render`とruntimeの`draw`は新しいフレームを開始します。

raw形式は`GameRgba8`です。Canvas内部のpremultiplicationとは別の値です。`CanvasPresenter`はraw RGBAをImageDataへコピーして表示し、独自のalpha補正・gamma変換を加えません。表示先からのreadbackはブラウザ内部変換を含むため、rawデータの比較とは分けます。

## 図形と文字

| 対象 | 契約 |
|---|---|
| 座標 | 左上原点、Y下向き。一律floorや一律f32化をしない |
| RectF | 1/256 snap（下記）。Xはceil境界、Yはfloor境界の半開区間。負寸法と空区間もケースで規定 |
| Line | snap済み端点でdiamond-exit。終端を除外し、major-axisと方向に対応するcorner ownershipを保持 |
| Rect/Triangleの輪郭 | 各辺をlineと同じ規則で処理。重複画素を勝手に除去しない |
| TriangleF | snap済みpolygon、Y sample offset1、境界符号-1 |
| Circle | segments=min(16,max(8,floor(abs(radius)/2)))。頂点位置でf32へ丸める |
| CircleF | 同じpolygon、Y sample offset0、境界符号-1 |
| Text | 4×5glyph、文字送り5、行送り6、位置はfloor |
| TextBox | 文字数はUTF-16コード単位。空白・折返し・配置・丸め順序は採用ケースで規定 |

座標は1/256px単位へsnapします。格子点どうしのちょうど中間（k+1/512）に乗った値は、上下の格子点までの距離が等しく、スクリプト座標上の丸め規則では丸める向きが決まりません。f32で投影（半画素オフセットを含む）、viewportの乗算と加算を行い、最近接偶数丸めで固定小数点化した結果に従います。xは画面幅、yは画面高さを使うため、同じ値でも画面の大きさで向きが変わります。f32へ収まらない座標だけは、JavaScriptのMath.round相当で扱います。

この2点（格子のちょうど中間での丸めの向きとCircleFの境界符号）は、Stormworks v1.15.23のモニターを撮影した画素（Windows・RTX 4070Ti）に合わせています。`ingame-`で始まるケースは撮影したページの命令列をそのまま使い、白で描いた画素が撮影結果と一致します。Apple M5ではxの中間値がすべて切り上げになり、yの中間値も別の向きになるため、ちょうど中間に乗った頂点だけは1画素ずれることがあります。巨大座標は処理量を制限したclip、非有限座標は描画なしとして扱います。サイズ・バイナリ命令・不正UTF-8は明示的なエラーです。Lua文字列自体はbytesのまま保持し、描画できない文字列を黙って置換しません。

## フォントとリソース

[同梱フォント](../../data/fonts/README.md)はprintable ASCII95文字とdegree記号です。小文字は大文字相当のグリフ、未知文字は枠状の代替グリフを使います。日本語フォント一式を提供するものではありません。

フォントの正本はJSON数値表です。`cargo xtask generate`はその表からRust定数だけを再生成し、外部データを取得しません。描画構成要素に含まれる第三者由来部分の権利表示は[MIT許諾原文](../../licenses/screen-components-MIT.txt)に保持します。

framebuffer上限16MiB、辺長1〜4096。バイナリbatch上限8MiB、65536命令、text合計1MiBです。Lua命令予算とrasterの処理量制限は別です。

デバッグ停止時は未完了の命令列を保持します。resumeは追加分だけを反映し、既描画部分を二重にblendしません。完成・停止・失敗は戻り値の状態で区別します。

## ホスト地図

`drawMap`は明示的なmap providerへ接続します。図形の契約を理由にゲームの地形データを同梱したり、provider不在を仮の画像で成功扱いしたりしません。ホストが返す地図の内容はホストの責任です。[ホストサービス](../guide/host-services.md)を参照してください。
