# Operating RomeDB on Linux

Use the [single-owner deployment contract](deployment.md). Assign an application
owner responsible for backups, restore drills, upgrades and incident response.
RomeDB maintainers own engine updates, dependency review and the six maintained
[core exceptions](core-exceptions.md); review each exception on every upstream
sync and release, retaining its regression when an upstream fix replaces the patch.

## Backup schedule and recovery

Choose a backup interval from the application's acceptable data-loss window and
schedule a maintenance window; the supported whole-database backup is offline.
A useful starting policy is a daily backup plus a backup before each migration or
engine upgrade, with a monthly restore drill. Tighten that schedule when a day's
loss is unacceptable. Keep multiple generations on a separate failure domain,
record timestamps/checksums/build identity, and restrict access to the data.

Follow [the complete backup/restore procedure](backup-restore.md):

1. Stop new application work and drain active requests. Commit or roll back
   outstanding transactions. Close all application connections.
2. Keep application access stopped. Open one maintenance connection, run
   `PRAGMA wal_checkpoint(TRUNCATE)`, and require a successful result with
   `busy=0`, `log=0`, `checkpointed=0`. A busy or failed checkpoint is a failed
   backup attempt. Close the maintenance database and connection.
3. Copy the closed main database to a new backup destination. Keep the original.
   Record the artifact checksum, engine/build identity and exact migration files.
   Only the successful checkpoint and absence of open handles make this
   main-file-only copy valid. Resume application access after the copy completes.
4. Restore a backup to a fresh directory and filename, with no stale WAL/SHM
   sidecars. Keep the backup immutable. Use the matching RomeDB build, require
   `PRAGMA integrity_check` to return `ok`, and run collection integrity checks,
   representative indexed queries and migration-history checks.
5. Verify a normal committed write, rollback, close and reopen on the restored
   working copy. Record elapsed recovery time and test outcome before directing
   application access to the restored database.

Document JSON/NDJSON export does not include relational schema, managed index
definitions, stored functions or migration history; it is not a whole-database
backup. A copy of an active main file can omit committed WAL data. Do not copy or
delete a live WAL as an improvised backup or disk-space remedy.

## Disk, WAL and resource monitoring

Monitor filesystem free bytes/inodes, database and `-wal` sizes, process RSS,
request/queue depth, operation latency, cancellation counts, and errors by code.
Record a baseline during the candidate's stated workload. Alert before free
space becomes smaller than the expected write growth plus planned backup,
checkpoint and index-build headroom; a fixed percentage alone is insufficient.
Keep backups on separate storage when they would exhaust the database volume.

Long-running reads can retain a WAL snapshot. Keep transactions short and avoid
holding a transaction while waiting on external I/O. Use checkpoint results and
disk trends to detect growing WALs. For an explicit truncation, schedule the same
exclusive maintenance window as the backup procedure. A successful checkpoint
does not replace an independent backup.

The engine can surface a synchronous checkpoint I/O error as a completed PRAGMA
with `busy=1`, `log=NULL`, `checkpointed=NULL`. An asynchronous completion error
at the WAL barrier before database backfill can instead propagate through the
query API. Handle thrown errors and inspect any returned row: neither successful
query completion nor absence of a row proves checkpoint success. A maintenance
TRUNCATE checkpoint must return `[0,0,0]` before a backup can proceed.

Result/queue limits bound their documented payloads, not all process memory.
FTS search can allocate scratch proportional to indexed documents and materialize
all matches. ANN keeps a graph per connection and some operations load/serialize
the whole graph. Follow the qualified workload and connection limits; reducing
the returned search limit alone does not bound working memory. Profile index use,
measure RSS/latency, and account for index construction separately from reads.

