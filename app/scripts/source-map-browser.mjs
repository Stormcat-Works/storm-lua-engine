/** Real SDK/Worker integration under the same strict CSP as the built Playground. */
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {join} from 'node:path';

async function complete(page){
 await page.waitForFunction(()=>['complete','error'].includes(document.body.dataset.runState));
 assert.equal(await page.locator('body').getAttribute('data-run-state'),'complete',await page.locator('#error-banner').textContent());
}
async function saved(page){await page.waitForFunction(()=>document.querySelector('#saved').textContent==='この端末に保存済み');}
async function select(page,side,needle,options={}){
 const range=await page.locator(`#map-${side}`).evaluate((area,{needle,skip,offset,length})=>{
  let at=-1;for(let i=0;i<=skip;i++)at=area.value.indexOf(needle,at+1);
  if(at<0)throw new Error(`missing ${needle} in ${area.value}`);
  at+=offset;area.focus();area.setSelectionRange(at,at+length);return [at,at+length];
 },{needle,skip:options.skip??0,offset:options.offset??0,length:options.length??needle.length});
 await page.locator(`#map-select-${side}`).click();return range;
}
async function download(page,button){const pending=page.waitForEvent('download');await page.locator(button).click();const d=await pending;return JSON.parse(await readFile(await d.path(),'utf8'));}
export async function sourceMapBrowser({page,browser,name,url,evidence}){
 await page.locator('[data-recipe="source-map"]').click();await page.locator('#run').click();await complete(page);
 await page.locator('#map-panel').waitFor({state:'visible'});
 assert.match(await page.locator('#map-meta').textContent(),/Engine 0\.3\.0.*Unknown 0 bytes/);
 assert.equal(await page.locator('#map-tick').isDisabled(),true,'compilation must not load a VM');
 await select(page,'generated','*2',{offset:1,length:1});
 assert.match(await page.locator('#map-detail').textContent(),/インライン文脈/);
 assert.match(await page.locator('#map-detail').textContent(),/twice\(input.getNumber\(1\)\)/);
 await select(page,'generated','6');
 assert.match(await page.locator('#map-detail').textContent(),/literal-operands-evaluated-and-size-nonincreasing/);
 assert.ok(Number(await page.locator('#map-disposition-count').textContent())>0);
 await select(page,'original','unused');assert.equal(await page.locator('#map-candidates .map-hit').count(),0);
 assert.match(await page.locator('#map-detail').textContent(),/除去・置換/);
 await select(page,'generated','6');
 await page.locator('#map-detail .map-reason').evaluateAll(nodes=>{for(const n of nodes)n.open=true;});
 if(name==='chromium'){
  await page.locator('#map-panel').scrollIntoViewIfNeeded();
  await page.screenshot({path:join(evidence,'source-map-desktop.png'),fullPage:true,caret:'initial'});
  await page.locator('#map-panel').screenshot({path:join(evidence,'source-map-inspector.png'),caret:'initial'});
 }
 const bundle=await download(page,'#map-export');assert.equal(bundle.kind,'storm-lua-artifact');
 const generated=await page.locator('#map-generated').inputValue();
 const beforeInput=await page.locator('#source').inputValue();
 await page.locator('#source').fill(beforeInput+'\n-- edited input, old artifact remains paired');
 await page.locator('#map-stale').waitFor({state:'visible'});
 assert.equal(await page.locator('#map-generated').inputValue(),generated);
 await select(page,'generated','6');
 // Runtime configuration is reproducible but never starts a VM on restore.
 await page.locator('#map-numbers').evaluate(n=>{n.value='[11]';n.dispatchEvent(new Event('input',{bubbles:true}));});
 await saved(page);
 const workspace=await download(page,'#export');assert.equal(workspace.kind,'storm-lua-playground-workspace');
 assert.equal(workspace.inspection.artifact.code,bundle.code);assert.equal(workspace.inspection.inputs.numbers,'[11]');
 const selection=workspace.inspection.selection;
 await page.reload();await page.locator('#map-panel').waitFor({state:'visible'});await saved(page);
 assert.equal(await page.locator('#map-generated').inputValue(),generated);
 assert.match(await page.locator('#map-detail').textContent(),/constant-folding/);
 assert.equal(await page.locator('#map-numbers').inputValue(),'[11]');
 assert.equal(await page.locator('#map-stale').isVisible(),true);assert.equal(await page.locator('#map-tick').isDisabled(),true);
 const roundtrip=await download(page,'#export');assert.deepEqual(roundtrip.inspection.selection,selection);
 const keepSource=await page.locator('#source').inputValue();
 const bad={...bundle,code:bundle.code.replace('6','8')};assert.notEqual(bad.code,bundle.code);
 await page.locator('#map-import-file').setInputFiles({name:'mismatched.json',mimeType:'application/json',buffer:Buffer.from(JSON.stringify(bad))});
 await page.waitForFunction(()=>document.body.dataset.runState==='error');assert.equal(await page.locator('#map-generated').inputValue(),generated);
 assert.equal(await page.locator('#source').inputValue(),keepSource);
 const invalidFrame={...workspace,lastFrame:{kind:'frame',width:64,height:32,format:'rgba8',$bytes:[1]}};
 await page.locator('#import-file').setInputFiles({name:'bad-workspace.json',mimeType:'application/json',buffer:Buffer.from(JSON.stringify(invalidFrame))});
 await page.waitForFunction(()=>document.querySelector('#error-banner').textContent.includes('インポートを拒否'));
 assert.equal(await page.locator('#source').inputValue(),keepSource);assert.equal(await page.locator('#map-generated').inputValue(),generated);
 const fresh=await browser.newContext({acceptDownloads:true,viewport:{width:1440,height:1100}});
 try{
  const other=await fresh.newPage();await other.goto(url);await other.locator('#run:not([disabled])').waitFor();
  await other.locator('#import-file').setInputFiles({name:'workspace.json',mimeType:'application/json',buffer:Buffer.from(JSON.stringify(workspace))});
  await other.locator('#map-panel').waitFor({state:'visible'});await saved(other);
  assert.equal(await other.locator('#source').inputValue(),workspace.project.source);
  assert.equal(await other.locator('#map-generated').inputValue(),generated);
  assert.equal(await other.locator('#map-tick').isDisabled(),true);
  assert.match(await other.locator('#map-detail').textContent(),/constant-folding/);
 }finally{await fresh.close();}
 // Module selection is from embedded snapshots, not from filesystem fetches.
 await page.locator('[data-recipe="source-map-modules"]').click();await page.locator('#run').click();await complete(page);
 const options=await page.locator('#map-source option').allTextContents();assert.ok(options.includes('main.lua')&&options.includes('lib.lua'));
 await page.locator('#map-source').selectOption({label:'lib.lua'});
 assert.match(await page.locator('#map-original').inputValue(),/return value\*2/);
 await select(page,'original','2');assert.ok(await page.locator('#map-candidates .map-hit').count()>0);
 // Actual native Lua WASM suspension, step and error (not a mocked runtime).
 await page.locator('[data-recipe="source-map-debug"]').click();await page.locator('#run').click();await complete(page);
 assert.match(await page.locator('#map-runtime-result').textContent(),/error/);
 assert.match(await page.locator('#map-runtime-result').textContent(),/error · @playground-map-\d+\.lua:\d+ · 生成行のみ/);
 const errorLink=page.locator('#map-runtime-result>button').filter({hasText:'error ·'});await errorLink.first().click();
 assert.match(await page.locator('#map-message').textContent(),/生成 → 原文/);
 await page.locator('#map-load').click();await complete(page);
 await select(page,'original','output.setNumber');await page.locator('#map-breakpoints').click();await complete(page);
 await page.locator('#map-tick').click();await complete(page);
 assert.match(await page.locator('#map-runtime-result>strong').textContent(),/suspended/);
 assert.equal(await page.locator('#map-over').isEnabled(),true);
 await page.locator('#map-clearBreakpoints').click();await complete(page);
 await page.locator('#map-over').click();await complete(page);
 assert.match(await page.locator('#map-runtime-result>strong').textContent(),/suspended/);
 await page.locator('#map-continue').click();await complete(page);
 assert.match(await page.locator('#map-runtime-result').textContent(),/error/);
 assert.ok(await page.locator('#map-runtime-result>button').filter({hasText:'log ·'}).count()>0);
 await saved(page);await page.reload();await page.locator('#map-panel').waitFor({state:'visible'});
 assert.match(await page.locator('#map-runtime-result').textContent(),/保存された実行結果 · VMは未ロード/);
 assert.equal(await page.locator('#map-continue').isDisabled(),true);
 // Cancelled compiler work cannot overwrite a newer linked artifact.
 await page.evaluate(()=>{document.querySelector('#map-minify').click();document.querySelector('#stop').click();document.querySelector('#map-link').click();});
 await complete(page);assert.deepEqual(await page.locator('#map-source option').allTextContents(),['main.lua']);
 // Compressed single-line execution must display many possible original ranges.
 await page.locator('#map-minify').click();await complete(page);await page.locator('#map-load').click();await complete(page);
 await page.locator('#map-tick').click();await complete(page);
 assert.match(await page.locator('#map-runtime-result').textContent(),/列不明.*複数の元位置/);
 if(name==='chromium'){
  await page.locator('#map-runtime-result').scrollIntoViewIfNeeded();await page.screenshot({path:join(evidence,'source-map-runtime.png'),fullPage:false,caret:'initial'});
  await page.setViewportSize({width:390,height:844});await page.locator('#map-panel').scrollIntoViewIfNeeded();
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,'map mobile overflow');
  await page.screenshot({path:join(evidence,'source-map-mobile.png'),fullPage:true,caret:'initial'});
  await page.setViewportSize({width:1440,height:1100});
 }
 if(name==='chromium'){
  // A real compiler-generated identity artifact larger than localStorage's usual
  // capacity. It is imported for inspection, never executed or used as an input recipe.
  const isolated=await browser.newContext({acceptDownloads:true});const largePage=await isolated.newPage();
  try{
   await largePage.goto(url);await largePage.locator('#run:not([disabled])').waitFor();
   // Deliberately incomplete imported map, not a claim about compiler output.
   // Regenerate its consistency digest and require the real SDK validator to accept it.
   const partial=await largePage.evaluate(async()=>{
    const api=await import(new URL('./engine/compiler/compiler.js',document.baseURI).href);await api.default();
    const r=api.compile('return 1',{sourceMap:true,targetSize:8192,zeroCostNewlines:false});if(r.code!=='return 1')throw new Error('unexpected fixture');
    const j=JSON.parse(r.map),x=j.x_storm;j.names=[];j.mappings='A,O,C';
    x.origins=[];x.reasons=[];x.relations=[];x.contexts=[];x.constructs=[];x.dispositions=[];
    x.mappings=[{start:0,end:8,origin:null,copied:null,inlineContexts:[]}];x.integrity='';
    const sorted=v=>Array.isArray(v)?v.map(sorted):v&&typeof v==='object'?Object.fromEntries(Object.keys(v).sort().map(k=>[k,sorted(v[k])])):v;
    x.integrity=Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',new TextEncoder().encode(JSON.stringify(sorted(j))))),b=>b.toString(16).padStart(2,'0')).join('');
    const map=JSON.stringify(j);api.validateSourceMap(r.code,map);return {kind:'storm-lua-artifact',version:1,code:r.code,map};
   });
   await largePage.locator('#map-import-file').setInputFiles({name:'partial-map.json',mimeType:'application/json',buffer:Buffer.from(JSON.stringify(partial))});
   await complete(largePage);await select(largePage,'generated','1');assert.match(await largePage.locator('#map-detail').textContent(),/Unknown.*由来未取得/);
   const largeArtifact=await largePage.evaluate(async()=>{
    const api=await import(new URL('./engine/compiler/compiler.js',document.baseURI).href);
    const source='-- '+ 'source map snapshot '.repeat(170000)+'\nfunction onTick()end';
    const result=api.compile(source,{sourceMap:true,sourceName:'large-identity.lua',targetSize:10000000});
    if(!result.ok||!result.map)throw new Error(JSON.stringify(result.diagnostics));
    return {kind:'storm-lua-artifact',version:1,code:result.code,map:result.map};
   });
   const encoded=JSON.stringify(largeArtifact);assert.ok(Buffer.byteLength(encoded)>6*1024*1024);
   await largePage.locator('#map-import-file').setInputFiles({name:'large-artifact.json',mimeType:'application/json',buffer:Buffer.from(encoded)});
   await complete(largePage);await saved(largePage);assert.equal(await largePage.locator('#map-generated').inputValue(),largeArtifact.code);
   await largePage.reload();await largePage.locator('#map-panel').waitFor({state:'visible'});await saved(largePage);
   assert.equal(await largePage.locator('#map-generated').inputValue(),largeArtifact.code);assert.equal(await largePage.locator('#map-tick').isDisabled(),true);
   assert.equal(await largePage.evaluate(()=>localStorage.getItem('storm-lua-playground-v1')),null);
   console.log('chromium: >6MiB validated source-map artifact survives IndexedDB reload without starting its VM');
  }finally{await isolated.close();}
 }
 assert.equal(await page.locator('vite-error-overlay').count(),0);
 console.log(`${name}: source maps, reasons, reverse selection, snapshots, portable workspace, pause/step/log/error and stale-artifact rejection passed`);
 return {bidirectional:true,inlineContexts:true,reasons:true,removedSource:true,workspaceRoundtrip:true,staleRejection:true,multiFile:true,runtimePause:true,runtimeError:true,lineOnlyAmbiguity:true};
}
