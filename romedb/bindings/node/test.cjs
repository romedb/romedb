'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { Database, Record, Vector } = require('./index.cjs');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
test('whole-document FETCH replaces named links in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      const writer = {id:new Record('writers','w1'),name:'Alice',large:9223372036854775807n,bytes:Buffer.from([0,255]),vector:Vector.float32([1,2]),next:new Record('writers','w1')};
      await db.execute('INSERT INTO writers DOCUMENT $writer', {$writer:writer});
      const article = {id:new Record('articles','a'), author:writer.id, editors:[writer.id,null,new Record('writers','missing')],value:null};
      await db.execute('INSERT INTO articles DOCUMENT $article', {$article:article});
      const result = await db.execute('SELECT * FROM articles WHERE author=$writer LIMIT 1 FETCH author,editors,absent', {$writer:writer.id});
      assert.deepEqual(result.columns,['document']);
      assert.deepEqual(result.rows,[[{...article,author:writer,editors:[writer,null,null]}]]);
      assert.deepEqual(await db.collection('articles').get('a'),article);
      await db.execute('BEGIN');
      await db.execute("UPDATE writers:w1 MERGE {name:'Pending'}");
      assert.equal((await db.exactlyOne('SELECT * FROM articles FETCH author'))[0].author.name,'Pending');
      await db.execute('ROLLBACK');
      assert.equal((await db.exactlyOne('SELECT * FROM articles FETCH author'))[0].author.name,'Alice');
    } finally { await db.close(); }
  }
});
test('typed array and object helpers agree in reads and writes in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      const value = {binary:Buffer.from([0,255]), integer:9223372036854775807n, null:null, record:new Record('docs','r'), vector:Vector.float32([1,2])};
      await db.execute('INSERT INTO docs {id:docs:a,body:$v,items:array::new($v,$v,null,null)}', {$v:value});
      const expected = [value,null];
      assert.deepEqual(await db.exactlyOne('SELECT array::distinct(items),doc::from_entries(doc::entries(body)),doc::keys(body) FROM docs'),[expected,value,['binary','integer','null','record','vector']]);
      assert.deepEqual((await db.execute('UPDATE docs:a MERGE {items:array::distinct(items)} RETURNING items')).rows,[[expected]]);
      await assert.rejects(async () => db.execute("UPDATE docs:a CONTENT doc::from_entries(array::new(array::new('x',1),array::new('x',2)))"), e=>e.code==='FDB_VALIDATION');
      assert.deepEqual((await db.collection('docs').get('a')).items,expected);
    } finally { await db.close(); }
  }
});
test('PATCH keeps typed array edits atomic in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      await db.execute('INSERT INTO docs {id:docs:a,items:[1,2],n:1}');
      await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
      const typed = {large:9223372036854775807n, bytes:Buffer.from([0,255]), vector:Vector.float32([1,2]), ref:new Record('docs','b'), value:null};
      const ops = [{op:'add',path:'/items/1',value:typed},{op:'move',from:'/items/0',path:'/items/-'},{op:'test',path:'/items/0',value:typed}];
      const result = await db.execute('UPDATE docs:a PATCH $ops RETURNING items', {$ops:ops});
      assert.deepEqual(result.rows, [[[typed,2n,1n]]]);
      await db.execute('BEGIN');
      await db.execute("UPDATE docs:a PATCH [{op:'add',path:'/prior',value:true}]");
      const before = await db.collection('docs').get('a');
      await assert.rejects(async () => db.execute("UPDATE docs:a PATCH [{op:'remove',path:'/items/0'},{op:'test',path:'/n',value:7}]"), e=>e.code==='FDB_VALIDATION');
      assert.deepEqual(await db.collection('docs').get('a'), before);
      await db.checkCollectionIntegrity('docs');
      await db.execute('ROLLBACK');
      assert.equal(Object.hasOwn(await db.collection('docs').get('a'),'prior'),false);
    } finally { await db.close(); }
  }
});
test('CONTENT replaces and MERGE recursively updates atomically in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      await db.execute("INSERT INTO docs {id:docs:a,uid:1,old:true,profile:{city:'Paris',country:'FR'}}");
      await db.execute("INSERT INTO docs {id:docs:b,uid:2}");
      await db.execute('CREATE UNIQUE INDEX docs_uid ON docs(uid)');
      const replacement = {uid:1n, bytes:Buffer.from([0,255]), large:9223372036854775807n, profile:{city:'Bangkok'}};
      const result = await db.execute('UPDATE docs:a CONTENT $body RETURNING *', {$body:replacement});
      assert.equal(result.affected,1n);
      assert.deepEqual(result.rows, [[{id:new Record('docs','a'), ...replacement}]]);
      await db.execute("UPDATE docs:a MERGE {profile:{country:'TH'},tags:[1,2]}");
      assert.deepEqual((await db.collection('docs').get('a')).profile, {city:'Bangkok',country:'TH'});
      await db.execute('BEGIN');
      await db.execute("UPDATE docs:a MERGE {pending:true}");
      const before = await db.all('SELECT * FROM docs ORDER BY id');
      await assert.rejects(async () => db.execute('UPDATE docs CONTENT {uid:1}'), e => e.code==='FDB_CONSTRAINT');
      assert.deepEqual(await db.all('SELECT * FROM docs ORDER BY id'), before);
      await db.checkCollectionIntegrity('docs');
      await db.execute('ROLLBACK');
      assert.equal(Object.hasOwn(await db.collection('docs').get('a'),'pending'),false);
    } finally { await db.close(); }
  }
});
test('OMIT keeps typed document fields and stored data in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      const id = new Record('docs','one');
      const retained = {large:9223372036854775807n, bytes:Buffer.from([0,255]), vector:Vector.float32([1,2]), link:new Record('docs','other'), flag:true, value:null};
      const document = {id, secret:'private', nested:{city:'Paris', secret:'hidden'}, retained};
      await db.execute('INSERT INTO docs DOCUMENT $doc', {$doc:document});
      const result = await db.execute('SELECT * OMIT secret,nested.secret FROM docs WHERE id=$id', {$id:id});
      assert.deepEqual(result.columns, ['document']);
      assert.deepEqual(result.rows, [[{id, nested:{city:'Paris'}, retained}]]);
      assert.deepEqual(await db.collection('docs').get('one'), document);
      await assert.rejects(async () => db.execute('SELECT id OMIT secret FROM docs'));
    } finally { await db.close(); }
  }
});
test('ordinary reference wildcard aliases preserve typed values in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      const payload = {name:'Alice', large:9223372036854775807n, bytes:Buffer.from([0,255]), active:true, nothing:null, vector:Vector.float32([1,2]), link:new Record('writers','missing')};
      await db.execute('INSERT INTO writers DOCUMENT $doc', {$doc:{id:new Record('writers','w1'), ...payload}});
      await db.execute("INSERT INTO articles {id:articles:a1,title:'Hello',author:writers:w1}");
      await db.execute('CREATE INDEX article_author ON articles(author)');
      const parameters = {$writer:'w1'};
      for (const [sql, fetch] of [
        ["SELECT id, title, author.* AS writer FROM articles WHERE author = type::record('writers', $writer) ORDER BY id", "SELECT id, title, record::fetch(author) AS writer FROM articles WHERE author = type::record('writers', $writer) ORDER BY id"],
        ["SELECT a.id, a.title, a.author.* AS writer FROM articles AS a WHERE a.author = type::record('writers', $writer) ORDER BY a.id LIMIT 20", "SELECT a.id, a.title, record::fetch(a.author) AS writer FROM articles AS a WHERE a.author = type::record('writers', $writer) ORDER BY a.id LIMIT 20"],
      ]) {
        const actual = await db.execute(sql,parameters);
        const expected = await db.execute(fetch,parameters);
        assert.deepEqual(actual.columns, ['id','title','writer']);
        assert.deepEqual(actual.rows, expected.rows);
        assert.deepEqual(actual.rows[0][2], {id:new Record('writers','w1'), ...payload});
      }
      const duplicate = await db.execute('SELECT author.* AS writer,author.* AS writer FROM articles');
      assert.deepEqual(duplicate.columns, ['writer','writer']);
      assert.deepEqual(duplicate.rows[0][0], duplicate.rows[0][1]);
      await assert.rejects(async () => db.execute('SELECT author.* AS writer FROM articles author'));
    } finally { await db.close(); }
  }
});
test('RomeQL V1 document examples compose with explicit setup in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      for (const sql of [
        'CREATE TABLE users', 'CREATE TABLE posts',
        'DEFINE FIELD name ON users TYPE string REQUIRED',
        'DEFINE FIELD nickname ON users TYPE string NULLABLE',
        'DEFINE FIELD profile ON users TYPE object',
        'DEFINE FIELD profile.city ON users TYPE string',
        'DEFINE FIELD title ON posts TYPE string REQUIRED CHECK(length(title)>0)',
        'DEFINE FIELD author ON posts TYPE record<users>',
        'DEFINE FIELD published ON posts TYPE boolean',
        'CREATE UNIQUE INDEX user_name ON users(name)',
        'CREATE INDEX post_author ON posts(author)',
        "INSERT INTO users {id:users:u1,name:'Alice',nickname:null,profile:{city:'Bangkok'},tags:[]}",
        "INSERT INTO posts {id:posts:p1,title:'Hello',author:users:u1,published:false}",
      ]) await db.execute(sql);
      await db.execute('UPDATE users SET profile.city=$city WHERE id=users:u1 RETURNING *', {$city:'Paris'});
      await db.execute("UPDATE posts {title:'Updated title',published:true} WHERE id=posts:p1 RETURNING *");
      await db.execute('UPDATE users SET tags=array::append(tags,$tag) WHERE id=users:u1 RETURNING *', {$tag:'romedb'});
      await db.execute('UPDATE users UNSET nickname WHERE id=users:u1 RETURNING *');
      assert.deepEqual(await db.all('SELECT p.title,u.name FROM posts p JOIN users u ON p.author=u.id WHERE u.name=$name ORDER BY p.title,p.id LIMIT 20 OFFSET 0', {$name:'Alice'}), [['Updated title','Alice']]);
      const [title,author] = await db.exactlyOne('SELECT p.title,record::fetch(p.author) FROM posts p WHERE p.id=posts:p1');
      assert.equal(title,'Updated title');
      assert.equal(author.profile.city,'Paris');
      assert.deepEqual(author.tags,['romedb']);
      assert.equal(Object.hasOwn(author,'nickname'),false);
      await db.execute("UPSERT posts {id:posts:p1,title:'Hello again',author:users:u1} RETURNING *");
      assert.equal((await db.collection('posts').get('p1')).published,true);
      await assert.rejects(async()=>db.execute("UPDATE posts:p1 {title:''}"), e=>e.code==='FDB_VALIDATION');
      await db.execute('BEGIN');
      await db.execute('DELETE FROM posts:p1 RETURNING *');
      assert.deepEqual(await db.all('SELECT posts:p1'),[]);
      await db.execute('ROLLBACK');
      assert.equal((await db.collection('posts').get('p1')).title,'Hello again');
      await db.checkCollectionIntegrity('users');
      await db.checkCollectionIntegrity('posts');
    } finally { await db.close(); }
  }
});
test('V1 record targets and row cardinality stay explicit in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      await db.execute('CREATE TABLE docs');
      assert.deepEqual(await db.all('SELECT docs:missing'), []);
      assert.equal(await db.first('SELECT docs:missing'), undefined);
      await assert.rejects(async () => db.exactlyOne('SELECT docs:missing'), /exactly one/);
      await db.execute("INSERT INTO docs {id:docs:1,n:10}");
      await db.execute("INSERT INTO docs {id:type::record('docs',$key),n:20}", {$key:'1'});
      assert.equal((await db.exactlyOne('SELECT docs:1'))[0].n, 10n);
      assert.equal((await db.exactlyOne('SELECT docs:`1`'))[0].n, 20n);
      assert.deepEqual(await db.exactlyOne('SELECT n FROM docs:1'), [10n]);
      assert.deepEqual(await db.exactlyOne('SELECT n FROM docs:`1`'), [20n]);
      assert.deepEqual(await db.exactlyOne("SELECT d.id,d.n FROM type::record('docs',$key) d WHERE d.n=$n", {$key:'1',$n:20n}), [new Record('docs','1'),20n]);
      assert.deepEqual(await db.all('SELECT n FROM docs:missing'), []);
      assert.equal(await db.first('SELECT n FROM docs:missing'), undefined);
      await assert.rejects(async () => db.exactlyOne('SELECT n FROM docs:missing'), /exactly one/);
      assert.deepEqual(await db.exactlyOne('SELECT docs:1 AS reference'), [new Record('docs',1n)]);
      assert.deepEqual(await db.all('SELECT record::id(id) FROM docs ORDER BY n'), [[1n],['1']]);
      await assert.rejects(async () => db.execute('INSERT INTO docs {id:other:1}'), error => error.code === 'FDB_VALIDATION');
      assert.deepEqual(await db.all('SELECT n FROM docs ORDER BY n'), [[10n],[20n]]);
      assert.deepEqual(await db.first('SELECT n FROM docs ORDER BY n'), [10n]);
      await assert.rejects(async () => db.exactlyOne('SELECT n FROM docs'), /exactly one/);
      assert.deepEqual((await db.execute('UPDATE docs SET n=n+1 RETURNING n')).rows.sort((a,b)=>Number(a[0]-b[0])), [[11n],[21n]]);
      assert.deepEqual((await db.execute('DELETE FROM docs WHERE 0 RETURNING n')).rows, []);
    } finally { await db.close(); }
  }
});
test('V1 parameter spellings bind through native and document routes in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      await db.execute('CREATE TABLE docs');
      for (const [expression, key] of [['?', '?1'], ['?1', '?1'], [':value', ':value'], ['@value', '@value'], ['$value', '$value']]) {
        const parameters = {[key]: 9223372036854775807n};
        assert.deepEqual(await db.exactlyOne(`SELECT ${expression} AS value`, parameters), [9223372036854775807n]);
        assert.deepEqual((await db.execute(`INSERT INTO docs(n) VALUES(${expression}) RETURNING n`, parameters)).rows, [[9223372036854775807n]]);
        assert.deepEqual(await db.exactlyOne(`SELECT count(*) FROM docs WHERE n=${expression}`, parameters), [BigInt((await db.all('SELECT n FROM docs')).length)]);
      }
      assert.deepEqual(await db.exactlyOne('SELECT ?1, :value, @value, $value', {'?1':1n, ':value':2n, '@value':3n, '$value':4n}), [1n,2n,3n,4n]);
    } finally { await db.close(); }
  }
});
test('native typed parameters and values preserve identities and binary64', () => {
  const db = new Database();
  db.execute('CREATE TABLE docs');
  const vectorBytes = Buffer.alloc(8); vectorBytes.writeFloatLE(1, 0); vectorBytes.writeFloatLE(2, 4);
  const doc = { id: new Record('docs', 9223372036854775807n), min: -9223372036854775808n,
    zero: -0, fraction: 1.25, delicate: 2.291712365432881e-9, yes: true, nil: null, text: 'docs:p1', bytes: Buffer.from([0,255]),
    vector: new Vector(vectorBytes), nested: { type: 'Integer', value: 'user text' }, list: [1n, 'a'] };
  db.execute('INSERT INTO docs DOCUMENT $doc', { $doc: doc });
  assert.deepEqual(db.exactlyOne('SELECT * FROM docs')[0], doc);
  const result = db.execute('SELECT $x AS x', { $x: 9223372036854775807n });
  assert.equal(result.rows[0][0], 9223372036854775807n);
  assert.equal(result.affected, 0n);
  assert.equal(result.transaction.after, 'autocommit');
  assert.throws(() => db.execute('SELECT $x', { $x: 9007199254740992 }), /bigint/);
  assert.throws(() => db.execute('SELECT $x', { $x: 1n << 63n }), /int64/);
  db.close();
});
test('validation, rollback and error transaction reports pass through frontend', () => {
  const db = new Database();
  db.execute('CREATE TABLE docs');
  db.execute('DEFINE FIELD value ON docs TYPE integer');
  db.execute('BEGIN');
  db.execute('INSERT INTO docs {id:docs:p1,value:1}');
  assert.throws(() => db.execute('INSERT INTO docs {id:docs:p1,value:2}'), error => error.code === 'FDB_CONSTRAINT' && error.transaction.after === 'active');
  assert.throws(() => db.execute("INSERT INTO docs {value:'bad'}"), error => error.code === 'FDB_VALIDATION' && error.transaction.after === 'active');
  db.execute('ROLLBACK');
  assert.deepEqual(db.all('SELECT * FROM docs'), []);
  db.execute('BEGIN');
  db.execute('INSERT INTO docs {value:2}');
  assert.throws(() => db.execute('SELECT array::append(1,2)'), error => error.transaction.after === 'active');
  assert.deepEqual(db.all('SELECT value FROM docs'), [[2n]]);
  db.execute('ROLLBACK');
  assert.deepEqual(db.all('SELECT * FROM docs'), []);
  db.close();
});
test('close is idempotent and persistence survives reopening', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'romedb-node-'));
  try {
    const file = path.join(dir, 'test.db');
    let db = new Database(file);
    db.execute('CREATE TABLE docs');
    db.execute("INSERT INTO docs {id:docs:p1,name:'saved'}");
    db.close(); db.close();
    assert.throws(() => db.all('SELECT * FROM docs'), /closed/);
    db = new Database(file);
    assert.equal(db.first('SELECT name FROM docs')[0], 'saved');
    assert.equal(db.first('SELECT name FROM docs WHERE 0'), undefined);
    assert.throws(() => db.exactlyOne('SELECT name FROM docs WHERE 0'), /exactly one/);
    db.close();
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});
test('prototype-looking object fields remain own data properties', () => {
  const db = new Database();
  const value = JSON.parse('{"__proto__":{"safe":true},"constructor":"data"}');
  assert.deepEqual(db.exactlyOne('SELECT $value AS value', { $value: value })[0], value);
  assert.equal({}.safe, undefined);
  db.close();
});
test('Node migration history retains int64 versions and failure atomicity', () => {
  const db = new Database();
  const plan = [{ version: 9007199254740993n, name: 'create', sql: 'CREATE TABLE docs;' }];
  assert.deepEqual(db.migrate(plan).applied, [9007199254740993n]);
  assert.equal(db.migrate(plan).alreadyApplied, 1);
  const pending = [...plan, { version: 9007199254740994n, name: 'fail', sql: 'INSERT INTO docs {value:1}; SELECT array::append(1,2);' }];
  assert.throws(() => db.migrate(pending), error => error.code === 'FDB_MIGRATION' && error.transaction.after === 'autocommit');
  assert.deepEqual(db.all('SELECT * FROM docs'), []);
  assert.throws(() => db.migrate([{ ...plan[0], sql: 'CREATE TABLE docs; -- edit' }]), error => error.code === 'FDB_VALIDATION');
  pending[1].sql = 'INSERT INTO docs {value:1};';
  assert.deepEqual(db.migrate(pending).applied, [9007199254740994n]);
  db.close();
});
test('Node transfers preserve values and roll back duplicate imports', () => {
  const source = new Database();
  source.execute('CREATE TABLE docs');
  source.execute('INSERT INTO docs DOCUMENT $doc', { $doc: { id: new Record('docs','p1'), value: 9223372036854775807n, zero: -0, bytes: Buffer.from([255]) } });
  for (const format of ['json', 'ndjson']) {
    const target = new Database();
    target.execute('CREATE TABLE docs');
    const payload = source.exportDocuments('docs', format);
    assert.equal(target.importDocuments('docs', payload, format).imported, 1);
    assert.deepEqual(target.all('SELECT * FROM docs'), source.all('SELECT * FROM docs'));
    assert.throws(() => target.importDocuments('docs', payload, format), error => error.transaction.after === 'autocommit');
    assert.equal(target.exportDocuments('docs', format), payload);
    target.execute('BEGIN');
    assert.throws(() => target.importDocuments('docs', 'bad input', format), error => error.transaction.after === 'active');
    target.execute('ROLLBACK');
    assert.throws(() => target.exportDocuments('docs', 'csv'), error => error.code === 'FDB_VALIDATION');
    target.close();
  }
  source.close();
});
test('async worker preserves submission order, typed values and graceful close', async t => {
  const { AsyncDatabase } = require('./index.cjs');
  const db = await AsyncDatabase.open();
  t.after(() => db.close());
  await db.migrate([{ version: 1n, name: 'create', sql: 'CREATE TABLE docs;' }]);
  const begin = db.execute('BEGIN');
  const insert = db.execute('INSERT INTO docs DOCUMENT $doc', { $doc: { id: new Record('docs','p1'), value: 9223372036854775807n, bytes: Buffer.from([255]) } });
  const read = db.exactlyOne('SELECT * FROM docs');
  const rollback = db.execute('ROLLBACK');
  const missing = db.all('SELECT * FROM docs');
  const [started, , row, , rows] = await Promise.all([begin,insert,read,rollback,missing]);
  assert.equal(started.transaction.after, 'active');
  assert.deepEqual(row[0].id, new Record('docs','p1'));
  assert.equal(row[0].value, 9223372036854775807n);
  assert.deepEqual(row[0].bytes, Buffer.from([255]));
  assert.deepEqual(rows, []);
  const queued = db.execute('INSERT INTO docs {id:docs:p2}');
  const close = db.close();
  assert.equal(db.close(), close);
  await assert.rejects(db.all('SELECT * FROM docs'), error => error.code === 'FDB_CLOSED' && !Object.hasOwn(error,'transaction'));
  await queued; await close;
});
test('async worker leaves event loop responsive and rejects excess queued requests', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const db = await AsyncDatabase.open();
  try {
    await db.execute('CREATE TABLE numbers(x INTEGER)');
    await db.execute('INSERT INTO numbers VALUES ' + Array.from({length:100}, (_,i) => `(${i+1})`).join(','));
    let completed = false;
    const computation = db.execute('SELECT sum(a.x) FROM numbers a CROSS JOIN numbers b CROSS JOIN numbers c').finally(() => { completed = true; });
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(completed, false);
    assert.deepEqual((await computation).rows, [[50500000n]]);
    const requests = Array.from({length: 257}, () => db.execute('SELECT 1'));
    const results = Promise.allSettled(requests);
    let ticked = false;
    await new Promise(resolve => setImmediate(() => { ticked = true; resolve(); }));
    const settled = await results;
    assert.equal(ticked, true);
    assert.equal(settled.filter(r => r.status === 'fulfilled').length, 256);
    assert.equal(settled[256].reason.code, 'FDB_LIMIT');
    assert.deepEqual(await db.exactlyOne('SELECT 2'), [2n]);
  } finally { await db.close(); }
});
test('async worker reports errors, transfers data and closes active transactions', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'romedb-async-'));
  let db;
  try {
    await assert.rejects(AsyncDatabase.open(path.join(dir, 'missing', 'db')));
    const file = path.join(dir, 'test.db');
    db = await AsyncDatabase.open(file);
    await db.execute('CREATE TABLE docs');
    await db.execute('INSERT INTO docs {id:docs:p1,value:1}');
    const data = await db.exportDocuments('docs','ndjson');
    await db.execute('DELETE FROM docs:p1');
    assert.equal((await db.importDocuments('docs',data,'ndjson')).imported,1);
    await db.execute('BEGIN');
    await db.execute('INSERT INTO docs {id:docs:p2}');
    await assert.rejects(db.execute('SELECT array::append(1,2)'), e => e.transaction.after === 'active');
    assert.equal((await db.all('SELECT * FROM docs')).length,2);
    await db.execute('ROLLBACK');
    await db.execute('BEGIN');
    await db.execute('INSERT INTO docs {id:docs:p3}');
    await db.close();
    db = await AsyncDatabase.open(file);
    assert.equal((await db.all('SELECT * FROM docs')).length,1);
    await db.close();
  } finally { if (db) await db.close(); fs.rmSync(dir,{recursive:true,force:true}); }
});
test('independent async workers isolate connections and failures', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const databases = await Promise.all([AsyncDatabase.open(), AsyncDatabase.open()]);
  try {
    await Promise.all(databases.map(db => db.execute('CREATE TABLE docs')));
    await Promise.all(databases.map((db,i) => db.execute('INSERT INTO docs {value:$value}', { $value: BigInt(i) })));
    assert.deepEqual(await Promise.all(databases.map(db => db.exactlyOne('SELECT value FROM docs'))), [[0n], [1n]]);
    await assert.rejects(databases[0].execute('SELECT array::append(1,2)'));
    assert.deepEqual(await databases[1].exactlyOne('SELECT value FROM docs'), [1n]);
  } finally { await Promise.all(databases.map(db => db.close())); }
});
test('async interrupt reaches active native work without terminating its worker', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const db = await AsyncDatabase.open();
  let timer;
  try {
    assert.equal(db.interrupt(),true); // Idle requests must not poison later work.
    await db.execute('CREATE TABLE numbers(x INTEGER)');
    await db.execute('INSERT INTO numbers VALUES ' + Array.from({length:100},(_,i)=>`(${i})`).join(','));
    await db.execute('CREATE TABLE sink(x INTEGER)');
    const operation = db.execute('INSERT INTO sink SELECT a.x FROM numbers a CROSS JOIN numbers b CROSS JOIN numbers c');
    timer = setInterval(() => db.interrupt(), 2);
    await assert.rejects(operation, e => e.code === 'FDB_CANCELLED' && e.transaction.after === 'autocommit');
    clearInterval(timer); timer = undefined;
    assert.deepEqual(await db.exactlyOne('SELECT count(*) FROM sink'),[0n]);
    await db.execute('INSERT INTO sink VALUES (1)');
    assert.deepEqual(await db.exactlyOne('SELECT count(*) FROM sink'),[1n]);
  } finally { clearInterval(timer); await db.close(); }
  assert.equal(db.interrupt(),false);
});
test('worker transport failures settle pending requests and permit cleanup', () => {
  const { spawnSync } = require('node:child_process');
  const result = spawnSync(process.execPath, [require.resolve('./worker-faults.cjs')], { encoding: 'utf8', timeout: 10000 });
  assert.equal(result.status, 0, result.stderr || String(result.error));
  assert.match(result.stdout, /worker-faults-complete/);
});
test('nested record targets follow validation even without a reference index', () => {
  const db = new Database();
  try {
    db.execute('CREATE TABLE docs');
    for (const table of ['__romedb_catalog', 'SQLITE_schema', 'bad\0target']) {
      assert.throws(() => db.execute('INSERT INTO docs DOCUMENT $doc', { $doc: { nested: [new Record(table, 'key')] } }), e => e.code === 'FDB_VALIDATION');
    }
    assert.deepEqual(db.all('SELECT * FROM docs'), []);
  } finally { db.close(); }
});
test('Node batches preserve offsets and stop at execution or encoding failures', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      const script = "-- é\nCREATE TABLE docs; BEGIN; INSERT INTO docs {id:docs:p1}; INSERT INTO docs {id:docs:p1}; COMMIT;";
      const reports = await db.executeBatch(script);
      assert.equal(reports.length,4);
      assert.equal(reports[0].offset,Buffer.byteLength('-- é\n'));
      assert.equal(reports[1].transaction.after,'active');
      assert(reports[3].error);
      assert.equal(reports[3].transaction.after,'active');
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT * FROM docs'),[]);
      const failedEncoding = await db.executeBatch('SELECT 1e308*1e308; INSERT INTO docs {id:docs:later};');
      assert.equal(failedEncoding.length,1);
      assert.equal(failedEncoding[0].error.code,'FDB_VALIDATION');
      assert.deepEqual(await db.all('SELECT * FROM docs'),[]);
      const prior=await db.executeBatch('INSERT INTO docs {id:docs:before}; SELECT * FROM docs;');
      assert.deepEqual(prior[1].result.rows[0][0].id,new Record('docs','before'));
      assert.equal(prior[0].result.affected,1n);
      if (db instanceof Database) assert.throws(() => db.executeBatch("INSERT INTO docs {id:docs:no}; SELECT 'unterminated"));
      else await assert.rejects(db.executeBatch("INSERT INTO docs {id:docs:no}; SELECT 'unterminated"));
      assert.equal((await db.all('SELECT * FROM docs')).length,1);
    } finally { await db.close(); }
  }
});

test('shared-file contention exposes busy codes and preserves committed values', () => {
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'romedb-contention-'));
  let a,b;
  try {
    const file=path.join(dir,'test.db');
    a=new Database(file); b=new Database(file);
    a.execute('CREATE TABLE docs');
    a.execute("INSERT INTO docs {id:docs:p1,value:1}");
    a.execute('BEGIN');
    a.execute("UPDATE docs:p1 {value:2}");
    assert.throws(() => b.execute("UPDATE docs:p1 {value:3}"), e => e.code==='FDB_BUSY' && e.transaction.after==='autocommit');
    assert.deepEqual(b.all('SELECT value FROM docs'),[[1n]]);
    a.execute('COMMIT');
    b.execute('BEGIN'); b.all('SELECT * FROM docs');
    a.execute("UPDATE docs:p1 {value:4}");
    assert.throws(() => b.execute("UPDATE docs:p1 {value:5}"), e => e.code==='FDB_BUSY_SNAPSHOT');
    b.execute('ROLLBACK');
    assert.deepEqual(b.all('SELECT value FROM docs'),[[4n]]);
  } finally { b?.close(); a?.close(); fs.rmSync(dir,{recursive:true,force:true}); }
});

test('sync and async SELECT profiles preserve typed values and bigint counters', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('BEGIN');
      // Counted index probes are real work. Keep ten matches in a selective
      // 400-row fixture so their lookup cost remains below a full scan.
      for (let n = 0; n < 400; n++) {
        await db.execute('INSERT INTO docs (id,bucket,data,flag) VALUES ($id,$bucket,$data,$flag)', {
          $id: new Record('docs', BigInt(n)), $bucket: BigInt(n % 40),
          $data: new Uint8Array([0, 255]), $flag: true,
        });
      }
      await db.execute('COMMIT');
      const sql = 'SELECT id,data,flag FROM docs WHERE bucket=$bucket ORDER BY id';
      const parameters = { $bucket: 2n };
      const scan = await db.profileSelect(sql, parameters);
      assert.deepEqual(scan.result, await db.execute(sql, parameters));
      assert.equal(scan.result.rows.length, 10);
      for (const value of Object.values(scan.metrics)) assert.equal(typeof value, 'bigint');
      assert.equal(scan.metrics.rowsWritten, 0n);
      assert.ok(scan.metrics.fullscanSteps >= 399n);
      await db.execute('CREATE INDEX docs_bucket ON docs(bucket)');
      const indexed = await db.profileSelect(sql, parameters);
      assert.deepEqual(indexed.result, scan.result);
      assert.ok(indexed.metrics.rowsRead < scan.metrics.rowsRead);
      assert.ok(indexed.metrics.fullscanSteps < scan.metrics.fullscanSteps);
      assert.deepEqual((await db.profileSelect(sql, parameters)).metrics, indexed.metrics);
      const native = await db.profileSelect('SELECT $value AS value', { $value: new Uint8Array([2, 10]) });
      assert.deepEqual(native.result.rows, [[Buffer.from([2, 10])]]);
      await db.execute('BEGIN');
      await assert.rejects(Promise.resolve().then(() => db.profileSelect('DELETE FROM docs')), error => {
        assert.equal(error.code, 'FDB_UNSUPPORTED');
        assert.equal(error.transaction.after, 'active');
        return true;
      });
      assert.equal((await db.execute('SELECT count(*) FROM docs')).rows[0][0], 400n);
      await db.execute('ROLLBACK');
    } finally {
      await db.close();
    }
    await assert.rejects(Promise.resolve().then(() => db.profileSelect('SELECT 1')), /clos/i);
  }
});

