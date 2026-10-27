'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { NativeDatabase } = require('./native.cjs');
const native = require('./index.cjs');
const { Float64, Record, Vector, encodeValue, decodeValue } = require('./client.cjs');

test('remote codec values round-trip through the real checked native frontend', () => {
  const db = new NativeDatabase(':memory:');
  try {
    const values = [-(1n << 63n), (1n << 63n) - 1n, new Float64(1), new Float64(-0), new Float64(Number.MIN_VALUE), new Float64(Number.MAX_VALUE), new Record('posts', 'p1'), new Record('posts', 1n), new Record('Thai ไทย posts', 'p1'), new Uint8Array([0, 255]), Vector.float32([1, -2]), Vector.float64([1, -0]), Vector.sparse32Entries(4, [[0, 1], [3, -1]]), { present: null, type: 'Number', nested: [{ x: 1n }] }];
    for (const value of values) {
      const wire = encodeValue(value);
      const report = JSON.parse(db.execute('SELECT $x AS x', JSON.stringify({ $x: wire })));
      assert.equal(report.execution.error, undefined);
      assert.deepEqual(encodeValue(decodeValue(report.execution.result.rows[0][0])), wire);
    }
  } finally { db.close(); }
});

test('native-free dense and sparse vector constructors match the released native factories', () => {
  for (const values of [[1, -2, 0, -0], [1.25, 3.5], [0, 0], [Number.MIN_VALUE, Math.fround(1e-30)]]) {
    assert.deepEqual(Vector.float32(values).bytes, native.Vector.float32(values).bytes);
    assert.deepEqual(Vector.float64(values).bytes, native.Vector.float64(values).bytes);
  }
  for (const entries of [[], [[0, 1], [3, -1]], [[0, 0], [1, -0], [3, Number.MIN_VALUE]]]) assert.deepEqual(Vector.sparse32Entries(4, entries).bytes, native.Vector.sparse32Entries(4, entries).bytes);
});

test('managed native batches bind portable values and restore all writes after failure', () => {
  const db = new NativeDatabase(':memory:');
  const call = (statements, readOnly = false, workLimits) => JSON.parse(db.executeAtomicRequest(JSON.stringify({ statements, readOnly, ...(workLimits ? { workLimits } : {}) })));
  try {
    const document = { id: new Record('docs', 'a'), number: (1n << 63n) - 1n, float: new Float64(-0), vector: Vector.float32([1, -2]), binary: new Uint8Array([0, 255]), present: null, nested: [{ type: 'Integer', value: 'ordinary' }] };
    const report = call([
      { sql: 'CREATE TABLE docs' },
      { sql: 'INSERT INTO docs DOCUMENT $doc', params: { $doc: encodeValue(document) } },
      { sql: 'SELECT $doc AS same, $doc AS same', params: { $doc: encodeValue(document) } },
    ]);
    assert.equal(report.execution.error, undefined);
    assert.deepEqual(report.transaction, { before: 'autocommit', after: 'autocommit' });
    assert.deepEqual(report.execution.result.results[2].columns, ['same', 'same']);
    for (const value of report.execution.result.results[2].rows[0]) assert.deepEqual(encodeValue(decodeValue(value)), encodeValue(document));
    const failure = call([{ sql: "INSERT INTO docs {id:docs:b,number:2}" }, { sql: 'INSERT INTO docs DOCUMENT $missing' }]);
    assert.equal(failure.execution.error.code, 'FDB_PARAMETER');
    assert.equal(failure.transaction.after, 'autocommit');
    const rows = call([{ sql: 'SELECT number FROM docs' }], true).execution.result.results[0].rows;
    assert.equal(rows.length, 1);
    assert.equal(decodeValue(rows[0][0]), (1n << 63n) - 1n);
    assert.equal(call([{ sql: 'DELETE FROM docs' }], true).execution.error.code, 'FDB_UNSUPPORTED');
    const limited = call([{ sql: 'DELETE FROM docs' }], false, { rowMutations: 0 });
    assert.equal(limited.execution.error.code, 'FDB_LIMIT');
    assert.equal(limited.transaction.after, 'autocommit');
    assert.equal(call([{ sql: 'SELECT number FROM docs' }], true).execution.result.results[0].rows.length, 1);
  } finally { db.close(); }
});

test('native worker executes an atomic batch as one operation and cancels through the registry', async () => {
  const { Worker } = require('node:worker_threads');
  const { once } = require('node:events');
  const binding = require('./native.cjs');
  const worker = new Worker(require.resolve('./worker.cjs'), { workerData: { path: ':memory:' } });
  const exit = once(worker, 'exit');
  const pending = new Map();
  let next = 0;
  const ready = new Promise((resolve, reject) => {
    worker.on('error', reject);
    worker.on('message', message => {
      if (message.ready) return resolve();
      const operation = pending.get(message.id);
      if (operation) {
        pending.delete(message.id);
        if (message.error) operation.reject(new Error(message.error.message));
        else operation.resolve(message.result === undefined ? undefined : JSON.parse(message.result));
      }
    });
  });
  const call = (method, args, cancellationKey) => new Promise((resolve, reject) => {
    const id = ++next;
    pending.set(id, { resolve, reject });
    worker.postMessage({ id, method, args, cancellationKey });
  });
  await ready;
  try {
    const body = JSON.stringify({ readOnly: false, statements: [{ sql: 'CREATE TABLE data (n INTEGER)' }, { sql: 'INSERT INTO data VALUES(1)' }, { sql: 'SELECT n FROM data' }] });
    const first = call('executeAtomicRequest', [body]);
    const second = call('execute', ['INSERT INTO data VALUES(2)', '{}']);
    assert.deepEqual((await first).execution.result.results[2].rows, [[{ type: 'Integer', value: '1' }]]);
    assert.equal((await second).execution.error, undefined);
    const cancellationKey = binding.createCancellationToken();
    try {
      binding.cancelOperation(cancellationKey);
      const reply = await call('executeAtomicRequest', [JSON.stringify({ readOnly: false, statements: [{ sql: 'DELETE FROM data' }] })], cancellationKey);
      assert.equal(reply.execution.error.code, 'FDB_CANCELLED');
      assert.equal(reply.transaction.after, 'autocommit');
    } finally { binding.releaseCancellationToken(cancellationKey); }
    const reply = await call('executeAtomicRequest', [JSON.stringify({ readOnly: true, statements: [{ sql: 'SELECT n FROM data ORDER BY n' }] })]);
    assert.deepEqual(reply.execution.result.results[0].rows.map(row => decodeValue(row[0])), [1n, 2n]);
    await call('close', []);
    assert.deepEqual(await exit, [0]);
  } finally { await worker.terminate(); }
});
