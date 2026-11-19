/** Component Mod モードは順序付きライフサイクルを持ちますが、Composite I/O、画面描画、HTTP は持ちません。 */
import { Bridge, EngineError, object, unsigned, type Outcome } from './bridge.js';
import { ScriptVm, type ScriptOptions, type LogHandler } from './script.js';
import type { ComponentProviders } from './host.js';
export type { ComponentProviders } from './host.js';
/** 実ゲームのエラーステータス。timedOut は数値コードが未確定のため code が null です。 */
export type ComponentStatusName = 'ok' | 'tickFailed' | 'loadFailed' | 'callbackFailed' | 'timedOut';
export interface ComponentStatusInfo { readonly status: ComponentStatusName; readonly code: number | null }
export interface ComponentOptions extends ScriptOptions {
  /** ホスト提供ドメイン。提供されたドメインの関数だけが Lua 環境へ登録されます。 */
  readonly providers?: ComponentProviders;
}
const STATUS_NAMES: readonly string[] = ['ok', 'tickFailed', 'loadFailed', 'callbackFailed', 'timedOut'];
export class ComponentVm extends ScriptVm {
  readonly mode = 'component' as const;
  constructor(bridge: Bridge, handle: number, onLog: LogHandler | undefined, releaseHost: () => void) {
    super(bridge, handle, onLog, releaseHost);
    if (bridge.query('mode', handle) !== 3) throw new EngineError(4, 'Handle is not a component');
  }
  /** ロード済みスクリプトの onParse を 1 度呼びます（定義データの読み書き期間）。 */
  parse(): Outcome { return this.control('component', {action: 'parse'}).outcome; }
  /** onAddToSimulation を 1 度呼びます。 */
  addToSimulation(): Outcome { return this.control('component', {action: 'add_to_simulation'}).outcome; }
  /** onTick(game_ticks) を呼びます。game_ticks は符号なし整数で、0 も受け付けます。 */
  tick(gameTicks = 1): Outcome { return this.control('component', {action: 'tick', gameTicks: unsigned(gameTicks, 'gameTicks')}).outcome; }
  /** onRender を呼びます。 */
  render(): Outcome { return this.control('component', {action: 'render'}).outcome; }
  /** onRemoveFromSimulation を 1 度呼びます。tick 失敗後でも配送されます。 */
  removeFromSimulation(): Outcome { return this.control('component', {action: 'remove_from_simulation'}).outcome; }
  /** 現在のエラーステータス。ステータスは自動復帰しません。 */
  status(): ComponentStatusInfo {
    const data = object(this.control('component', {action: 'status'}).data);
    const status = data['status'], code = data['code'];
    if (typeof status !== 'string' || !STATUS_NAMES.includes(status)) throw new TypeError('Invalid component status');
    if (code !== null && typeof code !== 'number') throw new TypeError('Invalid component status code');
    return {status: status as ComponentStatusName, code};
  }
}
