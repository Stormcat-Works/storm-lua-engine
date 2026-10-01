/** Logical cursor movement in the read-only map views, without changing their text.
 * Chromium/WebKit do not move a readonly textarea caret with unmodified arrows.
 * Handle the same basic keys on every engine and never split a surrogate pair.
 */
export interface TextSelection {start:number;end:number;direction:'forward'|'backward'|'none'}
export function moveMapSelection(value:string,selection:TextSelection,key:string,extend:boolean):TextSelection|null {
 if(!['ArrowLeft','ArrowRight','ArrowUp','ArrowDown','Home','End'].includes(key))return null;
 const {start,end,direction}=selection;
 const previous=(at:number)=>at===0?0:at-(at>1&&/[\uDC00-\uDFFF]/.test(value[at-1]!)&&/[\uD800-\uDBFF]/.test(value[at-2]!)?2:1);
 const next=(at:number)=>at>=value.length?value.length:at+(value.codePointAt(at)!>0xffff?2:1);
 const lineStart=(at:number)=>at===0?0:value.lastIndexOf('\n',at-1)+1;
 const lineEnd=(at:number)=>{const index=value.indexOf('\n',at);return index<0?value.length:index;};
 const focus=direction==='backward'?start:end;
 const anchor=direction==='backward'?end:start;
 let target=focus;
 switch(key){
  case 'ArrowLeft':target=!extend&&start!==end?start:previous(focus);break;
  case 'ArrowRight':target=!extend&&start!==end?end:next(focus);break;
  case 'Home':target=lineStart(focus);break;
  case 'End':target=lineEnd(focus);break;
  case 'ArrowUp':case 'ArrowDown':{
   const from=lineStart(focus),until=lineEnd(focus);
   const column=Array.from(value.slice(from,focus)).length;
   const destination=key==='ArrowUp'?(from===0?0:lineStart(from-1)):(until===value.length?value.length:until+1);
   if(key==='ArrowUp'&&from===0){target=0;break;}
   if(key==='ArrowDown'&&until===value.length){target=value.length;break;}
   const limit=lineEnd(destination);target=destination;
   for(let i=0;i<column&&target<limit;i++)target=next(target);
   break;
  }
 }
 if(!extend)return {start:target,end:target,direction:'none'};
 return {start:Math.min(anchor,target),end:Math.max(anchor,target),direction:target<anchor?'backward':'forward'};
}
