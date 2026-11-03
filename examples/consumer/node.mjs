/** ローカルでパックされたnpmパッケージをインストールしたコンシューマ環境から実行します。 */
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {loadRuntime,luaTable,luaText,luaField,encodeSavedata,decodeSavedata} from '@stormcat-works/storm-lua-engine';

const wasmBinary=await readFile(new URL(import.meta.resolve('@stormcat-works/storm-lua-engine/wasm/storm_lua_wasm.wasm')));
const engine=await loadRuntime({wasmBinary});
const output=[];
const onLog=record=>{
  // アプリケーションの表示境界でデコードを選択します。record.bytes はロスレスのまま保持されます。
  const text=new TextDecoder('utf-8',{fatal:true}).decode(record.bytes);
  output.push({source:record.source,text});
  console.log(`[${record.source}] ${text}`);
};
const vehicle=engine.createVehicle({properties:{Gain:2},onLog,mapProvider:request=>{
  // デモ用ホスト地形: ラベル付きテストグリッドであり、ゲームに忠実な地形レンダラではありません。
  const pixels=new Uint8Array(request.width*request.height*4);
  const water=request.colors.ocean??[16,48,80,255];
  for(let y=0;y<request.height;y++)for(let x=0;x<request.width;x++){
    const color=(x%8===0||y%8===0)?[96,128,160,255]:water;
    pixels.set(color,(y*request.width+x)*4);
  }
  return pixels;
}});
try {
  vehicle.load(`local gain=property.getNumber("Gain")
function onTick() output.setNumber(1,input.getNumber(1)*gain);debug.log("control tick") end
function onDraw() screen.setMapColorOcean(16,48,80);screen.drawMap(0,0,1);screen.setColor(255,255,255);screen.drawText(1,1,"MAP") end`);
  vehicle.io.inputNumbers[0]=3.5;
  assert.equal(vehicle.tick(),'completed');assert.equal(vehicle.io.outputNumbers[0],7);
  assert.equal(vehicle.draw(32,32),'completed');assert.equal(vehicle.frame().copy().length,4096);
} finally {vehicle.dispose();}

const announcements=[];
const addon=engine.createAddon({properties:{Enabled:true},onLog,server:{
  getPlayers:()=>[{kind:'table',entries:[[1n,luaTable({id:7n,name:'Ada'})]]}],
  announce:(title,message)=>{announcements.push([luaText(title),luaText(message)]);return [];}
}});
try {
  addon.load(`g_savedata={ticks=0,enabled=property.checkbox("Enabled",true)}
function onCreate(new)
 if g_savedata.enabled then
  for _,player in pairs(server.getPlayers()) do server.announce("Welcome",player.name) end
 end
 server.httpGet(8080,"/status");debug.log("addon started",new)
end
function onTick(game_ticks) g_savedata.ticks=g_savedata.ticks+game_ticks end
function httpReply(port,request,reply) g_savedata.reply=reply end`);
  assert.equal(addon.start(),'completed');addon.tick(400);
  assert.deepEqual(announcements,[['Welcome','Ada']]);
  for(const request of addon.drainHttpRequests()){
    // この実行可能サンプルでは実際のネットワーク呼び出しは不要です。
    // 実際のホストはポート/パスを認証し、選択したトランスポートを実行して、返信の配信をキューに入れます。
    assert.equal(request.port,8080);assert.equal(luaText(request.request),'/status');
    addon.httpReply(request.token,'OK');
  }
  const portable=encodeSavedata(addon.savedata());
  const checkpoint=decodeSavedata(portable);
  assert.equal(luaField(checkpoint,'ticks'),400n);assert.equal(luaText(luaField(checkpoint,'reply')),'OK');
  addon.reload(checkpoint);addon.start();addon.tick(1);
  assert.equal(luaField(addon.savedata(),'ticks'),401n);
  for(const request of addon.drainHttpRequests())addon.cancelHttp(request.token);
  addon.destroy();
} finally {addon.dispose();}
assert.ok(output.some(record=>record.text==='control tick'));
assert.ok(output.some(record=>record.source==='debug.log'));
console.log('Consumer example passed: vehicle, addon, host terrain, server calls, logs and save/reload.');

// The compiler is independently loaded, then its artifact is explicitly run.
const {loadCompiler}=await import('@stormcat-works/storm-lua-engine/compiler');
const compiler=await loadCompiler({wasmBinary:await readFile(new URL('./compiler-wasm/compiler_bg.wasm',import.meta.resolve('@stormcat-works/storm-lua-engine/compiler')))});
const compiled=compiler.minify('function onTick()output.setNumber(1,input.getNumber(1)*2)end',{target:'vehicle',numericMode:'exact'});
assert.equal(compiled.ok,true);
const compiledVm=engine.createVehicle();
try {
  compiledVm.load(compiled.code);compiledVm.io.inputNumbers[0]=4;compiledVm.tick();
  assert.equal(compiledVm.io.outputNumbers[0],8);
} finally {compiledVm.dispose();}
assert.equal(new Set(compiler.passIds()).size,73);
console.log('Installed compiler subpath: minification, explicit runtime load and output 8 passed.');

const snapshot=compiler.build({entry:'main',modules:{main:"local m=require('m') function onTick()output.setNumber(1,m.read())end",m:"local gain=property.getNumber('Gain') return {read=function()return gain end}"}},{target:'vehicle',numericMode:'exact',minify:true});
assert.equal(snapshot.ok,true);
const snapshotVm=engine.createVehicle({properties:{Gain:2}});
try {
  snapshotVm.load(snapshot.code);snapshotVm.tick();assert.equal(snapshotVm.io.outputNumbers[0],2);
  snapshotVm.setProperties({Gain:9});snapshotVm.tick();assert.equal(snapshotVm.io.outputNumbers[0],2);
} finally {snapshotVm.dispose();}
console.log('Installed compiler/runtime WASM: captured property remains 2 after host property changes to 9.');
