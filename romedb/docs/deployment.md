# Linux deployment contract

RomeDB is an embedded document/SQL database. Deploy one owning operating-system
process per database file. That process can use supported connections and worker
threads; separate processes must not open the same live file. This applies to
all seven native clients: Rust, Node.js/TypeScript, Python, PHP, Swift, C# and Go.
Cloud hosting, browser/WASM, graph-database APIs and multiprocess WAL are outside
this contract.

## Host and artifacts

The supported binary baseline is Ubuntu 24.04 on Linux x86_64 with glibc 2.39.
Install the distribution's `libc6`, `libgcc-s1` and `libstdc++6` packages and the
runtime required by the selected client. The 2.0.0 binaries require at most
glibc 2.38 (CLI), glibc 2.35 (Node/shared library) and GLIBCXX_3.4.29; these symbol
versions are compatibility floors, not evidence for other distributions.
Alpine/musl, ARM, Windows and macOS are not qualified binary targets.

The [2.0.0 release record](release-2.0.0.md) identifies exact supported runtime
versions and package tests. That evidence was collected on Ubuntu 24.04 under
WSL2. Follow the candidate release's installed-artifact and workload evidence
before upgrading; WSL2 evidence does not establish every host's storage behavior.
Use the published checksum manifest, matching native libraries, and the supplied
notices. Source builds follow the pinned toolchain and build policy. Do not infer
an optimized build or a throughput promise from a version number.

Store the database and its WAL on persistent local storage owned by the service
account. The account needs write access to the containing directory for database
and sidecar creation. Use one stable absolute path for the file. Avoid symlink
or hard-link aliases, shared network filesystems, and sharing the directory
between replicas; these configurations are not qualified. Restrict filesystem
access to the application and backup operator. SQL access is trusted application
access: the embedded library provides no network authentication boundary.

## Application topology

| Application | Supported arrangement |
|---|---|
| Node.js/TypeScript | One application process with `Database` or `AsyncDatabase`. The latter owns its connection in a worker thread. Multiple connections remain inside the owner process. |
| Rust, Python, Go, Swift, C# | One long-lived application process owns the database; coordinate concurrent callers and transaction ownership using the client's API. |
| PHP | A long-lived PHP CLI application can own a database using the PHP client. Independent PHP-FPM workers cannot each open the same file. |
| Node cluster, PHP-FPM, multiple service replicas | Route database operations through one application-owned process, using application IPC/RPC if needed, or give each process an independent database. RomeDB does not ship a network database server or coordinate these processes. |
| CLI, migrations, backup/restore | Stop application access, close all handles, then run one maintenance process. Restart only after maintenance closes its handles. |

Connections are not transaction scopes for individual HTTP requests. Interleaving
request A's `BEGIN` with request B's writes on the same connection includes B in
A's transaction. Serialize the entire transaction at the application boundary or
give the operation a dedicated connection. Node's async worker queue serializes
individual calls; it does not isolate a multi-call transaction. A client
transaction callback, where available, has the contract described by that client.

For a Node service, create the handle once during startup:

```js
const { AsyncDatabase } = require('@romedb/node');
const db = await AsyncDatabase.open('/var/lib/myapp/app.db');
await db.execute('PRAGMA foreign_keys=ON');
await db.execute('PRAGMA synchronous=FULL');
// Start accepting application requests only after initialization succeeds.
// On shutdown: stop new requests, await active operations, resolve transactions,
// then await db.close() before handing ownership to another process.
```

`foreign_keys` is a per-connection SQL setting. If the application relies on SQL
foreign keys, enable and read back `PRAGMA foreign_keys` on every connection
before starting transactions; require `1`. Do not assume a previous connection
enabled it. Typed document references are separate values, not SQL foreign-key
constraints. FULL is the recommended durability setting; NORMAL/OFF weaken the
acknowledgment guarantee. Application shutdown must explicitly commit or roll back
transactions, and await asynchronous close.

## Lock and error handling

A second process normally fails immediately while opening, including read-only
query workloads. Do not delete the WAL, rename the live file, or disable locking
to bypass this failure. Find the existing application or maintenance owner and
perform an orderly handoff. There is no lock-file cleanup step: the OS releases
the file lock when the final owning handle closes or the process exits.

Node's synchronous constructor currently reports the native lock message with
`code: 'GenericFailure'`; `AsyncDatabase.open` reports `FDB_WORKER` with that
startup failure in its message/cause. These open failures are distinct from
`FDB_BUSY` and `FDB_BUSY_SNAPSHOT` during queries on same-process connections.
An `FDB_WORKER` after startup can mean an unknown write outcome. Do not classify
every worker failure as a harmless open rejection. See [operations](operations.md).

## Executable ownership qualification

- [x] Both Node owner APIs reject opens from both APIs in separate processes.
- [x] Rejected opens preserve the owner's active transaction and committed data.
- [x] Closing one of two in-process handles retains ownership; closing the last
  permits a different process to open, read and write the database.
- [x] `SIGKILL` of each owner API releases ownership; either client reopens with
  acknowledged commits present, pending transaction work absent, relational
  integrity and collection/index agreement intact, and subsequent writes usable.

Run `node --test romedb/bindings/node/ownership.test.cjs` against the built addon.
The four tests passed on the Ubuntu 24.04 Linux x64 development host on 2026-09-25;
the scoped Node check also runs them. These tests establish process-exit behavior
and default file ownership, not machine power-loss recovery or unrestricted
SQLite-client coexistence. Existing same-process contention tests cover busy
and stale-snapshot errors. Exact release artifact qualification remains a
separate requirement.

For an installed package, set `ROMEDB_OWNERSHIP_PACKAGE` to its absolute package
directory and run the same test file. The bundle verifier does this with both
required Node runtimes, using the package installed from the exact bundle. Its
V2 upgrade check additionally requires an explicit published 2.0.0 package and
checks its native/JavaScript hashes before creating the old-version fixture.
