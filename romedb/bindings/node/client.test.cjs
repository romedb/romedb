'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { createServer } = require('node:http');
const { once } = require('node:events');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs/promises');
const path = require('node:path');
const { connect, ClientError, Float64, Record, Vector, encodeValue, decodeValue, normalizeUrl, saveRequest, loadRequest, validatePrepared } = require('./client.cjs');
const cloud = 'https://cloud.romedb.com/v1/organizations/11111111-1111-4111-8111-111111111111/databases/22222222-2222-4222-8222-222222222222';
const result = (value = 1n) => ({ columns: ['value'], rows: [[encodeValue(value)]], affected: '0' });
const details = sequence => ({ id: 'local-test', sequence, serviceVersion: 1, capabilities: { readVersion: 2, atomicWrites: true, replayWindow: 64, exactSequenceCondition: true }, limits: { maxRequestBytes: 65_536 } });

test('a shared transport failure preserves each pending write and its original outcome', async () => {
  const failure = new ClientError('FDB_TRANSPORT', 'shared failure', 'unknown');
  let discovered = false;
  const db = await connect({ url: 'http://127.0.0.1:4321', token: 'test-token', fetch: async (url, options) => {
    if (!discovered) { discovered = true; return json(details(0)); }
    throw failure;
  } });
  try {
    const first = db.prepareWrite([{ sql: 'INSERT INTO data VALUES(1)' }]);
    const second = db.prepareWrite([{ sql: 'INSERT INTO data VALUES(2)' }]);
    const replies = await Promise.allSettled([db.submit(first), db.submit(second)]);
    assert.equal(replies[0].reason.request, first);
    assert.equal(replies[1].reason.request, second);
    await assert.rejects(db.info(), error => error.outcome === 'read-failed');
    assert.equal(failure.outcome, 'unknown');
    assert.equal(failure.request, undefined);
    await assert.rejects(db.submit(first), error => error.outcome === 'unknown' && error.request === first);
  } finally { await db.close(); }
});

test('a transport exception cannot manufacture a confirmed rollback receipt', async () => {
  let discovered = false;
  const db = await connect({ url: 'http://127.0.0.1:4321', token: 'test-token', fetch: async () => {
    if (!discovered) { discovered = true; return json(details(0)); }
    throw new ClientError('FDB_CONSTRAINT', 'unconfirmed failure with test-token', 'rolled-back', { sequence: 1 });
  } });
  try {
    const request = db.prepareWrite([{ sql: 'INSERT INTO data VALUES(1)' }]);
    await assert.rejects(db.submit(request), error => error.outcome === 'unknown' && error.request === request && !error.message.includes('test-token'));
    assert.equal(db.sequence, 0);
  } finally { await db.close(); }
});
const json = (body, status = 200) => Response.json(body, { status });

test('query version 2 confirms writes; unknown versions preserve the exact recovery request', async () => {
  let version = '2';
  const db = await connect({ url: cloud, token: 'test-token', deferInfo: false, warm: false, fetch: async (_url, options) => {
    if (options.method === 'GET') return json({ id: 'cloud-test', sequence: 0 });
    const q = JSON.parse(options.body);
    return Response.json({ requestId: q.requestId, sequence: q.readVersion ? 1 : q.afterSequence + 1,
      ...(q.readVersion ? { readVersion: 2 } : {}), results: [result()] }, { headers: { 'x-romedb-query-version': version } });
  } });
  try {
    assert.equal((await db.writeAtomic([{ sql: 'INSERT INTO data VALUES(1)' }])).sequence, 1);
    version = '4';
    await assert.rejects(db.read([{ sql: 'SELECT 1' }]), error => error.code === 'FDB_PROTOCOL' && error.outcome === 'read-failed');
    const saved = db.prepareWrite([{ sql: 'INSERT INTO data VALUES(2)' }]);
    await assert.rejects(db.submit(saved), error => error.code === 'FDB_PROTOCOL' && error.outcome === 'unknown' && error.request === saved);
    assert.equal(db.sequence, 1);
    version = '2';
    assert.equal((await db.recover(saved)).sequence, 2);
  } finally { await db.close(); }
});

test('HTTP errors retain valid RomeDB codes without confirming an uncertain write', async () => {
  const db = await connect({ url: 'http://localhost:4321', token: 'test-secret', fetch: async (_url, options) => options.method === 'GET' ? json(details(0)) : json({ error: { code: 'FDB_UNSUPPORTED', message: 'unsupported test-secret' } }, 400) });
  try {
    await assert.rejects(db.read([{ sql: 'EXPLAIN QUERY PLAN SELECT 1' }]), error => error.code === 'FDB_UNSUPPORTED' && error.outcome === 'read-failed' && error.message === 'unsupported [redacted]');
    const request = db.prepareWrite([{ sql: 'CREATE TABLE data' }]);
    await assert.rejects(db.submit(request), error => error.code === 'FDB_UNSUPPORTED' && error.outcome === 'unknown' && error.request === request && error.sequence === undefined);
  } finally { await db.close(); }
});

