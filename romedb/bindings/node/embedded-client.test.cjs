'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const { spawn } = require('node:child_process');
const { once } = require('node:events');
const { openEmbedded, ClientError, Float64, Record, Vector, encodeValue, saveRequest, loadRequest } = require('./embedded-client.cjs');

async function openInChild(file) {
  const child = spawn(process.execPath, ['-e', `require(${JSON.stringify(require.resolve('./embedded-client.cjs'))}).openEmbedded({path:process.argv[1]}).then(db=>db.close()).then(()=>console.log('opened'),error=>{console.log(error.code);process.exitCode=2})`, file], { stdio: ['ignore', 'pipe', 'pipe'] });
  let output = '';
  child.stdout.on('data', chunk => { output += chunk; });
  const [code] = await once(child, 'exit');
  return { code, output: output.trim() };
}

async function fixture(t) {
  const root = path.resolve(__dirname, '../../../..', '.local/tmp');
  await fs.mkdir(root, { recursive: true });
  const directory = await fs.mkdtemp(path.join(root, 'studio-embedded-'));
  t.after(() => fs.rm(directory, { recursive: true, force: true }));
  return { directory, file: path.join(directory, 'app.romedb') };
}

test('malformed Unicode is rejected before dispatch and leaves the shared adapter usable', async t => {
  const { file } = await fixture(t);
  const db = await openEmbedded({ path: file, create: true });
  t.after(() => db.close());
  await db.writeAtomic([{ sql: 'CREATE TABLE notes(text)' }]);
  for (const statement of [
    { sql: 'INSERT INTO notes VALUES($text)', params: { $text: 'emoji \ud83d' } },
    { sql: 'INSERT INTO notes VALUES($text)', params: { $text: { ['\udc00']: 1 } } },
    { sql: 'INSERT INTO notes VALUES($text)', params: { ['$\ud800']: 1 } },
    { sql: "INSERT INTO notes VALUES('\ud800')" },
  ]) assert.throws(() => db.prepareWrite([statement]), { name: 'TypeError', message: /well-formed/ });
  await assert.rejects(db.read([{ sql: 'SELECT $text', params: { $text: new Record('notes', 'k\ud800') } }]), { name: 'TypeError', message: /well-formed/ });
  await db.writeAtomic([{ sql: 'INSERT INTO notes VALUES($text)', params: { $text: 'emoji \ud83d\ude00' } }]);
  assert.deepEqual((await db.read([{ sql: 'SELECT text FROM notes' }])).results[0].rows, [['emoji \ud83d\ude00']]);
});

test('embedded client explicitly creates files and round-trips portable values without a service', async t => {
  const { file } = await fixture(t);
  await assert.rejects(openEmbedded({ path: file }), error => error.code === 'RDB_NOT_FOUND');
  await assert.rejects(fs.stat(file), { code: 'ENOENT' });
  const db = await openEmbedded({ path: file, create: true });
  t.after(() => db.close());
  const info = await db.info();
  assert.equal(info.engineVersion, require('./package.json').version);
  assert.deepEqual((await db.read([{ sql: 'SELECT romedb_version()' }])).results[0].rows, [[info.engineVersion]]);
  assert.equal(info.journalMode, 'mvcc');
  assert.equal(info.capabilities.sequenceScope, 'adapter');
  assert.equal(info.capabilities.replayDurability, 'adapter-lifetime');
  const value = { id: new Record('docs', 'a'), n: (1n << 63n) - 1n, float: new Float64(-0), binary: new Uint8Array([0, 255]), vector: Vector.float64([1, -0]), present: null, nested: [{ type: 'Number', value: 'ordinary' }] };
  const write = await db.writeAtomic([{ sql: 'CREATE TABLE docs' }, { sql: 'INSERT INTO docs DOCUMENT $doc', params: { $doc: value } }]);
  assert.equal(write.sequence, 1);
  const result = await db.read([{ sql: 'SELECT $v AS same,$v AS same', params: { $v: value } }]);
  assert.deepEqual(result.results[0].columns, ['same', 'same']);
  for (const returned of result.results[0].rows[0]) assert.deepEqual(encodeValue(returned), encodeValue(value));
  const stored = (await db.read([{ sql: 'SELECT * FROM docs' }])).results[0].rows[0][0];
  assert.deepEqual(encodeValue(stored), encodeValue(value));
  assert.equal(Object.hasOwn(stored, 'absent'), false);
  assert.equal(Object.hasOwn(stored, 'present'), true);
  assert.equal((await db.read([{ sql: 'SELECT n FROM docs' }])).results[0].rows[0][0], (1n << 63n) - 1n);
  await assert.rejects(db.read([{ sql: 'DELETE FROM docs' }]), error => error.code === 'RDB_UNSUPPORTED' && error.outcome === 'read-failed');
});

