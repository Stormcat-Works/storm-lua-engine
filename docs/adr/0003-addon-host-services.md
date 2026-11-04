# ADR 0003: Separate profiles and explicit host services

Accepted: 2026-09-25. Implementation evidence is tracked separately.

## Decision

Addonはvm/specだけに依存する専用crateを置き、vehicleのI/O・screen・phase状態を共有しない。JSのcreateVehicle/createAddonも別型とし、生ABIでは1つのgenerational registryのenumでmodeを検査する。createVmという曖昧な入口の互換aliasは残さない。

実装の重複を避けるため、lossless owned values、引数付きcallback実行、構造化loggingをvmへ集約する。server関数はhostの同期closure、mapは描画命令列に従う同期provider、HTTPはrequest/reply queueとする。UI、world、通信client、スケジューラはhostに残す。

## Consequences

利用者は必要なworld APIを自由に提供できる。実装していないworldを固定値で模倣しない。serverの全ゲーム署名をengineが検証済みという保証にはならず、host adapterごとの検証が必要。

ホストの関数呼び出しはWASM経由では構造化codec・copyを伴う。普通のvehicle tickにはこの経路を追加せず、引き続き固定f32 I/Oを使う。非同期データは先に取得するか、request/replyで接続する。Promiseによる同期queryの偽装や同じmoduleへの再入はrejectする。

checkpointはowned dataのversion付き携帯形式で、全Lua heap、参照identity、ゲームXML、hostのworld状態を保存するものではない。可能な値を明示し、無理なsilent conversionをしない。

## Documentation

以下は決定時の文書配置方針である。利用者向け本文は後に移設され、現在の案内は[文書索引](../README.md)を参照する。

READMEを、利用するアプリケーションの開発者向け案内の起点とする。インストール・例・mode・ホスト責務・errors・メモリ寿命をdocs/guideへまとめ、contributor規約と分離する。Node例はpacked packageを入れた独立環境で実行する。
