'use strict';
const { parentPort, workerData } = require('node:worker_threads');
const { createHash } = require('node:crypto');
const { NativeDatabase, cancelOperation } = require('./native.cjs');
const { validatePrepared } = require('./client.cjs');
let database;
let sequence = 0;
let fenced = false;
const receipts = new Map();
const copies = new Map();
try {
  database = new NativeDatabase(workerData.path, '10000', '67108864', workerData.create ? 'create-mvcc' : 'mvcc');
  const foreignKeys = JSON.parse(database.execute('PRAGMA foreign_keys=ON', '{}'));
  if (foreignKeys.execution.error) throw new Error(foreignKeys.execution.error.message);
  parentPort.postMessage({ ready: true });
} catch (error) {
  const coded = /^(RDB_[A-Z0-9_]+): (.*)$/s.exec(error.message);
  parentPort.postMessage({ readyError: { code: /file is locked/i.test(error.message) ? 'RDB_BUSY' : coded?.[1] ?? (error.code?.startsWith('RDB_') ? error.code : 'RDB_STORAGE'), message: coded?.[2] ?? error.message } });
  database?.close();
  parentPort.close();
}

if (database) parentPort.on('message', ({ id, operation, body, cancellationKey }) => {
  try {
    if (operation === 'close') {
      for (const key of copies.keys()) cancelOperation(key);
      void Promise.allSettled(copies.values()).then(() => {
        if (!fenced) try { database.execute('PRAGMA wal_checkpoint(TRUNCATE)', '{}'); } catch {}
        database.close();
        parentPort.postMessage({ id });
        parentPort.close();
      });
      return;
    }
    if (fenced) fail('RDB_STORAGE', 'Embedded outcome is uncertain; close and reconcile the database', 'unknown');
    if (operation === 'export') {
      // Writes run on this thread, so none commits between the start of the copy's snapshot and reading the sequence.
      const copy = JSON.parse(body);
      const pending = database.startFileExport(copy.destination, cancellationKey, copy.emptyTables, copy.newIdentity);
      const snapshot = sequence;
      const finished = pending.finish().then(
        bytes => parentPort.postMessage({ id, result: { sequence: snapshot, bytes: Number(bytes) } }),
        error => {
          const coded = /^(RDB_[A-Z0-9_]+): (.*)$/s.exec(error.message);
          parentPort.postMessage({ id, error: { code: coded?.[1] ?? 'RDB_STORAGE', message: (coded?.[2] ?? error.message).slice(0, 2048), outcome: 'read-failed' } });
        },
      ).finally(() => copies.delete(cancellationKey));
      copies.set(cancellationKey, finished);
      return;
    }
    if (operation === 'info') {
      const mode = JSON.parse(database.execute('PRAGMA journal_mode', '{}'));
      if (mode.execution.error) fail('RDB_STORAGE', 'Cannot inspect embedded journal mode', 'read-failed');
      const engine = JSON.parse(database.execute('SELECT romedb_version()', '{}'));
      if (engine.execution.error) fail('RDB_STORAGE', 'Cannot read the embedded engine version', 'read-failed');
      parentPort.postMessage({ id, result: {
        id: workerData.databaseId, sequence, adapterVersion: 1,
        engineVersion: engine.execution.result.rows[0][0].value,
        journalMode: mode.execution.result.rows[0][0].value,
        capabilities: { readVersion: 2, atomicWrites: true, replayWindow: 64, exactSequenceCondition: true, sequenceScope: 'adapter', replayDurability: 'adapter-lifetime', metadata: true, checkedWork: true, foreignKeys: true },
        limits: { maxRequestBytes: 65536, maxStatements: 32, maxRows: 1000, maxResultBytes: 262144, activeRequests: 1, queuedRequests: 32, rowsRead: 1000000, rowMutations: 100000, vmSteps: 10000000 },
      } });
      return;
    }
    if (!['read', 'write'].includes(operation)) fail('RDB_UNSUPPORTED', 'Unknown embedded operation', 'not-sent');
    const reading = operation === 'read';
    const request = JSON.parse(body);
    let writeBody = body;
    if (reading) {
      if (request.readVersion !== 2 || request.ifSequence !== undefined) fail('RDB_VALIDATION', 'Invalid fresh read envelope', 'read-failed');
      const { readVersion, ...rest } = request;
      writeBody = JSON.stringify(rest);
    }
    validatePrepared({ version: 1, url: workerData.url, databaseId: workerData.databaseId, body: writeBody });
    const hash = createHash('sha256').update(body).digest('hex');
    if (!reading && receipts.has(request.requestId)) {
      const receipt = receipts.get(request.requestId);
      if (receipt.hash !== hash) fail('RDB_REQUEST_IDENTITY', 'Request ID was reused with another body', 'unknown');
      parentPort.postMessage({ id, result: receipt.result });
      return;
    }
    if (request.afterSequence > sequence || (!reading && sequence - request.afterSequence >= 64)) fail('RDB_CONFLICT', 'Request position is ahead or its replay window expired; reconcile the request', reading ? 'read-failed' : 'unknown');
    if (sequence >= Number.MAX_SAFE_INTEGER - 1 && !reading) fail('RDB_LIMIT', 'Adapter sequence is exhausted', 'not-sent');
    let result;
    if (!reading && request.ifSequence !== undefined && request.ifSequence !== sequence) {
      result = { error: { code: 'RDB_CONFLICT', message: 'Sequence changed; refresh before saving' } };
    } else {
      const report = JSON.parse(database.executeAtomicRequest(JSON.stringify({ readOnly: reading, statements: request.statements, ...(request.workLimits === undefined ? {} : { workLimits: request.workLimits }) }), cancellationKey));
      if (report.execution.error) {
        const error = report.execution.error;
        if (['RDB_STORAGE', 'RDB_ROLLBACK'].includes(error.code) || report.transaction.after !== 'autocommit') {
          fenced = true;
          fail('RDB_STORAGE', 'Atomic request outcome is uncertain; close and reconcile the database', 'unknown');
        }
        if (reading) fail(error.code, error.message, 'read-failed');
        result = { error };
      } else result = report.execution.result;
    }
    if (reading) result = { ...result, readVersion: 2, requestId: request.requestId, sequence };
    else {
      result = { ...result, requestId: request.requestId, sequence: ++sequence };
      receipts.set(request.requestId, { hash, result });
      if (receipts.size > 64) receipts.delete(receipts.keys().next().value);
    }
    parentPort.postMessage({ id, result });
  } catch (error) {
    const coded = operation === 'export' ? /^(RDB_[A-Z0-9_]+): (.*)$/s.exec(error.message) : null;
    parentPort.postMessage({ id, error: { code: coded?.[1] ?? error.code ?? 'RDB_VALIDATION', message: String(coded?.[2] ?? error.message).slice(0, 2048), outcome: error.outcome ?? (operation === 'read' || operation === 'export' ? 'read-failed' : 'unknown') } });
  }
});

function fail(code, message, outcome) {
  const error = new Error(message);
  Object.assign(error, { code, outcome });
  throw error;
}
