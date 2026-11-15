/**
 * CommonJSのホスト（VSCodeの拡張機能など）からSDKを使う例。
 * SDKはESMだけなので、require() では読み込めません。動的 import() で読み込みます。
 * WASMはパッケージ名から場所を求めて fs で読み、wasmBinary として渡します。
 * file: のURLを fetch できないホストでも同じ手順で動きます。
 */
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');

async function main() {
  // "." のexportsには import 条件しかないので、require() はここで失敗します。
  assert.throws(() => require('@stormcat-works/storm-lua-engine'), { code: 'ERR_PACKAGE_PATH_NOT_EXPORTED' });

  // WASMのサブパスには条件がないので、require.resolve で場所を求められます。
  const wasmBinary = readFileSync(require.resolve('@stormcat-works/storm-lua-engine/wasm/storm_lua_wasm.wasm'));
  const { loadRuntime, EngineError } = await import('@stormcat-works/storm-lua-engine');
  const engine = await loadRuntime({ wasmBinary });
  const vm = engine.createVehicle();
  try {
    vm.load('function onTick() output.setNumber(1, input.getNumber(1) * 2) end', '@main.lua');
    vm.io.inputNumbers[0] = 21;
    assert.equal(vm.tick(), 'completed');
    assert.equal(vm.io.outputNumbers[0], 42);
    // 動的 import() で得たクラスも、instanceof の判定にそのまま使えます。
    assert.throws(() => vm.load('local = 1', '@bad.lua'), error => error instanceof EngineError);
  } finally {
    vm.dispose();
  }

  // 描画専用のモジュールも同じ形で読み込めます。
  const { loadRaster } = await import('@stormcat-works/storm-lua-engine/raster');
  const renderer = await loadRaster({ wasmBinary: readFileSync(require.resolve('@stormcat-works/storm-lua-engine/wasm/screen.wasm')) });
  const raster = renderer.createRaster(8, 8);
  try {
    const frame = raster.render([{ kind: 'color', rgba: [0, 255, 0, 255] }, { kind: 'rect', x: 0, y: 0, width: 2, height: 2, fill: true }]);
    assert.deepEqual(Array.from(frame.pixels.subarray(0, 4)), [0, 255, 0, 255]);
  } finally {
    raster.dispose();
  }
  console.log('CommonJS consumer: dynamic import, require.resolve for WASM, runtime tick and raster passed.');
}
main().catch(error => { console.error(error); process.exitCode = 1; });
