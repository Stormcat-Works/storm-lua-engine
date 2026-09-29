import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {loadRuntime} from '../../dist/index.js';
import {loadCompiler} from '../../dist/compiler.js';
import {TraceMap,originalPositionFor} from '@jridgewell/trace-mapping';
const engine=await loadRuntime({wasmBinary:await readFile(new URL('../../dist/wasm/storm_lua_wasm.wasm',import.meta.url))});
const at=(chunk,line)=>({chunk,line});

test('actual WASM logs carry named source positions for aliases, includes, tick, draw and reset',()=>{
  const logs=[];
  const vm=engine.createVehicle({environment:'extended',onLog:r=>logs.push(r),
    requireLoader:()=>({name:'@lib/ログ.lua',source:'function helper()\n  debug.log("helper")\nend'})});
  try{
    vm.load('local say=print\nprint("top")\nrequire("helper")\nfunction onTick()\n helper()\n say("tick")\nend\nfunction onDraw()\n debug.log("draw")\nend','@main.lua');
    assert.deepEqual(logs[0].location,at('@main.lua',2));
    logs.length=0;
    vm.tick();vm.draw(32,32);
    assert.deepEqual(logs.map(r=>r.location),[at('@lib/ログ.lua',2),at('@main.lua',6),at('@main.lua',9)]);
    assert.deepEqual(logs.map(r=>r.source),['debug.log','print','debug.log']);
    vm.reset();assert.deepEqual(logs.at(-1).location,at('@main.lua',2));
  }finally{vm.dispose();}
});
test('logs keep emission lines across debugger suspension and subsequent runtime failure',()=>{
  const logs=[];const vm=engine.createVehicle({onLog:r=>logs.push(r)});
  try{
    vm.load('function onTick()\n debug.log("before")\n debug.log("after")\n missing()\nend','@stop.lua');
    vm.setBreakpoints([{source:'@stop.lua',line:3}]);
    assert.equal(vm.tick(),'suspended');assert.deepEqual(logs.map(r=>r.location),[at('@stop.lua',2)]);
    vm.setBreakpoints([]);assert.throws(()=>vm.resume());
    assert.deepEqual(logs.map(r=>r.location),[at('@stop.lua',2),at('@stop.lua',3)]);
    assert.equal(vm.drainLogRecords().length,0);
  }finally{vm.dispose();}
});
test('a builtin invoked directly by the host has no fabricated source location',()=>{
  const logs=[];const vm=engine.createVehicle({environment:'extended',onLog:r=>logs.push(r)});
  try{vm.load('onTick=print','@assigned.lua');vm.tick();assert.equal(logs.length,1);assert.equal(logs[0].location,undefined);}
  finally{vm.dispose();}
});
test('Addon game logs preserve raw bytes and leave Lua debug inspection unavailable',()=>{
  const logs=[];const vm=engine.createAddon({onLog:r=>logs.push(r)});
  try{
    vm.load('debug.log(type(print),type(debug.getinfo))\nfunction onTick(ticks)\n debug.log(string.char(255),ticks)\nend','@addon.lua');
    assert.equal(new TextDecoder().decode(logs[0].bytes),'nil\tnil');
    assert.deepEqual(logs[0].location,at('@addon.lua',1));
    vm.start();vm.tick(3);
    assert.deepEqual(logs[1].bytes,new Uint8Array([255,9,51]));
    assert.deepEqual(logs[1].location,at('@addon.lua',3));
  }finally{vm.dispose();}
});
test('non-minified maps translate recorded generated log positions without VM reentry',async()=>{
  const compiler=await loadCompiler({wasmBinary:await readFile(new URL('../../dist/compiler-wasm/compiler_bg.wasm',import.meta.url))});
  const project={entry:'main',modules:{main:'require("lib")\nfunction onTick() helper() end',lib:'function helper()\n debug.log("mapped")\nend'}};
  const built=compiler.build(project,{environment:'game',minify:false});
  assert.equal(built.ok,true);assert.ok(built.map);
  const logs=[];const vm=engine.createVehicle({onLog:r=>logs.push(r)});
  try{
    vm.load(built.code,'@linked.lua');vm.tick();
    assert.equal(logs[0].location.chunk,'@linked.lua');
    const origin=originalPositionFor(new TraceMap(built.map),{line:logs[0].location.line,column:0});
    assert.equal(origin.source,'lib.lua');assert.equal(origin.line,2);
  }finally{vm.dispose();}
});
