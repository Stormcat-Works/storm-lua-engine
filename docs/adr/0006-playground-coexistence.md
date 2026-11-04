# ADR 0006: Storm Minとの併存とStorm Lua Engine: Playground

日付: 2026-09-26。状態: **採用。** 決定時点では設計反映のみで、Playground実装・Addon Lab削除・本番配備は未完了だった。現在の実装・公開状況は[STATUS](../../STATUS.md)を参照する。

## 背景

共通の言語処理をEngineへ移す決定と、利用者向けフロントエンドを一つへ集約する決定は別である。EngineへCLI/Webを追加する提案は、Storm MinのCLI/Webを移設・廃止する提案ではなかった。

[ADR 0005](0005-compiler-sdk-integration.md)の言語処理移管・責務分離・既定無効パス解消の判断は維持する。同ADRのフロントエンド集約・旧配布終了に関する保留事項は、本ADRの併存方針で置き換える。過去の判断記録そのものは書き換えない。

## 決定

Storm Minは現在の短縮用CLI/Webを維持し、Engineの公開SDKを利用する。Engineには **Storm Lua Engine: Playground** という別のCLI/Webを追加する。SDKの全公開機能を実際に試し、できることと結果・失敗を確認する用途に限定する。それを超える独立アプリ機能は求めない。

配置は`app/cli/`と`app/web/`とし、makkii.jpでLua Engine / Playgroundを公開するCloudflare Workerの設定・必要な配信処理も`app/`で管理する。SDKのcrate/packageとビルドツールは既存の所有範囲を維持し、ルート直下へアプリ専用の配置を増やさない。

`examples/addon-lab`はSDK接続・確認に必要な部分をPlaygroundへ移行した後、独立アプリとして削除する。港、船、仮物理、3Dワールドをそのまま移すことは要件ではない。SDKを試す最小の明示的なテストホストと関連テストを維持する。

詳細な機能群、移行範囲、Workerの区別、完了条件は[Playground設計](../design/playground.md)を正本とする。正式な実行ファイル名・Web route・公開版は実装/公開時に確定し、名称・併存・`app/`配置の決定を再び保留に戻さない。

## 帰結

同じSDKのminifyをStorm MinとPlaygroundの双方から呼び出せる。共通のアルゴリズムや汎用接続処理はSDKに一つだけ置くが、目的の異なるUI/CLIの併存を二重実装の問題と扱わない。

SDKはどちらのアプリにも依存しない。Playgroundも公開APIの通常の利用者とし、内部実装や私有fixtureへアクセスする特権経路を作らない。完全なIDE、共同編集、クラウド保存、ゲームワールドの再実装を追加しない。

現行のSDK移管・検証済みコードや、公開済み0.1.0の契約は本ADRだけでは変更しない。Addon Labの移行先・テスト・CI・配布参照を揃えてから旧アプリを撤去する。公開、push、deployを自動的に実行する決定ではない。
