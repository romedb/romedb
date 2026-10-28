# V2 declared inverse relationships

Implemented in the V2 working tree; unavailable in released 1.0.0 artifacts.
Focused and combined milestone checks pass.

```sql
CREATE INDEX posts_author ON posts(author);
DEFINE RELATION authored_posts ON users FROM posts.author;
SELECT u.id, relation::fetch(u.id, 'authored_posts', 20) AS posts
FROM users AS u WHERE u.id = users:u1;
```

The declaration uses an existing managed scalar index for reference equality.
Expansion runs in the frontend after the outer query, like forward fetch. The
outer SELECT and all expansion reads share one atomic read snapshot, including
pending transaction writes. No database callback runs inside an engine function.

`relation::fetch` returns one array-valued column containing source documents.
Use a returned document's typed `id` as the exclusive cursor for the next page:

```sql
SELECT relation::fetch(users:u1, 'authored_posts', 20, $after) AS posts;
INFO FOR RELATION authored_posts;
DROP RELATION IF EXISTS authored_posts;
```

Bind `$after` to NULL for the first page. Each subsequent statement sees its own
snapshot; use an explicit read transaction when pages must share a snapshot.
`INFO FOR TABLE users` lists owned relation names. Rust exposes the same
`define_relation` and `drop_relation` lifecycle operations; Rust and both Node
clients execute and profile the RomeQL query without a new value/wire format.

## Contract and qualification checklist

- [x] Persist a relation's globally unique name, target collection, source
  collection/path and selected scalar index. Validate identifiers and dependencies
  on definition and reopen. A target collection with relations requires catalog
  version 3, preventing V1 readers from silently ignoring the new metadata.
- [x] Parse DEFINE RELATION, DROP RELATION [IF EXISTS] and INFO FOR RELATION.
  Definition requires existing source/target collections and a scalar index on
  the exact source path; spatial indexes are unsuitable. Other scalar or null
  values at that path simply do not match a typed target reference. Reject removal
  of a referenced index or source collection until the relation is dropped.
  Dropping a target removes its own relation definitions, retaining weak links.
- [x] Support top-level SELECT projection
  `relation::fetch(target, name [, limit [, after]])`. The target expression returns
  a typed record in the declared target collection or NULL. A NULL target returns
  NULL; no matches return an empty array. Each match is one source document;
  links inside it remain references. No implicit parent-existence constraint.
  Relation name, limit and cursor are preparation-time constants/parameters.
- [x] Default limit 100; accept integer limits 0–1000 (including integral finite
  float64 values). The optional exclusive
  cursor is a typed source-record ID; default NULL starts the first page. Sort
  by the stable stored ID encoding, with matching cursor comparison. Document
  that this is deterministic identity order, not numeric/text key sort order.
  Use the named native index explicitly for reference equality, then fetch only
  the selected IDs. Existing key-only indexes may sort the matching ID set;
  bounded output does not promise bounded total query work. Pagination across
  separate statements sees their respective snapshots.
- [x] Inverse expansion is projection-only: no nested fetch,
  fetched-value filtering/order, DISTINCT, compounds, derived/CTE expansion or
  INSERT SELECT/RETURNING. Count expansion occurrences and payloads before
  duplicating documents; enforce the existing 16,384-position and 64 MiB fetch
  bounds per inverse resolver call and shared caller ResultLimits. Deduplicate
  identical requests. Keep ordinary forward fetch behavior unchanged.
- [x] Prove indexed selective lookup, stable pagination, typed identities,
  update/delete/index consistency, definition/drop rollback, persistence,
  interruption and result bounds. Exercise synchronous/asynchronous Node clients
  and CLI. Record actual plans/counters and full scoped milestone checks before
  closing V2-Q2.

The position bound is shared by forward and inverse projections in the outer
query. Forward and inverse resolvers each retain their 64 MiB target/output
budgets; configured ResultLimits span both. Logical nesting is limited to 64,
including the new outer result array. Array wrapping can therefore reject a
maximum-depth source document with FDB_LIMIT. Result limits account for every
expanded occurrence before duplicate output is cloned. They are not a process
memory or total work cap.

Profile `fetchBatches`, `fetchRowsRead` and `fetchVmSteps` include inverse reads.
EXPLAIN for the outer SELECT describes that statement; the separate indexed
resolver plan is qualified in the regression evidence. With many references to
one target, the key-only index can require sorting all matching IDs before LIMIT.
The query never silently scans unrelated collection documents as a substitute
for the required index.

Removing the last relation leaves catalog version 3 on its target collection;
V1 readers reject it. Source collections with only scalar indexes retain their
existing version. Keep a pre-upgrade backup when rollback to V1 is required.

This design adds an explicit inverse lookup to existing references. It does not
add referential integrity, recursive graph traversal, relationship inference,
authorization, or cloud functionality.
