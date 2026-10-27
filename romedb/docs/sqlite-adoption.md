# Adopting SQLite files

V2.1 adds a source-preserving CLI workflow. Ordinary SQLite tables stay
relational: integer/text primary keys retain their types and JSON text stays
text. Adoption does not turn rows into RomeDB document collections or typed
record IDs. The existing `--import COLLECTION` command imports RomeDB typed
JSON/NDJSON and is a different operation.

```sh
romedb-cli sqlite check application.sqlite
romedb-cli sqlite import application.sqlite application.romedb
romedb-cli application.romedb
```

Successful checks and imports return JSON. A completed compatibility check sets
`compatible:false`, lists the reasons and exits unsuccessfully when it finds an
unsupported schema. Import publishes a new destination only
after every check succeeds. Operational failures also exit unsuccessfully and
write a diagnostic to stderr. Existing destination files, symlinks, hard links
and `-wal`, `-shm`, `-journal` sidecars are refused, including when source and
destination are the same path. Use a directory controlled by the operator and
keep other processes from creating or opening the destination during adoption.

The source is opened read-only by bundled SQLite with defensive mode enabled,
trusted schema disabled and extensions unavailable. A SQLite backup transaction
copies one consistent snapshot, including committed WAL pages; an uncommitted
writer's changes are excluded. SQLite may maintain its normal WAL read-lock and
shared-memory bookkeeping. RomeDB never opens the source. A missing source,
corrupt source or source requiring write access for journal recovery is rejected.
Do not copy only the main file of a live WAL database manually.

The snapshot lives in a private temporary directory, with a mode-0600 file on Linux. The importer checks its
schema and SQLite integrity, rehearses schema creation in RomeDB, opens and
validates the snapshot in RomeDB, prepares each table/view query, checks physical
integrity and requires a completed truncate checkpoint. It then closes both
engines, verifies SQLite integrity again, syncs the file, publishes it without
replacement and syncs the destination directory. Failed checks discard the
temporary copy. A process kill may leave the private staging directory for
operator cleanup. A directory-sync error after publication retains the complete
destination and identifies it; inspect it before retrying.

The original data remains unchanged, including on failure. Copying consumes
additional disk space and a pinned read snapshot can delay SQLite checkpoint
progress, so allow space and schedule large imports appropriately. Busy locks
are retried for at most five seconds at each backup stall; copying has no fixed
database-size limit. Run application acceptance against the new file before
switching traffic. Later SQLite writes to the original are not replicated.

## Supported-schema boundary

| SQLite feature | Adoption behavior |
|---|---|
| Ordinary rowid tables; TEXT/INTEGER/REAL/BLOB/NULL, Unicode and signed 64-bit integers | Preserved, including declared key types |
| Ordinary indexes, partial/expression indexes using supported functions | Preserved; schema creation and integrity checked |
| Views and triggers using supported SQL | Preserved; views prepared; execute application-specific trigger paths before switching |
| STRICT, AUTOINCREMENT, ordinary foreign keys and cascades | Preserved; enable `PRAGMA foreign_keys=ON` on **each** application connection |
| UTF-8; ordinary SQLite page sizes | Supported; database bytes copied through SQLite's backup API |
| WITHOUT ROWID | Rejected because RomeDB UPDATE/DELETE support is incomplete |
| VIRTUAL or STORED generated columns | Rejected |
| SQLite FTS5, R-tree and other virtual/shadow tables | Rejected as a whole database; no objects are silently skipped |
| UTF-16 or auto_vacuum FULL/INCREMENTAL | Rejected; deliberately rebuild a separate SQLite copy first |
| `__romedb_*` or `__turso_internal_*` object names | Rejected; rename application collisions in a separate copy |
| Required external extensions/custom collations/functions | Rejected when schema preparation or integrity requires unavailable behavior |

This is a compatibility preflight, not proof of complete SQLite compatibility or
every possible application query. Check and import perform the same validation;
a later import gets a new snapshot and can differ if the source has changed.
SQLite integrity does not imply foreign-key validity when the application has
previously disabled enforcement; run `PRAGMA foreign_key_check` in SQLite as
part of application migration acceptance when relevant.

RomeDB can directly open supported SQLite files through every client, but direct
open writes RomeDB metadata and lacks the importer's compatibility preflight.
Use the CLI once for migration, then open the adopted destination from Rust,
Node/TypeScript, Python, PHP, Swift, C# or Go. Existing RomeDB databases should
use ordinary open and the documented upgrade/backup workflow; adoption rejects
their reserved catalog names. Keep one owning process per file, and do not mix
SQLite and RomeDB against the same live file. See [deployment](deployment.md)
and [operations](operations.md).
