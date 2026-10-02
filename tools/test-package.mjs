/** パックされたライブラリを隔離された一時コンシューマにインストールし、両方のWASMパスを実行します。 */
import {mkdtemp,readFile,writeFile,rm,mkdir} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawnSync} from 'node:child_process';
import {checkArtifacts} from './check-artifacts.mjs';
import {checkPackage} from './check-package.mjs';
const root=fileURLToPath(new URL('../',import.meta.url));
const args=process.argv.slice(2);
if(args.length>1||args[0]?.startsWith('-'))throw new Error('usage: node tools/test-package.mjs [tarball]');
const provided=args[0]?resolve(args[0]):null;
const temporary=await mkdtemp(join(tmpdir(),'storm-lua-package-'));
const npm=process.platform==='win32'?'npm.cmd':'npm';
function run(program,args,cwd){
  const result=spawnSync(program,args,{cwd,encoding:'utf8',shell:process.platform==='win32'&&program===npm});
  if(result.error)throw result.error;
  if(result.status!==0)throw new Error(`${program} failed: ${result.stderr}\n${result.stdout}`);
  return result.stdout;
}
try{
  const packed=provided?null:JSON.parse(run(npm,['pack','--ignore-scripts','--json','--pack-destination',temporary],join(root,'packages/lua-engine')))[0];
  await writeFile(join(temporary,'package.json'),JSON.stringify({name:'isolated-engine-consumer',private:true,type:'module'}));
  run(npm,['install','--offline','--ignore-scripts','--no-audit','--no-fund',provided??join(temporary,packed.filename)],temporary);
  const installed=join(temporary,'node_modules/@stormcat-works/storm-lua-engine');
  await checkArtifacts([join(installed,'dist')]);
  await checkPackage(installed);
  await writeFile(join(temporary,'smoke.mjs'),`
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {loadRuntime} from '@stormcat-works/storm-lua-engine';
import {loadRaster} from '@stormcat-works/storm-lua-engine/raster';
import {CanvasPresenter} from '@stormcat-works/storm-lua-engine/canvas';
assert.equal(typeof CanvasPresenter,'function');
const binary=await readFile(new URL(import.meta.resolve('@stormcat-works/storm-lua-engine/wasm/storm_lua_wasm.wasm')));
const engine=await loadRuntime({wasmBinary:binary});const vm=engine.createVehicle();
try{vm.load('function onTick() output.setNumber(1,input.getNumber(1)*2) end');vm.io.inputNumbers[0]=3.5;vm.tick();assert.equal(vm.io.outputNumbers[0],7);}finally{vm.dispose();}
const renderer=await loadRaster({wasmBinary:await readFile(new URL(import.meta.resolve('@stormcat-works/storm-lua-engine/wasm/screen.wasm')))});
const raster=renderer.createRaster(8,8);
try{const frame=raster.render([{kind:'color',rgba:[255,0,0,128]},{kind:'rect',x:1,y:1,width:3,height:3,fill:true}]);assert.deepEqual(Array.from(frame.pixels.subarray(36,40)),[128,0,0,64]);}finally{raster.dispose();}
console.log('Isolated installed package: Lua execution, raster, export paths and optional Canvas import passed.');
`);
  console.log(run(process.execPath,['smoke.mjs'],temporary).trim());
  await writeFile(join(temporary,'consumer.mjs'),await readFile(join(root,'examples/consumer/node.mjs')));
  console.log(run(process.execPath,['consumer.mjs'],temporary).trim());
  await writeFile(join(temporary,'source-loading.mjs'),await readFile(join(root,'examples/consumer/source-loading.mjs')));
  console.log(run(process.execPath,['source-loading.mjs'],temporary).trim());
  await writeFile(join(temporary,'log-locations.mjs'),await readFile(join(root,'examples/consumer/log-locations.mjs')));
  console.log(run(process.execPath,['log-locations.mjs'],temporary).trim());
  // Source-map decoding belongs to this consumer, not the runtime-only SDK package.
  // npm ci caches integrity-addressed tarballs, not necessarily registry metadata.
  // Reuse the exact test-only lock entries in a child consumer; the tested SDK is
  // still resolved from the separately installed parent node_modules directory.
  const lock = JSON.parse(await readFile(join(root,'packages/lua-engine/package-lock.json'),'utf8'));
  const names = ['@jridgewell/trace-mapping','@jridgewell/resolve-uri','@jridgewell/sourcemap-codec'];
  const mapped = join(temporary,'mapped-consumer');
  await mkdir(mapped);
  const manifest = {name:'mapped-debug-consumer',private:true,type:'module',dependencies:{'@jridgewell/trace-mapping':lock.packages['node_modules/@jridgewell/trace-mapping'].version}};
  const packages = {'':{name:manifest.name,dependencies:manifest.dependencies}};
  for (const name of names) {
    const entry = lock.packages[`node_modules/${name}`];
    if (!entry?.resolved || !entry.integrity) throw new Error(`Missing locked consumer dependency: ${name}`);
    const {dev, ...installedEntry} = entry;
    packages[`node_modules/${name}`] = installedEntry;
  }
  await writeFile(join(mapped,'package.json'),JSON.stringify(manifest));
  await writeFile(join(mapped,'package-lock.json'),JSON.stringify({name:manifest.name,lockfileVersion:3,requires:true,packages}));
  run(npm,['ci','--offline','--ignore-scripts','--no-audit','--no-fund'],mapped);
  await writeFile(join(mapped,'source-map.mjs'),await readFile(join(root,'examples/consumer/source-map.mjs')));
  console.log(run(process.execPath,['source-map.mjs'],mapped).trim());
  await writeFile(join(mapped,'optimized-source-map.mjs'),await readFile(join(root,'examples/consumer/optimized-source-map.mjs')));
  console.log(run(process.execPath,['optimized-source-map.mjs'],mapped).trim());
  await writeFile(join(mapped,'optimized-source-map.mjs'),await readFile(join(root,'examples/consumer/optimized-source-map.mjs')));
  console.log(run(process.execPath,['optimized-source-map.mjs'],mapped).trim());
  console.log(`${provided?'Provided release tarball':`Packed ${packed.files.length} files`}; SDK has no runtime npm dependencies; mapped-debug consumer separately installs trace-mapping.`);
}finally{await rm(temporary,{recursive:true,force:true});}
