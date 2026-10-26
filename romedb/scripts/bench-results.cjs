'use strict';
// Linux maintainer diagnostic using the local addon, isolated processes per sample.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {execFileSync} = require('node:child_process');
const {createHash} = require('node:crypto');
const {performance} = require('node:perf_hooks');
const root = path.resolve(__dirname,'../..');
if (process.platform !== 'linux') throw new Error('Linux maxRSS units required');
const memory = () => ({rssBytes:process.memoryUsage().rss,processPeakRssKiB:process.resourceUsage().maxRSS});
(async () => {
  if (process.argv[2] === '--child') {
    const [client,operation,countText,widthText,fillText] = process.argv.slice(3);
    const count=Number(countText),width=Number(widthText),fill=Number(fillText);
    const {Database,AsyncDatabase}=require('../bindings/node/index.cjs');
    const db=client==='worker'?await AsyncDatabase.open():new Database();
    try {
      await db.execute('CREATE TABLE positions(n INTEGER PRIMARY KEY)');
      await db.execute('INSERT INTO positions VALUES '+Array.from({length:count},(_,n)=>'('+n+')').join(','));
      global.gc();
      const before=memory();
      const expression=fill===0?'zeroblob('+width+')':"x'"+fill.toString(16).padStart(2,'0').repeat(width)+"'";
      const sql='SELECT n,'+expression+' AS payload FROM positions ORDER BY n';
      const started=performance.now();
      const raw=operation==='profile'?await db.profileSelect(sql):await db.execute(sql);
      const elapsedMs=performance.now()-started;
      const after=memory();
      const result=operation==='profile'?raw.result:raw;
      assert.deepEqual(result.columns,['n','payload']);
      assert.equal(result.rows.length,count);
      for (let n=0;n<count;n++) {
        assert.equal(result.rows[n][0],BigInt(n));
        const bytes=result.rows[n][1];
        assert(bytes instanceof Uint8Array);
        assert.equal(bytes.length,width);
        assert(bytes.every(byte=>byte===fill));
      }
      console.log(JSON.stringify({client,operation,count,width,fill,logicalCellBytes:count*(8+width),elapsedMs,before,after,
        metrics:operation==='profile'?raw.metrics:undefined,correctness:'passed'},(_,v)=>typeof v==='bigint'?v.toString():v));
    } finally {await db.close();}
    return;
  }
  const count=Number(process.argv[2]??1000),width=Number(process.argv[3]??4096),fill=Number(process.argv[4]??0);
  if (!Number.isSafeInteger(count)||count<1||count>10000||!Number.isSafeInteger(width)||width<1||width>65536||count*width>32*1024*1024) throw new RangeError('Use 1..10000 rows, 1..65536 bytes, at most 32 MiB payload');
  if (!Number.isInteger(fill)||fill<0||fill>255) throw new RangeError('Fill byte must be 0..255');
  const samples=[];
  for (const client of ['sync','worker']) for (const operation of ['execute','profile']) for (let sample=0;sample<3;sample++) {
    const output=execFileSync(process.execPath,['--expose-gc',__filename,'--child',client,operation,String(count),String(width),String(fill)],{cwd:root,encoding:'utf8',timeout:120000,maxBuffer:1024*1024});
    samples.push({sample,...JSON.parse(output)});
  }
  const hash=file=>createHash('sha256').update(fs.readFileSync(file)).digest('hex');
  const git=args=>execFileSync('git',args,{cwd:root,encoding:'utf8'}).trim();
  console.log(JSON.stringify({platform:process.platform,arch:process.arch,node:process.versions.node,
    sourceCommit:git(['rev-parse','HEAD']),implementationChanges:git(['status','--porcelain','--','romedb/frontend','romedb/bindings']),
    addonSha256:hash(path.join(root,'romedb/bindings/node/romedb.node')),harnessSha256:hash(__filename),
    notes:'Local debug addon. Three fresh processes per client/operation. Setup precedes baseline; GC requested before measurement. maxRSS includes process startup/setup and worker memory. Timing includes query, portable JSON transport and decoding; assertions follow measurement. No before/after comparison, release guarantee or memory cap.',samples},null,2));
})().catch(error=>{console.error(error);process.exitCode=1;});
