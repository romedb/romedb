'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const net = require('node:net');
const { spawn } = require('node:child_process');
const { once } = require('node:events');
const { connect, Record, saveRequest, loadRequest } = require('../../bindings/node/client.cjs');

async function worker() {
  let input = '';
  for await (const chunk of process.stdin) input += chunk;
  const { url, token, id } = JSON.parse(input);
  const database = await connect({ url, token });
  try {
    const result = await database.writeAtomic([{ sql: 'INSERT INTO docs DOCUMENT $doc', params: { $doc: { id: new Record('docs', id), title: id, count: 9223372036854775807n } } }]);
    process.stdout.write(JSON.stringify({ sequence: result.sequence }));
  } finally { await database.close(); }
}

async function freePort() {
  const server = net.createServer();
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const port = server.address().port;
  await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  return port;
}

async function start(binary, file, port, create) {
  const args = ['serve', file, '--port', String(port)];
  if (create) args.push('--create');
  const child = spawn(binary, args, { stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  child.stderr.on('data', chunk => { stderr += chunk; });
  try {
    await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error('service startup timed out')), 15000);
      child.stdout.on('data', chunk => {
        stdout += chunk;
        if (stdout.split('\n').length >= 3) { clearTimeout(timeout); resolve(); }
      });
      child.once('error', error => { clearTimeout(timeout); reject(error); });
      child.once('exit', () => { clearTimeout(timeout); reject(new Error(`service startup failed: ${stderr}`)); });
    });
    const token = (await fs.readFile(`${file}.token`, 'utf8')).trim();
    assert(!stdout.includes(token));
    assert(!stderr.includes(token));
    return { child, token, url: `http://127.0.0.1:${port}/v1/database` };
  } catch (error) { child.kill('SIGKILL'); throw error; }
}

async function stop(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const exited = once(child, 'exit');
  const timeout = setTimeout(() => child.kill('SIGKILL'), 25000);
  child.kill('SIGTERM');
  const [code, signal] = await exited;
  clearTimeout(timeout);
  assert.equal(signal, null);
  assert.equal(code, 0);
}

async function independentClient(configuration) {
  const child = spawn(process.execPath, [__filename, '--worker'], { stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  child.stdout.on('data', chunk => { stdout += chunk; });
  child.stderr.on('data', chunk => { stderr += chunk; });
  const exited = once(child, 'exit');
  child.stdin.end(JSON.stringify(configuration));
  const [code] = await exited;
  assert.equal(code, 0, stderr);
  return JSON.parse(stdout);
}

async function main() {
  const binary = process.argv[2] ? path.resolve(process.argv[2]) : path.resolve(__dirname, '../../../target/debug/romedb');
  const fixtureRoot = path.resolve(__dirname, '../../../../.local/tmp');
  await fs.mkdir(fixtureRoot, { recursive: true });
  const directory = await fs.mkdtemp(path.join(fixtureRoot, 'studio-service-sdk-'));
  const file = path.join(directory, 'app.romedb');
  const port = await freePort();
  let service, database;
  try {
    service = await start(binary, file, port, true);
    database = await connect(service);
    await database.writeAtomic([{ sql: 'CREATE TABLE docs;' }]);
    const clients = await Promise.all(['client_0', 'client_1'].map(id => independentClient({ url: service.url, token: service.token, id })));
    assert.deepEqual(clients.map(value => value.sequence).sort(), [2, 3]);
    let snapshot = await database.read([{ sql: 'SELECT id,title,count FROM docs ORDER BY title' }]);
    assert.equal(snapshot.results[0].rows.length, 2);
    assert.equal(snapshot.results[0].rows[0][2], 9223372036854775807n);
    let loseResponse = true;
    const faulty = await connect({ url: service.url, token: service.token, fetch: async (url, options) => {
      const response = await fetch(url, options);
      if (loseResponse && String(url).endsWith('/query')) { loseResponse = false; await response.arrayBuffer(); throw new Error('lost response fixture'); }
      return response;
    } });
    const pending = faulty.prepareWrite([{ sql: 'INSERT INTO docs DOCUMENT $doc', params: { $doc: { id: new Record('docs', 'lost'), title: 'lost', count: 4n } } }]);
    const journal = path.join(directory, 'request.json');
    await saveRequest(journal, pending);
    await assert.rejects(faulty.submit(pending), error => error.outcome === 'unknown');
    await faulty.close();
    await database.close();
    database = undefined;
    const originalToken = service.token;
    await stop(service.child);
    service = await start(binary, file, port, false);
    assert.equal(service.token, originalToken);
    database = await connect(service);
    const restored = await loadRequest(journal);
    assert.deepEqual(restored, pending);
    assert.equal((await database.recover(restored)).sequence, 4);
    assert.equal((await database.recover(restored)).sequence, 4);
    await assert.rejects(database.writeAtomic([
      { sql: "INSERT INTO docs {id:docs:rollback,title:'rollback'}" },
      { sql: "INSERT INTO docs {id:docs:lost,title:'duplicate'}" },
    ]), error => error.outcome === 'rolled-back');
    snapshot = await database.read([{ sql: 'SELECT id FROM docs' }]);
    assert.equal(snapshot.results[0].rows.length, 3);
    const stale = database.prepareWrite([{ sql: 'UPDATE docs:lost SET title=$title', params: { $title: 'stale' } }], { ifSequence: snapshot.sequence });
    await database.writeAtomic([{ sql: 'UPDATE docs:lost SET title=$title', params: { $title: 'other' } }]);
    await assert.rejects(database.submit(stale), error => error.code === 'FDB_CONFLICT' && error.outcome === 'rolled-back');
    assert.equal((await database.read([{ sql: 'SELECT title FROM docs WHERE id=$id', params: { $id: new Record('docs', 'lost') } }])).results[0].rows[0][0], 'other');
    const wrongOwner = database.prepareWrite([{ sql: 'CREATE TABLE unexpected;' }], { afterSequence: 0 });
    await stop(service.child);
    const replacement = path.join(directory, 'replacement.romedb');
    await fs.copyFile(`${file}.token`, `${replacement}.token`);
    service = await start(binary, replacement, port, true);
    assert.equal(service.token, originalToken);
    await assert.rejects(database.submit(wrongOwner), error => error.outcome === 'unknown' && error.request === wrongOwner);
    await assert.rejects(database.recover(wrongOwner), error => error.outcome === 'unknown');
    const current = await connect(service);
    try {
      assert.notEqual(current.databaseId, wrongOwner.databaseId);
      assert.equal(current.sequence, 0);
      assert.deepEqual((await current.read([{ sql: 'INFO FOR DB' }])).results[0].rows[0][0].tables, []);
      assert.equal((await current.writeAtomic([{ sql: 'CREATE TABLE intended;' }])).sequence, 1);
    } finally { await current.close(); }
    process.stdout.write('Service SDK qualification passed: two independent clients, int64, lost response, journal restore, restart replay, rollback, stale edits and database identity across port/token reuse.\n');
  } finally {
    if (database) await database.close();
    if (service) await stop(service.child);
    await fs.rm(directory, { recursive: true, force: true });
  }
}

module.exports = { freePort, start, stop };
if (require.main === module) {
  (process.argv[2] === '--worker' ? worker() : main()).catch(error => { process.stderr.write(`${error.stack}\n`); process.exitCode = 1; });
}
