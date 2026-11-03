# Current status

## v0.4.0 ローカル公開候補（2026-11-02、未公開）

正確性修正群（S1〜S11・S14、ロード違法ソースのfail-fastを含む）、Source Map schema-2、コンパイル性能改善を統合した公開候補。版番号を0.4.0へ更新し、現行コーパス全件（4,155ファイル×4条件）の最終FULL検証では整列TSVが第300・第301ラウンドとバイト同一（SHA-256 `42ce28ca…`）、ワークスペーステスト1,775件合格、独立回帰パック114/114を確認した。`cargo xtask check`のアーキテクチャ検査は、第299ラウンドで入った`storm-lua-build`→`storm-lua-vm`依存を規則側が未許可のため失敗する（ベースでも同一の既存不整合であり、本準備の退行ではない。公開判定の入力として[候補記録](docs/verification/release-candidate-0.4.0.md)に明記）。公開（push・タグ・GitHub Release・npm）は管理者の明示承認待ちで、本候補は公開済みではない。

## API エイリアス解析の global／localized 間共有（2026-10-30、未公開）

target モードで joint-api（global 形）と localized の両レイアウトが
同じ phase ツリーに実行する API エイリアスパスの第一解析
（origin なし clone・resolve・EffectAnalyzer・builtin 使用 scan）を、
レイアウト構築で1回だけ構築して共有するようにした。解析は
`(source, root)` の決定的純粋関数であり、共有値は各呼び出しが
自前計算していた値と同一のため、選択・勝者・生成コードは変わらない。
localized 基底が relaxed 合成で差し替わる場合と LICM 基底は従来どおり
自前構築する。決定的計数で解析構築はコーパス全周あたり
26,781 回から 18,410 回へ減少（共有可能な組 8,371 件と一致）。
検証区分は Tier FULL：目標なしコーパス 16,620 行バイト同一、
target-delta（slack32）も全行バイト同一で遷移 0 件。
開発ホスト3 での target コンパイル時間は当初基底で
−0.25%〜−0.91%、第183ラウンドへの rebase 後の再測定では
ノイズ域内（勝者同一）。詳細は
docs/verification/api-alias-analysis-sharing-20261030.md。

## signed-expression-factoring のキャリア定義が代表出現ノードを共有する不具合の修正（2026-10-30、未公開）

signed-expression-factoring が、因数分解キャリアの定義文の右辺に代表出現のノードをそのまま挿入していたため、代表出現の所属文が書換範囲外（範囲前の代入文の右辺）にある場合に、その部分木が二重の親を持っていた。後段の available-expression-reuse がノード ID 経由で書換えると両方の親が書き変わり、`y = h/3` のような既存代入が `y = y` になって定義が失われ、生成コードが未代入グローバルを読んだ（コーパス e4727bd0 で opt のみ実行時エラー）。定義文の右辺には scratch アリーナから複製した fresh な部分木を使うよう改め、回帰テスト 2 件を追加した。検証記録は `docs/verification/signed-factoring-carrier-definition-copy-20261030.md` を参照。
## immutable-table-flattening のエラーを起こす値式の欠落の修正（2026-10-30、未公開）

immutable-table-flattening の純粋性判定 `table_literal_pure` が、リテラル同士の二項／単項式を「純粋」とみなしていたため、`{a = 1 + nil, b = 2}` のような、フィールド値の評価自体が実行時エラー（nil への算術・連結、整数のゼロ除算・剰余、非数文字列への算術、nil の長さ・単項マイナス・比較など）を起こすコンストラクタが、読み取りが全て静的な場合にコンストラクタごと削除され、エラーが消えていた（safe／smallest の双方、名前／配列／静的キー付きフィールドで再現）。複合式（Bin／Un）を純粋な値とみなさないよう変更した。定数畳み込みが有限と証明できる複合式は平坦化より前にリテラルへ具体化されるため、残る複合式は安全性を証明できないものだけであり、実用上の損失は非有限値（`0 / 0` 等、値としては合法だが落とさなくなる）などの稀な形に限られる。検証記録は `docs/verification/immutable-table-raising-value-20261030.md` を参照。

## 部分複合レイアウト（S1／MR フル＋SMC partial）の追加（2026-10-30、未公開）

target モード限定レイアウトに、S1／MR（フル）の上に SMC の
first-group-only 部分変種を重ねた部分複合レイアウト
（`joint-api-local-s1-smc-partial`／`joint-api-local-mr-smc-partial` と
licm 側 2 種）を追加した。フル複合が SMC グループを 2 件以上
キャッシュした場合のみ提供し、速度順位は自らのフル複合の直下に
置いた（既存レイアウト間の相対順序は不変）。`layouts_per_job` は
16 から 20 へ引き上げた。
単体テストは SMC 28 件が成功（部分複合の逐次適用 2 件を追加）。
検証区分は Tier FULL：目標なしコーパス比較は 16,620 行バイト同一、
target モード差分（slack32）は勝者変更 1 件
（licm-mr → licm-mr-smc-partial、+6 文字・目標超過 0）、
slack128 では 0 件（部分複合の価値はタイトな目標に集中）。
開発ホスト3 での採用後測定では、遷移形を再現する合成入力で
コンパイル時間は不変、実行時間はノイズ域（減速の証拠なし）。
詳細は docs/verification/partial-composite-smc-layouts-20261030.md。

## トークン表の複数ソース対応と混在形（部分保存）（2026-10-30、未公開）

schema-2 トークン表の残件2件を一設計パスで解消した。(1) ゲートの単一
ソース条件を外し、複数ソースではレコード第2要素に source index を持つ
レイアウト（完全 copy 5要素・リネーム6要素）で出す。レイアウトは source
数で切り替わるため単一ソースの既存 Map はバイト単位で不変で、
schemaVersion は 2 のまま。(2) compose に部分保存を追加し、変換後も
単純形を保つ mapping は origin を複製せず保持する。一般形エンコーダは
残余 origin が256件以下かつ単純 origin 件数以下の混在入力で、単純部分を
トークン表へ移し残余だけを origins／mappings に残す混在形を出す。
validate は残余 mapping がトークン展開の未知ギャップ内に収まることを
検査して差し込み、返却する拡張はトークン表を消費済みとして正規化する。
validate／validate_for_code の検査は緩めていない。生成コードとリネーム
自体には触れていない。
実測（base 07fad008f 比）：複数モジュール project（勝者3件をモジュール
化、sources＝4）は 49,485,112 B → 6,594,895 B（−86.7%）、lifeboat 成功
3件の合計は 49,389,389 B → 6,343,121 B（−87.2%、残余 origins は
テンプレート由来の69件のみ）。単一 compile と単一モジュール project は
不変（project 3件合計 4,581,213 B で第167ラウンドと同一）。
検証区分は Tier FULL：16,620 行すべて identical（expl／core／hard／
missing 0）。ワークスペース全件 1,370 件成功、対象クレート 134 件成功。
詳細は docs/verification/token-table-multisource-mixed-20261030.md。

## immutable-table-flattening の非リテラル値読み取りの束縛欠落の修正（2026-10-30、未公開）

immutable-table-flattening の使用走査が「静的読み取りは全て置換できる」と仮定していたが、実際の書き換えが置換できるのはスカラーリテラル（と末尾キーの欠落→nil）だけだった。有限の複合式は定数畳み込みが先に具体化するため問題にならないが、非有限の値（`0 / 0` の NaN、`1 / 0` の inf）は畳み込みが具体化を拒否する設計のため式のまま残り、`{a = 0 / 0, b = 2}` の `t.a` 読み取りが置換されないまま束縛だけが削除され、nil グローバルの読み取りになって実行時エラー化していた（欠落した中間キー `t.inner.b` も同型）。走査側に書き換えの歩行を鏡写しした `static_chain_substitutable` チェックを追加し、置換できない読み取りがあるテーブルは平坦化しないようにした。未読の非リテラルフィールドは従来どおり落とせる。検証記録は `docs/verification/immutable-table-nonliteral-value-read-20261030.md` を参照。

## immutable-table-flattening の nil／NaN コンストラクタキー欠落の修正（2026-10-30、未公開）

immutable-table-flattening が、計算キー付きコンストラクタを「純粋なリテラル」とみなす条件にキーの有効性を含んでいなかったため、`{[nil] = 1, b = 2}` や `{[0 / 0] = 1, b = 2}` のような、評価時に `table index is nil`／`table index is NaN` の実行時エラーを起こすコンストラクタが、読み取りが全て静的な場合にコンストラクタごと削除され、エラーが消えていた（safe／smallest の双方、伝播・埋め込み経由のローカルキー・nil 引数の特殊化でも再現）。`table_literal_pure` に `key_is_valid_table_key` ガードを追加し、計算キーが静的キー形式（文字列／整数／非 NaN 数値リテラル）または真偽値リテラルの場合にのみ平坦化可能とした。検証記録は `docs/verification/immutable-table-nil-nan-key-20261030.md` を参照。

## 速度パス（S1／MR／SMC）の初回解析共有（2026-10-30、未公開）

target モードのレイアウト構築段で、同一基底 AST に対する速度パス
5 回（S1／S1 partial／MR／SMC／SMC partial）が個別に再構築していた
`resolve()`＋`EffectAnalyzer`＋`mutated_builtin_roots` を、基底ごとに
1 回の構築へ共有した（新規 `passes::speed_analysis::SpeedAnalysis`）。
解析は arena 内容に対する決定的純粋関数のため勝者・生成コード・
Source Map は構成上不変。等価性ユニットテスト 3 件を追加。
検証区分は Tier FULL：コーパス比較 16,620 行バイト同一。
効果は 開発ホスト3 で target コンパイル時間の −0.3〜−1.0%
（小入力）／約 −0.2%（大入力）と小さいが一貫して非負で、回帰なし。
詳細は docs/verification/speed-analysis-sharing-20261030.md。

## constant-folding の積極的変換 3 件の値域ガード追加（2026-10-30、未公開）

ビットテストの剰余形変換（`(x & mask) == 0` の剰余比較化）・
因数分解形（`x + (-x * t) → x * (1 - t)`）・逆数乗算の除算化
（`x * (1 / d) → x / d`）が、被演算子の値域の証明なしに発火し、
小数／inf／NaN／整数範囲外の値に対する `&` の実行時エラーの
消去、inf での NaN と -inf の取り違え、不正確な逆数での
1 ulp 差による等価比較の反転を起こしていたバグを修正した。
各変換に値域ガード（整数値であることの証明・有限数値
リテラル・2 の冪分母）を追加し、回帰テスト 4 件を追加した。
検証区分は Tier FULL：16,620 行・欠損 0・ステータス変化 0・
写像中核 0・説明系のみ 0。生成コード変化は 10 ファイル
（合計 +92 文字）で、変化 10 件の発散分類は base／head で
完全一致（新規発散 0）。
詳細は docs/verification/aggressive-transform-value-classes-20261030.md。

## constant-folding の積極的恒等式（x * 0 / 0 * x / x - x）の修正（2026-10-30、未公開）

積極モードの恒等式 `x * 0 → 0`、`0 * x → 0`、`x - x → 0` が、相手の
被演算子が有限数であることの証明なしに発火し、inf／NaN の場合は
NaN であるべき結果を 0 に変え、非数文字列の場合は実行時エラーまで
消していたバグを修正した。発火条件に、捨てる側の被演算子が有限の
数値リテラルであることを追加した。回帰テスト 3 件を追加。
検証区分は Tier FULL：16,620 行・欠損 0・ステータス変化 0・
写像中核 0・説明系のみ 0。生成コード変化は 71 ファイル（全条件
共通）で、恒等式で消えていた式が残る分の伸び（合計 +4,700 文字）。
変化 71 件の差分実行では、発散 26 件が base／head の両側で同一で
あり、本修正による新規発散・回復は 0 件だった。
詳細は docs/verification/zero-product-self-subtraction-fold-20261030.md。

## SMC 複合レイアウト（S1＋SMC・MR＋SMC）の追加（2026-10-30、未公開）

target モード限定レイアウトに、SMC を S1／MR レイアウトの上に重ねた
複合レイアウト（`joint-api-local-s1-smc`／`joint-api-local-mr-smc` と
licm 側 2 種）を追加した。両パスが発火したときにのみ提供する。
速度順位は両構成要素より上に置き、既存レイアウト間の相対順序は不変。
`layouts_per_job` は 12 から 16 へ引き上げた。
単体テストは SMC 26 件が成功（複合の逐次適用 2 件を追加）。
検証区分は Tier FULL：目標なしコーパス比較は 16,620 行バイト同一、
target モード差分は勝者変更 3 件（s1 → s1-smc 2 件・licm-s1 →
licm-mr-smc 1 件）・合計 +20 文字・目標超過 0。遷移ペアの実行同一性は
両側同一のホスト依存エラーで対応することを確認。開発ホスト3 での
採用後測定は draw −5.24%（15/21 対）、tick は .us で +35.39% と出たが
二峰性の干渉ノイズと判断し 開発ホスト2 で再測定したところ +0.83%
でほぼ同等であり、減速の証拠はない。
詳細は docs/verification/composite-smc-layouts-20261030.md。

## final-dead-store-elimination の return 前ストア欠落の修正（2026-10-30、未公開）

分岐内の早期 return の直前のグローバル代入が、フォールスルー経路上の
後の上書きで dead と判定され除去されるバグを修正した。return は関数を
抜けるため、関数終了時生存（非固定グローバル＋関数間捕捉束縛）で判定
するよう後方生存解析を改めた。回帰テスト 3 件を追加。
検証区分は Tier FULL：16,620 行・欠損 0・ステータス変化 0・写像中核 0・
説明系のみ 0。生成コード変化は 9 件のみで、保持されたストア分の伸び
（default-nomap で +2〜+43 文字）であり、変化 9 件の修正後差分実行では
plain／opt／safe のトレースが全件一致した。
詳細は docs/verification/final-dead-store-return-20261030.md。

## リネーム済みトークンの Map 表現の統一と compose 適格性保存（2026-10-30、未公開）

R137 のリネーム済みトークンの Map 表現の未設計を一設計パスで解消した。
字句勝者の Source Map が自前の validate を通らなかった原因は、トークン表の
ワイヤ形が生成端を持たず元綴り長から生成端を算出していたため、短縮リネーム
済みトークンの直後で「token table is not in generated order」になっていたこと。
materialize 済み字句ライタはリネーム済み mapping に copied を書き、トークン表
レコードは 4 要素のままであった。変更は生成コードとリネーム処理自体には触れず、
リネーム済み mapping の copied を null とし、トークン表レコードをリネーム済み
のみ第 5 要素（generated_end）付きとし、simple_copy ゲートをリネーム形へ拡張し、
単純形の compose には origin のインプレース再マッピングによる高速路を加えた
（非単純入力は従来経路とバイト同一）。validate の検査内容は変更していない。
回帰テスト 5 件を追加した。
検証区分は Tier FULL：コーパス比較は 16,620 行で欠落 0 件、成功／失敗・生成
コード・Map 中核すべて完全一致し、差分は字句勝者 5 件の map 構成 2 条件のみ
（計 10 行）のトークン表ワイヤ形の変化そのものである。字句勝者 5 件はすべて
単一 compile の validate が通過へ。project 経路では 3 件がトークン表化し
Map 合計は約 49MB から約 4.6MB へ（約 44.8MB 削減）。lifeboat 経路は 69 件の
非単純 origin（テンプレート由来の変換・理由を持つ origin）を保持するため正しく
従来レイアウトに残り、validate は通過する。複数モジュール project はゲートの
単一ソース条件により引き続き従来レイアウト。
詳細は docs/verification/renamed-token-map-representation-20261030.md。

