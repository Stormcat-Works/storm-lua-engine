/** Queries over SDK-validated provenance; no Lua parsing or origin reconstruction. */
import type {OptimizationMap,OptimizationMapping,OriginalSpan} from '@stormcat-works/storm-lua-engine/compiler';
import {object,text,integer} from './wire.js';

export interface Artifact {kind:'storm-lua-artifact';version:1;code:string;map:string}
export type ValidateMap=(code:string,map:string)=>OptimizationMap|Promise<OptimizationMap>;
export interface MapSelection {side:'generated'|'original';source:number;start:number;end:number}
export interface MapHit {start:number;end:number;origin:number|null;via:string;original:OriginalSpan|null}
export function artifact(input:unknown):Artifact {
 const v=object(input);
 if(v['kind']!=='storm-lua-artifact'||v['version']!==1)throw new Error('未対応の生成物形式です');
 return {kind:'storm-lua-artifact',version:1,code:text(v['code']),map:text(v['map'])};
}
export function selection(input:unknown):MapSelection {
 const v=object(input);if(v['side']!=='generated'&&v['side']!=='original')throw new Error('不正な選択面です');
 return {side:v['side'],source:integer(v['source'],0,0xffffffff),start:integer(v['start'],0,0xffffffff),end:integer(v['end'],0,0xffffffff)};
}
/** A textarea normalizes CRLF/CR to LF. This index keeps original snapshot bytes. */
export class DisplayText {
 readonly value:string;
 readonly #bytes:number[]=[0];
 readonly #positions=new Map<number,number>([[0,0]]);
 readonly #lineStarts:number[]=[0];
 constructor(readonly raw:string){
  let displayed='',byte=0;
  for(let i=0;i<raw.length;){
   const cp=raw.codePointAt(i)!;const ch=String.fromCodePoint(cp);const crlf=ch==='\r'&&raw[i+1]==='\n';
   const out=ch==='\r'?'\n':ch;
   if(ch.length===2)this.#bytes.push(-1);
   byte+=new TextEncoder().encode(ch).length+(crlf?1:0);displayed+=out;
   this.#bytes.push(byte);this.#positions.set(byte,displayed.length);
   if(out==='\n')this.#lineStarts.push(byte);
   i+=ch.length+(crlf?1:0);
  }
  this.value=displayed;
 }
 get bytes():number{return this.#bytes.at(-1)!;}
 byteAt(utf16:number):number {const b=this.#bytes[utf16];if(b===undefined||b<0)throw new RangeError('選択が文字境界ではありません');return b;}
 uiAt(byte:number,end=false):number {
  if(!Number.isInteger(byte)||byte<0||byte>this.bytes)throw new RangeError('元範囲がsnapshotの外です');
  const exact=this.#positions.get(byte);if(exact!==undefined)return exact;
  // A CRLF interior has no visible textarea coordinate. Expand only that newline.
  const before=this.#positions.get(byte-1),after=this.#positions.get(byte+1);
  if(before!==undefined&&after!==undefined&&this.raw[this.rawOffset(byte-1)]==='\r')return end?after:before;
  throw new RangeError('UTF-8の途中を選択できません');
 }
 private rawOffset(byte:number):number {return new TextDecoder('utf-8',{fatal:true}).decode(new TextEncoder().encode(this.raw).subarray(0,byte)).length;}
 slice(start:number,end:number):string {return new TextDecoder('utf-8',{fatal:true}).decode(new TextEncoder().encode(this.raw).subarray(start,end));}
 range(start:number,end:number):void {if(start>end)throw new RangeError('逆順の選択範囲です');this.uiAt(start);this.uiAt(end,true);}
 position(byte:number):{line:number;column:number}{const ui=this.uiAt(byte);const prefix=this.value.slice(0,ui);return {line:prefix.split('\n').length,column:ui-(prefix.lastIndexOf('\n')+1)+1};}
 line(line:number):{start:number;end:number}|null {
  if(!Number.isInteger(line)||line<1)return null;
  const start=this.#lineStarts[line-1];if(start===undefined)return null;
  return {start,end:this.#lineStarts[line]??this.bytes};
 }
}
function overlaps(start:number,end:number,a:number,b:number):boolean {return start===end?a<=start&&start<b:a<end&&start<b;}
function intersection(span:OriginalSpan,start:number,end:number):OriginalSpan {return {...span,start:Math.max(span.start,start),end:Math.min(span.end,end)};}
export class MapDocument {
 readonly generated:DisplayText;
 readonly sources:readonly {name:string;coordinates:DisplayText}[];
 private constructor(readonly artifact:Artifact,readonly data:OptimizationMap,sources:{name:string;coordinates:DisplayText}[]){this.generated=new DisplayText(artifact.code);this.sources=sources;}
 static async open(value:Artifact,validate:ValidateMap):Promise<MapDocument>{
  const data=await validate(value.code,value.map);
  const json=object(JSON.parse(value.map));const names=json['sources'],contents=json['sourcesContent'];
  if(!Array.isArray(names)||!Array.isArray(contents)||names.length!==data.sources.length||contents.length!==names.length)throw new Error('snapshot一覧が不正です');
  return new MapDocument(value,data,names.map((name,i)=>({name:text(name),coordinates:new DisplayText(text(contents[i]))})));
 }
 validateSelection(s:MapSelection):void {if(!this.sources[s.source])throw new RangeError('選択sourceがありません');(s.side==='generated'?this.generated:this.sources[s.source]!.coordinates).range(s.start,s.end);}
 generatedHits(start:number,end=start):MapHit[]{
  this.generated.range(start,end);
  return this.data.mappings.filter(m=>overlaps(start,end,m.start,m.end)).map(m=>{
   if(m.copied){
    // Only a validated equal-byte copy permits interpolation. Preserve a caret
    // as a point, and restrict a range/line query to its actual overlap.
    const a=Math.max(start,m.start),b=start===end?a:Math.min(end,m.end);
    return {start:a,end:b,origin:m.origin,via:'copy',original:{source:m.copied.source,start:m.copied.start+a-m.start,end:m.copied.start+b-m.start}};
   }
   return {start:m.start,end:m.end,origin:m.origin,via:'primary',original:m.origin===null?null:this.data.origins[m.origin]!.primary};
  });
 }
 originalHits(source:number,start:number,end=start):MapHit[]{
  const buffer=this.sources[source];if(!buffer)throw new RangeError('sourceがありません');buffer.coordinates.range(start,end);
  const found:MapHit[]=[];const seen=new Set<string>();
  const add=(m:OptimizationMapping,span:OriginalSpan,via:string,copy=false)=>{
   if(span.source!==source||!overlaps(start,end,span.start,span.end))return;
   const range=copy?intersection(span,start,end):span;
   const a=copy?m.start+range.start-span.start:m.start,b=copy?m.start+range.end-span.start:m.end;
   const key=`${a}:${b}:${m.origin}:${via}:${span.start}:${span.end}`;
   if(!seen.has(key)){seen.add(key);found.push({start:a,end:b,origin:m.origin,via,original:range});}
  };
  for(const m of this.data.mappings){
   const origin=m.origin===null?null:this.data.origins[m.origin]!;
   if(m.copied)add(m,m.copied,'copy',true);
   else if(origin?.primary&&origin.precision!=='group')add(m,origin.primary,'primary');
   if(origin)for(const id of origin.related){const r=this.data.relations[id]!;add(m,r.span,r.role);}
   for(const id of m.inlineContexts){const c=this.data.contexts[id]!;add(m,c.callSite,'inlineCall');}
  }
  // The flat map can assign function punctuation to a wide function range.
  // Do not let that enclosing range masquerade as an executable copy of every
  // source line it contains. Prefer strict subranges and explicit dispositions.
  const anchors=[...found.flatMap(h=>h.original?[h.original]:[]),...this.dispositions(source,start,end).map(d=>d.original)].sort((a,b)=>a.start-b.start||a.end-b.end);
  const minimumAtStart=new Map<number,number>();
  for(const a of anchors)minimumAtStart.set(a.start,Math.min(a.end,minimumAtStart.get(a.start)??Infinity));
  const suffixEnd=new Array<number>(anchors.length+1).fill(Infinity);
  for(let i=anchors.length-1;i>=0;i--)suffixEnd[i]=Math.min(anchors[i]!.end,suffixEnd[i+1]!);
  return found.filter(h=>{
   const a=h.original;if(!a)return true;
   if((minimumAtStart.get(a.start)??Infinity)<a.end)return false;
   let low=0,high=anchors.length;
   while(low<high){const mid=(low+high)>>>1;if(anchors[mid]!.start<=a.start)low=mid+1;else high=mid;}
   return suffixEnd[low]!>a.end;
  });
 }
 dispositions(source:number,start:number,end=start){return this.data.dispositions.filter(d=>d.original.source===source&&overlaps(start,end,d.original.start,d.original.end));}
 /** Line-only runtime reports remain many candidates; no column zero is invented. */
 runtimeLine(line:number):{line:number;precision:'line-only';ambiguous:boolean;hits:MapHit[];synthetic:boolean;unknown:boolean}|null {
  const range=this.generated.line(line);if(!range)return null;
  const hits=this.generatedHits(range.start,range.end);const lines=new Set<string>();
  for(const h of hits)if(h.original){const pos=this.sources[h.original.source]!.coordinates.position(h.original.start);lines.add(`${h.original.source}:${pos.line}`);}
  return {line,precision:'line-only',ambiguous:lines.size>1,hits,synthetic:hits.some(h=>h.origin!==null&&this.data.origins[h.origin]!.kind==='synthetic'),unknown:hits.some(h=>h.origin===null)};
 }
 /** Static candidate lines, not proof that the VM has an executable instruction. */
 breakpointLines(source:number,start:number,end=start):number[]{
  return [...new Set(this.originalHits(source,start,end).flatMap(h=>{
   const ui=this.generated.uiAt(h.start),endUi=this.generated.uiAt(h.end,true);
   const width=this.generated.value.codePointAt(ui)!>0xffff?2:1;
   const actual=this.generated.value.slice(ui,endUi===ui?ui+width:endUi);if(!actual.trim())return [];
   let lastUi=endUi===ui?ui:endUi-1;
   if(lastUi>0&&/[\uDC00-\uDFFF]/.test(this.generated.value[lastUi]??''))lastUi--;
   const first=this.generated.position(h.start).line,last=this.generated.position(this.generated.byteAt(lastUi)).line;
   return Array.from({length:last-first+1},(_,i)=>first+i);
  }))].sort((a,b)=>a-b);
 }
}
