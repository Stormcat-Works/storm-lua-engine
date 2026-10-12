import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import {TraceMap, originalPositionFor} from '@jridgewell/trace-mapping';
import { loadCompiler } from '../../dist/compiler.js';

const wasmBinary = new Uint8Array(await readFile(new URL('../../dist/compiler-wasm/compiler_bg.wasm', import.meta.url)));
const compiler = await loadCompiler({ wasmBinary });
const project = {
  entry: 'main',
  modules: {
    main: 'local helper=require("lib.helper")\nfunction onTick()output.setNumber(1,helper.value())end',
    'lib.helper': 'return {value=function()return 1+2 end}',
  },
};

test('compiler-only single-source entry produces deterministic Lua without executing it', () => {
  const source = 'function onTick()output.setNumber(1,1+2)end';
  const a = compiler.minify(source, { target: 'vehicle', numericMode: 'exact', zeroCostNewlines: false });
  const b = compiler.minify(source, { target: 'vehicle', numericMode: 'exact', zeroCostNewlines: false });
  assert.equal(a.ok, true);
  assert.equal(a.code, b.code);
  assert.equal(a.size, a.code.length);
  assert.equal(a.code, source);
  assert.equal(compiler.minify(source, { numericMode: 'tolerant', zeroCostNewlines: false }).code, 'function onTick()output.setNumber(1,3)end');
  assert.equal(compiler.minify('error("must not execute")', {environment: 'extended'}).ok, true);
});

test('multi-module build retains a readable source map or returns minification statistics', () => {
  const linked = compiler.build(project, { minify: false, target: 'vehicle' });
  const minified = compiler.build(project, { minify: true, numericMode: 'exact', zeroCostNewlines: false });
  assert.equal(linked.ok, true);
  assert.equal(minified.ok, true);
  const map = JSON.parse(linked.map);
  assert.ok(map.sources.includes('lib/helper.lua'));
  assert.equal(minified.map, undefined);
  assert.ok(minified.code.length < linked.code.length);
  assert.ok(minified.search);
  assert.deepEqual(compiler.analyze(project).diagnostics, []);
});

test('syntax and dependency errors return diagnostics without artifacts', () => {
  const malformed = compiler.minify('local x=(');
  assert.equal(malformed.ok, false);
  assert.equal(malformed.code, undefined);
  assert.equal(malformed.diagnostics[0].code, 'syntax-error');
  const missing = compiler.build({ entry: 'main', modules: { main: 'local x=require("missing")' } });
  assert.equal(missing.ok, false);
  assert.ok(missing.diagnostics.some(d => d.code === 'module-not-found'));
});

test('retired passes are absent from metadata and rejected even in already-small code', () => {
  const metadata = compiler.passMetadata();
  const ids = new Set(compiler.passIds());
  assert.ok(metadata.every(entry => ids.has(entry.id)));
  assert.equal(ids.size, 67);
  for (const id of ['general-expression-factoring', 'repeated-expression-factoring',
    'scalar-vector-loop-synthesis', 'redundant-nil-fallback-elimination']) {
    assert.equal(ids.has(id), false);
    for (const enabled of [false, true]) {
      const options = { passToggles: { [id]: enabled }, targetSize: 8192 };
      const result = compiler.minify('function onTick()end', options);
      assert.equal(result.ok, false);
      assert.equal(result.diagnostics[0].code, 'unknown-optimization-pass');
      assert.equal(compiler.build(project, { ...options, minify: false }).ok, false);
    }
  }
});

test('Addon target is rejected rather than applying vehicle assumptions', () => {
  for (const call of [
    () => compiler.minify('function onTick()end', { target: 'addon' }),
    () => compiler.build(project, { target: 'addon' }),
    () => compiler.analyze(project, { target: 'addon' }),
  ]) assert.throws(call, error => String(error).includes('addon'));
});

test('property scanning and explicit specialization share the compiler implementation', () => {
  const source = 'function onTick()output.setNumber(1,property.getNumber("Gain"))end';
  assert.deepEqual(compiler.scanProperties(source).numbers, ['Gain']);
  const runtime = compiler.minify(source, { property: { mode: 'runtime' } });
  const fixed = compiler.minify(source, { property: { mode: 'hardcode', numbers: { Gain: 7 } } });
  assert.equal(runtime.ok, true);
  assert.equal(fixed.ok, true);
  assert.ok(runtime.code.includes('property'));
  assert.equal(fixed.propertyReadsHardcoded, 1);
  assert.ok(fixed.code.includes('7'));
});