## SMC 部分変種（S1-partial 同型）の追加（2026-10-30、未公開）

直線メンバ読取 CSE（SMC レイアウト）に、S1-partial と同型の
first-group-only 部分変種（`joint-api-local-smc-partial` /
`joint-api-local-licm-smc-partial`）を追加した。全グループ版が目標を
超過して local へフォールバックするファイルを回収する。
部分変種は全グループ版のキャッシュ数が 2 以上のときにのみ提供し、
速度順位は全グループ版と基底レイアウトの間に置く。
サブグループの確定順序を初出順に決定的にし、部分変種の選択を安定させた。
`layouts_per_job` は 8 から 12 へ引き上げた（1 ジョブあたり最大 12 布局。
従来値 8 は SMC 追加時点で既に実数 10 を下回っていた）。
単体テストは SMC 24 件が成功（部分変種 3 件を追加）。
検証区分は Tier FULL：目標なしコーパス比較は 16,620 行バイト同一、
target モード差分は勝者変更 6 件（local → partial 5 件・licm → partial 1 件）・
合計 +56 文字・目標超過 0。第164ラウンドでフォールバックした 4 件は
すべて部分変種へ遷移し、回収された。
開発ホスト3 での採用後測定では、合成入力の変更勝者の実行時間が
tick −21.92%・draw −15.39%（いずれも 21 対の交互測定ペア中央値）。
詳細は docs/verification/straightline-member-read-partial-20261030.md。

## constant-folding の and／or 畳み込みによる多値切り詰め喪失（2026-10-30、未公開）

`and`／`or` 式は Lua では常にちょうど 1 値を返すが、constant-folding
パスは左辺が既知のリテラル（伝播した名前を含む）の場合にノード全体を
選択した子で置換しており、子が裸の呼出または `...` の場合に 1 値調整が
失われていた。`local a, b, c = true and pair()`（`pair()` は 3 値を返す）
では plain の `(7, nil, nil)` に対し最適化後は `(7, 8, 9)` になった。
同型は `false or pair()`・`nil or pair()`・`"x" and pair()`・伝播名・
比較畳み・vararg・呼出最終引数・テーブル最終要素・return 最終式でも
発火し、safe モードでも発火した。修正は `literal_folding.rs` のみで、
fold 段と exact simplify 段の両箇所で、選択した子が Call または Vararg
の場合に括弧で包んで置換する（既に括弧化された子はそのまま）。回帰
テスト 3 件を追加（いずれも修正前は失敗し、修正後は成功）。検証区分は
Tier FULL のため、分岐元との全件コーパス比較（4,155 件×4 条件）を実施
した（結果は下記の検証記録を参照）。
詳細は docs/verification/and-or-fold-multivalue-20261030.md。
## SMC の Index 書込み分割のキー単位精密化（2026-10-30、未公開）

直線メンバ読取 CSE（SMC レイアウト）のセグメント分割を精密化した。
Index 書込み文はセグメント内にバリア文として残り、同一 `(base, key)` への
書込みはそのグループのみ、動的キーによる書込みはその基底の全グループ、
非 Name 対象からの書込みは全グループを閉じる。バリア文の値の読取は
書込み前側に属する。証明位置はセグメント先頭のままで保守性は不変。
単体テストは SMC 21 件が成功（新規 6 件・更新 1 件）。
検証区分は Tier FULL：目標なしコーパス比較は 16,620 行バイト同一、
target モード差分は勝者変更 8 件・合計 −60 文字・目標超過 0。
smc → local の 4 件は精密化で増えたキャッシュによる目標超過の
フォールバックで、部分変種（サブセット選択）が将来項目。
開発ホスト3 での採用後測定では、合成入力の変更勝者の実行時間が
tick −12.54%・draw −19.77%（いずれも 21 対の交互測定ペア中央値）。
詳細は docs/verification/straightline-member-read-perkey-barrier-20261030.md。

## 直線メンバ読取 CSE（SMC レイアウト）の実装（2026-10-30、未公開）

target モード限定の新レイアウト（`joint-api-local-smc` /
`joint-api-local-licm-smc`）として、直線コード上の反復メンバ読取キャッシュ
を実装した。セグメントは 1 ブロック内の `local`／代入文の最大ラン＋末尾の
呼出なし `return` で、呼出・制御フロー・Index 書込みが区切りになる。
基底の証明は MR の証明をそのまま再利用（書込み 0 の単一代入 local または
書込み 1 回のグローバル、いずれもテーブル構築子初期化子・非エスケープ・
goto/label なし）。速度順位は MR の下・基底レイアウトの上に置く。
検証区分は Tier FULL：目標なしコーパス比較は 16,620 行バイト同一、
target モード差分は勝者変更 14 件（すべて local → local-smc）・
合計 +146 文字・目標超過 0。開発ホスト3 での採用後測定では、
合成入力の target コンパイル壁時間 +0〜+1ms に対し、変更勝者の実行時間が
tick −12.58%・draw −12.92%（いずれも 21 対の交互測定ペア中央値）。
詳細は docs/verification/straightline-member-read-cse-20261030.md。

## screen-loop 合成の試行時由来付与の繰延べ化（2026-10-30、未公開）

ordered-screen-loop-synthesis パスが、失敗試行のたびに共有 target
arena へ由来をその場で付与していたコストを、試行時由来を `TrialOrigins`
（op 列＋単一 values arena）へ記録し、採用された場合のみ同じ呼び出し・
同じ順序・同じ source で適用する繰延べ化で削減した。失敗試行は
truncate され plan は破棄される。plan は文ループ外で 1 個を再利用する。
draw 入力の同パス追跡差は 102,532 回→3,176 回（−96.9%）で、無追跡
123,041 回・コード長・ノード数は不変。回帰テスト 1 件を追加した。
検証区分は Tier FULL のため、分岐元との全件コーパス比較（4,155 件×
4 条件、16,620 行すべてバイト同一、成功／失敗・生成コード・Map 中核
すべて一致）を実施した。時間改善は主張しないため .us 測定はなし。
詳細は docs/verification/screen-loop-trial-origins-deferral-20261030.md。

## constant-argument-specialization の多値最終引数切り詰め（2026-10-30、未公開）

constant-argument-specialization パスが、引数の書かれていないパラメータを
Lua の nil とみなして特殊化していたが、呼出の末尾引数が多値を返す式
（呼出または `...`）の場合、書かれていないスロットは nil ではなく展開値で
埋まるため、この推論は誤りだった。`take3(pair())`（`pair()` は 3 値を返す）
では第2・第3引数が nil 化され、`(7, 8, 9)` が `(7, nil, nil)` になった。
`take4(0, pair())` の形は safe モードでも発火した。修正は
`inline_functions.rs` のみで、調べる位置が書かれた引数列の長さ以上であり
末尾引数が Call または Vararg である場合、その呼出は nil と一致しないと
した（括弧化された呼出は 1 値化されるため従来どおり）。回帰テスト 4 件を
追加（多値系 3 件は修正前は失敗し、修正後は成功）。検証区分は Tier FULL
のため、分岐元との全件コーパス比較（4,155 件×4 条件）を実施した（結果は
下記の検証記録を参照）。
詳細は docs/verification/constant-argument-specialization-multivalue-20261030.md。
## ループ内反復メンバ読取ホイストの for 変数基底への拡張（2026-10-30、未公開）

target モード限定の MR レイアウト（`joint-api-local-mr` /
`joint-api-local-licm-mr`）に、for 変数基底の反復メンバ読取キャッシュを
折り込んだ（新規レイアウトなし）。for 変数にはテーブル証明がないため
プレヘッダは使わず、ループ本体ブロック内に初回読取文の直前へ
`local c = B.K` を挿入する。初回読取が head 位置にあり、その文で初回読取
より前に完了する呼出がない場合に限り適用し、初回読取のエラー挙動を保存
する。基底の証明（書込み 0・エスケープなし・goto/label なし・ループ
バリア・同一キー書込みのグループ失格）は従来と同型に保守的。検証区分は
Tier FULL：目標なしコーパス比較は 16,620 行バイト同一、target モード差分は
勝者変更 10 件（すべて local → local-mr）・合計 +90 文字・目標超過 0。
開発ホスト3 での採用後測定では、合成入力の target コンパイル壁時間
+1ms に対し、変更勝者の実行時間が tick −22.02%・draw −12.97%
（いずれも 21 対の交互測定中央値）。
詳細は docs/verification/loop-member-read-for-base-20261030.md。

## target モード satisficing の継続探索化（2026-10-30、未公開）

2026-10-30 のユーザー決定（選択肢 B、source 適合を含む）に従い、target
モード（OBJ-2）では source／base／structural のチェックポイントで適合候補が
現れても探索を打ち切らず、速度レイアウトまで評価して既存の順位付けで
最速の適合候補を選ぶようにした。適合する最適化候補がない場合の
フォールバックは従来通りで、目標なし（OBJ-1）は不変。実装は
`search.rs` の checkpoint マクロから適合時早期復帰を除去し、source 適合時の
identity 短絡（origin 表省略パース）を除去して常に `parse_for_search` する
ことで、勝ち得る最適化候補の tracked origins を保証した。旧契約のテスト
7 件を新契約に更新した。検証区分は Tier FULL のため、分岐元との全件
コーパス比較（4,155 件×4 条件、TSV バイト同一、成功／失敗・生成コード・
Map 中核すべて一致）と、累積比較（マーカー ac670ed 対 head、identical
4,148・hard 7＝先行ラウンドの効果）を実施した。target モード差分（目標＝
目標なし勝者＋32）は勝者変更 1,098 件・合計 −6,721 文字・目標超過 0。
開発ホスト3 での採用後測定では、合成入力の target モードコンパイル壁時間
が +1〜+3ms、変更勝者の実行時間が draw −9.37%・tick −50.25%（いずれも
21 対の交互測定中央値）。
詳細は docs/verification/target-satisficing-option-b-20261030.md。

## available-expression-reuse の展開位置・文位置ガード（2026-10-30、未公開）

available-expression-reuse パスが、直線ブロック内で既に計算済みの式を
キャリア（単一ローカル）へ置換する際、置換先が多値を展開する位置か
どうかを区別していなかった。キャリアは呼出の第1返値しか保持しないため、
`local c = pair()` の後に `local t = {pair()}` と書くと `#t` が 2 から 1
に、呼出最終引数 `take(pair())` では第2引数が nil に、文位置の呼出
`pair()` では最適化コンパイル全体が失敗した。修正は
`available_expressions.rs` のみで、展開位置（テーブル最終 Arr・呼出最終
引数・Return 最終式・名前数＞式数の Local／Assign 最終式・式数＜3 の
汎用 for 最終式）および文位置では Call ノードのキャリア置換を行わず、
元の呼出を残す。名前数＝式数では末尾呼出は 1 値に切り詰められるため
再利用を維持する。回帰テスト 3 件を追加（いずれも修正前は失敗し、
修正後は成功）。検証区分は Tier FULL のため、分岐元との全件コーパス
比較（4,155 件×4 条件）を実施した（結果は下記の検証記録を参照）。
詳細は docs/verification/available-expression-reuse-expansion-guard-20261030.md。

## Block rewrite の分離走査の小規模無確保化（2026-10-30、未公開）

compile 追跡コストのうち、immutable-carrier-synthesis パスの
挿入 rewrite 残差 16 回の内訳を切り分け、由来 Arc 1 回と
分離走査の retained HashSet 1 回からなることを確定した。
挿入 rewrite 自体の除去は、ルート Block の由来が監査
プリンタの文間ギャップ被覆に読まれるため不採用とし、
共有経路 `finish_rewrite_borrowed` の Block 分離走査を
小さい Block（旧・新とも 512 文以下）では線形走査に
変更した。両経路とも同一の分離集合を同一の旧ソース順で
返すため記録内容は同一で、変わるのは計算方法だけである。
immutable.lua での追跡差は 760 回→752 回（-8 回）、
immutable-small.lua では 103 回→102 回。無追跡側・
saved・コード長・ノード数は同一。検証区分は Tier FULL
のため、分岐元との全件コーパス比較（4,155 件×4 条件、
欠損 0 件、TSV バイト同一、成功／失敗・生成コード・
Map 中核すべて一致）を実施した。
詳細は docs/verification/block-rewrite-small-detachment-scan-20261030.md。
## immutable-carrier-synthesis のリプレイのインプレース化（2026-10-30、未公開）

immutable-carrier-synthesis パスで、勝者候補のリプレイを
ラウンドごとの複製＋コミットから作業中 AST への
インプレース適用に変更した。従来はラウンドごとに源と
候補の複製が由来スロットのページ（64 スロット／ページ）
を再共有し、occurrence 書き込みがページを切り離し直す
ため、8 ラウンドで約 656 ページコピーが残差の大半を
占めていた。derive 入力と定義 contributor を変異前に
スナップショットし、新設の `derive_from_borrowed_with_code`・
`relate_from_borrowed_role_code`・`derive_from_clone_self`
（いずれも従来 API と同一の記録路を通る）で適用する
ため、生成コード・由来・Map は同一で、変わるのは
確保回数だけである。immutable.lua での追跡差は
2,109 回→760 回（約 -64.0%、derive フロア 740 回＋
挿入 16 回が残る）、immutable-small.lua では 188 回→
103 回。検証区分は Tier FULL のため、分岐元との全件
コーパス比較（4,155 件×4 条件、欠損 0 件、TSV バイト同一、
成功／失敗・生成コード・Map 中核すべて一致）を実施した。
詳細は docs/verification/immutable-carrier-inplace-replay-20261030.md。
## immutable-carrier-synthesis の occurrence rewrite 由来の除去（2026-10-29、未公開）

immutable-carrier-synthesis パスで、リプレイの occurrence 書き換えにおける
rewrite 由来の記録を除去した。直後の derive が当該スロットを源 occurrence
から作り直す（未知入力時はクリアする）ため、rewrite 由来は読み取られる
ことなく上書きされ、記録内容は同一である。値の置換自体は IndexMut 代入
で行い、derive・relate・挿入 rewrite は従来通りである。immutable.lua での
追跡差は 2,849 回→2,109 回（約 -26.0%、rewrite 分 740 回）、
immutable-small.lua では 288 回→188 回（-100 回）。無追跡側・saved・
コード長・ノード数は同一。検証区分は Tier FULL のため、分岐元との全件
コーパス比較（4,155 件×4 条件、欠損 0 件、TSV バイト同一、成功／失敗・
生成コード・Map 中核すべて一致）を実施した。
詳細は docs/verification/immutable-carrier-redundant-rewrite-removal-20261029.md。
## relate の単一 span 共有化（2026-10-29、未公開）

