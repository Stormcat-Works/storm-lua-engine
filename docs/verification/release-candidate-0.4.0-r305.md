# リリース候補 0.4.0 検証記録（第305ラウンド: ブロッカー解消と規約修復）

- 対象コード先端: `42fb8477d1643d753e859b4f8baa3b4dcca9526a`
  - `1c2bc41` feat(vm): mlua を backend-mlua 背後の任意依存にし、ロード合法性判定を差し替え可能にする（sha2 asm 解除を含む）
  - `42fb847` feat(xtask): check_repository.py に推移的 C 依存検査とフィーチャ許可制を追加する
- 位置づけ: 第304ラウンドで残った候補 CI の失敗2件（wasm の build-compiler、Windows の sha2-asm）を解消し、候補を作り直すための変更と、その全表ゲートの記録。タグ・GitHub Release・npm 公開・配備は本ラウンドの範囲外（工程4以降は別途承認）。

## 実装1: mlua の任意依存化（方針(a)）

- `storm-lua-vm` の mlua を任意依存にし、`backend-mlua` を「型の再公開ゲート」から「依存ゲート」へ改めた。有効化するのは実行バックエンドを必要とする consumer（addon、microcontroller、wasm runtime、conformance、検証ハーネス）。コンパイラ専用 consumer（build、minify、compiler-wasm）は有効化しない。`debug` フィーチャは backend-mlua を含意する。実行系モジュール（runner、sandbox、bindings 等）は同フィーチャでゲートし、フィーチャ無効時の vm が提供するのは方言変換とロード合法性のみ。
- ロード合法性判定（`check_load_legality`）は差し替え可能にした。参照判定は従来どおり vendored PUC の parse-only 判定で、backend-mlua 有効時のみ存在する。native の通常経路は検証ハーネスを含め backend-mlua を有効化するため、判定結果は従来と完全同一（Tier FULL のステータス行を含む。下記のバイト同一がその証跡）。
- フィーチャ無効時の代替判定の意味（設計として明記）: 自前パーサ（storm-lua-syntax）の受理に加え、PUC ロード規則の専用チェッカで次を検査する — ラベル解決と同一ブロック内の重複、goto のスコープ跳躍（ブロック最終ラベル例外を含む）、有効局所変数の 200 上限（パラメータ・ループ変数を含む）、ループ外 break、ブロック最終文でない return、非可変長関数の `...`。参照判定との照合は公開テストケース集合＋18件の実測プローブで全件一致。既知の残差（S12 型の許容として検証記録に明記）: upvalue 上限（255）と巨大式のレジスタ上限は再現しない、メソッドの暗黙 `self` の数え方で上限近傍に ±1 の差があり得る、UTF-8 でないソースは拒否する。これらはいずれも受理プログラムの実行結果には影響しない。
- 副次変更: 代替判定のため vm が storm-lua-syntax に依存する（architecture.json 登録済み）。方言変換の実行意味論テストはバックエンドを必要とするため backend-mlua 有効時のみにゲートした（書き換え自体は常時有効）。
- 完了条件の確認: `cargo tree -p storm-lua-compiler-wasm` で mlua／lua-src がグラフに存在しないこと、`cargo check -p storm-lua-compiler-wasm --target wasm32-unknown-unknown` の成功、repo の build-compiler 手順（`node tools/build-compiler.mjs`）の成功（wasm パッケージ生成まで）を全て確認した。

## 実装2: sha2 asm 解除

- `storm-lua-build` の sha2 から `asm` フィーチャを外した（フィーチャはここでのみ有効化されていた）。Cargo.lock から sha2-asm が消えた。ハッシュ出力の不変は Tier FULL のバイト同一で担保した。

## 規約修復（棚卸し提案の実施）