test('sync and async collection audits preserve work and enforce bigint limits', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('CREATE INDEX docs_n ON docs(n)');
      assert.equal((await db.checkCollectionIntegrity('docs', {maxDocuments: 0n, maxEncodedBytes: 0n})).documents, 0n);
      await db.execute('INSERT INTO docs {id:docs:a,n:1}');
      const audit = await db.checkCollectionIntegrity('docs');
      assert.equal(audit.documents, 1n);
      assert.equal(audit.indexes, 1n);
      assert.equal(audit.indexEntries, 1n);
      assert.ok(audit.encodedBytes > 0n);
      assert.equal(audit.transaction.after, 'autocommit');
      assert.equal((await db.checkCollectionIntegrity('docs', {maxEncodedBytes: audit.encodedBytes})).encodedBytes, audit.encodedBytes);
      await db.execute('BEGIN');
      await db.execute('INSERT INTO docs {id:docs:b,n:null}');
      await assert.rejects(Promise.resolve().then(() => db.checkCollectionIntegrity('docs', {maxDocuments: 1n})), error => {
        assert.equal(error.code, 'FDB_LIMIT'); assert.equal(error.transaction.after, 'active'); return true;
      });
      assert.equal((await db.checkCollectionIntegrity('docs')).documents, 2n);
      for (const limits of [{maxDocuments: 1}, {maxDocuments: -1n}, {maxEncodedBytes: 1n << 64n}, {unknown: 1n}, null]) {
        await assert.rejects(Promise.resolve().then(() => db.checkCollectionIntegrity('docs', limits)), /integrity limit/);
      }
      await db.execute('ROLLBACK');
      assert.equal((await db.checkCollectionIntegrity('docs')).documents, 1n);
      await assert.rejects(Promise.resolve().then(() => db.checkCollectionIntegrity('missing')), error => error.code === 'FDB_NOT_FOUND');
    } finally { await db.close(); }
    await assert.rejects(Promise.resolve().then(() => db.checkCollectionIntegrity('docs')), /clos/i);
  }
});

test('recursive SQL reports parser depth errors through sync and worker clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('BEGIN');
      await db.execute('INSERT INTO docs {v:1}');
      for (const expression of ['NOT '.repeat(2000) + '1', 'CASE WHEN 1 THEN '.repeat(200) + '1' + ' ELSE 0 END'.repeat(200)]) {
        for (const suffix of ['', ' FROM docs']) {
          await assert.rejects(Promise.resolve().then(() => db.execute('SELECT ' + expression + suffix)), error => {
            assert.equal(error.code, 'FDB_ENGINE');
            assert.match(error.message, /maximum depth 100/);
            assert.equal(error.transaction.after, 'active');
            return true;
          });
          await assert.rejects(Promise.resolve().then(() => db.profileSelect('SELECT ' + expression + suffix)), /maximum depth 100/);
        }
      }
      assert.equal((await db.exactlyOne('SELECT v FROM docs'))[0], 1n);
      await db.execute('ROLLBACK');
    } finally { await db.close(); }
  }
});

test('typed VALUES CTEs preserve mixed values and atomic inserts in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      const id = new Record('docs', 7n);
      const data = Buffer.from([0,255]);
      const input = {$id:id, $data:data, $flag:true, $array:[1n,null]};
      const result = await db.execute('WITH v(n,x) AS (VALUES (1,$id),(2,$data),(3,$flag),(4,$array),(5,NULL)) SELECT x FROM v ORDER BY n', input);
      assert.deepEqual(result.rows, [[id],[data],[true],[[1n,null]],[null]]);
      const union = await db.execute('SELECT $id AS x UNION ALL SELECT $data UNION ALL SELECT $flag', {$id:id, $data:data, $flag:true});
      assert.deepEqual(union.rows, [[id],[data],[true]]);
      assert.deepEqual((await db.execute('SELECT $id AS x UNION SELECT $id', {$id:id})).rows, [[id]]);
      assert.deepEqual((await db.execute('SELECT $id AS x INTERSECT SELECT $data', {$id:id, $data:data})).rows, []);
      assert.deepEqual((await db.execute('SELECT $id AS x EXCEPT SELECT $data', {$id:id, $data:data})).rows, [[id]]);
      await db.execute('CREATE TABLE docs');
      await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
      await db.execute('BEGIN');
      await db.execute('WITH v(id,n) AS (VALUES ($id,1)) INSERT INTO docs(id,n) SELECT id,n FROM v', {$id:id});
      await assert.rejects(Promise.resolve().then(() => db.execute('WITH v(id,n) AS (VALUES (docs:b,2),(docs:c,1)) INSERT INTO docs(id,n) SELECT id,n FROM v')), error => error.code === 'FDB_CONSTRAINT' && error.transaction.after === 'active');
      assert.equal((await db.checkCollectionIntegrity('docs')).documents, 1n);
      assert.equal((await db.exactlyOne('SELECT n FROM docs'))[0], 1n);
      await db.execute('ROLLBACK');
      assert.equal((await db.checkCollectionIntegrity('docs')).documents, 0n);
    } finally { await db.close(); }
  }
});

test('vector factories match native encodings and work in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const factories = [['float32','vector32'],['float64','vector64'],['sparse32','vector32_sparse'],['quantized8','vector8'],['bit1','vector1bit']];
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      await db.execute('CREATE TABLE points');
      for (const [factory, sql] of factories) {
        for (const values of [[1,0,-1], new Float32Array([0.1,0.2,0.3]), new Float64Array([2.5,2.5,2.5]), new Array(9).fill(0)]) {
          const vector = Vector[factory](values);
          assert(vector instanceof Vector);
          const text = JSON.stringify(Array.from(values));
          const native = await db.exactlyOne(`SELECT ${sql}($text)`, {$text:text});
          assert.deepEqual(vector.bytes, native[0]);
          await db.execute('INSERT INTO points(v) VALUES ($v)', {$v:vector});
          assert.deepEqual((await db.exactlyOne('SELECT $v AS v', {$v:vector}))[0], vector);
        }
      }
      assert.equal((await db.checkCollectionIntegrity('points')).documents, 20n);
    } finally { await db.close(); }
  }
  for (const [factory] of factories) {
    for (const input of [[], new Float64Array(65537)]) assert.throws(() => Vector[factory](input), RangeError);
    for (const input of [[NaN],[Infinity],[-Infinity],[1n],['1'],[undefined],new Array(2), new Uint8Array([1])]) assert.throws(() => Vector[factory](input), TypeError);
    assert(Vector[factory](new Float32Array(65536)) instanceof Vector);
  }
  assert.throws(() => Vector.float32([Number.MAX_VALUE]), RangeError);
  assert(Vector.float64([Number.MAX_VALUE]) instanceof Vector);
  assert(Object.is(Vector.float32([-0]).bytes.readFloatLE(), -0));
  assert(Object.is(Vector.float64([-0]).bytes.readDoubleLE(), -0));
  const input = [1,2,3]; const vector = Vector.float32(input); input[0] = 99;
  assert.equal(vector.bytes.readFloatLE(0), 1);
  const { vectorFromComponents } = require('./romedb.node');
  for (const bytes of [Buffer.alloc(0),Buffer.alloc(7),Buffer.alloc(65537*8)]) assert.throws(() => vectorFromComponents('float32', bytes));
  assert.throws(() => vectorFromComponents('unknown', Buffer.alloc(8)));
  const invalid = Buffer.alloc(8); invalid.writeDoubleLE(NaN);
  for (const [factory] of factories) assert.throws(() => vectorFromComponents(factory, invalid));
});

test('vector factories preserve precision boundaries and reject conversion overflow locally', () => {
  const values = [1 + 2 ** -24, 1 + 3 * 2 ** -24, 2 ** -150, 3 * 2 ** -150, -(2 ** -150), -0];
  const expectedBits = [0x3f800000, 0x3f800002, 0, 2, 0x80000000, 0x80000000];
  const dense = Vector.float32(values);
  assert.deepEqual(values.map((_, i) => dense.bytes.readUInt32LE(i * 4)), expectedBits);
  const precise = Vector.float64([Number.MIN_VALUE, -Number.MIN_VALUE, 1 + Number.EPSILON, Number.MAX_VALUE]);
  assert.deepEqual([0,1,2,3].map(i => precise.bytes.readBigUInt64LE(i * 8)), [1n,0x8000000000000001n,0x3ff0000000000001n,0x7fefffffffffffffn]);
  assert.equal(Vector.bit1(values).bytes[0], 0b1011);
  const db = new Database();
  try {
    for (const [factory, sql] of [['sparse32','vector32_sparse'],['quantized8','vector8'],['bit1','vector1bit']]) {
      const native = db.exactlyOne(`SELECT ${sql}($bytes)`, {$bytes:dense.bytes})[0];
      assert.deepEqual(Vector[factory](values).bytes, native);
    }
    db.execute('CREATE TABLE docs');
    db.execute('BEGIN');
    db.execute('INSERT INTO docs(n) VALUES (7)');
    const max32 = (2 - 2 ** -23) * 2 ** 127;
    assert.throws(() => Vector.quantized8([-max32,max32]));
    assert(Vector.quantized8([max32,max32]) instanceof Vector);
    const result = db.execute('SELECT n FROM docs');
    assert.equal(result.transaction.after, 'active');
    assert.deepEqual(result.rows, [[7n]]);
    db.execute('ROLLBACK');
    assert.equal(db.checkCollectionIntegrity('docs').documents, 0n);
  } finally { db.close(); }
});

test('sparse entry vectors validate indices and roundtrip through both clients', async () => {
  const entries = [[0, 1 + 2 ** -24], [1, -0], [3, -2]];
  const vector = Vector.sparse32Entries(5, entries);
  assert.deepEqual(vector.bytes, Vector.sparse32([1, 0, 0, -2, 0]).bytes);
  entries[0][1] = 99;
  assert.equal(vector.bytes.readFloatLE(), 1);
  assert.equal(Vector.sparse32Entries(65536, []).bytes.length, 5);
  assert.equal(Vector.sparse32Entries(65536, [[0, -0], [65535, 0]]).bytes.length, 5);
  const last = Vector.sparse32Entries(65536, [[65535, 1]]);
  assert.equal(last.bytes.length, 13);
  assert.equal(last.bytes.readUInt32LE(4), 65535);
  assert.equal(last.bytes.readUInt32LE(8), 65536);
  for (const dimensions of [0, -1, 1.5, 65537, NaN, Infinity, '3', 3n]) assert.throws(() => Vector.sparse32Entries(dimensions, []), RangeError);
  for (const input of [null, {}, new Float32Array(2), [null], [[0]], [[0, 1, 2]], new Array(1), [[0, NaN]], [[0, Infinity]], [[0, 1n]]]) assert.throws(() => Vector.sparse32Entries(3, input), TypeError);
  for (const input of [[[0, 0], [0, -0]], [[1, 1], [0, 2]], [[-1, 1]], [[3, 1]], [[0.5, 1]], [[NaN, 1]], [['0', 1]], [[0, Number.MAX_VALUE]], [[0, 1], [1, 1], [2, 1], [3, 1]]]) assert.throws(() => Vector.sparse32Entries(3, input), RangeError);
  const { vectorFromSparseEntries } = require('./romedb.node');
  for (const dims of [0, 1.5, 65537, NaN, Infinity]) assert.throws(() => vectorFromSparseEntries(dims, Buffer.alloc(0)));
  for (const bytes of [Buffer.alloc(1), Buffer.alloc(13), Buffer.alloc(4 * 12)]) assert.throws(() => vectorFromSparseEntries(3, bytes));
  const invalid = Buffer.alloc(12);
  invalid.writeUInt32LE(3);
  assert.throws(() => vectorFromSparseEntries(3, invalid));
  invalid.writeUInt32LE(0);
  for (const value of [NaN, Infinity, Number.MAX_VALUE]) {
    invalid.writeDoubleLE(value, 4);
    assert.throws(() => vectorFromSparseEntries(3, invalid));
  }
  assert.throws(() => vectorFromSparseEntries(3, Buffer.alloc(24))); // Duplicate zero-valued indices.
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      await db.execute('CREATE TABLE points');
      await db.execute('DEFINE FIELD v ON points TYPE vector<5> REQUIRED');
      await db.execute('INSERT INTO points(v) VALUES ($v)', {$v:vector});
      assert.deepEqual((await db.exactlyOne('SELECT v FROM points'))[0], vector);
      assert.equal((await db.exactlyOne('SELECT vector_extract(vector32(v)) AS v FROM points'))[0], '[1,0,0,-2,0]');
      assert.equal((await db.checkCollectionIntegrity('points')).documents, 1n);
      await db.execute('BEGIN');
      assert.throws(() => Vector.sparse32Entries(5, [[5, 1]]), RangeError);
      assert.deepEqual((await db.exactlyOne('SELECT v FROM points'))[0], vector);
      await db.execute('ROLLBACK');
    } finally { await db.close(); }
  }
});


test('typed scalar subqueries preserve values in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('INSERT INTO docs {id:docs:a,n:1,link:docs:b,items:[1,true]}');
      const row = await db.exactlyOne('SELECT (SELECT link FROM docs) AS link,(SELECT items FROM docs) AS items,(SELECT n FROM docs WHERE n=99) AS missing');
      assert.deepEqual(row, [new Record('docs','b'), [1n,true], null]);
      assert.deepEqual(await db.exactlyOne('SELECT n FROM docs WHERE n=(SELECT max(n) FROM docs)'), [1n]);
      assert.deepEqual(await db.exactlyOne('SELECT DISTINCT n FROM docs LIMIT (SELECT $limit FROM docs) OFFSET (SELECT $offset FROM docs)', {$limit:1n,$offset:0n}), [1n]);
      assert.deepEqual(await db.exactlyOne('SELECT n FROM docs UNION SELECT n FROM docs ORDER BY n LIMIT (SELECT $limit FROM docs)', {$limit:1n}), [1n]);
      assert.deepEqual(await db.exactlyOne('SELECT EXISTS (SELECT link,items FROM docs) AS present,NOT EXISTS (SELECT n FROM docs WHERE n=99) AS absent'), [1n,1n]);
      assert.deepEqual(await db.exactlyOne('SELECT docs:b IN (SELECT link FROM docs) AS present,2 NOT IN (SELECT n FROM docs) AS absent,NULL IN (SELECT n FROM docs) AS unknown'), [1n,1n,null]);
      for (const value of [new Record('docs','key'), [1n,true], {ok:true}, Buffer.from([0,255]), Vector.float32([1,0])]) {
        assert.deepEqual(await db.exactlyOne('SELECT (SELECT $v AS v) AS v', {$v:value}), [value]);
        assert.deepEqual(await db.exactlyOne('WITH chosen AS (SELECT $v AS v) SELECT v FROM chosen', {$v:value}), [value]);
      }
      assert.deepEqual(await db.exactlyOne('SELECT ? AS n,(SELECT ? AS v) AS v', {'?1':3n,'?2':[true]}), [3n,[true]]);
      await db.execute('CREATE TABLE affinity_docs');
      await db.execute('INSERT INTO affinity_docs(n) VALUES (2)');
      assert.deepEqual(await db.exactlyOne("SELECT CAST('2' AS TEXT) IN (SELECT n FROM affinity_docs) AS v"), [0n]);
      await db.execute('CREATE TABLE affinity_native(n TEXT)');
      await db.execute("INSERT INTO affinity_native VALUES ('2')");
      assert.deepEqual(await db.exactlyOne('SELECT (SELECT n FROM affinity_native) AS v FROM docs'), ['2']);
      assert.deepEqual(await db.exactlyOne('SELECT EXISTS (SELECT n FROM affinity_native WHERE n=$n) AS present FROM docs', {$n:'2'}), [1n]);
      assert.deepEqual(await db.exactlyOne('SELECT (SELECT $v AS v FROM affinity_native) AS v', {$v:new Record('docs','native-source')}), [new Record('docs','native-source')]);
      assert.deepEqual(await db.exactlyOne('SELECT (SELECT $v AS v FROM affinity_native) AS v', {$v:[1n,true]}), [[1n,true]]);
      assert.deepEqual(await db.exactlyOne('WITH chosen AS (SELECT $v AS v FROM affinity_native) SELECT v FROM chosen', {$v:new Record('docs','cte-native')}), [new Record('docs','cte-native')]);
      assert.deepEqual(await db.exactlyOne('SELECT n IN (SELECT +n FROM affinity_docs) AS v FROM affinity_native'), [1n]);
      await db.execute('CREATE TABLE scalar_numeric(n INTEGER)');
      await db.execute('INSERT INTO scalar_numeric VALUES (2)');
      assert.deepEqual(await db.exactlyOne("SELECT '2'=(SELECT n FROM scalar_numeric) AS v FROM docs"), [1n]);
      assert.deepEqual(await db.exactlyOne("SELECT '2'=((SELECT n FROM scalar_numeric) COLLATE NOCASE) AS v FROM docs"), [1n]);
    } finally { await db.close(); }
  }
});

test('AbortSignal cancels only its queued or active query and cleans up listeners', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const { getEventListeners } = require('node:events');
  const db = await AsyncDatabase.open();
  let timer;
  try {
    await db.execute('CREATE TABLE numbers(n INTEGER)');
    await db.execute('INSERT INTO numbers VALUES ' + Array.from({length:100},(_,i)=>`(${i})`).join(','));
    await db.execute('CREATE TABLE docs');
    await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
    await db.execute('BEGIN');
    await db.execute('INSERT INTO docs {id:docs:prior,n:9}');
    const active = new AbortController(), queued = new AbortController();
    // A user listener must not suppress the driver's cancellation listener.
    active.signal.addEventListener('abort', event => event.stopImmediatePropagation(), {once:true});
    const first = db.execute('INSERT INTO docs(n) SELECT a.n FROM numbers a CROSS JOIN numbers b CROSS JOIN numbers c', {}, {signal:active.signal});
    const second = db.execute('INSERT INTO docs {n:77}', {}, {signal:queued.signal});
    const third = db.execute('INSERT INTO docs {n:88}');
    const outcomes = Promise.allSettled([first, second, third]);
    queued.abort();
    timer = setTimeout(() => active.abort(), 10);
    const [a,b,c] = await outcomes;
    for (const result of [a,b]) {
      assert.equal(result.status, 'rejected');
      assert.equal(result.reason.code, 'FDB_CANCELLED');
      assert.equal(result.reason.transaction.after, 'active');
    }
    assert.equal(c.status,'fulfilled');
    assert.deepEqual(await db.all('SELECT n FROM docs ORDER BY n'), [[9n],[88n]]);
    assert.equal((await db.checkCollectionIntegrity('docs')).documents,2n);
    assert.equal(getEventListeners(active.signal,'abort').length,0);
    assert.equal(getEventListeners(queued.signal,'abort').length,0);
    const finished = new AbortController();
    assert.deepEqual(await db.exactlyOne('SELECT 1', {}, {signal:finished.signal}),[1n]);
    assert.equal(getEventListeners(finished.signal,'abort').length,0);
    finished.abort();
    assert.deepEqual(await db.first('SELECT 2'),[2n]);
    const before = new AbortController(); before.abort();
    await assert.rejects(db.execute('INSERT INTO docs {n:66}',{}, {signal:before.signal}), e => e.code === 'FDB_CANCELLED');
    await assert.rejects(db.execute('SELECT 1',{}, {signal:{}}), TypeError);
    await db.execute('ROLLBACK');
    assert.deepEqual(await db.all('SELECT * FROM docs'),[]);
  } finally { clearTimeout(timer); await db.close(); }
});

test('AbortSignal profiling preserves transaction state and returns complete metrics on retry', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const { getEventListeners } = require('node:events');
  const db = await AsyncDatabase.open();
  let timer;
  try {
    await db.execute('CREATE TABLE numbers(n INTEGER)');
    await db.execute('INSERT INTO numbers VALUES ' + Array.from({length:100},(_,i)=>`(${i})`).join(','));
    await db.execute('BEGIN');
    await db.execute('CREATE TABLE prior');
    await db.execute('INSERT INTO prior {id:prior:a,n:9}');
    const active = new AbortController();
    const pending = db.profileSelect('SELECT count(*) FROM numbers a CROSS JOIN numbers b CROSS JOIN numbers c', {}, {signal:active.signal});
    const following = db.exactlyOne('SELECT n FROM prior');
    const settled = Promise.allSettled([pending,following]);
    timer = setTimeout(() => active.abort(),10);
    const [cancelled,success] = await settled;
    assert.equal(cancelled.status,'rejected');
    assert.equal(cancelled.reason.code,'FDB_CANCELLED');
    assert.equal(cancelled.reason.transaction.after,'active');
    assert.equal('metrics' in cancelled.reason,false);
    assert.equal(success.status,'fulfilled');
    assert.deepEqual(success.value,[9n]);
    assert.equal(getEventListeners(active.signal,'abort').length,0);
    const before = new AbortController(); before.abort();
    await assert.rejects(db.profileSelect('SELECT n FROM prior',{}, {signal:before.signal}), e => e.code === 'FDB_CANCELLED');
    const retry = new AbortController();
    const profile = await db.profileSelect('SELECT n FROM prior',{}, {signal:retry.signal});
    assert.deepEqual(profile.result.rows,[[9n]]);
    assert(profile.metrics.vmSteps>0n);
    assert.equal(profile.result.transaction.after,'active');
    assert.equal(getEventListeners(retry.signal,'abort').length,0);
    retry.abort();
    assert.deepEqual(await db.exactlyOne('SELECT n FROM prior'),[9n]);
    await db.execute('ROLLBACK');
  } finally { clearTimeout(timer); await db.close(); }
});

test('AbortSignal integrity audits preserve indexed data and allow exact retry', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const { getEventListeners } = require('node:events');
  const db = await AsyncDatabase.open();
  let timer;
  try {
    await db.execute('CREATE TABLE numbers(n INTEGER)');
    await db.execute('INSERT INTO numbers VALUES ' + Array.from({length:100},(_,i)=>`(${i})`).join(','));
    await db.execute('CREATE TABLE docs');
    await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
    await db.execute('INSERT INTO docs(n) SELECT a.n*10+b.n FROM numbers a CROSS JOIN numbers b WHERE b.n<10');
    await db.execute('BEGIN');
    await db.execute('INSERT INTO docs {id:docs:prior,n:1000}');
    const controller = new AbortController();
    const audit = db.checkCollectionIntegrity('docs',{}, {signal:controller.signal});
    const following = db.exactlyOne('SELECT n FROM docs WHERE id=docs:prior');
    const settled = Promise.allSettled([audit,following]);
    timer = setTimeout(() => controller.abort(),1);
    const [cancelled,read] = await settled;
    assert.equal(cancelled.status,'rejected');
    assert.equal(cancelled.reason.code,'FDB_CANCELLED');
    assert.equal(cancelled.reason.transaction.after,'active');
    assert.equal(read.status,'fulfilled');
    assert.deepEqual(read.value,[1000n]);
    assert.equal(getEventListeners(controller.signal,'abort').length,0);
    const before = new AbortController(); before.abort();
    await assert.rejects(db.checkCollectionIntegrity('docs',{}, {signal:before.signal}), e => e.code === 'FDB_CANCELLED');
    const retry = new AbortController();
    const complete = await db.checkCollectionIntegrity('docs',{}, {signal:retry.signal});
    assert.equal(complete.documents,1001n);
    assert.equal(complete.indexEntries,1001n);
    assert.equal(complete.transaction.after,'active');
    assert.equal(getEventListeners(retry.signal,'abort').length,0);
    retry.abort();
    await db.execute('ROLLBACK');
    assert.deepEqual(await db.exactlyOne('SELECT count(*) FROM docs'),[1000n]);
  } finally { clearTimeout(timer); await db.close(); }
});

test('AbortSignal batches retain completed reports and stop before later statements', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const { getEventListeners } = require('node:events');
  const db = await AsyncDatabase.open();
  let timer;
  try {
    await db.execute('CREATE TABLE numbers(n INTEGER)');
    await db.execute('INSERT INTO numbers VALUES ' + Array.from({length:100},(_,i)=>`(${i})`).join(','));
    await db.execute('CREATE TABLE docs');
    await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
    await db.execute('BEGIN');
    const prefix = '-- ไทย\nINSERT INTO docs {id:docs:prior,n:9}; ';
    const script = prefix + 'INSERT INTO docs(n) SELECT a.n*10000+b.n*100+c.n FROM numbers a CROSS JOIN numbers b CROSS JOIN numbers c; DELETE FROM docs; COMMIT;';
    const controller = new AbortController();
    const batch = db.executeBatch(script,{signal:controller.signal});
    const following = db.exactlyOne('SELECT n FROM docs WHERE id=docs:prior');
    timer = setTimeout(() => controller.abort(),50);
    const [reports,read] = await Promise.all([batch,following]);
    assert.equal(reports.length,2);
    assert.equal(reports[0].result.affected,1n);
    assert.equal(reports[1].offset,Buffer.byteLength(prefix));
    assert.equal(reports[1].error.code,'FDB_CANCELLED');
    assert.equal(reports[1].transaction.before,'active');
    assert.equal(reports[1].transaction.after,'active');
    assert.deepEqual(read,[9n]);
    assert.equal((await db.checkCollectionIntegrity('docs')).documents,1n);
    assert.equal(getEventListeners(controller.signal,'abort').length,0);
    const before = new AbortController(); before.abort();
    await assert.rejects(db.executeBatch("SELECT '",{signal:before.signal}),e=>e.code==='FDB_CANCELLED');
    const fresh = new AbortController();
    const retry = await db.executeBatch('INSERT INTO docs {id:docs:retry,n:10}; SELECT n FROM docs ORDER BY n;', {signal:fresh.signal});
    assert.deepEqual(retry[1].result.rows,[[9n],[10n]]);
    assert.equal(getEventListeners(fresh.signal,'abort').length,0);
    fresh.abort();
    await db.execute('ROLLBACK');
    assert.deepEqual(await db.exactlyOne('SELECT count(*) FROM docs'),[0n]);
  } finally { clearTimeout(timer); await db.close(); }
});

test('AbortSignal transfers preserve atomic imports and return complete exports on retry', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const { getEventListeners } = require('node:events');
  const source = new Database();
  try {
    source.execute('CREATE TABLE docs');
    source.execute('INSERT INTO docs(n) VALUES ' + Array.from({length:1000},(_,i)=>`(${i})`).join(','));
    for (const format of ['json','ndjson']) {
      const payload = source.exportDocuments('docs',format);
      const db = await AsyncDatabase.open();
      let timer;
      try {
        await db.execute('CREATE TABLE docs');
        await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
        await db.execute('BEGIN');
        await db.execute('INSERT INTO docs {id:docs:prior,n:1000}');
        const controller = new AbortController();
        const importing = db.importDocuments('docs',payload,format,{signal:controller.signal});
        const following = db.exactlyOne('SELECT n FROM docs WHERE id=docs:prior');
        const results = Promise.allSettled([importing,following]);
        timer = setTimeout(()=>controller.abort(),20);
        const [cancelled,read] = await results;
        assert.equal(cancelled.status,'rejected');
        assert.equal(cancelled.reason.code,'FDB_CANCELLED',cancelled.reason.message);
        assert.equal(cancelled.reason.transaction.after,'active');
        assert.deepEqual(read.value,[1000n]);
        assert.equal((await db.checkCollectionIntegrity('docs')).documents,1n);
        assert.equal(getEventListeners(controller.signal,'abort').length,0);
        const before = new AbortController(); before.abort();
        await assert.rejects(db.importDocuments('docs','invalid',format,{signal:before.signal}),e=>e.code==='FDB_CANCELLED');
        await assert.rejects(db.exportDocuments('docs',format,{signal:before.signal}),e=>e.code==='FDB_CANCELLED');
        const fresh = new AbortController();
        const imported = await db.importDocuments('docs',payload,format,{signal:fresh.signal});
        assert.equal(imported.imported,1000);
        assert.equal(getEventListeners(fresh.signal,'abort').length,0);
        const exporting = new AbortController();
        const output = db.exportDocuments('docs',format,{signal:exporting.signal});
        const rejected = assert.rejects(output,e=>e.code==='FDB_CANCELLED' && e.transaction.after==='active');
        timer = setTimeout(()=>exporting.abort(),1);
        await rejected;
        assert.equal(getEventListeners(exporting.signal,'abort').length,0);
        const complete = await db.exportDocuments('docs',format,{signal:fresh.signal});
        assert.equal(complete,await db.exportDocuments('docs',format));
        assert.equal((await db.checkCollectionIntegrity('docs')).documents,1001n);
        fresh.abort();
        await db.execute('ROLLBACK');
        assert.deepEqual(await db.exactlyOne('SELECT count(*) FROM docs'),[0n]);
      } finally { clearTimeout(timer); await db.close(); }
    }
  } finally { source.close(); }
});

test('AbortSignal migrations roll back all pending schema, data and history', async () => {
  const { AsyncDatabase, isRomeDBError } = require('./index.cjs');
  const { getEventListeners } = require('node:events');
  const db = await AsyncDatabase.open();
  let timer;
  try {
    const base = {version:1n,name:'base',sql:'CREATE TABLE docs; INSERT INTO docs {id:docs:prior,n:9}; CREATE TABLE numbers(n INTEGER);'};
    await db.migrate([base]);
    await db.execute('INSERT INTO numbers VALUES ' + Array.from({length:100},(_,i)=>`(${i})`).join(','));
    const intermediate = {version:2n,name:'intermediate',sql:'CREATE TABLE staged; INSERT INTO staged {id:staged:first};'};
    const pending = {version:3n,name:'pending',sql:'CREATE TABLE pending; CREATE UNIQUE INDEX pending_n ON pending(n); INSERT INTO pending(n) SELECT a.n*10000+b.n*100+c.n FROM numbers a CROSS JOIN numbers b CROSS JOIN numbers c;'};
    const controller = new AbortController();
    const operation = db.migrate([base,intermediate,pending],{signal:controller.signal});
    const rejected = assert.rejects(operation,e=>{
      assert(isRomeDBError(e));
      assert.equal(e.code,'FDB_CANCELLED');
      assert.equal(e.transaction.after,'autocommit');
      assert.equal(e.migration.version,pending.version);
      assert.equal(e.migration.offset,BigInt(Buffer.byteLength(pending.sql.slice(0,pending.sql.indexOf('INSERT INTO')))));
      assert.equal(e.migration.cause.code,'FDB_CANCELLED');
      return true;
    });
    timer=setTimeout(()=>controller.abort(),100);
    await rejected;
    assert.equal(getEventListeners(controller.signal,'abort').length,0);
    assert.deepEqual(await db.exactlyOne('SELECT n FROM docs'),[9n]);
    await assert.rejects(db.execute('SELECT * FROM pending'));
    await assert.rejects(db.execute('SELECT * FROM staged'));
    assert.equal((await db.migrate([base])).alreadyApplied,1);
    const before = new AbortController(); before.abort();
    await assert.rejects(db.migrate([base,intermediate,pending],{signal:before.signal}),e=>{
      assert(isRomeDBError(e));
      assert.equal(e.code,'FDB_CANCELLED');
      assert.equal(e.migration,undefined);
      return true;
    });
    const fresh = new AbortController();
    const retry = {...pending,sql:'CREATE TABLE pending; CREATE UNIQUE INDEX pending_n ON pending(n); INSERT INTO pending(n) VALUES (1),(2);'};
    assert.deepEqual((await db.migrate([base,intermediate,retry],{signal:fresh.signal})).applied,[2n,3n]);
    assert.equal((await db.migrate([base,intermediate,retry])).alreadyApplied,3);
    assert.equal((await db.checkCollectionIntegrity('pending')).documents,2n);
    assert.equal(getEventListeners(fresh.signal,'abort').length,0);
    fresh.abort();
    assert.deepEqual(await db.exactlyOne('SELECT n FROM docs'),[9n]);
  } finally {clearTimeout(timer); await db.close();}
});