test('two managed clients share one adapter and reject stale saves atomically', async t => {
  const { file } = await fixture(t);
  const first = await openEmbedded({ path: file, create: true });
  const second = await openEmbedded({ path: file });
  t.after(async () => { await first.close(); await second.close(); });
  assert.equal(first.databaseId, second.databaseId);
  await first.writeAtomic([{ sql: 'CREATE TABLE data (id INTEGER PRIMARY KEY,n INTEGER)' }, { sql: 'INSERT INTO data VALUES(1,1)' }]);
  const before = await second.read([{ sql: 'SELECT n FROM data' }]);
  await first.writeAtomic([{ sql: 'UPDATE data SET n=2 WHERE id=1' }]);
  const stale = second.prepareWrite([{ sql: 'UPDATE data SET n=3 WHERE id=1' }], { ifSequence: before.sequence });
  await assert.rejects(second.submit(stale), error => error.code === 'RDB_CONFLICT' && error.outcome === 'rolled-back' && error.request === stale);
  assert.equal((await second.read([{ sql: 'SELECT n FROM data' }])).results[0].rows[0][0], 2n);
  await first.close();
  await second.writeAtomic([{ sql: 'UPDATE data SET n=4 WHERE id=1' }]);
  assert.equal((await second.read([{ sql: 'SELECT n FROM data' }])).results[0].rows[0][0], 4n);
});

test('embedded replay keeps the exact body and expires when the last client closes', async t => {
  const { file, directory } = await fixture(t);
  const db = await openEmbedded({ path: file, create: true });
  try {
    await db.writeAtomic([{ sql: 'CREATE TABLE data (id INTEGER PRIMARY KEY,n INTEGER)' }, { sql: 'INSERT INTO data VALUES(1,0)' }]);
    const request = db.prepareWrite([{ sql: 'UPDATE data SET n=n+1 WHERE id=1' }]);
    const journal = path.join(directory, 'request.json');
    await saveRequest(journal, request);
    assert.deepEqual(await loadRequest(journal), request);
    const reply = await db.submit(request);
    assert.deepEqual(await db.recover(await loadRequest(journal)), reply);
    const changed = { ...request, body: request.body.replace('n=n+1', 'n=n+2') };
    await assert.rejects(db.submit(changed), error => error.code === 'RDB_REQUEST_IDENTITY' && error.outcome === 'unknown');
    assert.equal((await db.read([{ sql: 'SELECT n FROM data' }])).results[0].rows[0][0], 1n);
    await db.close();
    const reopened = await openEmbedded({ path: file });
    try {
      assert.notEqual(reopened.databaseId, request.databaseId);
      await assert.rejects(reopened.submit(request), error => error.code === 'RDB_REQUEST_IDENTITY' && error.outcome === 'not-sent');
      await assert.rejects(reopened.recover(request), error => error.outcome === 'unknown');
      assert.equal((await reopened.read([{ sql: 'SELECT n FROM data' }])).results[0].rows[0][0], 1n);
    } finally { await reopened.close(); }
  } finally { await db.close(); }
});

