'use strict';
const { parentPort, workerData } = require('node:worker_threads');
const { NativeDatabase } = require('./native.cjs');
const db = new NativeDatabase(workerData.path, ...(workerData.limits ?? []));
const methods = new Set(['execute', 'executeAtomicRequest', 'writeWithResultLimits', 'profileSelect', 'profileSelectWithLimits', 'checkCollectionIntegrity', 'executeBatch', 'migrate', 'exportDocuments', 'importDocuments', 'close']);
parentPort.on('message', ({ id, method, args, cancellationKey }) => {
  try {
    if (!methods.has(method)) throw new Error('unknown database worker operation');
    const result = method === 'execute' && cancellationKey !== undefined
      ? db.executeCancellable(...args, cancellationKey)
      : (method === 'executeAtomicRequest' || method === 'writeWithResultLimits' || method === 'profileSelect' || method === 'profileSelectWithLimits' || method === 'checkCollectionIntegrity' || method === 'executeBatch' || method === 'importDocuments' || method === 'exportDocuments' || method === 'migrate') && cancellationKey !== undefined
        ? db[method](...args, cancellationKey) : db[method](...args);
    parentPort.postMessage({ id, result });
    if (method === 'close') parentPort.close();
  } catch (error) {
    parentPort.postMessage({ id, error: { message: error.message } });
  }
});
parentPort.postMessage({ ready: true, interruptKey: db.interruptKey() });
