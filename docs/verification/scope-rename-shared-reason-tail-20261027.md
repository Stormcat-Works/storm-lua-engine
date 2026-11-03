# scope-renaming の reason tail 共有による追跡コスト削減（2026-10-27、未公開）

scope-renaming（`scope_rename`）は、改名するノードごとに由来本体と
名前由来の項目へ `scope-renaming` の理由を記録する。その際、理由テール
のノードを由来ごとに毎回新規確保していた。改名全体で1個ずつ作った
共有テールノードを seed 由来へ指す方式に変更し、記録内容を変えずに
追跡コストを削減した。生成 Lua・候補選択・schema1 のマップ本体は維持する。
Map 比 1.2 の目標は未達のままであり、この変更はその達成を主張しない。

## 原因

追跡差の内訳切り分け（合成入力 dense-normal、計数アロケータ付き
ハーネス、11 反復中央値）では、scope-renaming 単独の追跡差
5,665 回（無追跡 5,498 回→追跡 11,163 回）のうち、改名ノード
961 件に対する内訳は次のとおりだった。

- 基礎（capture・クローンの COW 等）：860 回。
- スロット格納（import＋`Arc::new`）：961 回（1 回／ノード）。
- 本体由来の理由テール：961 回（1 回／ノード）。
- 名前由来：2,883 回（名前ベクタの複写 1,922 回＝バッファ＋新 Arc
  ボックスの 2 回／ノード、理由テール 961 回）。

seed 由来の理由テールは、previous リンクが空で理由が同一であるため、
全ノードで同一のテールノードを共有できる。共有ノードへの後続 push は
`Arc::make_mut` が共有時に複写するため、他の由来へ漏れることはない。
tail を持つ由来は従来 push へ fallback し、retained 理由中の pending
Rewrite の変換も従来手順と同一の展開になる。

## 変更

- `crates/storm-lua-syntax/src/deferred_origin.rs` に
  `SharedTail`（previous 空の1ノード）と
  `StoredOrigin::derive_in_place_code_shared`／
  `derive_action_code_shared` を追加した。tail が空の由来にだけ共有
  ノードを指し、それ以外は従来 push を行う。公開 API のシグネチャは
  変えていない。
- `crates/storm-lua-syntax/src/node_arena.rs` に
  `RenameSharedTails`（Rewrite／Rename の2ノード）と
  `NodeArena::finish_rename_with_shared` を追加した。
  `finish_rename_with_code` は共通 inner へ委譲し、挙動は変わらない。
- `crates/storm-lua-minify/src/scope_rename.rs` の `apply_renames` は、
  追跡時のみ共有テールを1回作り、2箇所の finish 呼出を共有版に変更
  した。無追跡側は追加確保なし。候補選択・改名対象・生成コードには
  手を入れていない。

## 測定

合成入力 dense-normal、11 反復中央値。無追跡側は変更前後で完全同一。

- scope-renaming 単独：追跡側 11,163 回→9,243 回、追跡差
  5,665 回→3,745 回（約 −34%）。追跡差バイト数は
  631,776 B→401,376 B。
- api-alias 全体：追跡側 128,614 回→126,690 回、追跡差
  13,083 回→11,159 回（約 −15%）。saved=7127・コード長・ノード数は同一。
- 時間差は負荷変動が大きく、時間改善は主張しない。

## 検証

- 回帰テスト：node_arena の新旧一致テストに共有版を追加し、
  由来・名前由来・シリアライズ済み AST・生成コードの一致を検証。
  deferred_origin に、seed／tail あり／retained Rewrite 変換の各位置で
  共有版と従来版の展開一致テストを追加した。
- 対象 3 クレート（syntax 104・minify 441・build 126）とワークスペース
  全件（50 スイート）が成功。xtask check・fmt・clippy（警告 0）を通過。
- 実コーパス全件（4,155 ファイル×4 条件）の比較では、成功／失敗
  （4,127／28）・生成コード・Map 中核・正規化済み Map 全体のすべてが
  差分 0 件で、両 TSV はバイト同一だった。

## 残り

名前ベクタの複写（1,922 回／改名バッチ）が次の主因として残る。
`NodeOrigin.names` の表現変更で削れる見込みがあるが、NodeOrigin 複製
全体へ波及するため、別サイクルでの検討とする。
