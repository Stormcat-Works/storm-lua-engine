import {test} from 'node:test';
import assert from 'node:assert/strict';
import {encodeCommands} from '../dist/commands.js';
import {decodeDebugValue} from '../dist/debug.js';
import {decodeProperties,encodeProperties,numberBits,numberFromBits} from '../dist/properties.js';

test('properties preserve binary64 non-finite and signed zero values',()=>{
  for(const n of [0,-0,NaN,Infinity,-Infinity,16777217,Math.PI]) assert.ok(Object.is(numberFromBits(numberBits(n)),n));
  const payload=JSON.parse(new TextDecoder().decode(encodeProperties({gain:16777217,raw:new Uint8Array([0,255]),flag:false})));
  assert.equal(numberFromBits(payload[0].bits),16777217);
  assert.deepEqual(payload[1].bytes,[0,255]);
});
test('debug values preserve signed i64 domain and byte strings',()=>{
  assert.equal(decodeDebugValue({kind:'integer',value:'9223372036854775807'}).value,9223372036854775807n);
  assert.equal(decodeDebugValue({kind:'integer',value:'-9223372036854775808'}).value,-9223372036854775808n);
  assert.deepEqual(decodeDebugValue({kind:'bytes',value:[0,255]}).value,new Uint8Array([0,255]));
  assert.throws(()=>decodeDebugValue({kind:'integer',value:'9223372036854775808'}));
  assert.throws(()=>decodeDebugValue({kind:'number',bits:'nan'}));
  assert.throws(()=>decodeDebugValue({kind:'bytes',value:[256]}));
});
test('binary commands are self-delimiting and keep fractional f64 coordinates',()=>{
  const wire=encodeCommands([{kind:'color',rgba:[255,4,5,128]},{kind:'line',from:[0.1,-0.5],to:[8,12]}]);
  const view=new DataView(wire.buffer);assert.equal(wire.length,52);
  assert.equal(view.getUint16(0,true),1);assert.equal(view.getUint32(4,true),4);
  assert.equal(view.getUint16(12,true),3);assert.equal(view.getFloat64(20,true),0.1);
  assert.throws(()=>encodeCommands([{kind:'color',rgba:[256,0,0,255]}]));
});

test('property snapshots retain raw labels and numeric bit patterns and reject malformed entries',()=>{
  const raw=[{label:[0,255],kind:'text',bytes:[255,0]},{label:[1],kind:'number',bits:numberBits(-0)}];
  const properties=decodeProperties(raw);
  assert.deepEqual(properties[0],{label:new Uint8Array([0,255]),value:{kind:'text',bytes:new Uint8Array([255,0])}});
  assert.ok(Object.is(properties[1].value.value,-0));
  for(const invalid of [{},[raw[0],raw[0]],[{label:[1],kind:'bool',value:1}],
    [{label:[256],kind:'text',bytes:[]}],[{label:[1],kind:'number',bits:'bad'}]])assert.throws(()=>decodeProperties(invalid));
});
