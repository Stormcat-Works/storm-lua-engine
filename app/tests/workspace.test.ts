import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,writeFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {parseWorkspace,inspectionState,historyRecords,savedFrame} from '../shared/workspace.js';
import {recipe} from '../shared/recipes.js';
import {parseProject} from '../shared/project.js';
import {stringify,decodeWire} from '../shared/wire.js';
const cwd=fileURLToPath(new URL('../',import.meta.url));
function cli(args:string[]){return spawnSync(process.execPath,['--import','tsx','cli/index.ts',...args],{cwd,encoding:'utf8',timeout:30000});}

test('workspace validation retains reports, typed bytes and exact input project',()=>{
 const project=parseProject(recipe('source-map'));
 const state={kind:'storm-lua-playground-workspace',version:1,project,inspection:null,history:[{index:0,op:'logs',ok:true,value:new Uint8Array([0,255])}],lastFrame:{kind:'frame',width:1,height:1,format:'rgba8',pixels:new Uint8Array([1,2,3,255])}};
 const parsed=parseWorkspace(decodeWire(JSON.parse(stringify(state))));assert.deepEqual(parsed,state);
 assert.deepEqual(parseWorkspace(project).project,project);assert.equal(parseWorkspace(project).inspection,null);
 for(const patch of [{version:99},{history:[{index:-1,op:'load',ok:true}]},{lastFrame:{kind:'frame',width:1,height:1,format:'rgba8',pixels:new Uint8Array([0])}},{inspection:{artifact:{kind:'unknown',version:1},page:0}}]){
  assert.throws(()=>parseWorkspace({...state,...patch}));
 }
});
test('malformed selection, history and frame metadata are never silently accepted',()=>{
 assert.throws(()=>historyRecords([{index:0,op:'tick',ok:'yes'}]));assert.throws(()=>savedFrame({kind:'frame',width:99999,height:1,pixels:[]}));
 assert.throws(()=>inspectionState({artifact:{kind:'storm-lua-artifact',version:1,code:'',map:'{}'},page:-1,selection:null}));
 assert.throws(()=>parseWorkspace({kind:'storm-lua-playground-workspace',version:2}));
});
test('CLI emits explanation maps, inspects artifact bytes and validates workspace before executing',async()=>{
 const dir=await mkdtemp(join(tmpdir(),'storm-p5-cli-'));
 try{
  const source=join(dir,'input.lua');await writeFile(source,'function onTick()output.setNumber(1,2*3)end');
  const result=cli(['minify',source,'--source-map']);assert.equal(result.status,0,result.stderr);const compiled=JSON.parse(result.stdout);
  assert.equal(compiled.ok,true);assert.equal(typeof compiled.map,'string');assert.equal(JSON.parse(compiled.map).x_storm.schemaVersion,1);
  const bundle={kind:'storm-lua-artifact',version:1,code:compiled.code,map:compiled.map},artifactFile=join(dir,'artifact.json');
  await writeFile(artifactFile,JSON.stringify(bundle));const inspect=cli(['map-inspect',artifactFile,String(compiled.code.indexOf('6'))]);
  assert.equal(inspect.status,0,inspect.stderr);const inspected=JSON.parse(inspect.stdout);assert.equal(inspected.kind,'map-inspection');assert.ok(inspected.hits.length>0);
  const project=parseProject(recipe('catalog'));
  const workspace={kind:'storm-lua-playground-workspace',version:1,project,history:[],lastFrame:null,inspection:{artifact:bundle,selection:null,page:0,focused:null,inputStamp:'imported',inputs:{numbers:'[]',booleans:'[]',properties:'{}'},runtime:null}};
  const file=join(dir,'workspace.json');await writeFile(file,JSON.stringify(workspace));const loaded=cli(['--project',file]);assert.equal(loaded.status,0,loaded.stderr);assert.equal(JSON.parse(loaded.stdout).results[0].op,'catalog');
  await writeFile(artifactFile,JSON.stringify({...bundle,code:bundle.code.replace('6','8')}));assert.notEqual(cli(['map-inspect',artifactFile,'0']).status,0);
 }finally{await rm(dir,{recursive:true,force:true});}
});
