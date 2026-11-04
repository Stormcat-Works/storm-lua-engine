# 同梱ビットマップフォント

フォントの数値表の基準ファイルは[tiny-4x5.json](tiny-4x5.json)です。4×5px、文字送り5、行送り6、ASCII32〜126の95文字とdegree記号U+00B0を含みます。小文字は対応する大文字の形です。未収録の日本語グリフが存在するとは扱いません。

`cargo xtask generate`または`python3 tools/generate-font.py`で[定数表](../../crates/storm-screen-raster/src/font/data.rs)を生成します。生成コメントも日本語です。JSONと生成表の一致は検査しますが、ゲーム資産の読込・ネットワーク取得・外部フォント指定は通常のビルドと実行に必要ありません。

JSONのSHA256は[画面契約manifest](../../fixtures/screen/manifest.json)に含めます。生成ツールは数値表を新しい根拠で置き換えるものではありません。形やmetricsの変更は描画の契約変更としてレビューします。

権利者とMIT許諾文は[描画構成要素の表示](../../licenses/screen-components-MIT.txt)に保持します。これはゲーム資産全体についての権利許諾を主張するものではありません。
