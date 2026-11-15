import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {loadRuntime,EngineError} from '../../dist/index.js';
const engine=await loadRuntime({wasmBinary:await readFile(new URL('../../dist/wasm/storm_lua_wasm.wasm',import.meta.url))});
const SCRIPT='count = 0\nfunction onTick()\n  count = count + 1\n  output.setNumber(1, count)\nend\nfunction onDraw()\n  screen.drawClear()\nend';
const LOOP='n = 0\nfunction onTick()\n  while true do\n    n = n + 1\n    m = n\n  end\nend';
const top=vm=>{const frame=vm.stack()[0];return [frame.source,frame.line];};

test('actual WASM: a pause requested while idle stops on the first line of the next entry',()=>{
  const vm=engine.createVehicle();
  try{
    vm.load(SCRIPT,'@main.lua');
    assert.equal(vm.tick(),'completed');
    vm.requestPause();
    assert.equal(vm.draw(32,32),'suspended');assert.deepEqual(top(vm),['@main.lua',7]);
    assert.equal(vm.resume(),'completed');
    assert.equal(vm.tick(),'completed');
    vm.requestPause();vm.cancelPause();
    assert.equal(vm.tick(),'completed');assert.equal(vm.io.outputNumbers[0],3);
  }finally{vm.dispose();}
});
test('actual WASM: a step past the end of an entry is carried only on request',()=>{
  for(const carry of [false,true]){
    const vm=engine.createVehicle();
    try{
      vm.load(SCRIPT,'@main.lua');
      vm.setBreakpoints([{source:'@main.lua',line:5}]);
      assert.equal(vm.tick(),'suspended');vm.setBreakpoints([]);
      assert.equal(vm.resume('over',{carryToNextEntry:carry}),'completed');
      assert.equal(vm.tick(),carry?'suspended':'completed');
      if(carry){assert.deepEqual(top(vm),['@main.lua',3]);assert.equal(vm.resume(),'completed');}
      assert.equal(vm.tick(),'completed');
    }finally{vm.dispose();}
  }
  const vm=engine.createVehicle();
  try{vm.load(SCRIPT,'@main.lua');vm.requestPause();vm.tick();assert.throws(()=>vm.resume('over',{carryToNextEntry:1}),TypeError);}
  finally{vm.dispose();}
});
test('actual WASM: pausePoll stops a running loop and leaves the VM inspectable',()=>{
  let pause=false,calls=0;
  const vm=engine.createVehicle({pausePoll:()=>{calls++;return pause;}});
  try{
    vm.load(LOOP,'@main.lua');
    pause=true;
    assert.equal(vm.tick(),'suspended');assert.equal(calls,1);
    const [source,line]=top(vm);
    assert.equal(source,'@main.lua');assert.ok(line>=3&&line<=6);
    const n=vm.evaluateWatch('n');assert.equal(n.kind,'integer');assert.ok(n.value>0n);
    pause=false;
    assert.equal(vm.resume('over'),'suspended');assert.notEqual(top(vm)[1],line);
    // resetをまたいで有効。
    vm.setBreakpoints([]);pause=true;vm.reset();
    assert.equal(vm.tick(),'suspended');
  }finally{vm.dispose();}
});
test('actual WASM: without a pause the loop still ends at the instruction limit, and a throwing poll is an error',()=>{
  let calls=0;
  const vm=engine.createVehicle({instructionBudget:4096*10+100,pausePoll:()=>{calls++;return false;}});
  try{
    vm.load(LOOP,'@main.lua');
    assert.throws(()=>vm.tick(),error=>error instanceof EngineError&&error.code===2);
    assert.equal(calls,10);
  }finally{vm.dispose();}
  const failing=engine.createVehicle({pausePoll:()=>{throw new Error('poll failed');}});
  try{
    failing.load(LOOP,'@main.lua');
    assert.throws(()=>failing.tick(),error=>error instanceof EngineError&&error.code===10&&/poll failed/.test(error.message));
  }finally{failing.dispose();}
  assert.throws(()=>engine.createVehicle({pausePoll:true}),TypeError);
});
test('actual WASM: breakable lines include nested functions and match real stops',()=>{
  const source='local total = 0\n\n-- comment only\nlocal function add(n)\n  total = total + n\n\n  return function()\n    return total\n  end\nend\nfunction onTick()\n  local read = add(2)\n  output.setNumber(1, read())\nend\n';
  const probe=engine.createVehicle();
  let reported;
  try{
    reported=probe.breakableLines(source,'@main.lua');
    assert.deepEqual(reported,[...reported].sort((a,b)=>a-b));
    for(const blank of [2,3,4,6])assert.ok(!reported.includes(blank));
    assert.ok(reported.includes(8));
    // 構文エラーは、省略されていないチャンク名と行を位置情報として持つ。
    const long='@a/path/that/goes/on/and/on/for/more/than/sixty/characters/bad.lua';
    for(const name of [long,'@bad.lua'])assert.throws(()=>probe.breakableLines('local a = 1\nlocal = 1',name),error=>{
      assert.ok(error instanceof EngineError);assert.equal(error.code,1);
      assert.deepEqual(error.detail,{stage:'syntax',location:{chunk:name,line:2},stack:[]});
      return true;
    });
    // 問い合わせたソースは実行も保持もされない。
    assert.equal(probe.tick(),'missing');
  }finally{probe.dispose();}
  const stopped=[];
  for(let line=1;line<=source.split('\n').length;line++){
    const vm=engine.createVehicle();
    try{
      vm.setBreakpoints([{source:'@main.lua',line}]);
      let hit=false,outcome=vm.load(source,'@main.lua');
      for(let round=0;round<2;round++){
        while(outcome==='suspended'){assert.equal(vm.stack()[0].line,line);hit=true;outcome=vm.resume();}
        outcome=vm.tick();
      }
      if(hit)stopped.push(line);
    }finally{vm.dispose();}
  }
  assert.deepEqual(stopped,reported);
});
test('actual WASM: Addon callbacks honour pause requests',()=>{
  const addon=engine.createAddon();
  try{
    addon.load('function onTick(n)\n  ticks = n\nend','@addon.lua');
    addon.start();addon.requestPause();
    assert.equal(addon.tick(1),'suspended');assert.equal(addon.stack()[0].line,2);
    assert.equal(addon.resume('out',{carryToNextEntry:true}),'completed');
    assert.equal(addon.tick(1),'suspended');
    assert.deepEqual(addon.breakableLines('x = 1\n\ny = 2'),[1,3]);
  }finally{addon.dispose();}
});
