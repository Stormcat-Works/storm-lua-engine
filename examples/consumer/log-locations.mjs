/** Named-source logs from an independently installed SDK, without exposing Lua debug APIs. */
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {loadRuntime,luaText} from '@stormcat-works/storm-lua-engine';
const wasmBinary=await readFile(new URL(import.meta.resolve('@stormcat-works/storm-lua-engine/wasm/storm_lua_wasm.wasm')));
const engine=await loadRuntime({wasmBinary});
const logs=[];
const vm=engine.createVehicle({environment:'extended',onLog:record=>logs.push(record),
  requireLoader:()=>({name:'@lib/logger.lua',source:'function helper()\n  debug.log("helper")\nend'})});
try{
  vm.load('print("initial")\nrequire("lib")\nfunction onTick()\n  helper()\nend','@main.lua');
  vm.tick();
  assert.deepEqual(logs.map(record=>record.location),[{chunk:'@main.lua',line:1},{chunk:'@lib/logger.lua',line:2}]);
  for(const record of logs){
    const position=record.location?`${record.location.chunk}:${record.location.line}`:'(location unavailable)';
    console.log(`${position} [${record.source}] ${luaText(record.bytes)}`);
  }
}finally{vm.dispose();}
