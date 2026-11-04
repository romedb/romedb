'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const { randomUUID } = require('node:crypto');
const { spawn } = require('node:child_process');
const { connect } = require('../../bindings/node/client.cjs');
const { openEmbedded } = require('../../bindings/node/embedded-client.cjs');
const { freePort, start, stop } = require('./sdk.cjs');

async function startDirectory(binary, directory, port) {
  const child = spawn(binary, ['serve', '--directory', directory, '--create', '--port', String(port)], { stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  child.stderr.on('data', chunk => { stderr += chunk; });
  try {
    await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error('service startup timed out')), 15000);
      child.stdout.on('data', chunk => {
        stdout += chunk;
        if (stdout.split('\n').length >= 3) { clearTimeout(timeout); resolve(); }
      });
      child.once('exit', () => { clearTimeout(timeout); reject(new Error(`service startup failed: ${stderr}`)); });
    });
    const organization = /^Organization URL: (\S+)$/m.exec(stdout)[1];
    const token = (await fs.readFile(path.join(directory, 'romedb.token'), 'utf8')).trim();
    return { child, token, organization };
  } catch (error) { child.kill('SIGKILL'); throw error; }
}

async function manage(service, method, suffix = '', body) {
  const response = await fetch(`${service.organization}/databases${suffix}`, {
    method,
    headers: { authorization: `Bearer ${service.token}`, ...(body === undefined ? {} : { 'content-type': 'application/json' }) },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  return { status: response.status, body: response.status === 204 ? null : await response.json() };
}

async function rows(client, sql) {
  return (await client.read([{ sql }])).results[0].rows;
}

async function save(file, body) {
  const handle = await fs.open(file, 'wx', 0o600);
  try { for await (const chunk of body) await handle.write(chunk); }
  finally { await handle.close(); }
}

// Cossack's check: download a site while another request keeps writing, then open the file alone.
async function exportWhileWriting(remote, root, binary) {
  await remote.writeAtomic([{ sql: 'CREATE TABLE marks(n INTEGER)' }]);
  for (let start = 0; start < 2000; start += 25) {
    await remote.writeAtomic(Array.from({ length: 25 }, (_, i) => ({ sql: `INSERT INTO posts VALUES('filler ${start + i} ${'x'.repeat(1500)}')` })));
  }
  const written = [];
  let exported;
  const exporting = remote.exportFile({ timeoutMs: 120000 }).then(file => { exported = file; });
  let during = 0;
  for (let n = 1; exported === undefined; n++) {
    written.push([n, (await remote.writeAtomic([{ sql: `INSERT INTO marks VALUES(${n})` }])).sequence]);
    if (exported === undefined) during++;
  }
  await exporting;
  assert.ok(during > 0, 'no write finished while the file was built');
  const file = path.join(root, 'exported.romedb');
  await save(file, exported.body);
  assert.equal((await fs.stat(file)).size, exported.size);
  const expected = written.filter(([, sequence]) => sequence <= exported.sequence).map(([n]) => [BigInt(n)]);
  const embedded = await openEmbedded({ path: file, maxOpen: 32 });
  try { assert.deepEqual(await rows(embedded, 'SELECT n FROM marks ORDER BY n'), expected); }
  finally { await embedded.close(); }
  const served = await start(binary, file, await freePort(), false);
  try {
    const client = await connect({ url: served.url, token: served.token });
    try {
      assert.equal((await client.info()).sequence, exported.sequence);
      assert.deepEqual(await rows(client, 'SELECT n FROM marks ORDER BY n'), expected);
      const again = await client.exportFile();
      assert.equal(again.sequence, exported.sequence);
      await again.body.cancel();
    } finally { await client.close(); }
  } finally { await stop(served.child); }
  return { during, size: exported.size };
}

async function main() {
  const binary = process.argv[2] ? path.resolve(process.argv[2]) : path.resolve(__dirname, '../../../target/debug/romedb');
  const fixtureRoot = path.resolve(__dirname, '../../../../.local/tmp');
  await fs.mkdir(fixtureRoot, { recursive: true });
  const root = await fs.mkdtemp(path.join(fixtureRoot, 'romedb-directory-sdk-'));
  const directory = path.join(root, 'sites');
  const port = await freePort();
  let service, exported;
  try {
    service = await startDirectory(binary, directory, port);
    const created = await manage(service, 'POST', '', { name: 'main' });
    assert.equal(created.status, 201);
    const main = created.body;
    const remote = await connect({ url: main.url, token: service.token });
    try {
      assert.equal((await remote.info()).capabilities.provenance, 'negotiated');
      assert.equal(remote.databaseId, main.id);
      await remote.writeAtomic([{ sql: 'CREATE TABLE posts(title TEXT)' }, { sql: "INSERT INTO posts VALUES('from serve')" }]);
      exported = await exportWhileWriting(remote, root, binary);
      await remote.writeAtomic([{ sql: "DELETE FROM posts WHERE title LIKE 'filler %'" }, { sql: 'DROP TABLE marks' }]);
    } finally { await remote.close(); }
    await stop(service.child);
    service = undefined;

    const embedded = await openEmbedded({ path: path.join(directory, `${main.id}.romedb`), maxOpen: 32 });
    try {
      assert.equal((await embedded.info()).journalMode, 'mvcc');
      assert.deepEqual(await rows(embedded, 'SELECT title FROM posts'), [['from serve']]);
      await embedded.writeAtomic([{ sql: "INSERT INTO posts VALUES('from embedded')" }]);
    } finally { await embedded.close(); }
    const adoptedId = randomUUID();
    const adopted = await openEmbedded({ path: path.join(directory, `${adoptedId}.romedb`), create: true, maxOpen: 32 });
    try { await adopted.writeAtomic([{ sql: 'CREATE TABLE pages(slug TEXT)' }, { sql: "INSERT INTO pages VALUES('home')" }]); }
    finally { await adopted.close(); }

    service = await startDirectory(binary, directory, port);
    const listed = await manage(service, 'GET');
    assert.deepEqual(listed.body.databases.map(entry => [entry.id, entry.name]).sort(), [[adoptedId, `db-${adoptedId}`], [main.id, 'main']].sort());
    const again = await connect({ url: main.url, token: service.token });
    try { assert.deepEqual(await rows(again, 'SELECT title FROM posts ORDER BY title'), [['from embedded'], ['from serve']]); }
    finally { await again.close(); }
    const page = await connect({ url: listed.body.databases.find(entry => entry.id === adoptedId).url, token: service.token });
    try { assert.deepEqual(await rows(page, 'SELECT slug FROM pages'), [['home']]); }
    finally { await page.close(); }
    assert.equal((await manage(service, 'DELETE', `/${adoptedId}`)).status, 204);
    await assert.rejects(fs.stat(path.join(directory, `${adoptedId}.romedb`)), { code: 'ENOENT' });
    process.stdout.write(`Directory SDK qualification passed: Cloud-shaped management, negotiated loopback client, a ${exported.size}-byte file export during ${exported.during} writes opened by openEmbedded and serve FILE, serve to embedded and back, adopted embedded file, deletion.\n`);
  } finally {
    if (service) await stop(service.child);
    await fs.rm(root, { recursive: true, force: true });
  }
}

if (require.main === module) main().catch(error => { process.stderr.write(`${error.stack}\n`); process.exitCode = 1; });