relate で contributor が primary 1 span のみ・対象が related・
relations ともに空の場合に、記録される単一ベクトル（related 1 件
と typed relation 1 件）をインターン表から共有する高速路を
`NodeArena::relate_snapshot` に追加した。記録内容は同一で、変わる
のは確保回数だけである。immutable.lua での追跡差は 5,793 回→
2,849 回（約 -50.8%、relate 分を全額削減）、immutable-small.lua
では 686 回→288 回。検証区分は Tier FULL のため、分岐元との全件
コーパス比較（4,155 件×4 条件、欠損 0 件、TSV バイト同一、成功／
失敗・生成コード・Map 中核すべて一致）を実施した。
詳細は docs/verification/relate-singleton-sharing-20261029.md。
## 選択的 S1（target モードの部分キャッシュ変種。2026-10-29、未公開）

target モードで全グループ一括の S1 レイアウトが予算を超える場合に、
走査順の最初のグループだけをキャッシュする部分変種
（joint-api-local-s1-partial／joint-api-local-licm-s1-partial）を
追加した。速度順位はフル S1 の直下・基底レイアウトの直上とし、
フルが予算内に収まる場合は従来通りフルが勝つ。目標なしコンパイルは
不変（Tier FULL コーパス比較で 16,620 行バイト同一）。target モード
（slack32）では 10 ファイルが基底レイアウトから部分変種へ遷移し、
いずれも予算内（合計 +128 文字、target_met の反転なし）。計測専用
マシンでの合成ペア確認では実行結果は同一（Completed・描画コマンド
256 件とも同一）で、速度差はノイズ水準（ペア相対差の中央値 -0.33%、
head が速い対は 11/21）のため定量主張は控える。決定的な命令数閾値は
反復あたり不変で preheader 分の +1 のみである。
## immutable-carrier-synthesis の理由コード共有化（2026-10-29、未公開）

immutable-carrier-synthesis パスで、候補ごとに 1 個ずつの共有
コード文字列（use／definition／insertion）を使い回し、追跡コストを
削減した。occurrence の書き換えを `rewrite_with_code` へ、由来付与を
新設の `origins::derive_with_code` へ、定義関連付けを
`relate_from_role_code` へ、挿入 rewrite を `rewrite_with_code` へ
変更した。記録される文字列内容は変わらないため生成コード・由来・
Map は同一で、変わるのは確保回数だけである。immutable.lua での
追跡差は 7,273 回→5,793 回（約 -20.3%）、無追跡側も 32,513,605 回→
32,228,515 回（試行側の fresh コード確保の除去）。検証区分は
Tier FULL のため、分岐元との全件コーパス比較（4,155 件×4 条件、
欠損 0 件、TSV バイト同一、成功／失敗・生成コード・Map 中核
すべて一致）を実施した。
詳細は docs/verification/immutable-carrier-shared-code-20261029.md。
## strip 側の除去理由コード共有化（2026-10-29、未公開）

single-use-local-sinking パスで除去されるローカル宣言の strip 側で、
除去理由のコード文字列をパス全体で 1 個の共有 `Arc<str>` にした。
`NodeArena::retain_block_statements_with_code` を新設し、従来の
`retain_block_statements` はそこへ委譲する。記録される文字列内容は
変わらないため生成コード・由来・Map は同一で、変わるのは確保回数だけ
である。locals-single での追跡差は 4,504 回→4,003 回（約 -11.1%）、
無追跡側は実質不変。検証区分は Tier FULL のため、分岐元との全件
コーパス比較（4,155 件×4 条件、欠損 0 件、TSV バイト同一、成功／失敗・
生成コード・Map 中核すべて一致）を実施した。
詳細は docs/verification/strip-removal-shared-code-20261029.md。
## single-use-local-sinking の理由コード共有化（2026-10-29、未公開）

single-use-local-sinking パスで、パス全体で 1 個ずつ作った共有コード文字列を
由来記録に使い回し、追跡コストを削減した。`expand_tree` の文書き換えを
`rewrite_with_code` へ、2 箇所の `relate_from_role` を新設の
`relate_from_role_code`（`relate_within_role_code` のアリーナ間版）へ変更した。
記録される文字列内容は変わらないため生成コード・由来・Map は同一で、
変わるのは確保回数だけである。locals-single での追跡差は 5,004 回→4,504 回
（約 -10.0%）、無追跡側も 39,363 回→38,865 回（旧 `rewrite` 経路が無追跡でも
fresh なコード文字列を作っていた無駄の除去）。検証区分は Tier FULL のため、
分岐元との全件コーパス比較（4,155 件×4 条件、欠損 0 件、TSV バイト同一、
成功／失敗・生成コード・Map 中核すべて一致）を実施した。
詳細は docs/verification/single-use-local-shared-code-20261029.md。
## schema-2 トークン表の実装（2026-10-29、未公開）

字句形の Source Map におけるトークンあたりの重複記録を解消し、
schemaVersion を 1 から 2 へ上げた（互換断絶はユーザー了承済みで、
次回リリースは v0.4.0）。字句勝者経路と simple_copy 経路の両方に
トークン表（tokenNames プール＋[generatedStart, sourceStart,
sourceEnd, nameId] レコード）を実装し、validate() はワイヤ形のまま
integrity を照合した後に tokens を mappings＋origins へ展開して返す。
一般形の map は従来レイアウトのまま tokenNames／tokens を空配列で
出す。simple_copy のトークン表は適用条件を厳格化し（単一 source、
全 origin 参照、初出順＝origin 順、一意性、名前は name 精度のみ、
連続被覆）、満たさない場合は従来レイアウトへフォールバックする。
検証区分は Tier FULL のため、分岐元との全件コーパス比較（4,155 件
×4 条件、欠損 0 件、TSV バイト同一、成功／失敗・生成コード・Map
中核すべて一致、validate 失敗 0 件）を実施した。コーパス全体の
map 容量は -3.94%（トークン表 5 件で -44.9MB、他 map の空キー増は
+0.11MB）。計測専用マシンでの新旧交互測定では、dense-lexical の
Map 時間が約15.18ms→約6.14ms（約 -59.6%、Map 比 約7.4→約3.0）、
dense-normal の Map 比は約1.26 のまま不変である。
詳細は docs/verification/schema-2-token-table-20261029.md。
## target モードでのループ内単発メンバ呼出キャッシュ（S1）（2026-10-29、未公開）

target 付きコンパイルで、ループ内に1回だけ現れるメンバ呼出の
関数値をループ前へ局所化する target 限定レイアウトを追加した。
第56サイクルで否決された M2（ループ内複数回）形とは異なり、
単発呼出はメンバ・エイリアスの選択対象にならないため、勝者
コード上に反復ごとの表引きが残る。第57サイクルのセンサスでは
勝者コード上に非 LICM の S1 グループ 2,791 件／1,356 ファイルを
確認していた。

実装は `storm-lua-minify` のみ。新規パス
`passes/single_member_cache.rs`（第56サイクルの証明機構を流用：
builtin ルート未変異・単一定義エイリアス基底・未知呼出／
メソッド呼出／状態付き builtin／表書込み／goto のバリア、関数
内呼出の除外、最外ループによる claiming）と `search.rs` の
target 限定レイアウト `joint-api-local-s1`・
`joint-api-local-licm-s1`（LICM との結合を最上位とする速度順位
の拡張、polish の root-local-globalization 除外の拡張）からなる。
目標なしコンパイルには一切触れない。登録パス ID は追加しない
（LICM と同じレイアウト変換の扱いで、map の churn を避ける）。

キャッシュ局所名は、プログラムの文字列集合に存在しない最短の
1 文字識別子を使う。第56サイクル形の生成名
`__membercache` はレイアウト候補では短縮されず、1 グループ
あたり約 +34 文字かかっていた（合成入力で S1 候補 217 文字→
短縮名で 193 文字に改善）。短縮名では 1 グループあたり約
+9 文字（＋区切り 1 文字程度）になる。文字列集合に存在しない
名前はユーザーコードから宣言・参照され得ないため、シャドウの
心配はない。

コーパス全件の target モード差分（目標＝目標なし勝者サイズ＋
slack、目標なし側は完全同一）：slack +32 で 506 ファイルが
S1 勝者へ遷移（local-s1 499 件・licm-s1 7 件、サイズ合計
+6,863 文字、最大 +32 文字）、+64 で 496 ファイル（490 件・
6 件、+6,948 文字）、+128 で 140 ファイル（134 件・6 件、
+1,976 文字）。いずれも target_met の反転 0 件・予算超過
0 件・失敗 28 件は不変。slack が大きいほど遷移が減るのは、
ベース段階のチェックポイントが目標を満たして早期復帰し、
フルサーチに到達しない OBJ-2 の既定挙動による。詳細は
docs/verification/target-s1-member-cache-20261029.md。
検証区分は Tier FULL のため、分岐元からの全件コーパス比較で
目標なし側の差分 0 件を確認した。
## single-use-local-sinking の多値展開バグ修正（2026-10-29、未公開）

1 回だけ読まれるローカルへの定義式をその使用箇所へ置換する
single-use-local-sinking パスが、Lua の多値調整を崩すバグを
修正した。純粋なユーザー関数呼出は効果解析で calls=false と評価
されるため、呼出の定義式が転送ガードをすり抜け、展開位置（多重
local の最終初期化式など）に裸で置かれると元の 1 個ではなく全値を
渡していた（例：x = pair(); local c, d = x が local c, d = pair()
となり d が nil ではなく 2 個目の値を取る）。修正は置換を位置認識化し、
定義式のトップが呼出または ... でかつ使用箇所が展開位置の場合に限り
置換後の式を括弧で囲むことで、常に 1 値へ調整されるようにした
（実装は minify クレートの locals.rs のみ＋回帰テスト 4 件）。
非展開位置では従来通り裸で置換する。第2サイクルの残件だった
one-use-function-fusion の return 展開例は、融合そのものではなく
この sinking が後段で中間ローカルを除去した結果であり、同じ修正で
直る。検証区分は Tier FULL のため、分岐元との全件コーパス比較
（4,155 件×4 条件、欠損 0 件、ステータス変化 0 件）を実施した。
生成コードが変わったのは 36 件で、34 件は括弧挿入のみ、残る 1 件は
勝者再選択によるもので実行照合で意味変化なしと確認した。
詳細は docs/verification/single-use-local-sinking-multivalue-paren-20261029.md。

## NodeOrigin の contexts／dispositions のインライン小リスト化による追跡コスト削減（2026-10-29、未公開）

compile の追跡コストのうち、表現レベルの由来スロット共有設計
（ユーザー承認事項4）の第一歩として、NodeOrigin のノード単位の
由来リスト2種をインライン化した。従来 `contexts` は
`Arc<Vec<InlineContext>>`、`dispositions` は
`Arc<Vec<SourceDisposition>>` で、空の共有 Vec への初回書込みが
Arc ボックス＋バッファの 2 確保を払っていた。両者を
`SmallVec<[T; 1]>` 化し、1 件まではスロット内で 0 確保、2 件以上は
従来の Vec 同様ヒープへ spill する形に変更した（実装は syntax
クレートの provenance.rs・node_arena.rs のみ＋回帰テスト 1 件）。
シリアライズ形は JSON 配列のままで同一であり、記録される由来・
理由・生成コードは変わらない。

合成入力で literal-call-folding の追跡差が inline.lua 4,634 回→
2,634 回（約 -43.2%）、inline-50.lua 472 回→272 回へ減少した。
無追跡側の確保回数・バイト数、saved・コード長・ノード数は変更
前後で完全同一。時間改善は主張しない。なお dense-normal 全
コンパイルの追跡バイト差は +3,669,744 B と後退する
（NodeOrigin 構造体の肥大化による）が、件数では 53,322 回→
52,365 回へ改善した。詳細は
docs/verification/node-origin-small-lists-20261029.md。
検証区分は Tier FULL のため、分岐元からの全件コーパス比較で差分 0 件を
確認した。

## single-use-global-forwarding の多値展開バグ修正（2026-10-29、未公開）

1 回だけ読まれるグローバルへの代入式をその使用箇所へ転送する
single-use-global-forwarding パスが、Lua の多値調整を崩すバグを
修正した。純粋なユーザー関数呼出は効果解析で calls=false と評価
されるため、呼出の定義式が転送ガードをすり抜け、展開位置（呼出の
最終引数など）に裸で置かれると元の 1 個ではなく全値を渡していた
（例：x = pair(); count(x) が count(pair()) となり可変長関数の引数
個数が 1 個から 3 個へ変わる）。修正は転送を位置認識化し、定義式
のトップが呼出または ... でかつ使用箇所が展開位置の場合に限り
転送後の式を括弧で囲むことで、常に 1 値へ調整されるようにした
（実装は minify クレートの single_use_forwarding.rs のみ＋回帰テスト
3 件）。非展開位置では従来通り裸で転送する。検証区分は Tier FULL のため、分岐元との全件コーパス比較
（4,155 件×4 条件、欠損 0 件、ステータス変化 0 件）を実施した。
生成コードが変わったのは 292 件で、265 件は括弧挿入による
+2 の倍数の伸び（+2 が 181 件、+4 が 50 件、+6 が 23 件、
+8 以上が 11 件）であり、抽出確認したものは括弧挿入のみの差分。
残る 27 件（負数・ゼロ・奇数の差分）は勝者再選択によるもので、
実行照合で base と head が全構成一致し、意味変化なしと確認した。
エラー 28 件は全て同一で、コード同一・写像のみ異なる差分および
説明系のみの差分は 0 件だった。
詳細は docs/verification/single-use-global-forwarding-multivalue-paren-20261029.md。

## api-alias の勝者リネーム所有権化による追跡コスト削減（2026-10-29、未公開）

compile の追跡コストのうち、api-alias-optimization パスの追跡差の
主因である最終段の scope リネームの追跡コストを削減した。勝者 AST
は所有権のある状態で手元にあるため、リネームをクローン経由では
なく所有権版（`scope_rename_fast_owned`）で直接適用する形に変更
した（実装は minify クレートの scope_rename.rs の共有化と
api_aliases.rs の呼出切替えのみ＋回帰テスト 1 件）。リネーム計画は
由来なし複製から取る（計画は構文と文字列集合のみに依存するため
同一）が、計画複製は由来スロット表を持たないため作業 arena の
由来ページと共有されず、所有権のある作業 arena のページは一意な
まま適用され、クローン由来の共有ページに対する初回書込みごとの
ページ複写が消える。記録される由来・理由・生成コードは変わらない。

