#!/usr/bin/env node
'use strict';
// Location workloads on RomeDB (embedded, file-backed) and SurrealDB (server,
// RocksDB file backend) over identical generated data. Every timed query is also
// checked for identical results across systems. A single-host sample, not a
// capacity or latency claim. Run under scripts/romedb-heavy so both databases
// share one CPU and memory limit.
//
// Usage: bench-spatial-surreal.cjs ROMEDB_NODE_PACKAGE SURREAL_BINARY WORKDIR OUTPUT_JSON [DOCUMENTS] [SAMPLES]
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { spawn } = require('node:child_process');
const { performance } = require('node:perf_hooks');

const [packageDir, surrealBinary, workdir, output, documentsArg = '100000', samplesArg = '25'] = process.argv.slice(2);
assert(packageDir && surrealBinary && workdir && output, 'Usage: bench-spatial-surreal.cjs PACKAGE SURREAL WORKDIR OUTPUT [DOCUMENTS] [SAMPLES]');
const { Database, Record } = require(path.resolve(packageDir));
const DOCUMENTS = Number(documentsArg);
const SAMPLES = Number(samplesArg);
const ZONES = 500;
const MOVES = 500;
const PORT = 18766;
const RADIUS_M = 5000;
const FRESH = 90;
const EARTH = 6371008.8;
const DEG = Math.PI / 180;

let seed = 0x5eed;
const random = () => {
  seed |= 0; seed = (seed + 0x6d2b79f5) | 0;
  let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
  t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
};
const normal = () => Math.sqrt(-2 * Math.log(Math.max(random(), 1e-300))) * Math.cos(2 * Math.PI * random());
const clampPoint = ([x, y]) => [x > 180 ? x - 360 : x < -180 ? x + 360 : x, Math.max(-89.9, Math.min(89.9, y))];
const percentile = (values, p) => [...values].sort((a, b) => a - b)[Math.max(0, Math.ceil(values.length * p) - 1)];
const summary = values => ({ p50Ms: +percentile(values, 0.5).toFixed(3), p95Ms: +percentile(values, 0.95).toFixed(3), meanMs: +(values.reduce((a, b) => a + b, 0) / values.length).toFixed(3) });

function dataset(kind) {
  const cities = Array.from({ length: 20 }, () => [random() * 300 - 150, random() * 120 - 60]);
  const points = [];
  const attributes = [];
  for (let i = 0; i < DOCUMENTS; i++) {
    if (kind === 'uniform') points.push([random() * 360 - 180, Math.asin(2 * random() - 1) / DEG]);
    else { const c = cities[i % cities.length]; points.push(clampPoint([c[0] + normal() * 0.05, c[1] + normal() * 0.05])); }
    // Independent of location: one third available, freshness uniform 0..99,
    // and a rare fleet scattered across the whole dataset.
    attributes.push({ available: random() < 1 / 3, lastSeen: Math.floor(random() * 100), fleet: random() < 0.001 ? 'night' : 'day' });
  }
  const zones = Array.from({ length: ZONES }, (_, i) => {
    const c = kind === 'uniform' ? [random() * 340 - 170, random() * 140 - 70] : cities[i % cities.length];
    const [x, y] = [c[0] + normal() * 0.2, c[1] + normal() * 0.2];
    const w = 0.01 + random() * 0.1;
    const h = 0.01 + random() * 0.1;
    return [[[x, y], [x + w, y], [x + w * 1.2, y + h], [x - w * 0.2, y + h * 0.8], [x, y]]];
  });
  return { kind, points, attributes, zones };
}

// Probe centers come from the data, jittered, so dense areas are queried as often as users are there.
function probes(points, count) {
  return Array.from({ length: count }, () => {
    const p = points[Math.floor(random() * points.length)];
    return clampPoint([Math.max(-179, Math.min(179, p[0] + normal() * 0.01)), Math.max(-85, Math.min(85, p[1] + normal() * 0.01))]);
  });
}

function storm([x, y]) {
  const ring = [];
  for (let i = 0; i < 24; i++) {
    const a = (i / 24) * 2 * Math.PI;
    const r = 0.12 + 0.06 * Math.sin(3 * a);
    ring.push([x + r * Math.cos(a) * 1.4, y + r * Math.sin(a)]);
  }
  ring.push(ring[0]);
  return { type: 'Polygon', coordinates: [ring] };
}

