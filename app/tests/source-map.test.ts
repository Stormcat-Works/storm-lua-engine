import {moveMapSelection,type TextSelection} from '../web/map-selection.js';
import test from 'node:test';
import assert from 'node:assert/strict';
import {loadCompiler} from '@stormcat-works/storm-lua-engine/compiler';
import {nodeInit} from '../cli/init.js';
import {MapDocument,DisplayText,type Artifact} from '../shared/source-map.js';
import {PlaygroundSession} from '../shared/session.js';
const init=await nodeInit(),compiler=await loadCompiler(init.compiler);
async function compiled(source:string,options:Record<string,unknown>={}):Promise<MapDocument>{
 const result=compiler.minify(source,{sourceMap:true,sourceName:'controller.lua',...options});
 assert.equal(result.ok,true,JSON.stringify(result.diagnostics));assert.equal(typeof result.code,'string');assert.equal(typeof result.map,'string');
 return MapDocument.open({kind:'storm-lua-artifact',version:1,code:result.code!,map:result.map!},(code,map)=>compiler.validateSourceMap(code,map));
}
test('textarea UTF-16 coordinates preserve original UTF-8 and CRLF boundaries',()=>{
 const c=new DisplayText('a😀雪\r\nb');assert.equal(c.value,'a😀雪\nb');
 assert.equal(c.byteAt(3),5);assert.equal(c.byteAt(5),10);assert.equal(c.uiAt(10),5);assert.equal(c.uiAt(9),4);assert.equal(c.uiAt(9,true),5);
 assert.deepEqual(c.position(10),{line:2,column:1});assert.throws(()=>c.byteAt(2),/境界/);assert.throws(()=>c.uiAt(2),/UTF-8/);
 assert.deepEqual(c.line(2),{start:10,end:11});assert.equal(c.line(3),null);
});
test('generated token selections and exact-copy reverse selections do not mix occurrences',async()=>{
 const source='function onTick()\n local throttle=input.getNumber(1)\n output.setNumber(1,throttle)\n output.setNumber(2,throttle*2)\nend';
 const doc=await compiled(source,{zeroCostNewlines:false,numericMode:'exact'});
 const index=doc.artifact.code.indexOf('*'),hit=doc.generatedHits(index,index+1)[0]!;
 assert.equal(doc.data.origins[hit.origin!]!.precision,'token');assert.equal(doc.sources[0]!.coordinates.slice(hit.original!.start,hit.original!.end),'*');
 const originals=[...source.matchAll(/throttle/g)].map(m=>m.index!);
 for(const start of originals){const matches=doc.originalHits(0,start,start+8).filter(h=>h.via==='primary'||h.via==='copy');assert.ok(matches.length>0);assert.ok(matches.every(h=>h.original!.start===start));}
 assert.equal(doc.generatedHits(doc.generated.bytes).length,0);assert.equal(doc.originalHits(0,new TextEncoder().encode(source).length).length,0);
});
test('inline contexts, typed related arguments and reasons are available without coarse fallback',async()=>{
 const doc=await compiled('local function twice(v)return v*2 end function onTick()output.setNumber(1,twice(input.getNumber(1)))end',{numericMode:'exact',zeroCostNewlines:false});
 const at=doc.artifact.code.indexOf('*2')+1,m=doc.data.mappings.find(m=>m.start<=at&&at<m.end)!;
 assert.ok(m.inlineContexts.length>0);assert.ok(doc.data.contexts.some(c=>doc.sources[0]!.coordinates.slice(c.callSite.start,c.callSite.end).startsWith('twice(')));
 const fold=await compiled('function onTick()output.setNumber(1,2*3)end');
 assert.ok(fold.data.reasons.some(r=>r.basis==='literal-operands-evaluated-and-size-nonincreasing'));
});
test('removed declarations are not reverse-mapped through their containing function',async()=>{
 const source='function onTick()\n local unused=99\n output.setNumber(1,7)\nend';const doc=await compiled(source);const start=source.indexOf('unused');
 assert.equal(doc.originalHits(0,start,start+6).length,0);assert.ok(doc.dispositions(0,start,start+6).length>0);assert.deepEqual(doc.breakpointLines(0,start,start+6),[]);
});
test('identity accepts caret and Unicode while unmapped generated pieces have no fabricated source',async()=>{
 const source='-- 😀\r\nfunction onTick()\r\n output.setNumber(1,7)\r\nend';const doc=await compiled(source,{targetSize:8192});
 const start=new TextEncoder().encode(source.slice(0,source.indexOf('output'))).length;
 assert.deepEqual(doc.breakpointLines(0,start),[3]);assert.equal(doc.originalHits(0,start)[0]!.start,start);
 const shortened=await compiled('function onTick()local throttle=input.getNumber(1)output.setNumber(1,throttle)output.setNumber(2,throttle)end');
 const syn=shortened.data.mappings.find(m=>m.origin!==null&&shortened.data.origins[m.origin]!.kind==='synthetic');assert.ok(syn);assert.equal(shortened.generatedHits(syn.start)[0]!.original,null);
});
test('line-only runtime resolution exposes ambiguity instead of taking column zero',async()=>{
 const source='function onTick()\n output.setNumber(1,input.getNumber(1))\n output.setNumber(2,input.getNumber(2))\nend';const doc=await compiled(source,{zeroCostNewlines:false});
 const location=doc.runtimeLine(1)!;assert.equal(location.precision,'line-only');assert.equal(location.ambiguous,true);assert.ok(location.hits.length>1);assert.equal(doc.runtimeLine(999),null);
});
test('stale code, invalid maps and foreign schema are rejected by SDK before inspection',async()=>{
 const doc=await compiled('function onTick()output.setNumber(1,7)end');
 await assert.rejects(MapDocument.open({...doc.artifact,code:doc.artifact.code.replace('7','8')},compiler.validateSourceMap));
 const changed=JSON.parse(doc.artifact.map);changed.x_storm.schemaVersion=999;
 await assert.rejects(MapDocument.open({...doc.artifact,map:JSON.stringify(changed)},compiler.validateSourceMap));
});
test('mapped host pauses, steps and returns real error/log positions bound to its artifact',async()=>{
 const source='function onTick()\n local values={}\n output.setNumber(1,input.getNumber(1))\n debug.log("before error")\n output.setNumber(2,values.missing+1)\nend';
 const result=compiler.build({entry:'main',modules:{main:source}},{minify:false,sourceMap:true});assert.equal(result.ok,true);
 const a:Artifact={kind:'storm-lua-artifact',version:1,code:result.code!,map:result.map!};const session=new PlaygroundSession(init);
 try{
  const loaded=await session.execute({op:'mappedLoad',artifact:a}) as {artifact:string;outcome:string;chunk:string};assert.equal(loaded.outcome,'completed');
  const send=(action:string,options:Record<string,unknown>={})=>session.execute({op:'mappedAction',identity:loaded.artifact,action,options}) as Promise<Record<string,unknown>>;
  await send('breakpoints',{source:0,start:source.indexOf('output.setNumber'),end:source.indexOf('output.setNumber')+16});
  const paused=await send('tick');assert.equal(paused['outcome'],'suspended');assert.ok((paused['stack'] as unknown[]).length>0);
  await send('clearBreakpoints');const stepped=await send('over');assert.equal(stepped['outcome'],'suspended');
  const failed=await send('continue');assert.equal(failed['outcome'],'error');
  const locations=failed['locations'] as {kind:string;chunk:string;association:{precision:string}|null}[];
  assert.ok(locations.some(l=>l.kind==='error'&&l.chunk===loaded.chunk&&l.association?.precision==='line-only'));assert.ok(locations.some(l=>l.kind==='log'));
  await assert.rejects(session.execute({op:'mappedAction',identity:'wrong-artifact',action:'tick'}),/一致/);
 }finally{session.dispose();}
});


