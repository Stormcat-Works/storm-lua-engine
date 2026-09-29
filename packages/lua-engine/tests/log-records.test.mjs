import {test} from 'node:test';
import assert from 'node:assert/strict';
import {ScriptVm} from '../dist/script.js';

class LogsOnlyVm extends ScriptVm {mode='vehicle';}
function records(record){
  const bridge={call(){},response(){return [record];}};
  return new LogsOnlyVm(bridge,1,undefined,()=>{}).drainLogRecords();
}
test('log wire accepts optional call sites without rewriting legacy record shape',()=>{
  const old={source:'print',bytes:[65,255]};
  assert.deepEqual(records(old),[{source:'print',bytes:new Uint8Array([65,255])}]);
  assert.deepEqual(records({...old,location:null}),records(old));
  const location={chunk:'@lib/ログ.lua',line:12};
  assert.deepEqual(records({...old,location})[0].location,location);
});
test('log wire rejects malformed locations instead of fabricating a file or line',()=>{
  for(const location of [false,[],{chunk:1,line:2},{chunk:'@a.lua',line:0},{chunk:'@a.lua',line:-1},
    {chunk:'@a.lua',line:1.5},{chunk:'@a.lua',line:'2'},{chunk:'@a.lua',line:2**32}]){
    assert.throws(()=>records({source:'debug.log',bytes:[65],location}));
  }
});
