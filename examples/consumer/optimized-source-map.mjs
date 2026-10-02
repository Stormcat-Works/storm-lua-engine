/** v0.3.0: generate, validate and inspect an optimized source map.
 * Run in a Node ESM project after installing:
 * npm install @stormcat-works/storm-lua-engine@0.3.0 @jridgewell/trace-mapping
 * node optimized-source-map.mjs
 * This example never executes the supplied Lua or reads a source file named by a map.
 */
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {loadCompiler} from '@stormcat-works/storm-lua-engine/compiler';
import {TraceMap, originalPositionFor} from '@jridgewell/trace-mapping';

// Node supplies the bytes explicitly. Browser hosts serve the matching compiler.js
// and compiler_bg.wasm and should initialize the compiler inside their own Worker.
const assets = new URL('.', import.meta.resolve('@stormcat-works/storm-lua-engine'));
const compiler = await loadCompiler({
  moduleUrl: new URL('compiler-wasm/compiler.js', assets),
  wasmBinary: await readFile(new URL('compiler-wasm/compiler_bg.wasm', assets)),
});
const source = [
  '-- 保存する原文 😀',
  'local function twice(value)',
  '  return value*2',
  'end',
  'function onTick()',
  '  output.setNumber(1,twice(input.getNumber(1))+2*3)',
  'end',
].join('\n');
const result = compiler.minify(source, {
  sourceMap: true,
  sourceName: 'controller.lua',
  // This demonstration opts into the numeric transformations that fold 2*3.
  // Exact mode still maps correctly, but intentionally enables a different pass set.
  numericMode: 'tolerant',
  zeroCostNewlines: false,
});
if (!result.ok || result.code === undefined || result.map === undefined) {
  throw new Error(`Compilation failed: ${JSON.stringify(result.diagnostics)}`);
}
// Store/transfer this exact pair. A stale map must not be attached to edited code.
const artifact = {code: result.code, map: result.map};
const details = compiler.validateSourceMap(artifact.code, artifact.map);
assert.equal(details.schemaVersion, 1);
assert.equal(details.producer.version, '0.3.0');
const standard = JSON.parse(artifact.map); // Only inspect after the SDK's validation.
const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', {fatal: true});
const generatedBytes = encoder.encode(artifact.code);
const sourceBytes = standard.sourcesContent.map(text => encoder.encode(text));
function originalText(span) {
  return span === null ? null : decoder.decode(sourceBytes[span.source].subarray(span.start, span.end));
}

// Choose a concrete generated range in this example. Real editors supply a byte
// offset derived from their selection; JavaScript string indexes are UTF-16, not bytes.
const selected = details.mappings.find(mapping => mapping.origin !== null
  && originalText(details.origins[mapping.origin].primary) === '2*3');
assert.ok(selected, 'The original product must have a mapped generated value');
const origin = details.origins[selected.origin];
const prefix = decoder.decode(generatedBytes.subarray(0, selected.start));
const generatedLine = prefix.split('\n').length;       // trace-mapping uses 1-based lines.
const generatedColumn = prefix.length - prefix.lastIndexOf('\n') - 1; // 0-based UTF-16.
const location = originalPositionFor(new TraceMap(artifact.map), {
  line: generatedLine, column: generatedColumn,
});
assert.equal(location.source, 'controller.lua');
assert.equal(location.line, 6);
assert.equal(decoder.decode(generatedBytes.subarray(selected.start, selected.end)), '6');

// Reasons on a containing expression/statement are intentionally separate from
// a leaf's own source position. The host can show both, as the Playground does.
const reasonIds = new Set(origin.reasons);
for (const construct of details.constructs) {
  if (construct.origin !== null && construct.start <= selected.start && selected.end <= construct.end) {
    for (const id of details.origins[construct.origin].reasons) reasonIds.add(id);
  }
}
const explanation = {
  generated: decoder.decode(generatedBytes.subarray(selected.start, selected.end)),
  original: {source: location.source, line: location.line, column: location.column, text: originalText(origin.primary)},
  precision: origin.precision,
  kind: origin.kind,
  reasons: [...reasonIds].map(id => details.reasons[id]),
  related: origin.related.map(id => details.relations[id]),
  inlineContexts: selected.inlineContexts.map(id => details.contexts[id]),
};
assert.ok(explanation.reasons.some(reason => reason.code === 'constant-folding'));
assert.ok(details.contexts.length > 0, 'The inlined twice call retains its own context');
assert.throws(() => compiler.validateSourceMap(artifact.code + '\n', artifact.map), /code|fingerprint|belong/i);
console.log(JSON.stringify({producer: details.producer, code: artifact.code, explanation}, null, 2));

// A single module can use the same API. A project uses build(project,
// {minify:true,sourceMap:true}); the resulting source table identifies each file.
// Runtime errors and breakpoints are a separate host concern: if the VM only
// reports a generated line, show all candidates for that line, not a guessed column.