合成入力で同パスの追跡差が inline.lua 1,286 回→1,158 回、
common.lua 1,333 回→1,213 回、dense-normal.lua 2,685 回→2,497 回
へ減少した。無追跡側の確保回数・saved・コード長・ノード数は変更
前後で完全同一。時間改善は主張しない。
検証区分は Tier FULL のため、分岐元からの全件コーパス比較で差分 0 件を
確認した。
詳細は docs/verification/api-alias-owned-rename-20261029.md。

## temporary-global-packing の変換コード共有化による確保削減（2026-10-29、未公開）

compile の追跡コストのうち、temporary-global-packing パスの最終ループで、
改名ノードごとに変換コード文字列を新規確保していた無駄を除去した。
パス全体で 1 個のコード文字列を作り `rewrite_with_code` に渡す形に変更した
（実装は minify クレートの temporary_globals.rs のみ）。記録される由来・
理由は内容同一のため、最終 AST・由来・生成コードは変わらない。

合成入力（common.lua、発火入力）では無追跡・追跡とも約 1,050 回の確保が
減った（無追跡 6,917 回→5,869 回、追跡 8,088 回→7,036 回）。追跡差は
1,171 回→1,167 回で実質不変である。これは、変換コード文字列が無追跡
モードでもノードごとに確保されていたため両モードで同数減ったことによる。
追跡差そのものは、改名ノードごとの由来スロット Arc ボックスと触れた
ページごとのスロットページ COW による現行表現のフロアであり、その打開
には表現レベルの変更が必要である。saved・コード長・ノード数は変更前後で
同一。時間改善は主張しない。
検証区分は Tier FULL のため、分岐元からの全件コーパス比較で差分 0 件を
確認した。
詳細は docs/verification/temporary-global-packing-shared-code-20261029.md。

## literal-call-folding のスクラッチ arena 直接構成による追跡コスト削減（2026-10-29、未公開）

compile の追跡コストのうち、literal-call-folding パスに残っていた
スクラッチ arena 構成の無駄な確保を除去した。従来の
`empty_scratch_ast` は `inherit_ast` と `clone_without_origins`
の合成で作っていたが、`inherit_ast` が源の由来追跡時に空の由来
スロット表を確保し、`clone_without_origins` がそれを直後に破棄
していたため、追跡モードでのみ呼出ごとに 1 回の確保が発生して
いた。ノード空・由来なしの arena を直接構成する形に変更した
（実装は minify クレートの inline_functions.rs のみ＋回帰テスト
1 件）。できあがるスクラッチ arena の状態は新旧で同一（ノード
0 件・源と同一の文字列集合・由来なし）のため、最終 AST・由来・
生成コードは変わらず、変わるのは確保回数だけである。
合成入力で同パスの追跡差が 5,134 回から 4,634 回へ（約 −9.7%。
inline-1.lua は 20 回→19 回、inline-50.lua は 522 回→472 回、
common.lua は折り畳み 0 件のため 3 回で不変）。saved・コード長・
ノード数は変更前後で同一。時間改善は主張しない。
検証区分は Tier FULL のため、分岐元からの全件コーパス比較で
差分 0 件を確認した。
詳細は docs/verification/literal-call-scratch-arena-direct-20261029.md。

## literal-call-folding の tiny fold 再実行廃止による追跡コスト削減（2026-10-29、未公開）

compile の追跡コストのうち、literal-call-folding パスに残っていた
tiny fold 再実行のコストを削減した。scratch fold が既に折り畳み値を
証明済みの呼出について、tiny tracked arena でのルート fold 再実行を
やめ、折り畳み済みアトムの由来を 1 回のスロット構成で作る形に変更した
（実装は minify クレートの inline_functions.rs への direct_folded_atom
追加と Bin／Un 腕の置換、syntax クレートの node_arena.rs への
record_folded_atom_origin 追加、literal_folding.rs の可視性変更のみ。
回帰テストは構文レベルの等価性テスト 1 件と形状テスト 4 件）。
直接構成は fold の literal-evaluation 経路と同一の条件でのみ行い、
それ以外は従来の tracked 経路へ Fallback するため、由来の内容は
fold と同一である。合成入力で同パスの追跡差が 8,146 回から 5,134 回へ
（約 -37.0%、16.3 回／件から 10.3 回／件）。saved・コード長・ノード数は
変更前後で同一。時間改善は主張しない。検証区分は Tier FULL のため、
分岐元からの全件コーパス比較で差分 0 件を確認した。
詳細は docs/verification/literal-call-folded-atom-direct-20261029.md。

## literal-call-folding の relate における変換コード共有化による追跡コスト削減（2026-10-29、未公開）

compile の追跡コストのうち、literal-call-folding パスに残っていた
relate のコストを削減した。relate 5.0 回／件の内訳は spans 側
4.0 回と理由コード 1.0 回で、理由コードをパス全体で 1 個だけ作って
使い回す形に変更した（実装は syntax クレートの node_arena.rs への
共有コード経路 relate_within_role_code の追加と、minify クレートの
inline_functions.rs での配管＋回帰テスト 1 件）。記録されるコード
文字列の内容は同一で、変わるのは確保回数だけである。合成入力で
同パスの追跡差が 8,646 回から 8,146 回へ（約 -5.8%、17.3 回／件から
16.3 回／件）。saved・コード長・ノード数は変更前後で同一。
時間改善は主張しない。spans 側は共有ベクタの置き換えに伴う
バッファ＋ボックスの 2 確保がフロアであり、事前確保は件数で無益な
ため採用せず、表現の変更は親確認待ちとして見送った。検証区分は
Tier FULL のため、分岐元からの全件コーパス比較で差分 0 件を確認した。
詳細は docs/verification/literal-call-relate-shared-code-20261029.md。

## literal-call-folding の tiny tracked arena のパス内再利用による追跡コスト削減（2026-10-29、未公開）

compile の追跡コストのうち、literal-call-folding パスに残っていた
tiny tracked arena の呼出ごとの生成コストを除去した。同パスは
リテラル呼出のインストールごとに tiny arena を inherit_ast で
新規生成していたが、ページ式 origin slots（64 件／ページ）では
初回 push がページ用 Vec・ページボックス・ページバッファの
3 確保を払い、呼出ごとに約 4 回の追跡差になっていた。1 パス内で
1 個の tiny arena を使い回し、呼出ごとに reset_for_reuse で
値・スロットをゼロへ切り詰める形に変更した（実装は syntax
クレートの origin_slots.rs への clear_keep_page 追加と push の
ページ存在条件化、node_arena.rs への reset_for_reuse 追加、
minify クレートの inline_functions.rs での再利用配管＋回帰テスト
1 件）。リセットは前呼出の全ノード・全由来を破棄し、初回ページの
バッファだけを保持するため、各呼出が観測する arena は新規生成と
内容同一で、変わるのは確保回数だけである。合成入力で同パスの
追跡差が 10,642 回から 8,646 回へ（約 -18.8%、21.3 回／件から
17.3 回／件）。saved・コード長・ノード数は変更前後で同一。
時間改善は主張しない。検証区分は Tier FULL のため、分岐元からの
全件コーパス比較で差分 0 件を確認した。
詳細は docs/verification/literal-call-tiny-arena-reuse-20261029.md。

## literal-call-folding の tiny fold における被演算子由来の合成共有化による追跡コスト削減（2026-10-29、未公開）

compile の追跡コストのうち、literal-call-folding パスに残っていた
tiny fold の被演算子由来付けコストを除去した。Bin／Un ルートの tiny fold に
先立ち、折り畳み済み被演算子アトムへ本体ルート由来を derive_from で付けて
いたが、種類が異なるためフル rewrite 経路を通っていた。被演算子の由来は
折り畳み結果から参照されず tiny arena とともに破棄され、tiny fold 内では
known 判定の真偽だけに使われるため、共有合成由来へ置換した（実装は minify
クレートの inline_functions.rs のみ＋回帰テスト 1 件）。
合成由来は has_origin を真にし、本体ルート由来の有無によらず最終由来は
同一である。合成入力で同パスの追跡差が 14,642 回から 10,642 回へ
（約 -27.3%）。saved・コード長・ノード数は変更前後で同一。
時間改善は主張しない。検証区分は Tier FULL のため、分岐元からの
全件コーパス比較で差分 0 件を確認した。
詳細は docs/verification/literal-call-operand-synthetic-origin-20261029.md。

## literal-call-folding の tiny fold 間での FoldSharedFacts 共有による追跡コスト削減（2026-10-29、未公開）

compile の追跡コストのうち、literal-call-folding パスに残っていた
tiny fold ごとの FoldSharedFacts 構築コストを除去した。1 パス内の
全 tiny fold は同一の aggressive フラグとデフォルト許容誤差で走るため、
1 個の共有テーブルをパス全体で使い回す形に変更した（実装は minify
クレートの literal_folding.rs に共有版 fold API を追加し、
inline_functions.rs の同パス経路で共有する形＋回帰テスト 1 件）。
共有される事実文字列は純粋関数であり記録内容は同一で、変わるのは
確保回数だけである。合成入力で同パスの追跡差が 20,630 回から
14,642 回へ（約 −29.0%）。saved・コード長・ノード数は変更前後で同一。
時間改善は主張しない。検証区分は Tier FULL のため、分岐元からの
全件コーパス比較で差分 0 件を確認した。
詳細は `docs/verification/literal-call-shared-fold-facts-20261029.md`。

## literal-call-folding の呼出単位スクラッチ化による追跡コスト削減（2026-10-29、未公開）

compile の追跡コストのうち、literal-call-folding パスに残っていた、
発火した呼出のコミット計算そのものの由来確保コストを除去した。
本体複製＋定数畳み込みを由来なしスクラッチ上で行い、結果がアトムの
場合のみ tiny tracked arena でルート fold の由来を再現して呼出点へ
転送する形に変更した（実装は minify クレートの inline_functions.rs
のみ＋回帰テスト 4 件）。対象外（非アトム結果・Paren／Call 等の
本体ルート・非アトム被演算子・tiny fold の値不一致・アトムのテキスト
正規化）は従来の tracked 経路へ fallback し、builtin 分岐は変更なし。
合成入力で同パスの追跡差が 51,289 回から 20,630 回へ（約 −59.8%）。
saved・コード長・ノード数は変更前後で同一。時間改善は主張しない。
検証区分は Tier FULL のため、分岐元からの全件コーパス比較で差分 0 件を確認した。
詳細は `docs/verification/literal-call-scratch-origins-20261029.md`。

## expression-helper-inlining の試行由来なし化による追跡コスト削減（2026-10-28、未公開）

compile の追跡コストのうち、expression-helper-inlining パスに残っていた、
出力に反映されない試行の由来確保コストを除去した。同パスは呼出点ごとに
試行クローンを構築してサイズ測定し、採用が確定した場合のみコミットするが、
試行クローンが由来表付きだったため、敗北する試行も引数部分木コピーごとの
finish_rewrite の由来確保を積み上げていた（inline.lua で単独オン差 12,275 回）。
試行を由来なしクローン上で実行し、採用が確定した場合にだけ tracked な
ラウンド源で同一 rewrite をリプレイしてコミットする形に変更した（実装は
minify クレートの inline_functions.rs のみ＋回帰テスト 1 件）。
合成入力で同パスの追跡差が 12,275 回から 1 回へ。saved・コード長・
ノード数・候補数は変更前後で同一。時間改善は主張しない。
検証区分は Tier FULL のため、分岐元からの全件コーパス比較で差分 0 件を確認した。
詳細は `docs/verification/expression-helper-trial-origins-20261028.md`。

## immutable-carrier-synthesis の試行由来なし化による追跡コスト削減（2026-10-28、未公開）

compile の追跡コストのうち、immutable-carrier-synthesis パスに残っていた、
出力に反映されない試行の由来確保コストを除去した。同パスはラウンドごとに
多数の候補 AST を構築してサイズ測定し、勝者のみを採用するが、候補構築が
由来表付きだったため、敗北する試行も出現箇所ごとの rewrite＋derive＋relate
の由来確保を積み上げていた。候補構築とサイズ測定を由来なしクローン上で
行い、勝者は同一の候補構築コードを tracked なラウンド源でリプレイして
コミットする形に変更した（実装は minify クレートの
immutable_synthesis.rs のみ＋回帰テスト 1 件）。
合成入力で同パスの追跡差が 3,068,036 回から 7,273 回へ（約 −99.8%）。
saved・コード長・ノード数・候補数は変更前後で同一。時間改善は主張しない。
検証区分は Tier FULL のため、全件コーパスは FULL-VERIFY-TIP からの
累積比較で差分 0 件を確認した。
詳細は `docs/verification/immutable-carrier-trial-origins-20261028.md`。

## signed-expression-factoring の試行由来なし化による追跡コスト削減（2026-10-28、未公開）

compile の追跡コストのうち、signed-expression-factoring パスに残っていた、
出力に反映されない試行の由来確保コストを除去した。同パスは候補ごとに
試行 AST を構築してサイズ測定し、勝者のみを採用するが、試行 AST が
由来表付きだったため、敗北する試行もキャリア生成の derive／relate と
rewrite finish の由来確保を積み上げていた。試行を由来なしクローンで
構築して測定だけを行い、勝者は同一の rewrite コードを tracked クローンで
リプレイして適用する形に変更した（実装は minify クレートの
signed_factoring.rs のみ＋回帰テスト 1 件）。
合成入力で同パスの単独オン差が 12,685 回から 1,413 回へ（約 −89%）。
残差はグループ発見段階の正規化由来構築で、勝者に必要な一度きりの
コストのため残す。無追跡側・コード長は同一。時間改善は主張しない。
検証区分は Tier FULL のため、全件コーパスは FULL-VERIFY-TIP からの
累積比較で差分 0 件を確認した。
詳細は `docs/verification/signed-expression-trial-origins-20261028.md`。
## 末尾引数省略の試行ボディの由来なし化による追跡コスト削減（2026-10-28、未公開）

compile の追跡コストのうち、等価な末尾引数省略パスに残っていた、
出力に反映されない試行の由来確保コストを除去した。同パスは
呼出点ごとに、省略可否の判定のため関数本体の正規化ボディを
印字して比較するが、試行アリーナが由来表付きだったため、
複写と 3 回の constant_fold が追跡側だけの由来確保を積み上げて
いた。試行ボディは印字文字列での比較にだけ使われ、由来は
読まれないため、由来表なしの空アリーナで試行を構築する形に
変更した（実装は minify クレートの omit_arguments.rs のみ）。
合成入力で同パスの単独オン差が 78,041 回から 1 回へ。
無追跡側・コード長・候補数はすべて同一。時間改善は主張しない。
検証区分は Tier FULL（累積比較）のため、全件コーパスは
FULL-VERIFY-TIP からの累積比較で差分 0 件を確認した。
詳細は `docs/verification/trailing-argument-trial-origins-20261028.md`。
## immutable-table-flattening のブロック事前フィルタによる追跡コスト削減（2026-10-28、未公開）

