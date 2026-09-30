/** Exercise the compiler-only SDK in real module Workers without runtime assets. */
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { extname, resolve, sep } from 'node:path';
const require = createRequire(new URL('../packages/lua-engine/package.json', import.meta.url));
const playwright = require('playwright');
const dist = fileURLToPath(new URL('../packages/lua-engine/dist/', import.meta.url));
const requested = [];
const workerSource = `
import { loadCompiler } from '/dist/compiler.js';
self.onmessage = async () => {
  try {
    const compiler = await loadCompiler();
    const project = {entry:'main',modules:{main:'local m=require("lib.m") function onTick()output.setNumber(1,m.n)end','lib.m':'return {n=3}'}};
    const linked = compiler.build(project, {minify:false,target:'vehicle'});
    const result = compiler.build(project, {minify:true,target:'vehicle',numericMode:'exact',zeroCostNewlines:false});
    const diagnostics = compiler.analyze(project).diagnostics;
    const retired = compiler.minify('function onTick()end', {passToggles:{'general-expression-factoring':true}});
    let addonRejected=false;
    try {compiler.minify('function onTick()end',{target:'addon'});} catch (error) {addonRejected=String(error).includes('addon');}
    const original='local function twice(value)return value*2 end function onTick()output.setNumber(1,twice(input.getNumber(1)))end';
    const mapped=compiler.minify(original,{sourceMap:true,sourceName:'original.lua',numericMode:'exact'});
    if (!mapped.ok || typeof mapped.map!=='string') throw new Error('optimized source map is missing');
    const details=compiler.validateSourceMap(mapped.code,mapped.map);
    let staleRejected=false;
    try {compiler.validateSourceMap(mapped.code+' ',mapped.map);}catch {staleRejected=true;}
    const linkedMapped=compiler.build(project,{sourceMap:true});
    if (!linkedMapped.ok) throw new Error('mapped project compilation failed');
    const linkedDetails=compiler.validateSourceMap(linkedMapped.code,linkedMapped.map);
    self.postMessage({linked,result,diagnostics,retired,addonRejected,ids:compiler.passIds(),
      mapping:{schema:details.schemaVersion,producer:details.producer.version,contexts:details.contexts.length,
        leafContexts:details.mappings.some(m=>m.inlineContexts.length>0),reasonCount:details.reasons.length,
        staleRejected,linkedSources:linkedDetails.sources.length}});
  } catch (error) {self.postMessage({error:String(error)});}
};`;
const server = createServer(async (request, response) => {
  try {
    const path = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
    requested.push(path);
    if (path === '/') { response.setHeader('Content-Type', 'text/html'); response.end('<!doctype html><title>Compiler worker test</title>'); return; }
    if (path === '/worker.js') { response.setHeader('Content-Type', 'text/javascript'); response.end(workerSource); return; }
    if (!path.startsWith('/dist/')) { response.writeHead(404).end(); return; }
    const file = resolve(dist, path.slice('/dist/'.length));
    if (!file.startsWith(resolve(dist) + sep)) { response.writeHead(403).end(); return; }
    const data = await readFile(file);
    response.setHeader('Content-Type', ({'.js':'text/javascript','.wasm':'application/wasm','.json':'application/json'})[extname(file)] ?? 'application/octet-stream');
    response.end(data);
  } catch (error) {
    response.writeHead(404).end(String(error));
  }
});
await new Promise((ok, fail) => { server.once('error', fail); server.listen(0, '127.0.0.1', ok); });
try {
  const url = `http://127.0.0.1:${server.address().port}`;
  for (const name of ['chromium', 'firefox', 'webkit']) {
    const browser = await playwright[name].launch({ headless: true, timeout: 30000 });
    const start = requested.length;
    try {
      const page = await browser.newPage();
      await page.goto(url);
      const result = await page.evaluate(() => new Promise((resolve, reject) => {
        const worker = new Worker('/worker.js', { type: 'module' });
        const timer = setTimeout(() => { worker.terminate(); reject(new Error('Compiler worker timeout')); }, 30000);
        worker.onerror = event => { clearTimeout(timer); worker.terminate(); reject(new Error(event.message)); };
        worker.onmessage = event => { clearTimeout(timer); worker.terminate(); resolve(event.data); };
        worker.postMessage({});
      }));
      assert.equal(result.error, undefined);
      assert.equal(result.linked.ok, true);
      assert.equal(result.result.ok, true);
      assert.ok(result.result.code.length < result.linked.code.length);
      assert.equal(JSON.parse(result.linked.map).version, 3);
      assert.deepEqual(result.diagnostics, []);
      assert.equal(result.retired.ok, false);
      assert.equal(result.retired.diagnostics[0].code, 'unknown-optimization-pass');
      assert.equal(result.addonRejected, true);
      assert.equal(new Set(result.ids).size, 67);
      assert.equal(result.mapping.schema,1);
      assert.equal(result.mapping.producer,'0.3.0');
      assert.ok(result.mapping.contexts>0 && result.mapping.leafContexts);
      assert.ok(result.mapping.reasonCount>0);
      assert.equal(result.mapping.staleRejected,true);
      assert.equal(result.mapping.linkedSources,2);
      const wasmRequests = requested.slice(start).filter(path => path.endsWith('.wasm'));
      assert.deepEqual(wasmRequests, ['/dist/compiler-wasm/compiler_bg.wasm']);
      console.log(`${name}: module Worker, project build, optimized maps/reasons/inline contexts and stale rejection, lint, retired/Addon rejection; only compiler WASM fetched`);
    } finally { await browser.close(); }
  }
} finally { await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve())); }
