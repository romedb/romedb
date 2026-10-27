'use strict';
// Each phase uses one native library in a fresh process, including every reopen.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { execFileSync } = require('node:child_process');
const [oldPackage, newPackage, directory, phase, file] = process.argv.slice(2);
if (!oldPackage || !newPackage || !directory) {
  throw new Error('Usage: node check-v21-upgrade.cjs <absolute-published-2.0.0-package> <absolute-2.1.0-package> <new-output-directory>');
}
for (const value of [oldPackage, newPackage, directory]) assert.ok(path.isAbsolute(value), 'Use absolute paths');
const digest = file => createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const copy = (source, destination) => fs.copyFileSync(source, destination, fs.constants.COPYFILE_EXCL);
assert.equal(require(path.join(oldPackage, 'package.json')).version, '2.0.0');
assert.equal(require(path.join(newPackage, 'package.json')).version, '2.1.0');
// Verified against the immutable public bundle SHA256 614744de...d006.
const oldHashes = {
  'romedb.node': '11e472d275bb287bc4806e086d3ffed268bf170240b7f2ab6d981270e87f523d',
  'index.cjs': 'd13a91b2c76ceb1fe0253c13ffd6249947bbfaf76addbb9a8fabb1799067fead',
  'worker.cjs': '1a4bd8044d7438ff296297832fcc592b281994cf12554598eaf505832431d655',
  'native.cjs': '2ab72dfe0ae4f1782d113fe210924f23f0fcba42094e5d6892ed999e9bc1d1a7',
};
for (const [name, hash] of Object.entries(oldHashes)) assert.equal(digest(path.join(oldPackage, name)), hash, `Published 2.0.0 ${name}`);
const fixturePath = path.join(directory, 'fixture.json');
const migration = {
  version: 1n, name: 'v2-application', sql: [
    'CREATE TABLE users',
    'DEFINE FIELD name ON users TYPE string REQUIRED',
    'CREATE UNIQUE INDEX users_name ON users(name)',
    'CREATE TABLE docs',
    'DEFINE FIELD title ON docs TYPE string REQUIRED CHECK(length(title)>0)',
    'DEFINE FIELD owner ON docs TYPE record<users>',
    'DEFINE FIELD v ON docs TYPE vector<3>',
    'CREATE INDEX docs_owner ON docs(owner)',
    'CREATE INDEX docs_city ON docs(profile.city)',
    'CREATE SEARCH INDEX docs_text ON docs(title) USING FULLTEXT',
    "CREATE SEARCH INDEX docs_vec ON docs(v) USING VECTOR WITH (dimensions=3,metric='cosine')",
    'CREATE SEARCH INDEX docs_geo ON docs(p) USING SPATIAL',
    'DEFINE RELATION documents ON users FROM docs.owner',
    'CREATE TABLE events(id INTEGER PRIMARY KEY, message TEXT)',
    'CREATE VIEW event_view AS SELECT id,message FROM events',
    "CREATE FUNCTION app::normalize(value string) RETURNS string LANGUAGE JAVASCRIPT AS 'return value.trim().toLowerCase();'",
    // Persist with the old runtime; invoke the security regression only after upgrade.
    "CREATE FUNCTION app::rope() RETURNS string LANGUAGE JAVASCRIPT AS 'return JSON.stringify({value:1},null,\"A\".repeat(10000)+\"B\".repeat(10000));'",
  ].join('; ') + ';',
};