compile の追跡コストのうち、immutable-table-flattening パスに残っていた、
テーブル束縛のないブロックに対する毎ブロックの複製コストを除去した。
同パスはブロックごとに AST 全体を複製して resolve するが、直下の文に
テーブルリテラル束縛がなければ `init` は必ず空で従来も無変更のまま
break していた。ラウンドループの先頭でその必要条件を構文的に検査し、
該当しないブロックは複製・resolve を行わずに break する形に変更した
（由来機構の表現変更なし。実装は minify クレートの tables.rs のみ）。
合成入力で同パスの単独オン差が 322 回から 0 回へ、直接実行の追跡差が
161 回から 0 回へ、全コンパイルの追跡差は 54,284 回から 53,640 回へ
（−644 回）。api-alias・scope-renaming・constant-folding・root-local・
all-off 差は不変。無追跡側・コード長・候補数はすべて同一。
時間改善は主張しない。検証区分は Tier INVARIANT（構成上出力不変の、
空 `init` ブロックの複製・resolve 省略）のため、全件コーパスは
次回 Tier FULL の累積比較へ繰り延べる。
詳細は `docs/verification/immutable-table-block-prefilter-20261028.md`。

## conditional 系の棄却候補への由来付け省略による追跡コスト削減（2026-10-28、未公開）

compile の追跡コストのうち、conditional 系 3 パスに残っていた、
棄却された候補ノードへの由来付けコストを除去した。3 パスは
「候補生成→由来付け→採用判定」の順で、棄却候補のノードにも
finish_rewrite が走っていた。採用判定の後にだけ、候補生成直後の
`[start, end)` のノードへ由来付けを行う形に変更した。棄却候補は
パス出力から到達不能で、生成由来は印字出力の emissions から作られる
ため、由来の有無は生成コード・Source Map・由来監査に観測されない。
採用された候補の由来付け集合は従来と同一である。実装は minify
クレートの conditionals.rs のみ。
合成入力で conditional-call-lowering の単独オン差が 803 回から
1 回へ、conditional-assignment-lowering が 803 回から 1 回へ、
conditional-return-lowering が 802 回から 0 回へ、全コンパイルの
追跡差は 63,932 回から 54,284 回へ（−9,648 回、約 −15%）。
api-alias・scope-renaming・constant-folding・root-local・all-off 差は
不変。無追跡側・コード長・候補数はすべて同一。時間改善は主張しない。
検証区分は Tier INVARIANT（構成上出力不変の、到達不能ノードへの
由来付け省略）のため、全件コーパスは次回 Tier FULL の累積比較
（起点 882169d0）へ繰り延べる。INVARIANT 連続は本サイクルで
3 件目となり上限に達するため、次回マージは Tier FULL とする。
詳細は `docs/verification/conditional-rejected-candidate-origins-20261028.md`。

## 合成由来の共有による追跡コスト削減（2026-10-28、未公開）

compile の追跡コストのうち、合成由来マークに残っていた
transformation ごとの毎ノード確保を除去した。合成由来は
transformation 名だけで決まるため、同一名でマークする全ノードが
1 個の由来ノードを共有できる。由来への in-place 変更はすべて
copy-on-write 経由のため、共有者への漏れはない。実装は syntax
クレートの node_arena.rs のみ。OriginSlots の全 None ページ共有も
試したが、attach 直後の書込みで分離コストが戻って逆効果だったため
不採用・revert した。
合成入力で api-alias 単独の追跡差が 2,765 回から 2,685 回へ
（−80 回）、全コンパイルの追跡差は 64,122 回から 63,932 回へ
（−190 回）。scope-renaming・root-local・constant-folding・
all-off 差は不変。無追跡側・コード長・候補数はすべて同一。
時間改善は主張しない。
検証区分は Tier INVARIANT（構成上出力不変の共有化）のため、全件コーパスは
次回 Tier FULL の累積比較（起点 882169d0）へ繰り延べる。
詳細は `docs/verification/synthetic-origin-sharing-20261028.md`。

## 理由テールの一般共有による追跡コスト削減（2026-10-28、未公開）

compile の追跡コストのうち、api-alias パス内の後段改名 finish に
残っていた、置換ノード 640 件分の fresh 理由テール確保を除去した。
テールノードは previous の列＋reason で決まるため、同一 previous・
同一 reason のテールは 1 個のノードを共有できる。共有ノードは共有
テーブルが参照を保持するため決して in-place 変更されず、共有元への
後続 push は copy-on-write で複写される（初回理由共有と同一の安全性）。
理由の同一性は code・operation・before・after・basis・facts を含む
Eq・Hash で判定する。実装は syntax クレートの deferred_origin.rs のみ。
合成入力で api-alias 単独の追跡差が 3,405 回から 2,765 回へ（−640 回）、
constant-folding が 2,738 回から 2,258 回へ（−480 回）、全コンパイルの
追跡差は 66,303 回から 64,120 回へ（−2,183 回、約 −3.3%）。
scope-renaming・root-local・all-off 差は不変。無追跡側・コード長・
候補数はすべて同一。時間改善は主張しない。
検証区分は Tier INVARIANT（構成上出力不変の共有化）のため、全件コーパスは
次回 Tier FULL の累積比較（起点 882169d0）へ繰り延べる。
詳細は `docs/verification/reason-tail-sharing-general-20261028.md`。

## NodeOrigin.names の SmallVec 化による追跡コスト削減（2026-10-28、未公開）

compile の追跡コストのうち、改名 finish に残っていた names Vec
バッファの毎ノード確保を除去した。NodeOrigin.names を
`SmallVec<[(NameSite, StoredOrigin); 2]>` に変え、単一名・2名束縛
まではインラインで、3名以上は従来どおりヒープへ spill する。
ノード間でリストを共有しないため、改名時のその場書き換えで
copy-on-write の複写は発生しない。シリアライズ形は JSON 配列の
ままで、公開 API の既存シグネチャは不変。依存は workspace 宣言と
architecture.json へ追加し、Cargo.lock の差分は smallvec への
serde 依存追加と syntax への smallvec 追加のみ（版は 1.16.1 のまま）。
合成入力で scope-renaming 単独の追跡差が 2,142 回から 1,181 回へ
（−961 回）、root-local が 972 回から 652 回へ（−320 回）、
api-alias が 4,526 回から 3,405 回へ（−1,121 回）、全コンパイルの
追跡差は 77,897 回から 66,303 回へ（−11,594 回、約 −15%）。
注意：NodeOrigin 構造体の肥大で確保バイト数は増加する（主指標の
確保回数は減る）。時間改善は主張しない。
実世界コーパス 4,155 ファイル × 4 条件は両側バイト同一で出力に影響なし。
詳細は `docs/verification/node-origin-names-smallvec-20261028.md`。

## 初回理由テールの共有による追跡コスト削減（2026-10-28、未公開）

compile の追跡コストのうち、書き換え理由の記録に残っていた
テールノードの毎ノード確保を除去した。previous が空の初回理由は
理由だけで決まるため、同一理由の初回理由は全 origin で 1 個の
テールノードを共有できる。previous があるテールは従来通り fresh
に作り、共有ノードへの後続 push は copy-on-write のため、記録
される理由列は fresh ノードと同一である。公開 API の既存
シグネチャは不変。
合成入力で api-alias-optimization の追跡差が 5,167 回から 4,526 回へ
（−641 回）、constant-folding が 3,218 回から 2,738 回へ（−480 回）、
root-local-globalization が 1,292 回から 972 回へ（−320 回）、
全コンパイルの追跡差は 79,982 回から 77,897 回へ（−2,085 回）。
実世界コーパス 4,155 ファイル × 4 条件は両側バイト同一で出力に影響なし。
詳細は `docs/verification/first-reason-tail-sharing-20261028.md`。

## scope-renaming の改名 finish におけるノード値 clone 除去による追跡コスト削減（2026-10-28、未公開）

compile の追跡コストのうち、scope-renaming の改名 finish に残っていた
ノード値の clone を除去した。改名では綴りが変わることが構成上保証
されるため、snapshot のノード値を借りる finish が等価である。
syntax 側に `BorrowedOrigin`／`capture_origin_ref`／
`finish_rename_changed` を追加し、minify 側の `apply_renames` の
single・multi 両サイトを借用 finish に置換した。公開 API の既存
シグネチャは不変。
合成入力で scope-renaming 単独の追跡差が 2,784 回から 2,142 回へ
（−642 回）、api-alias 内の改名も同量 −642 回、全コンパイルの追跡差は
81,282 回から 79,982 回へ（−1,300 回）。api-alias 内の改名コスト増は、
エイリアス利用箇所 644 件が改名対象になったことによるノード数の増で、
単価の悪化ではないことも切り分けで確認した。
実世界コーパス 4,155 ファイル × 4 条件は両側バイト同一で出力に影響なし。
詳細は `docs/verification/scope-rename-borrowed-finish-20261028.md`。

## derive の previous 借用化による追跡コスト削減（2026-10-28、未公開）

compile の追跡コストのうち、constant-folding の derive 経路に残っていた
previous ノード値の clone を除去した。`derive_from` /
`derive_from_with_code` の slow path が `capture_origin` の clone をやめ、
source arena の値を借りて `finish_rewrite_borrowed` へ渡す。
rewrite 経路での previous の用途はすべて借用で、呼び出し中に source
arena は変更されないため、記録される origin は同一である。
公開 API のシグネチャは変わらず、呼出側の変更は不要。
合成入力で constant-folding-expr の追跡差が 3,697 回から 3,217 回へ
（−480 回）、全コンパイルの追跡差も 81,752 回から 81,270 回へ（−482 回）。
api-alias 転送は大半が fast path のため −1 回にとどまった。
実世界コーパス 4,155 ファイル × 4 条件は両側バイト同一で出力に影響なし。
詳細は `docs/verification/derive-borrowed-previous-20261028.md`。

## decision 証拠の fact リスト共有化による追跡コスト削減（2026-10-28、未公開）

compile の追跡コストのうち、constant-folding の証拠構築に対する共有化。
記録内容は内容比較で同一のため変わらない。従来は証拠ノードごとに
facts の `Vec` とその `Arc` 化で 2 回の確保を払っていたが、同じ規則を
同じリテラルへ適用する fold は事実上同じ fact リストを繰り返し作るため、
内容キーで共有するキャッシュを追加した。キーはスタック配列（強参照保持）
のため、キャッシュヒット時は確保 0 回である。形状上限は 10 件で、超過時は
従来の無キャッシュ経路へ fallback する。合成入力で constant-folding-expr
の追跡差が 4,657 回から 3,697 回へ（−960 回）、全コンパイルの追跡差も
66,084 回から 65,124 回へ減った。無追跡側は完全同一だった。
実コーパス全件比較では差分 0 件だった。
[検証記録](docs/verification/decision-fact-list-sharing-20261028.md)。

## rewrite の token・形状共有化による追跡コスト削減（2026-10-28、未公開）

compile の追跡コストのうち、constant-folding の追跡差に対する 2 件の
共有化。記録内容は文字列内容で同一のため変わらない。1 件目は
`finish_rewrite` で token が全落ちする場合（例：二項式からリテラルへの
fold）に、共有 token ベクタを複写してから空にする代わりに共有の空ベクタを
そのまま使う変更。2 件目は rewrite 理由の Num／Str 形状を、Num は綴り・
Str はバイト長をキーとする共有キャッシュから供給する変更で、ヒット時は
format 自体が不要になる。合成入力で constant-folding-expr の追跡差が
6,577 回から 4,657 回へ（−1,920 回、約 −29%）、全コンパイルの追跡差も
68,956 回から 66,396 回へ減った。無追跡側は完全同一だった。
実コーパス全件比較では差分 0 件だった。
[検証記録](docs/verification/rewrite-token-and-shape-sharing-20261028.md)。

## resolve の由来 interning における map キー clone 除去による固定費削減（2026-10-28、未公開）

Map 経路の固定費のうち、`GeneratedOrigins::from_arena` の由来 interning が、
計測専用マシンでの変種測定で最大の単一片だった。従来は
`FastMap<Origin, u32>` に由来を intern しており、1 件の intern ごとに
map キー用の clone と origins ベクタ用の clone の 2 回の clone が起きて
いた。由来値はほぼすべてちょうど 1 回だけ intern されるため、ハッシュ→
id 表 `OriginInterner` へ置き換え、map キー用の clone をなくした。等価性は
origins 側の値と比較して判定し、ハッシュ衝突は overflow リストで値比較
するため結果は変わらない。公開 API は変わらない。
計測専用マシンでの新旧交互測定では、print＋from＋resolve が dense-normal で
約 6.76ms から約 6.05ms（約 -10.5%）、dense-lexical で約 6.84ms から
約 6.13ms（約 -10%）、draw で約 0.98ms から約 0.81ms（約 -18%）、scale-80 で
約 3.09ms から約 1.80ms（約 -42%）へ減った。実コーパス全件比較では差分 0 件
だった。Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/resolve-intern-hash-table-20261028.md)。

## from_print のページ共有化による Map 経路固定費の削減（2026-10-28、未公開）

Map 経路の固定費のうち、`DeferredGeneratedOrigins::from_print` では、
印字済みノードのスロットを1件ずつ clone して新しい arena を組んでいた。
計測専用マシンでの交互測定では、この部分が dense-normal で約 0.27ms を
占め、内訳の大半はスロット `Arc` の clone だった。`from_arena` は印字に
現れるノードのスロットしか参照しないため、参照されるスロットの集合さえ
保てば解決結果は変わらない。そこで `OriginSlots::select` を追加し、
全スロット参照済みのページは `Arc` ごと共有し、部分参照のページだけを
再構築するようにした。ページの変更経路はすべて `Arc::make_mut` 経由で、
共有ページは書込み時に分離されるため安全である。公開 API は変わらない。
計測専用マシンでの新旧交互測定では、from_print 差分が dense-normal で
約 0.27ms から約 0.06ms、dense-lexical で約 0.23ms から約 0.03ms、
draw で約 0.04ms から約 0.003ms、scale-80 で約 0.10ms から約 0.02ms へ
減った。実コーパス全件比較では差分 0 件だった。
Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/from-print-page-share-20261028.md)。

## numeric-bit パスのサイズ試算由来除去による追跡コスト削減（2026-10-28、未公開）

numeric-boolean-bit-specialization のサイズ試算 `measured()` では、追跡付き
アリーナ上で改名由来を全ノード分構築してから破棄していたため、変更の
ない入力でもパス単独の追跡差 2,786 回が付いていた。改名の値は由来の
有無に依存せず、試算後の AST は破棄されるため、由来なしの複製で測る
ようにした（api_aliases の候補選択と同じ disposable 試算のパターン）。
合成入力で numeric-bit の追跡差が 2,786 回から 1 回へ、全コンパイルの
追跡差も 74,526 回から 68,956 回へ減った。無追跡側は実質不変だった。
実コーパス全件比較では差分 0 件だった。
[検証記録](docs/verification/numeric-bit-measured-without-origins-20261028.md)。

