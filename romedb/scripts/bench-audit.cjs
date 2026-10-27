'use strict';
// Maintainer-run diagnostic; requires the local Node addon built by check-node.sh.
const { performance } = require('node:perf_hooks');
const { Database } = require('../bindings/node/index.cjs');
const counts = process.argv.length > 2 ? process.argv.slice(2).map(Number) : [100, 300, 1000];
if (counts.some(n => !Number.isSafeInteger(n) || n < 1 || n > 10000)) {
  throw new RangeError('row counts must be integers from 1 through 10000');
}
for (const count of counts) {
  const db = new Database();
  try {
    db.execute('CREATE TABLE docs');
    db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
    const sql = 'INSERT INTO docs(n) VALUES ' + Array.from({length: count}, (_, n) => `(${n})`).join(',');
    const start = performance.now();
    db.execute(sql);
    const inserted = performance.now();
    console.log(JSON.stringify({count, phase: 'insert_complete', insertMs: inserted - start}));
    const auditStart = performance.now();
    const report = db.checkCollectionIntegrity('docs');
    const auditMs = performance.now() - auditStart;
    if (report.documents !== BigInt(count) || report.indexEntries !== BigInt(count)) {
      throw new Error('audit result mismatch');
    }
    console.log(JSON.stringify({count, phase: 'audit_complete', auditMs, documents: String(report.documents)}));
  } finally { db.close(); }
}