test('embedded constraints, work limits and result limits restore the whole batch', async t => {
  const { file } = await fixture(t);
  const db = await openEmbedded({ path: file, create: true });
  t.after(() => db.close());
  await db.writeAtomic([{ sql: 'CREATE TABLE data (id INTEGER PRIMARY KEY,n INTEGER)' }, { sql: 'INSERT INTO data VALUES(1,1)' }]);
  for (const [statements, options, code] of [
    [[{ sql: 'INSERT INTO data VALUES(2,2)' }, { sql: 'INSERT INTO data VALUES(1,3)' }], {}, 'RDB_CONSTRAINT'],
    [[{ sql: 'INSERT INTO data VALUES(2,2)' }, { sql: 'INSERT INTO data VALUES(3,3)' }], { workLimits: { rowMutations: 1 } }, 'RDB_LIMIT'],
    [[{ sql: 'INSERT INTO data VALUES(2,2)' }, { sql: 'SELECT hex(zeroblob(80000))' }, { sql: 'SELECT hex(zeroblob(80000))' }], {}, 'RDB_LIMIT'],
    [[{ sql: 'INSERT INTO data VALUES(2,2)' }, { sql: 'COMMIT' }], {}, 'RDB_UNSUPPORTED'],
  ]) {
    await assert.rejects(db.writeAtomic(statements, options), error => error.code === code && error.outcome === 'rolled-back');
    assert.deepEqual((await db.read([{ sql: 'SELECT n FROM data ORDER BY n' }])).results[0].rows, [[1n]]);
  }
  await db.writeAtomic([{ sql: 'CREATE TABLE parents (id INTEGER PRIMARY KEY)' }, { sql: 'CREATE TABLE children (id INTEGER PRIMARY KEY,parent INTEGER REFERENCES parents(id))' }]);
  await assert.rejects(db.writeAtomic([{ sql: 'INSERT INTO data VALUES(2,2)' }, { sql: 'INSERT INTO children VALUES(1,99)' }]), error => error.code === 'RDB_CONSTRAINT' && error.outcome === 'rolled-back');
  assert.deepEqual((await db.read([{ sql: 'SELECT n FROM data' }])).results[0].rows, [[1n]]);
});

test('embedded cancellation retains a recoverable request and releases the worker on close', async t => {
  const { file } = await fixture(t);
  const db = await openEmbedded({ path: file, create: true });
  await db.writeAtomic([{ sql: 'CREATE TABLE data (n INTEGER)' }]);
  const request = db.prepareWrite([{ sql: 'INSERT INTO data VALUES(1)' }, { sql: 'WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM t WHERE n<100000) SELECT sum(n) FROM t' }]);
  const controller = new AbortController();
  const pending = db.submit(request, { signal: controller.signal });
  controller.abort();
  await assert.rejects(pending, error => error instanceof ClientError && error.outcome === 'unknown' && error.request === request);
  await assert.rejects(db.recover(request), error => error.code === 'RDB_CANCELLED' && error.outcome === 'rolled-back');
  assert.deepEqual((await db.read([{ sql: 'SELECT n FROM data' }])).results[0].rows, []);
  const jobs = Array.from({ length: 4 }, () => db.read([{ sql: 'SELECT 1' }]));
  const settled = Promise.allSettled(jobs);
  await db.close();
  await settled;
  await assert.rejects(db.read([{ sql: 'SELECT 1' }]), error => error.code === 'RDB_CLOSED');
  const reopened = await openEmbedded({ path: file });
  await reopened.close();
});

test('embedded files retain single-process ownership and refuse file aliases', async t => {
  const { file, directory } = await fixture(t);
  const db = await openEmbedded({ path: file, create: true });
  t.after(() => db.close());
  const alias = path.join(directory, 'alias.romedb');
  await fs.symlink(file, alias);
  await assert.rejects(openEmbedded({ path: alias }), error => error.code === 'RDB_VALIDATION');
  const hardlink = path.join(directory, 'hardlink.romedb');
  await fs.link(file, hardlink);
  await assert.rejects(openEmbedded({ path: hardlink }), error => error.code === 'RDB_VALIDATION');
  await fs.unlink(hardlink);
  assert.deepEqual(await openInChild(file), { code: 2, output: 'RDB_BUSY' });
  assert.equal((await db.info()).sequence, 0);
});

test('embedded opens refuse WAL files without changing them and point to the migration', async t => {
  const { file } = await fixture(t);
  const { Database } = require('./index.cjs');
  const legacy = new Database(file);
  legacy.execute('CREATE TABLE notes(text)');
  legacy.execute("INSERT INTO notes VALUES('kept')");
  legacy.close();
  const snapshot = async () => Object.fromEntries(await Promise.all((await fs.readdir(path.dirname(file))).map(async name => [name, (await fs.readFile(path.join(path.dirname(file), name))).toString('base64')])));
  const before = await snapshot();
  await assert.rejects(openEmbedded({ path: file }), error => error.code === 'RDB_UNSUPPORTED' && error.outcome === 'not-sent' && /migrate-to-mvcc/.test(error.message));
  assert.deepEqual(await snapshot(), before);
});