test('ambiguous initialization fails explicitly', async () => {
  await assert.rejects(loadCompiler({ wasmBinary, wasmUrl: 'unused.wasm' }), /Choose wasmBinary or wasmUrl/);
});

test('runtime analysis accepts host-resolved dynamic includes but preserves syntax diagnostics',async()=>{
  const compiler=await loadCompiler({wasmBinary:await readFile(new URL('../../dist/compiler-wasm/compiler_bg.wasm',import.meta.url))});
  const project={entry:'main',modules:{main:'function onTick()require(property.getText("file"))end'}};
  const options={mode:'runtime',environment:'extended',hostBindings:['require']};
  const result=compiler.analyze(project,options);
  assert.equal(result.ok,true);assert.equal(result.diagnostics.some(d=>d.severity==='error'),false);
  assert.ok(compiler.analyze(project).diagnostics.some(d=>d.code==='require-not-top-level'));
  assert.ok(compiler.analyze({entry:'main',modules:{main:'local ='}},options).diagnostics.some(d=>d.code==='syntax-error'));
});

test('variadic-only functions accepted by Lua are accepted by both compiler analysis and minification',async()=>{
  const compiler=await loadCompiler({wasmBinary:await readFile(new URL('../../dist/compiler-wasm/compiler_bg.wasm',import.meta.url))});
  const source='local f=function(...)return ... end function onTick(...)local a,b=f(3,7);output.setNumber(1,a);output.setNumber(2,b)end';
  const result=compiler.analyze({entry:'main',modules:{main:source}},{mode:'runtime',environment:'game'});
  assert.equal(result.ok,true);assert.equal(result.diagnostics.some(d=>d.code==='syntax-error'),false);
  assert.equal(compiler.minify(source).ok,true);
  assert.ok(compiler.analyze({entry:'main',modules:{main:'function f(...,x)end'}}).diagnostics.some(d=>d.code==='syntax-error'));
});

test('compiled metadata keeps field presence, insertion order and nonfinite number values',()=>{
  for(const sourceMap of [false,true]) {
    const source='-- 雪😀\r\nfunction onTick()end';
    const result=compiler.minify(source,{sourceMap,targetSize:8192,numericMode:'tolerant',numericTolerance:{abs:-0,rel:0.01}});
    assert.equal(result.ok,true);assert.equal(result.code,source);
    assert.equal(Object.getPrototypeOf(result),Object.prototype);
    assert.ok(Object.is(result.assumptions.numericTolerance.abs,-0));
    assert.equal(result.assumptions.numericTolerance.rel,0.01);
    assert.equal(Object.hasOwn(result,'map'),sourceMap);
    assert.equal(Object.hasOwn(result,'error'),false);
    assert.equal(Object.hasOwn(result,'elapsedMs'),false);
    assert.equal(Object.keys(result)[0],sourceMap?'map':'ok');
    assert.equal(result.search.beamWidth,null);
    if(sourceMap) {
      assert.equal(JSON.parse(result.map).sourcesContent[0],source);
      compiler.validateSourceMap(result.code,result.map);
    }
    for(const abs of [Infinity,-Infinity,NaN]) {
      const unusual=compiler.minify(source,{sourceMap,targetSize:8192,numericMode:'tolerant',numericTolerance:{abs,rel:0.01}});
      assert.equal(unusual.ok,true);assert.ok(Object.is(unusual.assumptions.numericTolerance.abs,abs));
      if(sourceMap)compiler.validateSourceMap(unusual.code,unusual.map);
    }
  }
});

test('large returned code and map strings are exact values rather than nested encoded payloads',()=>{
  const text='\\"integrity\\":\\"\\"\\n雪😀'.repeat(256);
  const source=`local _ENV=_ENV\nlocal value=${JSON.stringify(text)}\nfunction onDraw()screen.drawText(0,0,value)end`;
  const plain=compiler.minify(source,{sourceMap:false});
  const mapped=compiler.minify(source,{sourceMap:true});
  assert.equal(plain.ok,true);assert.equal(mapped.ok,true);assert.equal(plain.code,mapped.code);
  const map=JSON.parse(mapped.map);
  assert.equal(map.sourcesContent[0],source);
  compiler.validateSourceMap(mapped.code,mapped.map);
  const {map:unused,...metadata}=mapped;
  assert.deepEqual(metadata,plain);
  const failed=compiler.minify('local =',{sourceMap:true});
  assert.equal(failed.ok,false);assert.equal(Object.hasOwn(failed,'map'),false);assert.equal(Object.hasOwn(failed,'code'),false);
  assert.equal(failed.error,failed.diagnostics[0].message);
});

