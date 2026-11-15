/**
 * ホストの関数（bindings）に届くLuaの文字列は、string ではなく Uint8Array です。
 * Luaの文字列は任意のバイト列で、UTF-8とは限らないためです。
 * 文字として読むなら TextDecoder か luaText で直し、バイト列として扱うならそのまま使います。
 */
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {loadRuntime,luaText} from '@stormcat-works/storm-lua-engine';
const wasmBinary=await readFile(new URL(import.meta.resolve('@stormcat-works/storm-lua-engine/wasm/storm_lua_wasm.wasm')));
const engine=await loadRuntime({wasmBinary});
// fatal を付けると、UTF-8でないバイト列を置換文字にせずエラーにする。
const decoder=new TextDecoder('utf-8',{fatal:true});
const received=[];
const vm=engine.createVehicle({environment:'extended',bindings:{functions:{
  // 文字として読む形。Luaの文字列は Uint8Array、整数は bigint、小数は number で届く。
  'host.label':(text,size)=>{
    assert.ok(text instanceof Uint8Array);
    // そのまま文字列へ連結すると、"49,120,49" のような数字の並びになる。
    received.push({joined:String(text),decoded:decoder.decode(text),size});
    // 戻り値は必ず配列。JSの string はUTF-8のLua文字列になる。
    return [`[${luaText(text)}]`];
  },
  // バイト列のまま扱う形。UTF-8でないデータも失わない。
  'host.checksum':payload=>{
    assert.ok(payload instanceof Uint8Array);
    assert.throws(()=>decoder.decode(payload),TypeError);
    let sum=0;for(const byte of payload)sum+=byte;
    // Uint8Array を返すと、同じバイト列のLua文字列になる。
    return [sum,payload.slice().reverse()];
  },
}}});
try{
  vm.load(`
local framed = host.label("1x1", 3)
local sum, reversed = host.checksum(string.char(255, 0, 65))
function onTick()
  output.setNumber(1, #framed)
  output.setNumber(2, sum)
  output.setNumber(3, reversed:byte(1))
  output.setNumber(4, reversed:byte(3))
end`,'@main.lua');
  vm.tick();
  assert.deepEqual(received,[{joined:'49,120,49',decoded:'1x1',size:3n}]);
  assert.deepEqual(Array.from(vm.io.outputNumbers.subarray(0,4)),[5,320,65,255]);
  console.log('Host-binding bytes consumer: Lua strings arrived as Uint8Array, decoded as text and kept as raw bytes.');
}finally{vm.dispose();}