test('close drains cancelled operations and rolls back the remaining outer transaction', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const { getEventListeners } = require('node:events');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(),'romedb-cancel-close-'));
  const file = path.join(dir,'test.db');
  const db = await AsyncDatabase.open(file);
  let timer;
  try {
    await db.execute('CREATE TABLE numbers(n INTEGER)');
    await db.execute('INSERT INTO numbers VALUES ' + Array.from({length:100},(_,i)=>`(${i})`).join(','));
    await db.execute('CREATE TABLE docs');
    await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
    await db.execute('INSERT INTO docs {id:docs:committed,n:7}');
    await db.execute('BEGIN');
    await db.execute('INSERT INTO docs {id:docs:prior,n:9}');
    const active = new AbortController();
    const queued = new AbortController();
    const tasks = [
      db.execute('INSERT INTO docs(n) SELECT a.n*10000+b.n*100+c.n FROM numbers a CROSS JOIN numbers b CROSS JOIN numbers c',{}, {signal:active.signal}),
      db.executeBatch('DELETE FROM docs;', {signal:queued.signal}),
      db.profileSelect('SELECT * FROM docs',{}, {signal:queued.signal}),
      db.checkCollectionIntegrity('docs',{}, {signal:queued.signal}),
      db.importDocuments('docs','invalid','json',{signal:queued.signal}),
      db.exportDocuments('docs','ndjson',{signal:queued.signal}),
      db.migrate([], {signal:queued.signal}),
    ];
    const settled = Promise.allSettled(tasks);
    queued.abort();
    const closing = db.close();
    assert.equal(db.close(),closing);
    await assert.rejects(db.execute('SELECT 1'),/closing or closed/);
    timer = setTimeout(()=>active.abort(),20);
    const results = await settled;
    for (const result of results) {
      assert.equal(result.status,'rejected');
      assert.equal(result.reason.code,'FDB_CANCELLED');
      assert.equal(result.reason.transaction.after,'active');
    }
    await closing;
    assert.equal(db.interrupt(),false);
    assert.equal(getEventListeners(active.signal,'abort').length,0);
    assert.equal(getEventListeners(queued.signal,'abort').length,0);
    const reopened = new Database(file);
    try {
      assert.deepEqual(reopened.exactlyOne('SELECT n FROM docs'),[7n]);
      assert.equal(reopened.checkCollectionIntegrity('docs').documents,1n);
    } finally { reopened.close(); }
  } finally { clearTimeout(timer); await db.close(); fs.rmSync(dir,{recursive:true,force:true}); }
});

test('native membership compound queries prepare in sync and worker clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [()=>new Database(),()=>AsyncDatabase.open()]) {
    const db=await open();
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('INSERT INTO docs(n) VALUES (1),(2),(NULL)');
      await db.execute('CREATE TABLE rhs(n INTEGER)');
      await db.execute('INSERT INTO rhs VALUES (1),(NULL)');
      assert.deepEqual(await db.all('SELECT n IN (SELECT n FROM rhs) FROM docs UNION ALL SELECT n NOT IN (SELECT n FROM rhs) FROM docs'),[[1n],[null],[null],[0n],[null],[null]]);
      assert.deepEqual(await db.all('WITH r AS (SELECT n FROM rhs) SELECT n IN (SELECT n FROM r) FROM docs'),[[1n],[null],[null]]);
    } finally { await db.close(); }
  }
});

test('profiles expose separate forward target counters in both clients', async () => {
  const {AsyncDatabase}=require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open()]) {
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('INSERT INTO docs {id:docs:a,n:1}');
      const sql='SELECT record::fetch(docs:a) AS a,record::fetch(docs:a) AS b';
      const profile=await db.profileSelect(sql);
      assert.equal(profile.metrics.fetchBatches,1n);
      assert(profile.metrics.fetchRowsRead>=1n);
      assert(profile.metrics.fetchVmSteps>0n);
      assert.deepEqual(profile.result.rows[0][0],profile.result.rows[0][1]);
      assert.deepEqual((await db.profileSelect(sql)).metrics,profile.metrics);
      const plain=await db.profileSelect('SELECT 1');
      assert.equal(plain.metrics.fetchBatches,0n);
      assert.equal(plain.metrics.fetchRowsRead,0n);
      assert.equal(plain.metrics.fetchVmSteps,0n);
    } finally {await db.close();}
  }
});

test('leading WITH updates and deletes work in both clients', async () => {
  const {AsyncDatabase}=require('./index.cjs');
  for (const db of [new Database(),await AsyncDatabase.open()]) {
    try {
      await db.execute('CREATE TABLE docs');
      const cte = 'WITH docs AS (SELECT 2 AS n), chosen AS (SELECT docs.n FROM docs) SELECT chosen.n FROM chosen';
      assert.deepEqual(await db.all(cte), [[2n]]);
      assert.deepEqual((await db.profileSelect(cte)).result.rows, [[2n]]);
      await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
      await db.execute('INSERT INTO docs(n) VALUES(1),(2),(3)');
      await db.execute('BEGIN');
      const updated=await db.execute('WITH chosen AS (SELECT $n AS n) UPDATE docs SET n=(SELECT n FROM chosen)+10 WHERE n IN (SELECT n FROM chosen) RETURNING n',{$n:2n});
      assert.deepEqual(updated.rows,[[12n]]);
      assert.equal(updated.affected,1n);
      const deleted=await db.execute('WITH chosen AS (SELECT n FROM docs WHERE n>$min) DELETE FROM docs WHERE n IN (SELECT n FROM chosen) RETURNING n',{$min:10n});
      assert.deepEqual(deleted.rows,[[12n]]);
      assert.equal((await db.checkCollectionIntegrity('docs')).documents,2n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT n FROM docs ORDER BY n'),[[1n],[2n],[3n]]);
    } finally {await db.close();}
  }
});


test('native scalar predicates correlate in sync and worker clients', async () => {
  const {AsyncDatabase}=require('./index.cjs');
  for (const db of [new Database(),await AsyncDatabase.open()]) {
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
      await db.execute('INSERT INTO docs(n) VALUES(1),(2),(3)');
      await db.execute('CREATE TABLE lookup(n INTEGER)');
      await db.execute('INSERT INTO lookup VALUES(1),(2),(3)');
      const sql='SELECT d.n,(SELECT max(n) FROM lookup WHERE n<d.n) AS prior FROM docs AS d ORDER BY d.n';
      assert.deepEqual(await db.all(sql),[[1n,null],[2n,1n],[3n,2n]]);
      assert.deepEqual((await db.profileSelect(sql)).result.rows,[[1n,null],[2n,1n],[3n,2n]]);
      const grouped='SELECT d.n,(SELECT max(n) AS maximum FROM lookup HAVING maximum>d.n) FROM docs AS d ORDER BY d.n';
      assert.deepEqual(await db.all(grouped),[[1n,3n],[2n,3n],[3n,null]]);
      assert.deepEqual((await db.profileSelect(grouped)).result.rows,[[1n,3n],[2n,3n],[3n,null]]);
      const membership='SELECT d.n,d.n IN(SELECT n FROM lookup WHERE n=d.n AND n<3) FROM docs AS d ORDER BY d.n';
      assert.deepEqual(await db.all(membership),[[1n,1n],[2n,1n],[3n,0n]]);
      assert.deepEqual((await db.profileSelect(membership)).result.rows,[[1n,1n],[2n,1n],[3n,0n]]);
      await db.execute('BEGIN');
      const result=await db.execute('UPDATE docs AS d SET n=(SELECT max(n) FROM lookup WHERE n<=d.n)+$delta RETURNING n',{$delta:10n});
      assert.equal(result.affected,3n);
      assert.deepEqual(result.rows,[[11n],[12n],[13n]]);
      assert.equal((await db.checkCollectionIntegrity('docs')).documents,3n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT n FROM docs ORDER BY n'),[[1n],[2n],[3n]]);
    } finally {await db.close();}
  }
});


test('async startup failure permits repeated opens and a healthy retry', {timeout:10000}, async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const fs = require('node:fs');
  const os = require('node:os');
  const path = require('node:path');
  const directory = fs.mkdtempSync(path.join(os.tmpdir(),'romedb-open-failure-'));
  try {
    for (let attempt=0; attempt<3; attempt++) {
      await assert.rejects(AsyncDatabase.open(path.join(directory,'missing','database.db')), error=>error.code==='FDB_WORKER');
    }
    const file=path.join(directory,'database.db');
    const db=await AsyncDatabase.open(file);
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('INSERT INTO docs {id:docs:saved,n:7}');
    } finally { await db.close(); }
    const reopened=await AsyncDatabase.open(file);
    try { assert.deepEqual(await reopened.exactlyOne('SELECT n FROM docs'),[7n]); }
    finally { await reopened.close(); }
  } finally { fs.rmSync(directory,{recursive:true,force:true}); }
});


test('closed sync and worker handles expose FDB_CLOSED across operations', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const operations = [
    db=>db.execute('SELECT 1'), db=>db.all('SELECT 1'), db=>db.first('SELECT 1'), db=>db.exactlyOne('SELECT 1'),
    db=>db.profileSelect('SELECT 1'), db=>db.executeBatch('SELECT 1;'),
    db=>db.checkCollectionIntegrity('docs'), db=>db.exportDocuments('docs'),
    db=>db.importDocuments('docs','{}'), db=>db.migrate([]),
  ];
  const closed = error => error.code === 'FDB_CLOSED' && !Object.hasOwn(error,'transaction');
  const sync = new Database();
  sync.close(); sync.close();
  for (const operation of operations) assert.throws(()=>operation(sync),closed);
  const worker = await AsyncDatabase.open();
  const closing=worker.close();
  assert.equal(worker.close(),closing);
  await closing;
  for (const operation of operations) await assert.rejects(operation(worker),closed);
});


test('exactlyOne cardinality errors retain completed statement transaction observations', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open()]) {
    const reject = (sql, state) => assert.rejects(async()=>db.exactlyOne(sql), error=>
      error instanceof RangeError && error.code==='FDB_CARDINALITY' &&
      error.transaction.before===state && error.transaction.after===state);
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
      await reject('SELECT n FROM docs','autocommit');
      await reject('INSERT INTO docs(n) VALUES(1),(2) RETURNING n','autocommit');
      assert.deepEqual(await db.all('SELECT n FROM docs ORDER BY n'),[[1n],[2n]]);
      await db.execute('BEGIN');
      await reject('UPDATE docs SET n=n+10 RETURNING n','active');
      assert.deepEqual(await db.all('SELECT n FROM docs ORDER BY n'),[[11n],[12n]]);
      assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,2n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT n FROM docs ORDER BY n'),[[1n],[2n]]);
      assert.deepEqual(await db.exactlyOne('SELECT n FROM docs WHERE n=1'),[1n]);
    } finally { await db.close(); }
  }
});

test('exactlyOne preserves execution errors and worker cancellation', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open()]) {
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
      await db.execute('INSERT INTO docs {n:1}');
      await db.execute('BEGIN');
      await assert.rejects(async()=>db.exactlyOne('INSERT INTO docs {n:1} RETURNING n'), error=>
        error.code === 'FDB_CONSTRAINT' && error.transaction.before==='active' && error.transaction.after==='active');
      assert.deepEqual(await db.exactlyOne('SELECT count(*) FROM docs'),[1n]);
      if (db instanceof AsyncDatabase) {
        const controller=new AbortController(); controller.abort();
        await assert.rejects(db.exactlyOne('SELECT n FROM docs',{}, {signal:controller.signal}), error=>
          error.code==='FDB_CANCELLED' && error.transaction.before==='active' && error.transaction.after==='active');
        assert.deepEqual(await db.exactlyOne('SELECT n FROM docs'),[1n]);
      }
      await db.execute('ROLLBACK');
      assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,1n);
    } finally { await db.close(); }
  }
});


test('isRomeDBError recognizes public errors and validates transaction observations', async () => {
  const { AsyncDatabase, isRomeDBError } = require('./index.cjs');
  for (const value of [null,undefined,{},new Error('ordinary'),{code:'FDB_ENGINE'},Object.assign(new Error(),{code:'FDB_'}),Object.assign(new Error(),{code:'FDB_ENGINE',transaction:null}),Object.assign(new Error(),{code:'FDB_ENGINE',transaction:{before:'active',after:'committed'}})]) {
    assert.equal(isRomeDBError(value),false);
  }
  for (const open of [()=>new Database(), ()=>AsyncDatabase.open()]) {
    const db = await open();
    try {
      await db.execute('CREATE TABLE docs');
      await assert.rejects(async()=>db.execute('INSERT INTO docs {n:$missing}'), error=>isRomeDBError(error) && error.code==='FDB_PARAMETER');
      await assert.rejects(async()=>db.exactlyOne('SELECT 1 WHERE 0'), error=>isRomeDBError(error) && error.code==='FDB_CARDINALITY' && error.transaction.after==='autocommit');
    } finally { await db.close(); }
    await assert.rejects(async()=>db.all('SELECT 1'), error=>isRomeDBError(error) && error.code==='FDB_CLOSED' && error.transaction===undefined);
  }
});

test('migration diagnostics preserve UTF-8 locations and history reasons in both clients', async () => {
  const { AsyncDatabase, isRomeDBError } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      const sql = "-- café 日本語\nSELECT 'unterminated";
      const offset = Buffer.byteLength(sql.slice(0, sql.indexOf("'")));
      const plan = [
        {version:1n,name:'base.sql',sql:'CREATE TABLE docs;'},
        {version:9007199254740993n,name:'pending.sql',sql},
      ];
      await assert.rejects(async () => db.migrate(plan), error => {
        assert(isRomeDBError(error));
        assert.equal(error.code,'FDB_SYNTAX');
        assert(error.message.includes(`byte ${offset}:`),error.message);
        assert(error.message.includes('migration 9007199254740993:'),error.message);
        assert.deepEqual(error.transaction,{before:'autocommit',after:'autocommit'});
        return true;
      });
      await assert.rejects(async () => db.all('SELECT * FROM docs'));
      plan[1].sql = 'SELECT 1;';
      assert.deepEqual((await db.migrate(plan)).applied,[1n,9007199254740993n]);
      for (const [field,value,reason] of [
        ['name','renamed.sql','name differs'],
        ['sql','SELECT 1; -- edited','SQL source differs'],
        ['version',9007199254740994n,'version does not match the applied sequence'],
      ]) {
        const changed = [plan[0],{...plan[1],[field]:value}];
        await assert.rejects(async () => db.migrate(changed),error => {
          assert.equal(error.code,'FDB_VALIDATION');
          assert(error.message.includes(reason),error.message);
          assert.deepEqual(error.transaction,{before:'autocommit',after:'autocommit'});
          return true;
        });
      }
      assert.equal((await db.migrate(plan)).alreadyApplied,2);
    } finally { await db.close(); }
  }
});

test('composite count preserves null presence through both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      await db.execute('CREATE TABLE docs');
      for (const [n,v] of [[1n,[]],[2n,{a:1n}],[3n,new Record('users','one')],[4n,Buffer.from([0,1])],[5n,null],[6n,false]]) {
        await db.execute('INSERT INTO docs(n,v) VALUES($n,$v)',{$n:n,$v:v});
      }
      await db.execute('INSERT INTO docs {n:7}');
      const sql='SELECT count(v),count(missing),count(*) FROM docs';
      assert.deepEqual(await db.exactlyOne(sql),[5n,0n,7n]);
      assert.deepEqual((await db.profileSelect(sql)).result.rows,[[5n,0n,7n]]);
      for (const value of [[],{},new Record('users','two'),Buffer.alloc(0),false,null]) {
        assert.deepEqual(await db.exactlyOne('SELECT count($value) FROM docs',{$value:value}),[value===null?0n:7n]);
      }
      assert.deepEqual(await db.all('SELECT count(v) OVER (ORDER BY n) FROM docs ORDER BY n'),[[1n],[2n],[3n],[4n],[4n],[5n],[5n]]);
    } finally { await db.close(); }
  }
});

test('unnamed derived joins preserve client values and ambiguity errors', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      await db.execute('CREATE TABLE docs');
      const bytes = Buffer.from([70,68,66,1,0,255]);
      await db.execute('INSERT INTO docs {n:1,flag:true,link:docs:b,data:$data}', {$data:bytes});
      await db.execute('CREATE TABLE labels(m INTEGER,label TEXT)');
      await db.execute("INSERT INTO labels VALUES(1,'A')");
      const sql = 'SELECT n,flag,link,data,label FROM (SELECT n,flag,link,data FROM docs) JOIN labels ON n=m';
      const expected = [1n,true,new Record('docs','b'),bytes,'A'];
      assert.deepEqual(await db.exactlyOne(sql),expected);
      assert.deepEqual((await db.profileSelect(sql)).result.rows,[expected]);
      await db.execute('ALTER TABLE labels ADD COLUMN n INTEGER');
      await assert.rejects(async () => db.execute(sql), {code:'FDB_VALIDATION'});
      assert.deepEqual(await db.exactlyOne('SELECT d.n,flag,link,data,label FROM (SELECT n,flag,link,data FROM docs) d JOIN labels l ON d.n=l.m'),expected);
    } finally { await db.close(); }
  }
});

test('JOIN membership preserves binary bindings and NULL rows in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      const bytes = Buffer.from([70, 68, 66, 1, 0, 255]);
      const other = Buffer.from([49]);
      await db.execute('CREATE TABLE docs');
      await db.execute('INSERT INTO docs {n:1,data:$data}', {$data:bytes});
      await db.execute('INSERT INTO docs {n:2,data:$data}', {$data:other});
      await db.execute('CREATE TABLE labels(v BLOB)');
      await db.execute('INSERT INTO labels VALUES($value)', {$value:bytes});
      await db.execute('CREATE TABLE marker(m INTEGER)');
      await db.execute('INSERT INTO marker VALUES(7)');
      for (const operator of ['IN', 'NOT IN']) {
        const sql = `SELECT d.n,d.data,marker.m FROM (SELECT n,data FROM docs) d LEFT JOIN marker ON d.data ${operator} (SELECT v FROM labels WHERE v=$value) ORDER BY d.n`;
        const expected = operator === 'IN'
          ? [[1n, bytes, 7n], [2n, other, null]]
          : [[1n, bytes, null], [2n, other, 7n]];
        assert.deepEqual((await db.execute(sql, {$value:bytes})).rows, expected);
        assert.deepEqual((await db.profileSelect(sql, {$value:bytes})).result.rows, expected);
      }
    } finally { await db.close(); }
  }
});


test('native loader enforces the declared Node minimum before loading', () => {
  const vm = require('node:vm');
  const source = fs.readFileSync(path.join(__dirname, 'native.cjs'), 'utf8');
  const declared = require('./package.json').engines.node;
  assert.equal(declared, '>=22');
  for (const version of ['18.20.0', '20.19.0', '21.7.0', '22.0.0', '24.19.0']) {
    let loads = 0;
    const addon = {};
    const context = {
      process: { versions: { node: version }, platform: 'linux', arch: 'x64' },
      module: { exports: {} },
      require(name) { assert.equal(name, './romedb.node'); loads++; return addon; },
    };
    if (Number.parseInt(version, 10) < 22) {
      assert.throws(() => vm.runInNewContext(source, context), error => {
        assert.equal(error.code, 'FDB_RUNTIME_VERSION');
        assert(error.message.includes(version));
        assert(error.message.includes('22 or newer'));
        return true;
      });
      assert.equal(loads, 0);
    } else {
      vm.runInNewContext(source, context);
      assert.equal(loads, 1);
      assert.equal(context.module.exports, addon);
    }
  }
});

test('duplicate projection names preserve positional values in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('INSERT INTO docs {n:1,flag:true,data:$data}', { $data: Buffer.from([49]) });
      const result = await db.execute('SELECT flag AS x,data AS x FROM docs');
      assert.deepEqual(result.columns, ['x', 'x']);
      assert.deepEqual(result.rows, [[true, Buffer.from([49])]]);
      const nested = await db.execute('SELECT q.* FROM (SELECT flag AS x,data AS x FROM docs) q');
      assert.deepEqual(nested.columns, result.columns);
      assert.deepEqual(nested.rows, result.rows);
      const cte = await db.execute('WITH q(x,x) AS (SELECT flag,data FROM docs), r AS (SELECT q.* FROM q) SELECT r.* FROM r');
      assert.deepEqual(cte.columns, result.columns);
      assert.deepEqual(cte.rows, result.rows);
      const foldedCte = await db.execute('WITH q(x,X) AS (SELECT flag,data FROM docs), r AS (SELECT q.* FROM q) SELECT r.* FROM r');
      assert.deepEqual(foldedCte.columns, ['x', 'x']);
      assert.deepEqual(foldedCte.rows, result.rows);
      const inheritedNames = await db.execute('WITH q AS (SELECT flag AS x,data AS X FROM docs) SELECT q.* FROM q');
      assert.deepEqual(inheritedNames.columns, ['x', 'X']);
      assert.deepEqual(inheritedNames.rows, result.rows);
      const declaredName = await db.execute('WITH q("Flag") AS (SELECT flag FROM docs) SELECT q."FLAG",q."Flag" AS "Public Name" FROM q');
      assert.deepEqual(declaredName.columns, ['flag', 'Public Name']);
      assert.deepEqual(declaredName.rows, [[true, true]]);
      const star = await db.execute('SELECT q.* FROM docs d JOIN (SELECT 10 AS x,20 AS x) q ON 1');
      assert.deepEqual(star.columns, ['x', 'x']);
      assert.deepEqual(star.rows, [[10n, 20n]]);
      const nativeCte = await db.execute('WITH q(x,x) AS (SELECT 10,20) SELECT v.* FROM docs d JOIN q v ON 1');
      assert.deepEqual(nativeCte.columns, star.columns);
      assert.deepEqual(nativeCte.rows, star.rows);
      const named = await db.execute('WITH q(x,x) AS (SELECT 10,20) SELECT v.X FROM docs d JOIN q v ON 1');
      assert.deepEqual(named.columns, ['x']);
      assert.deepEqual(named.rows, [[10n]]);
      const using = await db.execute('SELECT * FROM (SELECT n FROM docs) a JOIN (SELECT 1 AS n,2 AS extra) b USING(n)');
      assert.deepEqual(using.columns, ['n','extra']);
      assert.deepEqual(using.rows, [[1n,2n]]);
      const natural = await db.execute('SELECT * FROM (SELECT n FROM docs) a NATURAL JOIN (SELECT 1 AS n,2 AS extra) b');
      assert.deepEqual(natural.columns, using.columns);
      assert.deepEqual(natural.rows, using.rows);
      const correlatedUsing = await db.execute('SELECT n,(SELECT n) AS value FROM (SELECT n FROM docs) a RIGHT JOIN (SELECT 1 AS n UNION ALL SELECT 2) b USING(n) ORDER BY n');
      assert.deepEqual(correlatedUsing.rows, [[1n,1n],[2n,2n]]);
      const nestedUsing = await db.execute('SELECT n,(SELECT (SELECT n)) AS value FROM (SELECT n FROM docs) a RIGHT JOIN (SELECT 1 AS n UNION ALL SELECT 2) b USING(n) ORDER BY n');
      assert.deepEqual(nestedUsing.rows, correlatedUsing.rows);
      await db.execute('CREATE TABLE nums(value INTEGER)');
      await db.execute('INSERT INTO nums VALUES(0),(1)');
      for (const [join, constraint] of [['RIGHT JOIN', ' USING(n)'], ['NATURAL RIGHT JOIN', '']]) {
        const tableScalar = await db.execute(`SELECT n,(SELECT (SELECT n FROM nums WHERE value=1)) AS value FROM (SELECT n FROM docs) a ${join} (SELECT 1 AS n UNION ALL SELECT 2) b${constraint} ORDER BY n`);
        assert.deepEqual(tableScalar.columns, ['n', 'value']);
        assert.deepEqual(tableScalar.rows, correlatedUsing.rows);
      }
      for (const field of ['flag', 'data']) {
        const typedScalar = await db.execute(`SELECT k,(SELECT (SELECT k FROM nums ORDER BY value DESC LIMIT 1)) AS value FROM (SELECT ${field} AS k FROM docs) a JOIN (SELECT ${field} AS k FROM docs) b USING(k)`);
        const expected = field === 'flag' ? true : Buffer.from([49]);
        assert.deepEqual(typedScalar.rows, [[expected, expected]]);
      }
    } finally { await db.close(); }
  }
});

test('nested USING runtime errors preserve prior work in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of ['CREATE TABLE docs', 'INSERT INTO docs {k:1,v:[]}', 'INSERT INTO docs {k:2,v:7}', 'CREATE TABLE b(k INTEGER)', 'INSERT INTO b VALUES(1),(2)', 'CREATE TABLE copied', 'INSERT INTO copied {k:99,v:[]}']) await db.execute(sql);
      const insert = 'INSERT INTO copied(k,v) SELECT k,(SELECT (SELECT array::append(a.v,2))) FROM docs a JOIN b USING(k) ORDER BY k';
      await db.execute('BEGIN');
      await db.execute('INSERT INTO copied {k:98,v:[]}');
      await assert.rejects(async () => db.execute(insert), error => {
        assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
        return true;
      });
      assert.deepEqual((await db.execute('SELECT k FROM copied ORDER BY k')).rows, [[98n],[99n]]);
      await db.execute('ROLLBACK');
      assert.deepEqual((await db.execute('SELECT k FROM copied ORDER BY k')).rows, [[99n]]);
      await db.execute('BEGIN');
      await db.execute('UPDATE docs SET v=array::new() WHERE k=2');
      await db.execute(insert);
      assert.deepEqual((await db.execute('SELECT k,v FROM copied WHERE k<>99 ORDER BY k')).rows, [[1n,[2n]],[2n,[2n]]]);
      await db.execute('ROLLBACK');
      assert.deepEqual((await db.execute('SELECT k FROM copied ORDER BY k')).rows, [[99n]]);
    } finally { await db.close(); }
  }
});

test('nested membership CTE writes preserve recovery in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
      await db.execute('INSERT INTO docs(n) VALUES(1),(2),(3)');
      for (const prefix of [
        'WITH docs AS (SELECT 2 AS n), chosen AS (SELECT n FROM (SELECT n FROM docs) q WHERE n IN (SELECT x.n FROM docs x)) ',
        'WITH docs AS (SELECT 2 AS n), chosen AS (WITH local_q AS MATERIALIZED (SELECT n FROM docs) SELECT n FROM local_q) ',
        'WITH docs AS (SELECT 2 AS n), chosen AS (WITH docs AS (SELECT n FROM main.docs), local_q AS NOT MATERIALIZED (SELECT n FROM docs) SELECT n FROM local_q) ',
      ]) {
        for (let attempt = 0; attempt < 2; attempt++) {
          await db.execute('BEGIN');
          await db.execute('CREATE TABLE pending(n INTEGER)');
          await db.execute('INSERT INTO pending VALUES(9)');
          await assert.rejects(async () => db.execute(prefix + 'UPDATE docs SET n=1 WHERE n IN (SELECT n FROM chosen) RETURNING n'), error => error.code === 'FDB_CONSTRAINT' && error.transaction.after === 'active');
          assert.deepEqual(await db.all('SELECT n FROM docs ORDER BY n'), [[1n],[2n],[3n]]);
          assert.deepEqual(await db.all('SELECT n FROM pending'), [[9n]]);
          const result = await db.execute(prefix + 'UPDATE docs SET n=n+10 WHERE n IN (SELECT n FROM chosen) RETURNING n');
          const expected = prefix.includes('main.docs') ? [[11n],[12n],[13n]] : [[12n]];
          assert.equal(result.affected, BigInt(expected.length));
          assert.deepEqual(result.rows, expected);
          const audit = await db.checkCollectionIntegrity('docs');
          assert.equal(audit.documents, 3n);
          assert.equal(audit.indexEntries, 3n);
          await db.execute('ROLLBACK');
          assert.deepEqual(await db.all('SELECT n FROM docs ORDER BY n'), [[1n],[2n],[3n]]);
        }
      }
    } finally { await db.close(); }
  }
});

test('unsupported correlated CTE writes report query errors in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('INSERT INTO docs(n) VALUES(1),(2)');
      await db.execute('BEGIN');
      await db.execute('INSERT INTO docs(n) VALUES(9)');
      const sql = 'WITH chosen AS(WITH local_q AS(SELECT d.n AS n) SELECT n FROM local_q) UPDATE docs AS d SET n=n+10 WHERE EXISTS(SELECT 1 FROM chosen) RETURNING n';
      await assert.rejects(async () => db.execute(sql), error => {
        assert.equal(error.code, 'FDB_UNSUPPORTED');
        assert.deepEqual(error.transaction, {before:'active', after:'active'});
        return true;
      });
      assert.deepEqual(await db.all('SELECT n FROM docs ORDER BY n'), [[1n],[2n],[9n]]);
      assert.equal((await db.execute('UPDATE docs SET n=n+10 WHERE n=1')).affected, 1n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT n FROM docs ORDER BY n'), [[1n],[2n]]);
    } finally { await db.close(); }
  }
});

test('colliding scalar aliases preserve client values and atomic insert recovery', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of [
        'CREATE TABLE docs', 'INSERT INTO docs(k) VALUES(1),(2)',
        'CREATE TABLE b(k INTEGER)', 'INSERT INTO b VALUES(1),(3)',
        'CREATE TABLE nums(n INTEGER)', 'INSERT INTO nums VALUES(0),(1),(2)',
        'CREATE TABLE sink', 'CREATE UNIQUE INDEX sink_v ON sink(v)',
        'INSERT INTO sink(v) VALUES(2)',
      ]) await db.execute(sql);
      const params = { $delta: 0n, $flag: true, $data: Buffer.from([0, 255]) };
      for (const alias of ['a', 'b']) {
        for (const join of ['LEFT JOIN', 'RIGHT JOIN']) {
          const scalar = `(SELECT max(${alias}.n)+$delta FROM nums ${alias} WHERE ${alias}.n<k)`;
          const result = await db.execute(`SELECT k,${scalar} AS v,$flag AS flag,$data AS data FROM docs a ${join} b USING(k) ORDER BY k`, params);
          const keys = join === 'LEFT JOIN' ? [[1n, 0n], [2n, 1n]] : [[1n, 0n], [3n, 2n]];
          assert.deepEqual(result.columns, ['k', 'v', 'flag', 'data']);
          assert.deepEqual(result.rows, keys.map(row => [...row, true, params.$data]));
        }
      }
      await db.execute('BEGIN');
      await db.execute('INSERT INTO sink(v) VALUES(9)');
      const insert = 'INSERT INTO sink(v) SELECT (SELECT max(b.n)+$delta FROM nums b WHERE b.n<k) FROM docs a RIGHT JOIN b USING(k) ORDER BY k RETURNING v';
      await assert.rejects(async () => db.execute(insert, { $delta: 0n }), error => {
        assert.equal(error.code, 'FDB_CONSTRAINT');
        assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
        return true;
      });
      assert.deepEqual(await db.all('SELECT v FROM sink ORDER BY v'), [[2n], [9n]]);
      await db.execute('DELETE FROM sink WHERE v=2');
      const retry = await db.execute(insert, { $delta: 0n });
      assert.equal(retry.affected, 2n);
      assert.deepEqual(retry.rows, [[0n], [2n]]);
      const audit = await db.checkCollectionIntegrity('sink');
      assert.equal(audit.documents, 3n);
      assert.equal(audit.indexEntries, 3n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT v FROM sink'), [[2n]]);
    } finally { await db.close(); }
  }
});

test('deeper EXISTS merged keys preserve parameters and write recovery in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of [
        'CREATE TABLE docs', 'INSERT INTO docs(k) VALUES(1),(2)',
        'CREATE TABLE native(k INTEGER)', 'INSERT INTO native VALUES(1),(2)',
        'CREATE TABLE b(k INTEGER)', 'INSERT INTO b VALUES(1),(3)',
        'CREATE TABLE nums(n INTEGER)', 'INSERT INTO nums VALUES(0),(1),(2)',
        'CREATE TABLE sink', 'CREATE UNIQUE INDEX sink_k ON sink(k)',
        'INSERT INTO sink(k) VALUES(3)',
      ]) await db.execute(sql);
      const scalar = reference => `(SELECT max(b.n) FROM nums b WHERE b.n<k AND EXISTS(SELECT 1 FROM (SELECT 0 AS k) q WHERE EXISTS(SELECT 1 WHERE ${reference}>$min)))`;
      for (const reference of ['k', 'q.k']) {
        for (const min of [0n, 1n, 3n]) {
          const query = source => `SELECT k,${scalar(reference)} AS v FROM ${source} a RIGHT JOIN b USING(k) ORDER BY k`;
          const expected = await db.execute(query('native'), { $min: min });
          const actual = await db.execute(query('docs'), { $min: min });
          assert.deepEqual(actual.columns, expected.columns);
          assert.deepEqual(actual.rows, expected.rows);
        }
      }
      await db.execute('BEGIN');
      await db.execute('INSERT INTO sink(k) VALUES(9)');
      const insert = `INSERT INTO sink(k,v) SELECT k,${scalar('k')} FROM docs a RIGHT JOIN b USING(k) ORDER BY k RETURNING k,v`;
      for (const [params, code] of [[{}, 'FDB_PARAMETER'], [{ $min: 1n }, 'FDB_CONSTRAINT']]) {
        await assert.rejects(async () => db.execute(insert, params), error => {
          assert.equal(error.code, code);
          assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
          return true;
        });
        assert.deepEqual(await db.all('SELECT k FROM sink ORDER BY k'), [[3n], [9n]]);
      }
      await db.execute('DELETE FROM sink WHERE k=3');
      const retry = await db.execute(insert, { $min: 1n });
      assert.equal(retry.affected, 2n);
      assert.deepEqual(retry.rows, [[1n, null], [3n, null]]);
      const audit = await db.checkCollectionIntegrity('sink');
      assert.equal(audit.documents, 3n);
      assert.equal(audit.indexEntries, 3n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT k FROM sink'), [[3n]]);
    } finally { await db.close(); }
  }
});

