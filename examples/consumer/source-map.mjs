/** Non-minified project -> game VM -> source-qualified breakpoint and error locations.
 * Install the SDK and @jridgewell/trace-mapping in the consumer, then run this file.
 * This uses a normal source-map consumer; no map decoder or optimizer lives here.
 */
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {TraceMap, eachMapping, originalPositionFor} from '@jridgewell/trace-mapping';
import {loadRuntime} from '@stormcat-works/storm-lua-engine';
import {loadCompiler} from '@stormcat-works/storm-lua-engine/compiler';

const root = new URL('.', import.meta.resolve('@stormcat-works/storm-lua-engine'));
const engine = await loadRuntime({wasmBinary: await readFile(new URL('wasm/storm_lua_wasm.wasm', root))});
const compiler = await loadCompiler({
  moduleUrl: new URL('compiler-wasm/compiler.js', root),
  wasmBinary: await readFile(new URL('compiler-wasm/compiler_bg.wasm', root)),
});
const project = {entry: 'main', modules: {
  main: 'local util = require("lib.util")\nfunction onTick()\n  output.setNumber(1,util.twice(input.getNumber(1)))\nend\n',
  'lib.util': 'local M={}\nfunction M.twice(x)\n  local result=x*2\n  return result\nend\nreturn M\n',
}};
const artifact = compiler.build(project, {environment: 'game', minify: false});
if (!artifact.ok || artifact.code === undefined || artifact.map === undefined) {
  throw new Error(`Build failed: ${JSON.stringify(artifact.diagnostics)}`);
}
// Keep this exact code/map pair together. Edits or minification invalidate its locations.
const map = new TraceMap(artifact.map);
const chunkName = '@mapped-program.lua';
function generatedBreakpoints(source, line) {
  const generated = new Set();
  eachMapping(map, entry => {
    if (entry.source === source && entry.originalLine === line) generated.add(entry.generatedLine);
  });
  if (generated.size === 0) throw new Error(`No exact non-minified source mapping for ${source}:${line}`);
  // Do not silently use a nearest-line match for a removed or unmapped source line.
  return [...generated].map(line => ({source: chunkName, line}));
}
function originalFrame(frame) {
  if (frame.source !== chunkName || frame.line <= 0) return null;
  const result = originalPositionFor(map, {line: frame.line, column: 0});
  return result.source === null ? null : result;
}
const vm = engine.createVehicle({environment: 'game'});
try {
  vm.load(artifact.code, chunkName);
  vm.io.inputNumbers[0] = 3;
  vm.setBreakpoints(generatedBreakpoints('lib/util.lua', 3));
  assert.equal(vm.tick(), 'suspended');
  const stopped = originalFrame(vm.stack()[0]);
  assert.equal(stopped?.source, 'lib/util.lua');
  assert.equal(stopped?.line, 3);
  const caller = vm.stack().map(originalFrame).find(frame => frame?.source === 'main.lua');
  assert.equal(caller?.line, 3);
  vm.setBreakpoints([]);
  assert.equal(vm.resume('over'), 'suspended');
  assert.equal(originalFrame(vm.stack()[0])?.line, 4);
  assert.equal(vm.resume(), 'completed');
  assert.equal(vm.io.outputNumbers[0], 6);
  vm.reset();
  vm.io.inputNumbers[0] = 4;
  assert.equal(vm.tick(), 'completed');
  assert.equal(vm.io.outputNumbers[0], 8);
  console.log(`Mapped debug: ${stopped.source}:${stopped.line}; step -> line 4; outputs 6 and 8 after reset.`);
} finally {
  vm.dispose();
}

// Runtime errors use the same generated-line -> original-line direction.
const broken = compiler.build({entry: 'main', modules: {
  main: 'local run = require("lib.broken")\nfunction onTick()run()end\n',
  'lib.broken': 'return function()\n  local missing\n  return missing.value\nend\n',
}}, {environment: 'game', minify: false});
assert.equal(broken.ok, true);
const brokenMap = new TraceMap(broken.map);
const failed = engine.createVehicle();
try {
  failed.load(broken.code, chunkName);
  let original;
  assert.throws(() => failed.tick(), error => {
    const match = /mapped-program\.lua:(\d+):/.exec(error.message);
    if (!match) return false; // Unknown error formats are not assigned a guessed location.
    original = originalPositionFor(brokenMap, {line: Number(match[1]), column: 0});
    return original.source === 'lib/broken.lua' && original.line === 3;
  });
  console.log(`Mapped runtime error: ${original.source}:${original.line}.`);
} finally {
  failed.dispose();
}
const minified = compiler.build(project, {environment: 'game', minify: true});
assert.equal(minified.ok, true);
assert.equal(minified.map, undefined); // Optimized maps are opt-in; never attach a stale unoptimized linked map.

// v0.3.0: opt-in optimized maps include structured, validated explanations.
// This remains source navigation, not reconstruction of eliminated VM variables.
const optimizedSource = 'local function twice(value)\n return value*2\nend\nfunction onTick()output.setNumber(1,twice(input.getNumber(1)))end';
const optimized = compiler.minify(optimizedSource, {sourceMap:true,sourceName:'optimized.lua',numericMode:'exact',zeroCostNewlines:false});
assert.equal(optimized.ok,true);
const details = compiler.validateSourceMap(optimized.code,optimized.map);
assert.equal(details.schemaVersion,1);
assert.equal(details.producer.version,'0.3.0');
const star = optimized.code.indexOf('*2');assert.ok(star>=0);
const point = originalPositionFor(new TraceMap(optimized.map), {line:1,column:star+1});
assert.equal(point.source,'optimized.lua');assert.equal(point.line,2);
const leaf = details.mappings.find(m=>m.start<=star+1&&star+1<m.end);
assert.ok(leaf.inlineContexts.length>0);
assert.throws(()=>compiler.validateSourceMap(optimized.code.replace('*2','*3'),optimized.map),/code|fingerprint/i);
const optimizedVm=engine.createVehicle({environment:'game'});
try {
  optimizedVm.load(optimized.code,'@optimized.lua');optimizedVm.io.inputNumbers[0]=6;
  assert.equal(optimizedVm.tick(),'completed');assert.equal(optimizedVm.io.outputNumbers[0],12);
} finally { optimizedVm.dispose(); }
console.log('Optimized map: exact code/source fingerprints, original literal, inline call context, output 12.');