test('generated identity range and caret use the validated exact-copy offset, not the whole source',async()=>{
 const source='-- 😀雪\r\nfunction onTick()\r\n output.setNumber(1,7)\r\n output.setNumber(2,8)\r\nend\r\n';
 const doc=await compiled(source,{targetSize:8192});assert.equal(doc.artifact.code,source);
 const start=new TextEncoder().encode(source.slice(0,source.indexOf('output'))).length;
 const end=start+6;
 assert.deepEqual(doc.generatedHits(start,end).map(h=>({start:h.start,end:h.end,via:h.via,original:h.original})),[{start,end,via:'copy',original:{source:0,start,end}}]);
 const caret=doc.generatedHits(start+2)[0]!;
 assert.deepEqual([caret.start,caret.end],[start+2,start+2]);
 assert.deepEqual(caret.original,{source:0,start:start+2,end:start+2});
 assert.deepEqual(doc.originalHits(0,start,end).map(h=>[h.start,h.end]),[[start,end]]);
 const emoji=source.indexOf('😀'),emojiStart=new TextEncoder().encode(source.slice(0,emoji)).length;
 assert.deepEqual(doc.generatedHits(emojiStart,emojiStart+4)[0]!.original,{source:0,start:emojiStart,end:emojiStart+4});
 assert.throws(()=>doc.generatedHits(emojiStart+1),/UTF-8/);
});

test('identity runtime reports stay line-only while restricting candidates to that actual line',async()=>{
 const source='-- 😀\r\nfunction onTick()\r\n output.setNumber(1,7)\r\n output.setNumber(2,8)\r\nend\r\n';
 const doc=await compiled(source,{targetSize:8192});
 for(const line of [3,4]){
  const range=doc.generated.line(line)!;
  const location=doc.runtimeLine(line)!;
  assert.equal(location.precision,'line-only');assert.equal(location.ambiguous,false);
  assert.equal(location.hits.length,1);
  const hit=location.hits[0]!;assert.deepEqual([hit.start,hit.end],[range.start,range.end]);
  assert.deepEqual(hit.original,{source:0,...range});
  assert.equal(doc.sources[0]!.coordinates.position(hit.original!.start).line,line);
 }
 assert.deepEqual(doc.generatedHits(doc.generated.bytes),[]);
});

