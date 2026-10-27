'use strict';

const assert = require('node:assert/strict');
const { fork } = require('node:child_process');
const { once } = require('node:events');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const packagePath = process.env.ROMEDB_OWNERSHIP_PACKAGE;
if (packagePath && !path.isAbsolute(packagePath)) throw new Error('ROMEDB_OWNERSHIP_PACKAGE must be absolute');
const { Database, AsyncDatabase } = require(packagePath || './index.cjs');

const open = (api, file) => api === 'sync' ? new Database(file) : AsyncDatabase.open(file);

async function seed(db) {
  await db.execute('PRAGMA synchronous=FULL');
  await db.execute('CREATE TABLE ledger (id INTEGER PRIMARY KEY, label TEXT NOT NULL)');
  await db.execute('CREATE TABLE docs');
  await db.execute('CREATE UNIQUE INDEX docs_label ON docs(label)');
  await db.execute('BEGIN');
  await db.execute("INSERT INTO ledger VALUES (1, 'committed')");
  await db.execute("INSERT INTO docs {id:docs:committed,label:'committed'}");
  await db.execute('COMMIT');
}

async function pending(db) {
  await db.execute('BEGIN');
  await db.execute("INSERT INTO ledger VALUES (2, 'pending')");
  await db.execute("INSERT INTO docs {id:docs:pending,label:'pending'}");
}

async function verify(db, labels) {
  assert.deepEqual(await db.all('SELECT label FROM ledger ORDER BY label'), labels.map(x => [x]));
  assert.deepEqual(await db.all('SELECT label FROM docs ORDER BY label'), labels.map(x => [x]));
  assert.deepEqual(await db.all('PRAGMA integrity_check'), [['ok']]);
  const audit = await db.checkCollectionIntegrity('docs');
  assert.equal(audit.documents, BigInt(labels.length));
  assert.equal(audit.indexEntries, BigInt(labels.length));
}

async function childMain(api, file, role) {
  let db;
  try {
    db = await open(api, file);
  } catch (error) {
    process.send({ kind: 'open-error', code: error.code, message: error.message });
    process.disconnect();
    return;
  }
  if (role === 'owner') {
    await seed(db);
    await pending(db);
    // Keep both the native handle and the IPC channel alive until SIGKILL.
    process.on('message', () => {});
    process.send({ kind: 'ready' });
    return;
  }
  try {
    await verify(db, ['committed', 'pending']);
    await db.execute("INSERT INTO ledger VALUES (3, 'reopened')");
    await db.execute("INSERT INTO docs {id:docs:reopened,label:'reopened'}");
  } finally {
    await db.close();
  }
  process.send({ kind: 'reopened' });
  process.disconnect();
}

function child(api, file, role) {
  const process = fork(__filename, ['--ownership-child', api, file, role], {
    execArgv: [], stdio: ['ignore', 'ignore', 'pipe', 'ipc'],
  });
  let stderr = '';
  process.stderr.on('data', chunk => { stderr += chunk; });
  const exited = once(process, 'exit');
  // Attach handlers immediately so an early exit cannot leave a test waiting.
  const message = new Promise((resolve, reject) => {
    process.once('message', resolve);
    process.once('error', reject);
    process.once('exit', (code, signal) => reject(new Error(`child exited ${code}/${signal}: ${stderr}`)));
  });
  message.catch(() => {});
  exited.catch(() => {});
  return {
    process, message, exited,
    async stop() {
      if (process.exitCode === null && process.signalCode === null) process.kill('SIGKILL');
      await exited;
    },
  };
}

async function rejected(t, api, file) {
  const contender = child(api, file, 'probe');
  t.after(() => contender.stop());
  const result = await contender.message;
  assert.equal(result.kind, 'open-error', JSON.stringify(result));
  assert.match(result.message, /[Ff]ile is locked by another process/);
  // Open failures are not query FDB_BUSY errors. Async open wraps worker startup.
  assert.equal(result.code, api === 'sync' ? 'GenericFailure' : 'FDB_WORKER');
  assert.deepEqual(await contender.exited, [0, null]);
}

if (process.argv[2] === '--ownership-child') {
  childMain(...process.argv.slice(3)).catch(error => {
    console.error(error);
    // A failed async child can still own a worker; terminate the test fixture.
    process.exit(1);
  });
} else {
  const { test } = require('node:test');
  for (const api of ['sync', 'async']) {
    test(`${api} owner excludes both other-process clients until its last handle closes`, {
      timeout: 20000, skip: process.platform !== 'linux',
    }, async t => {
      const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'romedb-owner-'));
      t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
      const file = path.join(directory, 'app.db');
      let owner, peer;
      try {
        owner = await open(api, file);
        await seed(owner);
        peer = await open(api, file);
        await pending(owner);
        for (const otherApi of ['sync', 'async']) await rejected(t, otherApi, file);
        // A rejected process must not disturb the owner's active transaction.
        await verify(owner, ['committed', 'pending']);
        await owner.execute('COMMIT');
        await owner.close(); owner = undefined;
        for (const otherApi of ['sync', 'async']) await rejected(t, otherApi, file);
        await verify(peer, ['committed', 'pending']);
        await peer.close(); peer = undefined;
        const successor = child(api, file, 'probe');
        t.after(() => successor.stop());
        assert.deepEqual(await successor.message, { kind: 'reopened' });
        assert.deepEqual(await successor.exited, [0, null]);
        owner = await open(api, file);
        await verify(owner, ['committed', 'pending', 'reopened']);
      } finally {
        await peer?.close();
        await owner?.close();
      }
    });

    test(`${api} owner death releases the lock and preserves committed data for both clients`, {
      timeout: 20000, skip: process.platform !== 'linux',
    }, async t => {
      const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'romedb-owner-exit-'));
      t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
      const file = path.join(directory, 'app.db');
      const owner = child(api, file, 'owner');
      t.after(() => owner.stop());
      assert.deepEqual(await owner.message, { kind: 'ready' });
      for (const otherApi of ['sync', 'async']) await rejected(t, otherApi, file);
      await owner.stop();
      assert.deepEqual(await owner.exited, [null, 'SIGKILL']);
      for (const otherApi of ['sync', 'async']) {
        const db = await open(otherApi, file);
        try {
          await verify(db, ['committed']);
          await pending(db);
          await db.execute('ROLLBACK');
          await verify(db, ['committed']);
          // A post-recovery committed write must survive a separate reopen.
          await db.execute("UPDATE docs:committed {recovered:true}");
        } finally { await db.close(); }
        const reopened = await open(otherApi, file);
        try {
          assert.deepEqual(await reopened.all('SELECT recovered FROM docs'), [[true]]);
          await verify(reopened, ['committed']);
        } finally { await reopened.close(); }
      }
    });
  }
}
