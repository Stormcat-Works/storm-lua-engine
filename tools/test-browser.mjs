/** 実ブラウザ + モジュールWorkerの検証。一時HTTPサーバーは必ずクローズされます。 */
import {createRequire} from 'node:module';
import {createServer} from 'node:http';
import {readFile,mkdir} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {fileURLToPath} from 'node:url';
import {resolve,sep,extname,join} from 'node:path';
import assert from 'node:assert/strict';
const require=createRequire(new URL('../packages/lua-engine/package.json',import.meta.url));
const playwright=require('playwright');
const root=fileURLToPath(new URL('../',import.meta.url));
const server=createServer(async(req,res)=>{
  try{
    let pathname=decodeURIComponent(new URL(req.url,'http://local').pathname);
    if(pathname.endsWith('/'))pathname+='index.html';
    const path=resolve(root,'.'+pathname);
    if(!path.startsWith(root)||pathname.split('/').some(p=>p.startsWith('.'))){res.writeHead(403).end();return;}
    const content=await readFile(path);
    res.setHeader('Content-Type',({'.html':'text/html','.js':'text/javascript','.mjs':'text/javascript','.json':'application/json','.wasm':'application/wasm'})[extname(path)]??'application/octet-stream');
    res.end(content);
  }catch{res.writeHead(404).end();}
});
await new Promise((ok,error)=>{server.once('error',error);server.listen(0,'127.0.0.1',ok);});
const url=`http://127.0.0.1:${server.address().port}`;
const results=[];
const evidence=process.env.STORM_BROWSER_EVIDENCE_DIR??join(tmpdir(),'storm-lua-sdk-browser');
await mkdir(evidence,{recursive:true});
try{
  for(const name of ['chromium','firefox','webkit']){
    let browser;
    try{
      browser=await playwright[name].launch({headless:true,timeout:30000});
      const page=await browser.newPage();page.setDefaultTimeout(30000);
      const errors=[];page.on('pageerror',e=>errors.push(String(e)));
      await page.goto(url+'/examples/browser/');
      assert.equal(await page.title(),'Lua engine — headless embedding example');
      assert.equal(await page.locator('#run').count(),1);
      // Capture the first painted viewport before testing user interaction.
      await page.screenshot({path:join(evidence,`${name}-initial.png`)});
      await page.click('#run');await page.waitForFunction(()=>document.querySelector('#status').textContent.startsWith('tick:'));
      const result=await page.evaluate(async()=>{
        const assert=(v,m)=>{if(!v)throw new Error(m);};
        const api=await import('/packages/lua-engine/dist/index.js');
        const {loadRuntime}=api;
        const {hostSmoke}=await import('/tools/host-smoke.mjs');
        const {loadRaster}=await import('/packages/lua-engine/dist/raster.js');
        const {convert,luaSource}=await import('/tools/fixture-commands.mjs');
        const fixture=await (await fetch('/fixtures/screen/cases-v1.json')).json();
        const raster=await loadRaster();
        for(const c of fixture.cases){const r=raster.createRaster(c.width,c.height);try{
          const actual=r.render(c.ops.map(convert)).pixels;let offset=0;
          for(const [n,...rgba] of c.expectedRgbaRle)for(let k=0;k<n;k++)for(const component of rgba){assert(actual[offset++]===component,`raster mismatch ${c.id}`);}
          assert(offset===actual.length,'RLE extent');
        }finally{r.dispose();}}
        const engine=await loadRuntime();
        for(const c of fixture.cases){const vehicle=engine.createVehicle();try{
          vehicle.load(luaSource(c.ops),'=screen-contract');vehicle.draw(c.width,c.height);
          const actual=vehicle.frame().pixels;let offset=0;
          for(const [n,...rgba] of c.expectedRgbaRle)for(let k=0;k<n;k++)for(const component of rgba){assert(actual[offset++]===component,`Lua raster mismatch ${c.id}`);}
          assert(offset===actual.length,'Lua RLE extent');
        }finally{vehicle.dispose();}}
        const vm=engine.createVehicle({properties:{Gain:16777217}});
        try{
          vm.setBreakpoints([{source:'=browser',line:3}]);
          assert(vm.load('local gain=property.getNumber("Gain")\nlocal big=9223372036854775807\nlocal x=gain-16777216\nfunction onTick() output.setNumber(1,x) end','=browser')==='suspended','pause');
          assert(vm.evaluateWatch('big').value===9223372036854775807n,'i64');
          vm.setBreakpoints([]);assert(vm.resume()==='completed','resume');vm.tick();assert(vm.io.outputNumbers[0]===1,'f64');
        }finally{vm.dispose();}
        const runWorker=(id,input)=>new Promise((ok,fail)=>{
          const worker=new Worker('/examples/browser/runtime-worker.js',{type:'module'});
          const timer=setTimeout(()=>{worker.terminate();fail(new Error('worker timeout'));},30000);
          worker.onerror=e=>{clearTimeout(timer);worker.terminate();fail(new Error(e.message));};
          worker.onmessage=({data})=>{clearTimeout(timer);worker.terminate();data.error?fail(new Error(data.error)):ok(data);};
          worker.postMessage({id,source:'function onTick() output.setNumber(1,input.getNumber(1)*2) end function onDraw() screen.setColor(25,50,75) screen.drawClear() end',inputs:[input],width:8,height:8});
        });
        const workers=await Promise.all([runWorker(1,3),runWorker(2,7)]);
        assert(workers[0].outputs[0]===6&&workers[1].outputs[0]===14,'independent worker states');
        assert(workers.every(w=>w.pixels.length===256&&w.pixels[0]===25),'transferred pixels');
        return {rgbaCases:fixture.cases.length,luaCases:fixture.cases.length,workers:workers.length,debugger:true,canvasExample:true,...hostSmoke(engine,api)};
      });
      await page.screenshot({path:join(evidence,`${name}-result.png`)});
      assert.deepEqual(errors,[]);results.push({browser:name,version:browser.version(),...result});
      console.log(JSON.stringify(results.at(-1)));
    }finally{await browser?.close();}
  }
}finally{await new Promise(resolve=>server.close(resolve));}
console.log(`Browser conformance passed on ${results.length} engines; HTTP server and workers closed.`);
