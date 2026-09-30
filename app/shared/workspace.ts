/** Portable Playground workspace. It contains saved reports, not a live VM. */
import {artifact,selection,type Artifact,type MapSelection} from './source-map.js';
import {object,text,integer} from './wire.js';
import {parseProject,type PlaygroundProject,type StepResult} from './project.js';
import type {MappedResult} from './mapped-execution.js';
export interface InspectionState {artifact:Artifact;selection:MapSelection|null;inputStamp:string;page:number;runtime:MappedResult|null;focused:number|null;inputs:{numbers:string;booleans:string;properties:string}}
export function inspectionState(value:unknown):InspectionState|null {
 if(value===null||value===undefined)return null;const r=object(value);
 const page=r['page'];if(typeof page!=='number'||!Number.isInteger(page)||page<0)throw new Error('不正な候補pageです');
 const focused=r['focused'];if(focused!==null&&(typeof focused!=='number'||!Number.isInteger(focused)||focused<0))throw new Error('不正な候補選択です');
 const inputs=object(r['inputs']);
 let runtime:MappedResult|null=null;
 if(r['runtime']!=null){
  const v=object(r['runtime']);
  if(v['kind']!=='mapped-runtime'||typeof v['generation']!=='number'||!Number.isInteger(v['generation'])||v['generation']<1||!Array.isArray(v['locations'])||!Array.isArray(v['stack'])||!Array.isArray(v['logs']))throw new Error('保存された実行結果が不正です');
  const io=object(v['io']);if(!Array.isArray(io['outputNumbers'])||!Array.isArray(io['outputBooleans']))throw new Error('保存I/Oが不正です');
  runtime={kind:'mapped-runtime',artifact:text(v['artifact']),chunk:text(v['chunk']),generation:v['generation'],outcome:text(v['outcome']),...(v['error']===undefined?{}:{error:text(v['error'])}),
   locations:v['locations'].map(value=>{const l=object(value);if(l['kind']!=='pause'&&l['kind']!=='log'&&l['kind']!=='error')throw new Error('保存位置の種類が不正です');if(typeof l['line']!=='number'||!Number.isInteger(l['line'])||l['line']<1)throw new Error('保存行が不正です');return {chunk:text(l['chunk']),line:l['line'],kind:l['kind'],association:null};}),
   stack:v['stack'],logs:v['logs'],io:io as unknown as MappedResult['io']};
 }
 // Runtime reports are display-only on restore; they never recreate a live VM.
 return {focused:focused as number|null,inputs:{numbers:text(inputs['numbers']),booleans:text(inputs['booleans']),properties:text(inputs['properties'])},artifact:artifact(r['artifact']),selection:r['selection']==null?null:selection(r['selection']),inputStamp:text(r['inputStamp']),page,runtime};
}

export interface Workspace {kind:'storm-lua-playground-workspace';version:1;project:PlaygroundProject;inspection:InspectionState|null;history:StepResult[];lastFrame:unknown}
export function historyRecords(value:unknown):StepResult[]{
 if(!Array.isArray(value)||value.length>64)throw new Error('結果履歴は64件以内です');
 return value.map(item=>{const r=object(item);const index=integer(r['index'],0,255),op=text(r['op']);
  if(typeof r['ok']!=='boolean'||(r['expectedError']!==undefined&&typeof r['expectedError']!=='boolean'))throw new Error('結果履歴の状態が不正です');
  return {index,op,ok:r['ok'],...(r['value']===undefined?{}:{value:r['value']}),...(r['error']===undefined?{}:{error:text(r['error'])}),...(r['expectedError']===undefined?{}:{expectedError:r['expectedError']})};
 });
}
export function savedFrame(value:unknown):unknown {
 if(value===undefined||value===null)return undefined;
 const r=object(value);if(r['frame']!==undefined)return {frame:savedFrame(r['frame'])};
 if(r['kind']!=='frame')throw new Error('保存frameが不正です');
 const width=integer(r['width'],1,2048),height=integer(r['height'],1,2048),pixels=r['pixels'];
 if(!(pixels instanceof Uint8Array)||pixels.length!==width*height*4)throw new Error('保存frameの長さが不正です');
 return {kind:'frame',width,height,format:text(r['format']),pixels};
}
export function parseWorkspace(value:unknown):Workspace {
 const raw=object(value);
 if(raw['kind']==='storm-lua-playground')return {kind:'storm-lua-playground-workspace',version:1,project:parseProject(raw),inspection:null,history:[],lastFrame:undefined};
 if(raw['kind']!=='storm-lua-playground-workspace'||raw['version']!==1)throw new Error('未対応のworkspace形式です');
 return {kind:'storm-lua-playground-workspace',version:1,project:parseProject(raw['project']),inspection:inspectionState(raw['inspection']),history:historyRecords(raw['history']),lastFrame:savedFrame(raw['lastFrame'])};
}
