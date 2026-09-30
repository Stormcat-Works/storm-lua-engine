# PlaygroundのSource Map検査と実行接続

2026-10-01。P5と後続の利用検証を完了する。追加のコンパイラ性能最適化は今回の対象外。公開済みv0.2.1を変更せず、v0.3.0の開発ブランチで実施する。リリース・本番配備は既存計画の明示工程と分ける。

## 完了条件

- 元snapshotと生成Luaを並べ、文字選択・キーボード操作から双方向に辿れる。複数候補は一覧から選び、単一の由来へ暗黙に縮約しない。
- Source/Derived/Synthetic/Unknown、名前/Token/式/文/Group、copy、型付き関連元、inline context、親construct、理由/観測事実、除去/置換記録を表示する。記録のない適用根拠を補完しない。
- 生成コードとマップをSDKのvalidateSourceMapで検証してから使う。入力編集後もsnapshotはそのまま保持し、編集入力と過去成果物を区別する。インポート失敗時は既存入力を保持する。
- コンパイルと実行は明示操作。正確なcode/mapの組で生成物を独立したWorker内VMへロードし、chunk名・世代を固定する。tick/draw/step/continue、stack・ログ・エラーを実SDKから取得する。
- runtimeが生成行しか返さなければ列を捏造せず、その行の候補を示す。元の行から生成行breakpoint候補を示すが、停止の保証はしない。実際のsuspendedだけを停止と表示する。削除された行に近隣行を割り当てない。
- 大きなmapを含む状態はIndexedDBの単一workspace recordに保存し、リロードしても入力・成果物・選択・結果を維持する。VM継続は保存しない。旧localStorage状態は自動変換せず救出可能なエラーとして表示する。
- 入力専用project JSONと、成果物・選択も含むversion付きworkspace JSONを明示的に区別する。CLI/Webで同じschemaを読み、原文やmap中の文字列をHTML/Luaとして実行しない。
- CLIのmap検査、SDK独立consumer、Native/WASM回帰、3ブラウザーの操作・保存・持ち運び・失敗・中断・mobile表示を検証し、仕様・使用例・互換性棚卸しを保存する。

## 実装境界

位置情報の作成と検証はSDKのRust実装が正本。Playground共通層は検証済みデータを検索し、UTF-8 byteとtextareaのUTF-16選択範囲を変換するだけで、Lua解析・最適化の意味論を再実装しない。実行ホストもCLIとWorkerで共有する。元変数値、消えたstack frame、命令PC、生成loopの特定反復の復元、逆実行は本版の完了条件に追加しない。

UIは既存のPlaygroundに検査パネルを追加する。IDE、別コンパイラ、外部ネットワーク実行、新しいUIフレームワークは導入しない。各成果物に含まれる原文全文・固定property値は機密情報になり得るため、export時に表示する。
