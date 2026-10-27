# Turso 0.8.1 integration

This upgrade integrates Turso release `v0.8.1`, commit
`8549c16595d2faf1bdd6ee24aee0be8bfabb3d4a`. It is an embedded engine upgrade;
RomeDB release publication and the separate Cloud service are outside its scope.
Validation status is tracked in [the checklist](turso-0.8.1-tasks.md).

## Concurrent transactions

Opt a database into Turso MVCC explicitly, before starting concurrent work:

```sql
PRAGMA journal_mode=mvcc;
BEGIN CONCURRENT;
INSERT INTO articles {id:articles:a,title:'concurrent full text'};
COMMIT;
```

Each writer uses its own connection. Existing readers retain their snapshots.
Managed full-text indexes use Turso's transactional segments and no longer
update a shared RomeDB document-count row on every write. This removes an
unnecessary conflict between writers inserting different documents into the
same index. The default journal mode is unchanged.

Conflicting writes return `FDB_WRITE_CONFLICT` for Turso write/write conflicts
or aborted commit dependencies. A caller must retry the whole transaction from
a fresh snapshot, rather than retrying only its COMMIT. Transaction metadata
continues to report whether the engine retained or ended the transaction.

## Existing full-text indexes

Turso 0.8 uses a new full-text storage format. Back up existing databases before
upgrading. Old managed full-text metadata remains readable, but searches and writes
through an old-format index report an explicit migration error while preserving
caller transactions. Rebuild each such index:

```sql
REINDEX articles_text;
```

RomeDB rebuilds from the index's stored document text, removes its legacy
counter table, and updates the catalog in the same transaction. Rollback must
restore both the old index and its metadata. New or rebuilt FTS indexes use
catalog version 4 and FTS storage version 2. Older RomeDB binaries reject this
catalog version; restore a pre-upgrade backup to downgrade. Ordinary stored
documents and relational rows retain their existing representations.

Search still uses the native full-text index and BM25 scoring. The frontend
requests all indexed matches before applying its stable score/ID ordering and
public limit; it does not substitute a document scan. Turso supplies snapshot
isolation, transactional updates and automatic segment merging.

## SQL and query planning

Native SQL now supports recursive CTEs. Recursive CTEs involving managed
collections remain outside the document lowering contract. Native SQL and
document queries use the upgraded engine's expanded window functions and frame
clauses, NULLS ordering, and optimizer.
`EXPLAIN QUERY PLAN FORMAT=JSON` is preserved through document lowering, so
applications can inspect the actual selected index. No custom optimizer switch
or alternate scan implementation is added.

As explicitly selected by the user, standalone `UPDATE` and `DELETE` statements
with trailing `ORDER BY`/`LIMIT` now follow upstream rejection. This is a
compatibility change from RomeDB 2.1.0. Rejection occurs before mutation and
preserves caller transactions. SELECT limits, including SELECT subqueries used
by writes, remain supported. To bound a write, select target IDs with an
explicit ordering in a subquery/CTE, then update/delete those IDs. Limiting a
joined source is only equivalent when that source identifies the desired
unique targets; do not mechanically move LIMIT across a join.

The release builder records the new engine SHA and a development status.
Client package versions remain unchanged on this integration branch; choosing
and publishing the next RomeDB product release is a separate step. Existing
2.1.0 release records and packaged notice inventories describe that published
release; a future package build must regenerate and audit notices for its
actual lockfile before publication.

Other native planner changes include lexical CTE resolution, nested merged-key
shadowing, scalar/compound collation propagation, and outer aggregate lifting.
The frontend adapts its SQL lowering to those rules, including a scalar scope
for UPDATE assignment subqueries so aggregate lifting cannot collapse target
rows. Differential tests compare document results with native SQL. Hash-join
plans can report fewer full-scan steps; execution metering continues to count
completed row visits.
