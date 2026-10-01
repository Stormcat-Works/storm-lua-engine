# v0.3.0 Source Map 最小限性能最適化計画

2026-10-01。v0.3.0公開前に、理由付き最適化後Source Mapの明白なオーバーヘッドだけを1ラウンド削減する。**追加の性能追求はv0.3.1以降へ送る。**

## 前提

最適化後Source Mapは明示opt-inである。SDKの `sourceMap:true`、CLIの `--source-map` などを指定した場合だけ詳細由来・理由・Source Map v3 + `x_storm` を生成する。通常の `sourceMap:false` は既存の短縮結果と通常性能を維持する。

現行v0.3.0開発版はP0〜P5まで完了し、全67最適化パスの由来伝播、標準Source Map v3、`x_storm schemaVersion:1`、producer/version/revision、原文・生成物指紋、Playground/CLI/実VM接続まで検証済みである。この性能作業で精度や契約を弱めない。

現行の代表計測では、約13万文字の描画例でmapなし約3秒、理由付きmapあり約6.3〜6.5秒、mapは約8.23MB。これは公開前の改善対象だが、mapありをmapなしと同等にすることや、4秒以内を保証することは完成条件にしない。

## 目標

1. `sourceMap:false` の生成Lua・候補選択・通常性能を悪化させない。
2. `sourceMap:true` でも生成Lua、由来の精度、理由、関連元、inline context、除去/置換、schema 1の意味を変えない。
3. profileで上位3〜5個の明白なコスト源だけを修正する。
4. 約13万文字の代表例で、mapあり/mapなし比を可能なら1.5〜1.7倍程度まで下げる。**未達でもv0.3.0公開の阻害条件にはしない。**
5. mapの生JSONサイズも、情報を削らずに下げられる範囲で削減する。
6. 追加の大規模最適化・形式刷新・圧縮バイナリ形式はv0.3.1以降へ送る。

## 今回調べるコスト源

### A. 候補探索中の由来メタデータ複製

最終候補が決まる前にAST/NodeArena/由来テーブルをcloneする箇所をprofileする。重い場合は、既存の `Arc` 共有、copy-on-write、ID参照を活用し、**候補の意味論を変えず**複製量を減らす。

### B. 最終map生成時の重複排除

Origin / Relation / Reason / InlineContext / SourceDisposition の同値データを、意味を変えずに一度だけ持つ。既にpool済みの項目は再度別表現へコピーしない。理由の `code` が同じでも判断値・basis・factsが異なるものは統合しない。

### C. 座標変換の重複

UTF-8 byte → line/UTF-16列、source index、token anchor、source snapshot indexなどを標準Source Map v3と `x_storm` 生成で重複計算していないか測る。共有できる計算だけを共有する。

### D. JSON構築の冗長性

最終JSONで同じrelation/reason/context/spanが過剰に展開されていないか確認する。schema 1の公開意味論を変えず、既存のindex参照・interningを活用する。情報削除、Groupへの粗化、理由文の省略は禁止。

## 明示的にやらないこと

- Unknownを増やす。
- Token/Name/Expression/Statementの精度をGroupへ落とす。
- 関連元・inline context・最適化理由・除去記録を削る。
- Source Map v3をやめる、または`x_storm schemaVersion:1`を非互換変更する。
- 最適化パスを無効化して速く見せる。
- target探索やwinner選択を変更する。
- 壁時計時間で探索を打ち切る。
- gzip等の転送圧縮だけで生JSON/parse/memory問題を解決した扱いにする。
- 元変数値・仮想frame・命令PC等の別デバッグ機能を今回抱き合わせる。

## 測定方法

同じ固定入力・同じ設定・同じrevisionのrelease buildで、初期化・ファイルI/Oを除いて複数回測定する。mapなしとmapありを交互に実行し、中央値を主値とする。

最低限、次を記録する。

- 約2万文字の制御例: 全探索 / 8192目標。
- 約13万文字の描画例: 全探索 / 8192未達。
- mapなしcompile時間。
- mapありcompile時間。
- `validateSourceMap`時間。
- mapの生バイト数。
- 生成Lua SHA-256。
- mapのvalidation成功。
- 必要ならprofileの関数別上位時間/割り当て。

Node/WASMとNativeで意味を混同しない。ブラウザーやStormworks実ゲームの性能保証には使わない。

## 回帰ゲート

変更後に少なくとも以下を確認する。

- 代表30入力・240設定でmap ON/OFFの生成Lua一致。
- target未達時と最大探索の生成Lua・由来一致。
- 全67最適化パスの由来契約。
- Native workspace / clippy / rustdoc / architecture。
- SDK型・JS、実WASM、compiler worker。
- Chromium / Firefox / WebKit。
- Playground P5の双方向選択・理由・実runtime接続。
- 梱包済みnpm tarballの独立consumer。
- `sourceMap:false` の性能回帰確認。

## 打ち切り基準

profile上位の明白な無駄を3〜5件処理した時点で再計測する。

- 大きく改善した場合: v0.3.0へ採用し、before/afterを検証記録へ保存する。
- 改善が小さい、または追加修正が複雑化する場合: その時点でv0.3.0作業を打ち切り、残りを**v0.3.1以降の性能課題**としてTASKS/STATUSへ残す。
- 正確性、schema互換、通常minify性能を犠牲にする案は採用しない。

## 完了状態

この計画を保存後、profile → 上位コスト修正 → 回帰 → 再計測 → 文書更新を行う。v0.3.0のpublish/tag/releaseブランチ更新・本番Playground配備は別の明示工程であり、この作業では実施しない。
