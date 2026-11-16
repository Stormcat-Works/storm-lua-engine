/** アドオンモードはライフサイクル/イベント/セーブデータを持ちますが、Composite I/O や画面描画は行いません。 */
import { Bridge, EngineError, object, signed, byteArray, type Outcome } from './bridge.js';
import { ScriptVm, type ScriptOptions, type LogHandler } from './script.js';
import { decodeLuaValue, encodeLuaValue, encodeLuaValues, type LuaValue, type LuaTable } from './values.js';
import { ADDON_EVENTS } from './catalog.js';
import type { Properties } from './properties.js';
import type { ServerFunctions } from './host.js';
export type AddonEvent = typeof ADDON_EVENTS[number];
export interface AddonOptions extends ScriptOptions {
  readonly newWorld?: boolean;
  readonly properties?: Properties;
  readonly savedata?: LuaTable;
  readonly server?: ServerFunctions;
}
export type MenuProperty =
  | {readonly kind:'checkbox'; readonly label:Uint8Array; readonly default:boolean}
  | {readonly kind:'slider'; readonly label:Uint8Array; readonly min:number; readonly max:number; readonly increment:number; readonly default:number};
function finite(value: unknown): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) throw new TypeError('Invalid menu property number');
  return value;
}
export class AddonVm extends ScriptVm {
  readonly mode = 'addon' as const;
  constructor(bridge: Bridge, handle: number, onLog: LogHandler | undefined, releaseHost: () => void) {
    super(bridge,handle,onLog,releaseHost);
    if (bridge.query('mode',handle) !== 2) throw new EngineError(4,'Handle is not an addon');
  }
  start(): Outcome { return this.control('addon',{action:'start'}).outcome; }
  tick(gameTicks = 1): Outcome { return this.control('addon',{action:'tick',gameTicks:signed(gameTicks,'gameTicks')}).outcome; }
  dispatch(callback: AddonEvent, arguments_: readonly LuaValue[] = []): Outcome {
    return this.control('addon',{action:'dispatch',callback,arguments:encodeLuaValues(arguments_)}).outcome;
  }
  /** 明示的な onDestroy の呼び出し。dispose() が暗黙的にLuaを実行することはありません。 */
  destroy(): Outcome { return this.control('addon',{action:'destroy'}).outcome; }
  savedata(): LuaTable {
    const value = decodeLuaValue(this.control('addon',{action:'savedata'}).data);
    if (value === null || typeof value !== 'object' || value instanceof Uint8Array || value.kind !== 'table') throw new TypeError('g_savedata is not a table');
    return value;
  }
  /** ホストのチェックポイントから再生成します。その後、onCreate(false) のために明示的に start() を呼び出してください。 */
  reload(savedata: LuaTable): Outcome { return this.control('addon',{action:'reload',savedata:encodeLuaValue(savedata)}).outcome; }
  menuProperties(): MenuProperty[] {
    const values = this.control('addon',{action:'properties'}).data;
    if (!Array.isArray(values)) throw new TypeError('Invalid menu property list');
    return values.map(value => {
      const entry=object(value), label=byteArray(entry['label']);
      if (entry['kind']==='checkbox' && typeof entry['default']==='boolean') return {kind:'checkbox',label,default:entry['default']};
      if (entry['kind']==='slider') return {kind:'slider',label,min:finite(entry['min']),max:finite(entry['max']),increment:finite(entry['increment']),default:finite(entry['default'])};
      throw new TypeError('Invalid menu property');
    });
  }
}