const hex = n => { const b = Buffer.alloc(8); b.writeDoubleBE(n); return b.toString('hex'); };
const typedPoint = ([x, y]) => ({ type: 'Object', value: { type: { type: 'String', value: 'Point' }, coordinates: { type: 'Array', value: [{ type: 'Number', value: hex(x) }, { type: 'Number', value: hex(y) }] } } });
const typedPolygon = rings => ({ type: 'Object', value: { type: { type: 'String', value: 'Polygon' }, coordinates: { type: 'Array', value: rings.map(ring => ({ type: 'Array', value: ring.map(p => ({ type: 'Array', value: p.map(n => ({ type: 'Number', value: hex(n) })) })) })) } } });
const typedId = (table, i) => ({ type: 'Record', value: { table, key: { type: 'Integer', value: String(i) } } });
const header = '{"format":"romedb.documents","version":1}';

// ---- Geohash covering for SurrealDB's documented indexed-geohash pattern ----
const BASE32 = '0123456789bcdefghjkmnpqrstuvwxyz';
const PRECISIONS = [7, 6, 5, 4];
function geohash(ix, iy, precision) {
  const lngBits = Math.ceil((5 * precision) / 2);
  const latBits = Math.floor((5 * precision) / 2);
  let out = '';
  let bits = 0;
  let value = 0;
  let li = lngBits - 1;
  let ai = latBits - 1;
  for (let k = 0; k < 5 * precision; k++) {
    const bit = k % 2 === 0 ? (ix >> li--) & 1 : (iy >> ai--) & 1;
    value = (value << 1) | bit;
    if (++bits === 5) { out += BASE32[value]; bits = 0; value = 0; }
  }
  return out;
}
function cellsFor(boxes, precision) {
  const lngCells = 2 ** Math.ceil((5 * precision) / 2);
  const latCells = 2 ** Math.floor((5 * precision) / 2);
  const cells = new Set();
  for (const [w, s, e, n] of boxes) {
    const x0 = Math.max(0, Math.floor(((w + 180) / 360) * lngCells) - 1);
    const x1 = Math.min(lngCells - 1, Math.floor(((e + 180) / 360) * lngCells) + 1);
    const y0 = Math.max(0, Math.floor(((s + 90) / 180) * latCells) - 1);
    const y1 = Math.min(latCells - 1, Math.floor(((n + 90) / 180) * latCells) + 1);
    if ((x1 - x0 + 1) * (y1 - y0 + 1) > 400) return null;
    for (let x = x0; x <= x1; x++) for (let y = y0; y <= y1; y++) cells.add(geohash(x, y, precision));
  }
  return [...cells];
}
// Finest stored precision whose cover stays under 64 cells, as an application would choose.
function cover(boxes) {
  for (const p of PRECISIONS) {
    const cells = cellsFor(boxes, p);
    if (cells && cells.length <= 64) return { field: `gh${p}`, cells };
  }
  return null;
}
function wrapBoxes(w, s, e, n) {
  if (e - w >= 360) return [[-180, s, 180, n]];
  if (w < -180) return [[w + 360, s, 180, n], [-180, s, e, n]];
  if (e > 180) return [[w, s, 180, n], [-180, s, e - 360, n]];
  return [[w, s, e, n]];
}
function radiusBoxes([x, y], r) {
  const dLat = (r / EARTH) / DEG * 1.001 + 1e-6;
  const s = Math.max(-90, y - dLat);
  const n = Math.min(90, y + dLat);
  if (s <= -89.9 || n >= 89.9 || r >= EARTH * Math.PI / 2) return [[-180, s, 180, n]];
  const dLng = Math.min(360, dLat / Math.cos(Math.max(Math.abs(s), Math.abs(n)) * DEG) * 1.001);
  return wrapBoxes(x - dLng, s, x + dLng, n);
}

