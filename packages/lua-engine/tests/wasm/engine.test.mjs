import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {loadRuntime,EngineError} from '../../dist/index.js';
import {loadRaster,encodeCommands} from '../../dist/raster.js';

const root=new URL('../../../../',import.meta.url);
const runtime=await loadRuntime({wasmBinary:new Uint8Array(await readFile(new URL('../../dist/wasm/storm_lua_wasm.wasm',import.meta.url)))});
const raster=await loadRaster({wasmBinary:new Uint8Array(await readFile(new URL('../../dist/wasm/screen.wasm',import.meta.url)))});
const decoder=new TextDecoder();

import {convert,luaSource} from '../../../../tools/fixture-commands.mjs';
const rectangle={kind:'rect',x:1,y:1,width:4,height:3,fill:true};

test('all 743 accepted RGBA cases survive binary JS/WASM roundtrip',async()=>{
  const fixture=JSON.parse(await readFile(new URL('fixtures/screen/cases-v1.json',root),'utf8'));
  assert.equal(fixture.contract,'storm-lua-screen-rgba-v1');
  assert.equal(fixture.cases.length,743);
  for(const c of fixture.cases){
    const instance=raster.createRaster(c.width,c.height);
    try {
      const frame=instance.render(c.ops.map(convert));const expected=new Uint8Array(c.width*c.height*4);let i=0;
      for(const [n,r,g,b,a] of c.expectedRgbaRle)for(let k=0;k<n;k++){expected.set([r,g,b,a],i);i+=4;}
      assert.equal(i,expected.length);assert.deepEqual(frame.pixels,expected,c.id);
    } finally {instance.dispose();}
  }
});
test('raster leases expire on replacement while owned snapshots remain valid',()=>{
  const r=raster.createRaster(8,8);
  try{
    const a=r.render([{kind:'color',rgba:[255,0,0,128]},rectangle]);const snapshot=a.copy();
    assert.deepEqual(Array.from(snapshot.subarray(36,40)),[128,0,0,64]);
    r.render([]);assert.throws(()=>a.pixels,/expired/);assert.equal(snapshot[36],128);
    const b=r.frame();r.dispose();assert.throws(()=>b.pixels,/disposed/);
  }finally{r.dispose();}
});
test('malformed binary input is rejected before replacing a valid frame',()=>{
  const r=raster.createRaster(8,8);
  try{
    const a=r.render([rectangle]);const snapshot=a.copy();
    assert.throws(()=>r.render(new Uint8Array([1,2,3])),EngineError);
    assert.deepEqual(a.pixels,snapshot);
    const invalid=encodeCommands([{kind:'text',x:0,y:0,text:new Uint8Array([255])}]);
    assert.throws(()=>r.render(invalid),/UTF-8/);
    assert.deepEqual(a.pixels,snapshot);
  }finally{r.dispose();}
});
test('raw foreign/stale handles, uploads and bounds are rejected',()=>{
  const b=raster.bridge;
  assert.throws(()=>b.call('dispose',0),e=>e.code===3);
  const r=raster.createRaster(4,4);const old=r.handle;r.dispose();
  const replacement=raster.createRaster(4,4);
  try{assert.notEqual(replacement.handle,old);assert.throws(()=>b.query('frame_ptr',old),e=>e.code===3);
    assert.throws(()=>b.call('render',replacement.handle,123,16),e=>e.code===4);
    b.upload(new Uint8Array([1]),(p,n)=>{assert.throws(()=>b.call('render',replacement.handle,p,n+1),e=>e.code===4);});
    assert.throws(()=>b.call('dealloc',123),e=>e.code===3);
  }finally{replacement.dispose();}
});
test('property initialization, f64 arithmetic and f32 I/O are separate',()=>{
  const vm=runtime.createVehicle({properties:{gain:16777217,raw:new Uint8Array([0,255])}});
  try{
    assert.equal(vm.load(`local gain=property.getNumber("gain")
function onTick() output.setNumber(1,input.getNumber(1)) output.setNumber(2,gain-16777216) output.setNumber(3,16777217.0) debug.log(property.getText("raw")) end`),'completed');
    vm.io.inputNumbers[0]=16777217;assert.equal(vm.tick(),'completed');
    assert.deepEqual(Array.from(vm.io.outputNumbers.subarray(0,3)),[16777216,1,16777216]);
    assert.deepEqual(vm.drainLogs(),[new Uint8Array([0,255])]);
    vm.setProperties({gain:3});vm.tick();assert.equal(vm.io.outputNumbers[1],1);
    vm.reset();vm.tick();assert.equal(vm.io.outputNumbers[1],3-16777216);
    assert.equal(vm.draw(8,8),'missing');
  }finally{vm.dispose();}
});
test('output persistence and multiple draw callbacks share Lua state',()=>{
  const vm=runtime.createVehicle();
  try{
    vm.load(`n=0 function onTick() if n==0 then output.setNumber(1,42) end end function onDraw() n=n+1 screen.setColor(n,0,0) screen.drawClear() output.setNumber(1,99) end`);
    vm.tick();vm.draw(4,4);assert.equal(vm.frame().pixels[0],1);vm.draw(8,8);assert.equal(vm.frame().pixels[0],2);
    vm.tick();assert.equal(vm.io.outputNumbers[0],42);
  }finally{vm.dispose();}
});
test('runtime failures preserve the actual diagnostic across upload cleanup',()=>{
  const vm=runtime.createVehicle({instructionBudget:5000});
  try{
    assert.throws(()=>vm.load(`while true do end`),e=>e.code===2 && /instruction/.test(e.message));
    assert.throws(()=>vm.tick(),e=>e.code===9);
  }finally{vm.dispose();}
  const syntax=runtime.createVehicle();try{assert.throws(()=>syntax.load('local = ?'),e=>e.code===1 && e.message.length>10);}finally{syntax.dispose();}
});
test('debugger pauses top-level, preserves i64/bytes, and invalidates stopped table handles',()=>{
  const vm=runtime.createVehicle();
  try{
    vm.setBreakpoints([{source:'=debug',line:5}]);
    assert.equal(vm.load(`local big=9223372036854775807
local raw=string.char(0,255)
local t={value=17}
local z=0
z=z+1
function onTick() output.setNumber(1,z) end`,'=debug'),'suspended');
    const locals=vm.locals();const byName=new Map(locals.map(v=>[decoder.decode(v.name),v.value]));
    assert.equal(byName.get('big').value,9223372036854775807n);
    assert.deepEqual(byName.get('raw').value,new Uint8Array([0,255]));
    const h=byName.get('t').handle;assert.equal(vm.expandTable(h)[0].value.value,17n);
    assert.throws(()=>vm.tick(),e=>e.code===5);
    assert.throws(()=>vm.setProperties({a:1}),e=>e.code===5);
    assert.equal(vm.stack()[0].line,5);
    assert.equal(vm.evaluateWatch('big').value,9223372036854775807n);
    assert.throws(()=>vm.expandTable(h),/stale/);
    vm.setBreakpoints([]);assert.equal(vm.resume(),'completed');vm.tick();assert.equal(vm.io.outputNumbers[0],1);
  }finally{vm.dispose();}
});
test('debug drawing resume appends only the new prefix',()=>{
  const vm=runtime.createVehicle();
  try{
    vm.load(`function onDraw()
 screen.setColor(255,0,0,128)
 screen.drawRectF(1,1,3,3)
 screen.drawRectF(1,1,3,3)
end`,'=draw');
    vm.setBreakpoints([{source:'=draw',line:4}]);assert.equal(vm.draw(8,8),'suspended');
    const partial=vm.frame();assert.deepEqual(Array.from(partial.pixels.subarray(36,40)),[128,0,0,64]);
    vm.setBreakpoints([]);assert.equal(vm.resume(),'completed');assert.throws(()=>partial.pixels,/expired/);
    assert.deepEqual(Array.from(vm.frame().pixels.subarray(36,40)),[192,0,0,96]);
  }finally{vm.dispose();}
});
test('memory growth refreshes input views and frame leases; disposal is checked',()=>{
  const vm=runtime.createVehicle();
  try{
    vm.load(`function onTick() output.setNumber(1,input.getNumber(1)) end function onDraw() screen.drawClear() end`);
    vm.io.inputNumbers[0]=3.5;const before=vm.io;vm.draw(32,32);const frame=vm.frame();
    // Emscripten の内部に立ち入ることなく、アロケータのメモリ拡張を強制します。
    runtime.bridge.upload(new Uint8Array(16*1024*1024),()=>{});
    const after=vm.io;assert.equal(after.inputNumbers[0],3.5);
    if(before.inputNumbers.buffer!==after.inputNumbers.buffer)assert.equal(before.inputNumbers.byteLength,0);
    assert.equal(frame.pixels.length,4096);vm.tick();assert.equal(vm.io.outputNumbers[0],3.5);
    vm.dispose();assert.throws(()=>vm.io,/disposed/);assert.throws(()=>frame.pixels,/disposed/);
  }finally{vm.dispose();}
});


test('all 743 accepted RGBA cases also pass through actual Lua WASM bindings',async()=>{
  const fixture=JSON.parse(await readFile(new URL('fixtures/screen/cases-v1.json',root),'utf8'));
  assert.equal(fixture.contract,'storm-lua-screen-rgba-v1');assert.equal(fixture.cases.length,743);
  for(const c of fixture.cases) {
    const vm=runtime.createVehicle();
    try {
      assert.equal(vm.load(luaSource(c.ops),'=screen-contract'),'completed');
      assert.equal(vm.draw(c.width,c.height),'completed');
      const actual=vm.frame().pixels;let offset=0;
      for(const [count,...rgba] of c.expectedRgbaRle)for(let i=0;i<count;i++)for(const component of rgba) {
        assert.equal(actual[offset++],component,`${c.id}: byte ${offset-1}`);
      }
      assert.equal(offset,actual.length,c.id);
    }finally{vm.dispose();}
  }
});