test('every file of a new embedded database is readable only by its owner', async t => {
  const { directory, file } = await fixture(t);
  const modes = async () => Object.fromEntries(await Promise.all((await fs.readdir(directory)).map(async name => [name, ((await fs.stat(path.join(directory, name))).mode & 0o777).toString(8)])));
  const expected = { 'app.romedb': '600', 'app.romedb-wal': '600', 'app.db-log': '600' };
  const db = await openEmbedded({ path: file, create: true });
  await db.writeAtomic([{ sql: 'CREATE TABLE notes(text)' }, { sql: "INSERT INTO notes VALUES('private')" }]);
  assert.deepEqual(await modes(), expected);
  await db.close();
  const reopened = await openEmbedded({ path: file });
  await reopened.close();
  assert.deepEqual(await modes(), expected);
});

test('maxOpen bounds the files a process keeps open, and close checkpoints and releases each file', async t => {
  const { directory } = await fixture(t);
  const files = ['a', 'b', 'c'].map(name => path.join(directory, `${name}.romedb`));
  await assert.rejects(openEmbedded({ path: files[0], create: true, maxOpen: 0 }), RangeError);
  await assert.rejects(openEmbedded({ path: files[0], create: true, maxOpen: 1025 }), RangeError);
  const first = await openEmbedded({ path: files[0], create: true, maxOpen: 2 });
  t.after(() => first.close());
  const second = await openEmbedded({ path: files[1], create: true, maxOpen: 2 });
  t.after(() => second.close());
  await assert.rejects(openEmbedded({ path: files[2], create: true, maxOpen: 2 }), error => error.code === 'RDB_LIMIT' && error.outcome === 'not-sent');
  await assert.rejects(fs.stat(files[2]), { code: 'ENOENT' });
  const sameFile = await openEmbedded({ path: files[0], maxOpen: 2 });
  await sameFile.close();
  await first.writeAtomic([{ sql: 'CREATE TABLE notes(text)' }, { sql: "INSERT INTO notes VALUES('one')" }]);
  assert.ok((await fs.stat(path.join(directory, 'a.db-log'))).size > 0);
  await first.close();
  assert.equal((await fs.stat(path.join(directory, 'a.db-log'))).size, 0);
  assert.deepEqual(await openInChild(files[0]), { code: 0, output: 'opened' });
  const third = await openEmbedded({ path: files[2], create: true, maxOpen: 2 });
  await third.close();
});

test('a process opens 200 files in turn with at most 32 open', async t => {
  const { directory } = await fixture(t);
  const open = new Map();
  t.after(() => Promise.all([...open.values()].map(database => database.close())));
  const started = performance.now();
  for (let index = 0; index < 200; index++) {
    if (open.size === 32) {
      const [oldest, database] = open.entries().next().value;
      open.delete(oldest);
      await database.close();
    }
    const database = await openEmbedded({ path: path.join(directory, `${index}.romedb`), create: true, maxOpen: 32 });
    await database.writeAtomic([{ sql: 'CREATE TABLE site(id)' }, { sql: 'INSERT INTO site VALUES($id)', params: { $id: BigInt(index) } }]);
    open.set(index, database);
  }
  await Promise.all([...open.values()].map(database => database.close()));
  open.clear();
  const reopenStarted = performance.now();
  const reopened = await openEmbedded({ path: path.join(directory, '0.romedb') });
  const reopenMs = performance.now() - reopenStarted;
  assert.deepEqual((await reopened.read([{ sql: 'SELECT id FROM site' }])).results[0].rows, [[0n]]);
  await reopened.close();
  t.diagnostic(`200 files in ${Math.round(performance.now() - started)} ms; one reopen in ${reopenMs.toFixed(1)} ms`);
});