// ---- SurrealDB over HTTP ----
// Sign in once and reuse the token, as SurrealDB SDK sessions do. HTTP Basic
// authentication verifies the password hash on every request (about 60 ms here).
let auth;
async function signIn() {
  const response = await fetch(`http://127.0.0.1:${PORT}/signin`, { method: 'POST', headers: { Accept: 'application/json' }, body: JSON.stringify({ user: 'root', pass: 'root' }) });
  const body = await response.json();
  assert(body.token, `SurrealDB sign-in failed: ${JSON.stringify(body)}`);
  auth = `Bearer ${body.token}`;
}
async function surreal(sql, ns = true) {
  const headers = { Accept: 'application/json', Authorization: auth };
  if (ns) Object.assign(headers, { 'surreal-ns': 'bench', 'surreal-db': 'bench' });
  const response = await fetch(`http://127.0.0.1:${PORT}/sql`, { method: 'POST', headers, body: sql });
  const body = await response.json();
  if (!Array.isArray(body)) throw new Error(`SurrealDB error: ${JSON.stringify(body).slice(0, 400)}`);
  for (const statement of body) if (statement.status !== 'OK') throw new Error(`SurrealDB statement failed: ${JSON.stringify(statement).slice(0, 400)}\n${sql.slice(0, 300)}`);
  return body;
}
const serverMs = body => body.reduce((total, s) => {
  const m = /^([\d.]+)(ns|µs|us|ms|s)$/.exec(s.time);
  const scale = { ns: 1e-6, µs: 1e-3, us: 1e-3, ms: 1, s: 1000 }[m[2]];
  return total + Number(m[1]) * scale;
}, 0);
const tuple = ([x, y]) => `(${x}, ${y})`;
const geo = value => JSON.stringify(value);
const ids = rows => rows.map(row => String(row.id).replace(/^\w+:/, '')).sort();

const children = new Set();
process.on('exit', () => { for (const child of children) child.kill('SIGKILL'); });
async function startSurreal(dir) {
  try {
    await fetch(`http://127.0.0.1:${PORT}/health`);
    throw new Error(`port ${PORT} already answers; stop the other server first`);
  } catch (error) {
    if (error.message.startsWith('port ')) throw error;
  }
  const child = spawn(surrealBinary, ['start', '--user', 'root', '--pass', 'root', '--bind', `127.0.0.1:${PORT}`, '--log', 'warn', `rocksdb://${dir}`], { stdio: ['ignore', 'ignore', 'pipe'] });
  children.add(child);
  child.once('exit', () => children.delete(child));
  let stderr = '';
  child.stderr.on('data', d => { stderr += d; });
  for (let i = 0; i < 200; i++) {
    try { if ((await fetch(`http://127.0.0.1:${PORT}/health`)).ok) return child; } catch {}
    await new Promise(r => setTimeout(r, 100));
  }
  child.kill('SIGKILL');
  throw new Error(`SurrealDB did not start: ${stderr}`);
}

