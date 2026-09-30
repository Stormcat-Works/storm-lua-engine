import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {MessageChannel} from 'node:worker_threads';
import {TraceMap,originalPositionFor,eachMapping} from '@jridgewell/trace-mapping';
import {loadCompiler} from '../../dist/compiler.js';
import {CompilerWorkerClient,serveCompiler} from '../../dist/compiler-worker.js';
const wasmBinary=await readFile(new URL('../../dist/compiler-wasm/compiler_bg.wasm',import.meta.url));
const compiler=await loadCompiler({wasmBinary});
const coordinator=await import('../../dist/compiler-wasm/compiler.js');
const worker=await import('../../dist/compiler-wasm/compiler.js?explanation-worker');
await worker.default({module_or_path:wasmBinary});
const hash=text=>createHash('sha256').update(text).digest('hex');
const bytes=text=>Buffer.byteLength(text);
function position(text,needle) {
 const at=text.indexOf(needle);assert.ok(at>=0,needle);const prefix=text.slice(0,at);
 return {line:prefix.split('\n').length,column:prefix.length-prefix.lastIndexOf('\n')-1};
}
function mapped(source,options={}) {
 const result=compiler.minify(source,{sourceMap:true,sourceName:'source.lua',...options});
 assert.equal(result.ok,true,JSON.stringify(result.diagnostics));assert.equal(typeof result.map,'string');
 const details=compiler.validateSourceMap(result.code,result.map);
 assert.equal(compiler.minify(source,{...options,sourceMap:false}).code,result.code);
 return {...result,details,json:JSON.parse(result.map)};
}
function selected(result,needle,skip=0) {
 let index=-1;for(let i=0;i<=skip;i++)index=result.code.indexOf(needle,index+1);assert.ok(index>=0,needle);
 const at=bytes(result.code.slice(0,index));const mapping=result.details.mappings.find(m=>m.start<=at&&at<m.end);assert.ok(mapping);
 return {mapping,origin:mapping.origin===null?null:result.details.origins[mapping.origin]};
}

test('optimized maps are opt-in, standard-decodable and independently fingerprinted',()=>{
 const source='function onTick()output.setNumber(1,2*3)end';
 const result=mapped(source,{zeroCostNewlines:false});
 assert.equal(compiler.minify(source).map,undefined);
 assert.equal(result.json.version,3);assert.equal(result.details.schemaVersion,1);
 assert.equal(result.details.producer.name,'storm-lua-engine');assert.equal(result.details.producer.version,'0.3.0');
 assert.equal(result.details.generated.sha256,hash(result.code));assert.equal(result.details.sources[0].sha256,hash(source));
 assert.equal(result.details.generated.bytes,bytes(result.code));
 assert.deepEqual(result.json.sourcesContent,[source]);
 const point=originalPositionFor(new TraceMap(result.map),position(result.code,'6'));
 assert.deepEqual({source:point.source,line:point.line,column:point.column},{source:'source.lua',...position(source,'2*3')});
 const reason=result.details.reasons.find(r=>r.basis==='literal-operands-evaluated-and-size-nonincreasing');assert.ok(reason);
 const facts=Object.fromEntries(reason.facts.map(f=>[f.key,f.value]));
 assert.equal(facts.left,'i64:2');assert.equal(facts.right,'i64:3');assert.equal(facts.evaluated,'i64:6');
 assert.ok(Number(facts.expressionSizeAfter)<=Number(facts.expressionSizeBefore));
});

test('operator and end locations are fine tokens while parent expression reasons survive',()=>{
 const source='function onTick()\n output.setNumber(1,input.getNumber(1) + 2)\nend';
 const result=mapped(source,{zeroCostNewlines:false,numericMode:'exact'});
 const {mapping,origin}=selected(result,'+');assert.equal(origin.precision,'token');assert.ok(mapping.copied);
 assert.equal(source.slice(origin.primary.start,origin.primary.end),'+');
 const trace=new TraceMap(result.map);assert.deepEqual(originalPositionFor(trace,position(result.code,'+')),
  {source:'source.lua',...position(source,'+'),name:null});
 assert.equal(originalPositionFor(trace,position(result.code,'end')).line,3);
 assert.ok(result.details.constructs.some(c=>c.origin!==null && result.details.origins[c.origin].precision==='expression'));
});

