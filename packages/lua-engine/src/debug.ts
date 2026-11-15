/** ロスレスなホストデバッガプロトコル。検査時にユーザテーブルが文字列化されることはありません。 */
import { object, byteArray, unsigned } from './bridge.js';
import { numberFromBits } from './properties.js';
export interface DebugHandle { readonly vmId: bigint; readonly pauseEpoch: bigint; readonly slot: number }
export type DebugValue =
  | { readonly kind: 'nil' }
  | { readonly kind: 'bool'; readonly value: boolean }
  | { readonly kind: 'integer'; readonly value: bigint }
  | { readonly kind: 'number'; readonly value: number }
  | { readonly kind: 'bytes'; readonly value: Uint8Array }
  | { readonly kind: 'table'; readonly handle: DebugHandle }
  | { readonly kind: 'opaque'; readonly typeName: string };
export interface Variable { readonly name: Uint8Array; readonly value: DebugValue }
export interface StackFrame { readonly level: number; readonly source: string; readonly line: number; readonly functionName: string | null }
export interface TableEntry { readonly key: DebugValue; readonly value: DebugValue }
export interface Breakpoint { readonly source: string; readonly line: number }
export type StepMode = 'continue' | 'into' | 'over' | 'out';
export interface ResumeOptions {
  /**
   * ステップ（into/over/out）がコールバックの終わりを越えたとき、次に実行するコールバックの
   * 最初の行で止めます。既定はfalseで、ステップはコールバックの終わりで消えます。
   */
  readonly carryToNextEntry?: boolean;
}
/**
 * 実行中のスクリプトを止めるかどうかを答える関数。VMは一定の命令数ごとに同期で呼びます。
 * trueを返すと、次に中断できる命令で実行が suspended になります。例外はスクリプトのエラーになります。
 */
export type PausePoll = () => boolean;
function bigint(value: unknown, signed = false): bigint {
  if (typeof value !== 'string' || !(signed ? /^-?\d+$/ : /^\d+$/).test(value)) throw new TypeError('Invalid decimal integer');
  const integer = BigInt(value);
  if (signed ? integer < -(1n << 63n) || integer >= (1n << 63n) : integer < 0n || integer >= (1n << 64n)) throw new RangeError('Integer is outside its 64-bit domain');
  return integer;
}
export function decodeDebugValue(input: unknown): DebugValue {
  const v = object(input);
  switch (v['kind']) {
    case 'nil': return {kind:'nil'};
    case 'bool':
      if (typeof v['value'] !== 'boolean') throw new TypeError('Invalid Boolean');
      return {kind:'bool', value:v['value']};
    case 'integer': return {kind:'integer', value:bigint(v['value'], true)};
    case 'number': return {kind:'number', value:numberFromBits(v['bits'])};
    case 'bytes': return {kind:'bytes', value:byteArray(v['value'])};
    case 'table': {
      const h = object(v['handle']);
      if (typeof h['slot'] !== 'number') throw new TypeError('Invalid table slot');
      return {kind:'table', handle:{vmId:bigint(h['vmId']), pauseEpoch:bigint(h['pauseEpoch']), slot:unsigned(h['slot'],'slot')}};
    }
    case 'opaque':
      if (typeof v['typeName'] !== 'string') throw new TypeError('Invalid opaque value');
      return {kind:'opaque', typeName:v['typeName']};
    default: throw new TypeError('Unknown debugger value kind');
  }
}
export function decodeVariables(input: unknown): Variable[] {
  if (!Array.isArray(input)) throw new TypeError('Expected variable array');
  return input.map(v => { const item = object(v); return {name:byteArray(item['name']), value:decodeDebugValue(item['value'])}; });
}
export function decodeStack(input: unknown): StackFrame[] {
  if (!Array.isArray(input)) throw new TypeError('Expected stack array');
  return input.map(v => {
    const f = object(v); const level=f['level'], source=f['source'], line=f['line'], functionName=f['functionName'];
    if (typeof level !== 'number' || typeof source !== 'string' || typeof line !== 'number' || !Number.isInteger(line) || !(functionName === null || typeof functionName === 'string')) throw new TypeError('Invalid stack frame');
    return {level:unsigned(level,'level'),source,line,functionName};
  });
}