async function runDataset(data) {
  const dir = fs.mkdtempSync(path.join(workdir, `${data.kind}-`));
  const report = { dataset: data.kind, documents: DOCUMENTS, zones: ZONES, load: {}, queries: {}, moves: {} };

  // ---- RomeDB load ----
  const db = new Database(path.join(dir, 'romedb.db'));
  const people = [header, ...data.points.map((p, i) => JSON.stringify({ type: 'Object', value: {
    id: typedId('people', i), available: { type: 'Boolean', value: data.attributes[i].available }, last_seen: { type: 'Integer', value: String(data.attributes[i].lastSeen) },
    fleet: { type: 'String', value: data.attributes[i].fleet }, location: typedPoint(p) } }))].join('\n') + '\n';
  const zones = [header, ...data.zones.map((rings, i) => JSON.stringify({ type: 'Object', value: { id: typedId('zones', i), shape: typedPolygon(rings) } }))].join('\n') + '\n';
  db.execute('CREATE TABLE people');
  db.execute('CREATE TABLE zones');
  let start = performance.now();
  db.importDocuments('people', people, 'ndjson');
  db.importDocuments('zones', zones, 'ndjson');
  report.load.romedbImportSeconds = +((performance.now() - start) / 1000).toFixed(2);
  start = performance.now();
  db.execute('CREATE SEARCH INDEX people_location ON people(location) USING SPATIAL');
  db.execute("CREATE SEARCH INDEX zones_shape ON zones(shape) USING SPATIAL WITH (geometry = 'region')");
  report.load.romedbIndexSeconds = +((performance.now() - start) / 1000).toFixed(2);

  // ---- SurrealDB load ----
  const child = await startSurreal(path.join(dir, 'surreal'));
  try {
    await signIn();
    await surreal('DEFINE NAMESPACE IF NOT EXISTS bench; USE NS bench; DEFINE DATABASE IF NOT EXISTS bench;', false);
    await surreal(['DEFINE TABLE people SCHEMALESS;', 'DEFINE TABLE zones SCHEMALESS;',
      ...PRECISIONS.map(p => `DEFINE FIELD gh${p} ON people VALUE geo::hash::encode(location, ${p});`)].join(' '));
    start = performance.now();
    for (let i = 0; i < DOCUMENTS; i += 1000) {
      const rows = data.points.slice(i, i + 1000).map((p, j) => ({ id: i + j, available: data.attributes[i + j].available, last_seen: data.attributes[i + j].lastSeen, fleet: data.attributes[i + j].fleet, location: { type: 'Point', coordinates: p } }));
      await surreal(`INSERT INTO people ${JSON.stringify(rows)};`);
    }
    await surreal(`INSERT INTO zones ${JSON.stringify(data.zones.map((rings, i) => ({ id: i, shape: { type: 'Polygon', coordinates: rings } })))};`);
    report.load.surrealInsertSeconds = +((performance.now() - start) / 1000).toFixed(2);
    start = performance.now();
    await surreal(PRECISIONS.map(p => `DEFINE INDEX people_gh${p} ON people FIELDS gh${p};`).join(' '));
    report.load.surrealIndexSeconds = +((performance.now() - start) / 1000).toFixed(2);

    const centers = probes(data.points, SAMPLES);
    const fresh = BigInt(FRESH);
    const fast = (sql, params) => {
      const begin = performance.now();
      if (process.env.BENCH_PROFILE) {
        const profile = db.profileSelect(sql, params);
        const ms = performance.now() - begin;
        const m = profile.metrics;
        process.stderr.write(`  romedb ${ms.toFixed(1)} ms rows ${profile.result.rows.length} read ${m.rowsRead} ranges ${m.spatialRanges} rounds ${m.spatialRounds}\n`);
        return { ms, rows: profile.result.rows };
      }
      const rows = db.execute(sql, params).rows;
      return { ms: performance.now() - begin, rows };
    };
    const slow = async sql => {
      const begin = performance.now();
      const body = await surreal(sql);
      return { ms: performance.now() - begin, server: serverMs(body), rows: body[body.length - 1].result };
    };
    const fastIds = rows => rows.map(row => String(row[0].key)).sort();

    const workloads = {
      nearbyAvailable5km: {
        romedb: c => fast('SELECT id FROM people WHERE ST_DWithin(location, ST_Point($x, $y), $r) AND available = true', { $x: c[0], $y: c[1], $r: RADIUS_M }),
        romedbJoin: c => fast('SELECT p.id FROM search::near(\'people_location\', ST_Point($x, $y), $r) AS n JOIN people AS p ON p.id = n.id WHERE p.available = true', { $x: c[0], $y: c[1], $r: RADIUS_M }),
        surrealScan: c => slow(`SELECT id FROM people WHERE available = true AND geo::distance(location, ${tuple(c)}) <= ${RADIUS_M};`),
        surrealGeohash: c => { const k = cover(radiusBoxes(c, RADIUS_M)); return slow(`SELECT id FROM people WHERE ${k.field} IN ${geo(k.cells)} AND available = true AND geo::distance(location, ${tuple(c)}) <= ${RADIUS_M};`); },
      },
      viewport01deg: {
        romedb: c => fast('SELECT id FROM search::bbox(\'people_location\', $w, $s, $e, $n)', { $w: c[0] - 0.05, $s: c[1] - 0.05, $e: c[0] + 0.05, $n: c[1] + 0.05 }),
        surrealScan: c => slow(`SELECT id FROM people WHERE location INSIDE ${geo(box(c, 0.05))};`),
        surrealGeohash: c => { const k = cover([[c[0] - 0.05, c[1] - 0.05, c[0] + 0.05, c[1] + 0.05]]); return slow(`SELECT id FROM people WHERE ${k.field} IN ${geo(k.cells)} AND location INSIDE ${geo(box(c, 0.05))};`); },
      },
      stormPolygon: {
        romedb: c => fast('SELECT id FROM people WHERE ST_Covers(ST_GeomFromGeoJSON($g), location)', { $g: JSON.stringify(storm(c)) }),
        surrealScan: c => slow(`SELECT id FROM people WHERE location INSIDE ${geo(storm(c))};`),
        surrealGeohash: c => { const k = cover([bounds(storm(c))]); return slow(`SELECT id FROM people WHERE ${k.field} IN ${geo(k.cells)} AND location INSIDE ${geo(storm(c))};`); },
      },
      zoneLookup: {
        romedb: c => fast('SELECT id FROM zones WHERE ST_Covers(shape, ST_Point($x, $y))', { $x: c[0], $y: c[1] }),
        surrealScan: c => slow(`SELECT id FROM zones WHERE shape CONTAINS ${tuple(c)};`),
      },
      nearest5AvailableFresh: {
        romedb: c => fast('SELECT id, ST_DistanceSphere(location, ST_Point($x, $y)) AS d FROM people WHERE ST_DistanceSphere(location, ST_Point($x, $y)) IS NOT NULL AND available = true AND last_seen >= $fresh ORDER BY d, id LIMIT 5', { $x: c[0], $y: c[1], $fresh: fresh }),
        surrealScan: c => slow(`SELECT id, geo::distance(location, ${tuple(c)}) AS d FROM people WHERE available = true AND last_seen >= ${FRESH} ORDER BY d, id LIMIT 5;`),
        surrealGeohash: async c => {
          let total = 0;
          let server = 0;
          for (let radius = 100; ; radius *= 2) {
            const last = radius >= EARTH * Math.PI / 2;
            const k = last ? null : cover(radiusBoxes(c, radius));
            const where = k ? `${k.field} IN ${geo(k.cells)} AND ` : '';
            const result = await slow(`SELECT id, geo::distance(location, ${tuple(c)}) AS d FROM people WHERE ${where}available = true AND last_seen >= ${FRESH} AND geo::distance(location, ${tuple(c)}) <= ${last ? 1e12 : radius} ORDER BY d, id LIMIT 5;`);
            total += result.ms;
            server += result.server;
            if (result.rows.length === 5 || last) return { ms: total, server, rows: result.rows };
          }
        },
      },
      nearest5RareFleet: {
        romedb: c => fast('SELECT id, ST_DistanceSphere(location, ST_Point($x, $y)) AS d FROM people WHERE ST_DistanceSphere(location, ST_Point($x, $y)) IS NOT NULL AND fleet = \'night\' ORDER BY d, id LIMIT 5', { $x: c[0], $y: c[1] }),
        surrealScan: c => slow(`SELECT id, geo::distance(location, ${tuple(c)}) AS d FROM people WHERE fleet = 'night' ORDER BY d, id LIMIT 5;`),
      },
      dashboardCells05deg: {
        romedb: c => fast('SELECT geo::cell(p.location, 7) AS cell, count(*) AS n FROM search::bbox(\'people_location\', $w, $s, $e, $n) AS v JOIN people AS p ON p.id = v.id GROUP BY geo::cell(p.location, 7)', { $w: c[0] - 0.25, $s: c[1] - 0.25, $e: c[0] + 0.25, $n: c[1] + 0.25 }),
        surrealScan: c => slow(`SELECT geo::hash::encode(location, 6) AS cell, count() AS n FROM people WHERE location INSIDE ${geo(box(c, 0.25))} GROUP BY cell;`),
        surrealGeohash: c => { const k = cover([[c[0] - 0.25, c[1] - 0.25, c[0] + 0.25, c[1] + 0.25]]); return slow(`SELECT geo::hash::encode(location, 6) AS cell, count() AS n FROM people WHERE ${k.field} IN ${geo(k.cells)} AND location INSIDE ${geo(box(c, 0.25))} GROUP BY cell;`); },
      },
    };

    const only = process.env.BENCH_ONLY ? process.env.BENCH_ONLY.split(',') : null;
    for (const [name, systems] of Object.entries(workloads)) {
      if (only && !only.includes(name)) continue;
      const result = { mismatches: 0, rows: [] };
      for (const [system, run] of Object.entries(systems)) {
        await run(centers[0]);
        const times = [];
        const server = [];
        result[system] = { idsBySample: [] };
        for (const c of centers) {
          const r = await run(c);
          times.push(r.ms);
          if (r.server !== undefined) server.push(r.server);
          result[system].idsBySample.push(system.startsWith('romedb')
            ? (name === 'dashboardCells05deg' ? r.rows.reduce((t, row) => t + Number(row[1]), 0) : fastIds(r.rows))
            : (name === 'dashboardCells05deg' ? r.rows.reduce((t, row) => t + Number(row.n), 0) : ids(r.rows)));
        }
        result[system].client = summary(times);
        if (server.length) result[system].server = summary(server);
      }
      for (const system of Object.keys(systems).filter(s => s !== 'romedb')) {
        result[system].mismatchedSamples = result[system].idsBySample.filter((v, i) => JSON.stringify(v) !== JSON.stringify(result.romedb.idsBySample[i])).length;
      }
      result.meanResultSize = +(result.romedb.idsBySample.reduce((t, v) => t + (Array.isArray(v) ? v.length : v), 0) / centers.length).toFixed(1);
      for (const system of Object.keys(systems)) delete result[system].idsBySample;
      delete result.mismatches;
      delete result.rows;
      report.queries[name] = result;
      process.stderr.write(`${data.kind} ${name}: ${JSON.stringify(result)}\n`);
    }

    // Moving drivers: one location report per autocommit write.
    const moves = Array.from({ length: MOVES }, () => {
      const i = Math.floor(random() * DOCUMENTS);
      const p = data.points[i];
      return [i, clampPoint([p[0] + normal() * 0.001, p[1] + normal() * 0.001])];
    });
    let times = [];
    for (const [i, p] of moves) {
      const begin = performance.now();
      db.execute('UPDATE people SET location = geo::point($x, $y) WHERE id = $id', { $id: new Record('people', BigInt(i)), $x: p[0], $y: p[1] });
      times.push(performance.now() - begin);
    }
    report.moves.romedb = summary(times);
    times = [];
    const server = [];
    for (const [i, p] of moves) {
      const begin = performance.now();
      const body = await surreal(`UPDATE people:${i} SET location = ${tuple(p)};`);
      times.push(performance.now() - begin);
      server.push(serverMs(body));
    }
    report.moves.surreal = { client: summary(times), server: summary(server) };
    const check = moves.slice(-20);
    for (const [i, p] of check) {
      const f = db.execute('SELECT geo::lng(location), geo::lat(location) FROM people WHERE id = $id', { $id: new Record('people', BigInt(i)) }).rows[0];
      const s = (await surreal(`SELECT location FROM people:${i};`))[0].result[0].location.coordinates;
      const number = v => (v !== null && typeof v === 'object' && 'value' in v ? v.value : Number(v));
      assert.deepEqual([number(f[0]), number(f[1])], s);
    }
    report.integrity = Number(db.checkCollectionIntegrity('people', { maxDocuments: BigInt(DOCUMENTS) }).documents);
    report.storage = { romedbBytes: fileBytes(path.join(dir, 'romedb.db')) + fileBytes(path.join(dir, 'romedb.db-wal')), surrealBytes: directoryBytes(path.join(dir, 'surreal')) };
  } finally {
    db.close();
    child.kill('SIGTERM');
    await new Promise(r => child.once('exit', r));
    fs.rmSync(dir, { recursive: true, force: true });
  }
  return report;
}

