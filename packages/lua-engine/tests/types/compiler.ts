import { loadCompiler, type Compiler, type OptimizationMap, type LuaProject } from '../../src/compiler.js';

const project: LuaProject = { entry: 'main', modules: { main: 'function onTick()end' } };
function consumer(compiler: Compiler): void {
  compiler.minify('function onTick()end', { target: 'vehicle', numericMode: 'exact' });
  const generated = compiler.minify('function onTick()end', {sourceMap: true, sourceName: 'main.lua'});
  if (generated.code !== undefined && generated.map !== undefined) {
    const details: OptimizationMap = compiler.validateSourceMap(generated.code, generated.map);
    const schema: 1 = details.schemaVersion;
    const role = details.relations[0]?.role;
    void schema; void role;
  }
  compiler.build(project, { target: 'vehicle', minify: false, sourceMap: true });
  compiler.analyze(project, { target: 'vehicle' });
  const ids: string[] = compiler.passIds();
  void ids;
  // @ts-expect-error Addon optimization is not a supported compiler profile yet.
  compiler.minify('function onTick()end', { target: 'addon' });
  // @ts-expect-error Compiler instances do not expose VM state or scheduling.
  compiler.tick();
}
void consumer;
void loadCompiler({ wasmBinary: new Uint8Array() });
