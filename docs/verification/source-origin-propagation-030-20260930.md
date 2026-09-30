# v0.3.0 内部由来伝播 — 2026-09-30

状態: 開発中。v0.2.1はnpm/GitHub/Playgroundへ公開済み。今回の変更は専用branchだけに置き、既公開tag/tarballやrelease branchを変更しない。

## 実装した範囲

元snapshotとnode/name範囲を、optionalなNodeArenaの由来テーブルへ接続した。構文比較・短縮候補の採否からmetadataを分離する。cloneとrollbackは対応する由来を保持し、別Arenaの同じ数値IDへ古い位置を流用しない。未監査の直接mutationは由来を失効させ、新nodeは明示的な帰属がない限りUnknownとする。

通常探索、target早期終了、未達時の共通探索、字句短縮、原文候補の返却、JSON/binary Worker転送に接続した。低レベルCompileOptions.origin_sourceを指定するとCompileCodeResult.originsを返す。返却位置は採用した最終codeの実際のUTF-8 byte範囲で、標準Source Map v3への符号化はまだ行わない。高レベルRust/WASM/TSのminify結果へmapが付くようになったとは説明しない。

定数化、名前短縮、括弧除去、プロパティ固定化、宣言の結合/削除、一部のscope/global変換、alias生成、関数式の展開などへ明示的な引き継ぎを追加した。インライン化では、定義内の式、実引数の元範囲、呼び出し位置と仮引数使用位置を区別して保持する。削除する宣言を探すだけで無関係な全nodeの位置を消していたcleanupも修正した。

[全パス台帳](../design/source-provenance-pass-audit.json)は67パスを列挙し、19パスをpartial、48パスをpendingとした。部分対応はそのpassの全変換の監査完了を意味しない。source→sourceコピーが通る一つのfixtureを、複雑な変換一般の証明として扱わない。

## 誤った位置を返さないための検証

原文範囲のUTF-8境界、snapshot番号、identifierの元綴り、slot数、重複slotを確認する。既知の親範囲があっても、未知の子範囲はUnknownのままにする。明示的なAPI alias定義はSyntheticとし、追跡不足をSyntheticと偽らない。

内部座標検証は内容のfingerprintではない。code/map/元snapshotを識別する最終成果物契約はP4で完成させる。Ast.nodesを直接構築する低レベルconsumerはVecからNodeArenaへの更新が必要。未追跡ASTの通常JSON形は維持するが、binary Worker contextは同compiler版同士で使用する。

## 実際の変換例

`cargo run -p storm-lua-conformance --example minify_origins`で、原文`local signal=input.getNumber(1) output.setNumber(1,signal+2 * 3)`を実際のpassに通す。

限定した確認用passでは`local a=input.getNumber(1)output.setNumber(1,a+6)`を生成し、短縮名aの各出現は別のsignalの元位置、生成された6は元の`2 * 3`へ対応する。

全設定を有効にした通常探索では`output.setNumber(1,input.getNumber(1)+6)`になり、この小さな例の40byteすべてが帰属する。移動したinput呼び出しには定義元の範囲と、置換された使用位置の関連範囲がある。これは全コーパスや全描画変換の完全対応を意味しない。

## 測定・回帰の扱い

最終ゲート・Nativeの追跡ON/OFF比較・WASM既存出力比較の結果は、同名JSONへ保存した。大きな原文、短縮結果、収集元の私有情報を含む原ログはlocal-validation/source-provenance-030-stage2にのみ置き、公開文書には件数・集計・hashだけを残す。

Nativeの追跡有無は実際にorigin_sourceを切り替えて比較する。WASMの高レベルAPIはまだtrace指定を公開していないため、WASMの通常API回帰は非追跡経路の検証であり、最終ソースマップAPIの試験と混同しない。binary/JSON Workerの追跡付き往復はNativeの実codecで試験する。

追跡の不明率は未帰属byte数の指標である。帰属済みにはSyntheticやGroupも含むので、その補数を『精密に原文へ戻れる割合』と表現しない。処理時間は探索の停止条件に使用せず、4秒を任意入力の保証上限としない。

## 残っている主な作業

P3の未対応変換と組み合わせの監査、共有や複数元範囲、table/名前再編、helper/描画loop/data/辞書化、追跡コストの改善が主な残件。P4で標準mapと詳細由来の公開型、link範囲の合成、Rust/WASM/TS・Worker境界、code/map識別を完成させる。P5でPlayground双方向選択・関連由来・Unknown/Synthetic表示と実runtime提供位置へのエラー/停止対応を実装する。

関数式の展開テストが成功したことは、消えた元変数やインラインframeを完全に復元できることを意味しない。これらは位置マップと別の高度なデバッグ機能である。


## 最終実行結果

Native workspace **553件**、SDK **30件**、実WASM **69件**が成功した。compiler関連の17件も再実行したが全体WASMと重複するため合計へ加算しない。fmt、clippy、rustdoc、architecture、Chromium/Firefox/WebKitのcompiler Workerも成功。

Nativeでは代表30入力の240設定で、追跡ON/OFFの生成Lua・候補サイズが一致した。各設定のtarget=0、fast/beam1未達240件も、全探索と生成Lua・由来情報が一致した。240生成結果のhashは着手前WASMの基準結果とも一致。新しいWASMでは追跡指定なしの既存APIを240設定と未達30件で比較し、同じ出力を確認した。[機械記録](source-origin-propagation-030-20260930.json)。

### 原文追跡はまだ広範囲で未完成

代表240設定の未帰属byte割合は、中央値 **97.97%**、最小0.61%、最大100%。全出力がUnknownだった設定は68件。これを『テストが通ったからソースマップ対応済み』と扱わない。既存Luaの意味・文字数が変わらないことと、元の位置を辿れることは別のゲートである。

下記は同一Native release binaryの追跡OFF/ON比較で、各3回の中央値。原文はループ開始時に読み、測定にはコンパイルだけを含める。テスト環境のCPU負荷などによる変動を含み、WASMや実ゲームの時間ではない。

| 入力・設定 | 追跡OFF | 追跡ON |
| --- | ---: | ---: |
| 制御例・全探索 | 2.906秒 | 3.607秒 |
| 制御例・8192目標 | 0.446秒 | 0.393秒 |
| 矩形4,096個・全探索 | 5.568秒 | 7.475秒 |
| 矩形4,096個・8192未達 | 5.569秒 | 6.946秒 |

大きい描画入力では追跡の負担も残っている。標準mapの符号化や全pass対応前の内部追跡の測定であり、製品の完成性能ではない。原文snapshot/slotの共有と変更のない由来recordの再利用、最終出力originのinternを実装したが、これだけで性能課題が解決したとは説明しない。

private入力・生成コードを含む詳細測定データは公開JSONへ含めていない。公開記録は件数、集計、ビルドhashと検証範囲に限定する。


## GitHub上の検証

実装commit `ca87e1f3dbf7e39422fcb0cd5550e932ddf0f0da`に対する[CI run 36675436003](https://github.com/Stormcat-Works/storm-lua-engine/actions/runs/36675436003)が成功した。Linux/Windows/macOSのNative、WASMの全4jobで成功。WASM jobには実ブラウザー、独立した梱包consumerとPlayground検証も含む。

このCI成功は、現段階の機能の回帰と実行基盤の検証である。最適化後mapの高レベル公開や、partial/pendingパスの原文帰属が完成したという意味ではない。今回の実装は開発branchへ保存し、main/release/v0.2.1 tagは変更していない。