- P1（推移検査）: `tools/check_repository.py` に target_graph 検査を追加。登録クレート（storm-lua-compiler-wasm、storm-screen-wasm）について wasm32-unknown-unknown 向け解決グラフへの lua-src の到達を失敗扱いにする。負のテスト: 到達するクレートへ規則を課すと検出することを確認。
- P2（フィーチャ許可制）: 依存フィーチャの有効化を architecture.json の登録制にした。未登録の有効化は失敗する。負のテスト: sha2 への asm 再有効化を注入すると検出することを確認。登録内容は現行マニフェスト実態から生成した。
- P5: architecture.md の所有者表（minify／build の vm エッジ、vm の spec／syntax エッジ）と「compiler-only は VM に依存しない」旨の記述を現行構造に合わせて更新し、architecture.json と矛盾しない状態にした。
- P6: 本ラウンドのエッジ／構造変更は「architecture.json 登録＋設計文書更新＋推移先の性質確認」を同一の変更として実施した（vm→syntax エッジ、backend-mlua の意味変更、sha2 のフィーチャ解除が対象）。
- P7: dependencies.md に backend-mlua の意味変更（再公開ゲート→依存ゲート）と代替判定の正本ポインタを明記した。

## 全表ゲート（新コード先端 42fb847）

- Tier FULL（コーパス 4,155 ファイル×4 モード）: ソート済み TSV の SHA-256 が基準 `42ce28cacbfec693acb1c1dc3b338251ebe46e5a1f09dab284c4bd393e9b4e28` と**バイト同一**。16,620 行、フォールバック 0 ファイル。証跡は実行台帳へ提出済み（run: storm-lua-engine/sle-r305-full-verify-42fb847）。
- ワークスペーステスト: `--all-features` で 1,775 passed / 0 failed。既定フィーチャ構成でも 1,775 passed / 0 failed（ワークスペース一括ビルドではフィーチャ統合により backend-mlua が有効になるため同数。フィーチャ無効を単独で固定する vm ライブラリテストは別途実施: 無効時 5 件（代替判定テスト）、有効時 54 件、いずれも全件合格）。
- fmt check、clippy（`--workspace` および `--all-targets --all-features`、いずれも CI 条件 `-D warnings`）、`cargo xtask check`（P1／P2 の新検査を含み「2 target graphs pure」）、rustdoc（壊れた intra-doc リンクを禁止）: 全て合格。
- conformance 例題4件（microcontroller、differential、compiler、addon_host）: 実行成功。
- wasm32 両ターゲット（wasm32-unknown-unknown、wasm32-unknown-emscripten）でコンパイラ系クレート一式（syntax、spec、analysis、minify、build、vm、compiler-wasm、screen-wasm）の check: 合格。
- Python ツールテスト、node リリーステスト 9 件、npm パッケージテスト 30 件: 合格。
- 独立回帰パック（S9〜S14、114 件）: 新先端でツールを再ビルドして再実行し **114/114 PASS**。
- 分類A再監査（先端ツリーの追跡ファイル）: 開発ホスト名・ホームパス・ハードウェア型番・接続ツール名・私的ネットワークアドレスのいずれも 0 件。

## FULL-VERIFY-TIP

- `6118faf` から `42fb8477d1643d753e859b4f8baa3b4dcca9526a` へ前進させる（累積 FULL は本ラウンドで再実施済み）。

## 追補: 初回候補 CI の所見と処置（スカッシュ ad81e6a、run 41594904715）

初回 CI は4ジョブ全てが失敗した。いずれも内容を特定し、本ラウンド内で処置した（修正コミット `faf041b`）。

