import {test} from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp, mkdir, readFile, writeFile, copyFile, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join, dirname} from 'node:path';
import {fileURLToPath} from 'node:url';
import {checkPackage} from '../../../tools/check-package.mjs';

const source = fileURLToPath(new URL('../', import.meta.url));
async function fixture(t) {
  const directory = await mkdtemp(join(tmpdir(), 'engine-package-test-'));
  t.after(() => rm(directory, {recursive: true, force: true}));
  const metadata = JSON.parse(await readFile(join(source, 'package.json'), 'utf8'));
  await writeFile(join(directory, 'package.json'), JSON.stringify(metadata));
  for (const name of metadata.files.filter(name => name !== 'dist')) await copyFile(join(source, name), join(directory, name));
  const paths = JSON.stringify(metadata.exports).match(/\.\/dist\/[^"*]+/g);
  for (const path of paths.filter(path => !path.endsWith('/'))) {
    await mkdir(dirname(join(directory, path)), {recursive: true});
    await writeFile(join(directory, path), '// Test fixture\n');
  }
  await mkdir(join(directory, 'dist/compiler-wasm'), {recursive: true});
  await writeFile(join(directory, 'dist/compiler-wasm/compiler_bg.wasm'), Buffer.from([0,97,115,109,1,0,0,0]));
  await writeFile(join(directory, 'dist/compiler-wasm/compiler.js'), '// Compiler fixture\n');
  await mkdir(join(directory, 'dist/wasm'), {recursive: true});
  for (const name of ['screen.wasm', 'storm_lua_wasm.wasm']) {
    await writeFile(join(directory, 'dist/wasm', name), Buffer.from([0,97,115,109,1,0,0,0]));
  }
  await writeFile(join(directory, 'dist/wasm/storm_lua_wasm.js'), '// Test fixture\n');
  return directory;
}

test('a complete distributable satisfies the package gate', async t => {
  await checkPackage(await fixture(t));
});
test('a TS-only build cannot be packed without WASM', async t => {
  const directory = await fixture(t);
  await rm(join(directory, 'dist/wasm/screen.wasm'));
  await assert.rejects(checkPackage(directory), /ENOENT/);
});
test('truncated WASM and altered toolchain notices are rejected', async t => {
  const directory = await fixture(t);
  await writeFile(join(directory, 'dist/wasm/screen.wasm'), 'broken');
  await assert.rejects(checkPackage(directory), /Invalid WASM/);
  await writeFile(join(directory, 'TOOLCHAIN_LICENSES.txt'), 'truncated');
  await assert.rejects(checkPackage(directory), /notice digest differs/);
});
test('excluded notices and stray artifacts cannot pass packaging', async t => {
  const directory = await fixture(t);
  await writeFile(join(directory, 'dist/source.map'), '{}');
  await assert.rejects(checkPackage(directory), /Unexpected distribution entry/);
  const metadata = JSON.parse(await readFile(join(directory, 'package.json'), 'utf8'));
  metadata.files = metadata.files.filter(name => name !== 'TOOLCHAIN_LICENSES.txt');
  await writeFile(join(directory, 'package.json'), JSON.stringify(metadata));
  await assert.rejects(checkPackage(directory), /Required file excluded/);
});

test('a compiler export cannot be distributed without its WASM payload', async t => {
  const directory = await fixture(t);
  await rm(join(directory, 'dist/compiler-wasm/compiler_bg.wasm'));
  await assert.rejects(checkPackage(directory), /ENOENT/);
});

test('WASM asset directories must stay resolvable through package exports', async t => {
  for (const subpath of ['./wasm/*', './compiler-wasm/*']) {
    const directory = await fixture(t);
    const metadata = JSON.parse(await readFile(join(directory, 'package.json'), 'utf8'));
    assert.equal(metadata.exports[subpath], subpath.replace('./', './dist/'));
    delete metadata.exports[subpath];
    await writeFile(join(directory, 'package.json'), JSON.stringify(metadata));
    await assert.rejects(checkPackage(directory), /Missing WASM asset export/);
  }
});
