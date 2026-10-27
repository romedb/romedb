# Graph tables and traversal

Available in RomeDB/RomeQL 2.4.0.

RomeDB stores a property graph in the same database file, transaction and backup
as other data. A node is an ordinary document record. An edge is a document in an
edge table. It has its own record ID, a `source` record, a `target` record and
any other properties.

```sql
CREATE TABLE people;
CREATE TABLE articles;
CREATE EDGE TABLE follows FROM people TO people;
CREATE EDGE TABLE wrote FROM people TO articles ON DELETE CASCADE;

INSERT INTO people:alice { name: 'Alice' };
INSERT INTO people:bob { name: 'Bob' };
RELATE people:alice -> follows -> people:bob { since: '2026-10-12', weight: 1 };

SELECT g.node, p.name, e.weight
FROM GRAPH(people:alice -> follows -> people) AS g
JOIN people AS p ON p.id = g.node
JOIN follows AS e ON e.id = g.edge
ORDER BY p.name;
```

## Edge tables

```sql
CREATE EDGE TABLE [IF NOT EXISTS] name FROM source_table TO target_table
  [ON DELETE RESTRICT | ON DELETE CASCADE];
```

Both endpoint tables must already exist as document tables. An edge table cannot
be an endpoint of another edge table. Edges are directed. Parallel edges and
self-loops are allowed. Add a unique index, for example
`CREATE UNIQUE INDEX follows_pair ON follows(source, target)`, to allow at most
one edge per ordered pair.

`ON DELETE RESTRICT` is the default. Deleting a node that still has an incident
restricting edge fails with `FDB_CONSTRAINT`. `ON DELETE CASCADE` deletes the
incident edges in the same transaction. It never deletes neighboring nodes. When
several edge tables touch a node, any restricting edge blocks the deletion.

Every edge table owns two mandatory adjacency indexes: outgoing
`(source, target, edge)` and incoming `(target, source, edge)`. They cannot be
dropped. `INFO FOR TABLE` reports `kind: "edge"` and an `edge` object with
`source`, `target`, `on_delete` and `adjacency_indexes`. Endpoint tables report
their dependent `edge_tables`. `INFO FOR DB` marks edge tables with
`kind: "edge"`, `source` and `target`.

`DROP TABLE` on an edge table removes its edges and adjacency storage atomically.
Dropping an endpoint table fails while an edge table still names it, even when
that edge table is empty.

## Creating and changing edges

```sql
RELATE source -> edge_table[:key] -> target [ { properties } | DOCUMENT $value ]
  [RETURNING projection];
```

Each endpoint is a record literal, `type::record('table', $key)` or a parameter
holding a typed record. Parameters carry values only, never table names or SQL.
Both endpoints must exist in the declared tables, including records written
earlier in the caller's transaction. A missing endpoint fails with
`FDB_CONSTRAINT`; a record in the wrong table fails with `FDB_VALIDATION`.
Properties cannot contain `id`, `source` or `target`. Without a key, the edge
receives a generated UUIDv7 string key. A duplicate edge ID or unique pair fails
with the normal conflict error.

Ordinary writes use the same checks:

- `INSERT` into an edge table requires typed `source` and `target` fields.
- `source` and `target` are immutable. `UPDATE ... SET`, `MERGE`, `UPSERT` and
  `INSERT OR REPLACE` fail with `FDB_VALIDATION` if they would change them, and
  `UNSET` cannot remove them. `CONTENT` and `PATCH` keep the stored endpoints
  when the new document omits them.
- `DELETE` removes the edge, its adjacency entries and its property indexes.
- `DEFINE FIELD` cannot redefine `source` or `target`. Strict schemas accept them.
- Imports and the Rust/SDK collection methods apply the same rules.

To move an edge, delete it and create a new one in one transaction.

Deleting a node through `INSERT OR REPLACE` with the same ID, or as a unique
conflict victim of `UPDATE OR REPLACE`, applies the deletion policy.
`UPDATE`, `UPSERT` and `ON CONFLICT DO UPDATE` change a node in place and keep
its edges.

Endpoint checks run in the writing transaction. In MVCC mode an edge write also
rewrites both endpoint rows, so a concurrent endpoint deletion conflicts instead
of leaving a dangling edge. In WAL mode the single-writer lock serializes them.

## Traversal with GRAPH(...)

`GRAPH(...)` is a row source after `FROM` or `JOIN`. Ordinary SQL joins, filters,
groups, sorts and pages its rows. Arrows have this meaning only inside `GRAPH(...)`
and `RELATE`; the JSON operators `->` and `->>` keep their SQL meaning elsewhere,
and `graph` remains an ordinary name.

```sql
-- Outgoing, incoming and either direction.
SELECT g.node FROM GRAPH(people:alice -> follows -> people) AS g;
SELECT g.node FROM GRAPH(people:bob <- follows <- people) AS g;
SELECT DISTINCT g.node FROM GRAPH(people:alice <-> follows <-> people) AS g;

-- A fixed path of two hops, with the full path.
SELECT g.node, g.nodes, g.edges
FROM GRAPH(people:alice -> follows -> people -> wrote -> articles WITH PATH) AS g;
```

