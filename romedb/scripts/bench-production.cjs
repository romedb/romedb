#!/usr/bin/env node
'use strict';
// Exact-bundle application qualification. No locally built addon is loaded.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const {createHash} = require('node:crypto');
const {execFileSync} = require('node:child_process');
const {performance} = require('node:perf_hooks');
const encode = value => JSON.stringify(value, (_, item) => typeof item === 'bigint' ? item.toString() : item, 2);
const hash = file => {
  const digest = createHash('sha256'), buffer = Buffer.alloc(1024 * 1024), fd = fs.openSync(file, 'r');
  try { let size; while ((size = fs.readSync(fd, buffer, 0, buffer.length, null))) digest.update(buffer.subarray(0, size)); }
  finally { fs.closeSync(fd); }
  return digest.digest('hex');
};
const memory = () => ({rssBytes: process.memoryUsage().rss, peakRssKiB: process.resourceUsage().maxRSS});
const size = file => fs.existsSync(file) ? fs.statSync(file).size : 0;
const files = file => ({databaseBytes: size(file), walBytes: size(file + '-wal')});
const percentile = (values, p) => [...values].sort((a,b) => a-b)[Math.max(0, Math.ceil(values.length * p) - 1)];
const summarize = values => ({count: values.length, minMs: Math.min(...values), p50Ms: percentile(values, .5),
  p95Ms: percentile(values, .95), maxMs: Math.max(...values), totalMs: values.reduce((a,b) => a+b, 0)});

function progressReporter(connectionCount) {
  const started = performance.now(), reported = new Map();
  return (phase, done, total) => {
    if (total !== undefined) {
      const decile = Math.floor(done * 10 / total);
      if (reported.get(phase) === decile && done !== total) return;
      reported.set(phase, decile);
    }
    const count = total === undefined ? '' : ` ${done}/${total}`;
    process.stderr.write(`[${connectionCount} connections +${((performance.now()-started)/1000).toFixed(3)}s] ${phase}${count}\n`);
  };
}

function parse() {
  if (process.argv.includes('--help')) {
    console.log('Usage: node bench-production.cjs --bundle DIR --output FILE [--rows 1000] [--dimensions 32] [--operations 5000] [--connections 1,4] [--concurrent-rounds 1000] [--samples 1] [--rehearsal]\nLinux x64 only. Installs the exact bundle package offline with pnpm. Rehearsal accepts historical dev bundles and is never production evidence.');
    process.exit(0);
  }
  const options = {rows: 1000, dimensions: 32, operations: 5000, connections: '1,4', 'concurrent-rounds': 1000, samples: 1, rehearsal: false};
  const names = new Set(['bundle', 'output', 'rows', 'dimensions', 'operations', 'connections', 'concurrent-rounds', 'samples']);
  for (let i = 2; i < process.argv.length; i++) {
    const key = process.argv[i].replace(/^--/, '');
    if (key === 'rehearsal') options.rehearsal = true;
    else if (names.has(key) && process.argv[i+1] && !process.argv[i+1].startsWith('--')) options[key] = process.argv[++i];
    else throw new Error('Unknown or incomplete argument: ' + process.argv[i]);
  }
  for (const [key, low, high] of [['rows', 32, 10000], ['dimensions', 2, 256], ['operations', 20, 50000], ['concurrent-rounds', 1, 10000], ['samples', 1, 3]]) {
    options[key] = Number(options[key]);
    assert(Number.isSafeInteger(options[key]) && options[key] >= low && options[key] <= high, `${key}: expected ${low}..${high}`);
  }
  options.connections = [...new Set(options.connections.split(',').map(Number))];
  assert(options.connections.length > 0 && options.connections.every(n => Number.isSafeInteger(n) && n >= 1 && n <= 8), 'connections: comma-separated 1..8');
  assert(options.bundle && options.output, '--bundle and --output are required');
  return options;
}

