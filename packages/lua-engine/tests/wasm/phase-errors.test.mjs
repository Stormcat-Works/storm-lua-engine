import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {loadRuntime,EngineError} from '../../dist/index.js';
const engine=await loadRuntime({wasmBinary:await readFile(new URL('../../dist/wasm/storm_lua_wasm.wasm',import.meta.url))});
// 文言の根拠は docs/verification/phase-errors-ingame-20261115.md。
const INPUT='attempting to use input function outside onTick',OUTPUT='attempting to use output function outside onTick',SCREEN='attempting to use screen function outside onDraw';
function failure(action){
  try{action();}catch(error){assert.ok(error instanceof EngineError);return error;}
  assert.fail('expected an EngineError');
}

test('actual WASM: input and output inside onDraw stop the script with the game wording',()=>{
  for(const environment of ['game','extended'])for(const [call,wording] of [['input.getNumber(1)',INPUT],['input.getBool(1)',INPUT],['output.setNumber(1, 123)',OUTPUT],['output.setBool(1, true)',OUTPUT]]){
    const vm=engine.createVehicle({environment});
    try{
      vm.load(`function onTick()\n  output.setNumber(2, 7)\nend\nfunction onDraw()\n  local x = 1\n  ${call}\nend`,'@main.lua');
      vm.tick();
      const error=failure(()=>vm.draw(32,32));
      assert.equal(error.code,1);assert.ok(error.message.includes(`main.lua:6: ${wording}`),error.message);
      assert.equal(vm.io.outputNumbers[0],0);assert.equal(vm.io.outputBooleans[0],0);assert.equal(vm.io.outputNumbers[1],7);
      // E9: 通常のLuaエラーではVMは失敗状態にならず、エラーが記録されている間はonTick/onDrawがスキップされる。
      assert.equal(vm.tick(),'missing');
      assert.equal(vm.draw(32,32),'missing');
      vm.reset();
      assert.equal(vm.tick(),'completed');
    }finally{vm.dispose();}
  }
});
test('actual WASM: screen inside onTick stops the script with the game wording',()=>{
  for(const call of ['screen.getWidth()','screen.setColor(255, 0, 0)','screen.drawRectF(40, 60, 20, 20)','screen.drawMap(0, 0, 1)','screen.setMapColorOcean(0, 0, 0)']){
    const vm=engine.createVehicle();
    try{
      vm.load(`local S = screen\nfunction onTick()\n  ${call.replace('screen.','S.')}\nend\nfunction onDraw()\n  S.drawClear()\nend`,'@main.lua');
      assert.equal(vm.draw(32,32),'completed');
      const error=failure(()=>vm.tick());
      assert.equal(error.code,1);assert.ok(error.message.includes(`main.lua:3: ${SCREEN}`),error.message);
    }finally{vm.dispose();}
  }
});
test('actual WASM: correct phases work and top-level calls use the same phase rules',()=>{
  const vm=engine.createVehicle();
  try{
    vm.load('function onTick()\n  output.setNumber(2, input.getNumber(1) * 2)\nend\nfunction onDraw()\n  screen.drawRectF(0, 0, screen.getWidth(), screen.getHeight())\nend','@main.lua');
    vm.io.inputNumbers[0]=5;
    assert.equal(vm.tick(),'completed');assert.equal(vm.io.outputNumbers[1],10);
    assert.equal(vm.draw(8,8),'completed');
  }finally{vm.dispose();}
  // E4: トップレベルでもI/O・描画フラグは0であり、arityが正しければoutsideエラーになる。
  for(const [source,wording] of [['output.setNumber(1, 9)',OUTPUT],['local x = input.getNumber(1)',INPUT],['screen.drawClear()',SCREEN],['local w = screen.getWidth()',SCREEN]]){
    const top=engine.createVehicle();
    try{
      const error=failure(()=>top.load(source,'@main.lua'));
      assert.equal(error.code,1);assert.ok(error.message.includes(`main.lua:1: ${wording}`),error.message);
    }finally{top.dispose();}
  }
});
