# 定数畳み込みの由来付けを源からの derive に変更し追跡コストを削減する（2026-10-27、未公開）

定数畳み込み（`constant-folding`）とその後段の制御フロー単純化で、
走査の外側にあった由来の capture＋finish_rewrite を、走査対象の不変の
源に対する `derive_from` に置き換えた。生成 Lua・候補選択・schema1 の
マップ本体は維持する。Map 比 1.2 の目標は未達のままであり、この変更は
その達成を主張しない。

## 原因

`fold_node` と `simplify_node` は、源 AST を不変に保ちながら複製先
AST 上で全ノードを走査する分離型走査である。従来は各ノードについて
走査前に由来を capture（ノード値のクローンを伴う）し、走査後に
finish_rewrite で由来を付け直していた。変更されるノードは少数で、
大半のノードでは走査後の finish_rewrite が共有スロットの復元で終わる
だけだったため、未変更ノードに対する capture の値クローンが毎ノードの
追跡コストになっていた。

## 変更

- `crates/storm-lua-minify/src/passes/literal_folding.rs` のみ変更し、
  `fold_node` と `simplify_node` の外側の capture＋finish_rewrite を、
  源に対する `derive_from`（理由は従来どおり `constant-folding`／
  `control-flow-simplification`）に置き換えた。
- 内側走査は各ノードを既に rewrite しており、値が変わらないノードでは
  共有スロットが復元されているため、未変更ノードは `derive_from` の
  共有スロット高速経路で値クローンなしに処理される。値が変わった
  ノードは従来と同じ capture＋finish_rewrite 経路に落ち、理由も同じ
  ものが記録される。
- 源が走査中に不変であることが前提のため、単一 arena で走査と書換えを
  行う `fold_exact_safe_node` は対象外とし、従来の capture を残した。
- 公開 API のシグネチャは変えていない。

## 測定

合成入力は dense-normal（46,022 バイト）。VM 2 コア、測定時 load
average は 0.9〜2.7 程度。計数アロケータ付きハーネスで各条件 11 反復の
中央値を測った。

| 測定 | 変更前 | 変更後 |
| --- | ---: | ---: |
| constant-folding・追跡なし確保回数 | 39,834 回 | 39,834 回（不変） |
| constant-folding・追跡あり確保回数 | 58,798 回 | 53,992 回 |
| constant-folding・追跡差（確保回数） | 18,964 回 | 14,158 回 |
| constant-folding・追跡差（確保バイト数） | 960,848 B | 937,784 B |
| fold_expressions・追跡差（確保回数） | 15,600 回 | 13,197 回 |
| simplify_control_flow・追跡差（確保回数） | 3,844 回 | 961 回 |

追跡なし側が変更前後で完全に同一であることから、削減が追跡経路だけの
ものであることを確認した。時間差は 3.49ms から 3.25ms で、時間改善は
主張しない。

## 検証

- 対象 3 クレート（syntax 102 件・minify 441 件・build 126 件）全成功。
  ワークスペース全件（50 スイート超）も失敗 0。`cargo xtask check`・
  `cargo fmt --check`・`cargo clippy --workspace --all-targets`（警告 0）を
  通過。
- 回帰テスト `tracked_folding_keeps_code_and_generated_origins` を追加し、
  追跡付き fold のコードが非追跡 fold のコードと一致し、生成 origin の
  検証と unknown 0 バイトを確認した。
- 実コーパス全件比較（標準手順、base は分岐元、4,155 ファイル×4条件、
  欠落 0 行）：成功／失敗（成功 4,127／失敗 28 ファイル）・生成コード・
  Map 中核・正規化済み Map 全体まで、両 TSV がバイト同一だった。
  すなわち説明系の差分すら 0 件で、由来付けの内容が完全に維持された。
