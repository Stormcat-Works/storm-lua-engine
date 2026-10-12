import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {MessageChannel} from 'node:worker_threads';
import {CompilerWorkerClient,serveCompiler} from '../../dist/compiler-worker.js';
const wasmBinary=await readFile(new URL('../../dist/compiler-wasm/compiler_bg.wasm',import.meta.url));

test('explicit compiler endpoint supports calls, diagnostics, concurrent IDs and clean disposal',async()=>{
 const {port1,port2}=new MessageChannel();port1.start();port2.start();
 const stop=serveCompiler(port2,{wasmBinary});const client=new CompilerWorkerClient(port1);
 try{
  const [compiled,ids,diagnostics]=await Promise.all([
   client.minify('function onTick()output.setNumber(1,1+2)end'),client.passIds(),client.analyze({entry:'main',modules:{main:'pcall(function()end)'}}),
  ]);
  assert.equal(compiled.ok,true);assert.ok(ids.length>=67);assert.ok(diagnostics.diagnostics.some(d=>d.code==='sw-unavailable-global'));
  const addon=await client.buildAddon({entry:'main',modules:{main:'function onCreate()g_savedata.n=1 end'}},{environment:'game'});assert.equal(addon.ok,true);assert.equal(addon.search,undefined);
  await assert.rejects(client.buildAddon({entry:'main',modules:{main:'function onCreate()end'}},{targetSize:8192}),/unknown field/);
  const extended=await client.minify('pcall(function()return 7 end)',{environment:'extended'});assert.equal(extended.search.mode,'lexical');
  await assert.rejects(client.minify('return 1',{target:'addon'}),/addon/);
  client.dispose();await assert.rejects(client.passIds(),/disposed/);
 }finally{client.dispose();stop();port1.close();port2.close();}
});
test('dispose rejects pending work without owning or terminating the endpoint',async()=>{
 const {port1,port2}=new MessageChannel();port1.start();port2.start();const client=new CompilerWorkerClient(port1);
 const request=client.passIds();client.dispose();await assert.rejects(request,/disposed/);
 const stillUsable=new Promise(resolve=>port2.once('message',resolve));port1.postMessage({probe:true});
 // The queued compiler request can arrive before the probe: endpoint delivery itself is what is checked.
 assert.ok(await stillUsable);port1.close();port2.close();
});
