import{readFileSync,writeFileSync}from'node:fs';import{pathToFileURL}from'node:url';
const repo=process.argv[2];const{loadCompiler}=await import(pathToFileURL(`${repo}/packages/lua-engine/dist/compiler.js`));const{loadRuntime}=await import(pathToFileURL(`${repo}/packages/lua-engine/dist/index.js`));
const compiler=await loadCompiler({wasmBinary:new Uint8Array(readFileSync(`${repo}/packages/lua-engine/dist/compiler-wasm/compiler_bg.wasm`))});
const runtime=await loadRuntime({wasmBinary:new Uint8Array(readFileSync(`${repo}/packages/lua-engine/dist/wasm/storm_lua_wasm.wasm`))});
const cases=[
['byte-len','function onTick()output.setNumber(1,#"\\255")end'],
['unicode-len','function onTick()output.setNumber(1,#"\\u{1F600}")end'],
['long-crlf','function onTick()output.setNumber(1,#[[a\r\nb]])end'],
['line-comment-cr','-- comment\rfunction onTick()output.setNumber(1,42)end'],
['hex-wrap','function onTick()output.setNumber(1,0x10000000000000000)end'],
['negative-div','function onTick()output.setNumber(1,5//-2)output.setNumber(2,5%-2)end'],
['shift','function onTick()output.setNumber(1,1<<64)output.setNumber(2,4>>-1)end'],
['goto','function onTick()local x x=1 goto done x=2 ::done:: output.setNumber(1,x)end'],
['table-capture','local function f(t)local g=function()return t.x end return t.x+g()end function onTick()output.setNumber(1,f({x=input.getNumber(1)}))end'],
['root-globals','x=0 function onDraw()x=x+1 end local x=input.getNumber(1) function onTick()output.setNumber(1,x)end'],
['carrier-shadow','local tau=math.pi*2 function onTick()local tau=input.getNumber(1)output.setNumber(1,math.pi*2)output.setNumber(2,math.pi*2)output.setNumber(3,tau)end'],
['namespace-reject','local t={} function t.f()return 3 end function onTick()output.setNumber(1,t.f())end'],
];
function run(source){const vm=runtime.createVehicle({instructionBudget:100000});const results=[];try{vm.io.inputNumbers[0]=3;results.push({load:vm.load(source)});for(let i=0;i<2;i++){results.push({tick:vm.tick(),out:Array.from(vm.io.outputNumbers.subarray(0,4))});results.push({draw:vm.draw(32,32)});}return{ok:true,results};}catch(e){return{ok:false,results,error:String(e)}}finally{vm.dispose()}}
const results=[];
for(const[name,source]of cases){for(const opts of [{},{mode:'safe',numericMode:'exact'}]){for(const sourceMap of[false,true]){const options={...opts,sourceMap};const result=compiler.minify(source,options);const original=run(source);const generated=result.ok?run(result.code):null;results.push({name,source,options,compileOk:result.ok,code:result.code,diagnostics:result.diagnostics,original,generated,equal:JSON.stringify(original)===JSON.stringify(generated)});}}}
writeFileSync(process.argv[3],results.map(x=>JSON.stringify(x)).join('\n')+'\n');console.log(JSON.stringify({comparisons:results.length,originalErrors:results.filter(x=>!x.original.ok).length,differences:results.filter(x=>!x.equal).length,byCase:cases.map(([name])=>({name,differences:results.filter(x=>x.name===name&&!x.equal).length}))},null,2));