## simplify の条件証拠構築の遅延化による追跡コスト削減（2026-10-27、未公開）

simplify（`simplify_control_flow`）では、変更されない `If` ノードにも
`conditional_evidence` を構築してから破棄していたため、追跡差 961 回の
ほぼ全額が空振りの構築コストだった。arm 真偽を 2 ビット／arm で pack
したスナップショットだけを剪定前に取り、変更されたノードにだけ従来と
同一の理由を再構築するようにした。合成入力で simplify の追跡差が
961 回から 1 回へ、全コンパイルの追跡差も 81,246 回から 74,526 回へ
減った。無追跡側は完全同一だった。実コーパス全件比較では差分 0 件だった。
Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/simplify-conditional-evidence-deferral-20261027.md)。

## api-alias 由来転送の共有コード化による追跡コスト削減（2026-10-27、未公開）

api-alias 最適化の由来転送で、置換ノードの `derive_from` を
`derive_from_with_code` に置換し、転送全体と新ルート導出で
code `Arc<str>` を 1 個だけ共有するようにした。記録内容は文字列内容で
同一のため変わらない。合成入力で api-alias パス単独の追跡差が
6,450 回から 5,810 回へ（−640 回、約 −10%）、全コンパイルの追跡差も
82,526 回から 81,246 回へ減った。無追跡側は完全同一だった。
実コーパス全件比較では差分 0 件だった。
Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/api-alias-transfer-shared-code-20261027.md)。

## tracked 印字の flatten における中間 Option ベクタの除去（2026-10-27、未公開）

tracked 印字の最終段階（flatten）では、arena 全体を中間の
Vec<Option<NodeEmission>> へ移し替えてから排出していた。各 arena 項目は
ちょうど1葉から参照されるため、中間ベクタは確保と構造体移動だけの
コストになっていた。arena を Vec のまま取り出し、葉の順にダミーと
置換しながら排出するようにした。葉の走査順は arena 順と一致しないため
順次ドレインにはせず、走査順のままインデックスで取り出す。排出物の
順序・値は変わらない。公開 API は変わらない。
計測専用マシンでの新旧交互測定では、tracked 印字が dense-normal で
約 -3.5%、dense-lexical で約 -3.7%、draw で約 -7%、scale-80 で
約 -3.6% だった。実コーパス全件比較では差分 0 件だった。
[検証記録](docs/verification/tracked-print-flatten-no-option-vec-20261027.md)。

## tracked 印字の flatten における arena 項目の move 化（2026-10-27、未公開）

tracked 印字の最終段階（flatten）では、rope の葉の順に arena 項目を
clone して排出していた。各 arena 項目はちょうど1葉から参照されるため、
clone する代わりに arena 全体を std::mem::take で取り出し、葉の順に
take して排出するようにした。排出物の順序・値は変わらない。arena は
print ごとに record が再構築するため、同一 Printer での再印字にも
問題はない。公開 API は変わらない。
計測専用マシンでの新旧交互測定では、tracked 印字が dense-normal で
約 −3%（5ラウンド全勝）、dense-lexical で互角、draw で不変、
scale-80 で約 −1% だった。flatten の残りの大半は走査自体のコストで、
clone 除去だけでは大きくは削れないことも確認した。
実コーパス全件比較では差分 0 件だった。
[検証記録](docs/verification/tracked-print-flatten-move-20261027.md)。

## constant-folding 証拠理由のリテラル・サイズ共有化による追跡コスト削減（2026-10-27、未公開）

constant-folding の fold 証拠理由の構築時に、Un／Bin 証拠サイトが
ノードごとに整形していた演算子・リテラル事実・式サイズ文字列を
共有化した。`decision_shared` に借用値を intern する
`DecisionFactValue::Borrowed` を追加し（ヒット時は確保なし）、
fold 内ではリテラル事実を値キー、式サイズを数値キーでキャッシュして
初見でのみ整形する。記録内容は文字列内容で同一のため変わらない。
合成入力で fold_expressions の追跡差が 9,357 回から 6,577 回へ
（−2,780 回、約 −30%）、全コンパイルの追跡差も 113,836 回から
111,056 回へ減った。無追跡側は完全同一だった。
実コーパス全件比較では差分 0 件だった。
Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/fold-evidence-shared-literals-20261027.md)。

## tracked 印字のセグメント保持の連結ノード化（2026-10-27、未公開）

tracked 印字で、Fragment のセグメント Vec が合成のたびに全件コピー
されていたのを、連結ノード（rope）化した。Fragment はセグメント列の
代わりに rope の根だけを持ち、append は連結ノードを 1 個作るだけに
なる。全体記録の葉は rope の葉に、部分記録の葉は offset 0 の葉に、
セグメントなし接頭辞への append は shift ノードに対応する。最終印字は
rope を反復走査して絶対範囲を確定する。rope ノードは print ごとに
開始時・終了時にクリアする thread-local の再利用 arena に置き、無追跡
印字はノードを作らないためゼロコストのままである。公開 API と生成
コード・排出物の順序・値は変わらない。Fragment のサイズは縮小した。
計測専用マシンでの新旧交互測定では、tracked 印字が dense-normal で
約 1.81ms から約 1.40ms へ（約 −23%）、dense-lexical で約 −22%、
scale-80 で約 −33%、draw で約 −55% だった。無追跡印字は全入力で不変。
実コーパス全件比較では差分 0 件だった。
Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/tracked-print-segment-rope-20261027.md)。

## fold 証拠理由の共通ファクト共有化による追跡コスト削減（2026-10-27、未公開）

constant-folding の追跡コストのうち、fold 証拠理由の構築時にノードごとに
作っていた共通ファクト文字列（aggressive フラグと許容誤差の絶対・相対
ビットパターン）を、fold 全体で 1 個だけ作って使い回すようにした。
decision 理由の綴り・値キャッシュを共有化し、呼出側共有値をそのまま
渡せる `decision_shared`（Owned／Shared の区別付き）を追加した。
記録内容は文字列内容で同一のため変わらない。合成入力で
constant-folding 単独の追跡差が 11,758 回から 10,318 回へ（−1,440 回）、
全コンパイルの追跡差も 115,277 回から 113,837 回へ減った。
無追跡側は fold 1 回あたりの固定費 5 回のみの増加だった。
実コーパス全件比較では差分 0 件だった。
Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/fold-evidence-shared-facts-20261027.md)。

## 生成由来の解決における開始イベント列の直接マージによる固定費削減（2026-10-27、未公開）

生成由来の解決のイベントマージで、開始イベントのタプル列の具体化をやめ、
radix ソート済みの開始順序を直接走査する。開始 offset は emission から
その場で読み、同 offset では終了イベントを先に取る規則は従来のタプル比較
と同一のため、イベント列の順序は変わらない。合成入力の解決結果の件数は
変更前後で同一で、計測専用マシンでの交互測定では dense-normal で約 -3.5%、
draw で約 -18% だった（dense-lexical は互角、scale-80 は小幅悪化）。
実コーパス全件比較では差分 0 件だった。
Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/resolve-merge-start-direct-20261027.md)。

## 形状文字列と fold 由来コードの共有化による追跡コスト削減（2026-10-27、未公開）

constant-folding の追跡コストのうち、演算子・真偽の形状文字列と
fold 由来の code 文字列を共有化した。演算子集合は閉じているため
静的テーブルから供給し、未知の綴りは従来通り整形する。fold 全体で
code 文字列を 1 個だけ作って使い回す。記録内容は文字列内容で同一の
ため変わらない。合成入力で constant-folding 単独の追跡差が
13,198 回から 11,758 回へ（約 −11%）、全コンパイルの追跡差も
116,717 回から 115,277 回へ減った。無追跡側は完全同一だった。
実コーパス全件比較では差分 0 件だった。
Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/shape-and-fold-code-sharing-20261027.md)。

## rewrite 理由の共有化による追跡コスト削減（2026-10-27、未公開）

root-local-globalization の文 rewrite で、文ごとに作っていた理由の
code 文字列と before／after の形状文字列を共有化した。固定形状文字列
は静的テーブルから供給し、code 文字列はパス内で 1 個だけ作って
`rewrite_with_code` 経路で使い回す。記録内容は文字列内容で同一のため
変わらない。合成入力で root-local-globalization 単独の追跡差が
2,572 回から 1,292 回へ（約 −50%）、全コンパイルの追跡差も
122,325 回から 116,717 回へ減った。無追跡側は実質不変だった。
実コーパス全件比較では差分 0 件だった。
Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/rewrite-reason-sharing-20261027.md)。

## root-local-globalization の capture＋finish から rewrite() への置換による追跡コスト削減（2026-10-27、未公開）

ルート local のグローバル化で、変換文ごとに capture＋新規ノード＋finish を
行っていた箇所を、文スロットへの `NodeArena::rewrite()` に変えた。
スロットの由来を take するため値・由来の clone がなくなり、文 ID が再利用
されるため最終 Block の rewrite でも除去扱いにならない。
あわせて Block 除去時のフォールバック理由の共有化と、rewrite 理由の空 facts
の共有化を行った。合成入力で root-local-globalization 単独の追跡差が
4,847 回から 2,572 回へ（約 −47%）、無追跡側は完全同一だった。
全コンパイルの追跡差も 133,644 回から 130,353 回へ減った。
実コーパス全件比較では成功／失敗・生成コード・Map 中核が全件一致し、
Map 条件でのみ 592 ファイルに説明系差分が出た（除去扱いでなくなった
変換文の除去 dispositions が付かなくなった、という想定内の変化）。
Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/root-local-globalization-in-place-rewrite-20261027.md)。

## tracked 印字の literal_reason における定数 Arc の共有化（2026-10-27、未公開）

tracked 印字で、リテラル綴り理由の構築時に毎回確保していた定数
（理由コード・basis・空の facts）を共有 Arc 化し、文字列分岐の
長さタグはスタックバッファで整形して Arc 化する形に変えた。
理由の値は変わらない。合成入力で tracked 印字が約 1.10ms から
約 1.03ms へ減った（約 −6.5%）。実コーパス全件比較では差分 0 件だった。
Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/tracked-print-literal-reason-share-20261027.md)。

## 冗長括弧除去の capture＋代入＋finish から rewrite() への置換による追跡コスト削減（2026-10-27、未公開）

冗長括弧除去で、ノードごとに capture＋代入＋finish を行っていた箇所を
`NodeArena::rewrite()` に変え、追跡時の値 clone をなくした。値不変時は
共有 origin を温存し、変更時は従来と同じ finish 経路を通るため、生成
コードと記録内容は変わらない。合成入力で parentheses 単独の追跡差が
2,883 回から 0 回へ減り、無追跡側は完全同一だった。実コーパス全件比較では
差分 0 件だった。Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/parentheses-rewrite-origin-20261027.md)。

## resolve の start イベント列の基数ソート化（2026-10-27、未公開）

resolve で、start イベント列の順序付けをタプルに対する比較ソートから、
start offset に対する安定な 8 ビット桁の基数ソートへ変えた。マージ判断は
offset の比較のみで決まり、基数ソートの入力順は emission index 順のため、
得られる列は従来の (offset, index) 順と完全一致し、解決結果は変わらない。
合成入力で resolve 単体が約 2.75ms から約 2.62ms へ減った（約 −5%）。
小入力では悪化しないことを確認した。実コーパス全件比較では差分 0 件だった。
Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/resolve-start-radix-sort-20261027.md)。

## tracked 印字の Fragment append におけるセグメント Vec の移動化（2026-10-27、未公開）

tracked 印字で、Fragment の合成時にセグメント列を毎回新規確保して
コピーしていた箇所を、セグメントを持たない側への append では相手の
Vec を丸ごと移動する形に変えた。セグメントの順序・値・オフセットは
従来と同一で、生成コードと emissions は変わらない。合成入力で
tracked 印字が約 1.19ms から約 1.09ms へ減った（約 −8%）。
実コーパス全件比較では差分 0 件だった。Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/tracked-print-append-segment-move-20261027.md)。

## 目標付きコンパイルでの api-alias local 化（2026-10-27、未公開）

目標文字数付きコンパイルで、文字数上限内で生成 Lua の実行速度を
最優先する。api-alias の定義をチャンク local 宣言で出すレイアウト
`joint-api-local` を target 限定で追加し、目標内に収まる候補の中では
local 版を優先して選ぶ（目標を超える速度版は選ばない）。
目標なしコンパイルは生成コードが変わらない。実コーパス全件比較では
目標なし出力が base と完全一致した。描画例では命令数 2447→2318
（約 −5.3%）、壁時間ペア中央値 −3.3%。一般 LICM は次サイクル以降の
課題として残る。Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/target-alias-localization-20261027.md)。

## NodeOrigin.names の素の Vec 化による追跡コスト削減（2026-10-27、未公開）

NodeOrigin の名前由来列を `Arc<Vec>` から素の `Vec` に変え、改名時の
名前 Vec 複写に伴う Arc ボックス確保をなくした。シリアライズ形は配列の
ままで同一で、生成 Lua とマップ本体は維持し、実コーパス全件比較では
差分 0 件だった。合成入力で scope-renaming の追跡差の確保回数が
3,745 回から 2,784 回へ、api-alias の追跡差が 11,159 回から 9,394 回へ、
全コンパイルの追跡差が 170,832 回から 160,576 回へ減った。
Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/node-origin-names-plain-vec-20261027.md)。

## scope-renaming の reason tail 共有による追跡コスト削減（2026-10-27、未公開）

scope-renaming で、改名ノードごとに新規確保していた理由テールの
ノードを、改名全体で1個ずつ作った共有ノードに変えた。seed 由来の
記録される理由列は従来と同一に展開され、生成 Lua とマップ本体は維持し、
実コーパス全件比較では差分 0 件だった。合成入力で scope-renaming の
追跡差の確保回数が 5,665 回から 3,745 回へ、api-alias の追跡差が
13,083 回から 11,159 回へ減った。Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/scope-rename-shared-reason-tail-20261027.md)。

## 生成由来の解決における gap 勝者の永続メモ化（2026-10-27、未公開）

生成由来の解決で、gap 勝者の interning を emission index を鍵とする
永続 memo に変えた。直前勝者のみの従来キャッシュは、勝者がほぼ毎 gap
変わる合成入力では一度も当たらなかった。生成 Lua とマップ本体は維持し、
実コーパス全件比較では正規化済み Map 全体まで差分 0 件だった。
合成入力で gap の interning 呼び出しが 9,284 件から 7,364 件へ減った
（決定的な計数）。Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/resolve-winner-memo-20261027.md)。

## scope-renaming の変換コード共有による追跡コスト削減（2026-10-27、未公開）

scope-renaming で、改名ノードごとに新規確保していた変換コード文字列
を、改名全体で1個の共有文字列に変えた。由来本体と名前由来の記録内容は
同一で、生成 Lua とマップ本体は維持し、実コーパス全件比較では差分 0 件
だった。合成入力で scope-renaming の追跡差の確保回数が 7,587 回から
5,665 回へ、api-alias の追跡差が 15,649 回から 13,083 回へ減った。Map 比
1.2 の目標は未達のまま。[検証記録](docs/verification/scope-rename-shared-code-20261027.md)。

