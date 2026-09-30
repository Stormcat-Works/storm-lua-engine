/** Check an already-tested release tarball. Never runs package scripts or writes to npm. */
import assert from 'node:assert/strict';
import {readFile, writeFile, mkdir, appendFile} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {execFile as executeFile} from 'node:child_process';
import {promisify} from 'node:util';
import {resolve, join} from 'node:path';
import {pathToFileURL} from 'node:url';
const execFile = promisify(executeFile);
export const repository = 'Stormcat-Works/storm-lua-engine';
export const packageName = '@stormcat-works/storm-lua-engine';
export function releaseVersion(tag) {
  assert.match(tag, /^v(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/, 'Only an explicit stable vX.Y.Z release is publishable');
  return tag.slice(1);
}
export function checksums(text) {
  const result = new Map();
  for (const line of text.trim().split(/\r?\n/)) {
    const match = /^([0-9a-f]{64})  ([A-Za-z0-9][A-Za-z0-9._-]*)$/.exec(line);
    assert.ok(match, 'Invalid checksum line or unsafe asset name');
    assert.ok(!result.has(match[2]), 'Duplicate checksum entry');
    result.set(match[2], match[1]);
  }
  return result;
}
export function validateReceipt(receipt, version, revision, filename, digest) {
  assert.equal(receipt.version, version, 'Release receipt version mismatch');
  assert.equal(receipt.sourceRevision, revision, 'Release receipt is for another commit');
  assert.equal(receipt.assets?.[filename], digest, 'Release receipt tarball hash mismatch');
}
export function validatePackage(metadata, version) {
  assert.equal(metadata.name, packageName, 'Wrong npm package');
  assert.equal(metadata.version, version, 'Tarball version does not match tag');
  assert.ok(!metadata.private, 'A private package must not be published');
  assert.equal(metadata.repository?.url, `git+https://github.com/${repository}.git`, 'Wrong repository');
  assert.equal(metadata.publishConfig?.registry?.replace(/\/$/, ''), 'https://registry.npmjs.org', 'Wrong registry');
}
export function validateArchiveNames(names) {
  assert.ok(names.length > 0, 'Empty npm archive');
  for (const name of names) assert.ok(name.startsWith('package/') && !name.split('/').includes('..') && !name.includes('\\'), 'Unsafe archive path');
  assert.equal(names.filter(name => name === 'package/package.json').length, 1, 'Ambiguous package metadata');
}
export function assertNotOlder(version, latest) {
  const left = version.split('.').map(Number);
  const right = latest.split('.').map(Number);
  assert.equal(left.length, 3);
  assert.equal(right.length, 3);
  for (let i = 0; i < 3; i++) {
    assert.ok(Number.isInteger(left[i]) && Number.isInteger(right[i]));
    if (left[i] !== right[i]) { assert.ok(left[i] > right[i], 'Refusing to move latest backwards'); return; }
  }
}
export function validateRegistry(metadata, state) {
  assert.equal(metadata.name, packageName, 'Wrong registry package');
  assert.equal(metadata.version, state.version, 'Wrong registry version');
  assert.equal(metadata.dist?.integrity, state.integrity, 'Published version has different bytes; never replace it');
}
export function validateJobs(jobs) {
  for (const name of ['native (ubuntu-latest)', 'native (windows-latest)', 'native (macos-latest)', 'wasm']) {
    assert.ok(jobs.some(job => job.name === name && job.conclusion === 'success'), `Missing successful release gate: ${name}`);
  }
}
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const integrity = bytes => `sha512-${createHash('sha512').update(bytes).digest('base64')}`;
async function gh(path) {
  return JSON.parse((await execFile('gh', ['api', path], {maxBuffer: 16 * 1024 * 1024})).stdout);
}
async function registryMetadata(version) {
  const response = await fetch(`https://registry.npmjs.org/${encodeURIComponent(packageName)}/${encodeURIComponent(version)}`, {signal: AbortSignal.timeout(30000)});
  if (response.status === 404) return null;
  assert.ok(response.ok, `Registry read failed: HTTP ${response.status}`);
  return response.json();
}
async function verifyLocal(directory) {
  const state = JSON.parse(await readFile(join(directory, 'publication.json'), 'utf8'));
  assert.equal(state.version, releaseVersion(state.tag));
  assert.equal(state.filename, `stormcat-works-storm-lua-engine-${state.version}.tgz`);
  const bytes = await readFile(join(directory, state.filename));
  assert.equal(sha256(bytes), state.sha256, 'Artifact changed after verification');
  assert.equal(integrity(bytes), state.integrity);
  return state;
}
async function outputs(values) {
  if (process.env.GITHUB_OUTPUT) await appendFile(process.env.GITHUB_OUTPUT, Object.entries(values).map(([key, value]) => `${key}=${value}\n`).join(''));
  console.log(JSON.stringify(values));
}
async function prepare(tag, directory) {
  const version = releaseVersion(tag);
  assert.equal(process.env.GITHUB_REPOSITORY ?? repository, repository);
  const release = await gh(`repos/${repository}/releases/tags/${tag}`);
  assert.equal(release.tag_name, tag);
  assert.equal(release.draft, false, 'Publish the reviewed GitHub Release first');
  assert.equal(release.prerelease, false);
  let object = (await gh(`repos/${repository}/git/ref/tags/${tag}`)).object;
  for (let i = 0; object.type === 'tag' && i < 4; i++) object = (await gh(`repos/${repository}/git/tags/${object.sha}`)).object;
  assert.equal(object.type, 'commit', 'Unresolvable release tag');
  const revision = object.sha;
  const comparison = await gh(`repos/${repository}/compare/${revision}...main`);
  assert.ok(['ahead', 'identical'].includes(comparison.status), 'Release commit must be on main history');
  const runs = await gh(`repos/${repository}/actions/workflows/ci.yml/runs?head_sha=${revision}&status=success&per_page=100`);
  const run = runs.workflow_runs.find(value => value.head_sha === revision && value.conclusion === 'success' && ['push', 'workflow_dispatch'].includes(value.event));
  assert.ok(run, 'No successful canonical CI for this exact commit');
  validateJobs((await gh(`repos/${repository}/actions/runs/${run.id}/jobs?per_page=100`)).jobs);
  const filename = `stormcat-works-storm-lua-engine-${version}.tgz`;
  for (const name of [filename, 'SHA256SUMS', 'release-verification.json']) {
    const found = release.assets.filter(asset => asset.name === name && asset.state === 'uploaded');
    assert.equal(found.length, 1, `Missing or ambiguous asset: ${name}`);
    assert.ok(found[0].size <= 32 * 1024 * 1024, 'Asset exceeds the checked package limit');
  }
  await mkdir(directory, {recursive: true});
  await execFile('gh', ['release', 'download', tag, '--repo', repository, '--dir', directory, '--pattern', filename, '--pattern', 'SHA256SUMS', '--pattern', 'release-verification.json']);
  const hashes = checksums(await readFile(join(directory, 'SHA256SUMS'), 'utf8'));
  const tarball = await readFile(join(directory, filename));
  const digest = sha256(tarball);
  assert.equal(hashes.get(filename), digest, 'Release checksum mismatch');
  const receiptBytes = await readFile(join(directory, 'release-verification.json'));
  assert.equal(hashes.get('release-verification.json'), sha256(receiptBytes), 'Receipt checksum mismatch');
  validateReceipt(JSON.parse(receiptBytes), version, revision, filename, digest);
  const archive = join(directory, filename);
  validateArchiveNames((await execFile('tar', ['-tzf', archive], {maxBuffer: 8 * 1024 * 1024})).stdout.trim().split('\n'));
  const types = (await execFile('tar', ['-tvzf', archive], {maxBuffer: 8 * 1024 * 1024})).stdout.trim().split('\n');
  assert.ok(types.every(line => ['-', 'd'].includes(line[0])), 'Archive links/devices are not permitted');
  validatePackage(JSON.parse((await execFile('tar', ['-xOzf', archive, 'package/package.json'])).stdout), version);
  const state = {tag, version, revision, filename, sha256: digest, integrity: integrity(tarball), ciRun: run.id};
  const existing = await registryMetadata(version);
  if (existing) validateRegistry(existing, state);
  else {
    const latest = await registryMetadata('latest');
    if (latest) assertNotOlder(version, latest.version);
  }
  await writeFile(join(directory, 'publication.json'), `${JSON.stringify(state, null, 2)}\n`);
  // Old-tag bootstrap must not attest that today's workflow commit is its source.
  await outputs({version, revision, filename, exists: String(existing !== null), provenance: String(process.env.GITHUB_SHA === revision)});
}
async function main() {
  const [command, ...args] = process.argv.slice(2);
  if (command === 'prepare' && args.length === 2) return prepare(args[0], resolve(args[1]));
  if (command === 'check' && args.length === 1) return outputs(await verifyLocal(resolve(args[0])));
  if (command === 'verify-registry' && args.length === 1) {
    const directory = resolve(args[0]);
    const state = await verifyLocal(directory);
    let metadata;
    for (let i = 0; i < 6; i++) {
      metadata = await registryMetadata(state.version);
      if (metadata) break;
      await new Promise(resolve => setTimeout(resolve, 5000));
    }
    assert.ok(metadata, 'Version not found after publication');
    validateRegistry(metadata, state);
    const latest = await registryMetadata('latest');
    assert.ok(latest, 'Registry latest tag is missing');
    assertNotOlder(latest.version, state.version);
    const url = new URL(metadata.dist.tarball);
    assert.equal(url.origin, 'https://registry.npmjs.org');
    const response = await fetch(url, {signal: AbortSignal.timeout(30000)});
    assert.ok(response.ok, `Registry download failed: HTTP ${response.status}`);
    const bytes = Buffer.from(await response.arrayBuffer());
    assert.equal(sha256(bytes), state.sha256, 'Registry tarball is not byte-identical');
    const path = join(directory, `registry-${state.filename}`);
    await writeFile(path, bytes);
    await outputs({version: state.version, registryTarball: path, latest: latest.version, verified: true});
    return;
  }
  throw new Error('Usage: verify-publication.mjs prepare TAG DIRECTORY | check DIRECTORY | verify-registry DIRECTORY');
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) await main();
