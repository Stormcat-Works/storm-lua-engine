import {test} from 'node:test';
import assert from 'node:assert/strict';
import {HostDispatcher} from '../dist/host.js';

// 実WASMを使わず、VMからの問い合わせに対する応答の形だけを確かめる。
const request=new TextEncoder().encode(JSON.stringify({kind:'pausePoll'}));

test('pausePoll answers exactly one byte and is called once per request',()=>{
  const hosts=new HostDispatcher();let calls=0,pause=false;
  const key=hosts.register({pause:()=>{calls++;return pause;}});
  assert.deepEqual([...hosts.invoke(key,request)],[0]);
  pause=true;
  assert.deepEqual([...hosts.invoke(key,request)],[1]);
  assert.equal(calls,2);
});
test('a missing, non-Boolean or asynchronous pausePoll is an error, not a default answer',()=>{
  const hosts=new HostDispatcher();
  assert.throws(()=>hosts.invoke(hosts.register({}),request),{code:6});
  for(const answer of [undefined,null,0,1,'true',Promise.resolve(true)]){
    assert.throws(()=>hosts.invoke(hosts.register({pause:()=>answer}),request),TypeError);
  }
  assert.throws(()=>hosts.invoke(hosts.register({pause:()=>{throw new RangeError('poll failed');}}),request),/poll failed/);
});