test('deeper scalar collation and casts preserve client results and atomic writes', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of [
        'CREATE TABLE docs', "INSERT INTO docs(k) VALUES(1),(2),('a')",
        'CREATE TABLE native(k INTEGER)', "INSERT INTO native VALUES(1),(2),('a')",
        'CREATE TABLE b(k INTEGER)', "INSERT INTO b VALUES(1),(3),('a')",
        'CREATE TABLE nums(n INTEGER)', 'INSERT INTO nums VALUES(0),(1),(2)',
        'CREATE TABLE labels(v TEXT COLLATE NOCASE)', "INSERT INTO labels VALUES('A')",
        'CREATE TABLE sink', 'CREATE UNIQUE INDEX sink_k ON sink(k)',
        "INSERT INTO sink(k) VALUES('a')",
      ]) await db.execute(sql);
      for (const predicate of [
        '(SELECT CAST(k AS TEXT))=$value',
        '(SELECT v FROM labels LIMIT 1)=k',
        '(SELECT $value COLLATE NOCASE)=k',
      ]) {
        const params = predicate.includes('$value') ? { $value: predicate.includes('CAST') ? 3n : 'A' } : {};
        const query = source => `SELECT k,(SELECT max(x.n) FROM nums x WHERE x.n<k AND ${predicate}) AS v FROM ${source} d RIGHT JOIN b USING(k) ORDER BY k`;
        const expected = await db.execute(query('native'), params);
        const actual = await db.execute(query('docs'), params);
        assert.deepEqual(actual.columns, expected.columns);
        assert.deepEqual(actual.rows, expected.rows);
      }
      await db.execute('BEGIN');
      await db.execute('INSERT INTO sink(k) VALUES(9)');
      const insert = 'INSERT INTO sink(k,v) SELECT k,(SELECT max(x.n) FROM nums x WHERE x.n<k AND (SELECT v FROM labels LIMIT 1)=k) FROM docs d RIGHT JOIN b USING(k) ORDER BY k RETURNING k,v';
      await assert.rejects(async () => db.execute(insert), error => {
        assert.equal(error.code, 'FDB_CONSTRAINT');
        assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
        return true;
      });
      assert.deepEqual(await db.all('SELECT k FROM sink ORDER BY k'), [[9n], ['a']]);
      await db.execute("DELETE FROM sink WHERE k='a'");
      const retry = await db.execute(insert);
      assert.equal(retry.affected, 3n);
      assert.deepEqual(retry.rows, [[1n, null], [3n, null], ['a', null]]);
      const audit = await db.checkCollectionIntegrity('sink');
      assert.equal(audit.documents, 4n);
      assert.equal(audit.indexEntries, 4n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT k FROM sink'), [['a']]);
    } finally { await db.close(); }
  }
});

test('deeper membership preserves nulls and atomic writes in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of [
        'CREATE TABLE docs', 'INSERT INTO docs(k) VALUES(1),(2)',
        'CREATE TABLE native(k INTEGER)', 'INSERT INTO native VALUES(1),(2)',
        'CREATE TABLE b(k INTEGER)', 'INSERT INTO b VALUES(1),(3)',
        'CREATE TABLE nums(n INTEGER)', 'INSERT INTO nums VALUES(0),(1),(2),(NULL)',
        'CREATE TABLE sink', 'CREATE UNIQUE INDEX sink_k ON sink(k)',
        'INSERT INTO sink(k) VALUES(3)',
      ]) await db.execute(sql);
      const select = (source, rhs) => `SELECT k,(SELECT sum(CASE WHEN x.n IN(${rhs}) THEN 1 WHEN x.n NOT IN(${rhs}) THEN 10 ELSE 100 END) FROM nums x WHERE k IS k) AS v FROM ${source} d RIGHT JOIN b USING(k) ORDER BY k`;
      for (const rhs of ['SELECT k', 'SELECT k WHERE 0', 'SELECT $value', 'SELECT x.n']) {
        for (const value of rhs.includes('$value') ? [null, 1n] : [undefined]) {
          const params = value === undefined ? {} : { $value: value };
          const expected = await db.execute(select('native', rhs), params);
          const actual = await db.execute(select('docs', rhs), params);
          assert.deepEqual(actual.columns, expected.columns);
          assert.deepEqual(actual.rows, expected.rows);
        }
      }
      await db.execute('BEGIN');
      await db.execute('INSERT INTO sink(k) VALUES(9)');
      const insert = `INSERT INTO sink(k,v) ${select('docs', 'SELECT k')} RETURNING k,v`;
      await assert.rejects(async () => db.execute(insert), error => {
        assert.equal(error.code, 'FDB_CONSTRAINT');
        assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
        return true;
      });
      assert.deepEqual(await db.all('SELECT k FROM sink ORDER BY k'), [[3n], [9n]]);
      await db.execute('DELETE FROM sink WHERE k=3');
      const retry = await db.execute(insert);
      assert.equal(retry.affected, 2n);
      assert.deepEqual(retry.rows, [[1n, 121n], [3n, 130n]]);
      const audit = await db.checkCollectionIntegrity('sink');
      assert.equal(audit.documents, 3n);
      assert.equal(audit.indexEntries, 3n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT k FROM sink'), [[3n]]);
    } finally { await db.close(); }
  }
});

test('correlated compound membership preserves sets and write recovery in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of [
        'CREATE TABLE docs', 'INSERT INTO docs(k) VALUES(1),(2)',
        'CREATE TABLE native(k INTEGER)', 'INSERT INTO native VALUES(1),(2)',
        'CREATE TABLE b(k INTEGER)', 'INSERT INTO b VALUES(1),(3)',
        'CREATE TABLE nums(n INTEGER)', 'INSERT INTO nums VALUES(0),(1),(2),(NULL)',
        'CREATE TABLE sink', 'CREATE UNIQUE INDEX sink_k ON sink(k)',
        'INSERT INTO sink(k) VALUES(2)',
      ]) await db.execute(sql);
      const query = (source, operator) => 'SELECT k,(SELECT sum(CASE WHEN x.n IN(SELECT k ' + operator + ' SELECT CAST(k AS REAL)) THEN 1 WHEN x.n NOT IN(SELECT k ' + operator + ' SELECT CAST(k AS REAL)) THEN 10 ELSE 100 END) FROM nums x WHERE k IS k) AS v FROM ' + source + ' d LEFT JOIN b USING(k) ORDER BY k';
      for (const operator of ['UNION ALL', 'UNION', 'INTERSECT', 'EXCEPT']) {
        const expected = await db.execute(query('native', operator));
        const actual = await db.execute(query('docs', operator));
        assert.deepEqual(actual.columns, expected.columns);
        assert.deepEqual(actual.rows, expected.rows);
      }
      await db.execute('BEGIN');
      await db.execute('INSERT INTO sink(k) VALUES(9)');
      const insert = 'INSERT INTO sink(k,v) ' + query('docs', 'INTERSECT') + ' RETURNING k,v';
      await assert.rejects(async () => db.execute(insert), error => {
        assert.equal(error.code, 'FDB_CONSTRAINT');
        assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
        return true;
      });
      assert.deepEqual(await db.all('SELECT k FROM sink ORDER BY k'), [[2n], [9n]]);
      await db.execute('DELETE FROM sink WHERE k=2');
      const retry = await db.execute(insert);
      assert.equal(retry.affected, 2n);
      assert.deepEqual(retry.rows, [[1n, 121n], [2n, 121n]]);
      const audit = await db.checkCollectionIntegrity('sink');
      assert.equal(audit.documents, 3n);
      assert.equal(audit.indexEntries, 3n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT k FROM sink'), [[2n]]);
    } finally { await db.close(); }
  }
});

test('ordered compound pagination binds parameters and preserves write recovery in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of [
        'CREATE TABLE docs', 'INSERT INTO docs(k) VALUES(1),(2)',
        'CREATE TABLE native(k INTEGER)', 'INSERT INTO native VALUES(1),(2)',
        'CREATE TABLE b(k INTEGER)', 'INSERT INTO b VALUES(1),(3)',
        'CREATE TABLE nums(n INTEGER)', 'INSERT INTO nums VALUES(0),(1),(2),(NULL)',
        'CREATE TABLE sink', 'CREATE UNIQUE INDEX sink_k ON sink(k)',
        'INSERT INTO sink(k) VALUES(2)',
      ]) await db.execute(sql);
      const query = (source, operator, order) => {
        const projection = order === 'k DESC' ? 'k' : 'k AS chosen';
        const rhs = `SELECT ${projection} ${operator} SELECT NULL ORDER BY ${order} LIMIT $take OFFSET $skip`;
        return `SELECT k,(SELECT sum(CASE WHEN x.n IN(${rhs}) THEN 1 WHEN x.n NOT IN(${rhs}) THEN 10 ELSE 100 END) FROM nums x WHERE k IS k) AS v FROM ${source} d LEFT JOIN b USING(k) ORDER BY k`;
      };
      for (const operator of ['UNION ALL', 'UNION', 'INTERSECT', 'EXCEPT']) {
        for (const order of ['1', 'chosen DESC', 'k DESC']) {
          for (const params of [{ $take: 1n, $skip: 0n }, { $take: 1n, $skip: 1n }, { $take: 0n, $skip: 0n }]) {
            const expected = await db.execute(query('native', operator, order), params);
            const actual = await db.execute(query('docs', operator, order), params);
            assert.deepEqual(actual.columns, expected.columns);
            assert.deepEqual(actual.rows, expected.rows);
          }
        }
      }
      await db.execute('BEGIN');
      await db.execute('INSERT INTO sink(k) VALUES(9)');
      const insert = `INSERT INTO sink(k,v) ${query('docs', 'UNION', 'chosen DESC')} RETURNING k,v`;
      await assert.rejects(async () => db.execute(insert, { $take: 1n }), error => {
        assert.equal(error.code, 'FDB_PARAMETER');
        assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
        return true;
      });
      const params = { $take: 1n, $skip: 0n };
      const expectedRows = await db.all(query('native', 'UNION', 'chosen DESC'), params);
      await assert.rejects(async () => db.execute(insert, params), error => {
        assert.equal(error.code, 'FDB_CONSTRAINT');
        assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
        return true;
      });
      assert.deepEqual(await db.all('SELECT k FROM sink ORDER BY k'), [[2n], [9n]]);
      await db.execute('DELETE FROM sink WHERE k=2');
      const retry = await db.execute(insert, params);
      assert.equal(retry.affected, 2n);
      assert.deepEqual(retry.rows, expectedRows);
      const audit = await db.checkCollectionIntegrity('sink');
      assert.equal(audit.documents, 3n);
      assert.equal(audit.indexEntries, 3n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT k FROM sink'), [[2n]]);
    } finally { await db.close(); }
  }
});

test('compound expression ordering preserves RIGHT JOIN client recovery', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of [
        'CREATE TABLE docs', 'INSERT INTO docs(k) VALUES(1),(2)',
        'CREATE TABLE native(k INTEGER)', 'INSERT INTO native VALUES(1),(2)',
        'CREATE TABLE b(k INTEGER)', 'INSERT INTO b VALUES(1),(3)',
        'CREATE TABLE nums(n INTEGER)', 'INSERT INTO nums VALUES(0),(1),(2),(NULL)',
        'CREATE TABLE sink', 'CREATE UNIQUE INDEX sink_k ON sink(k)',
        'INSERT INTO sink(k) VALUES(3)',
      ]) await db.execute(sql);
      const query = (source, operator, order) => {
        const rhs = `SELECT k+1 ${operator} SELECT NULL ORDER BY ${order} LIMIT $take OFFSET $skip`;
        return `SELECT k,(SELECT sum(CASE WHEN x.n IN(${rhs}) THEN 1 WHEN x.n NOT IN(${rhs}) THEN 10 ELSE 100 END) FROM nums x WHERE k IS k) AS v FROM ${source} d RIGHT JOIN b USING(k) ORDER BY k`;
      };
      for (const operator of ['UNION ALL', 'UNION', 'INTERSECT', 'EXCEPT']) {
        for (const order of ['"k+1"', '"k+1" DESC']) {
          const params = { $take: 1n, $skip: 0n };
          const expected = await db.execute(query('native', operator, order), params);
          const actual = await db.execute(query('docs', operator, order), params);
          assert.deepEqual(actual.columns, expected.columns);
          assert.deepEqual(actual.rows, expected.rows);
        }
      }
      await db.execute('BEGIN');
      await db.execute('INSERT INTO sink(k) VALUES(9)');
      const insert = `INSERT INTO sink(k,v) ${query('docs', 'UNION', '"k+1" DESC')} RETURNING k,v`;
      await assert.rejects(async () => db.execute(insert, { $take: 1n }), error => {
        assert.equal(error.code, 'FDB_PARAMETER');
        assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
        return true;
      });
      const params = { $take: 1n, $skip: 0n };
      const expectedRows = await db.all(query('native', 'UNION', '"k+1" DESC'), params);
      await assert.rejects(async () => db.execute(insert, params), error => {
        assert.equal(error.code, 'FDB_CONSTRAINT');
        assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
        return true;
      });
      assert.deepEqual(await db.all('SELECT k FROM sink ORDER BY k'), [[3n], [9n]]);
      await db.execute('DELETE FROM sink WHERE k=3');
      const retry = await db.execute(insert, params);
      assert.equal(retry.affected, 2n);
      assert.deepEqual(retry.rows, expectedRows);
      const audit = await db.checkCollectionIntegrity('sink');
      assert.equal(audit.documents, 3n);
      assert.equal(audit.indexEntries, 3n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT k FROM sink'), [[3n]]);
    } finally { await db.close(); }
  }
});

test('unordered UNION ALL pagination preserves parameters and client recovery', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of [
        'CREATE TABLE docs', 'INSERT INTO docs(k) VALUES(1),(2)',
        'CREATE TABLE native(k INTEGER)', 'INSERT INTO native VALUES(1),(2)',
        'CREATE TABLE b(k INTEGER)', 'INSERT INTO b VALUES(1),(3)',
        'CREATE TABLE nums(n INTEGER)', 'INSERT INTO nums VALUES(0),(1),(2),(NULL)',
        'CREATE TABLE sink', 'CREATE UNIQUE INDEX sink_k ON sink(k)',
        'INSERT INTO sink(k) VALUES(2)',
      ]) await db.execute(sql);
      const query = source => {
        const rhs = 'SELECT NULL UNION ALL SELECT k LIMIT $take OFFSET $skip';
        return `SELECT k,(SELECT sum(CASE WHEN x.n IN(${rhs}) THEN 1 WHEN x.n NOT IN(${rhs}) THEN 10 ELSE 100 END) FROM nums x WHERE k IS k) AS v FROM ${source} d LEFT JOIN b USING(k) ORDER BY k`;
      };
      for (const params of [
        {$take:1n,$skip:0n}, {$take:1n,$skip:1n},
        {$take:0n,$skip:0n}, {$take:-1n,$skip:1n},
      ]) {
        const expected = await db.execute(query('native'),params);
        const actual = await db.execute(query('docs'),params);
        assert.deepEqual(actual.columns,expected.columns);
        assert.deepEqual(actual.rows,expected.rows);
        assert.deepEqual((await db.profileSelect(query('docs'),params)).result.rows,expected.rows);
      }
      await db.execute('BEGIN');
      await db.execute('INSERT INTO sink(k) VALUES(9)');
      const insert = `INSERT INTO sink(k,v) ${query('docs')} RETURNING k,v`;
      await assert.rejects(async () => db.execute(insert, { $take: 1n }), error => {
        assert.equal(error.code, 'FDB_PARAMETER');
        assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
        return true;
      });
      const params = { $take: 1n, $skip: 1n };
      const expectedRows = await db.all(query('native'), params);
      await assert.rejects(async () => db.execute(insert, params), error => {
        assert.equal(error.code, 'FDB_CONSTRAINT');
        assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
        return true;
      });
      assert.deepEqual(await db.all('SELECT k FROM sink ORDER BY k'), [[2n], [9n]]);
      await db.execute('DELETE FROM sink WHERE k=2');
      const retry = await db.execute(insert, params);
      assert.equal(retry.affected, 2n);
      assert.deepEqual(retry.rows, expectedRows);
      const audit = await db.checkCollectionIntegrity('sink');
      assert.equal(audit.documents, 3n);
      assert.equal(audit.indexEntries, 3n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT k FROM sink'), [[2n]]);
    } finally { await db.close(); }
  }
});

test('unordered UNION pagination preserves parameters and client recovery', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of [
        'CREATE TABLE docs', 'INSERT INTO docs(k) VALUES(1),(2)',
        'CREATE TABLE native(k INTEGER)', 'INSERT INTO native VALUES(1),(2)',
        'CREATE TABLE b(k INTEGER)', 'INSERT INTO b VALUES(1),(3)',
        'CREATE TABLE nums(n INTEGER)', 'INSERT INTO nums VALUES(0),(1),(2),(NULL)',
        'CREATE TABLE sink', 'CREATE UNIQUE INDEX sink_k ON sink(k)',
        'INSERT INTO sink(k) VALUES(2)',
      ]) await db.execute(sql);
      const query = source => {
        const rhs = 'SELECT NULL UNION SELECT k LIMIT $take OFFSET $skip';
        return `SELECT k,(SELECT sum(CASE WHEN x.n IN(${rhs}) THEN 1 WHEN x.n NOT IN(${rhs}) THEN 10 ELSE 100 END) FROM nums x WHERE k IS k) AS v FROM ${source} d LEFT JOIN b USING(k) ORDER BY k`;
      };
      for (const params of [
        {$take:1n,$skip:0n}, {$take:1n,$skip:1n},
        {$take:0n,$skip:0n}, {$take:-1n,$skip:1n},
      ]) {
        const expected = await db.execute(query('native'),params);
        const actual = await db.execute(query('docs'),params);
        assert.deepEqual(actual.columns,expected.columns);
        assert.deepEqual(actual.rows,expected.rows);
        assert.deepEqual((await db.profileSelect(query('docs'),params)).result.rows,expected.rows);
      }
      await db.execute('BEGIN');
      await db.execute('INSERT INTO sink(k) VALUES(9)');
      const insert = `INSERT INTO sink(k,v) ${query('docs')} RETURNING k,v`;
      await assert.rejects(async () => db.execute(insert, { $take: 1n }), error => {
        assert.equal(error.code, 'FDB_PARAMETER');
        assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
        return true;
      });
      const params = { $take: 1n, $skip: 1n };
      const expectedRows = await db.all(query('native'), params);
      await assert.rejects(async () => db.execute(insert, params), error => {
        assert.equal(error.code, 'FDB_CONSTRAINT');
        assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
        return true;
      });
      assert.deepEqual(await db.all('SELECT k FROM sink ORDER BY k'), [[2n], [9n]]);
      await db.execute('DELETE FROM sink WHERE k=2');
      const retry = await db.execute(insert, params);
      assert.equal(retry.affected, 2n);
      assert.deepEqual(retry.rows, expectedRows);
      const audit = await db.checkCollectionIntegrity('sink');
      assert.equal(audit.documents, 3n);
      assert.equal(audit.indexEntries, 3n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT k FROM sink'), [[2n]]);
    } finally { await db.close(); }
  }
});

test('typed compound parameters preserve projections and write recovery in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of [
        'CREATE TABLE docs', 'CREATE TABLE probe(n INTEGER)', 'INSERT INTO probe VALUES(1)',
        'CREATE TABLE sink', 'CREATE UNIQUE INDEX sink_n ON sink(n)', 'INSERT INTO sink(n) VALUES(2)',
      ]) await db.execute(sql);
      const query = 'SELECT d.n,(SELECT $same FROM probe WHERE d.k IN(SELECT d.k INTERSECT SELECT $same LIMIT 1)) AS value FROM docs d ORDER BY d.n';
      for (const value of [true, 1n, new Record('docs',7n), Buffer.from('FDB\x01{"type":"Integer","value":7}')]) {
        await db.execute('INSERT INTO docs(n,k) VALUES(1,$same)', {$same:value});
        assert.deepEqual((await db.execute(query, {$same:value})).rows, [[1n,value]]);
        assert.deepEqual((await db.profileSelect(query, {$same:value})).result.rows, [[1n,value]]);
        await db.execute('DELETE FROM docs');
      }
      const value = new Record('docs',7n);
      await db.execute('INSERT INTO docs(n,k) VALUES(1,$same),(2,$same)', {$same:value});
      await db.execute('BEGIN');
      await db.execute('INSERT INTO sink(n) VALUES(9)');
      const insert = 'INSERT INTO sink(n,value) ' + query + ' RETURNING n,value';
      await assert.rejects(async () => db.execute(insert, {$same:value}), error => {
        assert.equal(error.code,'FDB_CONSTRAINT');
        assert.deepEqual(error.transaction,{before:'active',after:'active'});
        return true;
      });
      assert.deepEqual(await db.all('SELECT n FROM sink ORDER BY n'),[[2n],[9n]]);
      await db.execute('DELETE FROM sink WHERE n=2');
      const retry = await db.execute(insert, {$same:value});
      assert.equal(retry.affected,2n);
      assert.deepEqual(retry.rows,[[1n,value],[2n,value]]);
      const audit = await db.checkCollectionIntegrity('sink');
      assert.equal(audit.documents,3n);
      assert.equal(audit.indexEntries,3n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT n FROM sink'),[[2n]]);
    } finally { await db.close(); }
  }
});

test('collated compound membership preserves client write recovery', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of [
        'CREATE TABLE docs', 'CREATE TABLE baseline(k TEXT)',
        'CREATE TABLE probe(n INTEGER)', 'INSERT INTO probe VALUES(1)',
        "INSERT INTO docs(k) VALUES('A'),('a'),('a '),('B')",
        "INSERT INTO baseline VALUES('A'),('a'),('a '),('B')",
        'CREATE TABLE sink', 'CREATE UNIQUE INDEX sink_k ON sink(k)',
        "INSERT INTO sink(k) VALUES('B')",
      ]) await db.execute(sql);
      const query = (source, operator, collation) => `SELECT d.k,(SELECT count(*) FROM probe WHERE d.k COLLATE ${collation} IN(SELECT d.k COLLATE ${collation} ${operator} SELECT 'a' LIMIT $take OFFSET $skip)) AS found FROM ${source} d ORDER BY d.k`;
      for (const operator of ['UNION ALL','UNION','INTERSECT','EXCEPT']) {
        for (const collation of ['BINARY','NOCASE','RTRIM']) {
          for (const params of [{$take:1n,$skip:0n},{$take:1n,$skip:1n},{$take:0n,$skip:0n}]) {
            const expected = await db.execute(query('baseline',operator,collation),params);
            const actual = await db.execute(query('docs',operator,collation),params);
            assert.deepEqual(actual.columns,expected.columns);
            assert.deepEqual(actual.rows,expected.rows);
            assert.deepEqual((await db.profileSelect(query('docs',operator,collation),params)).result.rows,expected.rows);
          }
        }
      }
      await db.execute('BEGIN');
      await db.execute("INSERT INTO sink(k) VALUES('pending')");
      const select = query('docs','UNION ALL','NOCASE');
      const insert = 'INSERT INTO sink(k,found) ' + select + ' RETURNING k,found';
      const params = {$take:1n,$skip:0n};
      await assert.rejects(async () => db.execute(insert,{$take:1n}), error => {
        assert.equal(error.code,'FDB_PARAMETER');
        assert.deepEqual(error.transaction,{before:'active',after:'active'});
        return true;
      });
      await assert.rejects(async () => db.execute(insert,params), error => {
        assert.equal(error.code,'FDB_CONSTRAINT');
        assert.deepEqual(error.transaction,{before:'active',after:'active'});
        return true;
      });
      assert.deepEqual(await db.all('SELECT k FROM sink ORDER BY k'),[['B'],['pending']]);
      await db.execute("DELETE FROM sink WHERE k='B'");
      const retry = await db.execute(insert,params);
      assert.equal(retry.affected,4n);
      assert.deepEqual(retry.rows,await db.all(query('baseline','UNION ALL','NOCASE'),params));
      const audit = await db.checkCollectionIntegrity('sink');
      assert.equal(audit.documents,5n);
      assert.equal(audit.indexEntries,5n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT k FROM sink'),[['B']]);
    } finally { await db.close(); }
  }
});

test('collated scalar ranges and between preserve client write recovery', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of [
        'CREATE TABLE docs', 'CREATE TABLE baseline(n INTEGER,a,b,c)',
        "INSERT INTO docs(n,a,b,c) VALUES(1,'beta','ALPHA','GAMMA'),(2,'a ','a','a'),(3,NULL,'a','z')",
        "INSERT INTO baseline VALUES(1,'beta','ALPHA','GAMMA'),(2,'a ','a','a'),(3,NULL,'a','z')",
        'CREATE TABLE sink', 'CREATE UNIQUE INDEX sink_n ON sink(n)',
        'INSERT INTO sink(n) VALUES(2)',
      ]) await db.execute(sql);
      const query = predicate => `SELECT d.n,(SELECT ${predicate} UNION ALL SELECT NULL LIMIT 1) AS value FROM docs d`;
      for (const collation of ['BINARY','NOCASE','RTRIM']) {
        for (const predicate of [
          `d.a < d.b COLLATE ${collation}`,
          `d.a COLLATE ${collation} >= d.b`,
          `d.a COLLATE ${collation} BETWEEN d.b AND d.c`,
          `d.a NOT BETWEEN d.b COLLATE ${collation} AND d.c COLLATE BINARY`,
        ]) {
          const native = await db.execute(`SELECT d.n,${predicate} AS value FROM baseline d`);
          const actual = await db.execute(query(predicate));
          assert.deepEqual(actual.columns,native.columns);
          assert.deepEqual(actual.rows,native.rows);
          assert.deepEqual((await db.profileSelect(query(predicate))).result.rows,native.rows);
        }
      }
      await db.execute('BEGIN');
      await db.execute('INSERT INTO sink(n) VALUES(9)');
      const predicate = 'd.a COLLATE NOCASE BETWEEN d.b AND d.c';
      const insert = 'INSERT INTO sink(n,value) ' + query(predicate) + ' RETURNING n,value';
      await assert.rejects(async () => db.execute(insert), error => {
        assert.equal(error.code,'FDB_CONSTRAINT');
        assert.deepEqual(error.transaction,{before:'active',after:'active'});
        return true;
      });
      assert.deepEqual(await db.all('SELECT n FROM sink ORDER BY n'),[[2n],[9n]]);
      await db.execute('DELETE FROM sink WHERE n=2');
      const retry = await db.execute(insert);
      assert.equal(retry.affected,3n);
      assert.deepEqual(retry.rows,await db.all(`SELECT d.n,${predicate} FROM baseline d`));
      const audit = await db.checkCollectionIntegrity('sink');
      assert.equal(audit.documents,4n);
      assert.equal(audit.indexEntries,4n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT n FROM sink'),[[2n]]);
    } finally { await db.close(); }
  }
});

test('local CTE merged keys preserve parameters and client write recovery', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of [
        'CREATE TABLE docs','CREATE TABLE baseline(n INTEGER)','CREATE TABLE keys(n INTEGER)',
        'INSERT INTO docs(n) VALUES(1),(2),(3)','INSERT INTO baseline VALUES(1),(2),(3)',
        'INSERT INTO keys VALUES(1),(4)',
        'CREATE TABLE sink','CREATE UNIQUE INDEX sink_n ON sink(n)','INSERT INTO sink(n) VALUES(4)',
      ]) await db.execute(sql);
      const query = (source,join,materialization,shadow) => `SELECT n,(WITH chosen AS ${materialization} (SELECT n+$shift AS m${shadow ? ',100 AS n' : ''} FROM baseline) SELECT max(m) FROM chosen WHERE m<n) AS prior FROM ${source} a ${join} keys b USING(n) ORDER BY n`;
      const params = {$shift:0n};
      for (const join of ['JOIN','LEFT JOIN','RIGHT JOIN']) {
        for (const materialization of ['','MATERIALIZED','NOT MATERIALIZED']) {
          for (const shadow of [false,true]) {
            const expected = await db.execute(query('baseline',join,materialization,shadow),params);
            const sql = query('docs',join,materialization,shadow);
            const actual = await db.execute(sql,params);
            assert.deepEqual(actual.columns,expected.columns);
            assert.deepEqual(actual.rows,expected.rows);
            assert.deepEqual((await db.profileSelect(sql,params)).result.rows,expected.rows);
          }
        }
      }
      await db.execute('BEGIN');
      await db.execute('INSERT INTO sink(n) VALUES(9)');
      const select = query('docs','RIGHT JOIN','MATERIALIZED',false);
      const insert = 'INSERT INTO sink(n,prior) ' + select + ' RETURNING n,prior';
      await assert.rejects(async () => db.execute(insert), error => {
        assert.equal(error.code,'FDB_PARAMETER');
        assert.deepEqual(error.transaction,{before:'active',after:'active'});
        return true;
      });
      await assert.rejects(async () => db.execute(insert,params), error => {
        assert.equal(error.code,'FDB_CONSTRAINT');
        assert.deepEqual(error.transaction,{before:'active',after:'active'});
        return true;
      });
      assert.deepEqual(await db.all('SELECT n FROM sink ORDER BY n'),[[4n],[9n]]);
      await db.execute('DELETE FROM sink WHERE n=4');
      const retry = await db.execute(insert,params);
      assert.equal(retry.affected,2n);
      assert.deepEqual(retry.rows,[[1n,null],[4n,3n]]);
      const audit = await db.checkCollectionIntegrity('sink');
      assert.equal(audit.documents,3n);
      assert.equal(audit.indexEntries,3n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT n FROM sink'),[[4n]]);
    } finally { await db.close(); }
  }
});

