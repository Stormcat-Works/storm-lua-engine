/**
 * フレームが前回と同じかどうかを、毎回 copy() せずに確かめる例。
 * 借用ビュー（pixels）を前回の写しと比べ、変わったときだけ copy() します。比較は厳密です。
 * Buffer.compare は Node 専用です。ブラウザでは、同じ手順の比較を自分のループで書きます。
 */
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {loadRuntime} from '@stormcat-works/storm-lua-engine';
const wasmBinary=await readFile(new URL(import.meta.resolve('@stormcat-works/storm-lua-engine/wasm/storm_lua_wasm.wasm')));
const engine=await loadRuntime({wasmBinary});
const vm=engine.createVehicle();
/** 前回送った画素の写し。pixels は次の描画で無効になるので、保持するのは copy() の結果だけ。 */
let sent=null,width=0,height=0;
/** 描画のあとで呼ぶ。内容か寸法が変わっていれば、新しい写しを返す。同じなら null。 */
function changedFrame(){
  const frame=vm.frame();
  // pixels はWASMメモリの借用で、写し取りは起きない。この呼び出しの中だけで使う。
  const now=frame.pixels;
  if(sent!==null&&frame.width===width&&frame.height===height&&Buffer.compare(now,sent)===0)return null;
  sent=frame.copy();width=frame.width;height=frame.height;
  return sent;
}
try{
  vm.load('function onTick() level=input.getNumber(1) end\nfunction onDraw() screen.setColor(0,255,0) screen.drawRectF(0,0,level,4) end');
  const sizes=[];
  for(const level of [3,3,3,5,5,3]){
    vm.io.inputNumbers[0]=level;vm.tick();vm.draw(32,32);
    const frame=changedFrame();
    sizes.push(frame===null?null:frame.length);
  }
  // 6回の描画のうち、内容が変わった3回だけ写しができる。
  assert.deepEqual(sizes,[4096,null,null,4096,null,4096]);
  // 寸法だけが変わった場合も、変化として扱う。
  vm.draw(64,32);assert.equal(changedFrame()?.length,8192);
  vm.draw(64,32);assert.equal(changedFrame(),null);
  // 写しはJS側の所有で、次の描画のあとも読める。
  vm.draw(32,32);assert.equal(sent.length,8192);assert.deepEqual(Array.from(sent.subarray(0,4)),[0,255,0,255]);
  console.log('Frame-change consumer: borrowed-view comparison copied 4 of 8 frames and kept owned snapshots valid.');
}finally{vm.dispose();}
