# tracked 印字の emission 保持の arena 化による印字コスト削減（2026-10-27、未公開）

tracked 印字（`output_with_positions`）で、Fragment 合成時に emission
（生成範囲）全件をシフト・移動していた内部表現を、Printer 所有の
arena と軽量セグメントへ変更した。生成 Lua・候補選択・schema1 の
マップ本体は維持する。Map 比 1.2 の目標は未達のままであり、この変更は
その達成を主張しない。

## 原因

`Fragment::append` は、追加される側の emission 全件に対して
`start`／`end` へのオフセット加算と 128 バイト級の構造体移動を行って
いた。emission は合成の段数だけ移動するため、10,725 件の emission を
持つ合成入力では 1 回の tracked 印字あたり約 19MB の確保バイト数が
積み上がっていた。plain 印字（0.43ms 程度）に対する tracked 印字の超過
（約 1.57ms）はこの機構によるもので、record を無効化すると tracked
印字は plain 印字と同程度まで軽くなることをスクラッチ変種で確認した。

## 変更

- `crates/storm-lua-syntax/src/print.rs` のみ変更し、公開型
  `PrintedSource`／`NodeEmission` は変えていない。
- emission 本体は Printer 所有の arena（`RefCell<Vec<NodeEmission>>`）
  に 1 回だけ記録し、Fragment は (arena index, fragment 相対 offset)
  のセグメントだけを持つ。合成時はセグメントの offset 加算と
  セグメント記述子の移動だけを行い、個々の emission のシフトは
  最終印字の flatten で一度だけ行う。
- flatten の順序は従来の eager な合成順と同一になる設計である。
  セグメントは移動のみで複製されず、arena の各項目はちょうど 1 つの
  セグメントから参照される。`record`／`record_syntax` 以外の印字経路や
  テキスト合成の順序は変えていない。
- 無追跡の印字ではセグメント列は空のままで、従来どおり追加の確保を
  行わない（既存テスト `untracked_fragments_allocate_no_emission_vectors`
  の趣旨を新フィールドで維持した）。

## 測定

合成入力 dense-normal（46,022 バイト）。計数アロケータ付きハーネスで
各条件 31 反復の中央値を測った。開発ホスト2 12 コアで他ワーカーと
共有のため、時間は参考値とし、確定的な確保バイト数を主に見る。
tracked 印字の emissions は 10,725 件、生成コードは 42,339 バイトで
変更前後とも同一。

| 測定 | 変更前 | 変更後 |
| --- | ---: | ---: |
| tracked 印字・時間（参考値） | 約 1.57ms | 約 1.41ms |
| tracked 印字・確保回数 | 28,184 回 | 28,198 回（ほぼ不変） |
| tracked 印字・確保バイト数 | 19,019,272 B | 8,062,904 B（約 −58%） |
| plain 印字（追跡 AST 上） | 0.431ms・9,607 回・395,560 B | 0.437ms・9,607 回・395,560 B（不変） |

なお、同じ方針の別実装（Fragment 内チャンク方式）も試したが、時間・
確保回数・確保バイト数のいずれも arena 方式と大差なく、arena 方式の
方が確保回数が少なかったため arena 方式を採用した。

## 同一性

- 合成入力 dense-normal の tracked 印字ダンプ（生成コードの SHA-256 と
  emission 全 10,725 行）は変更前後で完全一致した。
- 実コーパス検証（StormMin real-world corpus、4,155 ファイル×4条件）では、
  成功／失敗（成功 4,127／失敗 28 ファイル）・生成コード・Map 中核・
  正規化済み Map 全体のすべてが完全一致し、差分は 0 件だった。
  検証バイナリの SHA-256 が両側で異なることも確認済み。
