'use strict';
// Maintainer diagnostic using the locally built addon; no registry access.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {execFileSync} = require('node:child_process');
const {createHash} = require('node:crypto');
const {performance} = require('node:perf_hooks');
const {Database} = require('../bindings/node/index.cjs');
const count = Number(process.argv[2] ?? 1000);
if (!Number.isSafeInteger(count) || count < 130 || count > 8000) throw new RangeError('Use 130..8000 positions');
const root = path.resolve(__dirname, '../..');
const db = new Database();
const workloads = [];
try {
  db.execute('CREATE TABLE positions(n INTEGER PRIMARY KEY)');
  db.execute('CREATE TABLE docs');
  db.execute('INSERT INTO positions VALUES ' + Array.from({length:count}, (_,n)=>'('+n+')').join(','));
  db.execute("INSERT INTO docs(id,n) SELECT type::record('docs',n),n FROM positions");
  for (const target of ['docs','positions']) {
    for (const distinct of [...new Set([1,130,count])]) {
      for (const projections of [1,2]) {
        const columns = Array.from({length:projections}, (_,i)=>`record::fetch(type::record('${target}',n%${distinct})) AS fetched${i}`);
        const sql = 'SELECT '+columns.join(',')+' FROM positions ORDER BY n';
        const samples=[];
        let referenceMetrics;
        for (let sample=0;sample<4;sample++) {
          const started=performance.now();
          const profile=db.profileSelect(sql);
          const elapsedMs=performance.now()-started;
          assert.equal(profile.result.rows.length,count);
          for (let n=0;n<count;n++) {
            for (const value of profile.result.rows[n]) assert.equal(value.n,BigInt(n%distinct));
            if (projections===2) assert.deepEqual(profile.result.rows[n][0],profile.result.rows[n][1]);
          }
          assert.equal(profile.metrics.fetchBatches,BigInt(Math.ceil(distinct/128)));
          assert(profile.metrics.fetchRowsRead>=BigInt(distinct));
          assert(profile.metrics.fetchVmSteps>0n);
          if (referenceMetrics) assert.deepEqual(profile.metrics,referenceMetrics);
          referenceMetrics=profile.metrics;
          if (sample>0) samples.push({elapsedMs,metrics:profile.metrics});
        }
        workloads.push({target,distinct,projections,sql,samples,correctness:'passed'});
      }
    }
  }
} finally {db.close();}
const git = args => execFileSync('git',args,{cwd:root,encoding:'utf8'}).trim();
const hash = file => createHash('sha256').update(fs.readFileSync(file)).digest('hex');
console.log(JSON.stringify({platform:process.platform,arch:process.arch,node:process.versions.node,count,
  sourceCommit:git(['rev-parse','HEAD']),implementationChanges:git(['status','--porcelain','--','romedb/frontend','romedb/bindings']),
  addonSha256:hash(path.join(root,'romedb/bindings/node/romedb.node')),harnessSha256:hash(__filename),
  notes:'In-memory debug addon. One warmup and three measured samples per workload, one process for all workloads. Timing includes decoding and Node transport; assertions follow timing. Counters are strings encoding uint64. Not release latency, memory or cold-cache evidence.',workloads},(_,v)=>typeof v==='bigint'?v.toString():v,2));