test('an evicted embedded receipt cannot execute its write again', async t => {
  const { file } = await fixture(t);
  const db = await openEmbedded({ path: file, create: true });
  t.after(() => db.close());
  await db.writeAtomic([{ sql: 'CREATE TABLE data (n INTEGER)' }, { sql: 'INSERT INTO data VALUES(0)' }]);
  const request = db.prepareWrite([{ sql: 'UPDATE data SET n=n+1' }]);
  await db.submit(request);
  for (let index = 0; index < 64; index++) await db.writeAtomic([{ sql: 'SELECT 1' }]);
  await assert.rejects(db.recover(request), error => error.code === 'RDB_CONFLICT' && error.outcome === 'unknown');
  assert.equal((await db.read([{ sql: 'SELECT n FROM data' }])).results[0].rows[0][0], 1n);
});

test('embedded queue limits apply across clients that share the worker', async t => {
  const { file } = await fixture(t);
  const first = await openEmbedded({ path: file, create: true });
  const second = await openEmbedded({ path: file });
  t.after(async () => { await first.close(); await second.close(); });
  const statements = [{ sql: 'SELECT 1' }];
  const jobs = Array.from({ length: 32 }, (_, index) => (index % 2 ? first : second).read(statements));
  const complete = Promise.all(jobs);
  await assert.rejects(first.read([{ sql: 'SELECT 1' }]), error => error.code === 'RDB_LIMIT' && error.outcome === 'not-sent');
  const replies = await complete;
  assert.equal(replies.length, 32);
  for (const reply of replies) assert.equal(reply.results[0].rows[0][0], 1n);
  assert.equal((await first.info()).sequence, 0);
});

test('a lost acknowledgement recovers the exact write against the real embedded adapter', async t => {
  const { file } = await fixture(t);
  const backend = await openEmbedded({ path: file, create: true });
  t.after(() => backend.close());
  await backend.writeAtomic([{ sql: 'CREATE TABLE data (n INTEGER)' }, { sql: 'INSERT INTO data VALUES(0)' }]);
  const { createEmbeddedClient } = require('./client-internal.cjs');
  let loseAcknowledgement = true;
  const bodies = [];
  const client = await createEmbeddedClient({ url: backend.url }, async (route, body) => {
    if (route === '') return backend.info();
    assert.equal(route, '/query');
    bodies.push(body);
    const reply = await backend.submit({ version: 1, url: backend.url, databaseId: backend.databaseId, body });
    if (loseAcknowledgement) { loseAcknowledgement = false; throw new Error('Acknowledgement dropped after native publication'); }
    return { ...reply, results: reply.results.map(result => ({ columns: result.columns, rows: result.rows.map(row => row.map(encodeValue)), affected: result.affected.toString() })) };
  });
  t.after(() => client.close());
  const request = client.prepareWrite([{ sql: 'UPDATE data SET n=n+1' }]);
  await assert.rejects(client.submit(request), error => error.outcome === 'unknown' && error.request === request);
  await client.recover(request);
  assert.deepEqual(bodies, [request.body, request.body]);
  assert.equal((await backend.read([{ sql: 'SELECT n FROM data' }])).results[0].rows[0][0], 1n);
});

test('an embedded startup deadline returns promptly and its abandoned client releases ownership', async t => {
  const { file } = await fixture(t);
  await (await openEmbedded({ path: file, create: true })).close();
  await assert.rejects(openEmbedded({ path: file, timeoutMs: 1 }), error => error.code === 'RDB_CANCELLED' && error.outcome === 'not-sent');
  const healthy = await openEmbedded({ path: file });
  assert.equal((await healthy.info()).sequence, 0);
  await healthy.close();
  const reopened = await openEmbedded({ path: file });
  await reopened.close();
});

test('the embedded adapter preserves an existing MVCC file and its atomic rollback behavior', async t => {
  const { file } = await fixture(t);
  const { NativeDatabase } = require('./native.cjs');
  const seed = new NativeDatabase(file);
  try { assert.equal(JSON.parse(seed.execute('PRAGMA journal_mode=mvcc', '{}')).execution.error, undefined); }
  finally { seed.close(); }
  const client = await openEmbedded({ path: file });
  try {
    assert.equal((await client.info()).journalMode, 'mvcc');
    await client.writeAtomic([{ sql: 'CREATE TABLE data (id INTEGER PRIMARY KEY)' }, { sql: 'INSERT INTO data VALUES(1)' }]);
    await assert.rejects(client.writeAtomic([{ sql: 'INSERT INTO data VALUES(2)' }, { sql: 'INSERT INTO data VALUES(1)' }]), error => error.code === 'RDB_CONSTRAINT' && error.outcome === 'rolled-back');
    assert.deepEqual((await client.read([{ sql: 'SELECT id FROM data' }])).results[0].rows, [[1n]]);
  } finally { await client.close(); }
  const reopened = await openEmbedded({ path: file });
  try { assert.equal((await reopened.info()).journalMode, 'mvcc'); }
  finally { await reopened.close(); }
});

