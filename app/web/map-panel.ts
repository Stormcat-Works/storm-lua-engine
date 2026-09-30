/** Playground consumer UI. All sources and explanations are rendered as text. */
import {MapDocument,type MapSelection,type MapHit,type ValidateMap} from '../shared/source-map.js';
import {object,text,stringify,errorMessage} from '../shared/wire.js';
import type {MappedResult} from '../shared/mapped-execution.js';
import type {InspectionState} from '../shared/workspace.js';
import type {OriginalSpan,OptimizationReason} from '@stormcat-works/storm-lua-engine/compiler';

function el<T extends HTMLElement>(id:string):T{const node=document.getElementById(id);if(!node)throw new Error(`map UIがありません: ${id}`);return node as T;}
function node<K extends keyof HTMLElementTagNameMap>(tag:K,value:string):HTMLElementTagNameMap[K]{const n=document.createElement(tag);n.textContent=value;return n;}
export class MapPanel {
 #doc:MapDocument|undefined;#selection:MapSelection|null=null;#hits:MapHit[]=[];#page=0;#loaded=false;#paused=false;#runtime:MappedResult|null=null;#stamp='';#epoch=0;#busy=false;#focused:number|null=null;
 constructor(private readonly validate:ValidateMap,private readonly changed:()=>void,private readonly run:(action:string,options:Record<string,unknown>)=>Promise<unknown>){
  for(const side of ['original','generated'] as const){
   const area=el<HTMLTextAreaElement>(`map-${side}`);
   const select=()=>{try{this.readSelection(side);}catch(e){this.message(errorMessage(e));}};
   area.addEventListener('mouseup',select);area.addEventListener('keyup',select);
   el(`map-select-${side}`).addEventListener('click',select);
  }
  el<HTMLSelectElement>('map-source').addEventListener('change',()=>{
   const i=Number(el<HTMLSelectElement>('map-source').value);this.showSource(i);this.choose({side:'original',source:i,start:0,end:0});
  });
  for(const direction of [-1,1])el(direction<0?'map-prev':'map-next').addEventListener('click',()=>{this.#page+=direction;this.renderCandidates();this.changed();});
  for(const name of ['numbers','booleans','properties'])el(`map-${name}`).addEventListener('input',()=>this.changed());
  el('map-export').addEventListener('click',()=>{if(this.#doc)this.download('storm-lua-artifact.json',JSON.stringify(this.#doc.artifact));});
  el('map-download-code').addEventListener('click',()=>{if(this.#doc)this.download('generated.lua',this.#doc.artifact.code,'text/plain');});
  el('map-download-map').addEventListener('click',()=>{if(this.#doc)this.download('generated.lua.map',this.#doc.artifact.map);});
  for(const action of ['load','tick','draw','continue','into','over','out','clearBreakpoints','breakpoints','input']){
   el(`map-${action}`).addEventListener('click',()=>{void this.runtimeAction(action);});
  }
 }
 get document():MapDocument|undefined{return this.#doc;}
 state():InspectionState|null{return this.#doc?{artifact:this.#doc.artifact,selection:this.#selection,inputStamp:this.#stamp,page:this.#page,runtime:this.#runtime,focused:this.#focused,inputs:{numbers:el<HTMLInputElement>('map-numbers').value,booleans:el<HTMLInputElement>('map-booleans').value,properties:el<HTMLTextAreaElement>('map-properties').value}}:null;}
 async prepare(state:InspectionState):Promise<MapDocument>{const doc=await MapDocument.open(state.artifact,this.validate);if(state.selection)doc.validateSelection(state.selection);
 if(state.focused!==null){
  const s=state.selection;if(!s)throw new Error('候補選択に対応する範囲がありません');
  const hits=s.side==='generated'?doc.generatedHits(s.start,s.end):doc.originalHits(s.source,s.start,s.end);
  if(state.focused>=hits.length)throw new Error('保存された候補選択が範囲外です');
 }
 if(state.runtime){if(state.runtime.artifact!==doc.data.integrity)throw new Error('保存した実行結果と生成物が一致しません');for(const l of state.runtime.locations)l.association=l.chunk===state.runtime.chunk?doc.runtimeLine(l.line):null;}
 return doc;}
 async accept(code:string,map:string,inputStamp:string):Promise<void>{const epoch=++this.#epoch;const doc=await MapDocument.open({kind:'storm-lua-artifact',version:1,code,map},this.validate);if(epoch!==this.#epoch)return;this.install(doc,{artifact:doc.artifact,selection:null,inputStamp,page:0,runtime:null,focused:null,inputs:{numbers:'[3]',booleans:'[]',properties:'{}'}});}
 install(doc:MapDocument,state:InspectionState):void {
  this.invalidateRuntime();this.#doc=doc;this.#stamp=state.inputStamp;this.#selection=state.selection;this.#page=state.page;this.#runtime=state.runtime;this.#focused=state.focused;
  for(const name of ['numbers','booleans','properties'] as const)el<HTMLInputElement|HTMLTextAreaElement>(`map-${name}`).value=state.inputs[name];
  el('map-panel').hidden=false;el<HTMLTextAreaElement>('map-generated').value=doc.generated.value;
  const source=el<HTMLSelectElement>('map-source');source.replaceChildren(...doc.sources.map((s,i)=>{const o=node('option',s.name);o.value=String(i);return o;}));
  const index=state.selection?.source??0;if(doc.sources.length)this.showSource(index);
  const unknown=doc.data.mappings.reduce((sum,m)=>sum+(m.origin===null?m.end-m.start:0),0);
  el('map-meta').textContent=`Engine ${doc.data.producer.version} · schema ${doc.data.schemaVersion} · ${doc.data.generated.bytes.toLocaleString()} bytes · Unknown ${unknown.toLocaleString()} bytes · ${doc.data.integrity.slice(0,12)}`;
  el('map-contract').textContent=stringify({producer:doc.data.producer,compilation:doc.data.compilation,generated:doc.data.generated,sources:doc.sources.map((source,i)=>({name:source.name,...doc.data.sources[i]})),integrity:doc.data.integrity});
  this.renderDispositions();
  if(this.#selection)this.choose(this.#selection,false);else{this.#hits=[];this.renderCandidates();this.message('文字選択・カーソル操作で双方向の対応を確認できます。');for(const side of ['original','generated']){el(`map-${side}-position`).textContent='未選択';el(`map-${side}-selection`).textContent='未選択';el<HTMLTextAreaElement>(`map-${side}`).setSelectionRange(0,0);}el('map-detail').replaceChildren(node('p','コードを選択すると、元範囲・関連元・最適化理由を表示します。実行は下のロード操作を押すまで行いません。'));}
  if(state.focused!==null&&this.#hits[state.focused])this.showHit(this.#hits[state.focused]!,false);
  this.renderRuntime();this.buttons();this.changed();
 }
 inputChanged(stamp:string):void{el('map-stale').hidden=!this.#doc||stamp===this.#stamp;}
 invalidateRuntime():void{this.#epoch++;this.#loaded=false;this.#paused=false;this.buttons();if(this.#runtime)this.renderRuntime();}
 clear():void{this.invalidateRuntime();this.#doc=undefined;this.#runtime=null;this.#selection=null;this.#hits=[];this.#focused=null;el('map-panel').hidden=true;this.changed();}
 private message(value:string):void{el('map-message').textContent=value;}
 setBusy(value:boolean):void{this.#busy=value;this.buttons();}
 private buttons():void{
  for(const id of ['map-load','map-minify','map-link','map-import','map-export','map-download-code','map-download-map'])el<HTMLButtonElement>(id).disabled=this.#busy;

  for(const action of ['tick','draw','continue','into','over','out','clearBreakpoints','breakpoints','input']){
   el<HTMLButtonElement>(`map-${action}`).disabled=this.#busy||!this.#loaded||(['continue','into','over','out'].includes(action)&&!this.#paused);
  }
 }
 private showSource(i:number):void{const s=this.#doc?.sources[i];if(!s)throw new Error('元ファイルがありません');el<HTMLSelectElement>('map-source').value=String(i);el<HTMLTextAreaElement>('map-original').value=s.coordinates.value;}
 private highlight(side:'original'|'generated',start:number,end:number,source=Number(el<HTMLSelectElement>('map-source').value),focus=false):void{
  const doc=this.#doc!;if(side==='original')this.showSource(source);
  const coords=side==='generated'?doc.generated:doc.sources[source]!.coordinates;
  const area=el<HTMLTextAreaElement>(`map-${side}`);if(focus)area.focus({preventScroll:true});area.setSelectionRange(coords.uiAt(start),coords.uiAt(end,true));
  const p=coords.position(start),q=coords.position(end);el(`map-${side}-position`).textContent=`${p.line}:${p.column} → ${q.line}:${q.column}（UTF-16列・終端除外）`;
  const excerpt=coords.slice(start,end);el(`map-${side}-selection`).textContent=excerpt.length?`選択: ${excerpt.length>160?excerpt.slice(0,160)+'…':excerpt}`:'カーソル位置';
  // Bring the selected line into the existing scrollable editor, not the whole page.
  const font=getComputedStyle(area);const lineHeight=parseFloat(font.lineHeight)||22;area.scrollTop=Math.max(0,(p.line-3)*lineHeight);area.scrollLeft=Math.max(0,(p.column-15)*7);
 }
 private readSelection(side:'original'|'generated'):void{
  if(!this.#doc)return;const area=el<HTMLTextAreaElement>(`map-${side}`);const source=Number(el<HTMLSelectElement>('map-source').value);
  const coords=side==='generated'?this.#doc.generated:this.#doc.sources[source]!.coordinates;
  this.choose({side,source,start:coords.byteAt(area.selectionStart),end:coords.byteAt(area.selectionEnd)});
 }
 private choose(s:MapSelection,resetPage=true):void{
  if(!this.#doc)return;this.#doc.validateSelection(s);this.#selection=s;this.#focused=null;if(resetPage)this.#page=0;
  this.highlight(s.side,s.start,s.end,s.source);
  this.#hits=s.side==='generated'?this.#doc.generatedHits(s.start,s.end):this.#doc.originalHits(s.source,s.start,s.end);
  this.message(`${s.side==='generated'?'生成 → 原文':'原文 → 生成'}：${this.#hits.length}候補。候補一覧は静的な対応で、実行・停止可能性の保証ではありません。`);
  this.renderCandidates();el('map-detail').replaceChildren();
  if(this.#hits.length===1)this.showHit(this.#hits[0]!,false);
  else if(!this.#hits.length){
   const removed=s.side==='original'?this.#doc.dispositions(s.source,s.start,s.end):[];
   el('map-detail').append(node('p',removed.length?'この元範囲には除去・置換の記録があります。下の記録を確認してください。':'この選択に対応する生成範囲はありません。EOF・空白・未記録の位置を近隣のコードへ寄せません。'));
  }else el('map-detail').append(node('p','複数の対応があります。一つを選ぶと、その候補に記録された由来を確認できます。'));
  this.changed();
 }
 private label(span:OriginalSpan):string{const s=this.#doc!.sources[span.source]!,p=s.coordinates.position(span.start),q=s.coordinates.position(span.end);return `${s.name} ${p.line}:${p.column}–${q.line}:${q.column}`;}
 private sourceButton(span:OriginalSpan,role:string):HTMLButtonElement{
  const s=this.#doc!.sources[span.source]!,snippet=s.coordinates.slice(span.start,span.end);
  const b=node('button',`${role} · ${this.label(span)} · ${snippet.length>160?snippet.slice(0,160)+'…':snippet}`);b.className='map-source-link';
  b.addEventListener('click',()=>{this.choose({side:'original',source:span.source,start:span.start,end:span.end});this.highlight('original',span.start,span.end,span.source,true);});return b;
 }
 private renderCandidates():void{
  const root=el('map-candidates');root.replaceChildren();const pages=Math.max(1,Math.ceil(this.#hits.length/40));this.#page=Math.min(this.#page,pages-1);
  for(const h of this.#hits.slice(this.#page*40,(this.#page+1)*40)){
   const p=this.#doc!.generated.position(h.start),o=h.origin===null?null:this.#doc!.data.origins[h.origin]!;
   const b=node('button',`生成 ${p.line}:${p.column} · ${o?o.kind+' / '+o.precision:'Unknown'} · ${h.via}${h.original?' → '+this.label(h.original):''}`);
   b.className='map-hit';b.addEventListener('click',()=>this.showHit(h,true));root.append(b);
  }
  el('map-page').textContent=`${this.#hits.length}候補 · ${this.#page+1}/${pages}`;el<HTMLButtonElement>('map-prev').disabled=this.#page===0;el<HTMLButtonElement>('map-next').disabled=this.#page+1>=pages;
 }
 private reason(root:HTMLElement,reason:OptimizationReason):void{
  const d=document.createElement('details');d.className='map-reason';d.append(node('summary',`${reason.code} · ${reason.operation}`));
  d.append(node('p',reason.basis?`記録された適用根拠：${reason.basis}`:'詳細な適用根拠は記録されていません。変換名からの推測は行いません。'));
  if(reason.before!==null||reason.after!==null)d.append(node('p',`${reason.before??'未記録'} → ${reason.after??'未記録'}`));
  if(reason.facts.length){const table=document.createElement('table');for(const f of reason.facts){const tr=document.createElement('tr');tr.append(node('th',f.key),node('td',f.value));table.append(tr);}d.append(table);}root.append(d);
 }
 private showHit(h:MapHit,focus:boolean):void{
  const doc=this.#doc!,d=doc.data,root=el('map-detail');this.#focused=this.#hits.indexOf(h);root.replaceChildren();this.highlight('generated',h.start,h.end,undefined,focus);
  const o=h.origin===null?null:d.origins[h.origin]!;
  root.append(node('h3',o?`${o.kind} / ${o.precision}${o.name?' · '+o.name:''}`:'Unknown / 由来未取得'));
  if(!o){root.append(node('p','この区間の元位置は記録されていません。自動生成や近隣位置へ置き換えません。'));this.changed();return;}
  if(o.primary){root.append(this.sourceButton(o.primary,'主な由来'));if(this.#selection?.side==='generated')this.highlight('original',o.primary.start,o.primary.end,o.primary.source);}
  else root.append(node('p','コンパイラが生成したコードです。元の一行・一文字に対応させません。'));
  const ms=d.mappings.filter(m=>m.start<=h.start&&h.start<m.end);
  const copy=ms[0]?.copied;root.append(node('p',copy?'copy: 元snapshotと同じバイト列です。区間内部の位置も対応します。':'構文の由来です。文字位置を差分で補間しません。'));
  for(const id of o.related){const r=d.relations[id]!;root.append(this.sourceButton(r.span,r.role));}
  for(const id of [...new Set(ms.flatMap(m=>m.inlineContexts))]){
   const c=d.contexts[id]!;root.append(node('h4','インライン文脈（VMの実フレームではありません）'),this.sourceButton(c.definition,'定義'),this.sourceButton(c.callSite,'呼び出し'));
  }
  const reasonIds=new Set(o.reasons);
  for(const c of d.constructs)if(c.start<=h.start&&h.end<=c.end&&c.origin!==null)for(const id of d.origins[c.origin]!.reasons)reasonIds.add(id);
  root.append(node('h4',`この位置と親構文の説明 · ${reasonIds.size}件`));
  for(const id of reasonIds)this.reason(root,d.reasons[id]!);
  if(!reasonIds.size)root.append(node('p','この区間に記録された変換理由はありません。'));this.changed();
 }
 private renderDispositions():void{
  const doc=this.#doc!,root=el('map-dispositions');root.replaceChildren();el('map-disposition-count').textContent=String(doc.data.dispositions.length);
  // Details remain keyboard accessible without generating a huge open DOM.
  for(const d of doc.data.dispositions){const row=document.createElement('details');row.append(node('summary',this.label(d.original)),this.sourceButton(d.original,'除去・置換した元構文'));this.reason(row,doc.data.reasons[d.reason]!);for(const s of d.replacementSources)row.append(this.sourceButton(s,'関連する元構文（停止先ではありません）'));root.append(row);}
 }
 observeRuntime(result:MappedResult):void {
  if(!this.#doc||result.artifact!==this.#doc.data.integrity)return;
  this.#runtime=result;this.#loaded=true;this.#paused=result.outcome==='suspended'||(['breakpoints-set','breakpoints-cleared','input-updated'].includes(result.outcome)&&this.#paused);
  this.renderRuntime();this.buttons();this.changed();
 }
 private async runtimeAction(action:string):Promise<void>{
  if(!this.#doc)return;const epoch=this.#epoch;
  try{
   const options:Record<string,unknown>={identity:this.#doc.data.integrity};
   if(action==='load'){options['artifact']=this.#doc.artifact;options['properties']=JSON.parse(el<HTMLTextAreaElement>('map-properties').value);}
   if(action==='input'){options['numbers']=JSON.parse(el<HTMLInputElement>('map-numbers').value);options['booleans']=JSON.parse(el<HTMLInputElement>('map-booleans').value);}
   if(action==='breakpoints'){
    const s=this.#selection;if(!s)throw new Error('元コードの範囲を選択してください');
    let original:OriginalSpan|undefined;
    if(s.side==='original')original={source:s.source,start:s.start,end:s.end};
    else {const hits=this.#doc.generatedHits(s.start,s.end);if(hits.length===1&&hits[0]?.original)original=hits[0].original;}
    if(!original)throw new Error('元範囲が一つに決まっていません。元コード側から選択してください');Object.assign(options,original);
   }
   const result=await this.run(action,options) as MappedResult;if(epoch!==this.#epoch)return;
   if(result.kind!=='mapped-runtime'||result.artifact!==this.#doc.data.integrity)throw new Error('実行結果と生成物が一致しません');
   this.#runtime=result;this.#loaded=true;this.#paused=result.outcome==='suspended'||(['breakpoints-set','breakpoints-cleared','input-updated'].includes(result.outcome)&&this.#paused);
   this.renderRuntime();this.buttons();this.changed();
  }catch(e){this.message(errorMessage(e));}
 }
 private renderRuntime():void{
  const root=el('map-runtime-result');root.replaceChildren();const r=this.#runtime;
  if(!r){root.append(node('p','VMは未ロードです。マップの取得・閲覧だけではLuaを実行しません。'));return;}
  root.append(node('strong',`${this.#loaded?'実行結果':'保存された実行結果 · VMは未ロード'} / ${r.outcome}`));
  root.append(node('p',`chunk ${r.chunk} · 実行世代 ${r.generation}`));if(r.error)root.append(node('pre',r.error));
  for(const location of r.locations){
   const l=location.association;const b=node('button',`${location.kind} · ${location.chunk}:${location.line} · ${l?'生成行のみ（列不明）'+(l.ambiguous?' / 複数の元位置':''):'対応する生成物なし'}`);
   b.disabled=!l;b.addEventListener('click',()=>{if(!this.#doc||!l)return;const range=this.#doc.generated.line(l.line);if(range)this.choose({side:'generated',source:0,...range});});root.append(b);
  }
  root.append(node('p','生成行から実際の列や元変数の値は復元しません。ステップとbreakpointは生成Luaの実行位置に作用します。'));
  const data=document.createElement('details');data.append(node('summary','生成VMのstack・I/O・ログ'),node('pre',stringify({stack:r.stack,io:r.io,logs:r.logs})));root.append(data);
 }
 private download(name:string,value:string,type='application/json'):void{const url=URL.createObjectURL(new Blob([value],{type}));const a=document.createElement('a');a.href=url;a.download=name;a.click();setTimeout(()=>URL.revokeObjectURL(url),1000);}
}
