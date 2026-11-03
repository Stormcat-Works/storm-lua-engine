# 候補S10／S14 検証記録（w9 第73サイクル）

## 修正の概要

storm-lua-vm に公開チェック関数 `check_load_legality` を新設した
（`crates/storm-lua-vm/src/load_legality.rs`）。処理は
`dialect::rewrite(source)` で方言書換えを適用したのち、
mlua の `Lua::load(...).set_mode(ChunkMode::Text).into_function()` で
パースのみを行い、決して実行しない。判定は VM 判決そのもの
（VM-verdict）である: VM が受理するソース（R210 ラップを含む）は
通り、VM が拒否するソースだけが失敗する。

ゲートの配置は次の 2 箇所である。

- storm-lua-minify: `prepare_search_from_parsed` の直後
  （`environment_checks::validate` の後、lexical 分岐・`property_reads`
  変換より前）と `try_satisficing_from_parsed` の直後（target 経路）。
  この単一の choke point が単一ソース compile（C）、直接 minify
  `compile_code`（M）、プロジェクト minify（P+、リンク済みソース）、
  lifeboat（LB、リンク済みソースを `compile_code` に通す）、verifier
  経路、target 経路をすべて覆う。構文エラー優先順序は保たれる
  （ゲートはコンパイラ自身のパースの後で走る）。
- storm-lua-build: `public_api.rs::compile_project` で、minify=true の
  場合のみ、リンク後に `link.used_modules` の各モジュールを
  `prepared.project.modules` から取り出して検証する。失敗時は
  `compile_failure("module `{key}` is not loadable: {error}")` を返す。
  minify=false の link-only 経路は v43 の記録通り意図的にゲート対象外
  （リンクだけではコンパイルを主張しない）。Ambient Module のソースは
  注入ラップの意味論が未解析のため意図的に検証しない。

## S10: 使用バリアは閉じ、未使用バリアは方言として残る

- 使用バリア（barrier がラベル後に使用される形）: ゲートがパス前に
  ソースを拒否するため、tiny-literal-inlining や single-use-local-sinking
  がバリアを先に除去して受理してしまう経路は構造的に閉じる。
- 未使用バリア: R210 の方言（ラップ）により VM が受理するため、
  ゲートも受理する。第73サイクルの見立て通り、これは発散ではなく
  承認済みの方言動作である。

## S14: 全形を閉じる

v42 の確定 7 形＋プローブ 4 形、v43 の表面拡張（チャンク 201 局所の
洗浄、fusion による `return ...` のチャンク接合、プロジェクト経路の
無洗浄受理、lifeboat の部分集合の不一致）はすべて、入口ゲート
（＋プロジェクト経路のモジュール検証）で閉じる。根底の「API は
解析可能性のみを検証し、ロード合法性を検証していない」は解消された。
link-only エントリ（P-、L-）はコンパイルを主張しないため対象外と
明記する（v43 の記録通り）。

## 回帰証拠

- 単体テスト: vm 4 件（使用バリア拒否、方言ラップ受理、洗浄形拒否、
  合法プログラム受理）、minify 3 件、build 3 件、全合格。
- validation head の diffexec: S10 使用バリア形、S14 deadfn-vararg 形、
  fusion-break 形、ap-v02（fusion vararg）形は plain／opt／safe 全て
  compile-fail（修正前は opt／safe が実行していた）。受理側は
  gotoscope-min、gluelabel-min が全モード ok。vmgoto-min の vm-error は
  検証前ビルドと同一（diffexec が `emit` スタブを持たないための実行時
  エラー）で、ゲートによる変化ではない。
- Tier FULL（累積 base `bff123a` → head `c65a629`）: 16,620 行。
  identical=4,153／構成、hard=2、status は ok 4,126／error 27 に加え
  2 ファイルが ok→error へ変化した（下記）。map-core=0、expl-only=0、
  missing=0。
- ワークスペーステスト: validation head で 1,775 件全合格・失敗 0
  （S11 head 1,765 件に本修正の新規 10 件を加えたものと一致）。
  注記: 初回実行で storm-lua-syntax の deferred_origin テスト 1 件が
  失敗したが、base・head の単独実行および head のスイート全体実行
  （2 回）では再現せず、並行実行時の flaky と判断した。本変更と
  無関係（同クレートは本修正で触っていない）。
- fmt: 変更ファイル無指摘。clippy（lib）: 変更ファイルに新規警告なし
  （既存警告のみ）。

## FULL で新たに拒否された 2 ファイルの分析（重要・判断材料）

新たに error になったのは次の 2 コーパスファイルである。

- `772ab0b9c0e6da315eeaf799d16cc31d98478c1cd38478fa2a56d8be0b2bc692.lua`
- `ed484f05b6d062121fb242ad05ae560e2eac4570109d904b5f81c15bc7c1c2e6.lua`

両ファイルとも非 vararg 関数 `onDraw` の内部に死んだ空分岐
`if dNo==... then end` を含む。検証結果は次の通りである。

- プロジェクト VM 自身（`Vm::execute`、Game プロファイル）は両ソースを
  PUC と同一文言 `cannot use '...' outside a vararg function` で拒否する。
  ゲートの判決と一致し、誤検知ではない。
- ホストの raw PUC（luac 5.1.5）も同一文言で拒否する。
- 検証前コンパイラの plain 成果物には `...` が残らない（死んだ空分岐
  が除去される）ため、従来の成果物はロードできていた。すなわち従来
  はパイプラインが PUC 違法ソースを無言で修復していた。

したがってこの 2 ファイルの変化は、S14 の設計通りの fail-fast
（違法ソースをコンパイル時に拒否する）が働いた結果であり、合法な
プログラムの出力は一切変わっていない（identical 4,153 件＋状態不変
4,153 件）。ただし実在コード 2 件（4,155 件中 0.05%）のビルドが
新たに失敗するようになることは測定済みのコストとしてここに記録する。
この方針を覆す（無言の修復を残す）かどうかは親／ユーザーの判断に委ねる。

## S12 の処置（判断事項として残す）

S12（VM 自身の goto-scope 検査が未使用バリア形を受理する）は
PUC 対等への狭窄としては実装していない。狭窄は 2026-10-31 承認の
R210 決定（未参照横断ケースのラップ受理）そのものの逆転になるため
である。S11 修正＋本ゲートの後、plain 経路は S12 形を VM ソース直読み
と一致して受理する。S12 を閉じるには R210 の方言そのものを覆す
ユーザー判断が必要であり、本サイクルでは判断事項として残す。

## S13

本サイクルでは扱わない（帰属のみの別候補。plain の i08
`mapped=NONE` は本ゲートの対象外であり変化しない）。
