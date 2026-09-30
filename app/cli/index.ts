#!/usr/bin/env node
/** Engine自身のCLI。Storm Minのコマンドやフロントエンドは置き換えません。 */
import {readFile,writeFile} from 'node:fs/promises';
import {createInterface} from 'node:readline';
import {PlaygroundSession,OPERATIONS,type Command} from '../shared/session.js';
import {parseProject,StepRunner} from '../shared/project.js';
import {parseWorkspace} from '../shared/workspace.js';
import {artifact} from '../shared/source-map.js';
import {RECIPES,recipe} from '../shared/recipes.js';
import {decodeWire,object,stringify,errorMessage} from '../shared/wire.js';
import {nodeInit} from './init.js';

const args=process.argv.slice(2);
async function main():Promise<void> {
 if(args.length===0||args[0]==='--help'){
  console.log('Storm Lua Engine: Playground\n\n--list                    確認例と機能一覧\n--recipe ID               確認例を実行\n--project FILE            version付き確認プロジェクトを実行\n--export-recipe ID FILE   確認例をJSONへ保存\n--jsonl                   stdinのSDK操作JSONを同じセッションで順次実行\nminify FILE [--extended] [--source-map]  ソースを短縮してJSON結果を返す\nmap-inspect FILE BYTE     生成物JSONの指定byteを検査（Luaは実行しない）\nrun FILE [--extended]     原文をVehicleへloadし1tick実行\n\nLua/SDKはローカルで実行します。ネットワーク要求は送信しません。');return;
 }
 if(args[0]==='--list'){console.log(stringify(RECIPES.map(({id,title,features})=>({id,title,features}))));return;}
 if(args[0]==='--export-recipe'){
  if(args.length!==3)throw new Error('--export-recipe ID FILE');
  await writeFile(args[2]!,stringify(parseProject(recipe(args[1]!)))+'\n');return;
 }
 const session=new PlaygroundSession(await nodeInit());
 try {
  if(args[0]==='--jsonl'){
   if(args.length!==1)throw new Error('--jsonlに追加引数は不要です');
   const input=createInterface({input:process.stdin,crlfDelay:Infinity});
   for await(const line of input){if(!line.trim())continue;try{
    const request=decodeWire(JSON.parse(line));const command=object(request) as Command;
    console.log(stringify({ok:true,value:await session.execute(command)},0));
   }catch(error){console.log(stringify({ok:false,error:errorMessage(error)},0));process.exitCode=1;}}
   return;
  }
  if(args[0]==='map-inspect'){
   if(args.length!==3)throw new Error('map-inspect ARTIFACT_FILE GENERATED_BYTE');
   const value=artifact(JSON.parse(await readFile(args[1]!,'utf8'))),position=Number(args[2]);
   console.log(stringify(await session.execute({op:'inspectMap',artifact:value,start:position})));return;
  }
  if(args[0]==='minify'||args[0]==='run'){
   if(args.length<2||args.slice(2).some(a=>!['--extended',...(args[0]==='minify'?['--source-map']:[])].includes(a))||new Set(args.slice(2)).size!==args.slice(2).length)throw new Error('minify FILE [--extended] [--source-map] / run FILE [--extended]');
   const source=await readFile(args[1]!,'utf8');const environment=args.includes('--extended')?'extended':'game';
   if(args[0]==='minify'){
    const result=await session.execute({op:'minify',source,options:{environment,sourceMap:args.includes('--source-map'),sourceName:'input.lua'}});console.log(stringify(result));
    if(object(result)['ok']!==true)process.exitCode=1;
   }else{
    await session.execute({op:'createVehicle',options:{environment}});await session.execute({op:'load',source});console.log(stringify(await session.execute({op:'tick'})));
   }
   return;
  }
  if(!['--recipe','--project'].includes(args[0]!)||args.length!==2)throw new Error(`不明な引数です。--helpまたは--listを参照してください。操作: ${OPERATIONS.join(', ')}`);
  const workspace=parseWorkspace(args[0]==='--recipe'?recipe(args[1]!):decodeWire(JSON.parse(await readFile(args[1]!,'utf8'))));
  if(workspace.inspection){
   const s=workspace.inspection.selection;
   const inspected=object(await session.execute({op:'inspectMap',artifact:workspace.inspection.artifact,...(s??{side:'generated',start:0})}));
   if(workspace.inspection.runtime&&workspace.inspection.runtime.artifact!==inspected['identity'])throw new Error('保存された実行結果と生成物が一致しません');
  }
  const project=workspace.project;
  const runner=new StepRunner(project,command=>session.execute(command));
  const results=await runner.all();
  console.log(stringify({title:project.title,environment:project.environment,results}));
  if(results.some(r=>!r.ok))process.exitCode=1;
 }finally{session.dispose();}
}
try{await main();}catch(error){console.error(errorMessage(error));process.exitCode=2;}
