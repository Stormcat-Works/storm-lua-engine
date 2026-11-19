import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import * as api from '../../dist/index.js';
import {loadCompiler} from '../../dist/compiler.js';
const engine = await api.loadRuntime({wasmBinary: await readFile(new URL('../../dist/wasm/storm_lua_wasm.wasm', import.meta.url))});
const compiler = await loadCompiler({wasmBinary: await readFile(new URL('../../dist/compiler-wasm/compiler_bg.wasm', import.meta.url))});
const text = bytes => new TextDecoder().decode(bytes);

function logicRecorder() {
  const calls = [];
  const logic = {
    getInputLogicSlotBool: slot => { calls.push(['getBool', slot]); return true; },
    getInputLogicSlotFloat: slot => { calls.push(['getFloat', slot]); return 10.25; },
    getInputLogicSlotComposite: slot => {
      calls.push(['getComposite', slot]);
      return {numbers: Array.from({length: 32}, (_, i) => i + 0.5), booleans: Array.from({length: 32}, (_, i) => i % 2 === 0)};
    },
    setOutputLogicSlotBool: (slot, value) => { calls.push(['setBool', slot, value]); },
    setOutputLogicSlotFloat: (slot, value) => { calls.push(['setFloat', slot, value]); },
    setOutputLogicSlotComposite: (slot, value) => { calls.push(['setComposite', slot, value]); },
  };
  return {calls, logic};
}

