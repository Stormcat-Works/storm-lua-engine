/** Explicit artifact-bound VM, shared by CLI and browser Worker. */
import type {LuaEngine,VehicleVm,StepMode,LogRecord,Properties} from '@stormcat-works/storm-lua-engine';
import {MapDocument,type Artifact,type ValidateMap} from './source-map.js';
import {errorMessage,object,integer} from './wire.js';

export interface RuntimeLocation {chunk:string;line:number;kind:'pause'|'error'|'log';association:ReturnType<MapDocument['runtimeLine']>}
export interface MappedResult {
 kind:'mapped-runtime';artifact:string;chunk:string;generation:number;outcome:string;error?:string;
 locations:RuntimeLocation[];stack:unknown[];logs:LogRecord[];io:{outputNumbers:number[];outputBooleans:boolean[]};
 frame?:{kind:'frame';width:number;height:number;format:string;pixels:Uint8Array};
}
export class MappedExecution {
 #vm:VehicleVm|undefined;#document:MapDocument|undefined;#chunk='';#generation=0;#logs:LogRecord[]=[];#outcome='unloaded';
 constructor(private readonly engine:()=>Promise<LuaEngine>,private readonly validate:ValidateMap){}
 async load(value:Artifact,properties:Properties={}):Promise<MappedResult>{
  const doc=await MapDocument.open(value,this.validate);
  const environment=doc.data.compilation['environment'];
  if(environment!=='game'&&environment!=='extended')throw new Error('成果物に実行環境が記録されていません');
  const bindings=doc.data.compilation['hostBindings'];
  if(!Array.isArray(bindings)||bindings.length)throw new Error('ホストbindingを持つ成果物は、この生成物VMでは実行しません。SDK操作列で対応するホストを明示してください');
  const engine=await this.engine();
  this.dispose();this.#document=doc;this.#generation++;this.#chunk=`@playground-map-${this.#generation}.lua`;this.#logs=[];
  this.#vm=engine.createVehicle({environment,properties,instructionBudget:1_000_000,onLog:record=>{this.#logs.push(record);if(this.#logs.length>256)this.#logs.shift();}});
  return this.perform(()=>this.#vm!.load(doc.artifact.code,this.#chunk));
 }
 private current(identity:string):{vm:VehicleVm;doc:MapDocument}{
  if(!this.#vm||!this.#document)throw new Error('生成物をVMへロードしてください');
  if(identity!==this.#document.data.integrity)throw new Error('実行中の生成物と選択中のマップが一致しません。再ロードしてください');
  return {vm:this.#vm,doc:this.#document};
 }
 operate(identity:string,action:string,options:Record<string,unknown>={}):MappedResult {
  const {vm,doc}=this.current(identity);
  switch(action){
   case 'tick':return this.perform(()=>vm.tick());
   case 'draw':return this.perform(()=>vm.draw(64,32),true);
   case 'continue':case 'into':case 'over':case 'out':return this.perform(()=>vm.resume(action as StepMode));
   case 'breakpoints':{
    const source=integer(options['source'],0,doc.sources.length-1),start=integer(options['start'],0,0xffffffff),end=integer(options['end']??start,0,0xffffffff);
    const lines=doc.breakpointLines(source,start,end);
    if(!lines.length)throw new Error('選択した原文には生成コード上の候補がありません。削除済みの位置や近隣行には設定しません');
    vm.setBreakpoints(lines.map(line=>({source:this.#chunk,line})));return this.snapshot('breakpoints-set');
   }
   case 'clearBreakpoints':vm.setBreakpoints([]);return this.snapshot('breakpoints-cleared');
   case 'input':{
    const numbers=options['numbers'],bools=options['booleans'];
    if(!Array.isArray(numbers)||numbers.length>32||numbers.some(n=>typeof n!=='number'||!Number.isFinite(n)))throw new Error('入力Numberは有限数32個以内です');
    if(!Array.isArray(bools)||bools.length>32||bools.some(n=>typeof n!=='boolean'))throw new Error('入力Booleanは32個以内です');
    vm.io.inputNumbers.fill(0);vm.io.inputNumbers.set(numbers);vm.io.inputBooleans.fill(0);vm.io.inputBooleans.set(bools.map(Boolean).map(Number));
    return this.snapshot('input-updated');
   }
   case 'inspect':return this.snapshot(this.#outcome);
   default:throw new Error(`未対応の生成物実行操作: ${action}`);
  }
 }
 private perform(action:()=>string,draw=false):MappedResult{
  let error:string|undefined;
  try{this.#outcome=action();}catch(e){error=errorMessage(e);this.#outcome='error';}
  const result=this.snapshot(this.#outcome,error);
  if(draw&&this.#outcome==='completed'){
   const f=this.#vm!.frame();result.frame={kind:'frame',width:f.width,height:f.height,format:f.format,pixels:f.copy()};
  }
  return result;
 }
 private snapshot(outcome:string,error?:string):MappedResult {
  const vm=this.#vm!,doc=this.#document!;
  const stack=this.#outcome==='suspended'?vm.stack():[];
  const locations:RuntimeLocation[]=[];
  const add=(chunk:string,line:number,kind:RuntimeLocation['kind'])=>{
   // Matching a known loaded chunk is mandatory. Foreign frames remain raw.
   locations.push({chunk,line,kind,association:chunk===this.#chunk?doc.runtimeLine(line):null});
  };
  for(const f of stack)if(f.line>0)add(f.source,f.line,'pause');
  if(error){
   const name=this.#chunk.slice(1);const escaped=name.replace(/[.*+?^${}()|[\]\\]/g,'\\$&');
   const match=new RegExp(`(?:^|[\\s\\"\\[])${escaped}:(\\d+):`).exec(error);
   if(match)add(this.#chunk,Number(match[1]),'error');
  }
  for(const record of this.#logs)if(record.location)add(record.location.chunk,record.location.line,'log');
  return {kind:'mapped-runtime',artifact:doc.data.integrity,chunk:this.#chunk,generation:this.#generation,outcome,
   ...(error===undefined?{}:{error}),locations,stack,logs:this.#logs.slice(),io:{outputNumbers:Array.from(vm.io.outputNumbers),outputBooleans:Array.from(vm.io.outputBooleans,Boolean)}};
 }
 dispose():void{this.#vm?.dispose();this.#vm=undefined;this.#document=undefined;this.#outcome='unloaded';}
}
