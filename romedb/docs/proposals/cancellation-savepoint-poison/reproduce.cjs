const {AsyncDatabase}=require('@romedb/node');
const assert=require('node:assert/strict');
const heavyRead='SELECT count(*) FROM nums a,nums b,nums c,nums d,nums e,nums f,nums g,nums h,nums i';
const heavyWrite='INSERT INTO sink SELECT a.n FROM nums a,nums b,nums c,nums d,nums e,nums f,nums g,nums h,nums i';
(async()=>{
const results=[];
for(const [start,end] of [['BEGIN','COMMIT'],['SAVEPOINT caller','RELEASE caller']]){
  for(const [scenario,query] of [['control',null],['read_only',heavyRead],['write_only',heavyWrite]]){
    const db=await AsyncDatabase.open();
    try{
      for(const sql of ['CREATE TABLE nums(n INTEGER)','CREATE TABLE sink(n INTEGER)','CREATE TABLE prior(n INTEGER)','INSERT INTO nums VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9)',start,'INSERT INTO prior VALUES(99)']) await db.execute(sql);
      const entry={outer:start,scenario};
      if(query){
        const controller=new AbortController();
        const timer=setTimeout(()=>controller.abort(),30);
        try{
          await db.execute(query,{}, {signal:controller.signal});
          throw Error('heavy query must cancel');
        }catch(err){
          assert.equal(err.code,'FDB_CANCELLED');
          entry.cancel={code:err.code,transaction:err.transaction,message:err.message};
        }finally{clearTimeout(timer);}
      }
      entry.before_commit={sink:await db.all('SELECT count(*) FROM sink'),prior:await db.all('SELECT * FROM prior')};
      try{await db.execute(end); entry.commit='ok';}
      catch(err){entry.commit={code:err.code,transaction:err.transaction,message:err.message};}
      entry.after_commit={sink:await db.all('SELECT count(*) FROM sink'),prior:await db.all('SELECT * FROM prior')};
      results.push(entry);
    }finally{await db.close();}
  }
}
console.log(JSON.stringify(results,(_,v)=>typeof v==='bigint'?v.toString():v,2));
})().catch(err=>{console.error(err);process.exitCode=1});