test('ordered local CTE pagination preserves typed client recovery', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of [
        'CREATE TABLE docs','CREATE TABLE baseline(n INTEGER)','CREATE TABLE keys(n INTEGER)',
        'INSERT INTO docs(n) VALUES(1),(2),(3)','INSERT INTO baseline VALUES(1),(2),(3)',
        'INSERT INTO keys VALUES(1),(4)',
        'CREATE TABLE sink','CREATE UNIQUE INDEX sink_n ON sink(n)','INSERT INTO sink(n) VALUES(4)',
      ]) await db.execute(sql);
      const query = (materialization,direction) => `SELECT n,(WITH chosen AS ${materialization} (SELECT n AS m FROM baseline) SELECT $value FROM chosen WHERE m<n ORDER BY m ${direction} LIMIT $take OFFSET $skip) AS value FROM docs a RIGHT JOIN keys b USING(n) ORDER BY n`;
      for (const materialization of ['','MATERIALIZED','NOT MATERIALIZED']) {
        for (const direction of ['ASC','DESC']) {
          for (const [take,skip] of [[0n,0n],[1n,0n],[1n,1n],[1n,3n],[-1n,1n]]) {
            const params = {$take:take,$skip:skip,$value:true};
            const expected = [[1n,null],[4n,take === 0n || skip === 3n ? null : true]];
            const sql = query(materialization,direction);
            assert.deepEqual((await db.execute(sql,params)).rows,expected);
            assert.deepEqual((await db.profileSelect(sql,params)).result.rows,expected);
          }
        }
      }
      for (const [take,skip,kept] of [
        ['1','0',true],['1.0','1e0',true],[' +1 ',true,true],
        [true,false,true],[1,1,true],['.','0',false],['-1','3',false],
      ]) {
        const params = {$take:take,$skip:skip,$value:true};
        const expected = [[1n,null],[4n,kept ? true : null]];
        const sql = query('MATERIALIZED','DESC');
        assert.deepEqual((await db.execute(sql,params)).rows,expected);
        assert.deepEqual((await db.profileSelect(sql,params)).result.rows,expected);
      }
      await db.execute('BEGIN');
      await db.execute('INSERT INTO sink(n) VALUES(9)');
      const insert = 'INSERT INTO sink(n,value) ' + query('MATERIALIZED','DESC') + ' RETURNING n,value';
      const value = new Record('docs',7n);
      const params = {$take:'1.0',$skip:'1e0',$value:value};
      await assert.rejects(async () => db.execute(insert,{$take:1n,$value:value}), error => {
        assert.equal(error.code,'FDB_PARAMETER');
        assert.deepEqual(error.transaction,{before:'active',after:'active'});
        return true;
      });
      await assert.rejects(async () => db.execute(insert,{$take:'1',$value:value}), error => {
        assert.equal(error.code,'FDB_PARAMETER');
        assert.deepEqual(error.transaction,{before:'active',after:'active'});
        return true;
      });
      await assert.rejects(async () => db.execute(insert,params), error => {
        assert.equal(error.code,'FDB_CONSTRAINT');
        assert.deepEqual(error.transaction,{before:'active',after:'active'});
        return true;
      });
      assert.deepEqual(await db.all('SELECT n FROM sink ORDER BY n'),[[4n],[9n]]);
      await db.execute('DELETE FROM sink WHERE n=4');
      const retry = await db.execute(insert,params);
      assert.equal(retry.affected,2n);
      assert.deepEqual(retry.rows,[[1n,null],[4n,value]]);
      const audit = await db.checkCollectionIntegrity('sink');
      assert.equal(audit.documents,3n);
      assert.equal(audit.indexEntries,3n);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT n FROM sink'),[[4n]]);
    } finally { await db.close(); }
  }
});

test('bounded SELECT limits preserve typed results and transactions in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open()]) {
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('CREATE TABLE native(n INTEGER)');
      await db.execute('INSERT INTO native VALUES(1),(2)');
      const payload = { text: '猫', ref: new Record('docs', 7n), bytes: Buffer.from([0,255]), array: [true, null] };
      await db.execute('INSERT INTO docs {value:$value}', {$value: payload});
      const sql = 'SELECT value AS v FROM docs';
      // v=1; object keys=17; text=3; record=12; binary=2; array=2.
      const exact = {maxRows:1n, maxPayloadBytes:37n};
      const expected = await db.execute(sql);
      assert.deepEqual((await db.selectWithLimits(sql, exact)).rows, expected.rows);
      assert.deepEqual((await db.profileSelectWithLimits(sql, exact)).result.rows, expected.rows);
      await db.execute('BEGIN');
      await db.execute('INSERT INTO native VALUES(3)');
      for (const [query, limits] of [[sql, {...exact, maxPayloadBytes:36n}], [sql, {...exact,maxRows:0n}], ['SELECT n FROM native', {maxRows:2n,maxPayloadBytes:100n}], ['SELECT n FROM native WHERE 0',{maxRows:0n,maxPayloadBytes:0n}]]) {
        await assert.rejects(Promise.resolve().then(() => db.selectWithLimits(query, limits)), e => {
          assert.equal(e.code, 'FDB_LIMIT');
          assert.deepEqual(e.transaction, {before:'active',after:'active'});
          return true;
        });
      }
      assert.deepEqual((await db.selectWithLimits('SELECT n FROM native WHERE 0',{maxRows:0n,maxPayloadBytes:1n})).rows, []);
      await assert.rejects(Promise.resolve().then(() => db.selectWithLimits('DELETE FROM native', exact)), e => e.code === 'FDB_UNSUPPORTED');
      assert.equal((await db.selectWithLimits('SELECT n FROM native',{maxRows:3n,maxPayloadBytes:25n})).rows.length, 3);
      for (const limits of [undefined, null, {}, {maxRows:1,maxPayloadBytes:1n}, {maxRows:-1n,maxPayloadBytes:1n}, {maxRows:1n,maxPayloadBytes:2n**64n}, {...exact,typo:1n}]) {
        await assert.rejects(Promise.resolve().then(() => db.selectWithLimits(sql,limits)), e => e instanceof TypeError || e instanceof RangeError);
      }
      await db.execute('ROLLBACK');
      assert.equal((await db.all('SELECT n FROM native')).length, 2);
    } finally { await db.close(); }
    await assert.rejects(Promise.resolve().then(() => db.selectWithLimits('SELECT 1',{maxRows:1n,maxPayloadBytes:100n})), /clos/i);
  }
});

test('bounded worker SELECT cancellation cleans up and allows retry', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const db = await AsyncDatabase.open();
  const limits = {maxRows:1n,maxPayloadBytes:100n};
  try {
    const cancelled = new AbortController(); cancelled.abort();
    await assert.rejects(db.selectWithLimits('SELECT 1',limits,{}, {signal:cancelled.signal}), e => e.code === 'FDB_CANCELLED');
    await db.execute('CREATE TABLE nums(n INTEGER)');
    await db.execute('INSERT INTO nums VALUES ' + Array.from({length:1000}, (_,i) => '(' + (i+1) + ')').join(','));
    await db.execute('BEGIN');
    await db.execute('INSERT INTO nums VALUES(1001)');
    const active = new AbortController();
    const pending = db.profileSelectWithLimits('SELECT count(*) FROM nums a CROSS JOIN nums b CROSS JOIN nums c',limits,{}, {signal:active.signal});
    const timer = setTimeout(() => active.abort(), 25);
    try { await assert.rejects(pending, e => { assert.equal(e.code, 'FDB_CANCELLED'); assert.deepEqual(e.transaction, {before:'active',after:'active'}); return true; }); }
    finally { clearTimeout(timer); }
    assert.deepEqual((await db.selectWithLimits('SELECT count(*) AS n FROM nums',limits)).rows, [[1001n]]);
    await db.execute('ROLLBACK');
    assert.deepEqual((await db.selectWithLimits('SELECT count(*) AS n FROM nums',limits)).rows, [[1000n]]);
  } finally { await db.close(); }
});

test('bounded FETCH shares final payload limits across duplicates and scalar columns', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open()]) {
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('INSERT INTO docs {id:docs:a,n:7}');
      await db.execute('CREATE TABLE positions(n INTEGER)');
      await db.execute('INSERT INTO positions VALUES(1),(2)');
      await db.execute('BEGIN');
      await db.execute('INSERT INTO docs {id:docs:pending,n:9}');
      for (const [sql, bytes, rows] of [
        ['SELECT record::fetch(docs:a) AS v',17n,1n],
        ['SELECT record::fetch(docs:a) AS v FROM positions',33n,2n],
        ['SELECT record::fetch(docs:a) AS v,record::fetch(docs:a) AS w,1 AS n',43n,1n],
        ['SELECT record::fetch(docs:verylongmissingkey) AS v FROM positions',3n,2n],
        ['SELECT record::fetch(NULL) AS v',2n,1n],
      ]) {
        const limits = {maxRows:rows,maxPayloadBytes:bytes};
        const expected = await db.profileSelect(sql);
        const actual = await db.profileSelectWithLimits(sql,limits);
        assert.deepEqual(actual.result,expected.result);
        assert.equal(actual.metrics.fetchBatches,expected.metrics.fetchBatches);
        for (const rejected of [{...limits,maxPayloadBytes:bytes-1n},{...limits,maxRows:rows-1n}]) {
          await assert.rejects(Promise.resolve().then(() => db.selectWithLimits(sql,rejected)), e => {
            assert.equal(e.code,'FDB_LIMIT');
            assert.deepEqual(e.transaction,{before:'active',after:'active'});
            return true;
          });
        }
        assert.deepEqual((await db.selectWithLimits(sql,limits)).rows,expected.result.rows);
      }
      await db.execute('ROLLBACK');
      assert.equal((await db.all('SELECT n FROM docs')).length,1);
    } finally { await db.close(); }
  }
});

test('atomic write result policy restores rows indexes and trigger effects in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open()]) {
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
      await db.execute('CREATE TABLE native(n INTEGER UNIQUE)');
      await db.execute('CREATE TABLE audit(n INTEGER)');
      await db.execute('CREATE TRIGGER native_audit AFTER INSERT ON native BEGIN INSERT INTO audit VALUES(new.n); END');
      await db.execute('BEGIN');
      await db.execute('INSERT INTO docs {id:docs:prior,n:9}');
      await db.execute('INSERT INTO native VALUES(9)');
      for (const table of ['docs','native']) {
        const sql = 'INSERT INTO ' + table + '(n) VALUES(1),(2) RETURNING n';
        for (const limits of [{maxRows:1n,maxPayloadBytes:17n},{maxRows:2n,maxPayloadBytes:16n}]) {
          await assert.rejects(Promise.resolve().then(() => db.writeWithResultLimits(sql,limits)), e => {
            assert.equal(e.code,'FDB_LIMIT');
            assert.deepEqual(e.transaction,{before:'active',after:'active'});
            return true;
          });
          assert.deepEqual(await db.all('SELECT n FROM '+table),[[9n]]);
        }
        const result = await db.writeWithResultLimits(sql,{maxRows:2n,maxPayloadBytes:17n});
        assert.deepEqual(result.rows,[[1n],[2n]]);
        assert.equal(result.affected,2n);
        assert.deepEqual(result.transaction,{before:'active',after:'active'});
      }
      assert.deepEqual(await db.all('SELECT n FROM audit ORDER BY n'),[[1n],[2n],[9n]]);
      const mixed = 'INSERT INTO native(n) SELECT n+$offset FROM docs ORDER BY n RETURNING n';
      const mixedBudget = {maxRows:3n,maxPayloadBytes:25n};
      for (const rejected of [{...mixedBudget,maxPayloadBytes:0n},{...mixedBudget,maxPayloadBytes:24n},{...mixedBudget,maxRows:2n}]) {
        await assert.rejects(Promise.resolve().then(() => db.writeWithResultLimits(mixed,rejected,{$offset:20n})), error => {
          assert.equal(error.code,'FDB_LIMIT');
          assert.deepEqual(error.transaction,{before:'active',after:'active'});
          return true;
        });
        assert.deepEqual(await db.all('SELECT n FROM native ORDER BY n'),[[1n],[2n],[9n]]);
        assert.deepEqual(await db.all('SELECT n FROM audit ORDER BY n'),[[1n],[2n],[9n]]);
      }
      assert.deepEqual((await db.writeWithResultLimits(mixed,mixedBudget,{$offset:20n})).rows,[[21n],[22n],[29n]]);

      const value = {r:new Record('docs','prior'),b:Buffer.from([0,255]),t:'猫',a:[true,null]};
      const sql = 'UPDATE docs:prior {value:$v} RETURNING value AS v';
      const exact = {maxRows:1n,maxPayloadBytes:21n}; // v + keys 4 + record 9 + binary 2 + text 3 + array 2
      await assert.rejects(Promise.resolve().then(() => db.writeWithResultLimits(sql,{...exact,maxPayloadBytes:20n},{$v:value})), e => e.code === 'FDB_LIMIT');
      assert.deepEqual(await db.exactlyOne('SELECT value FROM docs WHERE n=9'),[null]);
      assert.deepEqual((await db.writeWithResultLimits(sql,exact,{$v:value})).rows,[[value]]);
      assert.equal((await db.checkCollectionIntegrity('docs')).documents,3n);
      for (const sql of ['COMMIT','ROLLBACK','CREATE TABLE nope(n)','SELECT 1']) {
        await assert.rejects(Promise.resolve().then(() => db.writeWithResultLimits(sql,exact)), e => e.code === 'FDB_UNSUPPORTED');
      }
      await assert.rejects(Promise.resolve().then(() => db.writeWithResultLimits('INSERT INTO native VALUES(5)',{maxRows:0,maxPayloadBytes:0n})), TypeError);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT n FROM native'),[]);
      assert.deepEqual(await db.all('SELECT n FROM audit'),[]);
      assert.equal((await db.checkCollectionIntegrity('docs')).documents,0n);
    } finally { await db.close(); }
    await assert.rejects(Promise.resolve().then(() => db.writeWithResultLimits('DELETE FROM docs',{maxRows:0n,maxPayloadBytes:0n})), /clos/i);
  }
});

test('worker write result cancellation rolls back and permits queued retry', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const db = await AsyncDatabase.open();
  const limits = {maxRows:1n,maxPayloadBytes:9n};
  try {
    await db.execute('CREATE TABLE nums(n INTEGER)');
    await db.execute('INSERT INTO nums VALUES '+Array.from({length:300},(_,n)=>'('+n+')').join(','));
    await db.execute('CREATE TABLE docs');
    await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
    await db.execute('BEGIN');
    await db.execute('INSERT INTO docs {id:docs:prior,n:9}');
    const before = new AbortController(); before.abort();
    await assert.rejects(db.writeWithResultLimits('DELETE FROM docs',limits,{}, {signal:before.signal}), e => e.code === 'FDB_CANCELLED');
    const active = new AbortController();
    const pending = db.writeWithResultLimits('INSERT INTO docs(n) SELECT count(*) FROM nums a CROSS JOIN nums b CROSS JOIN nums c RETURNING n',limits,{}, {signal:active.signal});
    const following = db.all('SELECT n FROM docs');
    const settled = Promise.allSettled([pending,following]);
    const timer = setTimeout(()=>active.abort(),20);
    const [cancelled,recovered] = await settled;
    clearTimeout(timer);
    assert.equal(cancelled.status,'rejected');
    assert.equal(cancelled.reason.code,'FDB_CANCELLED');
    assert.deepEqual(cancelled.reason.transaction,{before:'active',after:'active'});
    assert.equal(recovered.status,'fulfilled');
    assert.deepEqual(recovered.value,[[9n]]);
    assert.equal(require('node:events').getEventListeners(active.signal,'abort').length,0);
    assert.deepEqual((await db.writeWithResultLimits('INSERT INTO docs {n:1} RETURNING n',limits)).rows,[[1n]]);
    assert.equal((await db.checkCollectionIntegrity('docs')).documents,2n);
    await db.execute('ROLLBACK');
    assert.deepEqual(await db.all('SELECT n FROM docs'),[]);
  } finally { await db.close(); }
});

test('connection write buffer limits preserve pending work in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const options = { writeBufferLimits: { maxRows: 1n, maxPayloadBytes: 1000n } };
  for (const db of [new Database(':memory:', options), await AsyncDatabase.open(':memory:', options)]) {
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
      await db.execute('INSERT INTO docs {id:docs:a,n:1}');
      await db.execute('BEGIN');
      await db.execute('INSERT INTO docs {id:docs:b,n:2}');
      for (const sql of ['UPDATE docs SET n=n+10', 'UPDATE docs {n:n+10}', 'DELETE FROM docs', 'INSERT INTO docs(n) VALUES(3),(4)']) {
        await assert.rejects(async () => db.execute(sql), error => {
          assert.equal(error.code, 'FDB_LIMIT');
          assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
          return true;
        });
        assert.deepEqual(await db.all('SELECT n FROM docs ORDER BY n'), [[1n], [2n]]);
      }
      await db.execute('UPDATE docs SET n=3 WHERE n=2');
      assert.deepEqual(await db.all('SELECT n FROM docs ORDER BY n'), [[1n], [3n]]);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT n FROM docs'), [[1n]]);
      assert.equal((await db.checkCollectionIntegrity('docs')).documents, 1n);
    } finally { await db.close(); }
  }
});

test('joined update encoded identity limits preserve pending work in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const options = { writeBufferLimits: { maxRows: 10n, maxPayloadBytes: 300n } };
  const id = new Record('docs', '\0'.repeat(100));
  for (const db of [new Database(':memory:', options), await AsyncDatabase.open(':memory:', options)]) {
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
      await db.execute('INSERT INTO docs DOCUMENT $doc', {$doc: {id, n: 1n}});
      await db.execute('BEGIN');
      await db.execute('INSERT INTO docs {id:docs:pending,n:2}');
      await assert.rejects(async () => db.execute('UPDATE docs SET n=docs.n+10 FROM (SELECT 1 AS k) source WHERE docs.n=1'), error => {
        assert.equal(error.code, 'FDB_LIMIT');
        assert.match(error.message, /encoded value limit/);
        assert.deepEqual(error.transaction, {before: 'active', after: 'active'});
        return true;
      });
      assert.deepEqual(await db.all('SELECT id,n FROM docs ORDER BY n'), [[id, 1n], [new Record('docs', 'pending'), 2n]]);
      assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries, 2n);
      await db.execute('UPDATE docs SET n=11 WHERE n=1');
      assert.deepEqual(await db.all('SELECT n FROM docs ORDER BY n'), [[2n], [11n]]);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT id,n FROM docs'), [[id, 1n]]);
      assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries, 1n);
    } finally { await db.close(); }
  }
});

test('connection options reject invalid limits before opening a database', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'romedb-options-'));
  const file = path.join(dir, 'absent.db');
  try {
    for (const options of [null, [], { unknown: true }, { writeBufferLimits: {} },
      { writeBufferLimits: { maxRows: 1, maxPayloadBytes: 2n } },
      { writeBufferLimits: { maxRows: -1n, maxPayloadBytes: 2n } },
      { writeBufferLimits: { maxRows: 1n, maxPayloadBytes: 2n ** 64n } }]) {
      assert.throws(() => new Database(file, options), /options|option|limits/);
      await assert.rejects(AsyncDatabase.open(file, options), /options|option|limits/);
      assert.equal(fs.existsSync(file), false);
    }
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test('worker timeout uses native deadline and preserves queued request isolation', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const db = await AsyncDatabase.open();
  try {
    await db.execute('CREATE TABLE input(n)');
    await db.execute('INSERT INTO input VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9)');
    await db.execute('CREATE TABLE docs');
    await db.execute('BEGIN');
    await db.execute('INSERT INTO docs {n:1}');
    const sql = 'SELECT count(*) FROM input a,input b,input c,input d,input e,input f,input g,input h,input i,input j';
    const active = db.execute(sql, {}, { timeoutMs: 20 });
    const queued = db.execute('INSERT INTO docs {n:2}', {}, { timeoutMs: 0 });
    const next = db.all('SELECT n FROM docs');
    await Promise.all([active, queued].map(promise => assert.rejects(promise, error => {
      assert.equal(error.code, 'FDB_CANCELLED');
      assert.deepEqual(error.transaction, { before:'active', after:'active' });
      return true;
    })));
    assert.deepEqual(await next, [[1n]]);
    for (const timeoutMs of [-1, 0.5, NaN, Infinity, 4294967296, '1', 1n]) {
      await assert.rejects(db.execute('INSERT INTO docs {n:9}', {}, { timeoutMs }), /timeoutMs/);
    }
    const controller = new AbortController();
    controller.abort();
    await assert.rejects(db.execute('INSERT INTO docs {n:9}', {}, { signal:controller.signal, timeoutMs:60000 }), {code:'FDB_CANCELLED'});
    await db.execute('INSERT INTO docs {n:3}', {}, { timeoutMs:60000 });
    assert.deepEqual(await db.all('SELECT n FROM docs ORDER BY n'), [[1n],[3n]]);
    await db.execute('ROLLBACK');
    assert.deepEqual(await db.all('SELECT * FROM docs'), []);
  } finally { await db.close(); }
});

test('zero timeout rejects async operations before work and allows recovery', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const db = await AsyncDatabase.open();
  const limits = {maxRows:10n,maxPayloadBytes:1000n};
  const expired = {timeoutMs:0};
  const plan = [{version:1n,name:'initial',sql:'CREATE TABLE docs;'}];
  try {
    await db.migrate(plan);
    await db.execute('INSERT INTO docs {id:docs:a,n:1}');
    const payload = await db.exportDocuments('docs');
    for (const operation of [
      () => db.execute('DELETE FROM docs',{},expired),
      () => db.profileSelect('SELECT * FROM docs',{},expired),
      () => db.selectWithLimits('SELECT * FROM docs',limits,{},expired),
      () => db.profileSelectWithLimits('SELECT * FROM docs',limits,{},expired),
      () => db.writeWithResultLimits('DELETE FROM docs RETURNING n',limits,{},expired),
      () => db.checkCollectionIntegrity('docs',{},expired),
      () => db.executeBatch('DELETE FROM docs; SELECT 1;',expired),
      () => db.exportDocuments('docs','json',expired),
      () => db.importDocuments('docs',payload,'json',expired),
      () => db.migrate([...plan,{version:2n,name:'delete',sql:'DELETE FROM docs;'}],expired),
    ]) {
      await assert.rejects(operation(), error => {
        assert.equal(error.code,'FDB_CANCELLED');
        assert.deepEqual(error.transaction,{before:'autocommit',after:'autocommit'});
        return true;
      });
      assert.deepEqual(await db.all('SELECT n FROM docs'),[[1n]]);
    }
    assert.equal((await db.migrate(plan)).alreadyApplied,1);
    assert.equal((await db.checkCollectionIntegrity('docs')).documents,1n);
    await db.execute('UPDATE docs SET n=2',{}, {timeoutMs:60000});
    assert.deepEqual(await db.all('SELECT n FROM docs'),[[2n]]);
  } finally { await db.close(); }
});

test('timeout completion and invalid signal paths release native token capacity', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const { createCancellationToken, releaseCancellationToken } = require('./native.cjs');
  const db = await AsyncDatabase.open();
  const held = [];
  try {
    for (let i=0; i<256; i++) {
      await assert.rejects(db.execute('SELECT 1',{}, {timeoutMs:0}), {code:'FDB_CANCELLED'});
      await assert.rejects(db.execute('SELECT 1',{}, {timeoutMs:60000,signal:null}), TypeError);
      assert.deepEqual(await db.all('SELECT 1',{}, {timeoutMs:60000}), [[1n]]);
    }
    // The private native registry has a fixed 16,384-token cap. Filling all
    // slots proves these completed requests retained no registry entries.
    for (let i=0; i<16384; i++) held.push(createCancellationToken());
    assert.throws(() => createCancellationToken(), /token limit/);
    for (const key of held.splice(0)) releaseCancellationToken(key);
    assert.deepEqual(await db.all('SELECT 2',{}, {timeoutMs:60000}), [[2n]]);
  } finally {
    for (const key of held) releaseCancellationToken(key);
    await db.close();
  }
});

test('closing a worker with a timed write permits clean persistent reopen', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(),'romedb-timeout-close-'));
  const file = path.join(dir,'database.db');
  let db;
  try {
    db = await AsyncDatabase.open(file);
    await db.execute('CREATE TABLE input(n)');
    await db.execute('INSERT INTO input VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9)');
    await db.execute('CREATE TABLE docs');
    await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
    await db.execute('INSERT INTO docs {id:docs:a,n:1}');
    await db.execute('BEGIN');
    await db.execute('INSERT INTO docs {id:docs:b,n:2}');
    const operation = db.execute('INSERT INTO docs(n) SELECT count(*) FROM input a,input b,input c,input d,input e,input f,input g,input h,input i,input j', {}, {timeoutMs:20});
    const rejection = assert.rejects(operation, error => {
      assert.equal(error.code,'FDB_CANCELLED');
      assert.deepEqual(error.transaction,{before:'active',after:'active'});
      return true;
    });
    const closed = db.close();
    assert.equal(db.close(),closed);
    await assert.rejects(db.execute('SELECT 1'),{code:'FDB_CLOSED'});
    await Promise.all([rejection,closed]);
    db = undefined;
    const reopened = new Database(file);
    try {
      assert.deepEqual(reopened.all('SELECT n FROM docs'),[[1n]]);
      assert.equal(reopened.checkCollectionIntegrity('docs').documents,1n);
      reopened.execute('INSERT INTO docs {id:docs:b,n:2}');
      assert.deepEqual(reopened.all('SELECT n FROM docs ORDER BY n'),[[1n],[2n]]);
    } finally { reopened.close(); }
  } finally {
    if (db) await db.close();
    fs.rmSync(dir,{recursive:true,force:true});
  }
});

test('direct JSON response composition preserves escaped names and nested values', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const value = { '"}]\nไทย': ['\0\t\\"', new Record('docs','"}]\n'), Buffer.from([0,34,92,255]), -0] };
  for (const db of [new Database(), await AsyncDatabase.open()]) {
    try {
      const sql = 'SELECT $value AS "quoted""name"';
      const result = await db.execute(sql, {$value:value});
      assert.deepEqual(result.columns,['quoted"name']);
      assert.deepEqual(result.rows,[[value]]);
      assert.deepEqual((await db.profileSelect(sql, {$value:value})).result.rows,[[value]]);
      const batch = await db.executeBatch("SELECT '}]\\\"' AS text; SELECT 2 AS n;");
      assert.equal(batch.length,2);
      assert.deepEqual(batch[1].result.rows,[[2n]]);
    } finally { await db.close(); }
  }
});

test('array holes are rejected as invalid parameters in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open()]) {
    try {
      await assert.rejects(async () => db.execute('SELECT $v', { $v: [1, , 2] }), { name: 'TypeError', message: /array holes/ });
      await assert.rejects(async () => db.execute('SELECT $v', { $v: { nested: new Array(1) } }), { name: 'TypeError', message: /array holes/ });
      assert.deepEqual((await db.execute('SELECT $v', { $v: [1n, null] })).rows, [[[1n, null]]]);
    } finally { await db.close(); }
  }
});

test('malformed Unicode parameters are rejected as invalid input in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open()]) {
    try {
      for (const params of [{ $v: 'a\ud800' }, { $v: { ['\udc00']: 1 } }, { $v: new Record('docs', 'k\ud800') }, { ['$\ud800']: 1 }]) {
        await assert.rejects(async () => db.execute('SELECT $v', params), { name: 'TypeError', message: /well-formed/ });
      }
      assert.deepEqual((await db.execute('SELECT $v', { $v: '\ud83d\ude00' })).rows, [['\ud83d\ude00']]);
    } finally { await db.close(); }
  }
});

test('maximum logical depth survives wire framing, storage and transfer', async () => {
  const {AsyncDatabase} = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open()]) {
    try {
      let value = new Record('docs','leaf');
      for(let i=0;i<64;i++) value = i%2 ? {nested:value} : [value];
      assert.deepEqual((await db.execute('SELECT $v AS v',{$v:value})).rows,[[value]]);
      await assert.rejects(async()=>db.execute('SELECT $v',{$v:[value]}),/nesting exceeds 64/);
      const payload = value.nested;
      await db.execute('CREATE TABLE docs');
      await db.execute('INSERT INTO docs {id:docs:deep,payload:$v}',{$v:payload});
      assert.deepEqual((await db.execute('SELECT payload FROM docs')).rows,[[payload]]);
      for(const format of ['json','ndjson']) {
        const data = await db.exportDocuments('docs',format);
        await db.execute('BEGIN');
        await db.execute('DELETE FROM docs');
        await db.importDocuments('docs',data,format);
        assert.deepEqual((await db.execute('SELECT payload FROM docs')).rows,[[payload]]);
        await db.execute('ROLLBACK');
      }
    } finally { await db.close(); }
  }
});

test('migration failures expose lossless versions, UTF-8 offsets and typed causes', async () => {
  const {AsyncDatabase,isRomeDBError} = require('./index.cjs');
  for(const worker of [false,true]) for(const limited of [false,true]) {
    const options = limited ? {writeBufferLimits:{maxRows:1n,maxPayloadBytes:1000n}} : {};
    const db = worker ? await AsyncDatabase.open(':memory:',options) : new Database(':memory:',options);
    try {
      const base = {version:1n,name:'base',sql:'CREATE TABLE docs; CREATE UNIQUE INDEX docs_n ON docs(n); INSERT INTO docs {id:docs:saved,n:1};'};
      await db.migrate([base]);
      const prefix = "-- café 日本語\nINSERT INTO docs {id:docs:temp,n:2}; ";
      const bad = limited ? 'INSERT INTO docs (n) VALUES(3),(4);' : 'INSERT INTO docs {n:1};';
      const pending = {version:9007199254740993n,name:'pending',sql:prefix+bad};
      await assert.rejects(async()=>db.migrate([base,pending]),error=>{
        assert(isRomeDBError(error));
        assert.equal(error.code,'FDB_MIGRATION');
        assert.equal(error.migration.version,pending.version);
        assert.equal(error.migration.offset,BigInt(Buffer.byteLength(prefix)));
        assert.equal(error.migration.cause.code,limited?'FDB_LIMIT':'FDB_CONSTRAINT');
        assert.equal(typeof error.migration.cause.message,'string');
        assert.deepEqual(error.transaction,{before:'autocommit',after:'autocommit'});
        return true;
      });
      assert.deepEqual((await db.execute('SELECT n FROM docs')).rows,[[1n]]);
      assert.equal((await db.checkCollectionIntegrity('docs')).documents,1n);
      pending.sql=prefix;
      assert.deepEqual((await db.migrate([base,pending])).applied,[pending.version]);
      assert.equal((await db.migrate([base,pending])).alreadyApplied,2);
    } finally { await db.close(); }
  }
  for(const migration of [null,{}, {version:1,offset:0n,cause:{code:'FDB_LIMIT',message:'x'}},
    {version:1n,offset:-1n,cause:{code:'FDB_LIMIT',message:'x'}},
    {version:1n,offset:0n,cause:{code:'bad',message:'x'}}]) {
    assert.equal(isRomeDBError(Object.assign(new Error('test'),{code:'FDB_MIGRATION',migration})),false);
  }
});

