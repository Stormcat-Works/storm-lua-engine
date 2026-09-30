import test from 'node:test';
import {RECIPES} from '../shared/recipes.js';
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
const cwd=fileURLToPath(new URL('../',import.meta.url));
function cli(args:string[],input?:string){return spawnSync(process.execPath,['--import','tsx','cli/index.ts',...args],{cwd,input,encoding:'utf8',timeout:20000});}
test('CLIの確認例・JSONL・明示エラー終了',()=>{
 const listed=cli(['--list']);assert.equal(listed.status,0,listed.stderr);assert.equal(JSON.parse(listed.stdout).length,RECIPES.length);
 const example=cli(['--recipe','compiler']);assert.equal(example.status,0,example.stderr);const result=JSON.parse(example.stdout);assert.ok(result.results.every((r:{ok:boolean})=>r.ok));
 const pipeline=cli(['--jsonl'],[
  {op:'createVehicle'},{op:'load',source:'function onTick()output.setNumber(1,input.getNumber(1)*2)end'},{op:'io',numbers:[6]},{op:'tick'},
 ].map(v=>JSON.stringify(v)).join('\n'));
 assert.equal(pipeline.status,0,pipeline.stderr);const lines=pipeline.stdout.trim().split('\n').map(line=>JSON.parse(line));assert.equal(lines[3].value.io.outputNumbers[0],12);
 const invalid=cli(['--jsonl'],'{"op":"does-not-exist"}\n');assert.equal(invalid.status,1);assert.equal(JSON.parse(invalid.stdout).ok,false);
 assert.equal(cli(['--unknown']).status,2);
});
