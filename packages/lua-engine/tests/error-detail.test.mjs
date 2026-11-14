import {test} from 'node:test';
import assert from 'node:assert/strict';
import {Bridge} from '../dist/bridge.js';
import {EngineError,STATUS,ABI} from '../dist/index.js';

// 実WASMを使わず、エラーと位置情報のバッファを持つ最小のbackendでデコードだけを確かめる。
function backend(message,detail,withDetailExports=true){
  const memory={buffer:new ArrayBuffer(4096)},bytes=new Uint8Array(memory.buffer),encoder=new TextEncoder();
  const text=encoder.encode(message),json=detail===undefined?new Uint8Array():encoder.encode(detail);
  bytes.set(text,16);bytes.set(json,1024);
  const functions={abi_version:()=>ABI.abiVersion,capabilities:()=>ABI.capabilities,error_ptr:()=>16,error_len:()=>text.length};
  if(withDetailExports)Object.assign(functions,{error_detail_ptr:()=>1024,error_detail_len:()=>json.length});
  return {memory,functions};
}
test('STATUS names the codes that EngineError.code carries',()=>{
  assert.deepEqual(STATUS,{ok:0,lua:1,limit:2,invalidHandle:3,invalidArgument:4,busy:5,unsupported:6,suspended:7,missing:8,failed:9,host:10});
});
test('error detail is decoded into the public shape',()=>{
  const detail={stage:'runtime',location:{chunk:'@main.lua',line:3},stack:[{level:0,source:'=[C]',line:-1,functionName:'error'},{level:1,source:'@main.lua',line:3,functionName:null}]};
  const error=new Bridge(backend('boom',JSON.stringify(detail))).error(STATUS.lua);
  assert.ok(error instanceof EngineError);assert.equal(error.code,1);assert.equal(error.message,'boom');
  assert.deepEqual(error.detail,detail);
});
test('missing detail stays undefined instead of an empty placeholder',()=>{
  assert.equal(new Bridge(backend('bad handle')).error(STATUS.invalidHandle).detail,undefined);
  assert.equal(new Bridge(backend('raster only',undefined,false)).error(STATUS.invalidArgument).detail,undefined);
});
test('malformed detail is rejected, not repaired',()=>{
  for(const detail of ['{"stage":"other","location":null,"stack":[]}','{"stage":null,"location":{"chunk":"@a","line":0},"stack":[]}','{"stage":null,"location":null,"stack":[{"level":0,"source":1,"line":1,"functionName":null}]}','[]']){
    assert.throws(()=>new Bridge(backend('x',detail)).error(STATUS.lua),TypeError);
  }
});
