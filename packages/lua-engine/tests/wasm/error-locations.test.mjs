import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {loadRuntime,EngineError,STATUS} from '../../dist/index.js';
const engine=await loadRuntime({wasmBinary:await readFile(new URL('../../dist/wasm/storm_lua_wasm.wasm',import.meta.url))});
// Luaがメッセージの中で先頭を省略する長さ（60バイト超）のチャンク名。
const LONG='@a/path/that/goes/on/and/on/for/more/than/sixty/characters/main.lua';
const at=(chunk,line)=>({chunk,line});
function failure(action){
  try{action();}catch(error){assert.ok(error instanceof EngineError);return error;}
  assert.fail('expected an EngineError');
}

test('actual WASM runtime errors carry the full chunk name, line and call stack',()=>{
  const vm=engine.createVehicle();
  try{
    vm.load('local function inner()\n  local t = nil\n  return t.x\nend\nfunction outer()\n  return inner() + 1\nend\nfunction onTick()\n  outer()\nend',LONG);
    const error=failure(()=>vm.tick());
    assert.equal(error.code,STATUS.lua);
    assert.ok(error.message.includes('...')&&!error.message.includes(LONG.slice(1)));
    assert.deepEqual(error.detail,{stage:'runtime',location:at(LONG,3),stack:[
      {level:0,source:LONG,line:3,functionName:'inner'},
      {level:1,source:LONG,line:6,functionName:'outer'},
      {level:2,source:LONG,line:9,functionName:null}]});
  }finally{vm.dispose();}
});
test('syntax errors are distinguished from runtime errors and have no stack',()=>{
  const vm=engine.createVehicle();
  try{
    const syntax=failure(()=>vm.load('local a = 1\nlocal b = = 2\n',LONG));
    assert.equal(syntax.code,STATUS.lua);
    assert.deepEqual(syntax.detail,{stage:'syntax',location:at(LONG,2),stack:[]});
  }finally{vm.dispose();}
  const top=engine.createVehicle();
  try{
    const runtime=failure(()=>top.load('local t\nlocal x = t.field','@main.lua'));
    assert.equal(runtime.detail.stage,'runtime');assert.deepEqual(runtime.detail.location,at('@main.lua',2));
  }finally{top.dispose();}
});
test('a syntax error in a required chunk names that chunk, not the caller',()=>{
  const vm=engine.createVehicle({environment:'extended',requireLoader:()=>({name:'@lib/壊れた.lua',source:'local ok = 1\n\nlocal bad = = 2'})});
  try{
    const error=failure(()=>vm.load('local x = 1\nrequire("broken")','@main.lua'));
    assert.equal(error.detail.stage,'syntax');assert.deepEqual(error.detail.location,at('@lib/壊れた.lua',3));
    assert.ok(error.detail.stack.some(frame=>frame.source==='@main.lua'&&frame.line===2));
  }finally{vm.dispose();}
});
test('the instruction limit reports where the script was running',()=>{
  for(const environment of ['game','extended']){
    const vm=engine.createVehicle({environment,instructionBudget:10000});
    try{
      vm.load('local function spin()\n  while true do\n  end\nend\nfunction onTick()\n  spin()\nend','@loop.lua');
      const error=failure(()=>vm.tick());
      assert.equal(error.code,STATUS.limit);assert.equal(error.detail.stage,null);
      assert.equal(error.detail.location.chunk,'@loop.lua');
      assert.ok([2,3].includes(error.detail.location.line));
      assert.deepEqual(error.detail.stack.map(frame=>[frame.source,frame.functionName]),[['@loop.lua','spin'],['@loop.lua',null]]);
      assert.equal(error.detail.stack[1].line,6);
    }finally{vm.dispose();}
  }
});
test('errors raised by a host function point at the Lua caller',()=>{
  const vm=engine.createVehicle();
  try{
    vm.load('function onDraw()\n  local x = 1\n  screen.drawText(0, 0)\nend','@main.lua');
    const error=failure(()=>vm.draw(32,32));
    assert.deepEqual(error.detail.location,at('@main.lua',3));
    assert.deepEqual(error.detail.stack.map(frame=>frame.line),[-1,3]);
  }finally{vm.dispose();}
});
test('host argument errors have no detail, and a later error does not inherit an older one',()=>{
  const vm=engine.createVehicle();
  try{
    vm.load('function onDraw() missing() end','@main.lua');
    assert.equal(failure(()=>vm.draw(0,0)).detail,undefined);
    assert.equal(failure(()=>vm.draw(32,32)).detail.stage,'runtime');
    const failed=failure(()=>vm.draw(32,32));
    assert.equal(failed.code,STATUS.failed);assert.equal(failed.detail,undefined);
  }finally{vm.dispose();}
});
