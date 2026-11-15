/**
 * CommonJSのTypeScriptから型を使う例。実行はせず、型検査だけに使います。
 * SDKはESMだけなので、CommonJSのファイルでは次の2点が要ります。
 * - tsconfigのmoduleとmoduleResolutionをNode16（またはNodeNext）にする。
 * - 型のimportに resolution-mode: import を付け、値は動的 import() で読み込む。
 */
import type { LuaEngine, VehicleVm } from '@stormcat-works/storm-lua-engine' with { 'resolution-mode': 'import' };
import type { StackFrame } from '@stormcat-works/storm-lua-engine/debug' with { 'resolution-mode': 'import' };

export async function createVehicle(wasmBinary: Uint8Array): Promise<VehicleVm> {
  // Node16の設定では、この import() は require() に変換されずにそのまま出力されます。
  const sdk = await import('@stormcat-works/storm-lua-engine');
  const engine: LuaEngine = await sdk.loadRuntime({ wasmBinary });
  return engine.createVehicle();
}
export function topLine(vm: VehicleVm): number | undefined {
  const frames: StackFrame[] = vm.stack();
  return frames[0]?.line;
}
