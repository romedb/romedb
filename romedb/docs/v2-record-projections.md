# V2 record brace projections

Implemented in the V2 working tree; unavailable in released 1.0.0 artifacts.

```sql
SELECT posts:p1 { title, author.* AS writer, profile.*, profile.city AS city };
SELECT posts:p1 { * };
```

The fixed record target identifies a collection record. Results remain an ordered
column list and a zero-or-one rowset. An absent record returns no rows while
retaining the requested column names. Each projection contributes one column;
duplicate names and values retain their positions.

- `path` returns the existing typed field value; missing fields become NULL.
- `path.*` returns an embedded object unchanged, expands a typed reference to one
  document/relational record, or returns NULL for a null/missing field or absent
  referenced record. Scalar and array wildcard values are errors.
- `*` returns the entire source document as one column named `document`.
- The default name for a path or path wildcard is the dot-joined path. `AS name`
  sets an explicit name. Quoted identifiers use existing RomeQL identifier rules.

Only one reference hop is fetched. References inside an expanded document remain
typed record values. Nested braces, recursive/multi-hop wildcard expansion, arrays
of references and computed expressions are not accepted. The body is nonempty;
a trailing comma is accepted. Dynamic or multiple record targets and subsequent
WHERE/ORDER BY/LIMIT clauses are not part of this shorthand. Ordinary SELECT syntax
continues to support its existing expressions, filtering and forward fetches.

The source lookup and reference expansion share one read snapshot, including
pending writes in the caller's transaction. Repeated references are fetched in a
deduplicated batch. Collection targets use the managed ID lookup, without scanning
the collection. Relational references retain the existing forward-fetch primary-key
and value-conversion rules. No implicit inverse relationship is followed.

`execute`, `select_with_limits` and `profile_select` use the same implementation;
the corresponding synchronous and asynchronous Node methods do too. Profile
metrics distinguish source reads from batched reference reads. Parameter bindings
are unused by this grammar and are rejected rather than ignored.

Existing parser size/depth, path and fetch limits apply. Result limits charge the
column names and final values, including every occurrence of a repeated expansion;
missing targets are charged as NULL. The single source row is decoded before final
payload accounting, and reference fetching retains its own bounds. These limits
are not a total process-memory cap. A failure returns no partial result and
preserves the caller's transaction.

See [qualification evidence](v2-cell-projection-evidence.md).