test('component lifecycle and host providers cross real WASM', () => {
  const {calls, logic} = logicRecorder();
  const renderCalls = [];
  const render = {
    renderMesh0: matrix => { renderCalls.push(['mesh0', [...matrix]]); },
    renderMesh1: matrix => { renderCalls.push(['mesh1', [...matrix]]); },
    renderMesh2: matrix => { renderCalls.push(['mesh2', [...matrix]]); },
    renderLight: (...args) => { renderCalls.push(['light', ...args]); },
    renderLaser: (matrix, a, b) => { renderCalls.push(['laser', [...matrix], a, b]); },
  };
  // 定義データの簡易ストア。キーがあれば読む方向、無ければ書く方向として振る舞います。
  const store = new Map([['hp', 42.5]]);
  const parseCalls = [];
  const parse = {
    parseBool: (key, value) => {
      const name = text(key); parseCalls.push(['bool', name, value]);
      if (name === 'outside') return null;
      if (store.has(name)) return [store.get(name), true];
      store.set(name, value); return [value, true];
    },
    parseNumber: (key, value) => {
      const name = text(key); parseCalls.push(['number', name, value]);
      if (name === 'outside') return null;
      if (store.has(name)) return [store.get(name), true];
      store.set(name, value); return [value, true];
    },
    parseString: (key, value) => {
      const name = text(key); parseCalls.push(['string', name, text(value)]);
      if (name === 'outside') return null;
      if (store.has(name)) return [new TextEncoder().encode(store.get(name)), true];
      store.set(name, text(value)); return [value, true];
    },
  };
  const vm = engine.createComponent({providers: {logic, render, parse}});
  try {
    assert.equal(vm.mode, 'component');
    assert.deepEqual(vm.status(), {status: 'ok', code: 0});
    // 順序外の呼び出しは明示的に失敗します。
    assert.throws(() => vm.tick(), error => error instanceof api.EngineError && error.code === 4);
    vm.load(`
function onParse()
  local hp, ok = parser.parseNumber("hp", 3)
  component.setOutputLogicSlotFloat(1, hp)
  parser.parseBool("enabled", true)
  parser.parseString("label", "abc")
  local v, ok2 = parser.parseNumber("outside", 1)
  component.setOutputLogicSlotBool(9, v == false and ok2 == nil)
end
function onAddToSimulation()
  component.setOutputLogicSlotBool(1, component.getInputLogicSlotBool(2))
end
function onTick(dt)
  component.setOutputLogicSlotFloat(2, component.getInputLogicSlotFloat(3) + dt)
  local comp = component.getInputLogicSlotComposite(4)
  component.setOutputLogicSlotFloat(3, comp.number_values[5])
  component.setOutputLogicSlotComposite(5, {number_values = {[1] = 7.5}, bool_values = {[2] = true}})
  component.renderMesh0(matrix.identity())
end
function onRender()
  component.renderLight(1, 2, 3, 4, 5, 6, 7, true, false)
end
function onRemoveFromSimulation()
  component.setOutputLogicSlotBool(2, false)
end
`, 'test_component.lua');
    assert.equal(vm.parse(), 'completed');
    assert.equal(vm.addToSimulation(), 'completed');
    assert.equal(vm.tick(5), 'completed');
    assert.equal(vm.render(), 'completed');
    assert.equal(vm.removeFromSimulation(), 'completed');
    assert.deepEqual(vm.status(), {status: 'ok', code: 0});
  } finally { vm.dispose(); }
  // parser: 読み（hp は定義値 42.5 が勝つ）・書き（enabled / label が記録される）・期間外（null → false 1 個）
  assert.deepEqual(parseCalls[0], ['number', 'hp', 3]);
  assert.deepEqual(parseCalls[1], ['bool', 'enabled', true]);
  assert.deepEqual(parseCalls[2], ['string', 'label', 'abc']);
  assert.deepEqual(parseCalls[3], ['number', 'outside', 1]);
  assert.equal(store.get('enabled'), true);
  assert.equal(store.get('label'), 'abc');
  const setFloat = calls.filter(call => call[0] === 'setFloat');
  assert.deepEqual(setFloat[0], ['setFloat', 1n, 42.5]);
  assert.deepEqual(setFloat[1], ['setFloat', 2n, 15.25]);
  assert.deepEqual(setFloat[2], ['setFloat', 3n, 4.5]);
  const setBool = calls.filter(call => call[0] === 'setBool');
  assert.deepEqual(setBool[0], ['setBool', 9n, true]);
  assert.deepEqual(setBool[1], ['setBool', 1n, true]);
  assert.deepEqual(setBool[2], ['setBool', 2n, false]);
  const composite = calls.find(call => call[0] === 'setComposite');
  assert.equal(composite[1], 5n);
  assert.equal(composite[2].numbers.length, 32);
  assert.equal(composite[2].numbers[0], 7.5);
  assert.equal(composite[2].numbers[1], 0);
  assert.equal(composite[2].booleans[1], true);
  assert.equal(composite[2].booleans[0], false);
  // render: matrix は16数値の配列として届き、renderLight の引数は並びのまま届きます。
  assert.equal(renderCalls[0][0], 'mesh0');
  assert.deepEqual(renderCalls[0][1], [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
  assert.deepEqual(renderCalls[1], ['light', 1, 2, 3, 4, 5, 6, 7, true, false]);
});

test('component error status gates tick but not removal; missing providers fail explicitly', () => {
  const vm = engine.createComponent({providers: {logic: logicRecorder().logic}});
  try {
    vm.load('function onTick() error("boom") end\nfunction onRemoveFromSimulation() end');
    vm.parse();
    vm.addToSimulation();
    assert.throws(() => vm.tick(), error => error instanceof api.EngineError && error.code === 1);
    assert.deepEqual(vm.status(), {status: 'tickFailed', code: 2});
    // ステータスゲート: 失敗後は onTick を実行せず missing を返します。
    assert.equal(vm.tick(), 'missing');
    // ゲートの無い remove は失敗後でも配送されます。
    assert.equal(vm.removeFromSimulation(), 'completed');
  } finally { vm.dispose(); }
  const bare = engine.createComponent();
  try {
    bare.load('function onTick() component.getInputLogicSlotBool(1) end');
    bare.parse();
    bare.addToSimulation();
    // 未提供ドメインの関数は登録されないため、呼び出しは Lua エラーになります（成功 stub ではありません）。
    assert.throws(() => bare.tick(), error => error instanceof api.EngineError && error.code === 1);
    assert.deepEqual(bare.status(), {status: 'tickFailed', code: 2});
  } finally { bare.dispose(); }
  const failing = engine.createComponent({providers: {logic: {
    getInputLogicSlotBool: () => { throw new Error('host down'); },
    getInputLogicSlotFloat: () => 0,
    getInputLogicSlotComposite: () => ({numbers: new Array(32).fill(0), booleans: new Array(32).fill(false)}),
    setOutputLogicSlotBool: () => {},
    setOutputLogicSlotFloat: () => {},
    setOutputLogicSlotComposite: () => {},
  }}});
  try {
    failing.load('function onTick() component.getInputLogicSlotBool(1) end');
    failing.parse();
    failing.addToSimulation();
    // ホスト例外は code 10（host）として表面化します。
    assert.throws(() => failing.tick(), error => error instanceof api.EngineError && error.code === 10);
  } finally { failing.dispose(); }
});

test('component catalogs are generated and buildComponent links without optimizing', () => {
  assert.equal(api.COMPONENT_API_CATALOG.profile, 'component');
  assert.equal(api.COMPONENT_API_CATALOG.functions.length, 64);
  assert.deepEqual(api.ENVIRONMENT_CATALOG.componentCallbacks, ['onParse', 'onAddToSimulation', 'onTick', 'onRender', 'onRemoveFromSimulation']);
  assert.ok(api.ENVIRONMENT_CATALOG.componentRoots.includes('component'));
  const project = source => ({entry: 'main', modules: {main: source}});
  const built = compiler.buildComponent(project('function onParse() parser.parseBool("a", true) end\nfunction onTick(dt) component.setOutputLogicSlotBool(1, true) end'));
  assert.equal(built.ok, true, JSON.stringify(built.diagnostics));
  assert.ok(built.code.includes('onTick'));
  // Game 環境に無い base 関数は addon と同じくエラーになります。
  assert.equal(compiler.buildComponent(project('pcall(function() end)')).ok, false);
  assert.equal(compiler.buildComponent(project('pcall(function() end)'), {environment: 'extended'}).ok, true);
  // 未知のオプションキーは明示的に拒否されます。
  assert.throws(() => compiler.buildComponent(project(''), {minify: true}));
});

test('component WASM rejects requireLoader and classifies singular matrix.invert as tickFailed', async () => {
  const {Bridge, adaptModule} = await import(new URL('../../dist/bridge.js', import.meta.url));
  // High-level TS wrapper rejects requireLoader with TypeError:
  assert.throws(
    () => engine.createComponent({environment: 'extended', requireLoader: () => ({name: '@a.lua', source: new Uint8Array()})}),
    TypeError,
  );
  // Raw WASM C ABI (`_sle_new_component`) also rejects `"requireLoader": true` with Status::Unsupported (code 6)
  // and non-boolean `"requireLoader"` with Status::InvalidArgument (code 4), rather than silently ignoring it (BUG-T1-08):
  const rawFactory = (await import(new URL('../../dist/wasm/storm_lua_wasm.js', import.meta.url))).default;
  const rawMod = await rawFactory({wasmBinary: await readFile(new URL('../../dist/wasm/storm_lua_wasm.wasm', import.meta.url))});
  const bridge = new Bridge(adaptModule(rawMod, 'sle', true));
  const callRawNewComponent = payload => {
    const bytes = new TextEncoder().encode(JSON.stringify(payload));
    return bridge.upload(bytes, (ptr, len) => bridge.query('new_component', 100_000, 16 * 1024 * 1024, 0, ptr, len));
  };
  assert.throws(
    () => callRawNewComponent({environment: 'extended', requireLoader: true, bindings: {values: {}, functions: []}, providers: []}),
    error => error instanceof api.EngineError && error.code === api.STATUS.unsupported,
  );
  assert.throws(
    () => callRawNewComponent({environment: 'extended', requireLoader: 'yes', bindings: {values: {}, functions: []}, providers: []}),
    error => error instanceof api.EngineError && error.code === api.STATUS.invalidArgument,
  );

  // BUG-T1-07: Singular/malformed matrix errors are Lua runtime errors (status 2), not Stage::Failed (status 0).
  const vm = engine.createComponent();
  try {
    vm.load('function onTick() matrix.invert({1,0,0,0, 0,0,0,0, 0,0,1,0, 0,0,0,1}) end\nfunction onRemoveFromSimulation() end');
    assert.equal(vm.parse(), 'missing');
    assert.equal(vm.addToSimulation(), 'missing');
    assert.throws(() => vm.tick(1), error => error instanceof api.EngineError && error.code === 1);
    assert.deepEqual(vm.status(), {status: 'tickFailed', code: 2});
    assert.equal(vm.removeFromSimulation(), 'completed');
  } finally {
    vm.dispose();
  }
});
