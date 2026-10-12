import {readFileSync,writeFileSync} from 'node:fs';
import {pathToFileURL} from 'node:url';
const repo=process.argv[2];
const {loadCompiler}=await import(pathToFileURL(`${repo}/packages/lua-engine/dist/compiler.js`));
const compiler=await loadCompiler({wasmBinary:new Uint8Array(readFileSync(`${repo}/packages/lua-engine/dist/compiler-wasm/compiler_bg.wasm`))});
const cases=[
['byte-escape','return "\\xFF"',{}],
['unicode-escape','return "\\u{1F600}"',{}],
['long-crlf','return [[a\r\nb]]',{}],
['line-comment-cr','-- comment\rreturn 42',{environment:'extended'}],
['hex-dot','return 0x1.',{mode:'safe',numericMode:'exact'}],
['hex-wrap','return 0x10000000000000000',{mode:'safe',numericMode:'exact'}],
['inf-precedence','return 2/1e999',{mode:'safe',numericMode:'exact'}],
['negative-idiv','return 5//-2',{}],
['negative-mod','return 5%-2',{}],
['shift64','return 1<<64',{}],
['shift-negative','return 4>>-1',{}],
['goto','function onTick()local x x=1 goto done x=2 ::done:: output.setNumber(1,x)end',{}],
['table-capture','local function f(t)local g=function()return t.x end return t.x+g()end function onTick()output.setNumber(1,f({x=input.getNumber(1)}))end',{}],
];
const out=[];
for(const [name,source,options] of cases){
 for(const sourceMap of [false,true]){
  try { const result=compiler.minify(source,{...options,sourceMap});
   if(result.map)compiler.validateSourceMap(result.code,result.map);
   out.push({name,source,options:{...options,sourceMap},ok:result.ok,code:result.code,diagnostics:result.diagnostics,mapProduced:!!result.map});
  }catch(e){out.push({name,source,options:{...options,sourceMap},error:String(e)})}
 }
}
const source='--[[stormmin-metadata\nprivate development note\n]]\nfunction onTick()output.setNumber(1,3)end';
for(const kind of ['build','buildLifeboat']){
 const result=compiler[kind]({entry:'main',modules:{main:source}},{metadataTag:'stormmin-metadata',minify:false,sourceMap:true});
 if(result.map)compiler.validateSourceMap(result.code,result.map);
 out.push({name:'fixed-metadata-'+kind,ok:result.ok,code:result.code,mapProduced:!!result.map,diagnostics:result.diagnostics});
}
for(const [name,source] of [['integer-div-overflow','return 0x8000000000000000//-1'],['after-panic','return 1+2']]){
 try{const result=compiler.minify(source);out.push({name,ok:result.ok,code:result.code});}
 catch(e){out.push({name,error:String(e)})}
}
writeFileSync(process.argv[3],out.map(x=>JSON.stringify(x)).join('\n')+'\n');
console.log(JSON.stringify({cases:out.length,errors:out.filter(x=>x.error),compileFailures:out.filter(x=>x.ok===false)},null,2));
