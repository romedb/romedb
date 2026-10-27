# Offline backup and restore rehearsal

The embedded engine has a tested offline, checkpointed file-copy procedure on the pinned engine. This preserves the whole database: relational schema/data, collection metadata/documents, managed indexes, and migration history. Collection JSON/NDJSON export is a data transfer format and does not contain all of those objects.

This procedure requires exclusive maintenance access. It does not provide an online backup API or coordinate other processes for you.

1. Stop application access, including other processes, workers and connections. Resolve outstanding transactions explicitly. Keep access stopped until the copy has completed.
2. Open one maintenance connection to the source database and execute `PRAGMA wal_checkpoint(TRUNCATE)`. Check that it succeeds and returns `busy=0`, `log=0`, `checkpointed=0`. A busy/error result is not a completed checkpoint. Do not copy only the main file after such a result.
3. Close the maintenance connection and database. With every handle closed and no new access possible, copy the main database file to a new backup destination. Retain the original and do not overwrite an existing backup. The successful truncating checkpoint is what permits this main-file-only copy; copying an active database's main file can omit committed WAL data.
4. Record the RomeDB commit, pinned engine SHA from UPSTREAM.md, and application migration files with the backup. Protect the backup according to the data it contains. This rehearsal does not establish power-loss durability for the filesystem copy or backup storage.
5. Restore by copying the backup to a new database filename in a fresh directory. Do not reuse a target with old `-wal` or `-shm` sidecars, and do not replace an open database. Keep the backup artifact separate from the restored working copy.
6. Open the restored copy with the matching RomeDB build. Require `PRAGMA integrity_check` to return `ok`, then verify application records, validation definitions, index lookups and migration history. A native integrity check alone does not establish logical collection/index consistency.
7. Exercise a normal write and rollback against the restored copy, close it, and reopen it before directing application access to it. Reapplying the exact original migration plan must report its entries as already applied.

The executable rehearsal is `checkpointed_offline_backup_restores_schema_values_indexes_and_history` in `romedb/tests/tests/persistence.rs`. Run it from the repository root with the configured Rust toolchain:

```sh
cargo test --locked -p romedb-tests --test persistence checkpointed_offline_backup_restores_schema_values_indexes_and_history
```

The test verifies a zero-length WAL after checkpoint, closes all source handles before copying, advances the original database after backup, and restores into a separate directory. It checks typed values (including int64, binary64 bits, binary, references, arrays and vectors), relational rows/views, collection metadata, required/CHECK validation, unique and nested indexes, exact migration history, rollback after update/delete, and persistent new writes. It also checks that restoration and subsequent writes leave the backup bytes unchanged.

This is a controlled same-build restore rehearsal. Interrupted copies, machine or
storage failures, cross-process contention, other platforms and online backup
are outside its evidence. Selected commit/checkpoint process-exit boundaries
have separate evidence in recovery-io-evidence.md. RomeDB V1 is released;
[the older S4 rehearsal](v1-upgrade-restore-evidence.md) is historical evidence.
Current [V1-to-V2 qualification](v2-upgrade-restore-evidence.md) passes the native
upgrade/restore rehearsal after the approved FTS backing-storage fix, including
V1 downgrade rejection and restoration from unchanged V1 and V2 backups.
Installed browser checks cover their separately identified artifact without FTS. Preserve a V1 backup before introducing V2 metadata;
opening an upgraded database in V1 is not a supported rollback procedure.