If disk space or I/O errors occur, stop accepting writes and preserve database
and sidecars together. Restore storage capacity or resolve the underlying I/O
fault. Reopen with the matching build after all handles close; verify native and
collection integrity before restarting writes. Recover from a verified backup
if checks fail. Preserve a separate incident copy before attempting repairs.
See native recovery evidence for exactly which failure
paths have been exercised; process-kill tests do not establish power-loss safety.

## Transactions, errors and retries

Inspect each statement's transaction observation and result. A returned error
does not universally mean the whole transaction rolled back, nor does
`autocommit` prove whether a failed COMMIT committed. Batches are not implicitly
atomic and may contain earlier successful statements; inspect every report.
Cardinality/result conversion errors can occur after the statement executed.

| Situation | Application action |
|---|---|
| `FDB_BUSY` during a query | Resolve the conflicting same-process transaction. Retry only a known uncommitted, retry-safe operation with bounded backoff and a total deadline. |
| `FDB_BUSY_SNAPSHOT` | End the stale transaction and start a fresh one; reread inputs and retry the whole application unit if safe. Retrying just the write retains the stale snapshot. |
| Validation/constraint/UDF/result-limit error | Inspect transaction state and conflict policy. Roll back the application's unit or deliberately continue only when prior work is understood. Native SQL `OR FAIL` can preserve earlier row changes. |
| Cancellation or deadline | Inspect transaction observations. Cancellation is cooperative; completion can win the race. A client deadline is not proof of rollback. |
| Worker/process loss, transport failure, or an ambiguous commit/I/O error | Treat accepted writes as unknown until reconciled against stored application state. Reopen only after ownership is released. Do not automatically replay non-idempotent writes. |

Assign application operation IDs and enforce uniqueness in the same transaction
as the business change when retryable commands must be deduplicated. After an
unknown outcome, query that operation ID before deciding whether to repeat work.
The library performs no universal retry or exactly-once protocol for you.
The native failure matrix reproduces why this
matters: a failed WAL sync can report a COMMIT error while the complete new
transaction is present after process reopen. Treat that result as ambiguous
until application state is checked.

## Upgrade and rollback

Test the exact candidate artifacts and application migrations on a restored copy
first. Preserve the pre-upgrade backup, previous binaries, configuration and
migration files. Stop the owner process, complete a final backup, upgrade and run
integrity/application smoke checks before reopening traffic. Use one owner for
migrations and retain the exact already-applied migration source.

A binary downgrade is not a data rollback. V2 metadata intentionally rejects V1.
If a downgrade is necessary, restore the untouched pre-upgrade backup into a
fresh directory and run its compatible binary. Reconcile writes accepted since
that backup at the application level. Never run an older binary experimentally
against the only copy of an upgraded database.

## Security maintenance

The [2.1 dependency review](dependency-security.md) records the pinned advisory
scan, patched native runtimes and the retained non-applicable informational
finding. Keep service access and arbitrary SQL behind the application's own
authentication/authorization boundary, use parameters for untrusted values, and
restrict database/backup files to the service account and operator. Sandboxed
JavaScript limits are defense in depth for stored function execution; they are
not a multi-tenant access-control system.

For each release, record advisory database date/revision, exact dependency
versions and reachable package/features, findings, affected code paths, and the
resolution or explicitly accepted limitation. Review Rust dependencies plus
QuickJS/USearch and the language/build toolchain; a clean language package audit
does not cover embedded native code or system libraries. Patch candidate builds
must pass the affected regressions and installed-artifact checks. Never silently
change a published artifact; issue a new version with the affected versions and
upgrade/rollback guidance.

Report suspected security defects to the RomeDB maintainers through
[GitHub private vulnerability reporting](https://github.com/romedb/romedb/security/advisories/new),
enabled and verified on 2026-09-25. Include the RomeDB version/commit, platform,
minimal reproducer and impact; omit credentials and production data. Maintainers
should acknowledge the report, establish affected versions, reproduce privately,
coordinate a tested patch and publish an advisory with the fixed release. This
policy does not promise an unstaffed response-time SLA.