test('direct JSON iterator joins preserve parameters and values in both clients', async () => {
  const {AsyncDatabase}=require('./index.cjs');
  for(const db of [new Database(),await AsyncDatabase.open()]) {
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('INSERT INTO docs {n:1}');
      const sql='SELECT d.n,j.key,j.value FROM docs d CROSS JOIN json_each($json) j ORDER BY j.key';
      const params={$json:'[1,2,null]'};
      const rows=[[1n,0n,1n],[1n,1n,2n],[1n,2n,null]];
      assert.deepEqual((await db.execute(sql,params)).rows,rows);
      assert.deepEqual((await db.profileSelect(sql,params)).result.rows,rows);
      await assert.rejects(async()=>db.execute(sql),error=>error.code==='FDB_PARAMETER');
      const computed=sql.replace('json_each($json)', "json_each(coalesce($json,'[]'))");
      assert.deepEqual((await db.execute(computed,params)).rows,rows);
      assert.deepEqual((await db.profileSelect(computed,params)).result.rows,rows);
      await assert.rejects(async()=>db.execute(computed),error=>error.code==='FDB_PARAMETER');
      assert.deepEqual((await db.execute('SELECT value FROM json_each(json_array(1,2))')).rows,[[1n],[2n]]);
      await db.execute('UPDATE docs SET j=$json',params);
      const correlated=sql.replace('json_each($json)','json_each(d.j)');
      assert.deepEqual((await db.execute(correlated)).rows,rows);
      assert.deepEqual((await db.profileSelect(correlated)).result.rows,rows);
      const unqualified=correlated.replace('json_each(d.j)',"json_each(coalesce(j,'[]'))");
      assert.deepEqual((await db.execute(unqualified)).rows,rows);
      assert.deepEqual((await db.profileSelect(unqualified)).result.rows,rows);
      await db.execute('UPDATE docs SET payload=$p',{$p:{inner:{j:params.$json}}});
      const deep=correlated.replace('json_each(d.j)', 'json_each(d.payload.inner.j)');
      assert.deepEqual((await db.execute(deep)).rows,rows);
      assert.deepEqual((await db.profileSelect(deep)).result.rows,rows);
      const targetRecord=new Record('docs',9223372036854775807n);
      for(const iterator of ['json_each','main.json_each','json_tree']) {
        for(const argument of ['d.j','d.payload.inner.j']) {
          await db.execute('BEGIN');
          const updated=await db.execute('UPDATE docs AS d SET result=array::new(x.value,$record) FROM '+iterator+'('+argument+") x WHERE x.type='integer' AND x.key=0 RETURNING n,result",{$record:targetRecord});
          assert.equal(updated.affected,1n);
          assert.deepEqual(updated.rows,[[1n,[1n,targetRecord]]]);
          assert.deepEqual(updated.transaction,{before:'active',after:'active'});
          await db.execute('ROLLBACK');
          assert.deepEqual((await db.execute('SELECT result FROM docs')).rows,[[null]]);
        }
      }
      const scalar=sql.replace('json_each($json)','json_each((SELECT $json))');
      assert.deepEqual((await db.execute(scalar,params)).rows,rows);
      assert.deepEqual((await db.profileSelect(scalar,params)).result.rows,rows);
      await assert.rejects(async()=>db.execute(scalar),error=>error.code==='FDB_PARAMETER');
      await db.execute('CREATE TABLE native(n INTEGER,j TEXT)');
      await db.execute('INSERT INTO native VALUES(1,$json)',params);
      const nested=sql.replace('json_each($json)','json_each((SELECT j FROM native WHERE n=d.n))');
      assert.deepEqual((await db.execute(nested)).rows,rows);
      assert.deepEqual((await db.profileSelect(nested)).result.rows,rows);
      const cte="SELECT d.n,x.value,y.value FROM docs d CROSS JOIN json_each('[1,2]') x CROSS JOIN json_each((WITH a AS (SELECT x.value+$delta AS n) SELECT json_array(n) FROM a)) y ORDER BY x.key";
      const cteRows=[[1n,1n,2n],[1n,2n,3n]];
      assert.deepEqual((await db.execute(cte,{$delta:1n})).rows,cteRows);
      assert.deepEqual((await db.profileSelect(cte,{$delta:1n})).result.rows,cteRows);
      await assert.rejects(async()=>db.execute(cte),error=>error.code==='FDB_PARAMETER');
      const predicates='SELECT x.value FROM docs d CROSS JOIN json_each(json_array(EXISTS(SELECT 1 FROM native s WHERE s.n=d.n),$key IN (SELECT n FROM native))) x ORDER BY x.key';
      assert.deepEqual((await db.execute(predicates,{$key:1n})).rows,[[1n],[1n]]);
      assert.deepEqual((await db.profileSelect(predicates,{$key:1n})).result.rows,[[1n],[1n]]);
      await assert.rejects(async()=>db.execute(predicates),error=>error.code==='FDB_PARAMETER');
      const inner='SELECT d.n,(SELECT count(*) FROM json_each(d.j) x) AS total FROM docs d WHERE EXISTS(SELECT 1 FROM json_each(d.j) x WHERE x.value=d.n)';
      assert.deepEqual((await db.execute(inner)).rows,[[1n,3n]]);
      assert.deepEqual((await db.profileSelect(inner)).result.rows,[[1n,3n]]);
      const qualifiedCte='SELECT d.n,(WITH a AS (SELECT x.value AS n FROM temp.json_each(d.j) x) SELECT sum(n)+$delta FROM a) AS total FROM docs d';
      assert.deepEqual((await db.execute(qualifiedCte,{$delta:1n})).rows,[[1n,4n]]);
      assert.deepEqual((await db.profileSelect(qualifiedCte,{$delta:1n})).result.rows,[[1n,4n]]);
      await assert.rejects(async()=>db.execute(qualifiedCte),error=>error.code==='FDB_PARAMETER');
      for(const [expression,total] of [
        ['(WITH a AS (SELECT d.n+x.value AS v FROM json_each(d.j) x) SELECT sum(v) FROM a)',5n],
        ['(WITH a AS (SELECT x.value AS v FROM json_each(d.j) x UNION ALL SELECT d.n) SELECT sum(v) FROM a)',4n]
      ]) {
        const projected='SELECT d.n,'+expression+' AS total FROM docs d';
        assert.deepEqual((await db.execute(projected)).rows,[[1n,total]]);
        assert.deepEqual((await db.profileSelect(projected)).result.rows,[[1n,total]]);
      }









    } finally {await db.close();}
  }
});

test('worker iterator deadlines preserve queued recovery and indexed work', async () => {
  const {AsyncDatabase}=require('./index.cjs');
  const db=await AsyncDatabase.open();
  try {
    for(const sql of ['CREATE TABLE docs','INSERT INTO docs(n) VALUES(1)',
      'CREATE TABLE output','CREATE UNIQUE INDEX output_n ON output(n)',
      'BEGIN','INSERT INTO output(n) VALUES(0)']) await db.execute(sql);
    const params={$json:JSON.stringify(Array(5000).fill(1))};
    const query='SELECT count(*) AS n FROM docs d CROSS JOIN json_each($json) x CROSS JOIN json_each($json) y WHERE d.n=1';
    const write='INSERT INTO output(n) '+query+' RETURNING n';
    for(const operation of [
      ()=>db.execute(query,params,{timeoutMs:20}),
      ()=>db.profileSelect(query,params,{timeoutMs:20}),
      ()=>db.execute(write,params,{timeoutMs:20})
    ]) {
      const pending=operation();
      const recovery=db.execute('SELECT n FROM output');
      await assert.rejects(pending,error=>{
        assert.equal(error.code,'FDB_CANCELLED');
        assert.deepEqual(error.transaction,{before:'active',after:'active'});
        return true;
      });
      assert.deepEqual((await recovery).rows,[[0n]]);
      assert.equal((await db.checkCollectionIntegrity('output')).indexEntries,1n);
    }
    assert.deepEqual((await db.execute(write,{$json:'[1,2]'})).rows,[[4n]]);
    await db.execute('ROLLBACK');
    assert.equal((await db.checkCollectionIntegrity('output')).documents,0n);
  } finally {await db.close();}
});

test('malformed iterator input reports whole transaction rollback in both clients', async () => {
  const {AsyncDatabase}=require('./index.cjs');
  for(const db of [new Database(),await AsyncDatabase.open()]) {
    try {
      for(const sql of ['CREATE TABLE docs',"INSERT INTO docs(n,j) VALUES(1,'[1]'),(2,'invalid')",
        'CREATE TABLE output','CREATE UNIQUE INDEX output_n ON output(n)','INSERT INTO output(n) VALUES(-1)']) await db.execute(sql);
      for(const iterator of ['json_each','json_tree']) {
        const query="SELECT x.value AS n FROM docs d CROSS JOIN "+iterator+"(d.j) x WHERE x.type='integer' ORDER BY d.n,x.id";
        const insert='INSERT INTO output(n) '+query+' RETURNING n';
        const update="UPDATE output SET n=x.value FROM docs d CROSS JOIN "+iterator+"(d.j) x WHERE x.type='integer' RETURNING n";
        for(const operation of [()=>db.execute(query),()=>db.profileSelect(query),()=>db.execute(insert),()=>db.execute(update)]) {
          await db.execute('BEGIN');
          await db.execute('INSERT INTO output(n) VALUES(0)');
          await assert.rejects(async()=>operation(),error=>{
            assert.equal(error.code,'FDB_ENGINE');
            assert.deepEqual(error.transaction,{before:'active',after:'autocommit'});
            return true;
          });
          assert.deepEqual((await db.execute('SELECT n FROM output')).rows,[[-1n]]);
          assert.equal((await db.checkCollectionIntegrity('output')).indexEntries,1n);
        }
        await db.execute("UPDATE docs SET j='[2]' WHERE n=2");
        await db.execute('BEGIN');
        const recovered=await db.execute("UPDATE output SET n=x.value FROM docs d CROSS JOIN "+iterator+"(d.j) x WHERE d.n=2 AND x.type='integer' RETURNING n");
        assert.equal(recovered.affected,1n);
        assert.deepEqual(recovered.rows,[[2n]]);
        assert.deepEqual(recovered.transaction,{before:'active',after:'active'});
        await db.execute('ROLLBACK');
        assert.deepEqual((await db.execute('SELECT n FROM output')).rows,[[-1n]]);
        assert.deepEqual((await db.execute(insert)).rows,[[1n],[2n]]);
        await db.execute('DELETE FROM output WHERE n>0');
        await db.execute("UPDATE docs SET j='invalid' WHERE n=2");
      }
    } finally {await db.close();}
  }
});

test('tuple updates preserve typed snapshots and atomic failure in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of ['CREATE TABLE docs', 'DEFINE FIELD n ON docs TYPE integer CHECK(n<10)',
        'CREATE UNIQUE INDEX docs_n ON docs(n)', 'INSERT INTO docs(n,a,b) VALUES(1,2,3),(3,4,5)',
        'BEGIN', 'INSERT INTO docs(n,a,b) VALUES(0,0,0)']) await db.execute(sql);
      const before = (await db.execute('SELECT n,a,b FROM docs ORDER BY n')).rows;
      await assert.rejects(async () => db.execute('UPDATE docs SET (n,a)=(n+8,a+1)'), error => {
        assert.equal(error.code, 'FDB_VALIDATION');
        assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
        return true;
      });
      assert.deepEqual((await db.execute('SELECT n,a,b FROM docs ORDER BY n')).rows, before);
      const record = new Record('docs', 'target');
      const object = { nested: [true, Buffer.from([0,255]), 9223372036854775807n] };
      await assert.rejects(async () => db.execute('UPDATE docs SET (a,b)=($a,$b),n=n+1 WHERE n=0 RETURNING a,b', { $a: record, $b: object }), error => {
        assert.equal(error.code, 'FDB_CONSTRAINT');
        assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
        return true;
      });
      assert.deepEqual((await db.execute('SELECT n,a,b FROM docs ORDER BY n')).rows, before);
      await assert.rejects(async () => db.execute('UPDATE OR ABORT docs SET (a,b)=($a,$b),n=1 WHERE n=0 RETURNING a,b', { $a: record, $b: object }), error => {
        assert.equal(error.code, 'FDB_CONSTRAINT');
        assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
        return true;
      });
      assert.deepEqual((await db.execute('SELECT n,a,b FROM docs ORDER BY n')).rows, before);
      assert.deepEqual((await db.execute('UPDATE OR ABORT docs SET (a,b)=($a,$b),n=7 WHERE n=0 RETURNING a,b', { $a: record, $b: object })).rows, [[record, object]]);

      assert.deepEqual((await db.execute('UPDATE docs SET (a,b)=(b,a) WHERE n=7 RETURNING a,b')).rows, [[object, record]]);
      await assert.rejects(async () => db.execute('UPDATE docs SET (n,a)=(SELECT n+8,a)'), error => {
        assert.equal(error.code, 'FDB_VALIDATION');
        assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
        return true;
      });
      assert.deepEqual((await db.execute('SELECT a,b FROM docs WHERE n=7')).rows, [[object, record]]);
      assert.deepEqual((await db.execute('UPDATE docs SET (a,b)=(SELECT $a,$b WHERE docs.n=7) RETURNING n,a,b', { $a: record, $b: object })).rows,
        [[1n,null,null],[3n,null,null],[7n,record,object]]);
      assert.deepEqual((await db.execute('UPDATE docs SET (a,b)=(SELECT b,a) WHERE n=7 RETURNING a,b')).rows, [[object,record]]);
      assert.deepEqual((await db.execute('UPDATE docs SET (a,b)=(SELECT b AS chosen,a AS chosen WHERE docs.n=7 ORDER BY 1) RETURNING n,a,b')).rows,
        [[1n,null,null],[3n,null,null],[7n,record,object]]);
      assert.deepEqual((await db.execute('UPDATE docs SET (a,b)=(SELECT -n AS n,b AS value WHERE docs.n>3 ORDER BY n) RETURNING n,a,b')).rows,
        [[1n,null,null],[3n,null,null],[7n,-7n,object]]);
      await db.execute('ROLLBACK');
      assert.deepEqual((await db.execute('SELECT n,a,b FROM docs ORDER BY n')).rows, [[1n,2n,3n],[3n,4n,5n]]);
    } finally { await db.close(); }
  }
});

test('tuple collection lookups preserve typed results in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('INSERT INTO docs(n) VALUES(1),(2)');
      await db.execute('CREATE TABLE lookup');
      const record = new Record('docs', 9223372036854775807n);
      const payload = { values: [true, Buffer.from([0,255]), -9223372036854775808n] };
      await db.execute('INSERT INTO lookup(n,a,b) VALUES(1,$a,$b)', { $a: record, $b: payload });
      await db.execute('UPDATE lookup SET rank=1');
      await db.execute('INSERT INTO lookup(n,a,b,rank) VALUES(1,$a,$b,2)', { $a: new Record('docs', 'second'), $b: { selected: true } });
      await db.execute('BEGIN');
      assert.deepEqual((await db.execute('UPDATE docs SET (a,b)=(SELECT x.a,x.b FROM lookup x WHERE x.n=docs.n ORDER BY x.rank DESC LIMIT 1) RETURNING n,a,b')).rows,
        [[1n,new Record('docs', 'second'),{ selected: true }],[2n,null,null]]);
      assert.deepEqual((await db.execute('UPDATE docs SET (a,b)=(SELECT x.a,x.b FROM lookup x WHERE x.n=docs.n ORDER BY x.rank DESC LIMIT 1 OFFSET 1) RETURNING n,a,b')).rows,
        [[1n,record,payload],[2n,null,null]]);
      assert.deepEqual((await db.execute('UPDATE docs SET (a,b)=(WITH base(k,v,w,r) AS (SELECT n,a,b,rank FROM lookup), chosen AS (SELECT k,v,w,r FROM base) SELECT x.v,x.w FROM chosen x WHERE x.k=docs.n ORDER BY x.r DESC LIMIT 1) RETURNING n,a,b')).rows,
        [[1n,new Record('docs', 'second'),{ selected: true }],[2n,null,null]]);
      assert.deepEqual((await db.execute('UPDATE docs SET (a,b)=(SELECT x.a AS chosen,x.b AS chosen FROM lookup x WHERE x.n=docs.n ORDER BY x.rank DESC LIMIT 1 OFFSET 1) RETURNING n,a,b')).rows,
        [[1n,record,payload],[2n,null,null]]);
      const values = await db.execute('UPDATE docs SET (a,b)=(VALUES($record,$payload)) WHERE n=1 RETURNING a,b', { $record: record, $payload: payload });
      assert.deepEqual(values.rows, [[record,payload]]);
      assert.deepEqual(values.transaction, { before: 'active', after: 'active' });
      assert.deepEqual((await db.execute('UPDATE docs SET (a,b)=(VALUES(b,a)) WHERE n=1 RETURNING a,b')).rows, [[payload,record]]);
      await assert.rejects(async () => db.execute('UPDATE docs SET (a,b)=(VALUES(1,2),(3,4))'), error => error.code === 'FDB_UNSUPPORTED');
      assert.deepEqual((await db.execute('SELECT a,b FROM docs WHERE n=1')).rows, [[payload,record]]);
      const nested = await db.execute('UPDATE docs SET (a,b)=(SELECT (SELECT x.a FROM lookup x WHERE x.n=docs.n ORDER BY x.rank LIMIT 1),(SELECT x.b FROM lookup x WHERE x.n=docs.n ORDER BY x.rank LIMIT 1)) RETURNING n,a,b');
      assert.deepEqual(nested.rows, [[1n,record,payload],[2n,null,null]]);
      assert.deepEqual(nested.transaction, { before: 'active', after: 'active' });
      for (const [operator, expected] of [
        ['UNION ALL', [[1n,new Record('docs', 'second'),2n],[2n,null,null]]],
        ['UNION', [[1n,new Record('docs', 'second'),2n],[2n,null,null]]],
        ['INTERSECT', [[1n,record,1n],[2n,null,null]]],
        ['EXCEPT', [[1n,new Record('docs', 'second'),2n],[2n,null,null]]],
      ]) {
        const compound = await db.execute(`UPDATE docs SET (a,b)=(SELECT x.a,x.rank FROM lookup x WHERE x.n=docs.n ${operator} SELECT y.a,y.rank FROM lookup y WHERE y.n=docs.n AND y.rank=1 ORDER BY 2 DESC LIMIT 1) RETURNING n,a,b`);
        assert.deepEqual(compound.rows, expected);
        assert.deepEqual(compound.transaction, { before: 'active', after: 'active' });
      }
      const result = await db.execute('UPDATE docs SET (a,b)=(SELECT x.a,x.b FROM lookup x WHERE x.n=docs.n) RETURNING n,a,b');
      assert.deepEqual(result.rows, [[1n,record,payload],[2n,null,null]]);
      assert.deepEqual(result.transaction, { before: 'active', after: 'active' });
      assert.deepEqual((await db.execute('SELECT n,a,b FROM docs ORDER BY n')).rows, result.rows);
      await db.execute('INSERT INTO lookup(n,a,rank) VALUES(1,$a,1)', { $a: record });
      assert.deepEqual((await db.execute('UPDATE docs SET (a,b)=(SELECT DISTINCT x.a,x.rank FROM lookup x WHERE x.n=docs.n ORDER BY 2 LIMIT 1 OFFSET 1) RETURNING n,a,b')).rows,
        [[1n,new Record('docs', 'second'),2n],[2n,null,null]]);
      assert.deepEqual((await db.execute('UPDATE docs SET (a,b)=(SELECT DISTINCT x.a,x.rank FROM lookup x WHERE x.n=docs.n ORDER BY 2 LIMIT 1 OFFSET 2) RETURNING n,a,b')).rows,
        [[1n,null,null],[2n,null,null]]);
      await db.execute('ROLLBACK');
      assert.deepEqual((await db.execute('SELECT n,a,b FROM docs ORDER BY n')).rows, [[1n,null,null],[2n,null,null]]);
    } finally { await db.close(); }
  }
});

test('committed tuple lookups survive reopening in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'romedb-tuple-reopen-'));
  try {
    for (const [name, open] of [['sync', file => new Database(file)], ['worker', file => AsyncDatabase.open(file)]]) {
      const file = path.join(dir, name + '.db');
      const record = new Record('docs', 9223372036854775807n);
      const payload = { bytes: Buffer.from([0,255]), values: [true, -9223372036854775808n] };
      let db = await open(file);
      try {
        for (const sql of ['CREATE TABLE docs', 'CREATE TABLE lookup', 'CREATE UNIQUE INDEX docs_n ON docs(n)', 'INSERT INTO docs(n) VALUES(1)']) await db.execute(sql);
        await db.execute('INSERT INTO lookup(n,a,b) VALUES(1,$a,$b)', { $a: record, $b: payload });
        await db.execute('BEGIN');
        await db.execute('UPDATE docs SET (a,b)=(SELECT x.a,x.b FROM lookup x WHERE x.n=docs.n)');
        await db.execute('COMMIT');
      } finally { await db.close(); }
      db = await open(file);
      try {
        assert.deepEqual((await db.execute('SELECT n,a,b FROM docs')).rows, [[1n,record,payload]]);
        assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries, 1n);
        assert.deepEqual((await db.execute('UPDATE docs SET (a,b)=(SELECT b,a) RETURNING a,b')).rows, [[payload,record]]);
      } finally { await db.close(); }
    }
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test('hidden derived and CTE fields sort logically in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute("INSERT INTO docs(label,n) VALUES('six',6),('eleven',11),('negative',-2)");
      await db.execute('UPDATE docs SET payload.n=n');
      for (const [prefix, source, key] of [
        ['', '(SELECT label,n FROM docs) x', 'x.n'],
        ['WITH chosen AS (SELECT label,n FROM docs) ', 'chosen x', 'x.n'],
        ['', '(SELECT label,payload FROM docs) x', 'x.payload.n']
      ]) {
        for (const [direction, rows] of [['ASC', [['negative'],['six'],['eleven']]], ['DESC', [['eleven'],['six'],['negative']]]]) {
          const sql = `${prefix}SELECT x.label FROM ${source} ORDER BY ${key} ${direction}`;
          assert.deepEqual((await db.execute(sql)).rows, rows);
          assert.deepEqual((await db.profileSelect(sql)).result.rows, rows);
        }
      }
    } finally { await db.close(); }
  }
});


test('aggregate tuples preserve empty groups and rollback in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of ['CREATE TABLE docs', 'INSERT INTO docs(n,a,b) VALUES(1,0,0),(2,0,0)',
        'CREATE TABLE lookup', 'INSERT INTO lookup(n,a) VALUES(1,6),(1,11)', 'BEGIN']) await db.execute(sql);
      for (const [group, expected] of [
        ['', [[1n,17n,2n],[2n,null,0n]]],
        [' GROUP BY x.n', [[1n,17n,2n],[2n,null,null]]],
        [' GROUP BY x.n HAVING count(*)>2', [[1n,null,null],[2n,null,null]]],
      ]) {
        const result = await db.execute(`UPDATE docs SET (a,b)=(SELECT sum(x.a),count(*) FROM lookup x WHERE x.n=docs.n${group}) RETURNING n,a,b`);
        assert.deepEqual(result.rows, expected);
        assert.deepEqual(result.transaction, { before: 'active', after: 'active' });
      }
      const filtered = await db.execute('UPDATE docs SET (a,b)=(SELECT sum(x.a) FILTER(WHERE x.a>docs.n*7),count(*) FILTER(WHERE x.a>docs.n*7) FROM lookup x WHERE x.n=docs.n) RETURNING n,a,b');
      assert.deepEqual(filtered.rows, [[1n,11n,1n],[2n,null,0n]]);
      assert.deepEqual(filtered.transaction, { before: 'active', after: 'active' });
      await db.execute('CREATE TABLE allowed(n INTEGER,a INTEGER)');
      await db.execute('INSERT INTO allowed VALUES(1,11)');
      const nested = await db.execute('UPDATE docs SET (a,b)=(SELECT sum(x.a) FILTER(WHERE EXISTS(SELECT 1 FROM allowed y WHERE y.a=x.a AND y.n=docs.n)),count(*) FILTER(WHERE EXISTS(SELECT 1 FROM allowed y WHERE y.a=x.a AND y.n=docs.n)) FROM lookup x WHERE x.n=docs.n) RETURNING n,a,b');
      assert.deepEqual(nested.rows, [[1n,11n,1n],[2n,null,0n]]);
      assert.deepEqual(nested.transaction, { before: 'active', after: 'active' });
      for (const [offset, expected] of [[0,[[1n,2n,2n],[2n,null,null]]],[1,[[1n,1n,1n],[2n,null,null]]],[2,[[1n,null,null],[2n,null,null]]]]) {
        const windowed = await db.execute(`UPDATE docs SET (a,b)=(SELECT row_number() OVER w,count(*) OVER w FROM lookup x WHERE x.n=docs.n WINDOW w AS (ORDER BY x.a) ORDER BY x.a DESC LIMIT 1 OFFSET ${offset}) RETURNING n,a,b`);
        assert.deepEqual(windowed.rows, expected);
        assert.deepEqual(windowed.transaction, { before: 'active', after: 'active' });
      }
      await db.execute('ROLLBACK');
      assert.deepEqual((await db.execute('SELECT n,a,b FROM docs ORDER BY n')).rows, [[1n,0n,0n],[2n,0n,0n]]);
    } finally { await db.close(); }
  }
});

test('removed DML limits reject without mutation in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('INSERT INTO docs(n) VALUES(1),(2),(3)');
      await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
      for (const write of ['UPDATE docs SET n=n+10', 'DELETE FROM docs']) {
        await db.execute('BEGIN');
        await db.execute('INSERT INTO docs(n) VALUES(4)');
        const sql = write + ' LIMIT $count OFFSET $skip';
        for (const params of [{ $count: 1n }, { $count: 0n, $skip: 1n }, { $count: 1n, $skip: 1n }]) {
          await assert.rejects(async () => db.execute(sql, params), error => {
            assert.match(error.message, /LIMIT/);
            assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
            return true;
          });
          assert.deepEqual((await db.execute('SELECT n FROM docs ORDER BY n')).rows, [[1n],[2n],[3n],[4n]]);
        }
        await db.execute('ROLLBACK');
        assert.deepEqual((await db.execute('SELECT n FROM docs ORDER BY n')).rows, [[1n],[2n],[3n]]);
      }
    } finally { await db.close(); }
  }
});

test('update from preserves typed candidates and unmatched targets in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      await db.execute('CREATE TABLE docs');
      await db.execute('INSERT INTO docs(n,a,b) VALUES(1,NULL,NULL),(2,NULL,NULL)');
      await db.execute('CREATE TABLE source');
      const record = new Record('docs', 9223372036854775807n);
      const payload = { values: [true, Buffer.from([0,255]), -9223372036854775808n] };
      await db.execute('INSERT INTO source(k,a,b) VALUES(1,$a,$b)', { $a: new Record('docs','old'), $b: { old: true } });
      await db.execute('INSERT INTO source(k,a,b) VALUES(1,$a,$b)', { $a: record, $b: payload });
      await db.execute('BEGIN');
      await db.execute('CREATE TABLE keys(k INTEGER)');
      await db.execute('INSERT INTO keys VALUES(1),(1)');
      for (const condition of ['k.k=target.n', 'k.k=(SELECT target.n)']) {
        await assert.rejects(async () => db.execute('UPDATE docs AS target SET (a,b)=(s.a,s.b) FROM source s JOIN keys k ON '+condition+' RETURNING n,a,b'), error => {
          assert(['FDB_ENGINE','FDB_UNSUPPORTED'].includes(error.code));
          assert.deepEqual(error.transaction, { before: 'active', after: 'active' });
          return true;
        });
        assert.deepEqual((await db.execute('SELECT n,a,b FROM docs ORDER BY n')).rows, [[1n,null,null],[2n,null,null]]);
        assert.equal((await db.execute('SELECT count(*) FROM keys')).rows[0][0], 2n);
      }
      for (const join of ['JOIN','INNER JOIN','CROSS JOIN']) {
        const joined = await db.execute('UPDATE docs AS target SET (a,b)=(s.a,s.b) FROM source s ' + join + ' keys k ON k.k=s.k WHERE s.k=target.n RETURNING n,a,b');
        assert.equal(joined.affected, 1n);
        assert.deepEqual(joined.rows, [[1n,record,payload]]);
        assert.deepEqual(joined.transaction, { before: 'active', after: 'active' });
        await db.execute('UPDATE docs SET (a,b)=(NULL,NULL)');
      }
      await db.execute('INSERT INTO source(k,a,b) VALUES(2,$a,$b)', { $a: record, $b: payload });
      for (const join of ['LEFT JOIN','LEFT OUTER JOIN']) {
        const left = await db.execute('UPDATE docs AS target SET (a,b)=(s.a,s.b) FROM source s ' + join + ' keys k ON k.k=s.k WHERE s.k=target.n AND k.k IS NULL RETURNING n,a,b');
        assert.equal(left.affected, 1n);
        assert.deepEqual(left.rows, [[2n,record,payload]]);
        assert.deepEqual(left.transaction, { before: 'active', after: 'active' });
        await db.execute('UPDATE docs SET (a,b)=(NULL,NULL)');
      }
      for (const join of ['RIGHT JOIN','RIGHT OUTER JOIN']) {
        const right = await db.execute('UPDATE docs AS target SET (a,b)=(s.a,s.b) FROM keys k ' + join + ' source s ON k.k=s.k WHERE s.k=target.n AND k.k IS NULL RETURNING n,a,b');
        assert.equal(right.affected, 1n);
        assert.deepEqual(right.rows, [[2n,record,payload]]);
        assert.deepEqual(right.transaction, { before: 'active', after: 'active' });
        await db.execute('UPDATE docs SET (a,b)=(NULL,NULL)');
      }
      const derivedFull=await db.execute('UPDATE docs AS target SET (a,b)=(joined.a,joined.b) FROM (SELECT s.k AS n,s.a AS a,s.b AS b FROM source s FULL JOIN keys k ON s.k=k.k WHERE k.k IS NULL) joined WHERE target.n=joined.n RETURNING n,a,b');
      assert.equal(derivedFull.affected,1n);
      assert.deepEqual(derivedFull.rows,[[2n,record,payload]]);
      assert.deepEqual(derivedFull.transaction,{before:'active',after:'active'});
      assert.deepEqual((await db.execute('SELECT n,a,b FROM docs ORDER BY n')).rows,[[1n,null,null],[2n,record,payload]]);
      await db.execute('UPDATE docs SET (a,b)=(NULL,NULL)');
      for (const name of ['docs', 'target']) {
        for (const hint of ['', 'MATERIALIZED', 'NOT MATERIALIZED']) {
          const scoped = await db.execute(`WITH ${name}(n,a,b) AS ${hint} (SELECT k,a,b FROM source WHERE k=2 LIMIT $count OFFSET $skip), chosen AS (SELECT n,a,b FROM ${name}) UPDATE docs AS target SET (a,b)=(s.a,s.b) FROM chosen s WHERE s.n=target.n RETURNING n,a,b`, { $count: 1n, $skip: 0n });
          assert.equal(scoped.affected, 1n);
          assert.deepEqual(scoped.rows, [[2n,record,payload]]);
          assert.deepEqual(scoped.transaction, { before: 'active', after: 'active' });
          assert.deepEqual((await db.execute('SELECT n,a,b FROM docs ORDER BY n')).rows, [[1n,null,null],[2n,record,payload]]);
          await db.execute('UPDATE docs SET (a,b)=(NULL,NULL)');
        }
      }
      await db.execute('DELETE FROM source WHERE k=2');
      // SELECT pagination requires a source with one row per intended target.
      await db.execute('DELETE FROM source WHERE a=docs:old');
      const paginated = 'UPDATE docs AS target SET (a,b)=(s.a,s.b) FROM (SELECT k,a,b FROM source LIMIT $count OFFSET $skip) s WHERE s.k=target.n RETURNING n,a,b';
      await assert.rejects(async () => db.execute(paginated, { $count: 1n }), error => error.code === 'FDB_CONSTRAINT' && error.transaction.after === 'active');
      for (const [count, skip, expected] of [[0n,0n,[]],[1n,1n,[]],[1n,0n,[[1n,record,payload]]]]) {
        const page = await db.execute(paginated, { $count: count, $skip: skip });
        assert.deepEqual(page.rows, expected);
        assert.equal(page.affected, BigInt(expected.length));
        assert.deepEqual(page.transaction, { before: 'active', after: 'active' });
      }
      await db.execute('UPDATE docs SET (a,b)=(NULL,NULL)');
      const result = await db.execute('UPDATE docs AS target SET (a,b)=(s.a,s.b) FROM source s WHERE s.k=target.n RETURNING n,a,b');
      assert.equal(result.affected, 1n);
      assert.deepEqual(result.rows, [[1n,record,payload]]);
      assert.deepEqual(result.transaction, { before: 'active', after: 'active' });
      assert.deepEqual((await db.execute('SELECT n,a,b FROM docs ORDER BY n')).rows, [[1n,record,payload],[2n,null,null]]);
      await db.execute('ROLLBACK');
      assert.deepEqual((await db.execute('SELECT n,a,b FROM docs ORDER BY n')).rows, [[1n,null,null],[2n,null,null]]);
    } finally { await db.close(); }
  }
});

test('OR ROLLBACK reports full transaction loss and permits fresh client work', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of ['CREATE TABLE docs', 'DEFINE FIELD v ON docs TYPE integer REQUIRED CHECK(v<5)', 'CREATE UNIQUE INDEX docs_n ON docs(n)', 'INSERT INTO docs(n,v) VALUES(1,0),(2,0)']) await db.execute(sql);
      for (const [assignment,code] of [['n=1','FDB_CONSTRAINT'],['v=10','FDB_VALIDATION']]) {
        await db.execute('BEGIN');
        await db.execute('INSERT INTO docs(n,v) VALUES(3,0)');
        await assert.rejects(async () => db.execute('UPDATE OR ROLLBACK docs SET '+assignment+' WHERE n>1 RETURNING n,v'), error => {
          assert.equal(error.code,code);
          assert.deepEqual(error.transaction,{before:'active',after:'autocommit'});
          return true;
        });
        assert.deepEqual((await db.execute('SELECT n,v FROM docs ORDER BY n')).rows,[[1n,0n],[2n,0n]]);
        assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,2n);
      }
      await db.execute('BEGIN');
      const record=new Record('docs',9223372036854775807n);
      const result=await db.execute('UPDATE OR ROLLBACK docs SET payload=$value WHERE n=2 RETURNING payload',{$value:[record,Buffer.from([0,255])]});
      assert.deepEqual(result.rows,[[[record,Buffer.from([0,255])]]]);
      assert.deepEqual(result.transaction,{before:'active',after:'active'});
      await db.execute('COMMIT');
      assert.deepEqual((await db.execute('SELECT payload FROM docs WHERE n=2')).rows,result.rows);
    } finally { await db.close(); }
  }
});

