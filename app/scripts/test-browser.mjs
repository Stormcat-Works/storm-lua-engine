/** Browser plugin not available: Playwrightで配布物の実操作を検証します。 */
import {createServer} from 'node:http';
import {readFile,mkdir,writeFile} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import {join,resolve,extname,sep} from 'node:path';
import assert from 'node:assert/strict';
import {chromium,firefox,webkit} from 'playwright';
import {sourceMapBrowser} from './source-map-browser.mjs';
const app=fileURLToPath(new URL('../',import.meta.url)),root=join(app,'dist-site');
const evidence=process.env.PLAYGROUND_EVIDENCE_DIR??'/tmp/storm-lua-playground-evidence';await mkdir(evidence,{recursive:true});
const csp="default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self'; style-src 'self'; img-src 'self' data: blob:; connect-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'";
const server=createServer(async(req,res)=>{try{
 const pathname=decodeURIComponent(new URL(req.url,'http://local').pathname);const path=resolve(root,'.'+(pathname.endsWith('/')?pathname+'index.html':pathname));
 if(!path.startsWith(root+sep)){res.writeHead(403).end();return;}
 const bytes=await readFile(path);res.setHeader('Content-Type',({'.html':'text/html; charset=utf-8','.js':'text/javascript','.css':'text/css','.json':'application/json','.wasm':'application/wasm','.txt':'text/plain; charset=utf-8'})[extname(path)]??'application/octet-stream');res.setHeader('Content-Security-Policy',csp);res.setHeader('X-Content-Type-Options','nosniff');res.end(bytes);
 }catch{res.writeHead(404).end();}});