test('typed fresh-read errors preserve frontend codes without claiming write rollback', async () => {
  const db = await connect({ url: 'http://localhost:4321', token: 'test-token', fetch: async (_url, options) => {
    if (options.method === 'GET') return json(details(0));
    const q = JSON.parse(options.body);
    return json({ requestId: q.requestId, readVersion: 2, sequence: 0,
      error: { code: 'FDB_UNSUPPORTED', message: 'vector EXPLAIN unsupported' } });
  } });
  try {
    await assert.rejects(db.read([{ sql: 'EXPLAIN QUERY PLAN SELECT 1' }]),
      error => error.code === 'FDB_UNSUPPORTED' && error.outcome === 'read-failed' && error.sequence === 0);
  } finally { await db.close(); }
});

test('remote entry and vector factories never load the native addon', () => {
  const child = spawnSync(process.execPath, ['-e', `
    const Module = require('node:module');
    const original = Module._load;
    Module._load = function(id, ...args) {
      if (id.endsWith('.node') || /native\\.cjs|worker\\.cjs/.test(id)) throw Error('native load forbidden');
      return original.call(this, id, ...args);
    };
    const { Vector, Float64, encodeValue } = require('@romedb/node/client');
    encodeValue(new Float64(1));
    Vector.float32([1]); Vector.float64([1]); Vector.sparse32Entries(3, [[2,1]]);
    import('@romedb/node/client').then(m => { if (typeof m.connect !== 'function') process.exit(2); });
  `], { cwd: __dirname, encoding: 'utf8' });
  assert.equal(child.status, 0, child.stderr);
});

test('transfer values preserve scalar type, bit patterns, bytes, nested objects, missing and null', () => {
  const values = [null, true, 'ไทย', -(1n << 63n), (1n << 63n) - 1n, new Float64(1), new Float64(-0), new Float64(Number.MIN_VALUE), new Float64(Number.MAX_VALUE), new Record('posts', 'p1'), new Record('posts', -2n), new Uint8Array([0, 255]), Vector.float32([1, -2]), Vector.float64([1, -0]), Vector.sparse32Entries(4, [[0, 1], [3, -1]]), { type: 'Integer', value: 'text', present: null, nested: [{ x: 1n }] }];
  for (const value of values) {
    const wire = encodeValue(value);
    assert.deepEqual(encodeValue(decodeValue(wire)), wire);
  }
  assert.equal(Object.hasOwn(decodeValue(encodeValue({ present: null })), 'missing'), false);
  assert.equal(decodeValue(encodeValue({ present: null })).present, null);
  const protectedObject = decodeValue(JSON.parse('{"type":"Object","value":{"__proto__":{"type":"String","value":"safe"}}}'));
  assert.equal(Object.getPrototypeOf(protectedObject), Object.prototype);
  assert.equal(protectedObject.__proto__, 'safe');
  assert.equal(Object.is(decodeValue(encodeValue(new Float64(-0))).value, -0), true);
});

test('invalid values and malformed server tags fail without silent coercion', () => {
  for (const value of [undefined, NaN, Infinity, 2 ** 63, 1n << 63n, new Date(), [, 1], { x: undefined }]) assert.throws(() => encodeValue(value));
  for (const wire of [{ type: 'Null', value: null }, { type: 'Integer', value: '01' }, { type: 'Integer', value: '-0' }, { type: 'Integer', value: '9223372036854775808' }, { type: 'Number', value: '7ff0000000000000' }, { type: 'Number', value: '3FF0000000000000' }, { type: 'Boolean', value: 1 }, { type: 'Binary', value: [256] }, { type: 'Record', value: { table: 'posts', key: { type: 'Number', value: '3ff0000000000000' } } }, { type: 'Object', value: [], extra: true }]) assert.throws(() => decodeValue(wire));
  let deep = null;
  for (let i = 0; i < 65; i++) deep = [deep];
  assert.throws(() => encodeValue(deep), /nesting/);
  assert.throws(() => Vector.float32([Number.MAX_VALUE]), /float32/);
  assert.throws(() => Vector.sparse32Entries(3, [[1, 1], [0, 2]]), /increasing/);
  const bytes = Uint8Array.from([1, 0, 0, 0]);
  const vector = new Vector(bytes);
  bytes[0] = 3;
  vector.bytes[0] = 4;
  assert.equal(vector.bytes[0], 1);
});

test('URLs map local origins and Cloud query URLs without credential redirects', () => {
  assert.equal(normalizeUrl('http://127.0.0.1:4321/'), 'http://127.0.0.1:4321/v1/database');
  assert.equal(normalizeUrl(cloud + '/query'), cloud);
  assert.equal(normalizeUrl('http://[::1]:4321'), 'http://[::1]:4321/v1/database');
  for (const url of ['http://example.com/v1/database', 'https://user:secret@example.com/v1/database', 'http://localhost:4321?token=secret', 'http://localhost:4321/#token', 'https://cloud.romedb.com/', cloud + '/tokens']) assert.throws(() => normalizeUrl(url));
});

