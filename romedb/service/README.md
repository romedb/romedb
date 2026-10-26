# RomeDB local service

The optional Linux x64 database service is included in RomeDB 2.3.0. Studio integration and delivery
remain in the [implementation checklist](../../../plan-studio.md).

Shared client qualification, from `turso`, uses the built native addon and CLI:

```sh
../scripts/romedb-heavy node romedb/service/tests/shared-client.cjs target/debug/romedb
```

This runs the same typed-value, constraint, managed-index rollback, replay,
stale-edit and limit fixtures through embedded and actual HTTP clients. It also
runs a separate URL application and verifies fixture cleanup. Vector search works;
its checked EXPLAIN path remains unsupported and is tested as an explicit error.

The same URL application can qualify a dedicated empty Cloud database. Supply
`ROMEDB_FIXTURE_URL` and `ROMEDB_FIXTURE_TOKEN` through a protected environment,
then run `node romedb/service/tests/url-client-fixture.cjs`. It refuses nonempty
databases and removes its tables after success. Failure preserves the fixture
for investigation. Do not use a customer database. The hosted common fixture and production Studio browser qualification pass,
including exact write recovery and cleanup. Local results alone do not establish
hosted compatibility; rerun the dedicated Cloud fixture for relevant changes.

From the `turso` checkout, build the native commands with the resource wrapper:

```sh
../scripts/romedb-heavy cargo build --locked -p romedb-cli --bins
target/debug/romedb serve app.romedb --create --port 4321
```

`--create` requires a missing file and creates it in MVCC mode. For later starts,
omit that flag. Ordinary startup requires an existing MVCC database. It refuses
WAL files. Use the explicit [backup, migration and restore procedure](../docs/service-migration.md)
to create a converted copy before starting the service.
Do not convert an application database as part of ordinary service startup.
Existing service database files must be regular files with one link. The service
refuses symlink and hard-link aliases because the engine derives journal paths
from the main-file name. Use the original database path and keep its journal beside it.

The service runs in the foreground on `127.0.0.1`. Startup reports the full
database URL and token-file path. It never prints the token. The default token
file is beside the database, with `.token` appended to its name. Restart reuses
that token. `--token-file PATH` selects a different credential file. Existing
credentials must be owned regular files with mode 0600 and one link; symlinks are
refused. Tokens contain 32–128 ASCII letters, digits, underscores or hyphens.

Applications and Studio use the same URL and token through a Node backend. The
service rejects browser origins, cookies and browser fetch metadata. Native file
access requires the service to stop first. `romedb-cli serve` is the compatibility
command; its existing embedded, script, SQLite adoption and Cloud commands remain.

The [version-1 protocol](../docs/service-protocol.md) defines typed results,
fresh read snapshots, atomic writes, publication sequences and durable replay.
The service retains 64 write receipts. A write and its receipt commit together.
After a lost response, recover the exact saved request; do not silently create a
replacement. Reads do not advance the publication sequence.
The shared client also sends the saved database ID in `X-RomeDB-Database-ID`.
The service rejects mismatched IDs before execution, protecting saved requests
when a port and token are reused for a different database.

Four requests can execute at once, with 32 waiting requests and five database
connections including the receipt connection. The HTTP layer admits at most 64
connections and 36 database jobs. Header and body reads have five-second limits.
Database admission, lock waits and checked execution share a 15-second deadline.
These cooperative limits do not establish a total process-memory bound.

Ctrl-C or SIGTERM stops new requests and closes the listener. Active requests
finish within their checked execution limits; waiting requests are rejected.
The HTTP drain allows 20 seconds per connection. Shutdown then waits for all
admitted database jobs, including jobs whose clients disconnected. The deadline
checks cannot interrupt a stalled filesystem call. Restart opens the database and validates its
receipt history before accepting requests. Publication or rollback uncertainty
stops further database operations until the owner is stopped and recovered.

Run focused service tests from the `turso` checkout:

```sh
../scripts/romedb-heavy cargo test --locked -p romedb-service
../scripts/romedb-heavy cargo test --locked -p romedb-cli --test serve
../scripts/romedb-heavy cargo test --locked -p romedb-cli --test migration
../scripts/romedb-heavy node romedb/service/tests/sdk.cjs target/debug/romedb
```

The SDK fixture uses the development client without a native Node addon. It
starts a native service and two independent Node client processes, then checks
typed data, lost-response recovery, restart, rollback and stale edits. Temporary
databases and journals stay under the parent `.local/tmp` directory and are
removed after the run. These local tests do not establish live Cloud compatibility.
