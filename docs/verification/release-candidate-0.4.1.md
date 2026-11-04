# v0.4.1 公開候補の記録（2026-11-04）

v0.4.1は、StormMinのv0.4.0回帰サイクルで見つかった2件の正確性問題だけを修正するパッチリリースである。機能追加・非互換変更はない。統合ブランチ`muse/perf-optimization`の先端で検証し、本記録のcommitまでを公開候補とする。公開（GitHubへのpush・タグ・Release・npm）は本記録の範囲外で、残工程は末尾に列挙する。

## 修正1: targetSize探索の再開経路が一括実行と異なる勝者を選ぶ

### 症状

目標文字数（targetSize）を指定した最短化で、探索の途中経過を保存して再開する経路（一括実行の`finish_target_attempt`に対し、WASMの`try_satisficing`→`finishTargetSearch`）が、一括実行と異なる勝者を返した。StormMin側の測定では、targetSize:8192の全8設定で再開側が406〜417文字、一括側が428〜439文字を選び、worker-continuationでも再開169に対し一括181だった。

### 根本原因

勝者選択は`SearchContext.lexical_target`の有無で分岐する（Someなら「目標に収まる最速の候補」、Noneなら「最小の候補」）。一括経路は文脈生成後に`lexical_target`を明示設定していたが、逐次化した文脈から再開する経路の生成関数`prepare_search_from_base`は、同じオプションを受け取りながら`lexical_target: None`で文脈を作っていた。このため再開経路だけが最小化基準で勝者を選び直していた。既存の再開テストは到達不能な目標（0）と比較対象の両経路とも目標なしの設定しか使っておらず、欠落を検出できなかった。

### 修正と判断

`prepare_search_from_base`が`options.target_size`を文脈へ載せるよう1行を修正した（`e2433a4`）。**再開経路を一括経路に合わせる方向**であり、公開済みv0.4.0の一括実行の出力は変わらない。逆方向（一括を再開側の勝者に合わせる）は公開済み経路の出力を全面に変えるため採らない。再開側の勝者が小さかったのは目標基準を落とした副産物であり、「速く目標に収まる」というsatisficingの契約自体は一括側が正しい。

回帰テストを`storm-lua-minify`の探索逐次化テストに追加した。文脈とジョブを逐次化往復させて分割実行し、一括実行と勝者・コード・候補サイズ・目標到達が完全一致することを、目標到達可能なケースを含む3条件で assert する。修正前は期待どおり分岐して fail し、修正後に pass する。

## 修正2: サイズ計測が印字長より大きい

### 症状

印字時に1文へ統合される連続したlocal宣言（`local a=1 local b=2`→`local a,b=1,2`）を計測が数えず、印字35文字の入力を41と計測した。StormMin側では105 fixture中24件で計測が印字長を上回った。

### 根本原因と監査

印字器はブロック印字時に文レベルの整形（隣接localの統合、空のif文の除去）を行うが、`storm-lua-syntax`の計測は文を個別に数えていた。同種の乖離を監査した結果、乖離クラスは2つあった。(a)隣接local統合、(b)印字器が落とす空のif文（dead if）を計測だけが数える。数値リテラルの印字短縮形は式計測が既にモデル化しており、乖離はなかった。

### 修正

印字器のブロック整形（統合可否の判定一式）を`block_items`として切り出し、印字器と計測が同じ判定コードを共有するようにした（`4035e63`）。判定そのものが単一実装になったため、今後この種の乖離は構造的に起きない。計測が印字長と一致することを、統合可能な並び・統合を阻む各条件・dead ifを含む17+14件の回帰テストで固定した。

### 波及の是正（`5201dbc`）

計測を正したことで、旧計測に較正されていた消費側3件を是正した。

- `pack_adjacent_locals`の全プログラムgate: 印字器が既に統合する以上、pack前後の計測は同値になり「厳密に縮む場合のみ採用」gateが常時不採用化していた。gateの実質的な意味は旧計測下でも「増やすな」だったため、増加のみを却下するよう変更した（packは宣言originを1ノードへ集約するASTレベルの書き換えとして意味を持つ）。
- 正確モードの定数畳み込みのパス記録: 記録の発火条件が`saved > 0`だったため、印字上は消える文（dead if）への畳み込みが、正直な計測では saved=0 となり記録が消えた。畳み込み側が「書き換えたか」を返すよう変更し、書き換えがあれば truthful な saved（0の場合を含む）で記録する。公開メタデータの契約（正確モードでは定数畳み込みが記録される）は維持される。
- パステスト4件の期待値を是正した。immutable値再利用の3件は、印字統合を織り込んだ正直な採算では元の極小例で再利用が割に合わなくなったため、再利用が実際に勝つ反復回数の例へ差し替えた（再利用が音響的に許される/許されないの対比構造は維持）。符号因数分解の1件は、carrier localのpack費用が実費になったことで因数分解できる式グループが2→4へ増えたことを反映した（出力の単一親不変条件と既存代入の保存は従来どおり assert される）。