test('read-v2 keeps duplicate columns, bigint affected counts, and monotonic confirmed sequence', async () => {
  const requests = [];
  const db = await connect({ url: 'http://localhost:4321', token: 'test-secret', fetch: async (url, options) => {
    assert.equal(options.redirect, 'error');
    assert.equal(options.headers.authorization, 'Bearer test-secret');
    if (options.body !== undefined) assert.equal(options.headers['x-romedb-database-id'], 'local-test');
    if (options.method === 'GET') return json({ ...details(3), engineVersion: '2.7.0' });
    const request = JSON.parse(options.body);
    requests.push(request);
    return json({ readVersion: 2, requestId: request.requestId, sequence: 5, results: [{ columns: ['x', 'x'], rows: [[encodeValue(1n), encodeValue(new Float64(1))]], affected: '18446744073709551615' }] });
  } });
  const reply = await db.read([{ sql: 'SELECT $x, $x', params: { $x: 1n } }]);
  assert.equal(reply.results[0].affected, (1n << 64n) - 1n);
  assert.deepEqual(reply.results[0].columns, ['x', 'x']);
  assert.equal(reply.results[0].rows[0][1].value, 1);
  assert.equal(requests[0].afterSequence, 3);
  assert.equal((await db.info()).engineVersion, '2.7.0');
  assert.equal(db.sequence, 5);
  await assert.rejects(db.read([{ sql: 'SELECT 1' }], { ifSequence: 1 }), /ifSequence/);
  await db.close();
});

test('database identities must fit a bounded header-safe value', async () => {
  for (const id of ['', 'x'.repeat(81), 'line\nbreak', 'nonascii\u03bb']) {
    await assert.rejects(connect({ url: 'http://localhost:4321', token: 'test-secret', fetch: async () => json({ ...details(0), id }) }), error => error.code === 'FDB_PROTOCOL');
  }
});

test('Cloud adapter uses the same read/write calls and marks capabilities as documented', async () => {
  const sent = [];
  const db = await connect({ url: cloud + '/query', token: 'jwt-fixture', fetch: async (url, options) => {
    sent.push(url);
    if (url.endsWith('/warm')) return json({ state: 'starting' });
    if (options.method === 'GET') return json({ id: '22222222-2222-4222-8222-222222222222', sequence: 0 });
    const request = JSON.parse(options.body);
    return json({ requestId: request.requestId, sequence: request.readVersion === 2 ? 0 : 1, ...(request.readVersion ? { readVersion: 2 } : {}), results: [result()] });
  } });
  assert.equal((await db.info()).capabilities.provenance, 'documented');
  await db.read([{ sql: 'SELECT 1' }]);
  await db.writeAtomic([{ sql: 'SELECT 1' }]);
  assert.deepEqual(sent.slice(-2), [cloud + '/read', cloud + '/query']);
  await db.close();
});

test('lost write response recovers the exact saved body and never creates another request', async () => {
  const attempts = [];
  let published = false;
  const db = await connect({ url: 'http://localhost:4321', token: 'test', fetch: async (_url, options) => {
    if (options.method === 'GET') return json(details(0));
    attempts.push(options.body);
    const request = JSON.parse(options.body);
    if (!published) { published = true; throw Error('response lost after publication'); }
    return json({ requestId: request.requestId, sequence: 1, results: [result()] });
  } });
  const statements = [{ sql: 'INSERT INTO posts DOCUMENT $post', params: { $post: { title: 'Original' } } }];
  const prepared = db.prepareWrite(statements);
  statements[0].params.$post.title = 'Mutated after preparation';
  let failure;
  try { await db.submit(prepared); } catch (error) { failure = error; }
  assert.equal(failure.outcome, 'unknown');
  assert.equal(failure.request, prepared);
  assert.equal(JSON.parse(prepared.body).statements[0].params.$post.value.title.value, 'Original');
  assert.equal((await db.recover(prepared)).sequence, 1);
  assert.equal(attempts.length, 2);
  assert.equal(attempts[0], attempts[1]);
  await db.close();
});

test('unidentified write errors, including recovery rejections, remain unknown and redact bearer tokens', async () => {
  const db = await connect({ url: 'http://localhost:4321', token: 'test-secret', fetch: async (_url, options) => options.method === 'GET' ? json(details(0)) : json({ error: 'rejected test-secret' }, 409) });
  const prepared = db.prepareWrite([{ sql: 'SELECT 1' }]);
  await assert.rejects(db.submit(prepared), error => error.code === 'FDB_CONFLICT' && error.outcome === 'unknown' && !error.message.includes('test-secret'));
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(db.recover(prepared, { signal: controller.signal }), error => error.outcome === 'unknown' && error.request === prepared);
  await db.close();
});

