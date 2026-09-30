/** CLI/Worker共通のSDK操作。ゲーム世界・最適化処理・描画規則は実装しません。 */
import {
  loadRuntime, VEHICLE_API_CATALOG, ADDON_API_CATALOG, ADDON_EVENTS, ENVIRONMENT_CATALOG,
  bindingPaths, luaTable, luaText, luaField, encodeSavedata, decodeSavedata,
  type LogRecord, type LuaEngine, type VehicleVm, type AddonVm, type LuaValue, type ScriptOptions,
  type VehicleOptions, type AddonOptions, type HostBindings, type ServerFunctions,
  type DebugHandle, type StepMode, type Breakpoint, type AddonEvent, type Properties,
} from '@stormcat-works/storm-lua-engine';
import {loadRaster, encodeCommands, type RasterEngine, type Raster} from '@stormcat-works/storm-lua-engine/raster';
import type {DrawCommand} from '@stormcat-works/storm-lua-engine/commands';
import {loadCompiler, type Compiler, type CompilerInitOptions} from '@stormcat-works/storm-lua-engine/compiler';
import type {RuntimeInitOptions} from '@stormcat-works/storm-lua-engine';
import type {CompileOptions, ProjectCompileOptions, AnalyzeOptions, LuaProject} from '@stormcat-works/storm-lua-engine/compiler';
import {object, text, integer} from './wire.js';
import {MapDocument,artifact} from './source-map.js';
import {MappedExecution} from './mapped-execution.js';

export interface SessionInit {
  runtime: RuntimeInitOptions;
  compiler: CompilerInitOptions;
  raster: {wasmUrl?: string | URL; wasmBinary?: Uint8Array};
}
export type Command = Record<string, unknown> & {op: string};
export const OPERATIONS = [
  'inspectMap','mappedLoad','mappedAction','buildLifeboat','catalog','minify','build','analyze','scanProperties','passIds','passMetadata',
  'createVehicle','createAddon','load','tick','draw','frame','io','properties','reset','dispose',
  'start','dispatch','savedata','reload','menuProperties','destroy',
  'logs','drainLogRecords','drainLogs','flushLogs','hostCalls','httpRequests','httpReply','cancelHttp','enableLogs',
  'breakpoints','resume','stack','locals','upvalues','expandTable','watch',
  'createRaster','render','rasterFrame','disposeRaster','encodeCommands',
  'encodeSavedata','decodeSavedata','luaText','luaField','bindingPaths',
] as const;
export type Vm = VehicleVm | AddonVm;

function values(value: unknown): unknown[] {
  if (!Array.isArray(value)) throw new TypeError('配列が必要です');
  return value;
}
/** 手入力のrecordはSDKのluaTableへ渡します。SDKの型付きLuaTableは保持します。 */
function luaValue(value: unknown, depth = 0): LuaValue {
  if (depth > 32) throw new RangeError('Lua値の入れ子が深すぎます');
  if (value === null || typeof value === 'boolean' || typeof value === 'number' || typeof value === 'bigint' || typeof value === 'string' || value instanceof Uint8Array) return value;
  const record = object(value);
  if (record['kind'] === 'table') {
    return {kind:'table',entries:values(record['entries']).map(entry=>{
      const pair=values(entry);if(pair.length!==2)throw new TypeError('table entryはkey/valueの2要素です');
      return [luaValue(pair[0],depth+1),luaValue(pair[1],depth+1)] as const;
    })};
  }
  return luaTable(Object.fromEntries(Object.entries(record).map(([key,v])=>[key,luaValue(v,depth+1)])));
}

function tableValue(input:unknown):ReturnType<typeof luaTable> {
 const value=luaValue(input);
 if(value===null||typeof value!=='object'||!('kind' in value)||value.kind!=='table')throw new TypeError('Luaのテーブルが必要です');
 return value;
}
function bytesValue(input:unknown):Uint8Array {if(!(input instanceof Uint8Array))throw new TypeError('バイト列が必要です');return input;}

