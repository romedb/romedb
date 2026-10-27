#!/usr/bin/env node
// Moving drivers with concurrent nearest and viewport searches on one database
// file: one writer connection moves drivers with synced autocommit updates while
// reader connections search. Each connection is an AsyncDatabase worker thread
// in one process. This is a local sample, not a latency or capacity guarantee.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { performance } = require('node:perf_hooks');

const [packageDir, workdir, output, driversArg = '100000', secondsArg = '20', readersArg = '3'] = process.argv.slice(2);
assert(packageDir && workdir && output, 'Usage: bench-spatial-movement.cjs PACKAGE WORKDIR OUTPUT [DRIVERS] [SECONDS] [READERS]');
const { Database, AsyncDatabase, Record } = require(path.resolve(packageDir));
const DRIVERS = Number(driversArg);
const SECONDS = Number(secondsArg);
const READERS = Number(readersArg);
const FRESH = 90;
const STEP_DEG = 0.0005;

let seed = 0x5eed;
const random = () => {
  seed |= 0; seed = (seed + 0x6d2b79f5) | 0;
  let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
  t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
};
const normal = () => Math.sqrt(-2 * Math.log(Math.max(random(), 1e-300))) * Math.cos(2 * Math.PI * random());
const percentile = (values, p) => [...values].sort((a, b) => a - b)[Math.max(0, Math.ceil(values.length * p) - 1)];
const summary = values => ({
  count: values.length,
  p50Ms: +percentile(values, 0.5).toFixed(3),
  p95Ms: +percentile(values, 0.95).toFixed(3),
  p99Ms: +percentile(values, 0.99).toFixed(3),
  maxMs: +Math.max(...values).toFixed(3),
});

const hex = n => { const b = Buffer.alloc(8); b.writeDoubleBE(n); return b.toString('hex'); };
const typedPoint = ([x, y]) => ({ type: 'Object', value: { type: { type: 'String', value: 'Point' }, coordinates: { type: 'Array', value: [{ type: 'Number', value: hex(x) }, { type: 'Number', value: hex(y) }] } } });

const cities = Array.from({ length: 20 }, () => [random() * 300 - 150, random() * 120 - 60]);
const positions = Array.from({ length: DRIVERS }, (_, i) => {
  const c = cities[i % cities.length];
  return [c[0] + normal() * 0.05, c[1] + normal() * 0.05];
});

async function main() {
  fs.rmSync(workdir, { recursive: true, force: true });
  fs.mkdirSync(workdir, { recursive: true });
  const file = path.join(workdir, 'drivers.db');
  const loader = new Database(file);
  loader.execute('CREATE TABLE drivers');
  const lines = ['{"format":"romedb.documents","version":1}'];
  positions.forEach((p, i) => lines.push(JSON.stringify({ type: 'Object', value: {
    id: { type: 'Record', value: { table: 'drivers', key: { type: 'Integer', value: String(i) } } },
    available: { type: 'Boolean', value: random() < 1 / 3 },
    last_seen: { type: 'Integer', value: String(Math.floor(random() * 100)) },
    location: typedPoint(p),
  } })));
  let start = performance.now();
  loader.importDocuments('drivers', lines.join('\n') + '\n', 'ndjson');
  loader.execute('CREATE SEARCH INDEX drivers_location ON drivers(location) USING SPATIAL');
  const loadSeconds = +((performance.now() - start) / 1000).toFixed(2);
  loader.close();

  const writer = await AsyncDatabase.open(file);
  const readers = await Promise.all(Array.from({ length: READERS }, () => AsyncDatabase.open(file)));
  const writes = [];
  const nearest = [];
  const viewports = [];
  const deadline = performance.now() + SECONDS * 1000;
  let moved = 0;

  const writeLoop = async () => {
    while (performance.now() < deadline) {
      const i = Math.floor(random() * DRIVERS);
      const p = positions[i];
      p[0] += (random() - 0.5) * STEP_DEG;
      p[1] += (random() - 0.5) * STEP_DEG;
      const t = performance.now();
      await writer.execute('UPDATE drivers SET location = ST_Point($x, $y), last_seen = 99 WHERE id = $id',
        { $id: new Record('drivers', BigInt(i)), $x: p[0], $y: p[1] });
      writes.push(performance.now() - t);
      moved++;
    }
  };
  const readLoop = async (db, index) => {
    for (let n = 0; performance.now() < deadline; n++) {
      const [x, y] = positions[Math.floor(random() * DRIVERS)];
      const t = performance.now();
      if ((n + index) % 2 === 0) {
        const rows = await db.all('SELECT id FROM drivers WHERE ST_DistanceSphere(location, ST_Point($x, $y)) IS NOT NULL AND available = true AND last_seen >= $fresh ORDER BY ST_DistanceSphere(location, ST_Point($x, $y)), id LIMIT 5',
          { $x: x, $y: y, $fresh: FRESH });
        nearest.push(performance.now() - t);
        assert.equal(rows.length, 5);
      } else {
        await db.all('SELECT id FROM search::bbox(\'drivers_location\', $w, $s, $e, $n)', { $w: x - 0.01, $s: y - 0.01, $e: x + 0.01, $n: y + 0.01 });
        viewports.push(performance.now() - t);
      }
    }
  };
  start = performance.now();
  await Promise.all([writeLoop(), ...readers.map(readLoop)]);
  const elapsed = (performance.now() - start) / 1000;

  // Final state: the index matches every document, and indexed nearest results
  // match a scan after all moves.
  const integrity = await writer.checkCollectionIntegrity('drivers');
  for (let n = 0; n < 20; n++) {
    const [x, y] = positions[Math.floor(random() * DRIVERS)];
    const sql = 'SELECT id FROM drivers WHERE ST_DistanceSphere(location, ST_Point($x, $y)) IS NOT NULL AND available = true ORDER BY ST_DistanceSphere(location, ST_Point($x, $y)), id LIMIT 5';
    const indexed = await writer.all(sql, { $x: x, $y: y });
    const scanned = await writer.all(sql.replace('FROM drivers', 'FROM drivers NOT INDEXED'), { $x: x, $y: y });
    assert.deepEqual(indexed, scanned);
  }
  await Promise.all([writer.close(), ...readers.map(db => db.close())]);

  const result = {
    romedb: require(path.join(path.resolve(packageDir), 'package.json')).version,
    node: process.version,
    drivers: DRIVERS,
    seconds: +elapsed.toFixed(2),
    readers: READERS,
    loadSeconds,
    writesPerSecond: +(moved / elapsed).toFixed(1),
    writes: summary(writes),
    nearestAvailableFresh: summary(nearest),
    viewport: summary(viewports),
    integrityDocuments: Number(integrity.documents),
    scanMatchedSamples: 20,
    scope: 'One process, one writer and READERS reader AsyncDatabase connections on one file, synced autocommit moves; local sample, not a guarantee',
  };
  fs.writeFileSync(output, JSON.stringify(result, null, 2) + '\n');
  console.log(JSON.stringify(result, null, 2));
  fs.rmSync(workdir, { recursive: true, force: true });
}

main().catch(error => { console.error(error); process.exit(1); });