test('identity target keeps a precise copy contract after Unicode and CRLF',()=>{
 const source="-- 😀雪\r\nfunction onTick()\r\n local message='あ😀';output.setNumber(1,7)\r\nend\r\n";
 const result=mapped(source,{targetSize:8192});assert.equal(result.code,source);
 assert.equal(result.details.mappings.length,1);assert.deepEqual(result.details.mappings[0].copied,{source:0,start:0,end:bytes(source)});
 const point=originalPositionFor(new TraceMap(result.map),position(source,'output'));
 assert.deepEqual({line:point.line,column:point.column},position(source,'output'));
});

test('inlined literal carries caller context without replacing its definition range',()=>{
 const source='local function twice(value)\n return value*2\nend\nfunction onTick()output.setNumber(1,twice(input.getNumber(1)))end';
 const result=mapped(source,{numericMode:'exact',zeroCostNewlines:false});
 const offset=result.code.indexOf('*2')+1;assert.ok(offset>0);
 const at=bytes(result.code.slice(0,offset));const mapping=result.details.mappings.find(m=>m.start<=at&&at<m.end);
 const origin=result.details.origins[mapping.origin];assert.equal(source.slice(origin.primary.start,origin.primary.end),'2');
 assert.ok(mapping.inlineContexts.length>0);
 assert.ok(mapping.inlineContexts.some(id=>{
  const context=result.details.contexts[id];return source.slice(context.callSite.start,context.callSite.end)==='twice(input.getNumber(1))';
 }));
 assert.ok(result.details.relations.some(r=>r.role==='parameterUse'));
});

test('code, original snapshots, metadata and map-schema mismatches are rejected',()=>{
 const result=mapped('function onTick()output.setNumber(1,7)end');
 assert.throws(()=>compiler.validateSourceMap(result.code.replace('7','8'),result.map),/code|fingerprint|integrity/i);
 for(const mutate of [j=>j.sourcesContent[0]+=' ',j=>j.x_storm.schemaVersion=999,j=>j.x_storm.producer.version='0.0.0',j=>j.mappings='AAAA',j=>j.x_storm.relations.push({role:'contribution',span:{source:999,start:0,end:1}})]){
  const j=JSON.parse(result.map);mutate(j);assert.throws(()=>compiler.validateSourceMap(result.code,JSON.stringify(j)));
 }
});

test('original-file composition is preserved through optimized regular and LifeBoat builds',()=>{
 for(const lifeboat of [false,true])for(const minify of [false,true]){
  const modules=lifeboat?{main:"require('lib');function onTick()output.setNumber(1,twice(input.getNumber(1)))end",lib:'function twice(value)return value*2 end'}
   :{main:"local twice=require('lib');function onTick()output.setNumber(1,twice(input.getNumber(1)))end",lib:'return function(value)return value*2 end'};
  const result=(lifeboat?compiler.buildLifeboat:compiler.build)({entry:'main',modules},{sourceMap:true,minify,numericMode:'exact'});
  assert.equal(result.ok,true,JSON.stringify(result.diagnostics));const details=compiler.validateSourceMap(result.code,result.map);
  assert.equal(details.sources.length,2);
  let found=false;eachMapping(new TraceMap(result.map),m=>{if(m.source==='lib.lua'&&m.originalColumn===modules.lib.indexOf('2'))found=true;});
  assert.ok(found,`library literal missing ${lifeboat}/${minify}`);
 }
});

test('removal records distinguish discarded source syntax from runtime stop destinations',()=>{
 const source='function onTick()local unused=99 if false then output.setNumber(2,123)end output.setNumber(1,7)end';
 const result=mapped(source);
 assert.ok(result.details.dispositions.some(d=>source.slice(d.original.start,d.original.end)==='local unused=99'&&result.details.reasons[d.reason].operation==='remove'));
 assert.equal(result.code.includes('unused'),false);
 assert.equal(result.code.includes('123'),false);
 assert.ok(result.details.dispositions.every(d=>Array.isArray(d.replacementSources)));
});

