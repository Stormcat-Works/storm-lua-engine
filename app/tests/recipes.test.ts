import test from 'node:test';
import assert from 'node:assert/strict';
import {PlaygroundSession} from '../shared/session.js';
import {nodeInit} from '../cli/init.js';
import {RECIPES,recipe} from '../shared/recipes.js';
import {parseProject,StepRunner} from '../shared/project.js';
import {stringify,decodeWire} from '../shared/wire.js';
import {luaField} from '@stormcat-works/storm-lua-engine';
const init=await nodeInit();
for(const item of RECIPES)test(`実SDK確認例: ${item.id}`,async()=>{
 const session=new PlaygroundSession(init);
 try{
  const project=parseProject(item), runner=new StepRunner(project,c=>session.execute(c));
  const results=await runner.all();
  assert.equal(results.length,project.steps.length, stringify(results.filter(r=>!r.ok)));
  assert.equal(results.every(r=>r.ok),true,stringify(results.filter(r=>!r.ok)));
  const named=(name:string):Record<string,unknown>=>{const value=results.find(r=>project.steps[r.index]?.['as']===name)?.value;assert.ok(value&&typeof value==='object');return value as Record<string,unknown>;};
  const number=(name:string,index=0)=>{const io=named(name)['io'] as {outputNumbers:number[]};return io.outputNumbers[index];};
  switch(item.id){
   case 'source-map':assert.equal(named('mapped')['ok'],true);assert.ok(named('mapped')['map']);break;
   case 'source-map-modules':assert.equal(named('lifeboat')['ok'],true);assert.ok(named('mapped')['map']);break;
   case 'source-map-debug':assert.equal(named('paused')['outcome'],'suspended');assert.equal(named('failed')['outcome'],'error');assert.ok((named('failed')['locations'] as unknown[]).length>0);break;
   case 'source-loading':assert.equal(number('result'),27);assert.equal(number('afterReset'),27);break;
   case 'vehicle':assert.equal(number('initial'),6);assert.equal(number('cached'),6);assert.equal(number('resetResult'),12);break;
   case 'compiler':assert.equal(number('result'),8);assert.ok(named('linked').map);assert.equal(named('ambient')['ok'],true);assert.deepEqual((named('ambient')['injectedAmbient'] as Record<string,unknown>)['sim'],['constant']);assert.ok(Number(named('compiled')['size'])>0);break;
   case 'reflection':assert.equal((named('compiled')['search'] as {mode:string}).mode,'lexical');assert.match(String(named('compiled')['code']),/originalFunction/);assert.equal(number('result'),9);break;
   case 'environment':assert.equal(number('result'),7);break;
   case 'bindings':assert.equal(number('result'),110);assert.equal((named('result')['io'] as {outputBooleans:boolean[]}).outputBooleans[0],true);break;
   case 'addon':assert.equal(luaField(named('afterReload') as unknown as Parameters<typeof luaField>[0],'count'),3n);assert.equal(luaField(named('afterReload') as unknown as Parameters<typeof luaField>[0],'created'),false);break;
   case 'debugger':assert.equal((named('result')['outputNumbers'] as number[])[0],1);break;
   case 'raster':{const frames=results.filter(r=>r.op==='render').map(r=>r.value as {pixels:Uint8Array});assert.equal(frames[0]?.pixels.length,64*32*4);assert.deepEqual(frames[0]?.pixels,frames[1]?.pixels);break;}
   case 'services':assert.equal(number('result'),7);break;
   case 'values':assert.equal(luaField(named('decoded') as unknown as Parameters<typeof luaField>[0],'large'),9223372036854775807n);assert.deepEqual(luaField(named('decoded') as unknown as Parameters<typeof luaField>[0],'raw'),new Uint8Array([0,255]));break;
   case 'errors':assert.equal((results[1]?.value as {ok:boolean}).ok,false);assert.equal(results.filter(r=>r.expectedError).length,3);break;
   case 'catalog':assert.ok((results[0]?.value as {vehicle:unknown[]}).vehicle.length>20);break;
  }
 }finally{session.dispose();}
});
test('プロジェクト往復・未対応形式・不正な参照',async()=>{
 const original=parseProject(recipe('vehicle'));assert.deepEqual(parseProject(JSON.parse(stringify(original))),original);
 assert.throws(()=>parseProject({...original,version:2}),/version/);assert.throws(()=>parseProject({...original,kind:'addon-lab'}),/形式/);
 assert.throws(()=>parseProject({...original,environment:'oops'}),/environment/);
 const runner=new StepRunner({...original,steps:[{op:'load',source:{$ref:'missing.code'}}]},async()=>{throw new Error('実行されてはいけません');});
 const result=await runner.next();assert.equal(result.ok,false);assert.match(result.error??'',/参照/);
 const input={v:{$i64:'9223372036854775807'},raw:{$bytes:[0,255]},n:{$number:'-0'}};
 assert.deepEqual(JSON.parse(stringify(decodeWire(input))),input);
});
