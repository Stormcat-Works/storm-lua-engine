# temporary-global-packing の変換コード共有化による確保削減（2026-10-29）

## 変更

temporary-global-packing パスの最終ループで、改名する Name ノード
ごとに `NodeArena::rewrite` を呼んでいたのを、パス全体で 1 個だけ作った
変換コード文字列を使い回す `rewrite_with_code` に変更した（実装は
minify クレートの temporary_globals.rs のみ）。

- `crates/storm-lua-minify/src/passes/temporary_globals.rs`：ループ前に
  `packing_code: Arc<str> = Arc::from("temporary-global-packing")` を 1 個
  作り、各改名ノードの `rewrite` を `rewrite_with_code` に変更した。
  `copy_name_from` による名前由来の転送は従来どおり。

## 等価性の根拠

`rewrite_with_code` は `rewrite` と同一の記録を行う API で、異なるのは
変換コード文字列の確保を呼出側で共有することだけである。コード文字列は
内容で比較・格納されるため、記録される由来・理由は同一である。値が
変わらない場合の早期復帰も両 API で同一である。既存の追跡付きテスト
（`packs_function_confined_globals`、追跡付きパース経由）が新経路で
通過し、生成コードの `__gs` へのパッキングを確認している。

## 測定（開発ホスト1、directpass 11 反復・中央値）

common.lua（発火入力、packed=7・slots=7）：

- 無追跡：6,917 回・515,260 B → 5,869 回・473,316 B（約 −1,048 回）
- 追跡：8,088 回・1,025,660 B → 7,036 回・983,508 B（約 −1,052 回）
- 追跡差：1,171 回 → 1,167 回（約 −4 回で実質不変）
- saved・コード長・ノード数は変更前後で同一。時間改善は主張しない。

inline.lua（非発火入力、packed=0）：無追跡・追跡とも変更前後で同一
（無追跡 2,596 回、追跡 2,597 回）。

## 切り分けの記録（本変更の位置付け）

診断変種（すべて復元済み）により、common.lua の追跡差 1,171 回の
内訳は次のように確定した：

- 解析段（resolve・走査・DSATUR 着色）の追跡上乗せは 0 回
  （rewrite ループ省略変種で追跡差が 0 回に落ちる）。
- `copy_name_from` の追跡上乗せは 0 回程度（rewrite のみ変種で
  追跡差は 1,177 回とベースラインとほぼ同一）。
- 残る追跡差は、改名ノードごとの由来スロット Arc ボックスと、
  触れたページごとのスロットページ COW（64 スロット／ページ）による
  現行表現のフロアである。本変更で共有化した変換コード文字列は
  無追跡モードでもノードごとに確保されていた（`finish_rewrite` が
  追跡有無の判定前にコード文字列を確保するため）ため、両モードで
  同数だけ減り、追跡差そのものは動かない。追跡差の打開には、
  スロット表への差分オーバーレイ等の表現レベルの変更が必要である。

## コーパス検証（Tier FULL）

- ホスト：開発ホスト1（ローカルコーパスのミラーを使用）
- manifest：SHA-256 ハッシュ abfec87a… の manifest で実行前に
  4,155/4,155 OK を確認
- base：分岐元・統合先端 4b50c7a8e、head：本変更を含む作業ツリー
- 検証バイナリの SHA-256 相違を確認（base 3ec6f594…、head 919cfe8e…）
- 4,155 ファイル×4 条件、欠落 0。両 TSV はバイト同一（SHA-256
  304e2da92b0cdb8fcf0e4d73f14f577b6c42bba393c7e6241099663c9f6e1e27）。
  全条件 identical=4,155、hard・core・expl・missing すべて 0。
  成功 4,127／失敗 28 は両側同一。