## api-alias の追跡時再構築の勝者候補への由来移譲（2026-10-27、未公開）

api-alias 最適化で、追跡入力に対する勝者候補の再構築をやめ、勝者候補へ
由来だけを移譲する方式に変更した。置換ノードは `derive_from`、複製
ノードは `derive_from_clone` で源スロットを共有し、定義付き新ルートの
孤児本体 Block と detachment 順序も replay と一致させた。生成 Lua と
マップ本体は維持し、実コーパス全件比較では差分 0 件だった。合成入力で
api-alias の追跡差の確保回数が 19,680 回から 15,649 回へ、追跡差バイト数
が約 2,440,000 B から約 1,527,000 B へ減った。Map 比 1.2 の目標は未達の
まま。[検証記録](docs/verification/api-alias-origin-transfer-20261027.md)。

## 生成由来の解決のイベントソート分割（2026-10-27、未公開）

生成由来の解決で、開始・終了イベントの単一ソートを 2 列の分離ソート＋
マージへ変えた。生成 Lua とマップ本体は維持し、実コーパス全件比較では
正規化済み Map 全体まで差分 0 件だった。合成入力で解決を含む測定が
約 4.84ms から約 4.42〜4.53ms へ減った。Map 比 1.2 の目標は未達のまま。
[検証記録](docs/verification/resolve-event-sort-split-20261027.md)。

## 生成由来の解決時 interning のメモ化（2026-10-27、未公開）

生成由来の解決で、gap の直前勝者と constructs の主由来の interning を
メモ化し、dispositions の重複除去を bitset 化した。生成 Lua とマップ本体は
維持し、実コーパス全件比較では正規化済み Map 全体まで差分 0 件だった。
合成入力で解決を含む測定が約 5.46ms から約 4.72〜4.74ms へ減った。
Map 比 1.2 の目標は未達のまま。[検証記録](docs/verification/resolve-interning-memo-20261027.md)。

## seed の validate 高速化と parser token 直接受け渡し（2026-10-27、未公開）

由来 seed で、名前重複検査の BTreeSet 確保を線形走査へ変え、パーサーの
トークン列を BorrowedToken 変換 Vec なしで seed へ直接渡すようにした。
生成 Lua とマップ本体は維持し、実コーパス全件比較では正規化済み Map
全体まで差分 0 件だった。合成入力で positions＋seed が約 2.36ms から
約 2.27ms へ減った。Map 比 1.2 の目標は未達のまま。[検証記録](docs/verification/seed-validate-and-token-direct-20261027.md)。

## 定数畳み込みの由来付けの源からの derive 化（2026-10-27、未公開）

定数畳み込みと制御フロー単純化で、走査の外側にあった由来の
capture＋finish_rewrite を、走査対象の不変の源に対する `derive_from`
に置き換えた。生成 Lua とマップ本体は維持し、実コーパス全件比較では
正規化済み Map 全体まで差分 0 件だった。合成入力で constant-folding
の追跡差の確保回数が 18,964 回から 14,158 回へ減った。Map 比 1.2 の
目標は未達のまま。[検証記録](docs/verification/constant-folding-derive-from-source-20261027.md)。

## parse 由来 seed の字句解析再利用（2026-10-27、未公開）

parse 由来の seed で、パース時に既に字句解析済みのトークン列を再利用し、
同じテキストの 2 回目の字句解析をなくした。生成 Lua とマップ本体は維持し、
実コーパス全件比較では正規化済み Map 全体まで差分 0 件だった。合成入力で
positions＋seed が約 2.50ms から約 2.34〜2.40ms へ減った。Map 比 1.2 の
目標は未達のまま。[検証記録](docs/verification/parse-seed-lex-reuse-20261027.md)。

## tracked 印字の emission 保持の arena 化（2026-10-27、未公開）

tracked 印字（output_with_positions）で、Fragment 合成時に emission 全件を
シフト・移動していた内部表現を、Printer 所有の arena と (arena index, offset)
のセグメントへ変更した。生成 Lua とマップ本体は維持し、実コーパス全件比較では
正規化済み Map 全体まで差分 0 件だった。合成入力で tracked 印字の確保バイト数が
約 19.0MB から約 8.1MB へ減った。Map 比 1.2 の目標は未達のまま。[検証記録](docs/verification/tracked-print-arena-20261027.md)。

## 描画レコードの被呼出鎖クローン由来共有（2026-10-27、未公開）

描画レコード詰め込みで、詰め込み対象の被呼出鎖の複製に対する由来付けを
共有するよう変更した。生成 Lua とマップ本体は維持し、実コーパス全件比較では
生成コード・Map 中核に差分がなく、説明系のみ Map あり条件の 2 ファイルに
想定内の差分が出た。合成入力で追跡差の確保回数が約 10,319 回から
約 10,207 回へ減った（効果は小さい）。Map 比 1.2 の目標は未達のまま。[検証記録](docs/verification/draw-records-callee-clone-origins-20261027.md)。

## 区間起点シフトのクローン由来共有（2026-10-27、未公開）

区間起点シフトで、候補の点・幅・条件の複製に対する由来付けを共有する
よう変更した。生成 Lua とマップ本体は維持し、実コーパス全件比較では
正規化済み Map 全体まで差分 0 件だった。合成入力で追跡差の確保回数が
約 19,495 回から約 11,321 回へ減った。Map 比 1.2 の目標は未達のまま。[検証記録](docs/verification/interval-shifting-clone-origins-20261027.md)。

## 関数インラインの置換付きクローン由来共有（2026-10-27、未公開）

関数インラインで、置換された引数を除く本体の複製に対する由来付けを
共有するよう変更した。生成 Lua とマップ本体は維持し、実コーパス全件比較では
生成コード・Map 中核に差分がなく、説明系のみ 107 ファイルに想定内の差分が
出た。合成入力で追跡差の確保回数が約 56,275 回から約 14,275 回へ減った。
Map 比 1.2 の目標は未達のまま。[検証記録](docs/verification/inline-functions-clone-origins-20261027.md)。

## common-offset 吸収のクローン由来共有（2026-10-26、未公開）

common-offset 吸収で、オフセット式の複製に対する由来付けを共有するよう
変更した。生成 Lua とマップ本体は維持し、実コーパス全件比較では
正規化済み Map 全体まで差分 0 件だった。合成入力で追跡差の確保回数が
約 10,556 回から約 6,524 回へ減った。Map 比 1.2 の目標は未達のまま。[検証記録](docs/verification/common-offset-clone-origins-20261026.md)。

## immutable キャリア合成のクローン由来共有（2026-10-26、未公開）

immutable キャリア合成で、繰り返し式の複製に対する由来付けを共有する
よう変更した。生成 Lua とマップ本体は維持し、合成入力で追跡差の確保
回数が約 1,016,631 回から約 997,821 回へ減った。Map 比 1.2 の目標は未達
のまま。[検証記録](docs/verification/immutable-carrier-clone-origins-20261026.md)。

## captured 単一使用ホイストのクローン由来共有（2026-10-26、未公開）

captured 単一使用ホイストで、使用文と捕捉式の複製に対する由来付けを
共有するよう変更した。生成 Lua とマップ本体は維持し、合成入力で追跡差
の確保回数が約 19,131 回から約 10,631 回へ減った。この入力では
`x_storm` の説明履歴にも差が出ていない。Map 比 1.2 の目標は未達のまま。[検証記録](docs/verification/captured-use-clone-origins-20261026.md)。

## 単一使用ローカルのクローン由来共有（2026-10-26、未公開）

単一使用ローカルのシンクで、初期化式の複製に対する由来付けを共有する
よう変更した。生成 Lua とマップ本体は維持し、合成入力で追跡あり確保回数
が約 29% 減った。`x_storm` の説明履歴からは、クローンに対する冗長な
複製理由の一段が消える。Map 比 1.2 の目標は未達のまま。[検証記録](docs/verification/single-use-local-clone-origins-20261026.md)。

## 決定理由の fact 値共有（2026-10-26、未公開）

`OptimizationReason::decision` が理由ごとに新しく確保していた
fact 値の `Arc<str>` を、内容が等しい値同士で共有するよう変更した。
共有キャッシュには 4,096 件の上限を設け、入力内容に比例して
増え続けないようにした。生成 Lua とマップ本体・`x_storm` の説明
履歴は維持し、合成 dense-normal で定数畳み込みの確保回数が約 9.7%
減った。時間の改善は測定ノイズの範囲内であり主張しない。
Map 比 1.2 の目標は未達のまま。[検証記録](docs/verification/decision-fact-value-interning-20261026.md)。

## 決定理由の綴り共有（2026-10-26、未公開）

`OptimizationReason::decision` が理由ごとに新しく確保していた
code・basis・fact key の `Arc<str>` を、内容が等しい綴り同士で
共有するよう変更した。生成 Lua とマップはバイト単位で同一のまま、
合成 dense-normal で定数畳み込みの確保回数が約 10% 減った。
Map 比 1.2 の目標は未達のまま。[検証記録](docs/verification/decision-spelling-interning-20261026.md)。

## 定数畳み込みの無変更書き戻し省略（2026-10-26、未公開）

定数畳み込みの数値描画と制御フロー単純化で、値が変わらない書き戻し
を省略した。生成 Lua とマップ本体・`x_storm` の説明履歴は維持し、
合成 dense-normal で制御フロー単純化の追跡差が約 0.85ms から
約 0.64ms へ縮んだ。Map 比 1.2 の目標は未達のまま。[検証記録](docs/verification/fold-noop-write-skip-20261026.md)。

## API 別名クローンの由来共有（2026-10-26、未公開）

API 別名パスの追跡付き再適用で、変更のないクローンノードの由来スロットを
共有するよう変更した。置換による子 ID のずれで高速経路が外れていた問題を
解消し、合成 dense-normal で api-alias パス単体の追跡差が約 7.2ms から
約 3.7ms へ縮んだ。生成 Lua とマップ本体は維持するが、`x_storm` の説明
履歴からはクローンに対する冗長な一段が消える。Map 比 1.2 の目標は未達の
まま。[検証記録](docs/verification/api-alias-clone-origins-20261026.md)。

## 由来解決の高速化（2026-10-26、未公開）

`GeneratedOrigins::from_arena` の active 集合と interning ハッシュを高速化し、
生成 Lua とマップ内容を維持した。合成 dense-normal で由来解決が約 30% 短縮。
Map 比 1.2 の目標は未達のまま。[検証記録](docs/verification/from-arena-interning-20261026.md)。

## minifyレビュー修正（2026-10-13、未公開）

レビューの35指摘と3性能課題を修正し、Native/WASM・Source Map・既存最適化の回帰を検証した。深い構文の診断境界と保守的な適用条件を追加している。文字数増加・性能が悪化する小ケースを含む測定結果は[単一の実装レポート](docs/verification/minify-review-fixes-20261013.md)に記録した。metadata修正とともに未公開。

## metadataと詳細Source Mapの修正（2026-10-12、未公開）

公開v0.3.1で`metadataTag`・`minify:false`・`sourceMap:true`を併用すると、空白化したmetadataに完全一致コピーを主張して失敗する。除去範囲を共通リンカーのコピー区間から除外し、通常/LifeBoat/Addon、標準/詳細mapを修正した。元snapshotと変更範囲外のbyte/UTF-16位置を保持する。[修正と検証](docs/verification/metadata-source-map-20261012.md)。公開済み0.3.1のタグ・配布物は固定されており、この修正は未公開。
## AD-01/AD-02 host接続（2026-10-17）

`HostCall::truthy`で呼出し中の値をowned dataへ変換せずにLua truthinessを読みます。循環tableや関数の値を保持・実行しない引数参照APIです。hostの設定・同期・永続化はconsumerが所有し、公開工程とは区別します。

借用HTTP sink、Lua数値/integer coercion、u32返信port、破棄・失敗後の要求精算を追加。SDK既定キューを維持し、実通信・scene寿命・配送順はhostが所有する。

## v0.3.1 公開版の準備と検証（2026-10-12）

最新性能ブランチ、Addon実行/非短縮build/metadata、採用済み743件の描画契約を統合した。条件分岐のpanic/trap、部分/大量反復名mapとWASMオプション拒否を修正。Native871、SDK30、実WASM87、Playground/CLI38、3ブラウザ各16例、独立tarball consumerと基本ゲートが成功した。[候補の検証・性能・成果物](docs/verification/release-candidate-0.3.1.md)。

