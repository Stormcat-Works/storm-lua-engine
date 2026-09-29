# Storm Code統合用APIの検証（2026-09-30）

ブランチfeat/storm-code-integration。今回の対象は利用側のEngine移行、LBビルド、構造化Sim I/Oだけであり、最適化探索・Compiler性能・minify後ソースマップは対象外。

## 契約と実装

- inspectSourceは共有lexer/parserの実コメント・文span・lossless literal・動的式の区別を返す。UTF-8 byte offsetとUTF-16 editor offsetを混同しない。GUIの構文編集をEngineが解析し、Sim I/Oの意味は利用側が所有する。
- buildLifeboatは既存buildとは独立したinclude-onceビルド。LB section、EXACT/PATTERN、threshold、名前付き/入れ子区間を処理する。ゲームに残せない動的/別名requireは明示的エラー。通常buildのモジュール戻り値契約は変えていない。
- ゲーム向け出力で開発専用区間はminify有無に関わらず除去する。非短縮mapは既存形式、短縮後mapは提供しない。
- callDrawのmarginはEngine rasterに渡す開発用viewport。標準描画はmargin 0のまま、screen/mapの論理サイズを保持する。ホスト側でプリミティブ描画を再実装しない。

## 実行したゲート

fmt、xtask dependency/DAG/docs/fixture、全Native、clippy -D warnings、default check、broken intra-doc linksをエラーとするrustdoc、Runtime/CompilerのWASM再ビルド、SDK型/JS、実WASM63件、3ブラウザ（各731 RGBA +731 Lua描画・2 Worker）、隔離npm consumerを通過。

NativeのLB結合では循環include・戻り値破棄・ローカル変数隔離・開発区間除去・通常/PATTERN区間・ambient library・非exportableな動的requireを実VMで検証した。source inspectionは指数表記・i64・binary string・Unicode byte span・コメントと文字列の区別・動的値を確認した。

## 明示的な境界

LifeBoatのWindows外部Simulatorプロセスやsocket内部APIを移植するものではない。コード共有ライブラリの配置・取り込み、UIとデバッグ、課題帳はStorm Code側。上流MCライブラリ原文をこのEngineへ同梱していない。公開version、tag、npm公開、Playground配備は実行しない。
