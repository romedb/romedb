# Building applications with RomeDB V1

RomeDB is an embedded prototype using a pinned Turso SQLite dialect plus typed collections. Read [implementation status](status.md) before selecting query forms. The master plan is release scope; it is not a list of already-supported APIs. The [Node task tracker](../examples/node-task-tracker/README.md) provides a tested storage layer to adapt.

## Run a complete workflow

Build the local addon with `romedb/scripts/check-node.sh` from the repository root using the documented toolchain. This snippet also runs from the repository root. It uses an in-memory database and closes the worker before exiting.

```js
const { openTracker, addPerson, addTask, completeTask, listTasks } =
  require('./romedb/examples/node-task-tracker/app.cjs');

(async () => {
  const db = await openTracker(':memory:');
  try {
    const owner = await addPerson(db, 'sam', 'Sam');
    const task = await addTask(db, 'first', 'Review the schema', owner);
    await completeTask(db, task);
    console.log(JSON.stringify(await listTasks(db)));
  } finally {
    await db.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
```

The returned task is complete and its fetched owner name is Sam. The template smoke forces the event write to fail after the document update, then checks rollback, index integrity, retry and reopening. Run `node --test romedb/examples/node-task-tracker/test.cjs` after adapting the storage functions.

## Rules to carry into an application

| Concern | Working rule |
|---|---|
| Table model | `CREATE TABLE tasks;` creates a collection. A column list creates a relational table. `IF NOT EXISTS` does not convert models. |
| Record identity | Use `new Record('tasks', key)` for collection IDs and references. A string such as `'tasks:first'` in a bound document remains a string. Ordinary SQL primary keys keep their declared types. |
| User input | Bind values. Do not interpolate application values into SQL or build namespaced record literals from unchecked input. |
| Booleans and integers | Bind JavaScript booleans for typed boolean writes; native SQL TRUE/FALSE are not equivalent typed writes. Use bigint for lossless int64 values and counters. |
| Document writes | `INSERT INTO tasks DOCUMENT $task` accepts a bound object. Current object UPSERT uses `UPSERT people {id:$id,name:$name}`. Do not invent `UPSERT ... DOCUMENT` support. |
| Validation | Field declarations and CHECKs validate evaluated writes. Replacing definitions can fail against existing data; handle the error and preserve migration history. |
| Results | Rows are positional arrays. Use `all`, `first` or `exactlyOne` according to the required cardinality; `first` returns undefined when empty. |
| Transactions | One caller must own the connection throughout a multi-statement transaction. Worker ordering does not isolate concurrent Promise chains. Coordinate application access explicitly. |
| Batch errors | Inspect every executeBatch entry. A resolved batch may contain a final error entry and leave an explicit transaction active. |
| Cancellation | AbortSignal is cooperative. Inspect the database outcome and transaction observations; abort alone does not establish rollback or a fixed deadline. |
| Migrations | Supply the complete ordered plan; append new versions. Do not edit the name or SQL of an applied migration. The runner requires autocommit. |
| Links | `record::fetch(owner)` explicitly expands one forward hop. Use ordinary joins for relational requirements; inverse links and graph traversal are outside V1. |
| Transfer | Use exportDocuments/importDocuments for versioned typed JSON/NDJSON. A JSON.stringify display summary is not a backup or the portable transfer format. |
| Cleanup | Close clients in finally blocks. After worker/commit failures, reconcile persistent state before blindly replaying writes. |

The current native package is private. Checkout build and offline-install checks do not imply published cross-platform artifacts. See [Node client documentation](../bindings/node/README.md) and [Rust client documentation](rust-client.md) for actual signatures and platform evidence.

## Suggested application-agent instructions

Adapt this text into the application's own development instructions:

```text
Use the project's tested RomeDB client and storage-layer functions.
Consult RomeDB's implementation status before adding query syntax.
Keep database values parameterized and collection IDs typed as Record values.
Use bound booleans and bigint where the schema/client requires them.
Keep applied migration names and SQL unchanged; append a new migration.
Serialize ownership of multi-statement transactions on each connection.
Check batch error entries and transaction observations after failures.
Add a rollback/retry regression when changing a multi-write operation.
Run the application smoke test and relevant RomeDB scoped checks.
Describe remaining unsupported behavior accurately; do not promise full SQLite compatibility.
```

This guide is an initial tested application-development aid. External pilots, broader application patterns and the remaining V1 gates still require evidence.

The tracker also provides `exportTracker` and `restoreTracker` for a complete
application snapshot, including people, tasks and relational events. See the
[export/restore walkthrough](../examples/node-task-tracker/README.md#export-and-restore-the-application).
