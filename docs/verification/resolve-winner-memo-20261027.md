# 生成由来の解決における gap 勝者の永続メモ化（2026-10-27、未公開）

compile の固定費のうち、生成由来の解決（`GeneratedOrigins::from_arena`）
の gap ループの interning コストを減らした。生成 Lua・候補選択・schema1 の
マップ本体は維持する。Map 比 1.2 の目標は未達のままであり、この変更は
その達成を主張しない。

## 原因

gap ループは gap ごとに勝者の由来を interning する（ハッシュ＋クローン＋
未登録時の origins への push）。合成入力では gap 9,284 件に対して
interning 呼び出しも 9,284 件あり、直前勝者のみの従来キャッシュは
一度も当たらなかった。勝者はほぼ毎 gap 変わるためである。
一方、同じ勝者 emission が後の gap で再び勝つ回数は 1,920 件あり、
勝者の再出現は emission index でも格納ポインタでも同じ 1,920 件だった。

## 変更

- `crates/storm-lua-syntax/src/provenance.rs` のみ変更し、公開 API の
  シグネチャは変えていない。
- 直前勝者 1 件のキャッシュをやめ、勝者 emission index を鍵とする
  `Vec<Option<u32>>` の永続 memo に変えた。reason なし gap で勝者が
  初めて勝ったときに id を記録し、再出現時は interning を迂回して
  記録 id をそのまま使う。copied の計算・contexts の収集・
  main_origin_ids の記録は従来どおり行う。
- memo の正しさは次の理由による。勝者 emission の選択由来は、その
  emission のノードと site/token だけで決まり、解決中の arena スロットは
  不変である。reason 付き gap は memo の対象外とし、派生由来は従来どおり
  interning する。したがって記録 id は、その勝者を改めて interning した
  場合と exactly 一致する。

## 効果

- 合成入力（dense-normal、決定的な計数）で、gap の interning 呼び出しが
  9,284 件から 7,364 件へ減った。解決結果の件数は変更前後で同一
  （mappings 9,284・origins 10,725・contexts 0・constructs 6,883）。
- 解決を含む測定（31 反復中央値、旧・新バイナリの交互測定）では、
  ベースライン約 5.27〜5.91ms に対し変更版は約 4.74〜5.26ms で、
  負荷スパイクによる外れ値を除けば変更版が同等以上だった。
  共有マシンの負荷で中央値が揺れるため、時間差そのものは参考値とし、
  削減の根拠は上記の決定的な計数と同一の解決結果に置く。
- 実コーパス全件比較（4,155 ファイル×4 条件）では、成功／失敗・
  生成コード・Map 中核・正規化済み Map 全体のすべてが差分 0 件だった。

## 見送ったもの

- constructs ループの memo-first（主由来 memo ヒット分で由来の
  materialize を迂回する案）は、旧・新バイナリの交互測定で効果が
  ノイズ以下で実証できなかったため採用せず、変更していない。
- `validate_for_code` は勝者への本番安全網であり、単独の測定が
  約 0.16ms と小さいため、弱体化する変更は行わない。