async function fillPosts(db, count) {
  await db.writeAtomic([{ sql: 'CREATE TABLE posts' }, { sql: 'CREATE TABLE marks' }]);
  const body = 'x'.repeat(2000);
  for (let start = 0; start < count; start += 25) {
    const rows = Array.from({ length: 25 }, (_, i) => `{n:${start + i},body:'${body}'}`);
    await db.writeAtomic(rows.map(row => ({ sql: `INSERT INTO posts ${row}` })));
  }
}

async function names(directory) { return (await fs.readdir(directory)).sort(); }

test('exportFile copies the database at the sequence it reports while writes continue', async t => {
  const { directory, file } = await fixture(t);
  const db = await openEmbedded({ path: file, create: true });
  t.after(() => db.close());
  await fillPosts(db, 2500);
  const destination = path.join(directory, 'copy.romedb');
  const mark = async n => [n, (await db.writeAtomic([{ sql: `INSERT INTO marks {n:${n}}` }])).sequence];
  const written = [await mark(1), await mark(2), await mark(3)];
  let exported;
  const exporting = db.exportFile(destination).then(result => { exported = result; });
  let during = 0;
  for (let n = 4; exported === undefined; n++) {
    written.push(await mark(n));
    if (exported === undefined) during++;
  }
  await exporting;
  assert.ok(during > 0, 'no write finished during the export');
  assert.equal(exported.sequence, written[2][1]);
  const stats = await fs.stat(destination);
  assert.equal(stats.size, exported.bytes);
  assert.equal(stats.mode & 0o777, 0o600);
  const copy = await openEmbedded({ path: destination });
  try {
    const marks = (await copy.read([{ sql: 'SELECT n FROM marks ORDER BY n' }])).results[0].rows.map(([n]) => n);
    assert.deepEqual(marks, written.filter(([, sequence]) => sequence <= exported.sequence).map(([n]) => BigInt(n)));
    assert.deepEqual((await copy.read([{ sql: 'SELECT count() FROM posts' }])).results[0].rows, [[2500n]]);
  } finally { await copy.close(); }
  await assert.rejects(db.exportFile(destination), error => error instanceof ClientError && error.code === 'RDB_ALREADY_EXISTS' && error.outcome === 'read-failed');
  await assert.rejects(db.exportFile(destination, { afterSequence: 1 }), TypeError);
});

test('exportFile leaves named tables empty in both forms and refuses unknown names', async t => {
  const { directory, file } = await fixture(t);
  const db = await openEmbedded({ path: file, create: true });
  t.after(() => db.close());
  for (const sql of ['CREATE TABLE sessions', 'CREATE UNIQUE INDEX sessions_token ON sessions(token)', 'CREATE TABLE posts']) await db.writeAtomic([{ sql }]);
  await db.writeAtomic([{ sql: "INSERT INTO sessions {token: 'marker-session-token'}" }]);
  await db.writeAtomic([{ sql: "INSERT INTO posts {title: 'marker-kept-title'}" }]);

  const destination = path.join(directory, 'emptied.romedb');
  await db.exportFile(destination, { emptyTables: ['sessions'] });
  const bytes = await fs.readFile(destination);
  assert.equal(bytes.includes('marker-session-token'), false);
  assert.equal(bytes.includes('marker-kept-title'), true);

  const { body, size } = await db.exportFile({ emptyTables: ['sessions'] });
  const streamed = Buffer.from(await new Response(body).arrayBuffer());
  assert.equal(streamed.length, size);
  assert.equal(streamed.includes('marker-session-token'), false);

  const copy = await openEmbedded({ path: destination });
  t.after(() => copy.close());
  const count = async sql => (await copy.read([{ sql }])).results[0].rows[0][0];
  assert.equal(await count('SELECT count() FROM sessions'), 0n);
  assert.equal(await count('SELECT count() FROM posts'), 1n);
  assert.notEqual((await copy.info()).id, (await db.info()).id);
  await copy.writeAtomic([{ sql: "INSERT INTO sessions {token: 'marker-session-token'}" }]);

  await assert.rejects(db.exportFile(path.join(directory, 'unknown.romedb'), { emptyTables: ['missing'] }), { code: 'RDB_VALIDATION' });
  await assert.rejects(db.exportFile({ emptyTables: ['missing'] }), { code: 'RDB_VALIDATION' });
  await assert.rejects(db.exportFile(path.join(directory, 'bad.romedb'), { emptyTables: 'sessions' }), TypeError);
  await assert.rejects(db.exportFile(path.join(directory, 'bad.romedb'), { emptyTables: ['a,b'] }), TypeError);
  await assert.rejects(db.exportFile(path.join(directory, 'bad.romedb'), { newIdentity: 1 }), TypeError);
});

