'use strict';
const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');

const PACKAGE_FILES = [
  'LICENSE.md', 'README.md', 'client-internal.cjs', 'client.cjs', 'client.d.ts', 'client.mjs', 'package.json',
  'request-journal.cjs', 'values.cjs', 'values.d.ts', 'values.mjs',
];
const supplied = process.env.ROMEDB_CLIENT_TARBALL;
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'romedb-client-test-'));
test.after(() => fs.rmSync(root, { recursive: true, force: true }));
const consumer = path.join(root, 'consumer'), installed = path.join(consumer, 'node_modules/@romedb/client');
fs.mkdirSync(path.dirname(installed), { recursive: true });
if (supplied) {
  fs.mkdirSync(installed);
  execFileSync('tar', ['-xzf', supplied, '-C', installed, '--strip-components=1']);
} else fs.symlinkSync(__dirname, installed);
fs.writeFileSync(path.join(consumer, 'package.json'), JSON.stringify({ name: 'consumer', private: true }));
const runInConsumer = (name, source) => {
  fs.writeFileSync(path.join(consumer, name), source);
  return execFileSync(process.execPath, [name], { cwd: consumer, encoding: 'utf8', env: { ...process.env, NODE_PATH: '' } });
};
const fakeService = `
const details = { id: 'local-test', sequence: 3, serviceVersion: 1, capabilities: { readVersion: 2, atomicWrites: true, replayWindow: 64, exactSequenceCondition: true }, limits: { maxRequestBytes: 65536 } };
const fetch = async () => Response.json(details);
`;

test('the package holds the client and no native code', () => {
  const [report] = JSON.parse(execFileSync('npm', ['pack', '--dry-run', '--json', '--ignore-scripts'], { cwd: __dirname, encoding: 'utf8' }));
  assert.deepEqual(report.files.map(file => file.path).sort(), PACKAGE_FILES);
  assert.ok(report.size < 64 * 1024, `the package is ${report.size} bytes`);
});

test('a supplied package holds the files of this source', { skip: !supplied }, () => {
  assert.deepEqual(fs.readdirSync(installed).sort(), PACKAGE_FILES);
  const readManifest = directory => JSON.parse(fs.readFileSync(path.join(directory, 'package.json'), 'utf8'));
  assert.deepEqual(readManifest(installed), readManifest(__dirname));
  for (const name of PACKAGE_FILES.filter(file => file !== 'package.json')) {
    assert.ok(fs.readFileSync(path.join(installed, name)).equals(fs.readFileSync(path.join(__dirname, name))), name);
  }
});

test('the version and the identity match the release, and @romedb/node asks for this version', () => {
  const manifest = JSON.parse(fs.readFileSync(path.join(installed, 'package.json'), 'utf8'));
  const release = JSON.parse(fs.readFileSync(path.join(__dirname, '../../release.json'), 'utf8'));
  const node = JSON.parse(fs.readFileSync(path.join(__dirname, '../node/package.json'), 'utf8'));
  assert.equal(manifest.name, '@romedb/client');
  assert.equal(manifest.version, release.version);
  assert.equal(manifest.private, undefined);
  assert.equal(manifest.repository.url, 'git+https://github.com/romedb/romedb.git');
  assert.equal(node.dependencies['@romedb/client'], release.version);
});

test('CommonJS loads the package by name and connects', () => {
  const output = runInConsumer('load.cjs', `
const assert = require('node:assert/strict');
const client = require('@romedb/client'), values = require('@romedb/client/values');
assert.equal(client.Record, values.Record);
assert.equal(String(new client.Record('posts', 'a')), 'posts:a');
${fakeService}
client.connect({ url: 'http://127.0.0.1:4321', token: 'local-test-token', fetch }).then(async rql => {
  assert.equal(rql.sequence, 3);
  await rql.close();
  console.log('connected');
});
`);
  assert.equal(output.trim(), 'connected');
});

test('ES modules load the package by name, and the non-Node entries connect', () => {
  const output = runInConsumer('load.mjs', `
import assert from 'node:assert/strict';
import { connect, Record } from '@romedb/client';
import * as web from './node_modules/@romedb/client/client.mjs';
import * as webValues from './node_modules/@romedb/client/values.mjs';
assert.equal(new Record('posts', 'a').equals(new Record('posts', 'a')), true);
assert.equal(web.Record, webValues.Record);
${fakeService}
for (const open of [connect, web.connect]) {
  const rql = await open({ url: 'http://127.0.0.1:4321', token: 'local-test-token', fetch });
  assert.equal(rql.sequence, 3);
  await rql.close();
}
console.log('connected');
`);
  assert.equal(output.trim(), 'connected');
});

test('the native parts of @romedb/node are not exported', () => {
  const output = runInConsumer('missing.cjs', `
for (const name of ['@romedb/client/embedded-client', '@romedb/client/native']) {
  try { require(name); console.log('loaded', name); } catch (error) { console.log(error.code); }
}
`);
  assert.deepEqual(output.trim().split('\n'), ['ERR_PACKAGE_PATH_NOT_EXPORTED', 'ERR_PACKAGE_PATH_NOT_EXPORTED']);
});
