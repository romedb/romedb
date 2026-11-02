import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { connect, ClientError, Float64, Record, Vector, encodeValue, decodeValue, saveRequest } from './client.mjs';
import * as values from './values.mjs';

const local = 'http://127.0.0.1:4321';
const cloud = 'https://cloud.romedb.com/v1/organizations/11111111-1111-4111-8111-111111111111/databases/22222222-2222-4222-8222-222222222222';
const details = sequence => ({ id: 'local-test', sequence, serviceVersion: 1, capabilities: { readVersion: 2, atomicWrites: true, replayWindow: 64, exactSequenceCondition: true }, limits: { maxRequestBytes: 65_536 } });
const json = (body, status = 200, headers = {}) => Response.json(body, { status, headers });

test('the client and value sources use only Web APIs, so Workers and browsers can load them', async () => {
  for (const file of ['client.cjs', 'values.cjs', 'client.mjs', 'values.mjs']) {
    assert.doesNotMatch(await readFile(new URL(file, import.meta.url), 'utf8'), /node:|\bBuffer\b|\bprocess\.|\bsetImmediate\b/, file);
  }
  assert.equal(Float64, values.Float64, 'client.mjs and values.mjs share one set of value classes');
  assert.equal(Record, values.Record);
});

test('values encode and decode without Buffer, and binaries decode as plain Uint8Array', () => {
  const bytes = new Uint8Array([0, 1, 127, 128, 255]);
  for (const base64 of [false, true]) {
    const decoded = decodeValue(encodeValue(bytes, 0, base64));
    assert.equal(decoded.constructor, Uint8Array);
    assert.deepEqual(decoded, bytes);
  }
  assert.deepEqual(encodeValue(new Float64(-0)), { type: 'Number', value: '8000000000000000' });
  assert.ok(Object.is(decodeValue({ type: 'Number', value: '8000000000000000' }).value, -0));
  assert.equal(decodeValue(encodeValue(new Float64(Math.PI))).value, Math.PI);
  const vector = decodeValue(encodeValue(Vector.float64([1.5, -2]), 0, true));
  assert.equal(vector.bytes.constructor, Uint8Array);
  assert.deepEqual(vector.bytes, Vector.float64([1.5, -2]).bytes);
  assert.deepEqual(decodeValue(encodeValue(new Record('posts', 7n))), new Record('posts', 7n));
  assert.throws(() => decodeValue({ type: 'Binary', value: 'AQI' }), /base64/);
  assert.throws(() => decodeValue({ type: 'Binary', value: 'AQJ=' }), /base64/);
});

test('fetch is called unbound with manual redirects, and the default fetch keeps globalThis', async () => {
  const calls = [];
  const fetch = function (url, init) {
    calls.push({ self: this, redirect: init.redirect });
    if (init.method === 'GET') return json(details(2));
    const request = JSON.parse(init.body);
    return json({ readVersion: 2, requestId: request.requestId, sequence: 2, results: [{ columns: ['b'], rows: [[encodeValue(new Uint8Array([9]), 0, true)]], affected: '0' }] });
  };
  const db = await connect({ url: local, token: 'test', fetch });
  try {
    assert.equal(db.sequenceKnown, true);
    assert.equal(db.cloud, false);
    const rows = (await db.read([{ sql: 'SELECT $b', params: { $b: new Uint8Array([9]) } }])).results[0].rows;
    assert.deepEqual(rows, [[new Uint8Array([9])]]);
    assert.deepEqual(calls, [{ self: undefined, redirect: 'manual' }, { self: undefined, redirect: 'manual' }]);
  } finally { await db.close(); }

  const original = globalThis.fetch;
  globalThis.fetch = function (url, init) { calls.push({ self: this }); return fetch(url, init); };
  try {
    calls.length = 0;
    await (await connect({ url: local, token: 'test' })).close();
    assert.equal(calls[0].self, globalThis);
  } finally { globalThis.fetch = original; }
});

test('a redirect is refused without following it or sending the credential on', async () => {
  let posts = 0;
  const db = await connect({ url: local, token: 'test', fetch: async (_url, init) => {
    if (init.method === 'GET') return json(details(0));
    posts++;
    return new Response(null, { status: 307, headers: { location: 'https://elsewhere.example/v1/database/query' } });
  } });
  try {
    await assert.rejects(db.read([{ sql: 'SELECT 1' }]), error => error instanceof ClientError && error.code === 'FDB_PROTOCOL' && error.status === 307 && error.outcome === 'read-failed');
    const write = db.prepareWrite([{ sql: 'INSERT INTO t VALUES(1)' }]);
    await assert.rejects(db.submit(write), error => error.code === 'FDB_PROTOCOL' && error.outcome === 'unknown' && error.request === write);
    assert.equal(posts, 2);
  } finally { await db.close(); }
  const opaque = await connect({ url: local, token: 'test', fetch: async (_url, init) => init.method === 'GET' ? json(details(0)) : Object.defineProperty(new Response(null, { status: 200 }), 'type', { value: 'opaqueredirect' }) });
  try { await assert.rejects(opaque.read([{ sql: 'SELECT 1' }]), error => error.code === 'FDB_PROTOCOL' && /redirect/.test(error.message)); }
  finally { await opaque.close(); }
});

test('HTTP errors report Retry-After as retryAfterMs, and transport errors keep their cause', async () => {
  const date = new Date(Date.now() + 60_000).toUTCString();
  for (const [header, expected] of [['3', 3000], ['0.5', 500], [date, ms => ms > 50_000 && ms <= 60_000], ['soon', undefined], [null, undefined]]) {
    const db = await connect({ url: local, token: 'test', fetch: async (_url, init) => init.method === 'GET' ? json(details(0)) : json({ error: { code: 'FDB_LIMIT', message: 'busy' } }, 429, header === null ? {} : { 'retry-after': header }) });
    try {
      await assert.rejects(db.read([{ sql: 'SELECT 1' }]), error => {
        assert.equal(error.status, 429);
        if (typeof expected === 'function') assert.ok(expected(error.retryAfterMs), String(error.retryAfterMs));
        else assert.equal(error.retryAfterMs, expected, String(header));
        return true;
      });
    } finally { await db.close(); }
  }
  const refused = new TypeError('fetch failed', { cause: { code: 'ECONNREFUSED' } });
  const db = await connect({ url: local, token: 'test', fetch: async (_url, init) => { if (init.method === 'GET') return json(details(0)); throw refused; } });
  try {
    await assert.rejects(db.read([{ sql: 'SELECT 1' }]), error => error.code === 'FDB_TRANSPORT' && error.cause === refused && !Object.keys(error).includes('cause'));
  } finally { await db.close(); }
});

test('sequenceKnown starts false on a deferred Cloud connection and cloud is true', async () => {
  const db = await connect({ url: cloud, token: 'test', warm: false, fetch: async () => json({ id: '22222222-2222-4222-8222-222222222222', sequence: 4 }) });
  try {
    assert.equal(db.cloud, true);
    assert.equal(db.sequenceKnown, false);
    await db.info();
    assert.equal(db.sequenceKnown, true);
    assert.equal(db.sequence, 4);
  } finally { await db.close(); }
});

test('saveRequest needs Node.js file access outside Node', async () => {
  await assert.rejects(saveRequest('/tmp/request.json', {}), /Node\.js file access/);
});