async function concurrentWorkload(config, file, revisions, api, corpus, progress) {
  if (config.connectionCount < 2) {
    progress('concurrency skipped: requires a writer and reader');
    return {skipped: 'Requires at least two connections: one writer and one reader'};
  }
  const {AsyncDatabase} = api, {record, vector, text, point} = corpus;
  const clients = [];
  const summary = values => values.length ? summarize(values) : {count:0};
  const delay = milliseconds => new Promise(resolve => setTimeout(resolve,milliseconds));
  const isBusy = error => error?.code === 'FDB_BUSY' || error?.code === 'FDB_BUSY_SNAPSHOT';
  const laneStats = role => ({role,
    completed:0, busyCount:0, busySnapshotCount:0, transactionRestarts:0, commitRetries:0,
    priorRevisionReads:0,updatedRevisionReads:0,backoffMs:0, latency:[], busyLatency:[]});
  const stats = Array.from({length:config.connectionCount}, (_, index) => laneStats(index===0?'writer':'reader'));
  const options = {timeoutMs:30000};
  let inFlightTransactions = 0, maxInFlightTransactions = 0;
  async function transaction(...arguments_) {
    inFlightTransactions++;
    maxInFlightTransactions = Math.max(maxInFlightTransactions,inFlightTransactions);
    try { return await runTransaction(...arguments_); }
    finally { inFlightTransactions--; }
  }
  async function runTransaction(client, lane, body, onBusy) {
    const started = performance.now();
    for (let attempt = 0; attempt < 8; attempt++) {
      const attemptStarted = performance.now();
      let stage = 'begin';
      try {
        await client.execute(lane.role==='writer'?'BEGIN IMMEDIATE':'BEGIN',{},options);
        stage = 'body';
        const value = await body();
        stage = 'commit';
        for (let commitAttempt = 0; ; commitAttempt++) {
          const commitStarted = performance.now();
          try { await client.execute('COMMIT',{},options); break; }
          catch (error) {
            // Retry only this COMMIT while the original transaction remains
            // active. Never replay writes after an uncertain commit outcome.
            if (!isBusy(error) || error.transaction?.after !== 'active' || commitAttempt === 7) throw error;
            lane[error.code==='FDB_BUSY'?'busyCount':'busySnapshotCount']++;
            lane.commitRetries++;
            lane.busyLatency.push(performance.now()-commitStarted);
            const milliseconds = Math.min(2**commitAttempt,25); lane.backoffMs += milliseconds;
            await delay(milliseconds);
          }
        }
        lane.completed++;
        lane.latency.push(performance.now()-started);
        return value;
      } catch (error) {
        // BEGIN/body Busy can restart only after confirmed rollback, or after
        // BEGIN failed without entering a transaction. Other errors are fatal.
        if (!isBusy(error) || stage === 'commit' || attempt === 7) throw error;
        lane[error.code==='FDB_BUSY'?'busyCount':'busySnapshotCount']++;
        lane.busyLatency.push(performance.now()-attemptStarted);
        if (error.transaction?.after === 'active') {
          const rolledBack = await client.execute('ROLLBACK',{},options);
          assert.equal(rolledBack.transaction.after,'autocommit');
        } else assert(stage==='begin' && error.transaction?.before==='autocommit'
          && error.transaction?.after==='autocommit', 'Busy retry requires a known rollback boundary');
        lane.transactionRestarts++;
        if (onBusy) await onBusy();
        const milliseconds = Math.min(2**attempt,25); lane.backoffMs += milliseconds;
        await delay(milliseconds);
      }
    }
    throw new Error('Unreachable transaction retry exhaustion');
  }
  try {
    progress('concurrency: open workers');
    for (let i = 0; i < config.connectionCount; i++) {
      const client = await AsyncDatabase.open(file); clients.push(client);
      await client.execute('PRAGMA synchronous=FULL');
    }
    const writer = clients[0];
    progress('concurrency: contention probe');
    // A separate finite probe guarantees that the documented Busy path and
    // retry after lock release run even if the measured workload has no Busy.
    await writer.execute('BEGIN IMMEDIATE');
    await writer.execute('UPDATE items SET revision=revision WHERE id=$id',{$id:record(0)});
    const contentionStarted = performance.now();
    const probeStats = laneStats('writer');
    let lockHeld = true, rejectedMs, retryStarted;
    try {
      await transaction(clients[1],probeStats,async () => {
        assert.equal((await clients[1].execute('UPDATE items SET revision=revision WHERE id=$id',{$id:record(0)},options)).affected,1n);
      },async () => {
        if (lockHeld) {
          rejectedMs = performance.now()-contentionStarted;
          assert.equal((await writer.execute('ROLLBACK')).transaction.after,'autocommit');
          lockHeld = false; retryStarted = performance.now();
        }
      });
    } finally { if (lockHeld) await writer.execute('ROLLBACK'); }
    assert.equal(probeStats.busyCount,1);
    assert.equal(probeStats.busySnapshotCount,0);
    assert.equal(probeStats.completed,1);
    const contentionProbe = {code:'FDB_BUSY',rejectedAttempts:probeStats.busyCount,rejectedMs,retryMs:performance.now()-retryStarted,
      transactionRestarts:probeStats.transactionRestarts,backoffMs:probeStats.backoffMs,retryAfterConfirmedLockRelease:true};
    const initial = {memory:memory(),files:files(file)};
    const series = [];
    let peakWalBytes = initial.files.walBytes;
    progress('concurrency: rounds begin');
    const started = performance.now();
    for (let round = 0; round < config['concurrent-rounds']; round++) {
      const n = (Math.imul(round,7919) >>> 0) % config.rows;
      const nextRevision = revisions[n]+1;
      // Each round keeps a write transaction and all reader transactions in
      // flight together. The barrier bounds pending requests and keeps readers
      // active for the entire writing phase, rather than finishing early.
      const writes = transaction(writer,stats[0],async () => {
        const result = await writer.execute('UPDATE items SET revision=$revision,body=$body,v=$v,location=$point WHERE id=$id',
          {$id:record(n),$revision:BigInt(nextRevision),$body:text(n,nextRevision),$v:vector(n,nextRevision),$point:point(n)},options);
        assert.equal(result.affected,1n);
      }).then(() => { revisions[n] = nextRevision; });
      const readers = clients.slice(1).map((reader,index) => transaction(reader,stats[index+1],async () => {
        // Read the writer's current target. A pinned snapshot may expose the
        // old or new revision, but document and every index must agree with it.
        const row = await reader.exactlyOne('SELECT n,revision,body,location FROM items WHERE id=$id',{$id:record(n)},options);
        const observed = Number(row[1]);
        assert.equal(row[0],BigInt(n));
        assert(observed===nextRevision-1 || observed===nextRevision, 'Reader observed an impossible committed revision');
        assert.equal(row[2],text(n,observed));
        assert.deepEqual(Object.keys(row[3]).sort(),['coordinates','type']);
        assert.deepEqual({type:row[3].type,coordinates:row[3].coordinates.map(Number)},point(n));
        const kind = (round+index)%3;
        if (kind===0) {
          const hits = await reader.all("SELECT id FROM search::text('items_text',$query,10)",{$query:`token${n} AND revision${observed}`},options);
          assert.deepEqual(hits,[[record(n)]]);
        } else if (kind===1) {
          const hits = await reader.all("SELECT id,distance FROM search::vector('items_vector',$v,1)",{$v:vector(n,observed)},options);
          assert.deepEqual(hits,[[record(n),0]]);
        } else {
          const hits = await reader.all("SELECT id,distance_m FROM search::near('items_location',$point,0)",{$point:point(n)},options);
          assert.deepEqual(hits,[[record(n),0]]);
        }
        return observed;
      }).then(observed => { stats[index+1][observed===nextRevision?'updatedRevisionReads':'priorRevisionReads']++; }));
      const outcomes = await Promise.allSettled([writes,...readers]);
      const failure = outcomes.find(outcome=>outcome.status==='rejected');
      if (failure) throw failure.reason;
      peakWalBytes = Math.max(peakWalBytes,size(file+'-wal'));
      if (round%25===0) series.push({round:round+1,elapsedMs:performance.now()-started,...memory(),...files(file)});
      progress('concurrency: rounds',round+1,config['concurrent-rounds']);
    }
    const elapsedMs = performance.now()-started;
    const after = {memory:memory(),files:files(file)};
    progress('concurrency: integrity');
    const auditStarted = performance.now();
    const audit = await writer.checkCollectionIntegrity('items',{},options);
    assert.equal(audit.documents,BigInt(config.rows));
    const auditMs = performance.now()-auditStarted;
    progress('concurrency: checkpoint');
    const checkpointStarted = performance.now();
    assert.deepEqual(await writer.all('PRAGMA wal_checkpoint(TRUNCATE)',{},options),[[0n,0n,0n]]);
    return {connectionCount:config.connectionCount,writerCount:1,readerCount:config.connectionCount-1,
      rounds:config['concurrent-rounds'],elapsedMs,transactionsPerSecond:config['concurrent-rounds']*config.connectionCount/(elapsedMs/1000),
      maxInFlightTransactions,contentionProbe,initial,after,peakWalBytes,series,
      auditMs,checkpointMs:performance.now()-checkpointStarted,afterCheckpoint:{memory:memory(),files:files(file)},
      lanes:stats.map(lane=>({...lane,latency:summary(lane.latency),busyLatency:summary(lane.busyLatency)})),correctness:'passed'};
  } finally {
    // Join all in-flight round tasks before reaching cleanup; closing a worker
    // rolls back any transaction retained by a fatal error. No error is replayed.
    await Promise.all(clients.map(client=>client.close()));
  }
}

