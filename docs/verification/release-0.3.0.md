# Storm Lua Engine v0.3.0 公開記録

2026-10-02。管理者の公開指示に基づいてv0.3.0のリリースを準備している。P0〜P5と限定性能改善は完了。現時点は公開前の記録であり、npm/GitHub/本番公開の成功を先取りしない。公開後に同じファイルへ実行結果を追記する。

## 公開対象

最適化後Source Map v3＋x_storm schema1、全67パスの由来、理由・関連元・copy・inline文脈・除去/置換、Rust/WASM/TSとWorker、通常/LifeBoatの元ファイル合成、Playground/CLIの静的・実runtime接続を含む。追加の性能改善はv0.3.1以降。最適化結果やマップ精度をさらに変更するリリース準備は行わない。

## 非互換性と利用側

- npmのcompiler/worker型と生成WASMを0.3.0で揃える。Source Map本体version=3、拡張schemaVersion=1、producer.versionは別々の識別子。
- 低レベルRust Ast.nodesのNodeArena、opaque candidateのJSON/binary形式は同一compiler版を前提とする。
- PlaygroundのIndexedDB state version2とworkspace version1を使用する。旧localStorageを黙って自動変換せず、救出と明示importを案内する。入力project JSON v1は維持する。
- 描画命令ABI、Composite I/O、Addon savedataと既定のgame/extendedは維持する。
- SDKは生成と検証を提供する。ホストの画面、byte/UTF-16座標変換、元位置の候補表示、VM行との対応は利用側が接続する。列・元変数値・消えたframeを自動復元しない。

## 手順

mainのv0.2.1公開後の文書変更を保存済みの開発ブランチへ統合し、同じ公開候補commitでNative/WASM/SDK/3ブラウザー/パッケージconsumerを再検証する。成功したcommitにタグを付け、検査済みtarball/静的ZIP/SHA256SUMS/release-verification.jsonを添付したGitHub Releaseを公開する。npm Trusted Publishingとregistryからの独立導入を確認し、releaseブランチによるPlaygroundの自動配備と公開ガイドを同じ版に揃える。

[Source Map仕様](../specs/optimization-map-extension.md) / [最小性能改善](source-map-performance-20261002.md) / [P5の検証](p5-final-selection-20261001.md)。