test('copy clipping also respects nonzero source offsets in multi-module non-minified builds',async()=>{
 const lib='-- 😀\r\nlocal offset=7\r\nreturn offset';
 const main='local offset=require("lib")\nfunction onTick() output.setNumber(1,offset) end';
 const result=compiler.build({entry:'main',modules:{main,lib}},{sourceMap:true,minify:false});
 assert.equal(result.ok,true,JSON.stringify(result.diagnostics));
 const doc=await MapDocument.open({kind:'storm-lua-artifact',version:1,code:result.code!,map:result.map!},compiler.validateSourceMap);
 const source=doc.sources.findIndex(s=>s.name==='lib.lua');assert.ok(source>=0);
 const copy=doc.data.mappings.find(m=>m.copied?.source===source&&m.copied.start>0&&m.end-m.start>=4);assert.ok(copy);
 const start=copy.start+1,end=copy.start+3;
 // Use an ASCII part of a copied token; imported Unicode must not be split.
 const hit=doc.generatedHits(start,end)[0]!;
 assert.deepEqual(hit.original,{source,start:copy.copied!.start+1,end:copy.copied!.start+3});
});

test('rewritten expressions do not interpolate source positions just because a small range was selected',async()=>{
 const source='function onTick()output.setNumber(1,2*3)end';const doc=await compiled(source,{zeroCostNewlines:false});
 const at=doc.artifact.code.indexOf('6');const hit=doc.generatedHits(at,at+1)[0]!;
 assert.equal(hit.via,'primary');
 assert.equal(doc.sources[hit.original!.source]!.coordinates.slice(hit.original!.start,hit.original!.end),'2*3');
});


test('readonly map arrows move across whole Unicode scalars without changing source text',()=>{
 const value='a😀雪b';let caret:TextSelection={start:1,end:1,direction:'none'};
 const next=moveMapSelection(value,caret,'ArrowRight',false)!;
 assert.deepEqual(next,{start:3,end:3,direction:'none'});
 assert.deepEqual(moveMapSelection(value,next,'ArrowLeft',false),caret);
 assert.deepEqual(moveMapSelection(value,{start:0,end:0,direction:'none'},'ArrowLeft',false),{start:0,end:0,direction:'none'});
 assert.deepEqual(moveMapSelection(value,{start:value.length,end:value.length,direction:'none'},'ArrowRight',false),{start:value.length,end:value.length,direction:'none'});
 assert.equal(moveMapSelection(value,caret,'Tab',false),null);
});

test('readonly map Shift selection preserves its anchor through extension, shrinking and crossing',()=>{
 const value='a😀雪b';let range:TextSelection={start:4,end:4,direction:'none'};
 range=moveMapSelection(value,range,'ArrowLeft',true)!;assert.deepEqual(range,{start:3,end:4,direction:'backward'});
 range=moveMapSelection(value,range,'ArrowLeft',true)!;assert.deepEqual(range,{start:1,end:4,direction:'backward'});
 range=moveMapSelection(value,range,'ArrowRight',true)!;assert.deepEqual(range,{start:3,end:4,direction:'backward'});
 range=moveMapSelection(value,range,'ArrowRight',true)!;assert.deepEqual(range,{start:4,end:4,direction:'forward'});
 range=moveMapSelection(value,range,'ArrowRight',true)!;assert.deepEqual(range,{start:4,end:5,direction:'forward'});
 assert.deepEqual(moveMapSelection(value,range,'ArrowLeft',false),{start:4,end:4,direction:'none'});
 assert.deepEqual(moveMapSelection(value,range,'ArrowRight',false),{start:5,end:5,direction:'none'});
});

test('readonly map vertical and line-edge navigation respects line and Unicode boundaries',()=>{
 const value='a😀b\nx\na😀yz';let range:TextSelection={start:3,end:3,direction:'none'};
 range=moveMapSelection(value,range,'ArrowDown',false)!;assert.deepEqual(range,{start:6,end:6,direction:'none'});
 range=moveMapSelection(value,range,'ArrowUp',false)!;assert.deepEqual(range,{start:1,end:1,direction:'none'});
 assert.deepEqual(moveMapSelection(value,range,'End',false),{start:4,end:4,direction:'none'});
 assert.deepEqual(moveMapSelection(value,{start:10,end:10,direction:'none'},'Home',false),{start:7,end:7,direction:'none'});
 assert.deepEqual(moveMapSelection('\n',{start:0,end:0,direction:'none'},'Home',false),{start:0,end:0,direction:'none'});
});