公開v0.3.0比、実コーパス成功組の全探索平均はMapなし48.9%、Mapあり60.8%短縮。既知7trapの全設定が成功し、残る8,175成功組の生成Lua・完全map・診断・探索が一致。最大Map比1.2目標は未達で、追加性能研究はv0.3.2以降へ送る。source manifestと検査済みtarball/ZIP、SHA256SUMS、release notes、ガイドpatchを永続領域へ保存した。ローカル候補の記録と正式配布物は区別する。公開commitの3 OS CI、producer確定後の成果物再検査、registryと本番の最終確認は[GitHub Release](https://github.com/Stormcat-Works/storm-lua-engine/releases/tag/v0.3.1)の検証記録で確認する。

## v0.3.1 印字・数値読取り・名前保存の検証完了（2026-10-05）

`3f342dc`・`5736d0b`・`f3e5c87`を検証し、[印字と名前保存の記録](docs/verification/compile-emission-20261005.md)へ保存した。直前の`627f8d1`と比較し、全探索4,054本の平均はMapなし1.9%、Mapあり0.7%短縮。1.2倍の全入力目標は未達で、負の再測定も記録している。Native841、SDK30、実WASM83、Playground/CLI38、3ブラウザーと完全map比較を確認。push・公開は行っていない。

## v0.3.1 候補評価と表記生成の追加改善（2026-10-05）

`627f8d1`までのAPI別名候補の改名AST削減、小整数表記、重複由来関係の確認を完了。[実装・回帰・実測](docs/verification/compile-latency-20261005.md)。比較元は`353522c`。同じ実コーパス全探索の平均ではMapなし8.2%、Mapあり6.8%短縮。平均比1.135倍を、個別の最大1.2倍の達成に置き換えない。未達・再確認でも残った負の差・採用しなかった試行は記録した。

Native836件、SDK30件、実WASM83件、Playground/CLI38件と3ブラウザー、固定240設定・実コーパス8,182成功組・全map一致を検証済み。今回この引継ぎ記録を保存する。実装はv0.3.1ブランチだけで、push・公開は行わない。

## v0.3.1 依存解析と構文比較の高速化（2026-10-05）

`353522c`までの関数summary依存の再利用、文字列snapshotなしの構文比較、安定aliasの借用、ASCII文字数計測を検証済み。[実装と実測](docs/verification/compile-summary-dependencies-20261005.md)。全探索4,054成功組の平均は前の検証済み版よりMapなし3.0%、Mapあり1.9%短縮。最大1.2倍は未達であり、負の反復観測も残している。

Native834、SDK30、実WASM83、Playground/CLI38、3ブラウザー、全mapを含む固定240設定・実コーパス8,182成功組を確認した。公開版ではなく、v0.3.1開発ブランチのcommitのみ。次のラウンドはこの同一binaryを基準にする。

## v0.3.1 全経路高速化の継続（2026-10-05）

`feat/minify-performance-v0.3.1`で、借用したAPI参照の分類、ASTの葉/演算子snapshot、1文字の短縮名、UTF-16座標cursorと字句識別子の共有を実装した。対象実装は`0de60afd5b4ee106a9cc48be3ad24c1c099fcbe5`。[実装・実測・回帰・未達条件](docs/verification/compile-continuation-20261005.md)。

比較元は前回の検査済み`a400c55`。今回のコーパス全探索4,054成功組の平均ではMapなし2.1%、Mapあり1.1%短縮、Map比は1.126。平均比を全入力の最大条件に置き換えない。反復した成功40条件の1.2倍以下は11、最大中央値比は4.779で、未達と負の観測を残す。

Native827、SDK30、実WASM83、Playground/CLI38件、3ブラウザー各16例、固定240構成と実コーパス8,182成功組で同一結果を確認した。ブラウザー試験では一時領域不足を確認し、同じ配布物を専用TMPDIRで再実行して成功。アプリ/依存/安全設定は変更していない。commitのみで、push・公開・main/release変更はしていない。

## v0.3.1 追加高速化の検証記録（2026-10-04）

前回報告後の実装 `f463b72`〜`a400c55` とその比較を[追加ラウンド](docs/verification/compile-next-round-20261004.md)へ保存した。819 Native、83実WASM、実コーパス8,182成功組と全マップの一致を確認。全探索の平均時間は直前版よりMapなし3.0%、Mapあり2.2%短縮したが、最大1.2倍は未達のまま。失敗した試案と負の観測を除外せず記録する。公開・pushは行っていない。

## v0.3.1 全コンパイル経路の高速化（開発中・2026-10-04）

`feat/minify-performance-v0.3.1`を維持し、Mapなし/ありの両方を速くする改善を`2741357`〜`1f9d58e`へcommitした。[計画](docs/design/compile-all-scenes-v031.md)、[全条件の実装・測定・検証](docs/verification/compile-all-scenes-20261004.md)。**push・公開・main/releaseへの反映はしていない。**

blockが変わらない区間の束縛/効果解析共有、借用トークン、parse結果の診断/探索/buildへの再利用、使い終えた位置indexの早期解放、字句短縮専用経路の不要な由来生成回避、標準mapと理由JSONの一回出力を実装した。生成Lua・候補選択・診断・schema1の意味と精度を維持する。

直前のAPI別名改善版との全探索の成功4,054本比較では、Mapなし平均が24.6%、Mapあり平均が22.9%短縮した。全コーパスは各variant1回、別に44条件の反復と小projectの追加反復を実施した。**最大1.2倍（理想1.1倍）は未達**。成功40条件の最大中央値比は4.594倍で、小さい原文返却・字句短縮などの固定費を残す。通常経路の負の観測も保存し、平均値だけで全シーン達成としない。

Native 806件、SDK 30件、実WASM 81件、Playground/CLI 38件、3ブラウザー各16例、独立パッケージ導入を検証した。固定240設定と未達target、全4,155原文の成功/失敗境界、新旧reader、Worker混成を確認。既知の全探索WASM trap7原文は別に確認し、今回修正したとは扱わない。

先行するAPI別名候補の改善は`153a8c9`、基盤は`d3bd35c`。そのときの平均Map追加時間66.3%削減は[別の比較](docs/verification/map-delta-reduction-20261004.md)であり、今回の効果へ再加算しない。v0.3.1はまだ未公開。

## v0.3.0 公開完了（2026-10-02）

`feat/minify-source-provenance-v0.3`で全67パスの内部由来対応に加え、最適化理由付きSource Map v3をRust/WASM/TSの開発APIへ接続した。`sourceMap:true`で`map`を返し、`validateSourceMap(code,map)`で内容指紋と詳細/標準mapの整合性を確認する。通常/LifeBoatの複数ファイル合成と、Worker経由の取得も対応。[実装・検証・性能](docs/verification/optimization-explanations-20261001.md)。

旧実測例で課題だった保持演算子/keywordのToken位置、identity/copy契約、型付き関連元、インライン化された子の文脈、原文の除去/置換記録を実装した。[更新した実測例](docs/verification/provenance-examples-20261001.md)。Source Map本体のversion=3とx_storm.schemaVersion=1を分離し、producerのエンジン版・revision・dirty、code/source hashと実効設定を保存する。

全67パスの対応と、固定30入力・240設定のUnknown 0%/生成Lua一致は維持する。理由コードと構造操作は保持するが、全パスの適用根拠を形式的に証明するものではない。basis/factsの詳細は実際に記録した判断のみで、未記録の根拠を推測しない。Source/Derivedの名前・式・文などの精度と、Synthetic/Unknownを区別する。

**P0〜P5とリリース前の利用検証・梱包・互換性棚卸しを完了した。** identity/copyの文字選択・カーソル・runtime行の範囲を正確に保つ仕上げも完了した。Unicode/CRLFと読み取り専用欄のキーボード操作、保存復元を3ブラウザーで再確認。[最終確認](docs/verification/p5-final-selection-20261001.md)。 Playgroundで元snapshot/生成Luaの双方向選択、複数由来と理由・除去記録を表示し、実VMのpause/step/log/errorへ接続する。実行時に生成行しか得られない場合は列を捏造せず候補を示す。入力編集後の旧map、保存されたVM結果と新しい実行を混同しない。[P5の完成確認](docs/verification/p5-playground-20261001.md)、[アプリ契約](docs/specs/playground-source-maps.md)。

CLI/Webの16確認例、workspace export/import、IndexedDBの大容量map保存・復元を検証済み。v0.3.0向けの[限定性能ラウンド](docs/verification/source-map-performance-20261002.md)を完了した。サイズ比較にしか使わない候補での由来処理を減らし、生成Luaとschema1の内容を維持。追加改善はv0.3.1以降へ送る。元変数値・寿命・bytecode PC・消えた実frame・loop反復の動的復元は別機能として未実装。**v0.3.0をnpm・GitHub・Playgroundへ公開し、registry再導入と本番3ブラウザーの確認まで完了した。** npm latestは0.3.0。[公開記録](docs/verification/release-0.3.0.md)に対象commit・配布物・実行結果を残す。[確定設計](docs/design/optimization-explanation-schema.md)、[公開スキーマ契約](docs/specs/optimization-map-extension.md)。

## v0.2.1 公開状況

2026-09-30: GitHub Release・タグ・検査済みSDK tarballとPlaygroundの本番配備は完了した。**npmもTrusted Publishingで公開し、latest=0.2.1とregistry再導入を確認済み**。未達targetはfast/beam設定によらず最大探索と同一出力を返す。513 Native、実WASM、3 OS CI、716未達比較、本番3ブラウザを確認。[公開完了記録](docs/verification/release-0.2.1.md)。

npm公開workflowは実公開まで成功した。[run 36667537171](https://github.com/Stormcat-Works/storm-lua-engine/actions/runs/36667537171)のattempt 1で公開し、attempt 2で同一版の再公開を省略してregistry再取得・consumerを確認した。[自動化の検証記録](docs/verification/publication-workflow-20260930.md)。v0.3.0は専用branch `feat/minify-source-provenance-v0.3`で開発中であり、本公開とは分離する。

以下の0.2.0と統合ブランチの記述は過去の実装・検証記録であり、現在の公開状態は本節が正本である。

2026-09-27 — **Storm Lua Engine v0.2.0をnpm/GitHubへ公開し、Playground・ガイド・2記事の本番公開まで完了。** 版番号はworkspace、npm SDK、Playgroundとも0.2.0です。範囲と公開手順は[release](docs/release.md)、残件は[TASKS](TASKS.md)、利用ガイドは[docs.makkii.jp](https://docs.makkii.jp/storm-lua-engine/index)です。

## 後続ブランチの作業（未公開）

`feat/storm-code-integration`でログ発生位置を実装・検証済み。Rustの生成時記録、WASMの構造化配送、TSのoptionalなlocationを接続する。契約は[runtime-host](docs/specs/runtime-host.md#ログ発生位置未公開の後続版)。これは公開済みv0.2.0の機能ではなく、利用側IDEのログ表示への接続はconsumer側の対象。最適化後ソースマップはこの作業に含めない。

検証: Native478件、実WASM53件、SDK JS29件、3ブラウザ、隔離パッケージconsumer、fmt/clippy/default check/rustdoc/architectureを通過。[記録](docs/verification/log-locations-20260929.md)。

## consumer実行ホストの接続準備（未公開）

同じ専用ブランチで、名前付きVehicle tick/draw、extended限定controlNamespace、更新後propertyスナップショット、analyzeのruntime/build区分を追加。Designと通常実行で標準APIを再実装せず利用するための公開操作。新しい状態操作を行った場合だけ固定I/Oへ戻し、描画やloadが未消費のホスト入力を上書きしない。識別子位置で記号や予約語を受理していた既存構文不具合も修正した。詳細はruntime-host仕様。[検証記録](docs/verification/vehicle-host-controls-20260929.md): Native487、WASM60、SDK JS30、3ブラウザ、梱包consumerと各静的ゲート通過。

## v0.2.0に含む実装

| 領域 | 現在の範囲 |
| --- | --- |
| Runtime | Vehicle/Addon、信号・プロパティ、描画、ホストデバッガ、保存、明示HTTP・地図・server接続 |
| Compiler | Vehicle用syntax/analysis/minify/buildと独立WASM・TS入口。Storm MinのCLI/Webは同じSDKを利用 |
| 環境 | gameが既定、extendedは明示選択。debug.logは両環境、print等はextended。ログ配送先と関数公開を分離 |
| 正確性 | 4パス撤去、外部グローバル保持、_ENVと拡張環境の字句短縮、捕捉値の寿命と不正finalizer入力の修正 |
| ホスト拡張 | 初期化前のvalues/functions bindings、reset/reloadで再適用、専用compiler Worker接続 |
| ソース読み込み | extended専用のLB方式requireLoader。Vehicleは正常完了したload履歴をresetで再実行 |
| 非短縮Source Map | game向けプロジェクトをbuild(minify:false)し、元ファイルの停止位置・エラー位置へ戻す。行単位、合成行は原文位置を捏造しない |
| Playground | app/のCLI/Web、13確認例、明示Worker、入力・結果保存、入出力、中断。Storm Minと併存。Addon Labは撤去済み |
| 文書 | 利用ガイドはdocs-site、契約・設計・検証・実行例は本リポ。ブログの初回版/次版記事も公開済み |

## 候補の最終確認

実装commit `8707a80`に対する41種の最終ゲートがすべて成功しました。Engine Native472件、WASM48件、SDK JS27件、Playground15件＋3ブラウザ各13例、Storm Min Rust492件／独立JS25件と新しいNative/WASM400条件を再検証。[0.2.0 candidate](docs/verification/release-candidate-0.2.0.md)と[機械記録](docs/verification/release-candidate-0.2.0.json)へ集約しています。過去の件数や出力一致を、新しい候補の実行結果として使い回していません。

既存の検証記録: [source loaderと0.2準備](docs/verification/source-loading-20260927.md)、[環境修正](docs/verification/environment-contract-20260926.md)、[Playground](docs/verification/playground-20260926.md)。初回移管の685条件一致はその時点の証跡であり、後続の正確性修正後の出力保証ではありません。

## 公開・自動配備

[公開検証記録](docs/verification/release-0.2.0.md)と[機械記録](docs/verification/release-0.2.0.json)に、GitHub Actionsの3 OS/WASM、公開済みnpmの新規導入、本番3ブラウザ各13例、ガイドとブログの応答を集約しています。SDKタグv0.2.0とtarballは固定し、公開後に差し替えていません。

Playgroundは専用Workerで公開し、releaseブランチへのpushからCloudflare Workers Buildsでソースビルド・検証・自動配備します。main/developのpushでは配備せず、npm publishは別の明示工程です。配備後のCSP調整はappのHTML nonceと専用パスのゾーン設定で行い、SDKのLua実装は変更していません。

consumerの公開SDK参照は専用ブランチへpush済みですが、consumer側のremote CIは未実行です。Engineの公開CIは成功済みで、両者を混同しません。

**最適化後Source Mapはv0.2.5またはv0.3.0へ分離**します。由来情報を考慮せず実装された最適化器の大規模変更であり、v0.2.0の完成条件に含めません。Addonコンパイラ、require方式の統合・gameとloaderの分離、追加map API、全consumerの移行完了も今回の必須条件ではありません。

初回公開の事実と当時の測定は[0.1.0検証記録](docs/verification/release-0.1.0.md)を参照してください。


## consumer統合API（未公開）

共有parserによるsource inspection、独立したLBビルド、開発用overflow viewportを実装・検証。[記録](docs/verification/storm-code-integration-20260930.md)。通常build、通常game描画、最適化探索、minify後mapは変更しない。

## 最適化後位置追跡の基盤（未公開・別作業ブランチ）

`feat/optimized-source-maps`で[計画](docs/design/source-provenance.md)のP0/P1を実装。未達target探索は共通コアと一意候補を再利用する。非短縮の通常/LifeBoatビルドはUTF-8コピー範囲とトークン/UTF-16列のmapを保持し、合成prefix・削除済み開発区間に元位置を付けない。診断の列と終端も同じ範囲から変換する。

最適化ASTの由来引き継ぎ、最終minify出力のmap、元変数復元、Playgroundの双方向表示は未実装。`minify`と`build(minify:true)`がmapを返すとは扱わない。公開・版上げ・他ブランチの置換は行っていない。実行済みテスト・性能比較・未実装範囲は[検証記録](docs/verification/source-provenance-20260930.md)に記録する。
Native host APIs now support a shared CRT random provider and synchronous protected Addon event delivery. Lua conversion, sampling and execution budgets remain Engine-owned; the host owns scene state/order/checkpoint. The independent native sampler fixture covers 120 random and 27 seed cases. Isolated defaults are unchanged. This unpublished integration does not establish complete simulation-context acceptance.

## single-use-global-forwarding の制御文内上書きへの誤転送修正（2026-10-30）

function-globalization で再利用されたスロットに signed-expression-factoring の
キャリアが入る場合、ガード読み→本体内の上書き→上書き後の読みを含む If 文に
対し、single-use-global-forwarding が上書き前の値を上書き後の読みにまで転送し、
描画座標が変わるバグがあった（差分実行で検出。コーパス該当ファイルはハッシュ名
3dfd83… のみで同定）。修正は制御文が対象への書込みを含む場合にその文を使用文と
せず、書込みのみの制御文は後続使用への障壁とする。素の Assign 文は RHS 評価が
書込みに先行するため従来通り。回帰テスト 2 件を追加。
