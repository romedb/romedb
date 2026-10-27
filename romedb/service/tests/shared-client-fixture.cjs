'use strict';
const assert = require('node:assert/strict');
const { Float64, Record, Vector, encodeValue } = require('../../bindings/node/client.cjs');

async function qualify(client) {
  const value = {
    id: new Record('fixture_docs', 'a'),
    integer: 9223372036854775807n,
    negative: -9223372036854775808n,
    float: new Float64(-0),
    boolean: true,
    binary: new Uint8Array([0, 127, 255]),
    vector: Vector.float32([1, -0, 3]),
    sparse: Vector.sparse32Entries(4, [[1, 2], [3, -3]]),
    present: null,
    nested: [{ type: 'Number', value: 'ordinary object' }, false, []],
  };
  await client.writeAtomic([
    { sql: 'CREATE TABLE fixture_docs' },
    { sql: 'CREATE UNIQUE INDEX fixture_title ON fixture_docs(title)' },
    { sql: 'INSERT INTO fixture_docs DOCUMENT $doc', params: { $doc: { ...value, title: 'original' } } },
    { sql: 'CREATE TABLE fixture_rows (id INTEGER PRIMARY KEY, n INTEGER UNIQUE)' },
    { sql: 'INSERT INTO fixture_rows VALUES(1,1)' },
  ]);
  const expected = encodeValue({ ...value, title: 'original' });
  const stored = (await client.read([{ sql: 'SELECT * FROM fixture_docs' }])).results[0].rows[0][0];
  assert.deepEqual(encodeValue(stored), expected);
  assert.equal(Object.hasOwn(stored, 'absent'), false);
  assert.equal(Object.hasOwn(stored, 'present'), true);
  const duplicate = (await client.read([{ sql: 'SELECT $v AS same,$v AS same', params: { $v: value } }])).results[0];
  assert.deepEqual(duplicate.columns, ['same', 'same']);
  for (const returned of duplicate.rows[0]) assert.deepEqual(encodeValue(returned), encodeValue(value));
  const prepared = client.prepareWrite([{ sql: 'UPDATE fixture_rows SET n=n+1 WHERE id=1' }]);
  const published = await client.submit(prepared);
  assert.deepEqual(await client.recover(prepared), published);
  assert.deepEqual((await client.read([{ sql: 'SELECT n FROM fixture_rows' }])).results[0].rows, [[2n]]);
  const snapshot = await client.read([{ sql: 'SELECT n FROM fixture_rows' }]);
  const stale = client.prepareWrite([{ sql: 'UPDATE fixture_rows SET n=4 WHERE id=1' }], { ifSequence: snapshot.sequence });
  await client.writeAtomic([{ sql: 'UPDATE fixture_rows SET n=3 WHERE id=1' }]);
  await assert.rejects(client.submit(stale), error => error.code === 'FDB_CONFLICT' && error.outcome === 'rolled-back');
  await assert.rejects(client.writeAtomic([
    { sql: "UPDATE fixture_docs:a SET title='changed'" },
    { sql: 'INSERT INTO fixture_rows VALUES(2,3)' },
  ]), error => error.code === 'FDB_CONSTRAINT' && error.outcome === 'rolled-back');
  assert.deepEqual((await client.read([{ sql: "SELECT title FROM fixture_docs WHERE title='original'" }])).results[0].rows, [['original']]);
  assert.deepEqual((await client.read([{ sql: "SELECT title FROM fixture_docs WHERE title='changed'" }])).results[0].rows, []);
  await client.writeAtomic([
    { sql: 'CREATE TABLE fixture_search' },
    { sql: 'DEFINE FIELD title ON fixture_search TYPE string REQUIRED' },
    { sql: 'DEFINE FIELD v ON fixture_search TYPE vector<2>' },
    { sql: 'CREATE INDEX fixture_scalar ON fixture_search(title)' },
    { sql: 'CREATE SEARCH INDEX fixture_text ON fixture_search(title) USING FULLTEXT' },
    { sql: "CREATE SEARCH INDEX fixture_vector ON fixture_search(v) USING VECTOR WITH (dimensions=2,metric='l2')" },
    { sql: 'CREATE SEARCH INDEX fixture_spatial ON fixture_search(location) USING SPATIAL' },
    { sql: "INSERT INTO fixture_search {id:fixture_search:original,title:'originalterm',v:vector32('[1,0]'),location:geo::point(0,0)}" },
    // If a rolled-back vector update leaked [10,0] into the index, this
    // competitor would incorrectly become the closest result to [1,0].
    { sql: "INSERT INTO fixture_search {id:fixture_search:distant,title:'distantterm',v:vector32('[5,0]'),location:geo::point(50,50)}" },
  ]);
  await assert.rejects(client.writeAtomic([
    { sql: "UPDATE fixture_search:original SET title='changedterm',v=vector32('[10,0]'),location=geo::point(10,10)" },
    { sql: 'INSERT INTO fixture_search {id:fixture_search:invalid,title:123}' },
  ]), error => error.code === 'FDB_VALIDATION' && error.outcome === 'rolled-back');
  const searches = [
    ["SELECT id FROM fixture_search WHERE title='originalterm'", 'fixture_scalar'],
    ["SELECT id FROM search::text('fixture_text','originalterm',10)", 'QUERY INDEX METHOD fts'],
    ["SELECT id FROM search::vector('fixture_vector',vector32('[1,0]'),1)", null],
    ["SELECT id FROM search::near('fixture_spatial',geo::point(0,0),10)", 'fixture_spatial'],
  ];
  for (const [sql, index] of searches) {
    const rows = (await client.read([{ sql }])).results[0].rows;
    assert.deepEqual(rows.map(row => encodeValue(row[0])), [encodeValue(new Record('fixture_search', 'original'))]);
    if (index === null) {
      await assert.rejects(client.read([{ sql: `EXPLAIN QUERY PLAN ${sql}` }]), error => error.code === 'FDB_UNSUPPORTED');
    } else {
      const plan = (await client.read([{ sql: `EXPLAIN QUERY PLAN ${sql}` }])).results[0];
      assert(JSON.stringify(plan, (_, value) => typeof value === 'bigint' ? value.toString() : value).includes(index), `index absent from plan: ${index}`);
    }
  }
  assert.deepEqual((await client.read([{ sql: "SELECT id FROM search::text('fixture_text','changedterm',10)" }])).results[0].rows, []);
  await assert.rejects(client.writeAtomic([
    { sql: 'INSERT INTO fixture_rows VALUES(2,4)' },
    { sql: 'INSERT INTO fixture_rows VALUES(3,5)' },
  ], { workLimits: { rowMutations: 1 } }), error => error.code === 'FDB_LIMIT' && error.outcome === 'rolled-back');
  await assert.rejects(client.writeAtomic([
    { sql: 'INSERT INTO fixture_rows VALUES(2,4)' },
    { sql: 'SELECT hex(zeroblob(80000))' },
    { sql: 'SELECT hex(zeroblob(80000))' },
  ]), error => error.code === 'FDB_LIMIT' && error.outcome === 'rolled-back');
  assert.deepEqual((await client.read([{ sql: 'SELECT id,n FROM fixture_rows' }])).results[0].rows, [[1n, 3n]]);
  {
    await client.writeAtomic([
      { sql: 'CREATE TABLE fixture_parents (id INTEGER PRIMARY KEY)' },
      { sql: 'CREATE TABLE fixture_children (id INTEGER PRIMARY KEY,parent INTEGER REFERENCES fixture_parents(id))' },
    ]);
    await assert.rejects(client.writeAtomic([
      { sql: 'INSERT INTO fixture_parents VALUES(1)' },
      { sql: 'INSERT INTO fixture_children VALUES(1,999)' },
    ]), error => error.code === 'FDB_CONSTRAINT' && error.outcome === 'rolled-back');
    assert.deepEqual((await client.read([{ sql: 'SELECT id FROM fixture_parents' }])).results[0].rows, []);
  }
  const metadata = (await client.read([{ sql: 'INFO FOR DB' }])).results[0].rows[0][0];
  assert(metadata.tables.some(table => table.name === 'fixture_docs' && table.model === 'document'));
  assert(metadata.tables.some(table => table.name === 'fixture_rows' && table.model === 'relational'));
  return { typedValues: true, replay: true, staleEdits: true, indexRollback: true, limits: true, foreignKeys: true };
}

module.exports = { qualify };
