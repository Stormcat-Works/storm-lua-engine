import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {loadCompiler} from '../../dist/compiler.js';
import {loadRuntime} from '../../dist/index.js';
const compiler=await loadCompiler({wasmBinary:await readFile(new URL('../../dist/compiler-wasm/compiler_bg.wasm',import.meta.url))});
const engine=await loadRuntime({wasmBinary:await readFile(new URL('../../dist/wasm/storm_lua_wasm.wasm',import.meta.url))});
test('source inspection separates literals, comments and unexecuted control flow',()=>{
 const source='local s="-- >>> fake"\n-- real\ndo\n sim.group("名",{collapsed=true})\n sim.labelNumberInput(1,"名","Speed",{min=1e3,max=2e3})\nend\nif false then sim.setProperty("no",99)end';
 const r=compiler.inspectSource(source);assert.equal(r.ok,true);assert.equal(r.comments.length,1);
 assert.equal(r.statements[1].kind,'do');assert.equal(r.statements[2].kind,'other');
 const call=r.statements[1].body[1];assert.equal(call.function,'sim.labelNumberInput');
 const encoded=new TextEncoder().encode(source);assert.equal(new TextDecoder().decode(encoded.slice(call.start,call.end)),'sim.labelNumberInput(1,"名","Speed",{min=1e3,max=2e3})');
 const options=call.arguments[3];assert.equal(options.kind,'table');assert.equal(options.entries[0][1].bits,'408f400000000000');
 assert.equal(compiler.inspectSource('local =').ok,false);
});
test('LB build removes development blocks before loading game code and returns no minify map',()=>{
 const project={entry:'main',modules:{main:'---@section __LB_SIMULATOR_ONLY__\nprint("dev")\n---@endsection\nlocal x=require("lib")\nfunction onTick()output.setNumber(1,answer);output.setBool(1,x==nil)end',lib:'answer=7;return {bad=true}'}};
 for(const minify of [false,true]){
  const build=compiler.buildLifeboat(project,{environment:'game',minify,mode:'safe'});
  assert.equal(build.ok,true,JSON.stringify(build));assert.equal(Boolean(build.map),!minify);
  const vm=engine.createVehicle();try{vm.load(build.code,'@built.lua');vm.tick();assert.equal(vm.io.outputNumbers[0],7);assert.equal(vm.io.outputBooleans[0],1);}finally{vm.dispose();}
 }
 const raw=project.modules.main;assert.equal(compiler.stripDevelopment(raw).split('\n').length,raw.split('\n').length);
 assert.throws(()=>compiler.stripDevelopment('---@section missing\n'));
});