test('valid failure receipts prove rollback and update the confirmed sequence', async () => {
  const db = await connect({ url: 'http://localhost:4321', token: 'test', fetch: async (_url, options) => {
    if (options.method === 'GET') return json(details(0));
    const request = JSON.parse(options.body);
    return json({ requestId: request.requestId, sequence: 1, error: { code: 'FDB_VALIDATION', message: 'title required' } }, 400);
  } });
  await assert.rejects(db.writeAtomic([{ sql: 'INSERT INTO posts DOCUMENT {}' }]), error => error.outcome === 'rolled-back' && error.code === 'FDB_VALIDATION' && error.sequence === 1);
  assert.equal(db.sequence, 1);
  await db.close();
});

test('mismatched identity, wrong result width, extra results and unsafe sequence cannot confirm writes', async () => {
  for (const corrupt of [reply => ({ ...reply, requestId: 'different' }), reply => ({ ...reply, sequence: 65 }), reply => ({ ...reply, results: [{ columns: [], rows: [[encodeValue(1n)]], affected: '0' }] }), reply => ({ ...reply, results: [result(), result()] }), reply => ({ ...reply, results: [{ ...result(), affected: '01' }] })]) {
    const db = await connect({ url: 'http://localhost:4321', token: 'test', fetch: async (_url, options) => {
      if (options.method === 'GET') return json(details(0));
      const request = JSON.parse(options.body);
      return json(corrupt({ requestId: request.requestId, sequence: 1, results: [result()] }));
    } });
    await assert.rejects(db.writeAtomic([{ sql: 'SELECT 1' }]), error => error.code === 'FDB_PROTOCOL' && error.outcome === 'unknown');
    assert.equal(db.sequence, 0);
    await db.close();
  }
});

test('request preparation rejects oversized input, unknown fields and changed database identities', async () => {
  const db = await connect({ url: 'http://localhost:4321', token: 'test', fetch: async () => json(details(0)) });
  assert.throws(() => db.prepareWrite([{ sql: 'SELECT $x', params: { $x: 'x'.repeat(65_536) } }]), /64 KiB/);
  assert.throws(() => db.prepareWrite([{ sql: 'SELECT 1', arbitrary: 1 }]), /statement/);
  assert.throws(() => db.prepareWrite([{ sql: 'SELECT 1' }], { afterSequence: 2, ifSequence: 1 }), /ifSequence/);
  const write = db.prepareWrite([{ sql: 'SELECT 1' }]);
  await assert.rejects(db.submit({ ...write, databaseId: 'another' }), error => error.code === 'FDB_REQUEST_IDENTITY' && error.outcome === 'not-sent');
  await db.close();
  assert.throws(() => db.prepareWrite([{ sql: 'SELECT 1' }]), error => error.code === 'FDB_CLOSED');
});

test('private journals survive a new client, preserve exact body, and reject overwrite, symlinks and insecure modes', async t => {
  const root = path.resolve(__dirname, '../../../..', '.local/tmp');
  await fs.mkdir(root, { recursive: true });
  const dir = await fs.mkdtemp(path.join(root, 'studio-client-'));
  t.after(() => fs.rm(dir, { recursive: true, force: true }));
  const db = await connect({ url: 'http://localhost:4321', token: 'test-secret', fetch: async () => json(details(0)) });
  const prepared = db.prepareWrite([{ sql: 'SELECT $x', params: { $x: new Record('posts', 'p1') } }]);
  const file = path.join(dir, 'request.json');
  await saveRequest(file, prepared);
  assert.equal((await fs.stat(file)).mode & 0o777, 0o600);
  assert.equal((await fs.readFile(file, 'utf8')).includes('test-secret'), false);
  assert.deepEqual(await loadRequest(file), prepared);
  await assert.rejects(saveRequest(file, prepared), { code: 'EEXIST' });
  const link = path.join(dir, 'link.json');
  await fs.symlink(file, link);
  await assert.rejects(loadRequest(link));
  await fs.chmod(file, 0o644);
  await assert.rejects(loadRequest(file), /private/);
  await fs.chmod(file, 0o600);
  const newer = await connect({ url: 'http://localhost:4321', token: 'different-token', fetch: async (_url, options) => {
    if (options.method === 'GET') return json(details(2));
    assert.equal(options.body, prepared.body);
    const request = JSON.parse(options.body);
    return json({ requestId: request.requestId, sequence: 1, results: [result()] });
  } });
  await newer.recover(await loadRequest(file));
  assert.equal(newer.sequence, 2);
  await newer.close();
  await db.close();
});