if (!phase) {
  fs.mkdirSync(directory); // Refuse to overwrite a previous rehearsal or backup.
  const source = path.join(directory, 'source.db');
  const oldBackup = path.join(directory, 'v2.0-backup.db');
  const newBackup = path.join(directory, 'v2.1-backup.db');
  const phases = [];
  function run(step, database) {
    execFileSync(process.execPath, [__filename, oldPackage, newPackage, directory, step, database], {
      stdio: 'inherit', timeout: 120000,
    });
    phases.push(step);
  }
  run('seed', source);
  copy(source, oldBackup);
  const oldHash = digest(oldBackup);
  run('upgrade', source);
  run('candidate-reopen', source);
  copy(source, newBackup);
  const newHash = digest(newBackup);
  const candidateRestore = path.join(directory, 'restored-v2.1.db');
  copy(newBackup, candidateRestore);
  run('candidate-restore', candidateRestore);
  run('restored-reopen', candidateRestore);
  run('candidate-reopen', source); // Restore mutations must not affect the source.
  const previousRestore = path.join(directory, 'restored-v2.0.db');
  copy(oldBackup, previousRestore);
  run('previous-restore', previousRestore); // Original binary verifies its own backup.
  run('upgrade', previousRestore); // Candidate can also adopt the restored V2 backup.
  run('candidate-reopen', previousRestore);
  assert.equal(digest(oldBackup), oldHash, 'Original 2.0 backup must remain unchanged');
  assert.equal(digest(newBackup), newHash, 'Candidate backup must remain unchanged');
  const report = {
    fromVersion: '2.0.0', toVersion: '2.1.0', node: process.version,
    fromAddonSha256: oldHashes['romedb.node'], toAddonSha256: digest(path.join(newPackage, 'romedb.node')),
    fromBackupSha256: oldHash, toBackupSha256: newHash,
    backupsUnchanged: true, phases,
    scope: 'Offline V2 upgrade, new writes, reopen and separate backup restores; no binary downgrade promise',
  };
  fs.writeFileSync(path.join(directory, 'report.json'), JSON.stringify(report, null, 2) + '\n', { flag: 'wx' });
  console.log(JSON.stringify({ ok: true, ...report }));
} else {
  const old = phase === 'seed' || phase === 'previous-restore';
  const { Database, Record, Vector } = require(old ? oldPackage : newPackage);
  const normalize = value => {
    if (typeof value === 'bigint') return { integer: String(value) };
    if (typeof value === 'number') {
      const bytes = Buffer.alloc(8); bytes.writeDoubleLE(value); return { binary64: bytes.toString('hex') };
    }
    if (value instanceof Record) return { record: [value.table, normalize(value.key)] };
    if (value instanceof Vector) return { vector: Buffer.from(value.bytes).toString('hex') };
    if (value instanceof Uint8Array) return { binary: Buffer.from(value).toString('hex') };
    if (Array.isArray(value)) return value.map(normalize);
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value)
      .sort(([a], [b]) => a.localeCompare(b)).map(([key, item]) => [key, normalize(item)]));
    return value;
  };
  const documents = [
    { id: new Record('docs', 'a'), title: 'Fast river', owner: new Record('users', 'alice'),
      p: { type: 'Point', coordinates: [100n, 13n] }, v: Vector.float32([1, 0, 0]), profile: { city: 'Bangkok' },
      max: 9223372036854775807n, min: -9223372036854775808n, negativeZero: -0, tiny: Number.MIN_VALUE,
      binary: Buffer.from([0, 255, 128]), nil: null, flag: true, nested: { items: ['literal', 9n, false] } },
    { id: new Record('docs', 'b'), title: 'Quiet ocean', owner: new Record('users', 'bob'),
      p: { type: 'Point', coordinates: [-70n, 40n] }, v: Vector.float32([0, 1, 0]), profile: { city: 'Boston' } },
  ];
  const db = new Database(file);
  let fixture = phase === 'seed' ? null : JSON.parse(fs.readFileSync(fixturePath));
  const stableFunctionInfo = () => {
    const result = db.exactlyOne('INFO FOR FUNCTION app::normalize')[0];
    delete result.runtime; // The patched runtime identity intentionally changes.
    return normalize(result);
  };
  function assertBaseline(hasNew = false) {
    assert.deepEqual(db.all('PRAGMA integrity_check'), [['ok']]);
    for (const document of documents) assert.deepEqual(db.collection('docs').get(document.id.key), document);
    assert.equal(db.checkCollectionIntegrity('docs').documents, hasNew ? 3n : 2n);
    assert.equal(db.checkCollectionIntegrity('users').documents, 2n);
    assert.equal(db.exportDocuments('users'), fixture.usersExport);
    for (const table of ['users', 'docs']) assert.deepEqual(normalize(db.all(`INFO FOR TABLE ${table}`)), fixture.info[table]);
    assert.deepEqual(stableFunctionInfo(), fixture.functionInfo);
    assert.deepEqual(db.all("SELECT app::normalize(' ALICE ')"), [['alice']]);
    assert.deepEqual(db.all('SELECT * FROM event_view WHERE id=1'), [[1n, 'created in 2.0']]);
    assert.equal(db.migrate([migration]).alreadyApplied, 1);
    assert.throws(() => db.migrate([{ ...migration, sql: migration.sql + ' ' }]), error => error.code === 'FDB_VALIDATION');
    assert.deepEqual(db.all("SELECT d.id FROM docs d WHERE d.profile.city='Bangkok'"), [[new Record('docs', 'a')]]);
    assert.deepEqual(db.all("SELECT id FROM search::text('docs_text','river',10)"), [[new Record('docs', 'a')]]);
    assert.deepEqual(db.all("SELECT id FROM search::vector('docs_vec',vector32('[1,0,0]'),1)"), [[new Record('docs', 'a')]]);
    assert.deepEqual(db.all("SELECT id FROM search::near('docs_geo',geo::point(100,13),10)"), [[new Record('docs', 'a')]]);
    assert.deepEqual(db.exactlyOne("SELECT relation::fetch(users:alice,'documents',10)")[0], [documents[0]]);
    if (!old) {
      assert.deepEqual(db.all('SELECT app::rope()'), [['{\nAAAAAAAAAA"value": 1\n}']]);
      assert.equal(db.exactlyOne('INFO FOR FUNCTION app::normalize')[0].runtime, 'quickjs-ng-0.16.2-rquickjs-0.13.0');
      assert.deepEqual(db.exactlyOne('SELECT posts.*.* FROM users WHERE id=users:alice')[0], documents);
    }
    assert.throws(() => db.execute("UPDATE docs:a {title:''}"));
    assert.throws(() => db.execute("INSERT INTO users {id:users:duplicate,name:'Alice'}"));
    if (hasNew) {
      assert.deepEqual(db.all('SELECT message FROM events WHERE id=2'), [['written in 2.1']]);
      assert.deepEqual(db.all("SELECT id FROM search::text('docs_text','meadow',10)"), [[new Record('docs', 'c')]]);
      assert.deepEqual(db.all("SELECT id FROM search::vector('docs_vec',vector32('[0,0,1]'),1)"), [[new Record('docs', 'c')]]);
      assert.deepEqual(db.all("SELECT id FROM search::near('docs_geo',geo::point(0,0),10)"), [[new Record('docs', 'c')]]);
      assert.deepEqual(db.exactlyOne("SELECT relation::fetch(users:bob,'documents',10)")[0].map(d => d.id), [new Record('docs', 'b'), new Record('docs', 'c')]);
    } else {
      assert.equal(db.exportDocuments('docs'), fixture.docsExport);
      assert.deepEqual(db.all('SELECT count(*) FROM events'), [[1n]]);
    }
  }
  try {
    db.execute('PRAGMA synchronous=FULL');
    if (phase === 'seed') {
      assert.deepEqual(db.migrate([migration]).applied, [1n]);
      db.execute("INSERT INTO users {id:users:alice,name:'Alice',posts:[docs:a,docs:b]}");
      db.execute("INSERT INTO users {id:users:bob,name:'Bob'}");
      for (const document of documents) db.execute('INSERT INTO docs DOCUMENT $doc', { $doc: document });
      db.execute("INSERT INTO events VALUES(1,'created in 2.0')");
      fixture = { usersExport: db.exportDocuments('users'), docsExport: db.exportDocuments('docs'),
        functionInfo: stableFunctionInfo(), info: Object.fromEntries(['users', 'docs'].map(table => [table, normalize(db.all(`INFO FOR TABLE ${table}`))])) };
      fs.writeFileSync(fixturePath, JSON.stringify(fixture, null, 2) + '\n', { flag: 'wx' });
      assertBaseline();
    } else if (phase === 'upgrade') {
      assertBaseline();
      db.execute('BEGIN');
      db.execute("UPDATE docs:a {title:'Changed meadow',p:geo::point(0,0),v:vector32('[0,0,1]'),owner:users:bob}");
      assert.deepEqual(db.all("SELECT id FROM search::text('docs_text','river',10)"), []);
      assert.deepEqual(db.all("SELECT id FROM search::near('docs_geo',geo::point(100,13),10)"), []);
      assert.deepEqual(db.exactlyOne("SELECT relation::fetch(users:alice,'documents',10)")[0], []);
      db.execute('DELETE FROM docs:b');
      db.execute('ROLLBACK');
      assertBaseline();
      db.execute('BEGIN');
      db.execute("INSERT INTO docs {id:docs:c,title:app::normalize(' GREEN MEADOW '),owner:users:bob,p:geo::point(0,0),v:vector32('[0,0,1]')}");
      db.execute("INSERT INTO events VALUES(2,'written in 2.1')");
      db.execute('COMMIT');
      assertBaseline(true);
    } else if (phase === 'candidate-restore') {
      assertBaseline(true);
      db.execute('BEGIN'); db.execute('DELETE FROM docs:a'); db.execute('ROLLBACK');
      assertBaseline(true);
      db.execute("INSERT INTO events VALUES(3,'written after restore')");
    } else if (phase === 'candidate-reopen' || phase === 'restored-reopen') {
      assertBaseline(true);
      assert.deepEqual(db.all('SELECT message FROM events WHERE id=3'), phase === 'restored-reopen' ? [['written after restore']] : []);
    } else if (phase === 'previous-restore') {
      assertBaseline();
      db.execute('BEGIN'); db.execute('DELETE FROM docs:a'); db.execute('ROLLBACK');
      assertBaseline();
    } else throw new Error(`Unknown phase: ${phase}`);
    assert.deepEqual(db.all('PRAGMA wal_checkpoint(TRUNCATE)'), [[0n, 0n, 0n]]);
    assert.equal(fs.statSync(file + '-wal').size, 0);
    console.log(`${phase}: passed`);
  } finally { db.close(); }
}
