'use strict';
// In a distributed application, replace this checkout path with @romedb/node.
const { AsyncDatabase, Record } = require('../../bindings/node/index.cjs');

const migrations = [{
  version: 1n,
  name: 'task tracker',
  sql: `CREATE TABLE people;
CREATE TABLE tasks;
DEFINE FIELD name ON people TYPE string REQUIRED;
DEFINE FIELD title ON tasks TYPE string REQUIRED CHECK(length(trim(title)) > 0);
DEFINE FIELD done ON tasks TYPE boolean REQUIRED;
DEFINE FIELD owner ON tasks TYPE record<people> REQUIRED;
CREATE INDEX tasks_done ON tasks(done);
CREATE TABLE task_events(task_key TEXT PRIMARY KEY, kind TEXT NOT NULL);`,
}];

async function openTracker(file) {
  const db = await AsyncDatabase.open(file);
  try { await db.migrate(migrations); return db; }
  catch (error) {
    try { await db.close(); }
    catch (closeError) { throw new AggregateError([error, closeError], 'Tracker initialization and close failed'); }
    throw error;
  }
}

async function addPerson(db, key, name) {
  const id = new Record('people', key);
  await db.execute('UPSERT people {id:$id,name:$name}', {$id:id, $name:name});
  return id;
}

async function addTask(db, key, title, owner) {
  const id = new Record('tasks', key);
  await db.execute('INSERT INTO tasks DOCUMENT $task', {$task: {id, title, owner, done: false}});
  return id;
}

// The caller must serialize use of this connection for this entire operation.
// A worker serializes statements, not ownership of a multi-statement transaction.
async function completeTask(db, id) {
  await db.execute('BEGIN');
  try {
    const result = await db.execute('UPDATE tasks SET done=$done WHERE id=$id AND done=$pending', {$id:id, $done:true, $pending:false});
    if (result.affected !== 1n) throw new Error('Task is missing or already complete');
    await db.execute('INSERT INTO task_events VALUES ($key,$kind)', {$key:id.key, $kind:'completed'});
    await db.execute('COMMIT');
  } catch (error) {
    try { await db.execute('ROLLBACK'); }
    catch (rollbackError) { throw new AggregateError([error, rollbackError], 'Task operation and rollback failed'); }
    throw error;
  }
}

async function listTasks(db) {
  const rows = await db.all('SELECT id,title,done,record::fetch(owner) AS person FROM tasks ORDER BY title');
  return rows.map(([id,title,done,person])=>({id,title,done,owner:person?.name ?? null}));
}

// Exclusively own the connection until the operation finishes.
async function transferTransaction(db, operation) {
  await db.execute('BEGIN');
  try {
    const result = await operation();
    await db.execute('COMMIT');
    return result;
  } catch (error) {
    try { await db.execute('ROLLBACK'); }
    catch (rollbackError) { throw new AggregateError([error, rollbackError], 'Tracker transfer and rollback failed'); }
    throw error;
  }
}

async function exportTracker(db) {
  return transferTransaction(db, async () => JSON.stringify({
    version: 1,
    people: await db.exportDocuments('people', 'ndjson'),
    tasks: await db.exportDocuments('tasks', 'ndjson'),
    events: await db.all('SELECT task_key,kind FROM task_events ORDER BY task_key'),
  }));
}

async function restoreTracker(db, input) {
  const data = JSON.parse(input);
  if (data?.version !== 1 || typeof data.people !== 'string' || typeof data.tasks !== 'string' ||
      !Array.isArray(data.events) || data.events.some(row => !Array.isArray(row) || row.length !== 2 || row.some(value => typeof value !== 'string'))) {
    throw new Error('Invalid tracker export');
  }
  return transferTransaction(db, async () => {
    for (const table of ['people', 'tasks', 'task_events']) {
      if ((await db.exactlyOne('SELECT count(*) FROM '+table))[0] !== 0n) throw new Error('Restore requires an empty tracker');
    }
    await db.importDocuments('people', data.people, 'ndjson');
    await db.importDocuments('tasks', data.tasks, 'ndjson');
    for (const [key, kind] of data.events) {
      await db.execute('INSERT INTO task_events VALUES($key,$kind)', {$key:key, $kind:kind});
    }
  });
}

module.exports = {openTracker, addPerson, addTask, completeTask, listTasks, exportTracker, restoreTracker};

if (require.main === module) {
  (async()=> {
    if (process.argv.length !== 3) throw new Error('Usage: node app.cjs /path/to/new-demo.db');
    const db = await openTracker(process.argv[2]);
    try {
      const person = await addPerson(db, 'sam', 'Sam');
      const id = new Record('tasks','first');
      if (!await db.first('SELECT id FROM tasks WHERE id=$id', {$id:id})) {
        await addTask(db,'first','Ship the embedded prototype',person);
        await completeTask(db,id);
      }
      console.log(JSON.stringify(await listTasks(db),(_,value)=>typeof value==='bigint'?value.toString():value,2));
    } finally { await db.close(); }
  })().catch(error=>{console.error(error);process.exitCode=1;});
}