test('a journal saved for the largest escaped request body can be loaded', async t => {
  const root = path.resolve(__dirname, '../../../..', '.local/tmp');
  await fs.mkdir(root, { recursive: true });
  const dir = await fs.mkdtemp(path.join(root, 'studio-client-'));
  t.after(() => fs.rm(dir, { recursive: true, force: true }));
  const db = await connect({ url: cloud, token: 'test-token', deferInfo: false, warm: false, fetch: async () => json({ id: 'cloud-test', sequence: 0 }) });
  const prepared = db.prepareWrite([{ sql: 'SELECT $x', params: { $x: '\\'.repeat(32_690) } }]);
  const file = path.join(dir, 'request.json');
  await saveRequest(file, prepared);
  assert.ok((await fs.stat(file)).size > 131_072);
  assert.deepEqual(await loadRequest(file), prepared);
  await db.close();
});

test('real HTTP cancellation after dispatch leaves a write unknown; close cancels pending work', async t => {
  let received;
  const admitted = new Promise(resolve => { received = resolve; });
  const server = createServer(async (req, res) => {
    if (req.method === 'GET') { res.setHeader('content-type', 'application/json'); res.end(JSON.stringify(details(0))); return; }
    for await (const _ of req) { }
    received();
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => { server.closeAllConnections(); server.close(); });
  const db = await connect({ url: `http://127.0.0.1:${server.address().port}`, token: 'test' });
  const controller = new AbortController();
  const prepared = db.prepareWrite([{ sql: 'SELECT 1' }]);
  const pending = assert.rejects(db.submit(prepared, { signal: controller.signal }), error => error.code === 'FDB_CANCELLED' && error.outcome === 'unknown');
  await admitted;
  controller.abort();
  await pending;
  const closing = assert.rejects(db.submit(prepared), error => error.outcome === 'unknown');
  await db.close();
  await closing;
});

test('response streaming is bounded even without a content length', async () => {
  let cancelled = false;
  const db = await connect({ url: 'http://localhost:4321', token: 'test', fetch: async (_url, options) => options.method === 'GET' ? json(details(0)) : new Response(new ReadableStream({
    pull(controller) { controller.enqueue(new Uint8Array(65_536)); },
    cancel() { cancelled = true; },
  }), { headers: { 'content-type': 'application/json' } }) });
  await assert.rejects(db.writeAtomic([{ sql: 'SELECT 1' }]), error => error.code === 'FDB_PROTOCOL' && error.outcome === 'unknown');
  assert.equal(cancelled, true);
  await db.close();
});

test('client request timeout bounds fetch and pre-aborted writes never dispatch', async () => {
  let calls = 0;
  const db = await connect({ url: 'http://localhost:4321', token: 'test', fetch: async (_url, options) => {
    if (options.method === 'GET') return json(details(0));
    calls++;
    return new Promise((_resolve, reject) => options.signal.addEventListener('abort', () => reject(Error('aborted')), { once: true }));
  } });
  const prepared = db.prepareWrite([{ sql: 'SELECT 1' }]);
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(db.submit(prepared, { signal: controller.signal }), error => error.outcome === 'not-sent');
  assert.equal(calls, 0);
  await assert.rejects(db.submit(prepared, { timeoutMs: 5 }), error => error.code === 'FDB_CANCELLED' && error.outcome === 'unknown');
  assert.equal(calls, 1);
  await db.close();
});

test('unsupported capabilities and a changed database identity fail before mutation dispatch', async () => {
  let queries = 0;
  let info = { ...details(0), capabilities: { readVersion: 2, atomicWrites: false, exactSequenceCondition: false } };
  const db = await connect({ url: 'http://localhost:4321', token: 'test', fetch: async (_url, options) => {
    if (options.method === 'GET') return json(info);
    queries++;
    throw Error('unexpected dispatch');
  } });
  assert.throws(() => db.prepareWrite([{ sql: 'SELECT 1' }]), error => error.code === 'FDB_UNSUPPORTED' && error.outcome === 'not-sent');
  info = { ...details(0), id: 'replacement-database' };
  await assert.rejects(db.info(), error => error.code === 'FDB_PROTOCOL');
  assert.equal(db.databaseId, 'local-test');
  assert.equal(queries, 0);
  await db.close();
});

test('result JSON budget is checked before decoding even inside the larger envelope budget', async () => {
  const db = await connect({ url: 'http://localhost:4321', token: 'test', fetch: async (_url, options) => {
    if (options.method === 'GET') return json(details(0));
    const request = JSON.parse(options.body);
    return json({ requestId: request.requestId, sequence: 1, results: [result('x'.repeat(262_144))] });
  } });
  await assert.rejects(db.writeAtomic([{ sql: 'SELECT 1' }]), error => error.code === 'FDB_PROTOCOL' && error.outcome === 'unknown');
  await db.close();
});

test('a response racing close still redacts the credential captured at dispatch', async () => {
  let release;
  const db = await connect({ url: 'http://localhost:4321', token: 'test-secret', fetch: async (_url, options) => {
    if (options.method === 'GET') return json(details(0));
    return new Promise(resolve => { release = () => resolve(json({ error: 'rejected test-secret' }, 400)); });
  } });
  const response = assert.rejects(db.writeAtomic([{ sql: 'SELECT 1' }]), error => error.outcome === 'unknown' && !error.message.includes('test-secret'));
  await db.close();
  release();
  await response;
});

test('work limits are bounded and remain part of the exact prepared write envelope', async () => {
  const db = await connect({ url: 'http://localhost:4321', token: 'test', fetch: async (_url, options) => {
    if (options.method === 'GET') return json(details(0));
    const request = JSON.parse(options.body);
    assert.deepEqual(request.workLimits, { rowsRead: 100, rowMutations: 0, vmSteps: 1000 });
    return json({ readVersion: 2, requestId: request.requestId, sequence: 0, results: [result()] });
  } });
  const workLimits = { vmSteps: 1000, rowsRead: 100, rowMutations: 0 };
  const prepared = db.prepareWrite([{ sql: 'SELECT 1' }], { workLimits });
  workLimits.rowsRead = 200;
  assert.deepEqual(JSON.parse(prepared.body).workLimits, { rowsRead: 100, rowMutations: 0, vmSteps: 1000 });
  await db.read([{ sql: 'SELECT 1' }], { workLimits: { rowsRead: 100, rowMutations: 0, vmSteps: 1000 } });
  for (const limits of [{ rowsRead: 1_000_001 }, { rowMutations: -1 }, { vmSteps: Infinity }, { unknown: 1 }, { rowsRead: undefined }]) assert.throws(() => db.prepareWrite([{ sql: 'SELECT 1' }], { workLimits: limits }), /workLimits/);
  await db.close();
});

test('historical Cloud Record receipt keys decode without changing new request encoding', () => {
  for (const [key, expected] of [[{ String: 'p1' }, 'p1'], [{ Integer: '-9223372036854775808' }, -(1n << 63n)]]) {
    const decoded = decodeValue({ type: 'Record', value: { table: 'posts', key } });
    assert.deepEqual(decoded, new Record('posts', expected));
    assert.deepEqual(encodeValue(decoded).value.key, encodeValue(expected));
  }
  for (const key of [{ Integer: '01' }, { Integer: '9223372036854775808' }, { String: 'a', Integer: '1' }, { Number: '3ff0000000000000' }]) {
    assert.throws(() => decodeValue({ type: 'Record', value: { table: 'posts', key } }));
  }
});

test('romedb:// is an alias for https:// with the same URL checks', () => {
  const alias = cloud.replace('https://', 'romedb://');
  assert.equal(normalizeUrl(alias), cloud);
  assert.equal(normalizeUrl(alias + '/query'), cloud);
  assert.equal(normalizeUrl('romedb://127.0.0.1/v1/database'), 'https://127.0.0.1/v1/database');
  for (const url of ['romedb://user:secret@cloud.romedb.com/v1/organizations/11111111-1111-4111-8111-111111111111/databases/22222222-2222-4222-8222-222222222222',
    alias + '?authToken=secret', alias + '#token', 'romedb://cloud.romedb.com/', 'romedb:cloud.romedb.com', 'ROMEDB://cloud.romedb.com/v1/database']) assert.throws(() => normalizeUrl(url), url);
});

test('Cloud URLs skip the details request until a write needs a sequence', async () => {
  const calls = [];
  const fetch = async (url, options) => {
    calls.push(`${options.method} ${new URL(url).pathname.split('/').at(-1)}`);
    if (options.method === 'GET') return json({ id: '22222222-2222-4222-8222-222222222222', sequence: 40 });
    const body = JSON.parse(options.body);
    if (body.readVersion === 2) return json({ requestId: body.requestId, readVersion: 2, sequence: 40, results: [result()] }, 200, { 'x-romedb-query-version': '2' });
    return json({ requestId: body.requestId, sequence: body.afterSequence + 1, results: [result()] }, 200, { 'x-romedb-query-version': '2' });
  };
  const db = await connect({ url: cloud.replace('https://', 'romedb://'), token: 'test-token', warm: false, fetch });
  assert.equal(db.deferred, true); assert.deepEqual(calls, [], 'no request at connect');
  assert.equal(db.databaseId, '22222222-2222-4222-8222-222222222222');
  assert.throws(() => db.prepareWrite([{ sql: 'UPDATE t SET n=1' }]), error => error.code === 'FDB_SEQUENCE_UNKNOWN' && error.outcome === 'not-sent');
  const write = await db.writeAtomic([{ sql: 'UPDATE t SET n=1' }]);
  assert.equal(write.sequence, 41); assert.deepEqual(calls, ['GET 22222222-2222-4222-8222-222222222222', 'POST query'], 'the first write fetches details once');
  calls.length = 0;
  const reader = await connect({ url: cloud, token: 'test-token', deferInfo: true, warm: false, fetch });
  await reader.read([{ sql: 'SELECT 1' }]);
  assert.ok(reader.prepareWrite([{ sql: 'UPDATE t SET n=2' }]), 'a read supplies the sequence');
  assert.deepEqual(calls, ['POST read']);
  await assert.rejects(connect({ url: 'http://127.0.0.1:4321', token: 'test-token', deferInfo: true, fetch }), /Cloud database URLs/);
  await assert.rejects(connect({ url: cloud, token: 'test-token', deferInfo: 'yes', fetch }), /boolean/);
  const mismatch = await connect({ url: cloud, token: 'test-token', warm: false, fetch: async () => json({ id: '33333333-3333-4333-8333-333333333333', sequence: 1 }) });
  await assert.rejects(mismatch.info(), error => error.code === 'FDB_PROTOCOL', 'a different database id is still rejected');
  calls.length = 0;
  const eager = await connect({ url: cloud, token: 'test-token', deferInfo: false, warm: false, fetch });
  assert.equal(eager.deferred, false); assert.equal(eager.sequence, 40);
  assert.deepEqual(calls, ['GET 22222222-2222-4222-8222-222222222222'], 'deferInfo: false keeps the details request at connect');
});

test('Cloud connections warm the database in the background; warm() reports the state', async () => {
  const calls = [];
  let state = 'starting';
  const fetch = async (url, options) => {
    calls.push(`${options.method} ${new URL(url).pathname.split('/').at(-1)}`);
    if (url.endsWith('/warm')) {
      assert.equal(options.body, '{}'); assert.equal(options.headers.authorization, 'Bearer test-token');
      assert.equal(options.headers['x-romedb-database-id'], '22222222-2222-4222-8222-222222222222');
      return state === 'missing' ? json({ error: 'Not found' }, 404) : json({ state });
    }
    return json({ id: '22222222-2222-4222-8222-222222222222', sequence: 3 });
  };
  const db = await connect({ url: cloud, token: 'test-token', fetch });
  assert.equal(db.deferred, true);
  assert.deepEqual(calls, ['POST warm'], 'connect sends only the warm request and does not wait for it');
  state = 'ready';
  assert.deepEqual(await db.warm(), { state: 'ready' });
  state = 'skipped';
  assert.deepEqual(await db.warm(), { state: 'skipped' });
  state = 'unknown';
  await assert.rejects(db.warm(), error => error.code === 'FDB_PROTOCOL' && error.outcome === 'read-failed');
  state = 'missing';
  await assert.rejects(db.warm(), error => error.status === 404 && error.outcome === 'read-failed');
  calls.length = 0;
  const failing = await connect({ url: cloud, token: 'test-token', fetch: async () => { throw new Error('network down'); } });
  assert.equal(failing.databaseId, '22222222-2222-4222-8222-222222222222', 'a failed background warm does not fail connect');
  const eager = await connect({ url: cloud, token: 'test-token', deferInfo: false, fetch });
  assert.deepEqual(calls.sort(), ['GET 22222222-2222-4222-8222-222222222222', 'POST warm']);
  calls.length = 0;
  await connect({ url: cloud, token: 'test-token', warm: false, fetch });
  assert.deepEqual(calls, [], 'warm: false sends nothing at connect');
  let aborted;
  const pending = await connect({ url: cloud, token: 'test-token', fetch: (_url, options) => new Promise((_, reject) => {
    options.signal.addEventListener('abort', () => { aborted = true; reject(options.signal.reason); });
  }) });
  await pending.close();
  assert.equal(aborted, true, 'close cancels the background warm');
  const local = await connect({ url: 'http://127.0.0.1:4321', token: 'test-token', fetch: async () => json(details(0)) });
  await assert.rejects(local.warm(), error => error.code === 'FDB_UNSUPPORTED' && error.outcome === 'not-sent');
  await assert.rejects(connect({ url: 'http://127.0.0.1:4321', token: 'test-token', warm: true, fetch }), /Cloud database URLs/);
  await assert.rejects(connect({ url: cloud, token: 'test-token', warm: 'yes', fetch }), /boolean/);
  await Promise.all([db.close(), failing.close(), eager.close(), local.close()]);
});

test('Cloud query version 3: base64 bytes, 1 MiB bodies, saved writes record their version', async () => {
  const sent = [];
  const db = await connect({ url: cloud, token: 'test-token', deferInfo: false, warm: false, fetch: async (_url, options) => {
    if (options.method === 'GET') return json({ id: 'cloud-test', sequence: 0 });
    const q = JSON.parse(options.body);
    sent.push({ header: options.headers['x-romedb-query-version'], body: q, bytes: Buffer.byteLength(options.body) });
    return Response.json({ requestId: q.requestId, sequence: q.readVersion ? 1 : q.afterSequence + 1, ...(q.readVersion ? { readVersion: 2 } : {}),
      results: [{ columns: ['b', 'v'], rows: [[{ type: 'Binary', value: 'AAH/' }, { type: 'Vector', value: [1, 2, 3, 4] }]], affected: '0' }] },
      { headers: { 'x-romedb-query-version': sent.at(-1).header ?? '2' } });
  } });
  try {
    const read = await db.read([{ sql: 'SELECT $b', params: { $b: Buffer.from([0, 1, 255]) } }]);
    assert.equal(sent[0].header, '3');
    assert.deepEqual(sent[0].body.statements[0].params.$b, { type: 'Binary', value: 'AAH/' });
    assert.deepEqual([...read.results[0].rows[0][0]], [0, 1, 255], 'base64 rows decode');
    assert.deepEqual([...read.results[0].rows[0][1].bytes], [1, 2, 3, 4], 'array rows still decode');
    const big = Buffer.alloc(700_000, 7);
    const saved = db.prepareWrite([{ sql: 'INSERT INTO t VALUES($b)', params: { $b: big } }]);
    assert.deepEqual([saved.version, saved.queryVersion], [2, 3]);
    assert.ok(Buffer.byteLength(saved.body) > 65_536 && Buffer.byteLength(saved.body) < 1_048_576);
    await db.submit(saved);
    assert.equal(sent.at(-1).header, '3');
    assert.throws(() => db.prepareWrite([{ sql: 'INSERT INTO t VALUES($b)', params: { $b: Buffer.alloc(800_000) } }]), /exceeds 1 MiB/);
    const legacy = { version: 1, url: saved.url, databaseId: saved.databaseId, body: JSON.stringify({ requestId: 'legacy-write', afterSequence: 0,
      statements: [{ sql: 'INSERT INTO t VALUES($b)', params: { $b: { type: 'Binary', value: [1, 2] } } }] }) };
    await db.submit(legacy);
    assert.equal(sent.at(-1).header, undefined, 'a saved 2.8.0 request replays at version 2');
    assert.throws(() => validatePrepared({ ...saved, queryVersion: 9 }), /invalid saved request/);
  } finally { await db.close(); }
});

test('local services keep version 2: byte arrays and 64 KiB', async () => {
  let header = 'unset', params;
  const db = await connect({ url: 'http://localhost:4321', token: 'test-token', fetch: async (_url, options) => {
    if (options.method === 'GET') return json(details(0));
    header = options.headers['x-romedb-query-version']; const q = JSON.parse(options.body); params = q.statements[0].params;
    return json({ requestId: q.requestId, sequence: 1, readVersion: 2, results: [result()] });
  } });
  try {
    await db.read([{ sql: 'SELECT $b', params: { $b: Buffer.from([9]) } }]);
    assert.equal(header, undefined); assert.deepEqual(params.$b, { type: 'Binary', value: [9] });
    assert.deepEqual(db.prepareWrite([{ sql: 'SELECT 1' }]).queryVersion, 2);
    assert.throws(() => db.prepareWrite([{ sql: 'INSERT INTO t VALUES($b)', params: { $b: Buffer.alloc(40_000) } }]), /exceeds 64 KiB/);
  } finally { await db.close(); }
});

test('base64 byte values must be canonical', () => {
  assert.deepEqual([...decodeValue({ type: 'Binary', value: '' })], []);
  assert.deepEqual([...decodeValue({ type: 'Binary', value: 'AQI=' })], [1, 2]);
  for (const bad of ['AQI', 'AQJ=', 'A Q I=', 'AQI==', '****']) assert.throws(() => decodeValue({ type: 'Binary', value: bad }), /base64/);
  assert.deepEqual(encodeValue(Buffer.from([1, 2]), 0, true), { type: 'Binary', value: 'AQI=' });
  assert.deepEqual(encodeValue(Buffer.from([1, 2])), { type: 'Binary', value: [1, 2] });
});

test('a Cloud queue refusal is retried once; other 503 responses are not', async () => {
  const queueRefusal = () => new Response(JSON.stringify({ error: 'Query queue unavailable', code: 'query_queue_timeout' }), { status: 503, headers: { 'content-type': 'application/json', 'retry-after': '0' } });
  for (const [label, refusals, expected] of [['one refusal', 1, 2], ['two refusals', 2, 2]]) {
    let posts = 0;
    const db = await connect({ url: 'http://localhost:4321', token: 'test', fetch: async (_url, options) => {
      if (options.method === 'GET') return json(details(0));
      posts++;
      if (posts <= refusals) return queueRefusal();
      const request = JSON.parse(options.body);
      return json({ requestId: request.requestId, sequence: 0, readVersion: 2, results: [result()] });
    } });
    if (refusals === 1) await db.read([{ sql: 'SELECT 1' }]);
    else await assert.rejects(db.read([{ sql: 'SELECT 1' }]), error => error.status === 503, label);
    assert.equal(posts, expected, label);
    await db.close();
  }
  let posts = 0;
  const db = await connect({ url: 'http://localhost:4321', token: 'test', fetch: async (_url, options) => {
    if (options.method === 'GET') return json(details(0));
    posts++;
    return json({ error: 'Request not acknowledged' }, 503);
  } });
  await assert.rejects(db.read([{ sql: 'SELECT 1' }]), error => error.status === 503);
  assert.equal(posts, 1, 'an ordinary 503 is not retried');
  await db.close();
});