test('normalization and numerical approximation carry facts rather than fake exact copies',()=>{
 const result=mapped('function onTick()output.setNumber(1,0x10)end',{numericMode:'exact',zeroCostNewlines:false});
 const {mapping}=selected(result,'16');assert.equal(mapping.copied,null);
 assert.ok(result.details.reasons.some(r=>r.before==='number:0x10'&&r.after==='number:16'));
 const approx=mapped('function onTick()output.setNumber(1,1.234567890123456)end',{numericMode:'tolerant'});
 const reason=approx.details.reasons.find(r=>r.basis==='finite-nonintegral-literal-within-budget-and-shorter');
 assert.ok(reason);
 const facts=Object.fromEntries(reason.facts.map(f=>[f.key,f.value]));
 for(const key of ['originalBits','replacementBits','absoluteErrorBits','absoluteBudgetBits','relativeBudgetBits'])assert.match(facts[key],/^[0-9a-f]{16}$/);
});

test('binary candidate continuation returns the same selected map as the direct compiler',()=>{
 const source='local total=0 function onTick()local x=input.getNumber(1)local y=input.getNumber(2)total=total+x+y output.setNumber(1,total+x+x+y)end function onDraw()screen.drawText(1,1,total)end';
 const options={sourceMap:true,sourceName:'resume.lua',targetSize:0,searchMode:'fast',searchBeamWidth:1};
 const expected=compiler.minify(source,options);assert.equal(expected.ok,true);
 const attempt=coordinator.trySatisficing(source,options);
 const result=attempt.done?attempt.result:coordinator.finishTargetSearch(source,options,attempt.prepared.context,
  attempt.prepared.jobs.map(job=>worker.evaluateJob(job)).reverse(),attempt.checkpoints);
 assert.equal(result.ok,true);assert.equal(result.code,expected.code);assert.equal(result.map,expected.map);
 assert.deepEqual(compiler.validateSourceMap(result.code,result.map),compiler.validateSourceMap(expected.code,expected.map));
});

test('host-owned compiler RPC also supports validated optimization maps',async()=>{
 const {port1,port2}=new MessageChannel();port1.start();port2.start();
 const stop=serveCompiler(port2,{wasmBinary});const client=new CompilerWorkerClient(port1);
 try{
  const result=await client.minify('function onTick()output.setNumber(1,2*3)end',{sourceMap:true});
  const details=await client.validateSourceMap(result.code,result.map);
  assert.equal(details.schemaVersion,1);assert.ok(details.reasons.length>0);
  await assert.rejects(client.validateSourceMap(result.code+' ',result.map),/code|fingerprint/i);
 }finally{client.dispose();stop();port1.close();port2.close();}
});


test('a fully removed source keeps its concrete reasons without false executable mappings',()=>{
 const source='local unused=99 if not true then output.setNumber(1,7)end';
 const result=mapped(source);assert.equal(result.code,'');assert.deepEqual(result.details.mappings,[]);
 const reasons=result.details.dispositions.map(d=>result.details.reasons[d.reason]);
 assert.ok(reasons.some(r=>r.basis==='all-declared-bindings-unread-unwritten-and-initializers-movable'));
 assert.ok(reasons.some(r=>r.basis==='lua-truthiness-of-known-conditions'&&r.facts.some(f=>f.key==='armTruthiness'&&f.value==='false')));
});

test('loop body keywords map to do rather than the loop starting keyword',()=>{
 for(const source of ['while input.getBool(1)do output.setNumber(1,7)end','for i=1,8 do output.setNumber(i,i)end']){
  const passToggles=Object.fromEntries(compiler.passIds().map(id=>[id,false]));
  const result=mapped(source,{passToggles,zeroCostNewlines:false});
  const {origin,mapping}=selected(result,'do');assert.equal(origin.precision,'token');assert.ok(mapping.copied);
  assert.equal(source.slice(origin.primary.start,origin.primary.end),'do');
 }
});
