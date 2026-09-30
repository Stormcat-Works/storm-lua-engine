import test from 'node:test';
import assert from 'node:assert/strict';
import {releaseVersion, assertNotOlder, checksums, validateReceipt, validatePackage, validateArchiveNames, validateRegistry, validateJobs, packageName, repository} from './verify-publication.mjs';
const hash = 'a'.repeat(64);
test('release tags reject commands, traversal, leading zeroes and pre-releases', () => {
  assert.equal(releaseVersion('v0.2.1'), '0.2.1');
  for (const value of ['', 'main', 'v01.2.3', 'v0.3.0-rc.1', 'v0.2.1\nrun', '../../main', 'v0.2.1;echo x']) assert.throws(() => releaseVersion(value));
});
test('checksums have unique safe filenames and exact digests', () => {
  assert.equal(checksums(`${hash}  sdk.tgz\n`).get('sdk.tgz'), hash);
  for (const value of [`${hash}  ../sdk.tgz`, `${hash}  /sdk.tgz`, `${hash}  a\n${hash}  a`, `bad  sdk.tgz`, `${hash} sdk.tgz`]) assert.throws(() => checksums(value));
});
test('receipt binds version, source commit and bytes', () => {
  const receipt = {version: '0.2.1', sourceRevision: 'b'.repeat(40), assets: {'sdk.tgz': hash}};
  validateReceipt(receipt, '0.2.1', 'b'.repeat(40), 'sdk.tgz', hash);
  for (const field of ['version', 'sourceRevision', 'assets']) assert.throws(() => validateReceipt({...receipt, [field]: 'bad'}, '0.2.1', 'b'.repeat(40), 'sdk.tgz', hash));
});
test('archive names reject traversal and duplicate package descriptors', () => {
  validateArchiveNames(['package/package.json', 'package/dist/compiler.js']);
  for (const names of [[], ['package/../outside'], ['/package/package.json'], ['package/package.json', 'package/package.json'], ['package/package.json', 'package/..\\outside']]) assert.throws(() => validateArchiveNames(names));
});
test('package identity and registry are checked before any publish', () => {
  const metadata = {name: packageName, version: '0.2.1', repository: {url: `git+https://github.com/${repository}.git`}, publishConfig: {registry: 'https://registry.npmjs.org/'}};
  validatePackage(metadata, '0.2.1');
  for (const patch of [{name: 'another-package'}, {version: '0.2.0'}, {private: true}, {repository: {url: 'https://example.com'}}, {publishConfig: {registry: 'https://npm.pkg.github.com'}}]) assert.throws(() => validatePackage({...metadata, ...patch}, '0.2.1'));
});
test('an existing version is idempotent only when integrity is identical', () => {
  const state = {version: '0.2.1', integrity: 'sha512-expected'};
  const metadata = {name: packageName, version: '0.2.1', dist: {integrity: state.integrity}};
  validateRegistry(metadata, state);
  assert.throws(() => validateRegistry({...metadata, dist: {integrity: 'sha512-different'}}, state));
  assert.throws(() => validateRegistry({...metadata, version: '0.2.2'}, state));
});
test('all release platform gates must really succeed', () => {
  const jobs = ['native (ubuntu-latest)', 'native (windows-latest)', 'native (macos-latest)', 'wasm'].map(name => ({name, conclusion: 'success'}));
  validateJobs(jobs);
  assert.throws(() => validateJobs(jobs.slice(1)));
  assert.throws(() => validateJobs(jobs.map(job => ({...job, conclusion: 'skipped'}))));
});

test('automatic stable latest cannot move backwards', () => {
  assertNotOlder('0.2.1', '0.2.0');
  assertNotOlder('0.3.0', '0.2.99');
  assertNotOlder('0.2.1', '0.2.1');
  assert.throws(() => assertNotOlder('0.2.1', '0.3.0'));
});

test('post-publication latest must include this release or a newer stable release', () => {
  assertNotOlder('0.2.1', '0.2.1');
  assertNotOlder('0.3.0', '0.2.1');
  assert.throws(() => assertNotOlder('0.2.0', '0.2.1'));
});
