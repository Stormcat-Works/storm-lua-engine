import {byteArray, object} from './bridge.js';
/** コールドパスの値は、情報落ちするJSON数値を通さず、バイト列およびf64のビット表現をそのまま保持します。 */
export type PropertyValue =
  | { readonly kind: 'number'; readonly value: number }
  | { readonly kind: 'bool'; readonly value: boolean }
  | { readonly kind: 'text'; readonly bytes: Uint8Array };
export interface PropertyEntry { readonly label: string | Uint8Array; readonly value: PropertyValue }
export type Properties = readonly PropertyEntry[] | Readonly<Record<string, number | boolean | string | Uint8Array>>;
const encoder = new TextEncoder();
export function numberBits(value: number): string {
  const view = new DataView(new ArrayBuffer(8));
  view.setFloat64(0, value, false);
  return view.getBigUint64(0, false).toString(16).padStart(16, '0');
}
export function numberFromBits(value: unknown): number {
  if (typeof value !== 'string' || !/^[0-9a-fA-F]{16}$/.test(value)) throw new TypeError('Invalid f64 bit representation');
  const view = new DataView(new ArrayBuffer(8));
  view.setBigUint64(0, BigInt('0x' + value), false);
  return view.getFloat64(0, false);
}
export function encodeProperties(properties: Properties): Uint8Array {
  const entries: readonly PropertyEntry[] = Array.isArray(properties) ? properties :
    Object.entries(properties).map(([label, value]) => ({label, value: typeof value === 'number' ? {kind:'number', value} :
      typeof value === 'boolean' ? {kind:'bool', value} : {kind:'text', bytes:typeof value === 'string' ? encoder.encode(value) : value}}));
  if (entries.length > 4096) throw new RangeError('Too many properties');
  const payload = entries.map(({label, value}) => {
    const bytes = typeof label === 'string' ? encoder.encode(label) : label;
    if (!(bytes instanceof Uint8Array) || bytes.length > 1024 * 1024) throw new TypeError('Invalid property label');
    const name = Array.from(bytes);
    switch (value.kind) {
      case 'number':
        if (typeof value.value !== 'number') throw new TypeError('Number property requires a number');
        return {label:name, kind:'number', bits:numberBits(value.value)};
      case 'bool':
        if (typeof value.value !== 'boolean') throw new TypeError('Boolean property requires a Boolean');
        return {label:name, kind:'bool', value:value.value};
      case 'text':
        if (!(value.bytes instanceof Uint8Array) || value.bytes.length > 1024 * 1024) throw new TypeError('Invalid property bytes');
        return {label:name, kind:'text', bytes:Array.from(value.bytes)};
    }
  });
  const result = encoder.encode(JSON.stringify(payload));
  if (result.length > 4 * 1024 * 1024) throw new RangeError('Property request exceeds 4 MiB');
  return result;
}

/** Decode a property snapshot without losing binary strings or binary64 values. */
export function decodeProperties(input: unknown): PropertyEntry[] {
  if(!Array.isArray(input) || input.length>4096) throw new TypeError('Invalid property list');
  let bytes=0;const labels=new Set<string>();
  return input.map(entry=>{
    const v=object(entry);const label=byteArray(v['label']);
    if(label.length>1024*1024) throw new RangeError('Property label exceeds 1 MiB');
    const key=JSON.stringify(Array.from(label));
    if(labels.has(key)) throw new TypeError('Duplicate property label');
    labels.add(key);bytes+=label.length;
    let value:PropertyValue;
    switch(v['kind']){
      case 'number':value={kind:'number',value:numberFromBits(v['bits'])};break;
      case 'bool':
        if(typeof v['value']!=='boolean') throw new TypeError('Invalid property Boolean');
        value={kind:'bool',value:v['value']};break;
      case 'text':{
        const data=byteArray(v['bytes']);bytes+=data.length;
        if(data.length>1024*1024) throw new RangeError('Property text exceeds 1 MiB');
        value={kind:'text',bytes:data};break;
      }
      default:throw new TypeError('Unknown property kind');
    }
    if(bytes>4*1024*1024) throw new RangeError('Property snapshot exceeds byte budget');
    return {label,value};
  });
}
