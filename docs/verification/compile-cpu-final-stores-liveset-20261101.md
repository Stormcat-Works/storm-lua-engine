# コンパイル CPU: final-stores の live 集合を密ビットセット化（第91サイクル、Tier INVARIANT）

対象: `crates/storm-lua-minify/src/passes/final_stores.rs` の後方走査が保持する
live 束縛集合。第89サイクルの計装で、final-stores の走査内 clone が
646,696 回・総要素数 40,939,083 に達していた（除去は 3,459 件）。

## 順序観測可能性の監査（第86サイクルと同型）

コード読み（先端 `5b29f94` 時点の同ファイル 548 行）:

- live 集合（`live` / `live_in` / `live_out` / `exit_live` /
  `persistent_globals` / captured 集合）の消費は次のみ:
  membership（`contains`、除去判定 1 箇所）、`remove`、`insert`／`extend`
  （和集合）、clone（3 箇所: ブロック入口・else なし If の分岐入力・Return）、
  および move。
- 集合要素を反復するのは、和集合の「源」としてのみ（If 分岐のマージ、
  captured → persistent_globals の extend）。和集合の結果は内容のみで決まり、
  宛先集合自体も観測可能な形では反復されない。
- 基数（`len`）の読み取りは同ファイル内に存在しない。除去される文の順序は
  ブロック文の Vec 順で決まる。生成コード・Map の順序付けに集合反復は関与しない。
- 読み書き効果は `Vec<BindingId>`（決定的順序）であり、集合へは内容だけが入る。
- `function_scopes` の HashSet も contains のみ。集合は全てファイル内局所で、
  公開 API に現れない。

合成実験: 分岐・早期 return・ループ・捕捉・多重代入を含む 6 入力について、
未変更バイナリ／変更後バイナリの双方で独立 40 プロセスずつ実行し、
全入力で出力ハッシュが単一（プロセス毎に変わる std HashSet のランダム
シードが順序漏れを露呈させなかった）。

結論: 反復順は観測不能。表現変更は Tier INVARIANT（Tier FULL 相当の
全コーパス比較ゲートを併用）として実施可能。

## 変更

`HashSet<u32>` を、束縛 id 空間（`res.bindings.len()`）に対する密ビット
セット `LiveSet`（`Vec<u64>`）へ置換。clone はワードコピーになり、
contains／insert／remove／和集合は単一ワード操作になる。集合代数の
セマンティクスは従来と同一（範囲外 id への防御的成長も含め集合としての
振る舞いを保存）。LiveSet の単体テスト（ワード境界・和集合・clone
独立性）を追加。

## 決定論カウント（std-nomap、全コーパス、計装プローブ fsprobe）

| カウンタ | base | head |
|---|---:|---:|
| pass 呼び出し | 34,896 | 34,896 |
| clone 回数 | 646,744 | 646,744 |
| clone 要素数（HashSet len / bitset popcount） | 40,940,259 | 40,940,259 |
| clone ワード数（head のみ） | — | 1,899,105 |
| reads 和集合 | 6,429,357 | 6,429,357 |
| writes remove | 189,664 | 189,664 |
| writes extend | 446,720 | 446,720 |
| target remove | 641,132 | 641,132 |

clone のコピー量は要素 40,940,259 → ワード 1,899,105（約 21.6 分の 1、
かつ要素毎ハッシュ処理が消滅）。走査内の集合要素タッチ
（reads 6.43M＋writes 0.64M＋target 0.64M 等）も全てワード操作化。
プローブの std-nomap 行ハッシュは両側 `30ec6bd2…` で、FULL ゲート TSV の
std-nomap 部分集合のハッシュと一致（プローブ忠実性を確認）。

## ゲート

- FULL（統合先端 `79c0acf` へリベース後に実施）: base／head とも 16,620 行・
  fallback 0、SHA-256 `4a98d0939f13cb4f3f888393087bbb64f8945cf3e5a9154e9bb7680ade733028`
  でバイト一致。リベース前の base `5b29f94` でも同じハッシュでバイト一致を
  確認済み（候補 H 修正はコーパス出力を変えなかった）。
- ワークスペーステスト: 1,655 件全通過（新規 LiveSet テストを含む）。
- clippy: リベース後の base／head で警告集合同一。rustfmt は対象ファイル
  単体で無指摘。

## .us2 測定（MEASURE.lock、submit 実測 1 回、固定 208 ファイル std-nomap＝20 件ごと抽出、交互 3 回）

- base（`5b29f94`）中央値 13,522,462µs（13,506,651／13,565,837／13,522,462）
- head 中央値 13,422,520µs（13,422,520／13,384,521／13,535,875）→ −0.74%
- 範囲は重なるため壁時計改善の主張は中央値 −0.74% の範囲に留める。
  採用の主根拠は決定論的な消滅（clone のワードコピー化＋要素毎ハッシュの消滅）。
- status は全 rep・両側一致（ok 206／err 2）、行ハッシュも一致。
- 注: .us2 には Rust ツールチェーンが存在しないため、バイナリは .wsl で
  release ビルドしたものを同梱した（両側同一条件、glibc 2.43／x86_64 一致）。
  提出バンドルのビルド試行 2 回は cargo 不在で測定前に失敗し、実測は
  バイナリ同梱バンドルの 1 回のみ。
- 注: 測定はリベース前の base `5b29f94` に対する値。リベースで取り込んだ
  候補 H 修正（inline_functions）は本変更と直交し、本変更のコードは不変。

## 統合

muse/w7（実装＋本記録、分岐元 `79c0acf`）を MERGE.lock 下で CAS
fast-forward し、第253ラウンドとして統合。Tier INVARIANT のため
FULL-VERIFY-TIP は `79c0acf` のまま（第252の後の INVARIANT 1 件目）。
