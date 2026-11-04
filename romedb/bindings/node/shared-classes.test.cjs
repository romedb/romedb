'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');
const client = require('@romedb/client');
const clientValues = require('@romedb/client/values');

const entries = () => ({ client: require('./client.cjs'), values: require('./values.cjs'), embedded: require('./embedded-client.cjs') });

test('every @romedb/node client entry uses the classes of @romedb/client', () => {
  for (const [name, entry] of Object.entries(entries())) {
    for (const value of ['Float64', 'Record', 'Vector', 'encodeValue', 'decodeValue']) assert.equal(entry[value], client[value], `${name} ${value}`);
  }
  assert.equal(clientValues.Record, client.Record);
  assert.equal(require('./client.cjs').connect, client.connect);
  assert.equal(require('./client.cjs').ClientError, client.ClientError);
});

test('the ES module entries are the same objects', async () => {
  const [webClient, webValues] = await Promise.all([import('./client.mjs'), import('./values.mjs')]);
  assert.equal(webClient.Record, client.Record);
  assert.equal(webClient.connect, client.connect);
  assert.equal(webValues.Float64, client.Float64);
});

test('an embedded database takes values made by @romedb/client', async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'romedb-shared-classes-'));
  const { openEmbedded } = entries().embedded;
  const db = await openEmbedded({ path: path.join(directory, 'site.romedb'), create: true });
  try {
    const id = new client.Record('posts', 'a');
    await db.writeAtomic([{ sql: 'CREATE TABLE posts' }, { sql: 'INSERT INTO posts DOCUMENT $doc', params: { $doc: { id, score: new client.Float64(1) } } }]);
    const row = (await db.read([{ sql: 'SELECT id, score FROM posts WHERE id=$id', params: { $id: id } }])).results[0].rows[0];
    assert.ok(row[0] instanceof client.Record);
    assert.ok(row[0].equals(id));
    assert.ok(row[1] instanceof client.Float64);
  } finally {
    await db.close();
    fs.rmSync(directory, { recursive: true, force: true });
  }
});
