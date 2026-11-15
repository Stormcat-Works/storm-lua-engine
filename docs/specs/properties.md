# Vehicle property contract

## Values and ownership

specが`PropertyBag`/`PropertyValue`を所有し、microcontrollerがLua getterを登録する。UI表示名、slider範囲、dropdown選択肢、ファイルの探索順と保存先はホスト側。

Numberはf64、Booleanはbool、Textとlabelはbytes。labelはcase-sensitive。Compositeのf32境界をpropertiesへ自動適用しない。実ゲームのProperty Numberの保存精度は未確定であり、このf64保持を実ゲーム確認済みという意味で使わない。

Rustは`PropertyBag::insert/get/remove`、TSは`{Gain:1.5,Enabled:true,Title:"TEXT"}`のrecord、またはbyte labelを含むtyped entry配列を使える。TS→WASMのcold codecはf64を16桁のIEEE754 hex、text/labelをbyte配列とする。NaN/Infinity/-0/NUL/非UTF-8をJSONの暗黙変換で失わない。壊れたJSON・重複label・型不一致のホスト設定はエラー。

## Execution

VM生成後・load前に設定する。トップレベルの`property.getNumber`で初期値を読める。1callback中は同じbagを参照し、Running/Suspended時の更新はBusyとして拒否する。全設定をparse/検証してから一括置換し、部分更新しない。

### 1件の設定と削除

`set_property`／`setProperty`は、指定したラベルの1件だけを設定し、他の項目を変えない。同じラベルの値があれば、型が違っても置き換える。ラベルはバイト列のまま比較するので、ホストが文字列へ直して探す必要はない。TSの文字列ラベルはUTF-8のバイト列にしてから渡す。値はrecordと同じ素の値（number、Boolean、string、`Uint8Array`）か、型を明示した`PropertyValue`で指定する。

削除は別の操作`remove_property`／`removeProperty`で行い、その項目があったかどうかを返す。存在しないラベルの削除はエラーにしない。設定のAPIは削除を受け付けず、TSの`setProperty`へ`null`や`undefined`を渡すと`TypeError`になる。ホストの値が欠けていたときに、項目を気づかないうちに消さないためである。空のTextの設定と削除は別の結果になる。`control_namespace`のLua関数`setProperty`がnilで削除する規則は、Luaから呼ぶ開発用操作の規則であり、変えていない。

受け付ける時点は一括設定と同じで、アイドル中だけである。Running/Suspended時はBusyで拒否し、プロパティを変えない。`control_namespace`を設定したVMでは、設定後の件数と合計バイト数を先に検査し、4096件または1MiBを超える設定は何も変えずにLimitで拒否する。WASMは`sle_vehicle`の操作`setProperty`と`removeProperty`で受け、1件のwire表現は一括設定のentryと同じである。

Luaのlocalへ既にキャッシュした値は書き換えない。resetは現在のpropertiesでVMを再構築し、保持sourceを再実行する。標準Luaテーブルにはgetterのみを置き、勝手にsim.setPropertyを追加しない。

## Getter behavior and evidence

現在のengine getterは、欠損または値型違いをNumber=0、Bool=false、Text=空bytesとして扱う。これはエンジンの明示規則であり、実ゲームの全異常系を測定済みではない。hostの不正JSONやバッファ破損までこの既定値へ隠蔽しない。

f64保持、case/bytes、initialization順、cached localとreset、停止中の更新拒否、1件の設定と削除はNative/WASMのテストで検証する。実ゲームの保存精度とgetterの異常系oracleは[TASKS](../../TASKS.md)に残す。

Addonのmenu propertyは別APIであり、[Addon仕様](addon.md)を参照。vehicleのsetProperties、setProperty、removePropertyをAddonへ流用しない。
