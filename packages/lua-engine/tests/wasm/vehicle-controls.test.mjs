import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {loadRuntime,luaText} from '../../dist/index.js';
const engine=await loadRuntime({wasmBinary:await readFile(new URL('../../dist/wasm/storm_lua_wasm.wasm',import.meta.url))});
test('native development state and named callbacks cross WASM without host reentry',()=>{
  const logs=[];const vm=engine.createVehicle({environment:'extended',controlNamespace:'harness',onLog:r=>logs.push(r)});
  try{
    vm.load('harness.setProperty("gain",16777217)\nlocal n=property.getNumber("gain")\nfunction captured()output.setNumber(1,n-16777216)end','@setup.lua');
    vm.load('function custom(v) harness.setInputNumber(1,v);harness.setInputBool(1,true);output.setNumber(1,property.getNumber("gain")-16777216);output.setNumber(2,input.getNumber(1));output.setBool(1,input.getBool(1))end\nfunction preview() screen.setColor(255,0,0);screen.drawRectF(0,0,1,1);debug.log(screen.getWidth(),screen.getHeight())end\nfunction onTick() output.setNumber(3,7)end','@main.lua');
    for(let i=0;i<2;i++){
      assert.equal(vm.callTick('custom',[16777217]),'completed');
      assert.equal(vm.io.outputNumbers[0],1);assert.equal(vm.io.outputNumbers[1],16777216);
      assert.equal(vm.io.inputNumbers[0],16777216);assert.equal(vm.io.inputBooleans[0],1);
      assert.equal(vm.callDraw('preview',32,32),'completed');
      assert.deepEqual(Array.from(vm.frame().pixels.slice(0,4)),[255,0,0,255]);
      vm.tick();assert.equal(vm.io.outputNumbers[2],7);
      assert.equal(vm.callTick('absent'),'missing');
      const properties=vm.properties();assert.equal(luaText(properties[0].label),'gain');assert.equal(properties[0].value.value,16777217);
      vm.reset();
    }
    vm.load('harness.setProperty("gain",nil);harness.setProperty("raw",string.char(255))','@delete.lua');
    assert.deepEqual(vm.properties().map(p=>[luaText(p.label),Array.from(p.value.bytes)]),[['raw',[255]]]);
  }finally{vm.dispose();}
  assert.throws(()=>vm.callTick('custom'),/disposed/);
});
test('controls are opt-in, reject game/builtin collisions, and never enable protected calls in game',()=>{
  for(const options of [{controlNamespace:'harness'},{environment:'extended',controlNamespace:'screen'},
    {environment:'extended',controlNamespace:'_ENV'},{environment:'extended',controlNamespace:'bad.name'},
    {environment:'extended',controlNamespace:'harness',bindings:{values:{'harness.setProperty':null}}}]){
    assert.throws(()=>engine.createVehicle(options));
  }
  const vm=engine.createVehicle();
  try{vm.load('function check()output.setBool(1,pcall==nil and print==nil and harness==nil)end');vm.callTick('check');assert.equal(vm.io.outputBooleans[0],1);}
  finally{vm.dispose();}
});
test('named drawing suspends/resumes and missing or failing callbacks are not reported as successful frames',()=>{
  const vm=engine.createVehicle();
  try{
    vm.load('function preview()\n screen.setColor(255,0,0)\n screen.drawRectF(0,0,1,1)\n screen.drawRectF(2,0,1,1)\nend','@preview.lua');
    vm.setBreakpoints([{source:'@preview.lua',line:4}]);
    assert.equal(vm.callDraw('preview',32,32),'suspended');assert.equal(vm.stack()[0].line,4);
    assert.throws(()=>vm.callTick('anything'),error=>error.code===5);
    vm.setBreakpoints([]);assert.equal(vm.resume(),'completed');
    assert.deepEqual(Array.from(vm.frame().pixels.slice(8,12)),[255,0,0,255]);
    assert.equal(vm.callDraw('missing',32,32),'missing');
    assert.throws(()=>vm.callDraw('preview',0,32));
    assert.throws(()=>vm.callTick(''));assert.throws(()=>vm.callTick('bad\0name'));
    vm.load('function fail() debug.log("before failure");missing()end','@fail.lua');
    assert.throws(()=>vm.callDraw('fail',32,32),error=>error.code===1);
    assert.deepEqual(vm.drainLogRecords()[0].location,{chunk:'@fail.lua',line:1});
  }finally{vm.dispose();}
});
test('generic bindings still cannot reenter the active module',()=>{
  let vm;vm=engine.createVehicle({environment:'extended',controlNamespace:'harness',bindings:{functions:{reenter:()=>{vm.callTick('missing');return [];}}}});
  try{assert.throws(()=>vm.load('reenter()'),error=>error.code===5);}
  finally{vm.dispose();}
});

test('load and draw never overwrite host input that has not been consumed by a tick',()=>{
  for(const controlNamespace of [undefined,'harness']){
    const options=controlNamespace?{environment:'extended',controlNamespace}:{};
    const vm=engine.createVehicle(options);
    try{
      vm.io.inputNumbers[0]=3.5;
      vm.load('function onTick()output.setNumber(1,input.getNumber(1))end function onDraw()screen.drawClear()end');
      assert.equal(vm.io.inputNumbers[0],3.5);
      vm.draw(32,32);assert.equal(vm.io.inputNumbers[0],3.5);
      vm.tick();assert.equal(vm.io.outputNumbers[0],3.5);
      vm.io.inputNumbers[0]=7.5;vm.draw(32,32);vm.tick();assert.equal(vm.io.outputNumbers[0],7.5);
    }finally{vm.dispose();}
  }
});
