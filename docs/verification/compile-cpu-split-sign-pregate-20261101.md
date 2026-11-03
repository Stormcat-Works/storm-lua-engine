# コンパイル CPU: split-sign 解析前 necessary-condition ゲート（2026-11-01、w7 第96サイクル）

## 変更

- 実装: `64996f9`（`crates/storm-lua-minify/src/passes/split_sign.rs` のみ、分岐元 `0f59014`＝コード先端 `0bd5dff`）
- 内容: パス入口で入力 Ast 全体を走査し、復号値が `"max"`／`"min"` の `Str` キーを持つ `Index` ノードが1つも存在しない場合、`resolve`・`EffectAnalyzer` 構築・blocks 変換・二乗段ゲート走査を丸ごと省略し、入力に一切触れず、現行実装の無発火時と逐語一致する `PassResult`（`saved=0`、`eliminated=0`、`squared=0`）を返す。パターンが存在する場合は従来経路をそのまま実行する。新規単体テスト5件（パターン検出／不在、解決⇒パターンの前提テスト、ルート代入エイリアス経由の発火回帰、ローカルエイリアス非発火の回帰）。

## レバー選定の根拠（第95サイクル census）

- split-sign は std-nomap で起動 50,129 回、段合計 ≈4.19 秒: `EffectAnalyzer::new` 1.694 秒（40.4%）、`transform_blocks` 1.284 秒（30.7%）、`measure_size`（初回）0.497 秒（11.9%）、`resolve` 0.464 秒（11.1%）、二乗段ゲート walk 0.230 秒（5.5%）。`eliminated` は全コーパス 0 回、二乗段ゲート通過は 71 回のみで、≈93.6% が実質不発火のまま無条件に払われていた。
- 第246ラウンドのゲートは二乗段側（blocks 段の出力に対する判定）のみで、resolve＋解析＋blocks 変換のコストは残っていた。本ゲートはそれら全段の前段に置く。

## リゾルバ監査（健全性前提）

コード読みによる確認:

- split-sign の書換えは全て `call_operation` の `"max"`／`"min"` 成功を経由する。blocks 段は `transform_block` の `(Some, Some)` ペアでのみ除去し、二乗段は `squared_rewrite_possible` 自体が両演算の成功を要求する。`math.abs` 別名走査と `NumericProof` は二乗段ゲート通過後にのみ構築される。
- `EffectAnalyzer::resolve_builtin_reference`（`builtin_reference`）がドットテキストを構築する形は2つだけである: (1) `Name` 節点 → その束縛の証明済み別名、(2) `Index(obj, Str key)` → `obj` の再帰テキスト＋`"."`＋`borrowed_member(key)`（キーの復号値）。計算キー・呼出し結果など他の形は全て `None` を返す。
- 別名が生成される箇所も2つだけである: (a) `seed_root_occurrences`——API root 名（`VEHICLE_ROOTS`、ドットを含まない素の識別子のみ）と綴りが一致する未書込みグローバルへ、その名前自体を別名として付与する。(b) ルートブロック `Assign` に対する固定点伝播——RHS が `Name`（既存別名のコピー）か `Index(Name, Str)`（base 別名＋`"."`＋member）の場合のみ。`mutated_builtin_roots` と書込み数による retain フィルタは別名を除去するだけで、生成はしない。テスト専用の `reference_new` も同構造である。
- したがって、ある節点が `"math.max"` に解決されるなら、その節点自体が復号値 `"max"` の `Str` キーを持つ `Index` であるか、別名 `"math.max"` の保持者が存在する。後者の最初の保持者は、種（ドットを含み得ない）からも `Name` コピー（先行保持者が必要）からも生じ得ず、`Index(Name, Str)` 形の RHS から `base.member` として生成されたものでなければならない——つまり当該チャンク内に復号値 `"max"` の `Str` キーを持つ `Index` が存在する。`"min"` も同様である。`_ENV` 経由の綴りは base が接頭辞として残る（例 `"_ENV.math.max"`）ため裸の `"math.max"` にはならず、計算キーは `builtin_reference` が `Node::Str` キーを要求するため解決されない。

