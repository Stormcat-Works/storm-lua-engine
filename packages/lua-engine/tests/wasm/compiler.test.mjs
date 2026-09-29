import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
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
