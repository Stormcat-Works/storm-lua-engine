# tracked 印字の literal_reason における定数 Arc の共有化（2026-10-27）

## 変更

`Printer::literal_reason`（`crates/storm-lua-syntax/src/print.rs`
のみ）で、リテラル綴り理由の構築方法を変えた。公開 API・生成
コード・理由の値は変わらない。

- 3 種の理由コード・basis（`canonical-literal-emission`）・空の
  facts を、`OnceLock` で共有する1個の `Arc` に変えた。初回のみ
  確保し、以降は `Arc` の clone（参照カウント操作）で済む。
- 文字列分岐の before／after（`literal-bytes:{len}`）は、長さを
  スタックバッファで10進整形してから `Arc` 化する形に変え、
  `format!` による `String` の一時確保をなくした。
- 数値・真偽値分岐は `format!` を残し、コード・basis・facts の
  共有化のみ適用した。

## 正しさの根拠

- 共有化する値はすべて定数で、理由の各フィールドの値は従来と
  バイト同一である。`Arc<str>` の等価性は内容で決まるため、
  共有してもシリアライズ・比較・解決結果は変わらない。
- 文字列長タグは、接頭辞 `literal-bytes:` に10進の長さを連ねた
  もので、従来の `format!` と同一の文字列になる。バッファは接頭辞
  14 バイト＋`usize` 最大 20 桁で、長さ 0 の場合も 1 桁になる。
- バッファの UTF-8 化は、内容が ASCII のみのため
  `from_utf8_lossy` が借用で済み、確保は `Arc` 化の1回だけである。
  clippy の unwrap 禁止（`-D clippy::unwrap-used`）にも抵触しない。

## 測定の経緯

同サイクルの切り分けでは、gap ブロックの copied 照合は交互測定で
差なし、miss 時 intern は約 0.39ms だがこの設計ではほぼ下限で
あることを確認し、tracked 印字の arena 側へ焦点を移した。
`literal_reason` の無効化変種は、交互測定で tracked 印字を
約 1.11ms から約 0.93ms へ動かした（高負荷時のスパイクを除く）。
原因は、合成入力の 640 件の文字列リテラルで毎回
`string-literal-spelling` 理由が生成され、理由1件あたり
コード・basis・facts・before・after で計7回程度の確保が
起きていたことである。

## 測定（tracked 印字、31 反復中央値、旧・新バイナリ交互）

| 入力 | 変更前 | 変更後 | 差 |
| --- | --- | --- | --- |
| dense-normal | 約 1.10ms | 約 1.03ms | 約 −0.07ms（約 −6.5%） |
| dense-lexical | 約 1.08〜1.11ms | 約 0.99〜1.01ms | 約 −0.09ms |
| scale-80 | 約 0.54ms | 約 0.51ms | 約 −0.03ms |
| draw | 約 0.23ms | 約 0.22ms | 約 −0.014ms |

plain 印字は全入力で不変。print＋from＋resolve（resolvebench、
交互5ペア）は旧中央値約 3.84ms に対し新約 3.72ms（4勝1敗）。
件数（mappings・origins・constructs・dispositions）は新旧同一。
理由の値の同一性は、新旧バイナリのハッシュ版ダンプで、生成コード
全文＋理由（code・before・after・basis・facts 件数）の SHA-256 が
完全一致することで確認した（新旧バイナリ自体のハッシュは異なる）。

## テスト

- 対象 3 クレート全件成功：syntax 107 件・minify 453 件・build 126 件。
- ワークスペース全件で失敗なし。
- xtask check 通過、cargo fmt --check 通過、syntax クレートの clippy
  は警告なし。ワークスペース全体の clippy の警告 4 件は minify の
  テストコード由来で、本変更のファイル外（統合先端由来）。

## 実コーパス全件比較

実コーパス全件（4,155 ファイル×4 条件）の比較では、成功／失敗
（4,127／28）・生成コード・Map 中核・正規化済み Map 全体のすべてが
差分 0 件で、両 TSV はバイト同一だった。