function box([x, y], half) {
  return { type: 'Polygon', coordinates: [[[x - half, y - half], [x + half, y - half], [x + half, y + half], [x - half, y + half], [x - half, y - half]]] };
}
function bounds(g) {
  const ring = g.coordinates[0];
  return [Math.min(...ring.map(p => p[0])), Math.min(...ring.map(p => p[1])), Math.max(...ring.map(p => p[0])), Math.max(...ring.map(p => p[1]))];
}
function fileBytes(file) { try { return fs.statSync(file).size; } catch { return 0; } }
function directoryBytes(dir) {
  let total = 0;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, entry.name);
    total += entry.isDirectory() ? directoryBytes(p) : fs.statSync(p).size;
  }
  return total;
}

(async () => {
  fs.mkdirSync(workdir, { recursive: true });
  const surrealVersion = await new Promise(resolve => {
    const child = spawn(surrealBinary, ['version']);
    let out = '';
    child.stdout.on('data', d => { out += d; });
    child.on('exit', () => resolve(out.trim()));
  });
  const reports = [];
  const kinds = process.env.BENCH_DATASETS ? process.env.BENCH_DATASETS.split(',') : ['cities', 'uniform'];
  for (const kind of ['cities', 'uniform']) {
    const data = dataset(kind);
    if (kinds.includes(kind)) reports.push(await runDataset(data));
  }
  const result = {
    romedb: require(path.join(path.resolve(packageDir), 'package.json')).version + (process.env.BENCH_LABEL ? ` (${process.env.BENCH_LABEL})` : ''),
    surrealdb: surrealVersion,
    node: process.version,
    documents: DOCUMENTS,
    samples: SAMPLES,
    scope: 'Single local Linux x64 WSL2 sample under one 4-CPU, 24 GiB cgroup; RomeDB embedded in-process, SurrealDB over loopback HTTP; not a latency or capacity guarantee',
    reports,
  };
  fs.writeFileSync(output, JSON.stringify(result, null, 2) + '\n');
  process.stdout.write(`wrote ${output}\n`);
})().catch(error => { console.error(error); process.exit(1); });