test('Addon linking and metadata stripping preserve original snapshots without Vehicle rules', () => {
  const source='--[==[studio\n{}\n]==]\n---@section __LB_SIMULATOR_ONLY__\nrequire("missing")\n---@endsection\nfunction onCreate()g_savedata.x=3 end';
  const linked=compiler.buildAddon({entry:'main',modules:{main:source}}, {metadataTag:'studio'});
  assert.equal(linked.ok,true,JSON.stringify(linked.diagnostics));
  assert.ok(!linked.code.includes('studio') && !linked.code.includes('missing'));
  assert.equal(JSON.parse(linked.map).sourcesContent[0],source);
  assert.equal(linked.search,undefined);
  assert.throws(()=>compiler.buildAddon(project,{minify:true}),/unknown field/);
  assert.equal(compiler.buildAddon({entry:'main',modules:{main:'pcall(function()end)'}}).ok,false);
});

test('nested conditional calls complete in both map modes', async () => {
  const source='function onTick()if input.getBool(1) then if input.getBool(2) then output.setNumber(1,2)else output.setNumber(1,3)end else output.setNumber(1,4)end end';
  const plain=compiler.minify(source,{sourceMap:false});
  const mapped=compiler.minify(source,{sourceMap:true});
  assert.equal(plain.ok,true);assert.equal(mapped.ok,true);
  assert.equal(plain.code,mapped.code);compiler.validateSourceMap(mapped.code,mapped.map);
});


function metadataPosition(code, token) {
  const prefix=code.slice(0,code.indexOf(token));
  return {line:prefix.split('\n').length,column:prefix.length-prefix.lastIndexOf('\n')-1};
}
function metadataSnapshots(map, project) {
  for(const [name,source] of Object.entries(project.modules)) {
    assert.equal(map.sourcesContent[map.sources.indexOf(name.replaceAll('.','/')+'.lua')],source);
  }
}
function metadataToken(map, code, project, module, token) {
  const actual=originalPositionFor(new TraceMap(map),metadataPosition(code,token));
  assert.equal(actual.source,module.replaceAll('.','/')+'.lua');
  const expected=metadataPosition(project.modules[module],token);
  assert.deepEqual({line:actual.line,column:actual.column},expected);
}

test('non-minified metadata builds keep exact remaining positions and unmapped removed bytes', () => {
  for(const source of [
    '--[[stormmin-metadata\nprivate development note\n]]\nfunction onTick()output.setNumber(1,3)end',
    ' \t--[==[stormmin-metadata\r\n雪😀\r\n]==]\r\nfunction onTick()output.setNumber(1,3)end',
    '--[[stormmin-metadata\n雪😀]]function onTick()output.setNumber(1,3)end',
  ]) {
    const project={entry:'main',modules:{main:source}};
    for(const build of [compiler.build.bind(compiler),compiler.buildLifeboat.bind(compiler)]) {
      for(const sourceMap of [false,true]) {
        const result=build(project,{metadataTag:'stormmin-metadata',minify:false,sourceMap});
        assert.equal(result.ok,true,JSON.stringify(result));
        const map=JSON.parse(result.map);metadataSnapshots(map,project);
        metadataToken(map,result.code,project,'main','output.setNumber');
        const start=result.code.indexOf('function onTick')-Buffer.byteLength(source.slice(0,source.indexOf('function onTick')))+source.indexOf('--[');
        const prefix=Buffer.from(result.code).subarray(0,start).toString('utf8');
        const point={line:prefix.split('\n').length,column:prefix.length-prefix.lastIndexOf('\n')-1};
        assert.equal(originalPositionFor(new TraceMap(map),point).source,null);
        if(sourceMap) {
          const data=compiler.validateSourceMap(result.code,result.map);
          const removed=data.mappings.find(m=>m.start<=start && start<m.end);
          assert.equal(removed.copied,null);
          assert.equal(data.origins[removed.origin].kind,'synthetic');
        }
      }
    }
  }
});