async function child(config) {
  const progress = progressReporter(config.connectionCount);
  progress('load installed package');
  const {Database, AsyncDatabase, Record, Vector} = require(config.package);
  const file = path.join(config.directory, 'application.db');
  const revisions = new Uint32Array(config.rows);
  const record = n => new Record('items', String(n).padStart(6, '0'));
  const vector = (n,revision=revisions[n]) => { const values = new Float32Array(config.dimensions); values[0] = n; values[config.dimensions-1] = revision / 10000; return Vector.float32(values); };
  const point = n => ({type: 'Point', coordinates: [100, 13 + n / 100000]});
  const text = (n,revision=revisions[n]) => `common token${n} revision${revision} ` + 'application document text '.repeat(8);
  const timed = fn => { const start = performance.now(); const value = fn(); return {ms: performance.now()-start, value}; };
  const clients = [];
  let worker;
  const sql = {
    read: 'SELECT n,revision FROM items WHERE id=$id',
    write: 'UPDATE items SET revision=$revision,body=$body,v=$v,location=$point WHERE id=$id',
    text: "SELECT id,score FROM search::text('items_text',$query,10) ORDER BY score DESC,id",
    vector: "SELECT id,distance FROM search::vector('items_vector',$v,10) ORDER BY distance,id",
    spatial: "SELECT id,distance_m FROM search::near('items_location',$point,0) ORDER BY distance_m,id",
  };
  let setup;
  try {
    progress('setup: open/create');
    let db = new Database(file);
    clients.push(db);
    db.execute('PRAGMA synchronous=FULL');
    db.execute('CREATE TABLE items');
    progress('setup: seed begin');
    const seed = timed(() => {
      for (let offset = 0; offset < config.rows; offset += 100) {
        db.execute('BEGIN');
        for (let n = offset; n < Math.min(config.rows, offset+100); n++) {
          db.execute('INSERT INTO items {id:$id,n:$n,revision:0,body:$body,v:$v,location:$point}',
            {$id: record(n), $n: BigInt(n), $body: text(n), $v: vector(n), $point: point(n)});
        }
        db.execute('COMMIT');
        progress('setup: seed',Math.min(config.rows,offset+100),config.rows);
      }
    });
    const indexes = {};
    for (const [name, statement] of [
      ['text', 'CREATE SEARCH INDEX items_text ON items(body) USING FULLTEXT'],
      ['vector', `CREATE SEARCH INDEX items_vector ON items(v) USING VECTOR WITH (dimensions=${config.dimensions},metric='l2')`],
      ['spatial', 'CREATE SEARCH INDEX items_location ON items(location) USING SPATIAL'],
    ]) {
      progress(`setup: ${name} index begin`);
      indexes[name] = {...timed(() => db.execute(statement)), memory: memory(), files: files(file)};
      progress(`setup: ${name} index complete`);
    }
    progress('setup: checkpoint');
    const initialCheckpoint = timed(() => db.all('PRAGMA wal_checkpoint(TRUNCATE)'));
    assert.deepEqual(initialCheckpoint.value, [[0n,0n,0n]]);
    setup = {seedMs: seed.ms, indexes, checkpoint: initialCheckpoint, memory: memory(), files: files(file)};
    db.close(); clients.pop();

    progress('open measured connections');
    const openStart = performance.now();
    for (let i = 0; i < config.connectionCount; i++) {
      const connection = new Database(file);
      connection.execute('PRAGMA synchronous=FULL');
      clients.push(connection);
    }
    const openMs = performance.now()-openStart;
    db = clients[0];
    const counters = {read: [], write: [], text: [], vector: [], spatial: []};
    const recalls = [];
    function operation(connection, kind, n, broad = false) {
      const id = record(n);
      let params;
      if (kind === 'write') { revisions[n]++; params = {$id: id, $revision: BigInt(revisions[n]), $body: text(n), $v: vector(n), $point: point(n)}; }
      else if (kind === 'read') params = {$id: id};
      else if (kind === 'text') params = {$query: broad ? 'common' : `token${n}`};
      else if (kind === 'vector') params = {$v: vector(n)};
      else params = {$point: point(n)};
      const result = timed(() => connection.execute(sql[kind], params));
      const rows = result.value.rows;
      if (kind === 'read') assert.deepEqual(rows, [[BigInt(n), BigInt(revisions[n])]]);
      else if (kind === 'write') assert.equal(result.value.affected, 1n);
      else if (kind === 'text') {
        assert.equal(rows.length, broad ? Math.min(10, config.rows) : 1);
        if (!broad) assert.deepEqual(rows[0][0], id);
        else assert.deepEqual(rows.map(([hit]) => hit), Array.from({length:10}, (_, i) => record(i)));
        for (const [hit, score] of rows) { assert.equal(hit.table, 'items'); assert(Number(hit.key) < config.rows); assert(Number.isFinite(score) && score > 0); }
      } else if (kind === 'spatial') assert.deepEqual(rows, [[id, 0]]);
      else {
        assert.deepEqual(rows[0], [id, 0]);
        assert.equal(new Set(rows.map(([hit]) => hit.key)).size, 10);
        const distance = other => Math.hypot(other-n, Math.fround(revisions[other]/10000)-Math.fround(revisions[n]/10000));
        for (const [hit, actual] of rows) assert(Math.abs(actual-distance(Number(hit.key))) <= 1e-7, 'ANN returned distance differs from brute-force distance');
        const exact = Array.from({length: config.rows}, (_, i) => i).sort((a,b) => distance(a)-distance(b) || a-b).slice(0,10);
        recalls.push(rows.filter(([hit]) => exact.includes(Number(hit.key))).length / 10);
      }
      return result.ms;
    }
    const cold = clients.map((connection, i) => Object.fromEntries(['read','text','vector','spatial'].map(kind => {
      progress(`cold: connection ${i+1} ${kind}`);
      return [kind, operation(connection,kind,i)];
    })));
    progress('verify index plans');
    const plans = {};
    for (const kind of ['text','vector','spatial']) plans[kind] = db.all('EXPLAIN QUERY PLAN ' + sql[kind],
      kind === 'text' ? {$query:'common'} : kind === 'vector' ? {$v:vector(0)} : {$point:point(0)});
    assert(encode(plans.text).includes('QUERY INDEX METHOD fts'));
    assert(encode(plans.vector).includes('__romedb_ann_hnsw_hits'));
    assert(/SEARCH .*INDEX/.test(encode(plans.spatial)), 'spatial query must use an index');
    const before = {memory: memory(), files: files(file)};
    let peakWalBytes = before.files.walBytes;
    const series = [];
    const mix = ['read','read','read','read','write','write','text','text','vector','spatial'];
    progress('serial mix begin');
    const start = performance.now();
    for (let i = 0; i < config.operations; i++) {
      const kind = mix[i % mix.length], n = (Math.imul(i, 7919) >>> 0) % config.rows;
      counters[kind].push(operation(clients[i % clients.length], kind, n, i % 10 === 7));
      peakWalBytes = Math.max(peakWalBytes, size(file+'-wal'));
      if (i % 100 === 0) series.push({operations: i+1, elapsedMs: performance.now()-start, ...memory(), ...files(file)});
      progress('serial mix',i+1,config.operations);
    }
    const elapsedMs = performance.now()-start;
    const after = {memory: memory(), files: files(file)};
    progress('serial integrity');
    assert.equal(db.exactlyOne('SELECT count(*) FROM items')[0], BigInt(config.rows));
    const audit = timed(() => db.checkCollectionIntegrity('items'));
    assert.equal(audit.value.documents, BigInt(config.rows));
    progress('serial checkpoint');
    const checkpoint = timed(() => db.all('PRAGMA wal_checkpoint(TRUNCATE)'));
    assert.deepEqual(checkpoint.value, [[0n,0n,0n]]);
    const afterCheckpoint = {memory: memory(), files: files(file)};
    while (clients.length) clients.pop().close();

    progress('cancellation/retry');
    worker = await AsyncDatabase.open(file);
    await worker.execute('CREATE TABLE cancellation_input(n)');
    await worker.execute('INSERT INTO cancellation_input VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9)');
    await worker.execute('BEGIN');
    const cancellationStart = performance.now();
    await assert.rejects(worker.execute('SELECT count(*) FROM cancellation_input a,cancellation_input b,cancellation_input c,cancellation_input d,cancellation_input e,cancellation_input f,cancellation_input g,cancellation_input h,cancellation_input i,cancellation_input j', {}, {timeoutMs: 20}),
      error => error.code === 'FDB_CANCELLED' && error.transaction.after === 'active');
    const cancellationMs = performance.now()-cancellationStart;
    const retryStart = performance.now();
    assert.deepEqual(await worker.exactlyOne('SELECT count(*) FROM items'), [BigInt(config.rows)]);
    const retryMs = performance.now()-retryStart;
    await worker.execute('ROLLBACK');
    await worker.execute('DROP TABLE cancellation_input');
    await worker.close(); worker = undefined;
    const concurrent = await concurrentWorkload(config,file,revisions,{AsyncDatabase},{record,vector,text,point},progress);
    progress('reopen: integrity');
    db = new Database(file); clients.push(db);
    const reopenStart = performance.now();
    assert.equal(db.checkCollectionIntegrity('items').documents, BigInt(config.rows));
    progress('reopen: verify documents');
    for (let n = 0; n < config.rows; n++) {
      assert.deepEqual(db.exactlyOne(sql.read, {$id: record(n)}), [BigInt(n), BigInt(revisions[n])]);
      progress('reopen: documents',n+1,config.rows);
    }
    progress('correctness passed');
    return {connectionCount: config.connectionCount, setup, openMs, cold, plans, before, after, afterCheckpoint, peakWalBytes,
      elapsedMs, operationsPerSecond: config.operations/(elapsedMs/1000), operations: config.operations,
      latency: Object.fromEntries(Object.entries(counters).map(([key, values]) => [key, summarize(values)])),
      annRecall: {min: Math.min(...recalls), mean: recalls.reduce((a,b) => a+b,0)/recalls.length, samples: recalls.length},
      audit, checkpoint, concurrent, cancellation: {deadlineMs: 20, elapsedMs: cancellationMs, retryMs, transactionPreserved: true},
      reopenVerificationMs: performance.now()-reopenStart, series, final: {memory: memory(), files: files(file)}, correctness: 'passed'};
  } finally {
    if (worker) await worker.close();
    while (clients.length) clients.pop().close();
  }
}

