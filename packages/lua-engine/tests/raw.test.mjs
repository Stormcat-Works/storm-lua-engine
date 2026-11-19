import { test } from 'node:test';
import assert from 'node:assert/strict';
import { RawIoView } from '../dist/raw.js';
import { ABI } from '../dist/index.js';

function create(offset=1024) {
  const memory = new WebAssembly.Memory({initial:1, maximum:3});
  return {memory, io:new RawIoView(memory,offset,ABI.abiVersion)};
}
test('ABI is the generated 320-byte binary32 contract',()=>{
  assert.equal(ABI.ioByteLength,320); assert.equal(ABI.numberBytes,4);
  assert.equal(ABI.capabilities,63);
});
test('I/O is a live view, not a copy',()=>{
  const {memory,io}=create(); const views=io.borrow();
  views.inputNumbers[0]=16777217;
  assert.equal(new Float32Array(memory.buffer,1024,32)[0],16777216);
  new Float32Array(memory.buffer,1024+ABI.outputNumbersOffset,32)[0]=0.5;
  assert.equal(views.outputNumbers[0],0.5);
  assert.equal(io.borrow(),views);
});
test('memory growth refreshes all cached views and preserves values',()=>{
  const {memory,io}=create(); const before=io.borrow(); before.inputNumbers[2]=3.5;
  memory.grow(1); const after=io.borrow();
  assert.notEqual(after,before); assert.equal(before.inputNumbers.byteLength,0);
  assert.equal(after.inputNumbers[2],3.5);
});
test('grow(0) also refreshes a detached nonshared buffer',()=>{
  const {memory,io}=create(); const before=io.borrow(); memory.grow(0);
  assert.notEqual(io.borrow(),before);
});
test('invalid ABI, offsets and bounds reject',()=>{
  const {memory}=create();
  for(const offset of [-4,1,0.5,NaN,Infinity,2**53,65536-316]) {
    assert.throws(()=>new RawIoView(memory,offset,ABI.abiVersion));
  }
  assert.throws(()=>new RawIoView(memory,0,ABI.abiVersion+1));
  assert.doesNotThrow(()=>new RawIoView(memory,65536-320,ABI.abiVersion));
});
test('invalid booleans reject rather than coerce',()=>{
  const {io}=create(); io.borrow().inputBooleans[0]=2;
  assert.throws(()=>io.validateInputs()); io.borrow().inputBooleans[0]=1;
  assert.doesNotThrow(()=>io.validateInputs());
});
test('owner invalidation blocks subsequent borrowing',()=>{
  const {io}=create(); io.invalidate(); assert.throws(()=>io.borrow());
});
test('NaN infinity and signed zero do not pass through JSON',()=>{
  const {io}=create(); const a=io.borrow().inputNumbers;
  a[0]=NaN; a[1]=Infinity; a[2]=-0;
  assert.ok(Number.isNaN(a[0])); assert.equal(a[1],Infinity); assert.ok(Object.is(a[2],-0));
});
test('baseline shared memory is explicitly unsupported',()=>{
  const memory=new WebAssembly.Memory({initial:1,maximum:2,shared:true});
  assert.throws(()=>new RawIoView(memory,0,ABI.abiVersion),/synchronization/);
});