export class PlaygroundSession {
  #runtime?: Promise<LuaEngine>;
  #compiler?: Promise<Compiler>;
  #raster?: Promise<RasterEngine>;
  readonly #vms = new Map<string, Vm>();
  readonly #rasters = new Map<string, Raster>();
  readonly #logs: {id:string;source:string;bytes:Uint8Array}[]=[];
  readonly #calls: {name:string;args:unknown[]}[]=[];
  #disposed=false;
  #mapped?:MappedExecution;
  constructor(private readonly init:SessionInit) {}
  private runtime():Promise<LuaEngine> { return this.#runtime ??= loadRuntime(this.init.runtime); }
  private compiler():Promise<Compiler> { return this.#compiler ??= loadCompiler(this.init.compiler); }
  private renderer():Promise<RasterEngine> { return this.#raster ??= loadRaster(this.init.raster); }
  private id(command:Command):string { return text(command['id'] ?? 'main','id'); }
  private vm(command:Command):Vm {
    const vm=this.#vms.get(this.id(command));if(!vm)throw new Error('指定したVMは作成されていません');return vm;
  }
  private vehicle(command:Command):VehicleVm {
    const vm=this.vm(command);if(vm.mode!=='vehicle')throw new Error('Vehicle専用の操作です');return vm;
  }
  private addon(command:Command):AddonVm {
    const vm=this.vm(command);if(vm.mode!=='addon')throw new Error('Addon専用の操作です');return vm;
  }
  private raster(command:Command):Raster {
    const r=this.#rasters.get(this.id(command));if(!r)throw new Error('指定したrasterは作成されていません');return r;
  }
  private functions(input:unknown):ServerFunctions {
    return Object.fromEntries(Object.entries(object(input??{})).map(([name,recipe])=>{
      const definition=object(recipe);
      if(!['return','echo','sum','fail'].includes(text(definition['operation']??'return')))throw new Error(`未知のテストホスト定義: ${name}`);
      return [name,(...args:LuaValue[]):readonly LuaValue[]=>{
        if(this.#calls.length>=256)this.#calls.shift();this.#calls.push({name,args});
        switch(definition['operation']??'return') {
          case 'echo':return args;
          case 'sum': {
            if(args.every(v=>typeof v==='bigint'))return [(args as bigint[]).reduce((a,b)=>a+b,0n)];
            if(!args.every(v=>typeof v==='number'||typeof v==='bigint'))throw new TypeError('sumテストホストは数値のみです');
            return [args.reduce<number>((a,b)=>a+Number(b),0)];
          }
          case 'fail':throw new Error(text(definition['message']??'明示的なテストホストの失敗'));
          default:return values(definition['returns']??[]).map(v=>luaValue(v));
        }
      }];
    }));
  }
  private bindings(input:unknown):HostBindings {
    const config=object(input??{});
    return {
      values:Object.fromEntries(Object.entries(object(config['values']??{})).map(([k,v])=>[k,luaValue(v)])),
      functions:this.functions(config['functions']),
    };
  }
  private options(command:Command):ScriptOptions {
    const options=object(command['options']??{});
    const id=this.id(command);
    const modules=command['modules']===undefined?undefined:object(command['modules']);
    const requireOptions=modules===undefined?{}:{requireLoader:(name:string)=>{
      if(!Object.hasOwn(modules,name))throw new Error(`module not found: ${name}`);
      const chunk=object(modules[name]);
      const source=chunk['source'];
      if(typeof source!=='string'&&!(source instanceof Uint8Array))throw new TypeError('module source must be text or bytes');
      if(this.#calls.length>=256)this.#calls.shift();this.#calls.push({name:'requireLoader',args:[name]});
      return {source,name:text(chunk['name'])};
    }};
    const onLog=(record:LogRecord)=>{
      if(this.#logs.length>=256)this.#logs.shift();this.#logs.push({id,source:record.source,bytes:record.bytes,...(record.location?{location:record.location}:{})});
    };
    return {...options,...requireOptions,bindings:this.bindings(options['bindings']),...(command['manualLogs']===true?{}:{onLog})} as ScriptOptions;
  }
  private frameCopy(frame:{width:number;height:number;strideBytes:number;format:string;copy():Uint8Array}):unknown {
    return {kind:'frame',width:frame.width,height:frame.height,strideBytes:frame.strideBytes,format:frame.format,pixels:frame.copy()};
  }
  async execute(command:Command):Promise<unknown> {
    if(this.#disposed)throw new Error('セッションは破棄済みです');
    const id=this.id(command);
    switch(command.op) {
      case 'buildLifeboat':return (await this.compiler()).buildLifeboat(object(command['project']) as unknown as LuaProject,object(command['options']??{}) as ProjectCompileOptions);
      case 'inspectMap':{
        const a=artifact(command['artifact']);const api=await this.compiler();const doc=await MapDocument.open(a,(code,map)=>api.validateSourceMap(code,map));
        const side=command['side']??'generated';if(side!=='original'&&side!=='generated')throw new Error('不正な検査面です');
        const start=integer(command['start']??0,0,0xffffffff),end=integer(command['end']??start,0,0xffffffff);
        return {kind:'map-inspection',producer:doc.data.producer,identity:doc.data.integrity,hits:command['side']==='original'?doc.originalHits(integer(command['source']??0,0,doc.sources.length-1),start,end):doc.generatedHits(start,end)};
      }
      case 'mappedLoad':{
        const api=await this.compiler();this.#mapped??=new MappedExecution(()=>this.runtime(),(code,map)=>api.validateSourceMap(code,map));
        return this.#mapped.load(artifact(command['artifact']),object(command['properties']??{}) as Properties);
      }
      case 'mappedAction':{
        if(!this.#mapped)throw new Error('生成物VMは未ロードです');
        return this.#mapped.operate(text(command['identity']),text(command['action']),object(command['options']??{}));
      }
      case 'catalog':return {environment:ENVIRONMENT_CATALOG,vehicle:VEHICLE_API_CATALOG,addon:ADDON_API_CATALOG,events:ADDON_EVENTS,operations:OPERATIONS};
      case 'minify':return (await this.compiler()).minify(text(command['source']),object(command['options']??{}) as CompileOptions);
      case 'build':return (await this.compiler()).build(object(command['project']) as unknown as LuaProject,object(command['options']??{}) as ProjectCompileOptions);
      case 'analyze':return (await this.compiler()).analyze(object(command['project']) as unknown as LuaProject,object(command['options']??{}) as AnalyzeOptions);
      case 'scanProperties':return (await this.compiler()).scanProperties(text(command['source']));
      case 'passIds':return (await this.compiler()).passIds();
      case 'passMetadata':return (await this.compiler()).passMetadata();
      case 'createVehicle': {
        if(this.#vms.has(id))throw new Error('同じidのVMがあります。先にdisposeしてください');
        if(this.#vms.size>=16)throw new Error('同時VM数は16までです');
        const options=this.options(command) as VehicleOptions;
        const map=command['mapFixture'];
        const provider=map===undefined?{}:{mapProvider:(request:Parameters<NonNullable<VehicleOptions['mapProvider']>>[0])=>{
          const color=values(object(map)['rgba']).map(v=>integer(v,0,255));if(color.length!==4)throw new Error('mapFixtureはRGBA4要素です');
          this.#calls.push({name:'mapFixture',args:[request]});if(this.#calls.length>256)this.#calls.shift();
          const pixels=new Uint8Array(request.width*request.height*4);for(let i=0;i<pixels.length;i+=4)pixels.set(color,i);return pixels;
        }};
        const vm=(await this.runtime()).createVehicle({...options,...provider});this.#vms.set(id,vm);return {id,mode:vm.mode,environment:options.environment??'game'};
      }
      case 'createAddon': {
        if(this.#vms.has(id))throw new Error('同じidのVMがあります。先にdisposeしてください');
        if(this.#vms.size>=16)throw new Error('同時VM数は16までです');
        const raw=object(command['options']??{});
        const savedata=raw['savedata']===undefined?{}:{savedata:luaValue(raw['savedata'])};
        const options={...this.options(command),...raw,...savedata,bindings:this.bindings(raw['bindings']),server:this.functions(command['server'])} as AddonOptions;
        const vm=(await this.runtime()).createAddon(options);this.#vms.set(id,vm);return {id,mode:vm.mode,environment:options.environment??'game'};
      }
      case 'load':return this.vm(command).load(text(command['source']),text(command['name']??'=playground'));
      case 'tick': {
        const vm=this.vm(command);let outcome:unknown='missing';
        const count=integer(command['count']??1,1,4096);
        for(let i=0;i<count;i++){outcome=vm.mode==='vehicle'?vm.tick():vm.tick(integer(command['gameTicks']??1,1,4294967295));if(outcome==='suspended')break;}
        return {outcome,...(vm.mode==='vehicle'?{io:this.io(vm)}:{})};
      }
      case 'io': {
        const vm=this.vehicle(command), io=vm.io;
        const numbers=command['numbers']===undefined?undefined:values(command['numbers']);
        const booleans=command['booleans']===undefined?undefined:values(command['booleans']);
        if(numbers&&(numbers.length>32||numbers.some(v=>typeof v!=='number')))throw new TypeError('Number入力は32chまでの数値配列です');
        if(booleans&&(booleans.length>32||booleans.some(v=>typeof v!=='boolean')))throw new TypeError('Boolean入力は32chまでの真偽値配列です');
        if(numbers)io.inputNumbers.set(numbers as number[]);
        if(booleans)io.inputBooleans.set(booleans.map(v=>v?1:0));
        return this.io(vm);
      }
      case 'properties':this.vehicle(command).setProperties(object(command['properties']) as Properties);return 'properties updated';
      case 'reset':this.vehicle(command).reset();return 'reset';
      case 'draw': {
        const vm=this.vehicle(command),outcome=vm.draw(integer(command['width']??64,1,2048),integer(command['height']??32,1,2048));
        return outcome==='suspended'?{outcome}:{outcome,frame:this.frameCopy(vm.frame())};
      }
      case 'frame':return this.frameCopy(this.vehicle(command).frame());
      case 'dispose':this.vm(command).dispose();this.#vms.delete(id);return 'disposed';
      case 'start':return this.addon(command).start();
      case 'dispatch':return this.addon(command).dispatch(text(command['callback']) as AddonEvent,values(command['args']??[]).map(v=>luaValue(v)));
      case 'savedata':return this.addon(command).savedata();
      case 'reload':return this.addon(command).reload(tableValue(command['value']));
      case 'menuProperties':return this.addon(command).menuProperties();
      case 'destroy':return this.addon(command).destroy();
      case 'logs':return this.#logs.filter(r=>r.id===id);
      case 'drainLogRecords':return this.vm(command).drainLogRecords();
      case 'drainLogs':return this.vm(command).drainLogs();
      case 'flushLogs': {const records:LogRecord[]=[];const count=this.vm(command).flushLogs(record=>records.push(record));return {count,records};}
      case 'hostCalls':return this.#calls.slice();
      case 'httpRequests':return this.vm(command).drainHttpRequests();
      case 'httpReply':return this.vm(command).httpReply(object(command['token']) as unknown as Parameters<Vm['httpReply']>[0],command['reply'] instanceof Uint8Array?command['reply']:text(command['reply']));
      case 'cancelHttp':this.vm(command).cancelHttp(object(command['token']) as unknown as Parameters<Vm['cancelHttp']>[0]);return 'cancelled';
      case 'enableLogs':this.vm(command).enableLogs();return 'extended logging enabled';
      case 'breakpoints':this.vm(command).setBreakpoints(values(command['points']) as Breakpoint[]);return 'breakpoints set';
      case 'resume':return this.vm(command).resume(text(command['mode']??'continue') as StepMode);
      case 'stack':return this.vm(command).stack();
      case 'locals':return this.vm(command).locals(integer(command['level']??0));
      case 'upvalues':return this.vm(command).upvalues(integer(command['level']??0));
      case 'expandTable':return this.vm(command).expandTable(object(command['handle']) as unknown as DebugHandle,integer(command['start']??0),integer(command['limit']??64,1,256));
      case 'watch':return this.vm(command).evaluateWatch(text(command['expression']),integer(command['level']??0));
      case 'createRaster': {
        if(this.#rasters.has(id))throw new Error('同じidのrasterがあります');
        if(this.#rasters.size>=16)throw new Error('同時raster数は16までです');
        this.#rasters.set(id,(await this.renderer()).createRaster(integer(command['width']??64,1,2048),integer(command['height']??32,1,2048)));return {id};
      }
      case 'render':return this.frameCopy(this.raster(command).render(command['commands'] instanceof Uint8Array?command['commands']:values(command['commands']) as DrawCommand[]));
      case 'rasterFrame':return this.frameCopy(this.raster(command).frame());
      case 'disposeRaster':this.raster(command).dispose();this.#rasters.delete(id);return 'disposed';
      case 'encodeCommands':return encodeCommands(values(command['commands']) as DrawCommand[]);
      case 'encodeSavedata':return encodeSavedata(tableValue(command['value']));
      case 'decodeSavedata':return decodeSavedata(bytesValue(command['value']));
      case 'luaText':return luaText(luaValue(command['value']));
      case 'luaField':return luaField(tableValue(command['value']),text(command['key']));
      case 'bindingPaths':return bindingPaths(this.bindings(command['bindings']));
      default:throw new Error(`未対応の操作: ${command.op}`);
    }
  }
  private io(vm:VehicleVm):unknown { const io=vm.io;return {inputNumbers:Array.from(io.inputNumbers),inputBooleans:Array.from(io.inputBooleans,Boolean),outputNumbers:Array.from(io.outputNumbers),outputBooleans:Array.from(io.outputBooleans,Boolean)}; }
  dispose():void {
    if(this.#disposed)return;
    const errors:unknown[]=[];
    try{this.#mapped?.dispose();}catch(e){errors.push(e);}
    for(const vm of this.#vms.values())try{vm.dispose();}catch(e){errors.push(e);}
    for(const raster of this.#rasters.values())try{raster.dispose();}catch(e){errors.push(e);}
    this.#vms.clear();this.#rasters.clear();this.#disposed=true;
    if(errors.length)throw new AggregateError(errors,'SDKセッションの破棄に失敗しました');
  }
}
