/** One durable workspace record. Large maps do not belong in localStorage. */
const name='storm-lua-playground';
function database():Promise<IDBDatabase>{return new Promise((resolve,reject)=>{
 const request=indexedDB.open(name,1);
 request.onupgradeneeded=()=>request.result.createObjectStore('workspace');
 request.onerror=()=>reject(request.error??new Error('IndexedDBを開けません'));
 request.onblocked=()=>reject(new Error('別のタブが保存領域の更新を妨げています'));
 request.onsuccess=()=>resolve(request.result);
});}
export async function readState():Promise<unknown>{
 const db=await database();try{return await new Promise((resolve,reject)=>{
  const tx=db.transaction('workspace','readonly'),request=tx.objectStore('workspace').get('current');
  request.onerror=()=>reject(request.error);tx.onabort=()=>reject(tx.error??new Error('保存読取が中断されました'));
  tx.oncomplete=()=>resolve(request.result);
 });}finally{db.close();}
}
let queue:Promise<void>=Promise.resolve();
export function writeState(value:unknown):Promise<void>{
 const write=async()=>{const db=await database();try{await new Promise<void>((resolve,reject)=>{
  const tx=db.transaction('workspace','readwrite');tx.objectStore('workspace').put(value,'current');
  tx.oncomplete=()=>resolve();tx.onerror=()=>reject(tx.error??new Error('保存に失敗しました'));tx.onabort=()=>reject(tx.error??new Error('保存が中断されました'));
 });}finally{db.close();}};
 // A failed earlier transaction is reported to its caller, but must not poison future explicit saves.
 const current=queue.then(write,write);queue=current;return current;
}
