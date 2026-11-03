# literal-call-folding の tiny fold 再実行廃止による追跡コスト削減（2026-10-29）

## 変更

literal-call-folding パスで、scratch fold が既に折り畳み値を証明済みの
呼出について、tiny tracked arena でのルート fold 再実行をやめ、
折り畳み済みアトムの由来を 1 回のスロット構成で作る形に変更した。

- `crates/storm-lua-minify/src/passes/inline_functions.rs`：
  新規ヘルパー `direct_folded_atom` を追加し、Bin／Un 腕を置換した。
  失敗時は従来の tracked 経路へ Fallback する（損失は節約の逸失のみ）。
  パス全体で 1 個の `fold_code`（`constant-folding`）を共有化した。
- `crates/storm-lua-syntax/src/node_arena.rs`：
  新規公開メソッド `NodeArena::record_folded_atom_origin` を追加した。
  源 arena のスロットを共有し、fold と同一の rewrite reason
  （`constant-folding`）、disposition、トークンフィルタ、名前フィルタ、
  decision reason を構成して 1 回だけ格納する。
- `crates/storm-lua-minify/src/passes/literal_folding.rs`：
  可視性の変更のみ（挙動変更なし）。
- 回帰テスト：構文レベルの等価性テスト 1 件（Bin／Un の最終スロット
  Debug 文字列を旧経路と比較）と、形状テスト 4 件（unary・comparison・
  concat・and、tracked コードと plain コードの一致＋由来の validate）。

## 安全性の条件

直接構成は次のすべてを満たす場合に限る。いずれかを満たさない場合は
従来の tracked 経路へ Fallback する。

- Bin／Un ルートで、被演算子数が Bin＝2、Un＝1 であること。
- 被演算子がすべてアトムで、literal_value が取得できること。
- 演算子が評価できること（eval_binary／eval_unary が Some を返すこと）。
- aggressive_literal_node が候補を返し、その候補値が scratch fold の
  簡約済み値と完全一致すること。
- Bin のみ：簡約済みアトムの measure がルート Bin の measure 以下である
  こと（fold の literal-evaluation 経路のサイズ条件と同一）。Un には
  fold 側にサイズ条件がないためサイズ条件を付けない。
- 構文ヘルパーが源スロットの共有に失敗した場合は Fallback する。

これらにより、fold が取る literal-evaluation 経路（Bin は
`literal-operands-evaluated-and-size-nonincreasing`、Un は
`literal-unary-evaluation` の decision reason 付き）と同一の経路だけが
直接構成の対象になる。decision reason の事実は fold と完全同一に構成する
（Bin 10 事実、Un 7 事実）。rewrite reason の形状はノード種・演算子・
リテラル綴り／長さのみに依存し、子 ID には依存しないため、previous 値
としてダミー子 ID（Bin は `Node::Bin(op, 0, 0)`、Un は `Node::Un(op, 0)`）
を用いてよい。

## 測定（開発ホスト1、directpass inline.lua、11 反復・中央値）

- 追跡差：8,146 回（16.3 回／件）→ 5,134 回（10.3 回／件）、約 -37.0%。
- 無追跡側も 180,180 回から 158,192 回へ減少した（無追跡モードでも
  tiny fold 再実行がなくなったため）。
- saved=3001、code_len=4443、nodes 4022→4022 は変更前後で同一。
- 時間改善は主張しない（決定的計数のみ）。

残差の内訳（診断変種 V1〜V6 による切り分け）：
relate 2,000 回（表現フロア）＋ context 1,000 回（表現フロア）＋
tiny fold 約 4,512 回＋その他約 634 回で 8,146 回に閉じる。
本変更は tiny fold 約 4,512 回のうち約 3,012 回を除去し、直接構成の
残り約 1,500 回とその他約 634 回が残る。

## 検証

- 検証区分：Tier FULL（由来の構成方法を変えるため）。
- ホスト：開発ホスト1。コーパス manifest 照合 4,155/4,155 OK
  （manifest SHA-256
  abfec87ab0c29dc088d4e93f25cf563849b062d3eb15eeaa5d54f8d08b8341c3）。
- 対象クレート：syntax 126、minify 697（新規 4 件含む）、build 126
  全成功。ワークスペース全件 50 スイート 1,203 件全成功。
  `cargo xtask check`・fmt・clippy（syntax・minify lib、警告 0）通過。
- コーパス FULL（base は分岐元 b5ce8ab5、head は自ブランチ）：
  4 条件、16,620 行、欠落 0。両 TSV はバイト同一（SHA-256
  304e2da92b0cdb8fcf0e4d73f14f577b6c42bba393c7e6241099663c9f6e1e27）。
  比較結果は全条件で identical=4155、expl-only=0、map-core=0、hard=0、
  missing=0。status 対は (ok, ok) 4127 件、(error, error) 28 件で一致。

## 付記：形状テストの期待値について

当初、comparison・and の形状テストは `r=true`／`r=false` を期待して
失敗したが、これは Printer の意図的な Bool 短縮形（`true`→`1>0`、
`false`→`1>2`）によるもので、pass の出力自体は正しかった。
ベースライン（変更前）でも同一の出力になることを確認済みであり、
テストの期待値を短縮形に修正した。コードの不具合ではない。