await new Promise((ok,fail)=>{server.once('error',fail);server.listen(0,'127.0.0.1',ok);});
const url=`http://127.0.0.1:${server.address().port}/tools/stormworks/storm-lua-engine/`;
const receipts=[];
try{
 for(const [name,launcher] of Object.entries({chromium,firefox,webkit})){
  const browser=await launcher.launch({headless:true});
  try{
   const context=await browser.newContext({viewport:{width:1440,height:1100},acceptDownloads:true});const page=await context.newPage();page.setDefaultTimeout(20000);
   let phase='initial';const errors=[];const requests=[];page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(`${phase}: ${m.text()} ${JSON.stringify(m.location())}`);});page.on('request',r=>requests.push(r.url()));
   await page.goto(url);await page.locator('[data-recipe="vehicle"]').waitFor();assert.equal(await page.title(),'Storm Lua Engine: Playground');
   assert.equal(requests.some(u=>u.endsWith('.wasm')),false,'initial UI must not initialize WASM');
   const ids=await page.locator('[data-recipe]').evaluateAll(nodes=>nodes.map(e=>e.dataset.recipe));assert.equal(ids.length,16);
   for(const id of ids){
    await page.locator(`[data-recipe="${id}"]`).click();await page.locator('#run').click();
    await page.waitForFunction(()=>['complete','error'].includes(document.body.dataset.runState));
    assert.equal(await page.locator('body').getAttribute('data-run-state'),'complete',`${name}/${id}: ${await page.locator('#error-banner').textContent()}`);
    if(id==='source-loading'){
     assert.ok((await page.locator('#results').textContent()).includes('hostCalls'));
     if(name==='chromium')await page.screenshot({path:join(evidence,'source-loading-desktop.png'),fullPage:true,caret:'initial'});
    }
    if(id==='vehicle'){
     assert.equal(await page.locator('#screen').getAttribute('width'),'64');assert.equal(await page.locator('#screen').getAttribute('height'),'32');
     const opaque=await page.locator('#screen').evaluate(canvas=>Array.from(canvas.getContext('2d').getImageData(0,0,64,32).data).some(v=>v>0));assert.equal(opaque,true);
     // WebKit's Playwright screenshot preparation injects inline body{} CSS; keep strict CSP and use Chromium for visual evidence.
     phase='screenshot-desktop';if(name==='chromium')await page.screenshot({path:join(evidence,`${name}-desktop.png`),fullPage:true,caret:'initial'});
    }
   }
   const sourceMaps=await sourceMapBrowser({page,browser,name,url,evidence});
   await page.locator('[data-recipe="reflection"]').click();await page.locator('#run').click();await page.waitForFunction(()=>document.body.dataset.runState==='complete');
   await page.waitForFunction(()=>document.querySelector('#saved').textContent==='この端末に保存済み');const savedSource=await page.locator('#source').inputValue();const savedCount=await page.locator('.result-row').count();await page.reload();
   await page.locator('[data-recipe="reflection"][aria-current="true"]').waitFor();assert.equal(await page.locator('#source').inputValue(),savedSource);assert.equal(await page.locator('.result-row').count(),savedCount);assert.equal(await page.locator('body').getAttribute('data-run-state'),'restored');
   phase='invalid-import';await page.locator('#import-file').setInputFiles({name:'bad.json',mimeType:'application/json',buffer:Buffer.from(JSON.stringify({kind:'storm-lua-playground',version:999}))});
   await page.locator('#error-banner').waitFor({state:'visible'});assert.equal(await page.locator('#source').inputValue(),savedSource);
   const downloadPromise=page.waitForEvent('download');await page.locator('#export').click();const download=await downloadPromise;const exported=JSON.parse(await readFile(await download.path(),'utf8'));assert.equal(exported.project.source,savedSource);assert.equal(exported.version,1);assert.equal(exported.kind,'storm-lua-playground-workspace');
   phase='valid-import';await page.locator('#import-file').setInputFiles({name:'case.json',mimeType:'application/json',buffer:Buffer.from(JSON.stringify(exported))});await page.waitForFunction(()=>document.querySelector('#error-banner').hidden);
   // Cancel during Worker initialization, then immediately start a new run.
   await page.locator('[data-recipe="vehicle"]').click();
   await page.evaluate(()=>{document.querySelector('#run').click();document.querySelector('#stop').click();document.querySelector('#run').click();});
   await page.waitForFunction(()=>['complete','error'].includes(document.body.dataset.runState));
   assert.equal(await page.locator('body').getAttribute('data-run-state'),'complete',`cancelled initializer must not overwrite the next run: ${await page.locator('#error-banner').textContent()}`);
   // Explicit step and cancelling a CPU-bound VM do not freeze the UI.
   await page.locator('[data-recipe="vehicle"]').click();await page.locator('#step').click();await page.waitForFunction(()=>document.body.dataset.runState==='paused');assert.equal(await page.locator('.result-row').count(),1);
   await page.locator('#steps-tab').click();await page.locator('#steps').fill(JSON.stringify([{op:'createVehicle',options:{instructionBudget:1000000000}},{op:'load',source:'while true do end'}]));await page.locator('#run').click();
   await page.waitForFunction(()=>document.querySelector('#progress').textContent.startsWith('2 /'));await page.locator('#stop').click();await page.waitForFunction(()=>document.body.dataset.runState==='cancelled');
   await page.locator('[data-recipe="vehicle"]').click();await page.locator('#run').click();await page.waitForFunction(()=>document.body.dataset.runState==='complete');
   await page.setViewportSize({width:390,height:844});await page.locator('#source-tab').click();
   assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth),true,'mobile overflow');phase='screenshot-mobile';if(name==='chromium')await page.screenshot({path:join(evidence,`${name}-mobile.png`),fullPage:true,caret:'initial'});
   // A fresh context demonstrates raster-only and compiler-only fetching.
   for(const [id,allowed] of [['raster','screen.wasm'],['reflection',null]]){
    const isolated=await browser.newContext();const p=await isolated.newPage();const wasm=[];p.on('request',r=>{if(r.url().endsWith('.wasm'))wasm.push(r.url());});await p.goto(url);await p.locator(`[data-recipe="${id}"]`).click();
    if(id==='reflection'){
     await p.locator('#steps-tab').click();await p.locator('#steps').fill(JSON.stringify([{op:'minify',source:'function onTick()output.setNumber(1,1+2)end'}]));
    }
    await p.locator('#run').click();await p.waitForFunction(()=>['complete','error'].includes(document.body.dataset.runState));assert.equal(await p.locator('body').getAttribute('data-run-state'),'complete');
    assert.ok(wasm.length>0);assert.ok(wasm.every(u=>allowed?u.endsWith(allowed):u.includes('compiler_bg.wasm')),`${id} fetched unexpected ${wasm}`);await isolated.close();
   }
   // A malformed local selection is recoverable without overwriting its original record.
   const corrupt=JSON.stringify({kind:'storm-lua-playground-state',version:1,selected:'missing-recipe',title:'recover me',source:'return 7',steps:'[]',environment:'game'});
   const recoveryContext=await browser.newContext({storageState:{cookies:[],origins:[{origin:new URL(url).origin,localStorage:[{name:'storm-lua-playground-v1',value:corrupt}]}]}});
   const recovery=await recoveryContext.newPage();await recovery.goto(url);await recovery.locator('#recover').waitFor({state:'visible'});
   assert.equal(await recovery.evaluate(()=>localStorage.getItem('storm-lua-playground-v1')),corrupt);
   assert.ok((await recovery.locator('#source').inputValue()).includes('onTick'));
   await recovery.locator('#import-file').setInputFiles({name:'valid.json',mimeType:'application/json',buffer:Buffer.from(JSON.stringify(exported))});
   await recovery.waitForFunction(()=>document.querySelector('#error-banner').hidden);
   assert.equal(await recovery.locator('#source').inputValue(),exported.project.source);await recoveryContext.close();
   assert.deepEqual(errors,[],`${name} console/page errors`);receipts.push({browser:name,recipes:ids.length,desktop:[1440,1100],mobile:[390,844],sourceMaps,reload:true,invalidImportPreserved:true,cancel:true,compilerOnly:true,rasterOnly:true,errors});
   console.log(`${name}: ${ids.length} recipes, reload/import/export, step/cancel, mobile and independent WASM loading passed`);await context.close();
  }finally{await browser.close();}
 }
 await writeFile(join(evidence,'browser-results.json'),JSON.stringify({route:'/tools/stormworks/storm-lua-engine/',browserPlugin:'absent',receipts},null,2)+'\n');
}finally{await new Promise(resolve=>server.close(resolve));}
