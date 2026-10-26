'use strict';
const { parentPort, workerData } = require('node:worker_threads');
const { createHash } = require('node:crypto');
const { NativeDatabase } = require('./native.cjs');
const { validatePrepared } = require('./client.cjs');
let database;
let sequence = 0;
let fenced = false;
const receipts = new Map();
try {
  database = new NativeDatabase(workerData.path, '10000', '67108864');
  const foreignKeys = JSON.parse(database.execute('PRAGMA foreign_keys=ON', '{}'));
  if (foreignKeys.execution.error) throw new Error(foreignKeys.execution.error.message);
  parentPort.postMessage({ ready: true });
} catch (error) {
  parentPort.postMessage({ readyError: { code: /file is locked/i.test(error.message) ? 'FDB_BUSY' : error.code?.startsWith('FDB_') ? error.code : 'FDB_STORAGE', message: error.message } });
  database?.close();
  parentPort.close();
}

if (database) parentPort.on('message', ({ id, operation, body, cancellationKey }) => {
  try {
    if (operation === 'close') {
      database.close();
      parentPort.postMessage({ id });
      parentPort.close();
      return;
    }
    if (fenced) fail('FDB_STORAGE', 'Embedded outcome is uncertain; close and reconcile the database', 'unknown');
    if (operation === 'info') {
      const mode = JSON.parse(database.execute('PRAGMA journal_mode', '{}'));
      if (mode.execution.error) fail('FDB_STORAGE', 'Cannot inspect embedded journal mode', 'read-failed');
      const engine = JSON.parse(database.execute('SELECT romedb_version()', '{}'));
      if (engine.execution.error) fail('FDB_STORAGE', 'Cannot read the embedded engine version', 'read-failed');
      parentPort.postMessage({ id, result: {
        id: workerData.databaseId, sequence, adapterVersion: 1,
        engineVersion: engine.execution.result.rows[0][0].value,
        journalMode: mode.execution.result.rows[0][0].value,
        capabilities: { readVersion: 2, atomicWrites: true, replayWindow: 64, exactSequenceCondition: true, sequenceScope: 'adapter', replayDurability: 'adapter-lifetime', metadata: true, checkedWork: true, foreignKeys: true },
        limits: { maxRequestBytes: 65536, maxStatements: 32, maxRows: 1000, maxResultBytes: 262144, activeRequests: 1, queuedRequests: 32, rowsRead: 1000000, rowMutations: 100000, vmSteps: 10000000 },
      } });
      return;
    }
    if (!['read', 'write'].includes(operation)) fail('FDB_UNSUPPORTED', 'Unknown embedded operation', 'not-sent');
    const reading = operation === 'read';
    const request = JSON.parse(body);
    let writeBody = body;
    if (reading) {
      if (request.readVersion !== 2 || request.ifSequence !== undefined) fail('FDB_VALIDATION', 'Invalid fresh read envelope', 'read-failed');
      const { readVersion, ...rest } = request;
      writeBody = JSON.stringify(rest);
    }
    validatePrepared({ version: 1, url: workerData.url, databaseId: workerData.databaseId, body: writeBody });
    const hash = createHash('sha256').update(body).digest('hex');
    if (!reading && receipts.has(request.requestId)) {
      const receipt = receipts.get(request.requestId);
      if (receipt.hash !== hash) fail('FDB_REQUEST_IDENTITY', 'Request ID was reused with another body', 'unknown');
      parentPort.postMessage({ id, result: receipt.result });
      return;
    }
    if (request.afterSequence > sequence || (!reading && sequence - request.afterSequence >= 64)) fail('FDB_CONFLICT', 'Request position is ahead or its replay window expired; reconcile the request', reading ? 'read-failed' : 'unknown');
    if (sequence >= Number.MAX_SAFE_INTEGER - 1 && !reading) fail('FDB_LIMIT', 'Adapter sequence is exhausted', 'not-sent');
    let result;
    if (!reading && request.ifSequence !== undefined && request.ifSequence !== sequence) {
      result = { error: { code: 'FDB_CONFLICT', message: 'Sequence changed; refresh before saving' } };
    } else {
      const report = JSON.parse(database.executeAtomicRequest(JSON.stringify({ readOnly: reading, statements: request.statements, ...(request.workLimits === undefined ? {} : { workLimits: request.workLimits }) }), cancellationKey));
      if (report.execution.error) {
        const error = report.execution.error;
        if (['FDB_STORAGE', 'FDB_ROLLBACK'].includes(error.code) || report.transaction.after !== 'autocommit') {
          fenced = true;
          fail('FDB_STORAGE', 'Atomic request outcome is uncertain; close and reconcile the database', 'unknown');
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
    parentPort.postMessage({ id, error: { code: error.code ?? 'FDB_VALIDATION', message: String(error.message).slice(0, 2048), outcome: error.outcome ?? (operation === 'read' ? 'read-failed' : 'unknown') } });
  }
});

function fail(code, message, outcome) {
  const error = new Error(message);
  Object.assign(error, { code, outcome });
  throw error;
}