test('exportFile with a new identity keeps the rows of a database with a new id and sequence 0', async t => {
  const { directory, file } = await fixture(t);
  const db = await openEmbedded({ path: file, create: true });
  t.after(() => db.close());
  await db.writeAtomic([{ sql: 'CREATE TABLE posts' }]);
  await db.writeAtomic([{ sql: "INSERT INTO posts {title: 'kept'}" }]);
  const destination = path.join(directory, 'duplicate.romedb');
  await db.exportFile(destination, { newIdentity: true });
  const copy = await openEmbedded({ path: destination });
  t.after(() => copy.close());
  const info = await copy.info();
  assert.notEqual(info.id, (await db.info()).id);
  assert.equal(info.sequence, 0);
  assert.equal((await copy.read([{ sql: 'SELECT count() FROM posts' }])).results[0].rows[0][0], 1n);
});

test('exportFile streams a copy that leaves nothing on disk and stops when cancelled', async t => {
  const { directory, file } = await fixture(t);
  const db = await openEmbedded({ path: file, create: true });
  t.after(() => db.close());
  await fillPosts(db, 100);
  const before = await names(directory);
  const { sequence, size, body } = await db.exportFile();
  assert.equal(sequence, db.sequence);
  assert.deepEqual(await names(directory), before);
  const bytes = new Uint8Array(await new Response(body).arrayBuffer());
  assert.equal(bytes.byteLength, size);
  const restored = path.join(directory, 'restored.romedb');
  await fs.writeFile(restored, bytes, { mode: 0o600 });
  const copy = await openEmbedded({ path: restored });
  try { assert.deepEqual((await copy.read([{ sql: 'SELECT count() FROM posts' }])).results[0].rows, [[100n]]); }
  finally { await copy.close(); }
  for (const name of await names(directory)) if (name.startsWith('restored.')) await fs.rm(path.join(directory, name));
  const cancelled = new AbortController();
  const pending = db.exportFile(path.join(directory, 'cancelled.romedb'), { signal: cancelled.signal });
  cancelled.abort();
  await assert.rejects(pending, error => error.code === 'RDB_CANCELLED');
  const late = await db.exportFile({ signal: cancelled.signal }).catch(error => error);
  assert.equal(late.code, 'RDB_CANCELLED');
  const drained = await db.exportFile();
  await drained.body.cancel();
  assert.deepEqual((await names(directory)).filter(name => !name.startsWith('app.')), []);
});

test('closing an embedded database cancels its running exports first', async t => {
  const { directory, file } = await fixture(t);
  const db = await openEmbedded({ path: file, create: true });
  await fillPosts(db, 2500);
  const destination = path.join(directory, 'copy.romedb');
  const exporting = db.exportFile(destination).then(() => 'finished', error => error.code);
  await db.close();
  await assert.rejects(db.exportFile(), error => error.code === 'RDB_CLOSED' && error.outcome === 'not-sent');
  const outcome = await exporting;
  assert.ok(['RDB_CANCELLED', 'finished'].includes(outcome), outcome);
  assert.deepEqual((await names(directory)).filter(name => !name.startsWith('app.')), outcome === 'finished' ? ['copy.romedb'] : []);
});
