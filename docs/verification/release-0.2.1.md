# Storm Lua Engine v0.2.1 リリース確認

2026-09-30。状態: リリース準備中。npm/GitHub公開・本番配備の成功はまだ主張しない。

## 範囲

既存のP0/P1（共通のtarget探索・詳細な非短縮map）と、ログ位置、Vehicleホスト操作、構造化source inspection、LifeBoatビルドを含む。最適化後minify mapはv0.3.0へ分離する。独立したdevelopブランチの描画変更は今回の候補に含めない。

## 必須の同一出力条件

同じ入力・環境・numeric mode・pass設定・property・出力形式に対して、target未達なら`searchMode:exhaustive`かつtargetなしの生成Luaとbyte-identicalであること。文字数はUTF-16単位で一致する。target探索ではfast/beamによる候補切り捨てを行わない。早期達成時とtargetなしの明示fastは最大短縮と同一である必要はない。

同一のコアと候補集合を利用し、評価済みbatchを継続contextへ保存する。Workerの完了順によらずcanonical orderでtie-breakする。wall-clockによる打ち切りは追加しない。全探索の意味論・短縮品質を変更しない。

## 公開前チェック

検証実行後に結果と対象revisionを記録する。公開済みタグ/npm版の差し替えは行わない。

## 互換性

Source Map v3とTS成果物型は維持する。低レベルRustのLinkedRange初期化にはbyte範囲が必要。Workerのopaque contextは同じcompiler版の一時データとする。savedata、描画命令、Composite I/Oの形式は変更しない。

## 認証

GitHub CLIの認証を確認済み。npmの既存認証は401 Unauthorizedを返したため再認証が必要。認証情報そのものは記録しない。
