/** コンシューマ公開面のコンパイル時チェック。このファイルが実行されたりパッケージングされたりすることはありません。 */
import { type LuaEngine, type LogLocation, type MapProvider, luaTable } from '../../src/index.js';
declare const engine: LuaEngine;
const vehicle=engine.createVehicle({onLog:record=>{
  const source:'print'|'debug.log'=record.source;void source;
  const location:LogLocation|undefined=record.location;
  if(location){const line:number=location.line;const chunk:string=location.chunk;void line;void chunk;}
}});
const addon=engine.createAddon({server:{getPlayers:()=>[luaTable({})]}});
const vehicleMode:'vehicle'=vehicle.mode;
const addonMode:'addon'=addon.mode;
void vehicleMode;void addonMode;
vehicle.io.inputNumbers[0]=1;
addon.tick(400);addon.dispatch('onChatMessage',[1n,'name','text']);
// @ts-expect-error アドオンモードには Composite I/O がありません。
addon.io.inputNumbers[0]=1;
// @ts-expect-error アドオンモードには onDraw/フレームのライフサイクルがありません。
addon.draw(32,32);
// @ts-expect-error ビークルモードにはアドオンのチェックポイントがありません。
vehicle.savedata();
// @ts-expect-error ライフサイクルイベントを通常のコールバックとしてディスパッチすることはできません。
addon.dispatch('onCreate',[true]);
// @ts-expect-error ホストサーバーのコールバックは同期的な結果リストでなければなりません。
engine.createAddon({server:{getPlayers:async()=>[luaTable({})]}});
// @ts-expect-error 地形プロバイダが Promise を返すことはできません。
const asyncMap:MapProvider=async()=>new Uint8Array();
void asyncMap;

const harness=engine.createVehicle({environment:'extended',controlNamespace:'controls'});
harness.callTick('setup',[1n,'text']);harness.callDraw('preview',32,32,[true]);
for(const property of harness.properties()){const bytes:string|Uint8Array=property.label;void bytes;}
// @ts-expect-error Named Vehicle drawing is not an Addon operation.
addon.callDraw('preview',32,32);
