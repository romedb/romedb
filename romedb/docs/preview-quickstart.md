# RomeDB V1 preview quickstart

An embedded evaluation preview for Linux x64, Node 22/24 and Rust 1.88.0.
No cloud account is needed. The manifest identifies the exact source and build.
The supplied native binaries use the debug build profile: evaluate functionality,
not production performance. This is not a stable V1 or production durability claim.

Run these commands inside the extracted candidate directory.

## Verify and use the CLI

```sh
sha256sum -c SHA256SUMS
printf "CREATE TABLE notes; INSERT INTO notes {id:notes:first,text:'hello'}; SELECT notes:first;\n" | ./romedb-cli notes.db
printf 'SELECT notes:first;\n' | ./romedb-cli notes.db
```

Use a fresh path for the first command. The second database invocation demonstrates
persistence. CLI results use tagged JSON; errors exit nonzero. Source provenance
and prerequisites are recorded in `manifest.json` and `UPSTREAM.md`. The CLI
and addon require a compatible Linux dynamic loader and system libraries; only
the build host is qualified, not every Linux distribution.

## Install the Node client and run an application

```sh
npm install --no-audit --no-fund --no-save ./romedb-node-0.1.0.tgz
node tracker.cjs ./tracker.db
node tracker.cjs ./tracker.db
```

The application runs migrations, creates a person and task, completes the task
with a relational event in one transaction, and fetches the linked owner. A
second invocation reopens the database without duplicating the initial task.
The package is local; public registry installation is not advertised.

For export/restore, save this as `copy-tracker.cjs` beside `tracker.cjs`:

```js
const fs = require('node:fs/promises');
const {openTracker, exportTracker, restoreTracker, listTasks} = require('./tracker.cjs');
(async () => {
  const source = await openTracker('./tracker.db');
  let target;
  try {
    const snapshot = await exportTracker(source);
    await fs.writeFile('./tracker.json', snapshot, 'utf8');
    target = await openTracker('./restored.db'); // fresh, empty tracker
    await restoreTracker(target, await fs.readFile('./tracker.json', 'utf8'));
    console.log(await listTasks(target));
  } finally {
    try { if (target) await target.close(); } finally { await source.close(); }
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
```

Run `node copy-tracker.cjs`. The snapshot includes both collections and events;
restore rejects nonempty targets and rolls back partial imports on failure.

## Rust and pinned source

```sh
tar -xzf romedb-source.tar.gz
cd romedb-source
cargo +1.88.0 build --locked -p romedb-cli
```

Install the Rust 1.88.0 toolchain and platform build prerequisites from
`romedb/UPSTREAM.md`; building source may download locked dependencies. In a
separate Rust application, use a path dependency pointing at the extracted
`romedb-source/romedb/frontend` directory. Copy the source `Cargo.lock` to the
application before its first build to seed the pinned dependency resolutions.
The runnable consumer and detailed API example are in
`romedb-source/romedb/docs/rust-client.md`; the candidate's Rust smoke evidence
records the verified dependency identities. No crates.io package is advertised.

## Application contract and limitations

Bind values instead of interpolating them. Collection IDs are `Record` values;
use bigint for int64 and bound JavaScript booleans for typed boolean fields.
Results contain positional rows; document stars return a document-valued column.
Native SQL retains its scalar types. SDK errors expose `code` and observed
`transaction.before/after`; do not assume every error retains a transaction.
Own the connection exclusively across a multi-statement transaction. A commit
error can have an uncertain outcome: reconcile persisted state before retrying.

The preview includes typed collections, validation/indexes, transactions, one-hop
forward fetches, exact vectors, bundled string functions, migrations, transfer,
CLI and Rust/Node APIs. Existing examples/tests in the source document each.

Unsupported collection SQL forms fail explicitly; ON CONFLICT and some joined
UPDATE and index-hint forms are not available. Use the tracker queries as the
application baseline. Results and snapshots materialize in memory; input/result
limits are not a global memory cap. Cancellation of trigger-bearing writes is
not supported in this preview because of a known pinned-engine defect. No
Windows/macOS qualification, sync, inverse links, ANN/FTS/spatial indexes or
user JavaScript is included. See the source `romedb/docs/contracts.md` and
`romedb/docs/preview-release.md` for the frozen preview boundaries.
