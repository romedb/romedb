'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {openTracker,addPerson,addTask,completeTask,listTasks,exportTracker,restoreTracker}=require('./app.cjs');

test('task tracker persists linked tasks and atomic completion events',async()=>{
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'romedb-tracker-'));
  const file=path.join(dir,'tracker.db');
  let db;
  try {
    db=await openTracker(file);
    const owner=await addPerson(db,'sam','Sam');
    await assert.rejects(addTask(db,'invalid','  ',owner));
    const id=await addTask(db,'first','First task',owner);
    // Force the second write to fail after the document update has succeeded.
    await db.execute("INSERT INTO task_events VALUES ('first','conflict')");
    await assert.rejects(completeTask(db,id));
    assert.equal((await listTasks(db))[0].done,false);
    assert.equal((await db.checkCollectionIntegrity('tasks')).indexEntries,1n);
    await db.execute("DELETE FROM task_events WHERE task_key='first'");
    await completeTask(db,id);
    await assert.rejects(completeTask(db,id),/already complete/);
    assert.deepEqual((await listTasks(db)).map(({title,done,owner})=>({title,done,owner})),[{title:'First task',done:true,owner:'Sam'}]);
    assert.deepEqual(await db.exactlyOne('SELECT count(*) FROM task_events'),[1n]);
    await db.close(); db=await openTracker(file);
    assert.equal((await listTasks(db))[0].done,true);
    assert.equal((await db.checkCollectionIntegrity('tasks')).documents,1n);
    assert.match(await db.exportDocuments('tasks','ndjson'),/First task/);
  } finally {if(db)await db.close();fs.rmSync(dir,{recursive:true,force:true});}
});


test('tracker initialization retains migration and cleanup failures',async()=>{
  const {AsyncDatabase}=require('../../bindings/node/index.cjs');
  const originalOpen=AsyncDatabase.open;
  try {
    for(const failClose of [false,true]) {
      const migrationError=Object.assign(new Error('migration failed'),{code:'FDB_MIGRATION'});
      const closeError=Object.assign(new Error('close failed'),{code:'FDB_WORKER'});
      let closed=0;
      AsyncDatabase.open=async()=>({
        migrate:async migrations=>{assert.equal(migrations[0].version,1n);throw migrationError;},
        close:async()=>{closed++;if(failClose)throw closeError;},
      });
      await assert.rejects(openTracker('unused'),error=>{
        if(!failClose)return error===migrationError;
        assert(error instanceof AggregateError);
        assert.deepEqual(error.errors,[migrationError,closeError]);
        return true;
      });
      assert.equal(closed,1);
    }
  } finally {AsyncDatabase.open=originalOpen;}
});

test('task completion only rolls back a transaction it started',async()=>{
  const {Record}=require('../../bindings/node/index.cjs');
  const id=new Record('tasks','first');
  for(const failAt of ['BEGIN','UPDATE','INSERT','COMMIT']) {
    const original=new Error(`failed ${failAt}`);
    const calls=[];
    const db={execute:async sql=>{
      const operation=sql.split(' ')[0]; calls.push(operation);
      if(operation===failAt)throw original;
      return {affected:1n};
    }};
    await assert.rejects(completeTask(db,id),error=>error===original);
    assert.deepEqual(calls,failAt==='BEGIN'?['BEGIN']:
      failAt==='UPDATE'?['BEGIN','UPDATE','ROLLBACK']:
      failAt==='INSERT'?['BEGIN','UPDATE','INSERT','ROLLBACK']:
      ['BEGIN','UPDATE','INSERT','COMMIT','ROLLBACK']);
  }
  const original=new Error('audit insert failed');
  const cleanup=new Error('rollback failed');
  const db={execute:async sql=>{
    if(sql.startsWith('INSERT'))throw original;
    if(sql==='ROLLBACK')throw cleanup;
    return {affected:1n};
  }};
  await assert.rejects(completeTask(db,id),error=>{
    assert(error instanceof AggregateError);
    assert.deepEqual(error.errors,[original,cleanup]);
    return true;
  });
});


test('tracker snapshot restores documents, links and events atomically into a fresh database', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'romedb-tracker-restore-'));
  let source, restored;
  try {
    source = await openTracker(path.join(dir, 'source.db'));
    const owner = await addPerson(source, 'sam', 'Sam');
    const done = await addTask(source, 'done', 'Complete task', owner);
    await addTask(source, 'pending', 'Pending task', owner);
    await completeTask(source, done);
    const expected = await listTasks(source);
    const snapshot = await exportTracker(source);
    restored = await openTracker(path.join(dir, 'restored.db'));
    const invalid = JSON.parse(snapshot);
    invalid.events.push(invalid.events[0]); // fail after both collection imports
    await assert.rejects(restoreTracker(restored, JSON.stringify(invalid)));
    for (const table of ['people', 'tasks', 'task_events']) {
      assert.deepEqual(await restored.exactlyOne('SELECT count(*) FROM '+table), [0n]);
    }
    assert.equal((await restored.checkCollectionIntegrity('tasks')).indexEntries, 0n);
    await restoreTracker(restored, snapshot);
    assert.deepEqual(await listTasks(restored), expected);
    assert.equal((await restored.checkCollectionIntegrity('tasks')).indexEntries, 2n);
    await assert.rejects(restoreTracker(restored, snapshot), /empty tracker/);
    await restored.close();
    restored = await openTracker(path.join(dir, 'restored.db'));
    assert.deepEqual(await listTasks(restored), expected);
    for (const table of ['people', 'tasks']) {
      assert.deepEqual(await restored.all('SELECT * FROM '+table+' ORDER BY id'), await source.all('SELECT * FROM '+table+' ORDER BY id'));
      await restored.checkCollectionIntegrity(table);
    }
    assert.deepEqual(await restored.all('SELECT * FROM task_events'), await source.all('SELECT * FROM task_events'));
  } finally {
    if (restored) await restored.close();
    if (source) await source.close();
    fs.rmSync(dir, {recursive:true, force:true});
  }
});