test('OR IGNORE preserves typed successes and pending client work', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of ['CREATE TABLE docs', 'DEFINE FIELD v ON docs TYPE integer REQUIRED CHECK(v<5)', 'CREATE INDEX docs_v ON docs(v)', 'CREATE UNIQUE INDEX docs_n ON docs(n)', 'INSERT INTO docs(n,v) VALUES(1,0),(2,0),(3,0)', 'BEGIN', 'INSERT INTO docs(n,v) VALUES(4,0)']) await db.execute(sql);
      const payload=[new Record('docs',9223372036854775807n),Buffer.from([0,255])];
      for (const from of ['', ' FROM (SELECT 1 AS k) source']) {
        await assert.rejects(async () => db.execute(`UPDATE OR IGNORE docs SET v=10${from} RETURNING n LIMIT 1`), error => /LIMIT/.test(error.message));
        // A mixed batch must continue past rejected candidates and return only successes.
        const mixed=await db.execute(`UPDATE OR IGNORE docs SET v=CASE WHEN docs.n=2 THEN 10 ELSE 1 END,payload=$value${from} WHERE docs.n IN (2,3) RETURNING n,payload`,{$value:payload});
        assert.equal(mixed.affected,1n);
        assert.deepEqual(mixed.rows,[[3n,payload]]);
        assert.deepEqual(mixed.transaction,{before:'active',after:'active'});
        const conflict=await db.execute(`UPDATE OR IGNORE docs SET n=1,v=2,payload=$value${from} WHERE docs.n=2 RETURNING n`,{$value:payload});
        assert.equal(conflict.affected,0n);
        assert.deepEqual(conflict.rows,[]);
        assert.deepEqual((await db.execute('SELECT n,v,payload FROM docs ORDER BY n')).rows,[[1n,0n,null],[2n,0n,null],[3n,1n,payload],[4n,0n,null]]);
        await assert.rejects(async () => db.execute(`UPDATE OR IGNORE docs SET v=$missing${from}`),error => {
          assert.equal(error.code,'FDB_PARAMETER');
          assert.deepEqual(error.transaction,{before:'active',after:'active'});
          return true;
        });
        assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,8n);
      }
      await db.execute('ROLLBACK');
      assert.deepEqual((await db.execute('SELECT n,v,payload FROM docs ORDER BY n')).rows,[[1n,0n,null],[2n,0n,null],[3n,0n,null]]);
      const committed=await db.execute('UPDATE OR IGNORE docs SET v=CASE WHEN n=2 THEN 10 ELSE 1 END,payload=$value WHERE n IN (2,3) RETURNING n,payload',{$value:payload});
      assert.deepEqual(committed.rows,[[3n,payload]]);
      assert.deepEqual(committed.transaction,{before:'autocommit',after:'autocommit'});
      assert.deepEqual((await db.execute('SELECT payload FROM docs WHERE n=3')).rows,[[payload]]);
      assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,6n);
    } finally { await db.close(); }
  }
});

test('OR FAIL exposes retained typed updates and accurate transaction reports', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db = await open();
    try {
      for (const sql of ['CREATE TABLE docs','DEFINE FIELD v ON docs TYPE integer REQUIRED CHECK(v<5)','CREATE INDEX docs_v ON docs(v)','CREATE UNIQUE INDEX docs_n ON docs(n)','INSERT INTO docs(n,v) VALUES(1,0),(2,0),(3,0)']) await db.execute(sql);
      const payload=[new Record('docs',9223372036854775807n),Buffer.from([0,255])];
      for (const from of ['', ' FROM (SELECT 1 AS k) source']) {
        for (const limited of [false,true]) {
          await db.execute('BEGIN');
          await db.execute('INSERT INTO docs(n,v) VALUES(4,0)');
          const sql=`UPDATE OR FAIL docs SET v=CASE WHEN docs.n=2 THEN 10 ELSE 1 END,payload=$value${from} RETURNING n,payload`;
          await assert.rejects(async () => limited
            ? db.writeWithResultLimits(sql,{maxRows:10n,maxPayloadBytes:10000n},{$value:payload})
            : db.execute(sql,{$value:payload}), error => {
              assert.equal(error.code,'FDB_VALIDATION');
              assert.deepEqual(error.transaction,{before:'active',after:'active'});
              return true;
            });
          assert.deepEqual((await db.execute('SELECT n,v,payload FROM docs ORDER BY n')).rows,[[1n,limited?0n:1n,limited?null:payload],[2n,0n,null],[3n,0n,null],[4n,0n,null]]);
          assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,8n);
          await db.execute('ROLLBACK');
          assert.deepEqual((await db.execute('SELECT n,v,payload FROM docs ORDER BY n')).rows,[[1n,0n,null],[2n,0n,null],[3n,0n,null]]);
        }
      }
      await assert.rejects(async () => db.execute('UPDATE OR FAIL docs SET n=10,v=1,payload=$value WHERE n<3 RETURNING n,payload',{$value:payload}),error => {
        assert.equal(error.code,'FDB_CONSTRAINT');
        assert.deepEqual(error.transaction,{before:'autocommit',after:'autocommit'});
        return true;
      });
      assert.deepEqual((await db.execute('SELECT n,v,payload FROM docs ORDER BY n')).rows,[[2n,0n,null],[3n,0n,null],[10n,1n,payload]]);
      assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,6n);
      await db.execute('BEGIN');
      await db.execute('UPDATE docs SET n=1 WHERE n=10');
      await db.execute('COMMIT');
      assert.deepEqual((await db.execute('SELECT n FROM docs ORDER BY n')).rows,[[1n],[2n],[3n]]);
    } finally { await db.close(); }
  }
});

test('OR REPLACE preserves typed results and restores deleted conflicts on failure', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db=await open();
    try {
      for (const sql of ['CREATE TABLE docs','DEFINE FIELD v ON docs TYPE integer REQUIRED CHECK(v<5)','CREATE UNIQUE INDEX docs_n ON docs(n)','CREATE INDEX docs_v ON docs(v)','INSERT INTO docs(n,v) VALUES(1,0),(2,0),(3,0)']) await db.execute(sql);
      const payload=[new Record('docs',9223372036854775807n),Buffer.from([0,255])];
      for (const from of ['', ' FROM (SELECT 1 AS k) source']) {
        await db.execute('BEGIN');
        await db.execute('INSERT INTO docs(n,v) VALUES(4,0)');
        const before=(await db.execute('SELECT * FROM docs ORDER BY n')).rows;
        await assert.rejects(async()=>db.execute('UPDATE OR REPLACE docs SET n=docs.n+1,v=CASE WHEN docs.n=3 THEN 10 ELSE 1 END'+from+' WHERE docs.n<4 RETURNING n'),error=>{
          assert.equal(error.code,'FDB_VALIDATION');
          assert.deepEqual(error.transaction,{before:'active',after:'active'});
          return true;
        });
        assert.deepEqual((await db.execute('SELECT * FROM docs ORDER BY n')).rows,before);
        assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,8n);
        const result=await db.execute('UPDATE OR REPLACE docs SET n=10,payload=$value'+from+' WHERE docs.n<3 RETURNING n,payload',{$value:payload});
        assert.equal(result.affected,2n);
        assert.deepEqual(result.rows,[[10n,payload],[10n,payload]]);
        assert.deepEqual(result.transaction,{before:'active',after:'active'});
        assert.deepEqual((await db.execute('SELECT n,payload FROM docs ORDER BY n')).rows,[[3n,null],[4n,null],[10n,payload]]);
        assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,6n);
        await db.execute('ROLLBACK');
        assert.deepEqual((await db.execute('SELECT n FROM docs ORDER BY n')).rows,[[1n],[2n],[3n]]);
      }
      const committed=await db.execute('UPDATE OR REPLACE docs SET n=n+1,payload=$value RETURNING n,payload',{$value:payload});
      assert.deepEqual(committed.rows,[[2n,payload],[4n,payload]]);
      assert.equal(committed.affected,2n);
      assert.deepEqual(committed.transaction,{before:'autocommit',after:'autocommit'});
      assert.deepEqual((await db.execute('SELECT n,payload FROM docs ORDER BY n')).rows,committed.rows);
      assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,4n);
    } finally { await db.close(); }
  }
});

test('INSERT OR ABORT restores failed batches and retains typed client values', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db=await open();
    try {
      for (const sql of ['CREATE TABLE docs','DEFINE FIELD v ON docs TYPE integer REQUIRED CHECK(v<5)','CREATE UNIQUE INDEX docs_n ON docs(n)','INSERT INTO docs(n,v) VALUES(1,0)','BEGIN','INSERT INTO docs(n,v) VALUES(4,0)']) await db.execute(sql);
      const payload=[new Record('docs',9223372036854775807n),Buffer.from([0,255])];
      for (const [source,code] of [
        ['VALUES(2,0,$value),(1,0,$value)','FDB_CONSTRAINT'],
        ['SELECT 2,0,$value UNION ALL SELECT 3,10,$value','FDB_VALIDATION'],
      ]) {
        await assert.rejects(async()=>db.execute('INSERT OR ABORT INTO docs(n,v,payload) '+source+' RETURNING n,payload',{$value:payload}),error=>{
          assert.equal(error.code,code);
          assert.deepEqual(error.transaction,{before:'active',after:'active'});
          return true;
        });
        assert.deepEqual((await db.execute('SELECT n,v FROM docs ORDER BY n')).rows,[[1n,0n],[4n,0n]]);
        assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,2n);
      }
      const result=await db.execute('INSERT OR ABORT INTO docs(n,v,payload) SELECT 2,1,$value RETURNING n,payload',{$value:payload});
      assert.equal(result.affected,1n);
      assert.deepEqual(result.rows,[[2n,payload]]);
      assert.deepEqual(result.transaction,{before:'active',after:'active'});
      await db.execute('ROLLBACK');
      assert.deepEqual((await db.execute('SELECT n FROM docs')).rows,[[1n]]);
      const committed=await db.execute('INSERT OR ABORT INTO docs(n,v,payload) VALUES(2,1,$value) RETURNING payload',{$value:payload});
      assert.deepEqual(committed.rows,[[payload]]);
      assert.deepEqual(committed.transaction,{before:'autocommit',after:'autocommit'});
      assert.deepEqual((await db.execute('SELECT payload FROM docs WHERE n=2')).rows,[[payload]]);
      assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,2n);
    } finally { await db.close(); }
  }
});

test('multi-column INSERT SELECT preserves native and collection sources in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db=await open();
    try {
      for (const sql of ['CREATE TABLE native_source(n INTEGER,v BLOB)','CREATE TABLE docs','CREATE TABLE target(n INTEGER UNIQUE,v BLOB)']) await db.execute(sql);
      const bytes=Buffer.from([0,127,255]);
      await db.execute('INSERT INTO native_source(n,v) VALUES(1,$v),(2,$v)',{$v:bytes});
      await db.execute('INSERT INTO docs(n,v) VALUES(1,$v),(2,$v)',{$v:bytes});
      for (const source of ['native_source','docs']) {
        for (const policy of ['', 'OR ABORT']) {
          await db.execute('BEGIN');
          const sql='INSERT '+policy+' INTO target(n,v) SELECT n,v FROM '+source+' ORDER BY n RETURNING n,v';
          const result=await db.execute(sql);
          assert.equal(result.affected,2n);
          assert.deepEqual(result.rows,[[1n,bytes],[2n,bytes]]);
          assert.deepEqual(result.transaction,{before:'active',after:'active'});
          await assert.rejects(async()=>db.execute(sql),error=>{
            assert.equal(error.code,'FDB_CONSTRAINT');
            assert.deepEqual(error.transaction,{before:'active',after:'active'});
            return true;
          });
          assert.deepEqual((await db.execute('SELECT n,v FROM target ORDER BY n')).rows,result.rows);
          await db.execute('ROLLBACK');
          assert.deepEqual((await db.execute('SELECT * FROM target')).rows,[]);
        }
      }
      await assert.rejects(async()=>db.execute("INSERT INTO __romedb_catalog(name,metadata) SELECT 'injected',X'00'"),error=>error.code==='FDB_UNSUPPORTED');
      assert.equal((await db.checkCollectionIntegrity('docs')).documents,2n);
      const final=await db.execute('INSERT INTO target(n,v) SELECT n,v FROM native_source RETURNING n,v');
      assert.equal(final.affected,2n);
      assert.deepEqual(final.transaction,{before:'autocommit',after:'autocommit'});
    } finally { await db.close(); }
  }
});

test('INSERT OR ROLLBACK reports transaction loss and permits typed retries', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db=await open();
    try {
      for (const sql of ['CREATE TABLE docs','DEFINE FIELD v ON docs TYPE integer REQUIRED CHECK(v<5)','CREATE UNIQUE INDEX docs_n ON docs(n)','INSERT INTO docs(n,v) VALUES(1,0)']) await db.execute(sql);
      const payload=[new Record('docs',9223372036854775807n),Buffer.from([0,255])];
      for (const [source,code] of [
        ['VALUES(2,0,$value),(1,0,$value)','FDB_CONSTRAINT'],
        ['SELECT 2,0,$value UNION ALL SELECT 3,10,$value','FDB_VALIDATION'],
      ]) {
        await db.execute('BEGIN');
        await db.execute('INSERT INTO docs(n,v) VALUES(4,0)');
        await assert.rejects(async()=>db.execute('INSERT OR ROLLBACK INTO docs(n,v,payload) '+source+' RETURNING n,payload',{$value:payload}),error=>{
          assert.equal(error.code,code);
          assert.deepEqual(error.transaction,{before:'active',after:'autocommit'});
          return true;
        });
        assert.deepEqual((await db.execute('SELECT n,v FROM docs ORDER BY n')).rows,[[1n,0n]]);
        assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,1n);
      }
      await db.execute('BEGIN');
      await assert.rejects(async()=>db.execute('INSERT OR ROLLBACK INTO docs(n,v) VALUES($missing,0)'),error=>{
        assert.equal(error.code,'FDB_PARAMETER');
        assert.deepEqual(error.transaction,{before:'active',after:'active'});
        return true;
      });
      const result=await db.execute('INSERT OR ROLLBACK INTO docs(n,v,payload) SELECT 2,1,$value RETURNING n,payload',{$value:payload});
      assert.equal(result.affected,1n);
      assert.deepEqual(result.rows,[[2n,payload]]);
      assert.deepEqual(result.transaction,{before:'active',after:'active'});
      await db.execute('COMMIT');
      assert.deepEqual((await db.execute('SELECT payload FROM docs WHERE n=2')).rows,[[payload]]);
      assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,2n);
    } finally { await db.close(); }
  }
});

test('INSERT OR IGNORE returns only valid typed candidates in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db=await open();
    try {
      for (const sql of ['CREATE TABLE docs','DEFINE FIELD v ON docs TYPE integer REQUIRED CHECK(v<5)','CREATE INDEX docs_v ON docs(v)','CREATE UNIQUE INDEX docs_n ON docs(n)',"INSERT INTO docs(id,n,v) VALUES(docs:existing,1,0)"]) await db.execute(sql);
      const payload=[new Record('docs',9223372036854775807n),Buffer.from([0,255])];
      for (const source of [
        "VALUES(docs:a,2,1,$value),(docs:existing,3,1,$value),(docs:b,1,2,$value),(docs:c,4,10,$value),(docs:d,5,3,$value)",
        "SELECT docs:a,2,1,$value UNION ALL SELECT docs:existing,3,1,$value UNION ALL SELECT docs:b,1,2,$value UNION ALL SELECT docs:c,4,10,$value UNION ALL SELECT docs:d,5,3,$value",
      ]) {
        await db.execute('BEGIN');
        const result=await db.execute('INSERT OR IGNORE INTO docs(id,n,v,payload) '+source+' RETURNING id,payload',{$value:payload});
        assert.equal(result.affected,2n);
        assert.deepEqual(result.rows,[[new Record('docs','a'),payload],[new Record('docs','d'),payload]]);
        assert.deepEqual(result.transaction,{before:'active',after:'active'});
        assert.deepEqual((await db.execute('SELECT n,v FROM docs ORDER BY n')).rows,[[1n,0n],[2n,1n],[5n,3n]]);
        assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,6n);
        const skipped=await db.execute('INSERT OR IGNORE INTO docs(n,v) VALUES(1,0),(9,10) RETURNING n');
        assert.equal(skipped.affected,0n);
        assert.deepEqual(skipped.rows,[]);
        await db.execute('ROLLBACK');
        assert.deepEqual((await db.execute('SELECT n FROM docs')).rows,[[1n]]);
      }
      const committed=await db.execute('INSERT OR IGNORE INTO docs(n,v,payload) VALUES(1,0,$value),(2,1,$value) RETURNING payload',{$value:payload});
      assert.deepEqual(committed.rows,[[payload]]);
      assert.deepEqual(committed.transaction,{before:'autocommit',after:'autocommit'});
      assert.deepEqual((await db.execute('SELECT payload FROM docs WHERE n=2')).rows,[[payload]]);
      assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,4n);
    } finally { await db.close(); }
  }
});

test('INSERT OR FAIL retains typed prefixes while bounded writes remain atomic', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db=await open();
    try {
      for (const sql of ['CREATE TABLE docs','DEFINE FIELD v ON docs TYPE integer REQUIRED CHECK(v<5)','CREATE INDEX docs_v ON docs(v)','CREATE UNIQUE INDEX docs_n ON docs(n)','INSERT INTO docs(n,v) VALUES(1,0)']) await db.execute(sql);
      const payload=[new Record('docs',9223372036854775807n),Buffer.from([0,255])];
      for (const [source,code] of [
        ['VALUES(2,1,$value),(1,2,$value),(5,3,$value)','FDB_CONSTRAINT'],
        ['SELECT 2,1,$value UNION ALL SELECT 3,10,$value UNION ALL SELECT 5,3,$value','FDB_VALIDATION'],
      ]) {
        for (const limited of [false,true]) {
          await db.execute('BEGIN');
          await db.execute('INSERT INTO docs(n,v) VALUES(4,0)');
          const sql='INSERT OR FAIL INTO docs(n,v,payload) '+source+' RETURNING n,payload';
          await assert.rejects(async()=>limited
            ? db.writeWithResultLimits(sql,{maxRows:10n,maxPayloadBytes:10000n},{$value:payload})
            : db.execute(sql,{$value:payload}),error=>{
              assert.equal(error.code,code);
              assert.deepEqual(error.transaction,{before:'active',after:'active'});
              return true;
            });
          const expected=[[1n,null]];
          if (!limited) expected.push([2n,payload]);
          expected.push([4n,null]);
          assert.deepEqual((await db.execute('SELECT n,payload FROM docs ORDER BY n')).rows,expected);
          assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,BigInt(expected.length)*2n);
          await db.execute('ROLLBACK');
          assert.deepEqual((await db.execute('SELECT n FROM docs')).rows,[[1n]]);
        }
      }
      await assert.rejects(async()=>db.execute('INSERT OR FAIL INTO docs(n,v,payload) VALUES(2,1,$value),(1,0,$value)',{$value:payload}),error=>{
        assert.equal(error.code,'FDB_CONSTRAINT');
        assert.deepEqual(error.transaction,{before:'autocommit',after:'autocommit'});
        return true;
      });
      assert.deepEqual((await db.execute('SELECT payload FROM docs WHERE n=2')).rows,[[payload]]);
      await db.execute('BEGIN');
      await db.execute('INSERT OR FAIL INTO docs(n,v) VALUES(3,1)');
      await db.execute('COMMIT');
      assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,6n);
    } finally { await db.close(); }
  }
});

test('INSERT OR REPLACE replaces typed documents and restores earlier deletions on failure', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open()]) {
    const db=await open();
    try {
      for (const sql of ['CREATE TABLE docs','DEFINE FIELD v ON docs TYPE integer REQUIRED CHECK(v<5)','CREATE UNIQUE INDEX docs_n ON docs(n)','CREATE INDEX docs_v ON docs(v)',"INSERT INTO docs(id,n,v,extra) VALUES(docs:a,1,0,'old'),(docs:b,2,0,'other')"]) await db.execute(sql);
      const payload=[new Record('docs',9223372036854775807n),Buffer.from([0,255])];
      for (const source of ["VALUES(docs:a,2,1,$value)","SELECT docs:a,2,1,$value"]) {
        await db.execute('BEGIN');
        const before=(await db.execute('SELECT * FROM docs ORDER BY n')).rows;
        await assert.rejects(async()=>db.execute('INSERT OR REPLACE INTO docs(id,n,v,payload) '+source+' UNION ALL SELECT docs:c,3,10,$value',{$value:payload}),error=>{
          assert.equal(error.code,'FDB_VALIDATION');
          assert.deepEqual(error.transaction,{before:'active',after:'active'});
          return true;
        });
        assert.deepEqual((await db.execute('SELECT * FROM docs ORDER BY n')).rows,before);
        const result=await db.execute('INSERT OR REPLACE INTO docs(id,n,v,payload) '+source+' RETURNING id,n,payload,extra',{$value:payload});
        assert.equal(result.affected,1n);
        assert.deepEqual(result.rows,[[new Record('docs','a'),2n,payload,null]]);
        assert.deepEqual(result.transaction,{before:'active',after:'active'});
        assert.deepEqual((await db.execute('SELECT id,n,payload,extra FROM docs')).rows,result.rows);
        assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,2n);
        await db.execute('ROLLBACK');
        assert.deepEqual((await db.execute('SELECT * FROM docs ORDER BY n')).rows,before);
      }
      const result=await db.execute('INSERT OR REPLACE INTO docs(n,v,payload) VALUES(1,1,$value) RETURNING id,payload',{$value:payload});
      assert.equal(result.affected,1n);
      assert.notDeepEqual(result.rows[0][0],new Record('docs','a'));
      assert.deepEqual(result.rows[0][1],payload);
      assert.deepEqual(result.transaction,{before:'autocommit',after:'autocommit'});
      assert.deepEqual((await db.execute('SELECT docs:a')).rows,[]);
      assert.equal((await db.checkCollectionIntegrity('docs')).indexEntries,4n);
    } finally { await db.close(); }
  }
});

test('document INSERT and UPSERT automatically create collections in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      await db.execute('INSERT INTO auto_docs DOCUMENT $doc', {$doc: {id: new Record('auto_docs', 'a'), n: 1n}});
      await db.execute('UPSERT auto_people:a {name:\'Alice\'}');
      assert.equal((await db.all('SELECT * FROM auto_docs')).length, 1);
      assert.equal((await db.all('SELECT * FROM auto_people')).length, 1);
      await db.execute('BEGIN');
      await db.execute('UPSERT rolled_back:a {n:1}');
      await db.execute('ROLLBACK');
      await db.execute('CREATE TABLE rolled_back');
    } finally { await db.close(); }
  }
});

test('collection SDK supports typed CRUD, merge, rollback and native-table rejection', async () => {
  const {AsyncDatabase} = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open()]) {
    try {
      const people = db.collection('people');
      const alice = await people.upsert('alice', {name:'Alice', profile:{active:true}, bytes:Buffer.from([0,255])});
      assert.deepEqual(alice.id, new Record('people','alice'));
      assert.deepEqual(await people.get('alice'), alice);
      assert.equal(await people.get('absent'), undefined);
      const changed = await people.merge('alice', {name:'Bob'});
      assert.deepEqual(changed.profile, {active:true});
      assert.equal(changed.name,'Bob');
      await db.execute('BEGIN');
      await people.delete('alice');
      await db.execute('ROLLBACK');
      assert.deepEqual(await people.get('alice'), changed);
      assert.equal((await people.all()).length,1);
      const generated = await people.insert({name:'Generated'});
      assert(generated.id instanceof Record);
      assert.deepEqual(await people.delete(generated.id.key),generated);
      const numeric = await people.upsert(1n, {'quoted"field':'safe'});
      assert.equal(numeric['quoted"field'],'safe');
      assert.equal(await people.merge('absent',{name:'Nobody'}),undefined);
      await db.execute('CREATE TABLE native(n INTEGER)');
      await db.execute('INSERT INTO native VALUES(1)');
      await assert.rejects(async()=>db.collection('native').delete(1n),/document collection/);
      assert.equal((await db.all('SELECT * FROM native')).length,1);
    } finally { await db.close(); }
  }
});

test('V2 spatial points and radius predicates preserve values in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      await db.execute('INSERT INTO places {id:places:a,location:geo::point(180,0)}');
      const point = {type: 'Point', coordinates: [180, 0]};
      assert.deepEqual(await db.exactlyOne('SELECT location FROM places'), [point]);
      assert.deepEqual(await db.all('SELECT geo::distance(location,$point),geo::within(location,$point,0) FROM places', {$point: point}), [[0, true]]);
      await db.execute('BEGIN');
      await db.execute('UPDATE places SET location=geo::point(0,0)');
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.exactlyOne('SELECT location FROM places'), [point]);
      await assert.rejects(async () => db.execute('UPDATE places SET location=geo::point(181,0)'), /geo:/);
      assert.deepEqual(await db.exactlyOne('SELECT location FROM places'), [point]);
    } finally { await db.close(); }
  }
});

test('V2 indexed spatial search works in both clients with rollback and inspection', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      await db.execute('INSERT INTO places {id:places:a,location:geo::point(180,0)}');
      await db.execute('CREATE SEARCH INDEX locations ON places(location) USING SPATIAL');
      const sql = 'SELECT id,distance_m FROM search::near($index,$point,$radius) ORDER BY distance_m,id';
      const params = {$index: 'locations', $point: {type: 'Point', coordinates: [-180,0]}, $radius: 0};
      const rows = await db.all(sql, params);
      assert.equal(rows.length, 1);
      assert.equal(rows[0][0].key, 'a');
      assert.equal(rows[0][1], 0);
      await db.execute('BEGIN');
      await db.execute('UPDATE places SET location=geo::point(0,0)');
      assert.deepEqual(await db.all(sql, params), []);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all(sql, params), rows);
      await assert.rejects(async () => db.execute("UPDATE places {location:{type:'Point',coordinates:[181,0]}}"));
      assert.deepEqual(await db.all(sql, params), rows);
      await db.checkCollectionIntegrity('places');
      await db.execute('DROP INDEX locations');
      await assert.rejects(async () => db.all(sql, params), /spatial index/);
    } finally { await db.close(); }
  }
});

test('V2 H3 aggregation and cell centers preserve values in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open(':memory:')]) {
    const db = await open();
    try {
      await db.execute('INSERT INTO places {id:places:a,location:geo::point(40,45)}');
      await db.execute('INSERT INTO places {id:places:b,location:geo::point(40,45)}');
      assert.deepEqual(await db.all('SELECT geo::cell(location,$r),count(*) FROM places GROUP BY geo::cell(location,$r)', {$r:2}), [['822d57fffffffff',2n]]);
      const [center] = await db.exactlyOne('SELECT geo::cell_center($cell)', {$cell:'822d57fffffffff'});
      assert.equal(center.type, 'Point');
      assert.deepEqual(await db.exactlyOne('SELECT geo::cell($point,2)', {$point:center}), ['822d57fffffffff']);
      await assert.rejects(async () => db.execute('UPDATE places SET location=geo::cell_center($bad)', {$bad:'822D57FFFFFFFFF'}));
      assert.deepEqual(await db.exactlyOne('SELECT geo::cell(location,2) FROM places LIMIT 1'), ['822d57fffffffff']);
    } finally { await db.close(); }
  }
});

test('V2 record brace projections preserve cardinality, types and limits in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open(':memory:')]) {
    const db = await open();
    try {
      await db.execute("INSERT INTO users {id:users:u1,name:'Alice'}");
      await db.execute("INSERT INTO posts {id:posts:p1,title:'Hello',author:users:u1,profile:{city:'Bangkok'}}");
      const sql='SELECT posts:p1 {title,author.* AS writer,profile.*,profile.city AS city}';
      const result=await db.execute(sql);
      assert.deepEqual(result.columns,['title','writer','profile','city']);
      assert.equal(result.rows[0][1].name,'Alice');
      assert.deepEqual(result.rows[0][1].id,new Record('users','u1'));
      assert.deepEqual(result.rows[0][2],{city:'Bangkok'});
      assert.equal(result.rows[0][3],'Bangkok');
      const missing=await db.execute('SELECT posts:missing {title,author.* AS writer}');
      assert.deepEqual(missing.columns,['title','writer']);
      assert.deepEqual(missing.rows,[]);
      const profile=await db.profileSelect(sql);
      assert.deepEqual(profile.result.rows,result.rows);
      await assert.rejects(async () => db.selectWithLimits(sql,{maxRows:0n,maxPayloadBytes:10000n}),e=>e.code==='FDB_LIMIT');
      await db.execute('BEGIN');
      await db.execute("UPDATE users:u1 {name:'Pending'}");
      assert.equal((await db.exactlyOne('SELECT posts:p1 {author.*}'))[0].name,'Pending');
      await db.execute('ROLLBACK');
      assert.equal((await db.exactlyOne('SELECT posts:p1 {author.*}'))[0].name,'Alice');
    } finally { await db.close(); }
  }
});

test('V2 inverse relationships preserve indexed pages, transactions and bounds in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open(':memory:')]) {
    const db = await open();
    try {
      await db.execute("INSERT INTO users {id:users:u1,name:'Alice'}");
      await db.execute("INSERT INTO posts {id:posts:a,author:users:u1,title:'A'}");
      await db.execute("INSERT INTO posts {id:posts:b,author:users:u1,title:'B'}");
      await db.execute('CREATE INDEX authors ON posts(author)');
      await db.execute('DEFINE RELATION authored ON users FROM posts.author');
      const sql="SELECT relation::fetch(id,'authored',1,$after) AS posts FROM users WHERE id=users:u1";
      const first=await db.exactlyOne(sql,{$after:null});
      assert.deepEqual(first[0].map(p=>p.id),[new Record('posts','a')]);
      const second=await db.exactlyOne(sql,{$after:first[0][0].id});
      assert.deepEqual(second[0].map(p=>p.id),[new Record('posts','b')]);
      assert.deepEqual(await db.exactlyOne(sql,{$after:second[0][0].id}),[[]]);
      const profile=await db.profileSelect("SELECT relation::fetch(users:u1,'authored'),relation::fetch(users:u1,'authored')");
      assert.equal(profile.metrics.fetchBatches,1n);
      assert.deepEqual(profile.result.rows[0][0],profile.result.rows[0][1]);
      await assert.rejects(async()=>db.selectWithLimits(sql,{maxRows:1n,maxPayloadBytes:5n},{$after:null}),e=>e.code==='FDB_LIMIT');
      await assert.rejects(async()=>db.execute('DROP INDEX authors'),/dependent relations/);
      await db.execute('BEGIN');
      await db.execute('UPDATE posts:a {author:users:u2}');
      assert.deepEqual((await db.exactlyOne(sql,{$after:null}))[0].map(p=>p.id),[new Record('posts','b')]);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.exactlyOne(sql,{$after:null}),first);
      const [info]=await db.exactlyOne('INFO FOR RELATION authored');
      assert.equal(info.index,'authors');
      await db.execute('DROP RELATION authored');
      await db.execute('DROP INDEX authors');
      await db.checkCollectionIntegrity('posts');
    } finally { await db.close(); }
  }
});