async function main() {
  assert(process.platform === 'linux' && process.arch === 'x64', 'Qualified harness requires Linux x64');
  if (process.argv[2] === '--child') { console.log(encode(await child(JSON.parse(fs.readFileSync(process.argv[3], 'utf8'))))); return; }
  const options = parse(), bundle = path.resolve(options.bundle), output = path.resolve(options.output);
  assert(!fs.existsSync(output), 'Output already exists');
  assert(!output.startsWith(bundle + path.sep), 'Evidence must be outside the bundle');
  const manifest = JSON.parse(fs.readFileSync(path.join(bundle, 'manifest.json'), 'utf8'));
  const expected = new Map();
  for (const line of fs.readFileSync(path.join(bundle, 'SHA256SUMS'), 'utf8').trim().split('\n')) {
    const [digest, name] = line.split('  '), file = path.resolve(bundle,name);
    assert(file.startsWith(bundle+path.sep) && !expected.has(name) && hash(file) === digest, 'Invalid bundle checksum: '+name);
    expected.set(name,digest);
  }
  assert(expected.has('manifest.json'), 'Manifest must be checksummed');
  if (!options.rehearsal) {
    assert.equal(manifest.buildProfile, 'romedb-production', 'Production evidence requires the production profile; use --rehearsal only to test the harness');
    assert.equal(manifest.rustProfilePolicy['debug-assertions'], true);
    assert.equal(manifest.rustProfilePolicy['overflow-checks'], true);
    assert.equal(manifest.sourceSnapshot, false, 'Production evidence requires committed source');
  }
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'romedb-production-bench-'));
  try {
    const consumer = path.join(temporary,'consumer'); fs.mkdirSync(consumer);
    fs.writeFileSync(path.join(consumer,'package.json'), '{"private":true}');
    const archive = path.join(bundle, 'packages', `romedb-node-${manifest.version}.tgz`);
    assert(expected.has(path.relative(bundle,archive)), 'Node package must be checksummed');
    execFileSync('pnpm', ['add','--offline','--ignore-scripts',archive], {cwd:consumer,stdio:['ignore',process.stderr,process.stderr]});
    const installed = fs.realpathSync(path.join(consumer,'node_modules/@romedb/node'));
    const extract = path.join(temporary, 'expected'); fs.mkdirSync(extract);
    execFileSync('tar', ['-xzf',archive,'-C',extract,'package/romedb.node']);
    const addonSha256 = hash(path.join(installed,'romedb.node'));
    assert.equal(addonSha256, hash(path.join(extract,'package/romedb.node')));
    const samples = [];
    for (const connectionCount of options.connections) for (let sample = 0; sample < options.samples; sample++) {
      const directory = path.join(temporary, `${connectionCount}-${sample}`); fs.mkdirSync(directory);
      const config = {...options, package:installed, directory, connectionCount};
      const configPath = path.join(directory,'config.json'); fs.writeFileSync(configPath, encode(config));
      process.stderr.write(`Benchmark: ${options.rows} documents, ${options.dimensions} dimensions, ${connectionCount} connections, sample ${sample+1}\n`);
      const result = execFileSync(process.execPath, [__filename,'--child',configPath], {encoding:'utf8',stdio:['ignore','pipe','inherit'],timeout:1200000,maxBuffer:32*1024*1024});
      samples.push({sample, ...JSON.parse(result)});
      fs.writeFileSync(output+'.partial', encode(samples));
    }
    const report = {schemaVersion:2, productionEvidence:!options.rehearsal, sourceCommit:manifest.sourceCommit,
      buildProfile:manifest.buildProfile, version:manifest.version, bundleManifestSha256:hash(path.join(bundle,'manifest.json')),
      bundleChecksumsSha256:hash(path.join(bundle,'SHA256SUMS')), packageSha256:hash(archive), addonSha256,
      harnessSha256:hash(__filename), node:process.versions.node, platform:os.version(), kernel:os.release(),
      cpu:os.cpus()[0].model, logicalCpus:os.cpus().length, hostMemoryBytes:os.totalmem(), options,
      notes:[
        'One owning process; synchronous connections execute serially in round-robin order. This measures cache/resource multiplication, not concurrent throughput.',
        'The separate concurrent phase uses AsyncDatabase worker threads inside that same process, with one writer and remaining connections as readers. Each bounded round overlaps one write transaction with all reader transactions; a barrier prevents unbounded queues and keeps readers active throughout writes.',
        'Concurrent transaction latency includes consistency assertions, Busy handling and backoff. Only explicit Busy codes are retried at confirmed transaction boundaries; active COMMIT Busy retries the same COMMIT and never replays writes after an ambiguous outcome.',
        'A separate forced contention probe records one rejected competing writer and a retry after confirmed lock release. It is reported separately from natural workload contention. Final integrity/reopen checks include all concurrent writes.',
        'Cold means newly opened RomeDB connections after a checkpoint. OS filesystem cache is not evicted; this is not disk-cold latency.',
        '40% document reads, 20% indexed updates, 20% FTS (half corpus-wide), 10% ANN, 10% spatial. FULL synchronous; the pinned core automatically checkpoints above 1000 unbackfilled WAL frames. No checkpoint-threshold PRAGMA override is used.',
        'Per-operation latency includes frontend, engine and Node transport; assertions follow timing. Sustained throughput includes assertions and resource sampling.',
        'Lightweight phase/progress logs go to stderr. Individual operation timers exclude logging; overall setup/workload/reopen timers include progress logging.',
        'Linux peak RSS includes startup, corpus/index construction and Node overhead. WAL peak is sampled after every operation, so transient within-operation peaks may be missed.',
        'ANN recall is against exact generated-data distances; this synthetic distribution does not establish recall for application embeddings.',
        'Cancellation tests the native VM deadline and successful retry; ANN/FTS native-call preemption and hard cancellation bounds are not claimed.',
        'Parameter caps bound the experiment only. They are not a claim that every allowed combination is a supported production envelope.'
      ],samples};
    fs.writeFileSync(output, encode(report)+'\n', {flag:'wx'}); fs.unlinkSync(output+'.partial');
    console.log(output);
  } finally { fs.rmSync(temporary,{recursive:true,force:true}); }
}
main().catch(error => { console.error(error); process.exitCode=1; });