test('metadata maps retain each module snapshot including fully blanked bodies in both minify modes', () => {
  const project={entry:'main',modules:{
    main:'--[[stormmin-metadata\nmain note\n]]\nrequire("empty");require("lib");function onTick()output.setNumber(1,value())end',
    lib:'--[=[stormmin-metadata\n雪😀]=]function value()return 3 end',
    empty:'--[[stormmin-metadata\nempty note\n]]',
  }};
  for(const build of [compiler.build.bind(compiler),compiler.buildLifeboat.bind(compiler)]) {
    for(const minify of [false,true]) {
      const plain=build(project,{metadataTag:'stormmin-metadata',minify,sourceMap:false});
      const mapped=build(project,{metadataTag:'stormmin-metadata',minify,sourceMap:true});
      assert.equal(plain.ok,true,JSON.stringify(plain));assert.equal(mapped.ok,true,JSON.stringify(mapped));
      assert.equal(plain.code,mapped.code);
      const map=JSON.parse(mapped.map);metadataSnapshots(map,project);compiler.validateSourceMap(mapped.code,mapped.map);
      if(!minify) {
        metadataSnapshots(JSON.parse(plain.map),project);
        metadataToken(map,mapped.code,project,'main','output.setNumber');
        metadataToken(map,mapped.code,project,'lib','function value');
      }
    }
    const empty={entry:'main',modules:{main:project.modules.empty}};
    for(const sourceMap of [false,true]) {
      const result=build(empty,{metadataTag:'stormmin-metadata',minify:false,sourceMap});
      assert.equal(result.ok,true,JSON.stringify(result));metadataSnapshots(JSON.parse(result.map),empty);
      if(sourceMap) assert.ok(compiler.validateSourceMap(result.code,result.map).mappings.every(m=>m.copied===null));
    }
  }
});

test('Addon metadata map excludes blanked text and preserves following UTF16 columns', () => {
  const project={entry:'main',modules:{main:'--[[stormmin-metadata\n雪😀]]function onCreate()g_savedata.value=3 end'}};
  const result=compiler.buildAddon(project,{metadataTag:'stormmin-metadata'});
  assert.equal(result.ok,true,JSON.stringify(result));const map=JSON.parse(result.map);metadataSnapshots(map,project);
  metadataToken(map,result.code,project,'main','g_savedata');
  const start=result.code.indexOf('function onCreate')-Buffer.byteLength(project.modules.main.slice(0,project.modules.main.indexOf('function onCreate')));
  const prefix=Buffer.from(result.code).subarray(0,start).toString('utf8');
  assert.equal(originalPositionFor(new TraceMap(map),{line:prefix.split('\n').length,column:prefix.length-prefix.lastIndexOf('\n')-1}).source,null);
  const empty={entry:'main',modules:{main:'--[[stormmin-metadata\nempty note\n]]'}};
  const blank=compiler.buildAddon(empty,{metadataTag:'stormmin-metadata'});
  assert.equal(blank.ok,true,JSON.stringify(blank));metadataSnapshots(JSON.parse(blank.map),empty);
});


test('metadata detailed maps validate identity targets and conservative minification', () => {
  for(const source of [
    '--[[stormmin-metadata\nnote\n]]\nfunction onTick()output.setNumber(1,3)end',
    '--[[stormmin-metadata\n雪😀\n]]\nlocal _ENV=_ENV;function onTick()output.setNumber(1,3)end',
  ]) {
    for(const build of [compiler.build.bind(compiler),compiler.buildLifeboat.bind(compiler)]) {
      const project={entry:'main',modules:{main:source}};
      const result=build(project,{metadataTag:'stormmin-metadata',minify:true,sourceMap:true,targetSize:8192});
      assert.equal(result.ok,true,JSON.stringify(result));compiler.validateSourceMap(result.code,result.map);
      metadataSnapshots(JSON.parse(result.map),project);
    }
  }
});


test('minify nesting and integer overflow return normally and preserve the compiler instance', () => {
  for (const source of [
    'return '+'('.repeat(6400)+'1'+')'.repeat(6400),
    'return '+'not '.repeat(6400)+'1',
    'return 1'+'+1'.repeat(6400),
    'do '.repeat(6400)+'end '.repeat(6400),
  ]) {
    const result=compiler.minify(source);
    assert.equal(result.ok,false);
    assert.match(result.error,/nesting/);
    assert.equal(compiler.minify('return 1+2').ok,true);
  }
  const result=compiler.minify('return 0x8000000000000000//-1',{sourceMap:true});
  assert.equal(result.ok,true);
  compiler.validateSourceMap(result.code,result.map);
});