- native 3 OS: `cargo xtask check` で失敗。P1 の推移検査が `cargo tree` を `--offline` で実行していたため、対象ターゲットで初めて現れるクレート（base64-simd 等）のマニフェストが CI の冷たいキャッシュに無く、解決自体が成立しなかった。wasm ジョブでは先行ステップがキャッシュを温めるため同検査を通過しており、環境依存の見逃しだった。処置: `--offline` を外し（`--locked` は維持）、失敗時に理由を明示する形にした。冷たいキャッシュを模した再現（対象クレートのキャッシュを退避して xtask check を実行）で通過を確認した。
- wasm ジョブ: compiler テスト18件中2件が失敗。いずれも製品の退行ではなく、v0.3.1 系統の期待値の陳腐化だった。(1) パス総数 67 は公開 main の値で、現行は Rust 側レジストリテストが 73 を固定している。(2) 既に小さいコードの恒等返しの期待は、ネイティブ（参照判定）で同一入力・同一条件を実測し、現行の satisficing 出力（`function\nonTick()end`）と一致することを確認した。本スイートは第299ラウンド以降 build-compiler の失敗で実行できておらず、今回初めて露出した。処置: 期待値を現行の検証済み値へ更新した。
- 処置後の再検証: `cargo xtask check` 合格、compiler テストはローカルで 18/18 合格。Tier FULL は修正後先端でも再実施し、バイト同一を確認した（FULL-VERIFY-TIP は修正後先端へ前進）。

## 追補2: 2回目候補 CI の所見と処置（スカッシュ 02cf8f1、run 41597006080）

ubuntu／macOS は全ステップ通過し、ブロッカー2件の解消（xtask 検査の冷たいキャッシュ対応を含む）を確認した。残った失敗は2件で、いずれも wasm ジョブと Windows がテストステップまで到達したことで初めて露出した、v0.3.1 系統の期待値の陳腐化だった（修正コミット `ce29be3`）。

- Windows: ワークスペーステスト 1,148 件中 1 件（16進浮動小数点の境界リテラル）。vendored PUC が MSVC 環境で `0x1.0000000000000800001p0`（厳密値 1+2^-53+2^-76）の末尾桁を取りこぼし 1.0 を返す一方、生成側と Linux/macOS の PUC は最近接の 1+2^-52 に一致する。オラクル側のプラットフォーム差異であり、この1件のみ Windows で差分比較を省略した（理由はテスト内のコメントに明記）。
- wasm: test-wasm 92 件中 5 件。内訳は (a) 候補集合の比較2件 — target-size モードは exhaustive な構造集合に target 専用レイアウト（joint-api-local 系、2026-10-30 の SMC 複合レイアウト追加）を上乗せする現行設計に対し、旧比較は完全一致を要求していた。勝者コードの一致は維持されており、比較を現行契約の形（exhaustive 候補の包含＋追加分の同定）に直した。(b) identity 短絡の前提2件 — 2026-10-30 のユーザー決定（db920cf）で廃止済みの契約。うち Source Map のコピー系は、短縮しないリンクビルドの verbatim コピー領域を対象に書き換え、帰属が無傷であることを実測で確認した（モジュール本体の `function()` は lib の元位置へ正確に引ける。ラッパ側の出現はリンカ形状変更 4955397 によるテスト側の針のずれ）。(c) UTF-16 カラムのマップ解決1件 — 同じくラッパ形状変更に伴う針のずれ。
- 後段ステップの先行確認: check-package、test-package（隔離インストール検査）、Playground の build／test をローカルで先行実行し、残っていた同種の陳腐化（パス総数 67→73、schemaVersion 1→2、Playground の identity 前提4件）を同コミットで更新した。ローカル結果: test-wasm 92/92、test:compiler 18/18、test-package 完走、Playground 38/38、ワークスペーステスト 1,775 件全通過。
- Tier FULL は修正後先端 `ce29be3` でも再実施しバイト同一（証跡 run: storm-lua-engine/sle-r305-full-verify-ce29be3）。FULL-VERIFY-TIP は `ce29be3` へ前進する。

## 追補3: 3回目候補 CI の所見と処置（スカッシュ 783b180、run 41599982391）

