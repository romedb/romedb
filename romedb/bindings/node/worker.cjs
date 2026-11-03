'use strict';
const { parentPort, workerData } = require('node:worker_threads');
const { NativeDatabase, createCancellationToken, cancelOperation, releaseCancellationToken } = require('./native.cjs');
const db = new NativeDatabase(workerData.path, ...(workerData.limits ?? []));
const methods = new Set(['execute', 'executeAtomicRequest', 'writeWithResultLimits', 'profileSelect', 'profileSelectWithLimits', 'checkCollectionIntegrity', 'executeBatch', 'migrate', 'exportDocuments', 'importDocuments', 'exportFile', 'close']);
const copies = new Map();
parentPort.on('message', ({ id, method, args, cancellationKey }) => {
  try {
    if (!methods.has(method)) throw new Error('unknown database worker operation');
    if (method === 'exportFile') {
      const key = cancellationKey ?? createCancellationToken();
      const finished = Promise.resolve()
        .then(() => db.startFileExport(args[0], key).finish())
        .then(bytes => parentPort.postMessage({ id, result: { bytes: Number(bytes) } }), error => parentPort.postMessage({ id, error: { message: error.message } }))
        .finally(() => { copies.delete(key); if (cancellationKey === undefined) releaseCancellationToken(key); });
      copies.set(key, finished);
      return;
    }
    if (method === 'close') {
      for (const key of copies.keys()) cancelOperation(key);
      void Promise.allSettled(copies.values()).then(() => {
        db.close();
        parentPort.postMessage({ id, result: undefined });
        parentPort.close();
      });
      return;
    }
    const result = method === 'execute' && cancellationKey !== undefined
      ? db.executeCancellable(...args, cancellationKey)
      : (method === 'executeAtomicRequest' || method === 'writeWithResultLimits' || method === 'profileSelect' || method === 'profileSelectWithLimits' || method === 'checkCollectionIntegrity' || method === 'executeBatch' || method === 'importDocuments' || method === 'exportDocuments' || method === 'migrate') && cancellationKey !== undefined
        ? db[method](...args, cancellationKey) : db[method](...args);
    parentPort.postMessage({ id, result });
  } catch (error) {
    parentPort.postMessage({ id, error: { message: error.message } });
  }
});
parentPort.postMessage({ ready: true, interruptKey: db.interruptKey() });
