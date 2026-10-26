'use strict';
// Linux maintainer diagnostic. Build the local addon first; no registry access.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {execFileSync} = require('node:child_process');
const {createHash} = require('node:crypto');
const {performance} = require('node:perf_hooks');
const root = path.resolve(__dirname,'../..');
if (process.platform !== 'linux') throw new Error('This diagnostic currently records Linux maxRSS units only');
const memory = () => ({rssBytes:process.memoryUsage().rss, processPeakRssKiB:process.resourceUsage().maxRSS});

if (process.argv[2] === '--child') {
  const [format,file,countText,widthText] = process.argv.slice(3);
  const count=Number(countText),width=Number(widthText);
  const {Database}=require('../bindings/node/index.cjs');
  const db=new Database();
  try {
    db.execute('CREATE TABLE docs');
    db.execute('CREATE UNIQUE INDEX docs_n ON docs(n)');
    const input=fs.readFileSync(file,'utf8');
    const before=memory();
    const start=performance.now();
    assert.equal(db.importDocuments('docs',input,format).imported,count);
    const importMs=performance.now()-start;
    const afterImport=memory();
    const exportStart=performance.now();
    const output=db.exportDocuments('docs',format);
    const exportMs=performance.now()-exportStart;
    const afterExport=memory();
    // Correctness checks run after recording phase measurements.
    assert.deepEqual(db.exactlyOne('SELECT count(*),sum(n),min(length(text)),max(length(text)) FROM docs'),[BigInt(count),BigInt(count*(count-1)/2),BigInt(width),BigInt(width)]);
    const audit=db.checkCollectionIntegrity('docs');
    assert.equal(audit.documents,BigInt(count));
    assert.equal(audit.indexEntries,BigInt(count));
    const other=new Database();
    try {
      other.execute('CREATE TABLE docs');
      assert.equal(other.importDocuments('docs',output,format).imported,count);
      assert.equal(other.exportDocuments('docs',format),output);
    } finally {other.close();}
    console.log(JSON.stringify({format,count,textBytesPerDocument:width,inputBytes:Buffer.byteLength(input),outputBytes:Buffer.byteLength(output),importMs,exportMs,before,afterImport,afterExport,correctness:'passed'}));
  } finally {db.close();}
} else {
  const count=Number(process.argv[2] ?? 1000),width=Number(process.argv[3] ?? 4096);
  if (!Number.isSafeInteger(count)||count<1||count>10000||!Number.isSafeInteger(width)||width<1||width>16384||count*(width+256)>16*1024*1024) throw new RangeError('Use 1..10000 rows, 1..16384 text bytes, with count*(width+256) <= 16 MiB');
  const temporary=fs.mkdtempSync(path.join(os.tmpdir(),'romedb-transfer-bench-'));
  try {
    const header={format:'romedb.documents',version:1};
    const documents=Array.from({length:count},(_,n)=>({type:'Object',value:{id:{type:'Record',value:{table:'docs',key:{type:'String',value:`r${n}`}}},n:{type:'Integer',value:String(n)},text:{type:'String',value:'x'.repeat(width)}}}));
    const samples=[];
    for (const format of ['json','ndjson']) {
      const file=path.join(temporary,format);
      const payload=format==='json'?JSON.stringify({header,documents}):[JSON.stringify(header),...documents.map(value=>JSON.stringify(value))].join('\n')+'\n';
      fs.writeFileSync(file,payload);
      samples.push(JSON.parse(execFileSync(process.execPath,[__filename,'--child',format,file,String(count),String(width)],{cwd:root,encoding:'utf8',timeout:120000,maxBuffer:1024*1024})));
    }
    const git=args=>execFileSync('git',args,{cwd:root,encoding:'utf8'}).trim();
    console.log(JSON.stringify({platform:process.platform,arch:process.arch,node:process.versions.node,sourceCommit:git(['rev-parse','HEAD']),implementationChanges:git(['status','--porcelain','--','romedb/frontend','romedb/bindings']),addonSha256:createHash('sha256').update(fs.readFileSync(path.join(root,'romedb/bindings/node/romedb.node'))).digest('hex'),harnessSha256:createHash('sha256').update(fs.readFileSync(__filename)).digest('hex'),notes:'Single debug-addon sample per format, fresh process per format. Peak RSS is cumulative from child startup and includes import during export. Input and output coexist. Correctness work follows measurements. No forced GC or release performance guarantee.',samples},null,2));
  } finally {fs.rmSync(temporary,{recursive:true,force:true});}
}