native 3 OS が全ステップ通過し、ブロッカー2件の解消が候補 CI で確定した。wasm ジョブは test-browser まで通過し、test-compiler-browser の schemaVersion 期待値（1→2、同種の陳腐化が1件残っていた）で停止したため、同期待値を更新した（コミット `775f4bb`）。Tier FULL は同先端で再実施しバイト同一（証跡 run: storm-lua-engine/sle-r305-full-verify-775f4bb）。FULL-VERIFY-TIP は `775f4bb` へ前進する。残る未実行ステップ（test-package、Playground consumer、Playground browser）は次回 CI で確認する（test-package と Playground の build／test はローカルで通過済み）。

## 追補4: 4回目候補 CI の所見と処置（スカッシュ 8019c55、run 41601917008）

native 3 OS 通過。wasm ジョブは test-package と Playground consumer（build／test）まで通過し、最後の Playground ブラウザ操作ステップで停止した。原因は `app/scripts/source-map-browser.mjs` の2箇所で、いずれも陳腐化した前提であることを実測で確認した:

1. 全体コピーシナリオ: minify の identity 成果物を前提にしていた。全体コピーの成果物はモジュールなし・非 minify のビルドが現在も生成するため（生成コード＝原文、コピー範囲1件を実測）、シナリオの駆動を build ステップへ切り替えた。選択・キャレット・リロード保持・ブレークポイント一時停止の各検証は無変更で全て通過する。
2. 大規模成果物フィクスチャ: 巨大コメントは minify で除去され成果物が 6MiB に届かない。onTick から参照される巨大文字列リテラルへ変更し、実コンパイラ出力で成果物 6,811,813 バイト（コード 3,400,046 バイト）を構成した。

Playwright ブラウザ（chromium／firefox／webkit）を .ud に導入し、アプリのブラウザテスト全体をローカルで全エンジン通過させてからコミットした（`96d5691`）。Tier FULL は同先端で再実施しバイト同一（証跡 run: storm-lua-engine/sle-r305-full-verify-96d5691）。FULL-VERIFY-TIP は `96d56916855fe174147e711457915282c7fe50e2` へ前進する。

## 追補5: 最終候補 CI 全通過と工程2・3の完了（スカッシュ f957190、run 41604704264）

5回目の候補 CI（run 41604704264）が全ジョブ通過した: native ubuntu／windows／macOS、wasm（test-wasm 92件、test:compiler 18件、ブラウザ検証、test-package、Playground consumer の build／test／ブラウザ操作を含む全ステップ）。これによりブロッカー2件の解消と規約修復を含む第305ラウンドの候補が確定した。最終スカッシュは `f957190e4fafc75e80728e929c30de9569a073df`（親 `44376b89`、ツリーは統合先端 `a5a0d87` と一致、コード先端 `96d56916855fe174147e711457915282c7fe50e2`、FULL-VERIFY-TIP 同値）。

工程2・3の成果物（`~/sle-release/v0.4.0/artifacts/`、producer は最終スカッシュのチェックアウト）:

| 成果物 | bytes | SHA-256 |
| --- | ---: | --- |
| source-manifest.txt（831ファイル） | 95,451 | fd9a20bb28911c52c6fce7a1940f37ef26ef8ffe1b7452c222dd3ec22c0a7afd |
| stormcat-works-storm-lua-engine-0.4.0.tgz（62ファイル、test-package 隔離検査通過） | 1,883,549 | f4606a8c97a01b87ee08dcfde0874a1bc420a96cda6d08a83861c6735b2bf81e |
| storm-lua-engine-playground-0.4.0.zip | 3,295,220 | bf2c0a3d252f3c3d9887013f802f876e955ad616797d9ee02440f9743baae8a4 |
| release-candidate-0.4.0.json（状態 ci-passed-unpublished） | 2,967 | 1341d06dac07f5942540b0fbab2c166c31d9660b4c51b1f12a25ebd801bf6fdf |

SHA256SUMS は上記4成果物を対象に再生成した。タグ・GitHub Release・npm 公開・配備（工程4以降）は未実施であり、2026-11-04 の最終承認待ちである。