## 検証

検証は統合先端`179372b`（版番号commit）で実施した。本記録のcommitはドキュメント追加のみで、コードツリーは変わらない。

- ワークスペーステスト: `cargo test --workspace --all-features --locked`が1,832件 pass / 0 fail。v0.4.0基準1,828件との差+4は新規回帰テスト（修正1が2件、修正2が2件）と一致する。ベース（`6c465f5`）でも同コマンドが1,828/0であり、失敗集合の差分はない。
- 静的ゲート: `cargo fmt --all -- --check`、`cargo xtask check`、`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`、`cargo doc --workspace --all-features --no-deps --locked`がすべて pass。
- コーパスFULL検証（現行コーパス4,155ファイル×4設定=16,620行/側）: 状態は全行一致（ok 16,504 / error 116）。コードが変わったのは187ファイル（748行）で、すべて修正2の計測是正に帰属する（修正1単独の変種はベースとバイト同一のTSVを返した。コーパス検証系に再開経路の設定はなく、修正1はエンジン側回帰テストで担保する）。ok側の総バイト数は26,635,688→26,632,636（-3,052、-0.0115%）。変わった行の内訳は縮小652 / 拡大56 / 同長40で、最大の拡大は1ファイルの+77バイト（勝者・名前割当ての変更）。記録は`~/local-runs/sle-v041-full-verify-179372b/`（runs-submit済み）。
- StormMin交差確認: 使い捨てコピーでStormMinを候補リビジョン`179372b`へ差し替えてWASMを再構築し、(b1)のprobe群とgateスクリプトを実行した。probe-splitで一括175==再開175（コード・mapともバイト一致）、probe-pathsでtargetSize:8192の両経路が428へ一致（修正前は再開406〜417）、probe-routes全件一致、probe-determinismは全設定で distinct 1。`scripts/check-compile-paths.mjs`、`scripts/check-compile-speed.mjs`、`scripts/check-browser-wasm.mjs`がすべて pass。ログは`~/local-runs/sle-v041-release-prep/stormmin-crosscheck/`。

## 候補の構成

- `e2433a4` fix(minify): target探索の再開経路がone-shotと同じ勝者を選ぶよう修正
- `4035e63` fix(syntax): サイズ計測が印字時のlocal統合・dead-if除去と一致するよう修正
- `5201dbc` fix(minify): keep gates and pass records truthful under the corrected measure
- `179372b` release: v0.4.1 リリース準備（版番号・変更履歴・リリース文書）
- 本記録のcommit（docs/verification のみ）

いずれも統合ブランチ`muse/perf-optimization`上にあり、hub（.udのベアリポジトリ）へpush済み。GitHubへのpush・タグ・Release・npm公開は行っていない。

## 残る公開工程（実施者: 親）

1. GitHub main（`44376b89`）の上に、統合先端（本記録のcommit）のツリーと同一のスカッシュcommitを1つ構築する。v0.4.0の `f26c9b3` と同じ形式。
2. スカッシュを検証用ブランチへpushし、CIのconformance runが全passであることを確認する。
3. main / develop / release をスカッシュへ fast-forward し、pushする。
4. スカッシュへ注釈付きタグ `v0.4.1` を打ち、pushする。
5. GitHub Release `v0.4.1` を、CHANGELOGの0.4.1節をnotesとして公開する（配布物はv0.4.0と同じ構成）。
6. publish.yml による npm 公開を確認し、registryで `@stormcat-works/storm-lua-engine@0.4.1` が引けることを検証する。
7. Playground本番の version.json が 0.4.1 / スカッシュのSHA を返すことを確認する。
8. 公開記録 `docs/verification/release-0.4.1.md` を統合ブランチへ書き、hubへpushする。
9. StormMin側へ、v0.4.1スカッシュのSHAへのpin差し替えが可能になったことを伝える。
