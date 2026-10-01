# Playground Source Map検査・実行契約

対象はv0.3.0。Compilerの[理由付きmap](optimization-map-extension.md)を利用するアプリ側の契約であり、コンパイラやLua意味論を再実装しない。[P5採用方針](../design/playground-source-map-inspector.md)。

## 入力と生成物

元入力の編集と、生成物に埋め込まれた元snapshotは別のデータである。入力を編集しても既存生成物の原文を書き換えず、「入力変更・過去の生成物」と表示する。新たな対応は再コンパイルでのみ作る。生成物のコードとsnapshot欄は読み取り専用。

確認例はSource Mapの説明・元moduleへの対応・停止/ログ/エラーを含む16種類。説明例はコンパイルとmap検査だけでLuaを実行しない。通常/LifeBoatの複数ファイルは既存のSDK操作列から指定する。単一入力は「入力を最適化 + map」「入力を非短縮build + map」でも検査できる。最適化ボタンの数値モードはexact。許容近似やtargetSize等の設定はSDK操作列へ明示する。

生成物JSONは`{kind:'storm-lua-artifact',version:1,code,map}`。インポート時、SDKの`validateSourceMap`で標準部分・拡張・コード・snapshotの内容を照合する。`.map`だけを編集中ソースに暗黙に取り付けない。Luaと.mapの単独ダウンロード、および組のJSONダウンロードを提供する。

## 双方向の選択

生成範囲の選択は、検証済みの非重複mappingsを照合する。原文の選択はcopy/primary/型付きrelated/inline call siteの明示的な範囲を検索する。複数の対応は一覧から選ぶ。表示の候補ページングは全候補を保持し、見えていない候補を削除しない。

逆引きでは、同じ選択に対するより小さい元範囲や明示的なdispositionがある場合、これらを完全包含する粗い範囲を候補から外す。関数全体に対応した括弧が、その関数内の削除済みの行を実行しているかのように表示しないためである。親constructの説明は詳細欄から参照できる。この優先付けは元コードを再解析した停止可能性の判定ではない。

Source/Derived/Synthetic/Unknownと精度name/token/expression/statement/groupを別々に表示する。Unknownには元位置を付けない。SyntheticをUnknownの別名にせず、実際の生成理由を表示する。copyのみ区間内部を等バイトの差分で対応付ける。生成→原文でも原文→生成でも、copy区間は選択範囲との交差だけに限定する。カーソルは正確な一点に対応させる。原文をそのまま返すidentityがファイル全体の1区間であっても、選択やruntime行を原文全体へ拡張しない。

textareaの選択位置はUTF-16で、CRLFが画面上でLFへ正規化される。原文snapshotは正規化せずUTF-8 byte座標を維持し、明示的な座標変換表を使用する。サロゲート/UTF-8文字途中の選択は拒否する。読み取り専用欄でも方向キー・Home/End・Shift範囲選択が各ブラウザーで同じ論理位置を移動するよう明示処理し、文字の変更を許可しない。画面表示の行と列は1-based、範囲の終端は除外する。

## 理由・関連元・削除

選択された区間の主な元範囲に加え、definition/callSite/argument/parameterUse/useSite/contribution、inlineの定義と呼び出し、内包するconstructの理由を表示する。理由のcode/operation/before/after/basis/factsは生成時の記録そのもの。basisがない場合、現行ルールや最終コードから適用根拠を推測しない。

採用候補・実効設定・producer・内容識別も折りたたみ表示で確認できる。数値bitsや長いliteralをUIのJavaScript数値へ評価し直さない。dispositionは元の範囲が候補内で除去/置換された記録で、コピー先や実行可能地点の完全な一覧ではない。除去記録だけを根拠に元コードの全出現が消滅したと扱わない。

## 実行・停止・エラー

「生成物をVMへロード」で初めて、検証した正確な生成Luaを独立VMへロードする。SDKは専用Workerで実行し、chunk名とロード世代を固定する。次の生成物をロードする際は古いVMを破棄する。map integrityが異なる実行要求は拒否する。

1tick、draw、continue、into/over/out、Number/Boolean入力、ロード時のproperty指定を提供する。元範囲から求めたbreakpointは生成行の候補で、VMが停止できることを保証しない。実際に`suspended`となった結果だけを停止として表示する。生成候補がない元範囲に近い行を勝手に設定しない。

この生成物VMは記録されたgame/extended環境を使用する。任意のhostBindingsを要求する成果物は明示拒否する。その用途はSDK操作列で実ホスト定義を明示して実行する。ネットワーク要求を自動転送しない。

stack、ログのlocation、既知chunkに一致するruntime errorの生成行を受け取ったら、同じ生成行にあるすべての対応候補を検索する。**列がない情報をcolumn=0へ置換しない。** 元の複数行へ対応する場合は「列不明・複数の元位置」と表示する。未知chunkのframeや位置形式のないエラーは、そのまま保持し未対応とする。

VMの値・stackは生成コードの状態である。元の変数値/保存先/寿命、消えた実frame、bytecode PC、生成loopの特定反復を復元するものではない。watchによる元式の再評価を復元の代わりに行わない。

## 保存と持ち運び

IndexedDBの`storm-lua-playground` databaseの単一workspace recordを正本にする。入力・環境・操作列・結果・monitor・生成物・選択範囲・候補選択・実行入力・実行結果を保持する。大きなmapをlocalStorageへ二重保存しない。再読み込みでVMは作り直さず、保存した実行結果を「VMは未ロード」と表示する。

端末内レコードは`kind:'storm-lua-playground-state',version:2`。旧localStorageのv1は自動移行せず、元レコードの救出または有効な入力の読み込みを案内する。未読/不正な保存データを自動的に上書きしない。

持ち運びは`kind:'storm-lua-playground-workspace',version:1`でproject・inspection・history・lastFrameを含める。既存の入力専用`storm-lua-playground` version1は引き続き明示的に受理する別の形式であり、旧端末stateの変換機構ではない。インポートはproject、選択、frame長、map、保存runtimeとの組み合わせを検証してから反映する。失敗時は既存入力と生成物を維持する。

実行を伴う状態の復元や、外部ファイルの自動読込はしない。原文全文・固定property値・ログが含まれ得るため、書き出し前に共有先を判断する。ユーザー文字列はtextContent/textareaとして表示し、HTMLやLuaとして評価しない。

## CLIと実行検証

`minify FILE --source-map`でmap付き結果JSONを出力する。`map-inspect ARTIFACT_FILE GENERATED_BYTE`で生成物JSONを検証し、元範囲を表示する。この操作はLuaを実行しない。`--project`はWebのworkspaceまたは入力projectを検証し、新しいセッションでその操作列だけを実行する。保存されていたVMは継続しない。`--jsonl`にもinspectMap/mappedLoad/mappedAction/buildLifeboatを提供する。

アプリ単体テストは実Compiler/Runtime WASMを使う。ブラウザー検証はChromium/Firefox/WebKitで双方向選択、理由/文脈/除去、元module、保存/export/import、誤った組の拒否、実際の停止/ステップ/ログ/エラーと中断を確認する。大容量mapのIndexedDB往復とUnknownの意図的な入力も別に検証する。試験件数・対象commit・結果はverificationへ記録する。