実証（反例探索）: 単体テスト `resolution_to_max_or_min_implies_pattern` で、直接形・括弧形・エスケープ形（`math["m\97x"]`）・ルート代入エイリアス（連鎖含む）・メンバー代入・ローカル遮蔽・組込み再定義・動的キー・`_ENV` 経由など20ソースについて実アナライザで全節点を走査し、`"math.max"`／`"math.min"` へ解決される節点が存在する場合は常にパターンが存在することを確認した。併せて、ルート代入エイリアス（`f=math.max`／`g=math.min`）経由では blocks 段・二乗段の双方が base と同じく発火すること、ローカルエイリアス（`local m=math`）では解決されず発火しないことを回帰テストで固定した。

## 出力不変性（Tier INVARIANT）

- パターン不在の場合、上記監査により `call_operation` は全節点で失敗し、blocks 段の除去は 0、二乗段ゲートは false になる。現行実装はこの場合、入力と値・origin 同一の複製（blocks 段出力）を破棄し、入力 `Ast` に触れず `saved=0`・`eliminated=0`・`squared=0` を返す（等しい値での `rewrite` が origin 保持の no-op であることは node_arena の明示仕様）。ゲートの早期 return はこの `PassResult` と逐語一致する。
- `resolve` と `EffectAnalyzer::new` は入力の clone に対して作用するため、それらの省略は入力 `Ast` への観測可能な効果を持たない。解析対象の clone は入力と節点・文字列が同一であり、監査の構文条件はそのまま移る。

## 決定論カウント（プローブ、std-nomap 全コーパス、逐次単一プロセス）

- 呼出 50,129 回（第95 census と一致＝プローブ忠実性）。うちパターン存在 12,139 回（24.2%）、ゲート省略 37,990 回（75.8%）。プローブ出力行（std-nomap）は base／head でバイト一致。
- ゲート走査自体の累積は 0.29 秒（50,129 回）。第95 census の段時間に省略率を適用すると、resolve＋analyzer＋blocks＋二乗段ゲート ≈3.67 秒 × 75.8% ≈ 2.78 秒が消滅し、無発火時 return の再計測分がこれに加わる。逐次 A/B の壁時計（base 131.7s vs head 132.5s、単一測定・非交互）は共有ホスト（compat 稼働中）のノイズ帯であり、この会計と矛盾する主張はしない。

## 検証

- ワークスペーステスト 1,675 件全通過（base の 1,670＋新規 5）。clippy 警告集合は base／head 同一（本変更ファイルに無し）。対象ファイル単体の `rustfmt --check` 無指摘。
- Tier FULL 相当ゲート（合格条件として実施）: 正本 corpuscheck（SHA-256 `3c07a763…`）の batch で base=`0bd5dff`／head を全コーパス比較し、16,620 行・fallback 0、両側とも SHA-256 `4a98d0939f13cb4f3f888393087bbb64f8945cf3e5a9154e9bb7680ade733028` でバイト一致（TSV ファイル同士も `cmp` 一致）。
- .us2 測定（submit 1 回・全入力同梱、固定 208 ファイル・全4構成、batch プロセス壁時間、交互 3 rep）: base 中央値 23,333,583µs（18,854,510／23,333,583／23,466,454）→ head 中央値 22,795,320µs（22,565,631／23,089,588／22,795,320）で −2.31%。ただし base rep1 は他全 rep から大きく外れた外れ値（ジョブ開始直後の値）で、全 rep の範囲は重なる。rep2／rep3 のペアでは head が一貫して base を下回った。行・status（ok 206／err 2）・フィンガープリント（`c56b9d735ede0d7f23df38171cd82a3880f8dd2459a8796a85dab858bfca4bbf`——ゲート TSV の当該部分集合から独立に再計算した期待値と一致）は全 rep・両側一致し、rep ごとのソート済み行も両側一致した。壁時計の主張はこの範囲に限定し、採用根拠は決定論的な段作業の消滅に置く。提出経路は .wsl→.us2 直通 SSH が publickey 拒否のため VM 経由（第93・94と同状態）。
- 本ラウンドは Tier INVARIANT のため FULL-VERIFY-TIP は `0bd5dff` のまま（第258ラウンドの後の INVARIANT 1件目）。ただし上記のとおり FULL-VERIFY-TIP を base とする FULL 相当比較自体を実施し、累積ハッシュのバイト一致を確認済みである。
