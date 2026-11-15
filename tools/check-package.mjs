/** 公開用パッケージの必須ファイル・export・通知を検査する。ビルドや公開は行わない。 */
import {readFile, readdir} from 'node:fs/promises';
import {resolve, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';

const root = fileURLToPath(new URL('../', import.meta.url));
const notices = JSON.parse(await readFile(new URL('./toolchain-licenses.json', import.meta.url), 'utf8'));
export async function checkPackage(directory) {
  const metadata = JSON.parse(await readFile(join(directory, 'package.json'), 'utf8'));
  if (metadata.private || metadata.publishConfig?.access !== 'public' || metadata.publishConfig?.registry !== 'https://registry.npmjs.org/') {
    throw new Error('Package must explicitly target the public npm registry');
  }
  if (!metadata.files?.includes('dist')) throw new Error('Distribution directory excluded from package');
  if (Object.keys(metadata.dependencies ?? {}).length) throw new Error('Unexpected runtime npm dependencies');
  for (const name of ['README.md', 'LICENSE', 'SCREEN_COMPONENTS_LICENSE', 'THIRD_PARTY_LICENSES.txt', ...Object.keys(notices.files)]) {
    if (!metadata.files.includes(name)) throw new Error(`Required file excluded from package: ${name}`);
    const data = await readFile(join(directory, name));
    if (!data.length) throw new Error(`Empty required file: ${name}`);
    if (notices.files[name] && createHash('sha256').update(data).digest('hex') !== notices.files[name]) {
      throw new Error(`Toolchain notice digest differs: ${name}`);
    }
  }
  async function checkExport(value) {
    if (typeof value === 'object' && value !== null) {
      for (const nested of Object.values(value)) await checkExport(nested);
    } else if (typeof value !== 'string' || !value.startsWith('./dist/')) {
      throw new Error(`Unexpected package export: ${value}`);
    } else if (!value.includes('*')) {
      if (!(await readFile(join(directory, value))).length) throw new Error(`Empty export: ${value}`);
    }
  }
  await checkExport(metadata.exports);
  // WASMはホストがディスクから読んで渡すので、置き場所をexportsから解決できる必要がある。
  for (const [subpath, target] of [['./wasm/*', './dist/wasm/*'], ['./compiler-wasm/*', './dist/compiler-wasm/*']]) {
    if (metadata.exports?.[subpath] !== target) throw new Error(`Missing WASM asset export: ${subpath}`);
  }
  for (const name of ['screen.wasm', 'storm_lua_wasm.wasm']) {
    const bytes = await readFile(join(directory, 'dist/wasm', name));
    if (!WebAssembly.validate(bytes)) throw new Error(`Invalid WASM: ${name}`);
  }
  const compiler = await readFile(join(directory, 'dist/compiler-wasm/compiler_bg.wasm'));
  if (!WebAssembly.validate(compiler)) throw new Error('Invalid WASM: compiler_bg.wasm');
  if (!(await readFile(join(directory, 'dist/compiler-wasm/compiler.js'))).length) throw new Error('Missing compiler loader');
  if (!(await readFile(join(directory, 'dist/wasm/storm_lua_wasm.js'))).length) throw new Error('Missing runtime loader');
  const files = await readdir(join(directory, 'dist'), {recursive: true, withFileTypes: true});
  for (const entry of files) {
    if (entry.isSymbolicLink() || (entry.isFile() && !/^(?:.*\.(?:js|d\.ts|wasm)|package\.json)$/.test(entry.name))) {
      throw new Error(`Unexpected distribution entry: ${entry.name}`);
    }
  }
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await checkPackage(join(root, 'packages/lua-engine'));
  console.log('Package exports, WASM binaries, public registry and license files passed');
}
