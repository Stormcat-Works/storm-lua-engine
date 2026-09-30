import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import test from 'node:test';
import {TraceMap, eachMapping, originalPositionFor} from '@jridgewell/trace-mapping';
import {loadCompiler} from '../../dist/compiler.js';

const wasmBinary = await readFile(new URL('../../dist/compiler-wasm/compiler_bg.wasm', import.meta.url));
const compiler = await loadCompiler({wasmBinary});
const coordinator = await import('../../dist/compiler-wasm/compiler.js');
const worker = await import('../../dist/compiler-wasm/compiler.js?target-resume-worker');
await worker.default({module_or_path: wasmBinary});

const source = `local total=0
function onTick()
 local x=input.getNumber(1) local y=input.getNumber(2)
 total=total+x+y output.setNumber(1,total+x+x+y)
end
function onDraw()
 local w=screen.getWidth()
 screen.drawLine(0,0,w,w) screen.drawText(1,1,total)
end`;

test('an unmet target resumes only remaining canonical jobs across WASM instances', () => {
  const options = {mode: 'smallest', searchMode: 'exhaustive', targetSize: 0};
  const canonical = coordinator.prepareJobs(source, options);
  const attempt = coordinator.trySatisficing(source, options);
  assert.equal(attempt.done, false);
  assert.ok(attempt.prepared.jobs.length > 0);
  assert.equal(attempt.prepared.jobs.length, canonical.jobs.length - 2);
  const batches = attempt.prepared.jobs.map(job => worker.evaluateJob(job)).reverse();
  const resumed = coordinator.finishTargetSearch(source, options, attempt.prepared.context, batches, attempt.checkpoints);
  const expected = compiler.minify(source, {mode: 'smallest', searchMode: 'exhaustive'});
  assert.equal(resumed.ok, true);
  assert.equal(resumed.code, expected.code);
  assert.deepEqual(resumed.search.candidateSizes, expected.search.candidateSizes);
  assert.equal(resumed.search.attempted, expected.search.attempted);
  assert.equal(resumed.search.targetMet, false);
  assert.equal(resumed.search.stoppedEarly, false);
});

test('a fully evaluated small target search returns without exporting duplicate jobs', () => {
  const source = 'function onDraw()screen.drawRectF(1,2,3,4)end';
  const result = coordinator.trySatisficing(source, {targetSize: 0});
  assert.equal(result.done, true);
  assert.equal(result.prepared, undefined);
  assert.equal(result.result.code, compiler.minify(source).code);
  assert.equal(result.result.search.stage, 'full-search');
  const alreadySmall = '-- 日本語😀\nfunction onTick()end';
  const fit = compiler.minify(alreadySmall, {targetSize: alreadySmall.length});
  assert.equal(fit.code, alreadySmall);
  assert.equal(fit.search.candidateSizes[0].size, alreadySmall.length);
});

function position(text, needle) {
  const offset = text.indexOf(needle);
  assert.ok(offset >= 0, needle);
  const before = text.slice(0, offset);
  return {line: before.split('\n').length, column: before.length - before.lastIndexOf('\n') - 1};
}

test('non-minified map resolves UTF-16 columns on both sides of same-line linking', () => {
  const modules = {
    main: "local banner='😀あ';local f=require('lib');function onTick()output.setNumber(1,f())end\r\n",
    lib: "return function()\r\n local caption='雪😀';return 7\r\nend",
  };
  const result = compiler.build({entry: 'main', modules}, {minify: false});
  assert.equal(result.ok, true, JSON.stringify(result.diagnostics));
  const map = new TraceMap(result.map);
  for (const [file, needle] of [['main', 'output.setNumber'], ['lib', 'return 7'], ['lib', 'function()']]) {
    const generated = position(result.code, needle);
    const original = originalPositionFor(map, generated);
    assert.deepEqual({source: original.source, line: original.line, column: original.column},
      {source: `${file}.lua`, ...position(modules[file], needle)});
  }
  const fn = position(result.code, 'function()');
  assert.ok(fn.column > 0);
  assert.equal(originalPositionFor(map, {line: fn.line, column: 0}).source, null);
  assert.deepEqual(JSON.parse(result.map).sourcesContent.sort(), Object.values(modules).sort());
});

test('LifeBoat blanked Unicode code has no original mapping and kept tokens remain exact', () => {
  const main = "---@section __LB_SIMULATOR_ONLY__\nprint('😀雪')\n---@endsection\n---@section unused\nfunction unused() return '😀' end\n---@endsection\nlocal title='あ😀';function onTick()output.setNumber(1,7)end\n";
  const result = compiler.buildLifeboat({entry: 'main', modules: {main}}, {minify: false});
  assert.equal(result.ok, true, JSON.stringify(result.diagnostics));
  const map = new TraceMap(result.map);
  const p = originalPositionFor(map, position(result.code, 'output.setNumber'));
  assert.deepEqual({source: p.source, line: p.line, column: p.column},
    {source: 'main.lua', ...position(main, 'output.setNumber')});
  eachMapping(map, entry => {
    if (entry.source === 'main.lua') assert.ok(![2, 5].includes(entry.originalLine));
  });
  assert.equal(compiler.minify('function onTick()end').map, undefined);
});


test('reaching a target in the last canonical job reports complete search', () => {
  const source = 'function onDraw()\n' + Array.from({length: 64}, (_,i) =>
    `screen.drawRectF(${i%32},${Math.floor(i/32)},3,4)`).join('\n') + '\nend';
  const full = compiler.minify(source);
  const target = compiler.minify(source, {targetSize: full.code.length});
  assert.equal(target.code, full.code);
  assert.equal(target.search.targetMet, true);
  assert.equal(target.search.stoppedEarly, false);
  assert.equal(target.search.stage, 'full-search');
});
