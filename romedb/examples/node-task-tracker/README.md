# Embedded task tracker

A runnable CommonJS application template using the native asynchronous Node client. It demonstrates versioned migrations, typed records and booleans, required fields and CHECK validation, an indexed task field, one-hop owner expansion, and a transaction spanning document and relational writes.

Build the local addon from the repository root with `romedb/scripts/check-node.sh` using the documented Rust toolchain. Then run:

```sh
node romedb/examples/node-task-tracker/app.cjs ./tracker-demo.db
node --test romedb/examples/node-task-tracker/test.cjs
```

Use a new database path for the demo. It creates a person and a completed task, prints a board summary, and can be rerun without duplicating the first task. The script closes its worker in a finally block. The import points at the checkout client; a future distributed application would import `@romedb/node` after installing an appropriate native artifact.

The exported `openTracker`, `addPerson`, `addTask`, `completeTask` and `listTasks` functions are the starting point for an application's storage layer. Keep migration version/name/source unchanged after applying it; append a new migration for later schema changes. Pass application values as parameters and use Record for collection identities. JavaScript booleans remain typed when bound; native SQL TRUE/FALSE are not a substitute for typed boolean writes in this example.

`completeTask` updates the document and inserts an audit event in one explicit transaction. Its caller must exclusively own the connection until the function returns. Worker statement ordering does not isolate concurrent callers' transactions. On failure it attempts rollback and retains both errors if cleanup also fails. A commit error can have an uncertain outcome; inspect/reconcile stored state before deciding to retry.

The smoke test forces the relational event insert to fail after the document update, checks document/index rollback, retries successfully, rejects repeat completion, and reopens the file through the migration runner. It also checks required-title validation, owner expansion and NDJSON export. The example is included in the scoped Node check; it is an application starting point, not external pilot or production-release evidence.

See the [AI application guide](../../docs/ai-application-guide.md) for a runnable storage-layer walkthrough and suggested application-agent instructions.

`openTracker` closes its connection if migration fails. It rethrows the migration error when cleanup succeeds and retains both errors in an AggregateError when close also fails. The cleanup fault test uses a simulated client; it does not establish native interrupted-close durability.

## Export and restore the application

`exportTracker` returns a versioned JSON string containing typed NDJSON exports
of both collections and the relational completion events, read in one transaction.
`restoreTracker` imports that snapshot into an empty tracker initialized by
`openTracker`. It restores all three datasets in one transaction and rejects a
nonempty target. Migrations recreate validation rules and indexes before import.

```js
const fs = require('node:fs/promises');
const {openTracker, exportTracker, restoreTracker} = require('./app.cjs');

async function copyTracker() {
  const source = await openTracker('./tracker-demo.db');
  const target = await openTracker('./restored-tracker.db'); // fresh path
  try {
    const snapshot = await exportTracker(source);
    await fs.writeFile('./tracker-export.json', snapshot, 'utf8');
    await restoreTracker(target, await fs.readFile('./tracker-export.json', 'utf8'));
  } finally {
    await target.close();
    await source.close();
  }
}
copyTracker().catch(console.error);
```

Run this snippet from this example directory. Exclusively own each connection
through the entire operation; do not call these helpers inside an existing
transaction. This application snapshot is materialized in memory and is not a
physical database backup or a crash-safe file-writing utility. Keep the export
with the matching application migration version. The test verifies a late
relational failure rolls back both imported collections, a corrected retry,
exact data and index integrity after reopening, and nonempty-target rejection.
