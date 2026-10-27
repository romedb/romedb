#!/usr/bin/env node
'use strict';
// Bounded graph workload against an installed @romedb/node package. Records
// timings and graph work counters; this is a sample, not a capacity claim.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { performance } = require('node:perf_hooks');
const [packageDir, output, nodesArg = '10000', degreeArg = '10'] = process.argv.slice(2);
assert(packageDir && output, 'Usage: bench-graph.cjs PACKAGE_DIR OUTPUT [NODES] [OUT_DEGREE]');
const { Database, Record } = require(packageDir);
const nodes = Number(nodesArg);
const degree = Number(degreeArg);
const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'romedb-graph-bench-'));
const file = path.join(directory, 'graph.db');
const db = new Database(file);
const percentile = (values, p) => [...values].sort((a, b) => a - b)[Math.max(0, Math.ceil(values.length * p) - 1)];
const summarize = values => ({ count: values.length, p50Ms: percentile(values, .5), p95Ms: percentile(values, .95), maxMs: Math.max(...values) });
let seed = 42;
const random = () => { seed = (seed * 1103515245 + 12345) % 2147483648; return seed / 2147483648; };
try {
  db.execute('CREATE TABLE people');
  db.execute('CREATE EDGE TABLE follows FROM people TO people');
  const started = performance.now();
  db.execute('BEGIN');
  for (let i = 0; i < nodes; i++) db.execute('INSERT INTO people DOCUMENT $doc', { $doc: { id: new Record('people', BigInt(i)), weight: BigInt(i % 100) } });
  for (let i = 0; i < nodes; i++) {
    for (let j = 0; j < degree; j++) {
      db.execute('RELATE $s -> follows -> $t {w:$w}', { $s: new Record('people', BigInt(i)), $t: new Record('people', BigInt(Math.floor(random() * nodes))), $w: BigInt(j) });
    }
  }
  db.execute('COMMIT');
  const loadMs = performance.now() - started;
  const run = (sql, samples = 50) => {
    const timings = [];
    let metrics;
    for (let i = 0; i < samples; i++) {
      const root = new Record('people', BigInt(Math.floor(random() * nodes)));
      const begin = performance.now();
      const profile = db.profileSelect(sql, { $root: root });
      timings.push(performance.now() - begin);
      metrics = profile.metrics;
      assert(profile.result.rows.length > 0);
    }
    return { ...summarize(timings), lastMetrics: Object.fromEntries(Object.entries(metrics).filter(([key]) => key.startsWith('graph')).map(([key, value]) => [key, Number(value)])) };
  };
  const report = {
    package: require(path.join(packageDir, 'package.json')).version,
    node: process.version,
    nodes,
    edges: nodes * degree,
    loadSeconds: loadMs / 1000,
    oneHop: run('SELECT count(*) FROM GRAPH($root -> follows -> people) g'),
    twoHopFixed: run('SELECT count(*) FROM GRAPH($root -> follows -> people -> follows -> people) g'),
    reachableDepth2: run('SELECT count(*) FROM GRAPH($root -> follows -> people DEPTH 1 TO 2 MODE REACHABLE) g'),
    filteredReachableDepth3: run('SELECT count(*) FROM GRAPH($root -> (follows WHERE w < 3) -> (people WHERE weight < 50) DEPTH 1 TO 3 MODE REACHABLE) g', 20),
    integrity: Number(db.checkCollectionIntegrity('follows', { maxDocuments: BigInt(nodes * degree) }).documents),
    scope: 'Single local Linux x64 sample on the qualification host; not a latency or capacity guarantee',
  };
  fs.writeFileSync(output, JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify(report));
} finally {
  db.close();
  fs.rmSync(directory, { recursive: true, force: true });
}