test('V2 full-text search preserves typed ranked hits and rollback in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open(':memory:')]) {
    const db = await open();
    try {
      await db.execute("INSERT INTO articles {id:articles:b,title:'Fast database',body:'atomic transactions'}");
      await db.execute("INSERT INTO articles {id:articles:a,title:'Fast database',body:'atomic transactions'}");
      await db.execute('CREATE SEARCH INDEX articles_text ON articles(title,body) USING FULLTEXT');
      const sql = 'SELECT id,score FROM search::text($index,$query,$limit) ORDER BY score DESC,id';
      const params = {$index:'articles_text',$query:'database',$limit:1};
      const rows = await db.all(sql,params);
      assert.deepEqual(rows.map(row=>row[0]),[new Record('articles','a')]);
      assert.ok(Number.isFinite(rows[0][1]) && rows[0][1]>0);
      await assert.rejects(async()=>db.all(sql,{...params,$query:'fast AND NOT database'}),
        e=>e.code==='FDB_VALIDATION' && /negative-only/.test(e.message));
      assert.deepEqual((await db.all(sql,{...params,$query:'+fast -absent'})).map(row=>row[0]),
        [new Record('articles','a')]);
      await db.execute('BEGIN');
      await db.execute("UPDATE articles SET title='empty'");
      assert.deepEqual(await db.all(sql,params),[]);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all(sql,params),rows);
      await assert.rejects(async()=>db.execute('UPDATE articles {body:42}'),e=>e.code==='FDB_VALIDATION');
      assert.deepEqual(await db.all(sql,params),rows);
      const profile = await db.profileSelect(sql,params);
      assert.deepEqual(profile.result.rows,rows);
      await assert.rejects(async()=>db.selectWithLimits(sql,{maxRows:0n,maxPayloadBytes:10000n},params),e=>e.code==='FDB_LIMIT');
      await db.checkCollectionIntegrity('articles');
      const [info] = await db.exactlyOne('INFO FOR INDEX articles_text');
      assert.equal(info.kind,'fulltext');
      assert.deepEqual(info.paths,[['title'],['body']]);
      await db.execute('DROP INDEX articles_text');
      await assert.rejects(async()=>db.all(sql,params),e=>e.code==='FDB_NOT_FOUND');
    } finally { await db.close(); }
  }
});

test('V2 ANN search preserves typed ranked hits, rollback and bounds in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [() => new Database(), () => AsyncDatabase.open(':memory:')]) {
    const db = await open();
    try {
      await db.execute("INSERT INTO items {id:items:b,v:vector32('[1,0]')}");
      await db.execute("INSERT INTO items {id:items:a,v:vector32('[1,0]')}");
      await db.execute("CREATE SEARCH INDEX embeddings ON items(v) USING VECTOR WITH (dimensions=2,metric='cosine')");
      const sql="SELECT id,distance FROM search::vector('embeddings',vector32($query),$limit) ORDER BY distance,id";
      const params={$query:'[1,0]',$limit:1};
      const rows=await db.all(sql,params);
      assert.deepEqual(rows,[[new Record('items','a'),0]]);
      await db.execute('BEGIN');await db.execute('DELETE FROM items WHERE id=items:a');
      assert.deepEqual((await db.all(sql,params))[0][0],new Record('items','b'));
      await db.execute('ROLLBACK');assert.deepEqual(await db.all(sql,params),rows);
      await assert.rejects(async()=>db.execute("UPDATE items {v:vector32('[0,0]')}"),e=>e.code==='FDB_VALIDATION');
      assert.deepEqual((await db.profileSelect(sql,params)).result.rows,rows);
      await assert.rejects(async()=>db.selectWithLimits(sql,{maxRows:0n,maxPayloadBytes:10000n},params),e=>e.code==='FDB_LIMIT');
      await db.checkCollectionIntegrity('items');
      const [info]=await db.exactlyOne('INFO FOR INDEX embeddings');
      assert.equal(info.kind,'vector');assert.equal(info.metric,'cosine');assert.equal(info.dimensions,2n);
      await db.execute('DROP INDEX embeddings');
      await assert.rejects(async()=>db.all(sql,params),e=>e.code==='FDB_NOT_FOUND');
    } finally {await db.close();}
  }
});

test('V2 JavaScript functions persist typed values and roll back failed writes in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'romedb-udf-'));
  try {
    for (const asynchronous of [false, true]) {
      const file = path.join(dir, asynchronous ? 'async.db' : 'sync.db');
      const open = () => asynchronous ? AsyncDatabase.open(file) : new Database(file);
      let db = await open();
      try {
        await db.execute("CREATE FUNCTION app::identity(v any) RETURNS any LANGUAGE JAVASCRIPT AS 'return v;'");
        const value = { large: 9223372036854775807n, nested: [true, null, 1.5, 'text'] };
        assert.deepEqual(await db.exactlyOne('SELECT app::identity($v)', { $v:value }), [value]);
        await db.execute("CREATE FUNCTION app::normalize(v string) RETURNS string LANGUAGE JAVASCRIPT AS 'return v.trim().toLowerCase();'");
        await db.execute("INSERT INTO docs {name:app::normalize(' ALICE '),n:1}");
        await db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
        await db.execute("CREATE FUNCTION app::late(n integer) RETURNS integer LANGUAGE JAVASCRIPT AS 'if(n===2n)throw Error(\"late\");return n+10n;'");
        await db.execute('BEGIN');
        await db.execute('INSERT INTO docs {n:2}');
        await assert.rejects(async () => db.execute('UPDATE docs SET n=app::late(n)'), error => {
          assert.equal(error.code, 'FDB_VALIDATION');
          assert.deepEqual(error.transaction, { before:'active', after:'active' });
          return true;
        });
        assert.deepEqual(await db.all('SELECT n FROM docs ORDER BY n'), [[1n], [2n]]);
        await db.checkCollectionIntegrity('docs');
        await db.execute('ROLLBACK');
        const info = (await db.exactlyOne('INFO FOR FUNCTION app::normalize'))[0];
        assert.match(info.digest, /^[0-9a-f]{64}$/);
        await db.close();
        db = await open();
        assert.deepEqual(await db.exactlyOne("SELECT app::normalize(' BOB ')"), ['bob']);
        await db.execute('DROP FUNCTION app::normalize');
        await assert.rejects(async () => db.execute("SELECT app::normalize('x')"), { code:'FDB_NOT_FOUND' });
      } finally { await db.close(); }
    }
  } finally { fs.rmSync(dir, { recursive:true, force:true }); }
});

test('V2 JavaScript worker cancellation preserves prior transaction work', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const db = await AsyncDatabase.open();
  try {
    await db.execute("CREATE FUNCTION app::runaway() RETURNS any LANGUAGE JAVASCRIPT AS 'while(true){}'");
    await db.execute('CREATE TABLE docs');
    await db.execute('BEGIN');
    await db.execute('INSERT INTO docs {n:1}');
    await assert.rejects(db.execute('SELECT app::runaway()', {}, { timeoutMs:5 }), error => {
      assert.equal(error.code, 'FDB_CANCELLED');
      assert.deepEqual(error.transaction, { before:'active', after:'active' });
      return true;
    });
    assert.deepEqual(await db.all('SELECT n FROM docs'), [[1n]]);
    await db.execute('ROLLBACK');
  } finally { await db.close(); }
});

test('nested wildcard and signed array paths preserve embedded objects in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      const first = {title:'First', n:9223372036854775807n, bytes:Buffer.from([0,255]), ref:new Record('users','a')};
      const last = {title:'Last', meta:{active:true}};
      await db.execute('INSERT INTO users {id:users:a,post:$first,posts:$posts}', {$first:first,$posts:[first,last]});
      assert.deepEqual(await db.all('SELECT post.* FROM users'), [[first]]);
      const result = await db.execute('SELECT posts.*.*,posts.0.*,posts.-1.* FROM users');
      assert.deepEqual(result.columns,['posts','posts','posts']);
      assert.deepEqual(result.rows, [[[first,last],first,last]]);
      assert.deepEqual(await db.all('SELECT u.posts.*.title AS titles FROM users u'), [[['First','Last']]]);
      assert.deepEqual(await db.all('SELECT posts.2.*,posts.-3.*,unknown.* FROM users'), [[null,null,null]]);
      assert.deepEqual(await db.all('SELECT post.* FROM users post'), await db.all('SELECT * FROM users'));
    } finally { await db.close(); }
  }
});

test('linked projection wildcards fetch records while indexes retain typed IDs', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      await db.execute("INSERT INTO authors {id:authors:a,name:'Alice'}");
      await db.execute("INSERT INTO posts {id:posts:p1,title:'First',author:authors:a}");
      await db.execute("INSERT INTO posts {id:posts:p2,title:'Last',author:authors:a}");
      await db.execute('INSERT INTO users {id:users:u,posts:[posts:p1,posts:p2]}');
      const ids = [new Record('posts','p1'),new Record('posts','p2')];
      const first = {id:ids[0],title:'First',author:new Record('authors','a')};
      const last = {id:ids[1],title:'Last',author:new Record('authors','a')};
      assert.deepEqual(await db.all('SELECT posts,posts[0],posts.0,posts[-1],posts[$] FROM users'), [[ids,ids[0],ids[0],ids[1],ids[1]]]);
      assert.deepEqual(await db.all('SELECT posts.*,posts.*.*,posts[0].*,posts[-1].* FROM users'), [[[first,last],[first,last],first,last]]);
      assert.deepEqual(await db.all('SELECT posts.*.author.*.name FROM users'), [[['Alice','Alice']]]);
      assert.deepEqual(await db.all('SELECT posts[-3].*,posts[2].* FROM users'), [[null,null]]);
    } finally { await db.close(); }
  }
});

test('Turso 0.8.1 concurrent FTS and SQL plans work in both native clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const open of [file => new Database(file), file => AsyncDatabase.open(file)]) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'romedb-mvcc-'));
    let a, b;
    try {
      const file = path.join(dir, 'db');
      a = await open(file);
      await a.execute('PRAGMA journal_mode=mvcc');
      await a.execute('CREATE TABLE articles');
      await a.execute('CREATE SEARCH INDEX articles_text ON articles(title) USING FULLTEXT');
      b = await open(file);
      await a.execute('BEGIN CONCURRENT');
      await b.execute('BEGIN CONCURRENT');
      await a.execute("INSERT INTO articles {id:articles:a,title:'parallel alpha'}");
      await b.execute("INSERT INTO articles {id:articles:b,title:'parallel beta'}");
      await Promise.all([a.execute('COMMIT'), b.execute('COMMIT')]);
      assert.equal((await a.all("SELECT id FROM search::text('articles_text','parallel',10)")).length, 2);
      assert.deepEqual(await a.all('WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<3) SELECT sum(x) FROM n'), [[6n]]);
      await a.execute('CREATE INDEX articles_title ON articles(title)');
      const plan = await a.all("EXPLAIN QUERY PLAN FORMAT=JSON SELECT title FROM articles WHERE title='parallel alpha'");
      assert.match(plan[0][0], /articles_title/);
      await a.checkCollectionIntegrity('articles');
    } finally {
      await b?.close();
      await a?.close();
      fs.rmSync(dir, { recursive: true, force: true });
    }
  }
});

test('array predicates and field selection preserve typed values in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      const selected = {n:9223372036854775807n,bytes:Buffer.from([0,255]),ref:new Record('refs','a'),nil:null};
      await db.execute('INSERT INTO docs {items:$items}', {$items:[{active:true,...selected},{active:false,n:1n},null]});
      const result = await db.execute('SELECT items[WHERE active=true AND n>$min].{n,bytes,ref,nil,missing} AS picked FROM docs', {$min:9007199254740992n});
      assert.deepEqual(result.columns,['picked']);
      assert.deepEqual(result.rows,[[[selected]]]);
      await db.execute('CREATE TABLE copies');
      await db.execute('INSERT INTO copies(payload) SELECT items[WHERE active=true].{n,bytes,ref,nil} FROM docs');
      assert.deepEqual(await db.all('SELECT payload FROM copies'), [[[selected]]]);
      await assert.rejects(async () => db.execute('SELECT items[WHERE n=$missing] FROM docs'), error => error.code === 'FDB_PARAMETER');
    } finally { await db.close(); }
  }
});

test('typed unnest expands arrays through SQL in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      const values = [9223372036854775807n,true,Buffer.from([0,255]),new Record('refs','a'),{n:2n},null];
      const result = await db.execute('SELECT u.key,u.value FROM array::unnest($items) u ORDER BY u.key', {$items:values});
      assert.deepEqual(result.columns,['key','value']);
      assert.deepEqual(result.rows,values.map((value,i) => [BigInt(i),value]));
      await db.execute('INSERT INTO docs {items:$items}', {$items:[1n,2n,2n]});
      assert.deepEqual(await db.all('SELECT u.value,count(*) FROM docs d CROSS JOIN array::unnest(d.items) u GROUP BY u.value ORDER BY u.value'), [[1n,1n],[2n,2n]]);
      assert.deepEqual(await db.all('SELECT * FROM array::unnest(NULL)'), []);
      await assert.rejects(async () => db.execute('SELECT count(*) FROM array::unnest($items)', {$items:Array(100001).fill(null)}), error => error.code === 'FDB_LIMIT');
      assert.deepEqual(await db.all('SELECT count(*) FROM docs'), [[1n]]);
    } finally { await db.close(); }
  }
});

test('field defaults and readonly validation preserve both client transactions', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      const data = {n:9223372036854775807n,bytes:Buffer.from([0,255]),ref:new Record('refs','a')};
      await db.execute('CREATE TABLE docs');
      await db.execute('DEFINE FIELD stamp ON docs TYPE integer REQUIRED DEFAULT 7 READONLY');
      await db.execute('DEFINE FIELD data ON docs TYPE object DEFAULT $data READONLY', {$data:data});
      assert.deepEqual(await db.all('INSERT INTO docs {id:docs:a} RETURNING stamp,data'), [[7n,data]]);
      await db.execute('BEGIN');
      await db.execute('INSERT INTO docs {id:docs:prior}');
      await assert.rejects(async () => db.execute('UPDATE docs:a PATCH [{op:\'replace\',path:\'/stamp\',value:8}]'), error => error.code === 'FDB_VALIDATION');
      await assert.rejects(async () => db.execute('UPDATE docs:a MERGE {data:{n:0}}'), error => error.code === 'FDB_VALIDATION');
      assert.deepEqual(await db.all('SELECT stamp,data FROM docs ORDER BY id'), [[7n,data],[7n,data]]);
      await db.execute('COMMIT');
      const info = (await db.all('INFO FOR TABLE docs'))[0][0];
      const stamp = info.fields.find(field => field.path[0] === 'stamp');
      assert.equal(stamp.has_default,true);
      assert.equal(stamp.readonly,true);
      assert.equal(stamp.default,7n);
    } finally { await db.close(); }
  }
});

test('strict schemas and typed arrays validate writes in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      for (const sql of [
        'CREATE TABLE docs',
        'DEFINE FIELD n ON docs TYPE integer REQUIRED DEFAULT 7',
        'DEFINE FIELD ints ON docs TYPE array<integer?>',
        'DEFINE FIELD refs ON docs TYPE array<record<refs>>',
        'DEFINE FIELD extra ON docs TYPE object FLEXIBLE',
        'DEFINE SCHEMA ON docs STRICT',
      ]) await db.execute(sql);
      const refs = [new Record('refs','a')];
      const ints = [9223372036854775807n,null];
      await db.execute('INSERT INTO docs {id:docs:a,ints:$ints,refs:$refs,extra:{anything:true}}', {$ints:ints,$refs:refs});
      assert.deepEqual(await db.all('SELECT n,ints,refs FROM docs'), [[7n,ints,refs]]);
      for (const sql of ["UPDATE docs:a {ints:['wrong']}", 'UPDATE docs:a {unknown:1}', 'UPDATE docs:a {refs:[other:a]}', 'REMOVE FIELD ints ON docs']) {
        await assert.rejects(async () => db.execute(sql), error => error.code === 'FDB_VALIDATION');
      }
      const info = (await db.all('INFO FOR TABLE docs'))[0][0];
      assert.equal(info.strict,true);
      const field = info.fields.find(field => field.path[0] === 'ints');
      assert.equal(field.element_type,'integer');
      assert.equal(field.element_nullable,true);
      await db.execute('DEFINE SCHEMA ON docs FLEXIBLE');
      await db.execute('UPDATE docs:a {unknown:1}');
      await assert.rejects(async () => db.execute('DEFINE SCHEMA ON docs STRICT'), error => error.code === 'FDB_VALIDATION');
    } finally { await db.close(); }
  }
});

test('stored calculations preserve typed results and atomic indexes in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      for (const sql of [
        'CREATE TABLE docs',
        'DEFINE FIELD source ON docs TYPE integer DEFAULT 7',
        'DEFINE FIELD calculated ON docs TYPE integer VALUE (source+1)',
        'DEFINE FIELD ref ON docs TYPE record<refs> VALUE (type::record(\'refs\',source))',
        'CREATE UNIQUE INDEX docs_calculated ON docs(calculated)',
      ]) await db.execute(sql);
      assert.deepEqual(await db.all('INSERT INTO docs {id:docs:a,calculated:999} RETURNING calculated,ref'), [[8n,new Record('refs',7n)]]);
      await db.execute('BEGIN');
      await db.execute('INSERT INTO docs {id:docs:b,source:9}');
      await assert.rejects(async () => db.execute('UPDATE docs SET source=7'), error => error.code === 'FDB_CONSTRAINT');
      assert.deepEqual(await db.all('SELECT calculated FROM docs ORDER BY id'), [[8n],[10n]]);
      assert.deepEqual(await db.all('UPDATE docs:a PATCH [{op:\'replace\',path:\'/source\',value:10}] RETURNING calculated,ref'), [[11n,new Record('refs',10n)]]);
      await db.execute('COMMIT');
      await assert.rejects(async () => db.execute('DEFINE FIELD bad ON docs TYPE integer VALUE (bad+1)'), error => error.code === 'FDB_VALIDATION');
      const field=(await db.all('INFO FOR TABLE docs'))[0][0].fields.find(field => field.path[0] === 'calculated');
      assert.equal(field.computed,'source+1');
      await db.checkCollectionIntegrity('docs');
    } finally { await db.close(); }
  }
});

test('analyzer inspection and managed rebuilds preserve typed results in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      await db.execute("INSERT INTO docs {id:docs:a,body:'Database',n:7}");
      await db.execute("CREATE SEARCH INDEX words ON docs(body) USING FULLTEXT WITH(tokenizer='ngram',min_gram=2,max_gram=2)");
      await db.execute('CREATE INDEX docs_n ON docs(n)');
      const tokens=(await db.all("SELECT search::analyze('words','Café')"))[0][0];
      assert.deepEqual(tokens.map(token=>token.text),['ca','af','fé']);
      assert.equal(tokens[2].offset_to,5n);
      const before=await db.all("SELECT id FROM search::text('words','data',10)");
      assert.deepEqual(before,[[new Record('docs','a')]]);
      await db.execute('BEGIN');
      await db.execute('REINDEX words'); await db.execute('REINDEX docs_n');
      await db.execute('ROLLBACK');
      await db.execute('REINDEX words'); await db.execute('REINDEX docs_n');
      assert.deepEqual(await db.all("SELECT id FROM search::text('words','data',10)"),before);
      assert.deepEqual(await db.all('SELECT id FROM docs WHERE n=7'),before);
      await assert.rejects(async()=>db.all("SELECT search::analyze('words',7)"),error=>error.code==='FDB_VALIDATION');
      await db.checkCollectionIntegrity('docs');
    } finally { await db.close(); }
  }
});

test('mutation snapshots and diffs preserve typed values in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const async of [false,true]) {
    const db=async ? await AsyncDatabase.open(':memory:') : new Database();
    try {
      const id=new Record('docs','a');
      const data={n:9223372036854775807n,bytes:Buffer.from([0,255]),ref:new Record('refs','a'),items:[true,null],vector:Vector.float32([1,2])};
      const inserted=await db.execute('INSERT INTO docs {id:docs:a,data:$data} RETURNING doc::before() AS old,doc::after() AS new,doc::diff() AS diff',{$data:data});
      assert.deepEqual(inserted.rows,[[null,{id,data},[{op:'add',path:'',value:{id,data}}]]]);
      await db.execute('BEGIN');
      const updated=await db.execute('UPDATE docs:a MERGE {data:{n:7}} RETURNING doc::before() AS old,doc::after() AS new,doc::diff() AS diff,$before,$after,$diff',{$before:1n,$after:true,$diff:null});
      assert.deepEqual(updated.rows,[[{id,data},{id,data:{...data,n:7n}},[{op:'replace',path:'/data/n',value:7n}],1n,true,null]]);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('DELETE FROM docs:a RETURNING doc::before() AS old,doc::after() AS new,doc::diff() AS diff'),[[{id,data},null,[{op:'remove',path:''}]]]);
    } finally { await db.close(); }
  }
});

test('statement TIMEOUT composes with client cancellation and preserves transactions', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  const long='WITH RECURSIVE n(v) AS (VALUES(1) UNION ALL SELECT v+1 FROM n WHERE v<100000000) SELECT sum(v) FROM n';
  for (const async of [false,true]) {
    const db=async ? await AsyncDatabase.open(':memory:') : new Database();
    try {
      await db.execute('INSERT INTO docs {id:docs:a,n:1} TIMEOUT 1s');
      await db.execute('BEGIN');
      await db.execute('INSERT INTO docs {id:docs:b,n:2}');
      await assert.rejects(async()=>db.execute('UPDATE docs SET n=10 RETURNING doc::diff() TIMEOUT 0ms'),error=>error.code==='FDB_CANCELLED');
      await assert.rejects(async()=>db.execute(`${long} TIMEOUT 10ms`),error=>error.code==='FDB_CANCELLED');
      assert.deepEqual(await db.all('SELECT n FROM docs ORDER BY n TIMEOUT 1s'),[[1n],[2n]]);
      await db.execute('ROLLBACK');
      assert.deepEqual(await db.all('SELECT n FROM docs'),[[1n]]);
      if (db instanceof AsyncDatabase) {
        const controller=new AbortController();
        const pending=db.execute(`${long} TIMEOUT 5s`,{}, {signal:controller.signal});
        const timer=setTimeout(()=>controller.abort(),25);
        try { await assert.rejects(pending,error=>error.code==='FDB_CANCELLED'); }
        finally { clearTimeout(timer); }
        assert.deepEqual(await db.all('SELECT 42'),[[42n]]);
      }
    } finally { await db.close(); }
  }
});

test('candidate filters and compressed vectors keep typed results in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const async of [false,true]) {
    const db=async ? await AsyncDatabase.open(':memory:') : new Database();
    try {
      await db.execute('BEGIN');
      for(let n=0;n<32;n++) await db.execute("INSERT INTO docs {id:$id,body:'alpha',v:$v}",{$id:new Record('docs',BigInt(n)),$v:Vector.float32([n,0])});
      await db.execute('CREATE SEARCH INDEX words ON docs(body) USING FULLTEXT');
      await db.execute("CREATE SEARCH INDEX vectors ON docs(v) USING VECTOR WITH(dimensions=2,metric='l2',quantization='f16')");
      await db.execute('COMMIT');
      const expected=[[new Record('docs',31n)]];
      const params={$ids:[new Record('DOCS',31n),new Record('docs',31n),new Record('docs','missing')]};
      const sources=["search::text('words','alpha',1,$ids)","search::vector('vectors',vector32('[0,0]'),1,$ids)"];
      for(const source of sources) {
        assert.deepEqual(await db.all(`SELECT id FROM ${source}`,params),expected);
        assert.deepEqual(await db.all(`SELECT id FROM ${source}`,{$ids:[]}),[]);
        await assert.rejects(async()=>db.all(`SELECT id FROM ${source}`,{$ids:[new Record('other',31n)]}),error=>error.code==='FDB_VALIDATION');
        await assert.rejects(async()=>db.all(`SELECT id FROM ${source} TIMEOUT 0ms`,params),error=>error.code==='FDB_CANCELLED');
      }
      assert.deepEqual(await db.all("SELECT id FROM search::vector('vectors',vector32('[0,0]'),1) WHERE id=type::record('docs',31)"),[]);
      await db.execute('BEGIN');await db.execute('DELETE FROM docs WHERE id=type::record(\'docs\',31)');
      await db.execute('REINDEX vectors');await db.execute('REINDEX words');await db.execute('ROLLBACK');
      for(const source of sources) assert.deepEqual(await db.all(`SELECT id FROM ${source}`,params),expected);
      await assert.rejects(async()=>db.execute("UPDATE docs SET v=vector32('[65505,0]')"),error=>error.code==='FDB_VALIDATION');
      assert.deepEqual(await db.all('SELECT v FROM docs WHERE id=type::record(\'docs\',31)'),[[Vector.float32([31,0])]]);
      await db.checkCollectionIntegrity('docs');
    } finally { await db.close(); }
  }
});

test('compound and array indexes support atomic conflict updates in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      await db.execute("INSERT INTO docs {id:docs:a,tenant:1,email:'one',n:2,tags:[true,1],body:'before',v:vector32('[1,0]')}");
      await db.execute('CREATE UNIQUE INDEX address ON docs(tenant,email)');
      await db.execute('CREATE SEARCH INDEX tags ON docs(tags) USING ARRAY');
      await db.execute('CREATE SEARCH INDEX words ON docs(body) USING FULLTEXT');
      await db.execute("CREATE SEARCH INDEX vectors ON docs(v) USING VECTOR WITH(dimensions=2,metric='l2')");
      const tags=[false,2n,Buffer.from([0,255]),new Record('docs','link')];
      const result=await db.execute("INSERT INTO docs(id,tenant,email,n,tags,body,v) VALUES(docs:b,1,'one',3,$tags,'after',$v) ON CONFLICT(tenant,email) DO UPDATE SET n=n+excluded.n,tags=excluded.tags,body=excluded.body,v=excluded.v RETURNING id,n,tags,doc::before(),doc::after()",{$tags:tags,$v:Vector.float32([2,0])});
      assert.equal(result.affected,1n);
      assert.deepEqual(result.rows[0].slice(0,3),[new Record('docs','a'),5n,tags]);
      assert.equal(result.rows[0][3].n,2n);
      assert.equal(result.rows[0][4].n,5n);
      for(const needle of tags) {
        const params={$needle:needle};
        assert.deepEqual(await db.all('SELECT id FROM docs WHERE array::contains(tags,$needle)',params),[[new Record('docs','a')]]);
        assert.deepEqual(await db.all('SELECT id FROM docs NOT INDEXED WHERE array::contains(tags,$needle)',params),[[new Record('docs','a')]]);
      }
      const plan=await db.all('EXPLAIN QUERY PLAN SELECT id FROM docs WHERE array::contains(tags,$needle)',{$needle:2n});
      assert.ok(plan.flat().some(value=>typeof value==='string' && value.includes('SEARCH') && value.includes('tags')));
      assert.deepEqual(await db.all("SELECT id FROM search::text('words','after',1)"),[[new Record('docs','a')]]);
      assert.deepEqual(await db.all("SELECT id FROM search::vector('vectors',vector32('[2,0]'),1)"),[[new Record('docs','a')]]);
      assert.deepEqual((await db.execute("INSERT INTO docs(tenant,email) VALUES(1,'one') ON CONFLICT DO NOTHING RETURNING *")).rows,[]);
      await db.execute('BEGIN');
      await db.execute("UPDATE docs:a MERGE {pending:true}");
      const before=await db.collection('docs').get('a');
      await assert.rejects(async()=>db.execute("INSERT INTO docs(tenant,email,tags) VALUES(1,'one',$bad) ON CONFLICT(tenant,email) DO UPDATE SET tags=excluded.tags",{$bad:[{invalid:true}]}),error=>error.code==='FDB_VALIDATION');
      assert.deepEqual(await db.collection('docs').get('a'),before);
      await db.execute('REINDEX address');await db.execute('REINDEX tags');
      await db.checkCollectionIntegrity('docs');
      await db.execute('ROLLBACK');
    } finally { await db.close(); }
  }
});

test('composite sets, row membership and collection recursion preserve values in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      const value={items:[true,null,9223372036854775807n],ref:new Record('docs','b'),bytes:Buffer.from([0,255]),vector:Vector.float32([1,2])};
      await db.execute('INSERT INTO docs {id:docs:a,n:1,payload:$value}',{$value:value});
      await db.execute('INSERT INTO docs {id:docs:b,n:2,payload:$value}',{$value:value});
      assert.deepEqual(await db.all('SELECT DISTINCT payload FROM docs'),[[value]]);
      assert.deepEqual(await db.all('SELECT payload FROM docs UNION SELECT $value',{$value:value}),[[value]]);
      assert.deepEqual(await db.all('SELECT n FROM docs WHERE (n,payload) IN((1,$value))',{$value:value}),[[1n]]);
      assert.deepEqual(await db.all('SELECT n FROM docs WHERE (n,payload) IN(SELECT n,payload FROM docs WHERE n=2)'),[[2n]]);
      const recursive='WITH RECURSIVE r(n,payload) AS (SELECT n,payload FROM docs WHERE n=1 UNION ALL SELECT n+1,payload FROM r WHERE n<$end) SELECT n,payload FROM r ORDER BY n';
      assert.deepEqual(await db.all(recursive,{$end:3n}),[[1n,value],[2n,value],[3n,value]]);
      await db.execute('BEGIN');
      await db.execute('UPDATE docs:a MERGE {prior:true}');
      await assert.rejects(async()=>db.all('WITH RECURSIVE r(n) AS (SELECT n FROM docs WHERE n=1 UNION ALL SELECT n+1 FROM r) SELECT sum(n) FROM r TIMEOUT 10ms'),e=>e.code==='FDB_CANCELLED');
      assert.equal((await db.collection('docs').get('a')).prior,true);
      await db.execute('ROLLBACK');
    } finally { await db.close(); }
  }
});
test('edge tables, RELATE and GRAPH sources keep typed records in both clients', async () => {
  const { AsyncDatabase } = require('./index.cjs');
  for (const db of [new Database(), await AsyncDatabase.open(':memory:')]) {
    try {
      for (const sql of [
        'CREATE TABLE people',
        'CREATE EDGE TABLE follows FROM people TO people',
        "INSERT INTO people:alice {name:'Alice'}",
        "INSERT INTO people:bob {name:'Bob'}",
        "INSERT INTO people:carol {name:'Carol'}",
      ]) await db.execute(sql);
      const edge = await db.exactlyOne('RELATE $s -> follows:ab -> $t {weight:$w} RETURNING id,source,target,weight', {$s:new Record('people','alice'),$t:new Record('people','bob'),$w:2n});
      assert.deepEqual(edge,[new Record('follows','ab'),new Record('people','alice'),new Record('people','bob'),2n]);
      await db.execute('RELATE people:bob -> follows:bc -> people:carol');
      await assert.rejects(async () => db.execute('RELATE people:bob -> follows -> people:nobody'), e=>e.code==='FDB_CONSTRAINT');
      await assert.rejects(async () => db.execute('DELETE FROM people WHERE id = people:bob'), e=>e.code==='FDB_CONSTRAINT');
      const sql = "SELECT g.node, g.depth, g.edges, p.name FROM GRAPH(type::record('people', $root) -> follows -> people DEPTH 1 TO 3 MODE REACHABLE WITH PATH) AS g JOIN people p ON p.id = g.node ORDER BY g.depth";
      assert.deepEqual(await db.all(sql, {$root:'alice'}), [
        [new Record('people','bob'),1n,[new Record('follows','ab')],'Bob'],
        [new Record('people','carol'),2n,[new Record('follows','ab'),new Record('follows','bc')],'Carol'],
      ]);
      const profile = await db.profileSelect(sql, {$root:'alice'});
      assert.equal(profile.metrics.graphRows, 2n);
      assert.equal(profile.metrics.graphAdjacencyEntries, 2n);
      assert.equal(profile.metrics.graphVisited, 3n);
      await assert.rejects(async () => db.execute('SELECT * FROM GRAPH(people:alice -> follows) g'), e=>e.code==='FDB_SYNTAX');
      const info = await db.exactlyOne('INFO FOR TABLE follows');
      assert.equal(info[0].kind, 'edge');
      assert.deepEqual(info[0].edge.source, 'people');
    } finally { await db.close(); }
  }
});