| Column | Type | Meaning |
| --- | --- | --- |
| `root` | record | The start record. |
| `node` | record | The last node of the match. |
| `edge` | record or null | The last edge of the match; null at depth zero. |
| `depth` | integer | Number of edges in the match. |
| `nodes` | array of records | With `WITH PATH`: every node, root first. |
| `edges` | array of records | With `WITH PATH`: every edge; one fewer than `nodes`. |

The root is one record literal, `type::record(...)` or a parameter. A missing
root record yields zero rows. Null or a non-record value fails with
`FDB_VALIDATION`. An unknown table fails with `FDB_NOT_FOUND`. Each hop must match
its edge table's declared endpoints for the chosen direction; otherwise
preparation fails with `FDB_VALIDATION`.

A fixed path returns one row per matching sequence of edges. Different edges or
paths that reach the same node give separate rows; use `DISTINCT g.node` for
distinct nodes. Fixed paths can revisit nodes and edges. With `<->`, a self-loop
counts once per expansion. SQL output order is undefined without `ORDER BY`.
Arrays such as `nodes` cannot be sort keys.

### Step filters

```sql
SELECT g.node
FROM GRAPH(
  people:alice
  -> (follows AS f WHERE f.weight >= $minimum)
  -> (people AS p WHERE p.active = true)
) AS g;
```

An edge-step filter decides which edges are followed. A node-step filter decides
which nodes are entered and expanded. The outer `WHERE` only removes output rows.
Filters compare stored fields of that step's document with literals, parameters,
records or other fields, using `=`, `<>`, `!=`, `<`, `<=`, `>`, `>=`, `IS`,
`IS NOT`, `AND`, `OR`, `NOT` and parentheses. Comparisons follow SQL null rules
and SQLite's number < text < blob ordering. Fields may be written with the step
alias, the table name, or bare. Arithmetic or functions over fields fail with
`FDB_UNSUPPORTED`; join the result instead. Each filter loads the edge or node
document once per statement.

### Recursion

```sql
-- Distinct nodes whose shortest distance is 1 to 4 edges.
SELECT g.node, g.depth
FROM GRAPH(people:alice -> follows -> people DEPTH 1 TO 4 MODE REACHABLE) AS g;

-- Simple paths of 1 to 3 edges.
SELECT g.node, g.nodes, g.edges
FROM GRAPH(people:alice -> follows -> people DEPTH 1 TO 3 MODE PATHS) AS g;
```

Recursion repeats one hop. The edge table must connect a table to itself and the
root must be in that table. `DEPTH` and `MODE` are both required; bounds count
edges, are finite nonnegative integers, and the minimum cannot exceed the maximum.
Depth zero returns the root with a null `edge`.

- `MODE REACHABLE` returns each reachable node once, at its shortest distance
  that passes the filters. The minimum bound filters that distance: a node one
  hop away is excluded from `DEPTH 2 TO 3`. `WITH PATH` adds one shortest witness
  path. Expansion is breadth-first in record-ID order, so the witness is
  repeatable within one snapshot.
- `MODE PATHS` returns every simple path in the range (no repeated node).
  Self-loops never form a simple path. It implies `WITH PATH`.

Weighted or all shortest paths, mixed recursive patterns, unbounded recursion,
graph algorithms and correlated `GRAPH(p.id ...)` sources are not available.

### Limits, plans and profiles

One statement's graph work is bounded. Exceeding a limit fails the whole
statement with `FDB_LIMIT`; a traversal is never returned truncated. An outer
`LIMIT` does not reduce graph work.

| Limit | Default |
| --- | --- |
| Fixed hops or recursion depth | 32 edges |
| Adjacency entries examined | 100,000 |
| Expansions considered | 100,000 |
| Visited and frontier nodes | 50,000 |
| Path states | 10,000 |
| Rows produced by GRAPH sources | 10,000 |
| Retained traversal bytes (estimate) | 32 MiB |

The Rust API `Connection::set_graph_limits` can lower these for later statements.
Caller execution budgets, deadlines, cancellation and result limits also apply.

Adjacency reads are batched indexed seeks. Each statement verifies that the
outgoing or incoming adjacency index is used and fails rather than scanning.
`EXPLAIN QUERY PLAN` lists `GRAPH ...` rows (root lookup, each hop's index and
filters, recursion mode and limits) before the SQL plan, without traversing.
Query profiles report `graph_adjacency_entries`, `graph_expansions`,
`graph_documents`, `graph_rows`, `graph_visited` and `graph_peak_bytes`
(Node: `graphAdjacencyEntries` and so on).

A GRAPH source runs before the consuming SQL and shares its transaction snapshot,
including the caller's pending writes. Statements with a GRAPH source run inside
one read transaction.

## File format and upgrades

Creating the first edge table writes catalog version 6 metadata and a permanent
graph marker. RomeDB 2.3.0 and older refuse to open such a file, even after every
edge table is dropped. Keep a backup made before the upgrade if you may need an
older release. Files without edge tables keep their existing format.

The collection integrity check also compares adjacency entries with edge
documents and confirms both endpoints exist.
