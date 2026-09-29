import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import * as api from '../../dist/index.js';
import {hostSmoke} from '../../../../tools/host-smoke.mjs';
const engine=await api.loadRuntime({wasmBinary:await readFile(new URL('../../dist/wasm/storm_lua_wasm.wasm',import.meta.url))});

test('addon server calls, snapshots, maps, HTTP and structured logs cross real WASM',()=>{
  assert.deepEqual(hostSmoke(engine,api),{addon:true,hostServer:true,mapProvider:true,http:true,structuredLogs:true,logLocations:true});
});
test('host exceptions and Promise returns become explicit failures, not success stubs',()=>{
  for(const server of [{fail:()=>{throw new Error('service unavailable');}},{fail:async()=>[1n]},{fail:async()=>{throw new Error("async failure");}}]){
    const vm=engine.createAddon({server});
    try {
      vm.load('function onCreate() server.fail() end');
      assert.throws(()=>vm.start(),error=>error instanceof api.EngineError&&error.code===10);
      assert.throws(()=>vm.tick(),error=>error.code===9);
    } finally {vm.dispose();}
  }
  const missing=engine.createAddon();
  try {missing.load('function onCreate() server.getPlayers() end');assert.throws(()=>missing.start(),error=>error.code===1);}finally{missing.dispose();}
});
test('synchronous callbacks cannot reenter their own VM or another VM in the same module',()=>{
  const other=engine.createVehicle();other.load('function onTick() output.setNumber(1,99) end');
  let addon;
  addon=engine.createAddon({server:{reenter:()=>{other.tick();return [];}}});
  try {
    addon.load('function onCreate() server.reenter() end');
    assert.throws(()=>addon.start(),error=>error.code===5);
    assert.equal(other.io.outputNumbers[0],0);
    other.tick();assert.equal(other.io.outputNumbers[0],99);
  } finally {addon.dispose();other.dispose();}
});
test('automatic logs preserve bytes on failure and surface sink errors separately',()=>{
  const records=[];
  const vm=engine.createVehicle({environment:'extended',onLog:record=>records.push(record)});
  try {
    assert.throws(()=>vm.load('print(string.char(0,255));debug.log("before error");error("script failed")'),error=>error.code===1&&error.message.includes('script failed'));
    assert.deepEqual(records.map(r=>r.source),['print','debug.log']);assert.deepEqual(records[0].bytes,new Uint8Array([0,255]));
    assert.deepEqual(vm.drainLogRecords(),[]);
  }finally{vm.dispose();}
  const sinkFailure=new Error('sink failed'),attempted=[];
  const broken=engine.createAddon({environment:'extended',onLog:record=>{attempted.push(record);throw sinkFailure;}});
  try {
    assert.throws(()=>broken.load('print("one");debug.log("two");error("lua failed")'),error=>error instanceof AggregateError&&error.errors[0].code===1&&error.errors[1] instanceof AggregateError);
    assert.equal(attempted.length,2);
  }finally{broken.dispose();}
});
test('map absence, malformed maps and provider exceptions are visible errors',()=>{
  for(const options of [{},{mapProvider:()=>new Uint8Array(3)},{mapProvider:()=>{throw new Error('terrain failed');}},{mapProvider:async()=>new Uint8Array(16)}]){
    const vm=engine.createVehicle(options);
    try {vm.load('function onDraw() screen.drawMap(0,0,1) end');assert.throws(()=>vm.draw(2,2),error=>[6,10].includes(error.code));}finally{vm.dispose();}
  }
});
test('addon suspension preserves lifecycle and HTTP tokens; reload rejects old debug objects',()=>{
  const vm=engine.createAddon({newWorld:false,savedata:api.luaTable({count:41n})});
  try {
    vm.setBreakpoints([{source:'=paused-addon',line:2}]);
    assert.equal(vm.load('local function assert(v)if not v then local fail=nil;fail()end end;g_savedata={count=999}\ng_savedata.count=1000\nfunction onCreate(new) assert(not new and g_savedata.count==41);server.httpGet(8080,"/wait") end\nfunction onTick()\n local n=1\n n=n+1\nend\nfunction httpReply(p,q,r) g_savedata.reply=r end','=paused-addon'),'suspended');
    assert.throws(()=>vm.start(),error=>error.code===5);assert.throws(()=>vm.savedata(),error=>[4,5].includes(error.code));
    const table=vm.evaluateWatch('g_savedata');assert.equal(table.kind,'table');
    vm.setBreakpoints([]);vm.resume();vm.start();
    const request=vm.drainHttpRequests()[0];
    vm.setBreakpoints([{source:'=paused-addon',line:6}]);assert.equal(vm.tick(),'suspended');
    assert.throws(()=>vm.httpReply(request.token,'early'),error=>error.code===5);
    vm.setBreakpoints([]);vm.resume();vm.httpReply(request.token,'delivered');
    const checkpoint=vm.savedata();vm.reload(checkpoint);vm.start();
    assert.equal(api.luaText(api.luaField(vm.savedata(),'reply')),'delivered');
    assert.throws(()=>vm.httpReply(request.token,'stale'),error=>error.code===4);
    assert.throws(()=>vm.expandTable(table.handle),error=>error.code===4);
  }finally{vm.dispose();}
});
test('addon limits and zero-length source reload work without a vehicle I/O block',()=>{
  const empty=engine.createAddon();
  try {empty.load('');assert.equal(empty.start(),'missing');const saved=empty.savedata();empty.reload(saved);assert.equal(empty.start(),'missing');assert.throws(()=>engine.bridge.query('io_ptr',empty.handle),error=>error.code===4);}finally{empty.dispose();}
  const runaway=engine.createAddon({instructionBudget:1000});
  try {runaway.load('function onCreate() while true do end end');assert.throws(()=>runaway.start(),error=>error.code===2);}finally{runaway.dispose();}
});

test('Promise-returning log sinks fail synchronously without unhandled rejection',()=>{
  for(const onLog of [async()=>{},async()=>{throw new Error('async sink failed');}]) {
    const vm=engine.createVehicle({onLog});
    try {assert.throws(()=>vm.load('debug.log("entry")'),error=>error instanceof AggregateError && error.errors[0] instanceof TypeError);}
    finally {vm.dispose();}
  }
});
