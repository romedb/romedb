# Nested object, array and record projections

Requested after the 2.0.0 release; these additions are not in the published
2.0.0 binaries. SQL table/alias stars and positional result rows retain their
existing meanings.

The 2.2.0 working tree also accepts an explicit alias on an unqualified field
wildcard in an ordinary SELECT list:

```sql
SELECT id, title, author.* AS writer
FROM articles
WHERE author = type::record('writers', $writer)
ORDER BY id;

SELECT a.id, a.title, a.author.* AS writer
FROM articles AS a
WHERE a.author = type::record('writers', $writer)
ORDER BY a.id LIMIT 20;
```

For a scalar reference, `author.* AS writer` equals
`record::fetch(author) AS writer`. It contributes one positional column, not
one column per target field. Plain `author` retains its typed record value.
Object, array, null, missing-target and scalar behavior follows the same path
rules below. Source filters, ordering and pagination apply before expansion;
they can use the existing managed indexes. In joins, qualify the field.

Source names and aliases still take precedence: `author.* FROM articles author`
selects the whole source. SQL source stars cannot be renamed with `AS writer`;
use `author.author.* AS writer` to select that source's reference field. SQL
identifier quoting remains unchanged, including quoting reserved field names.
Whitespace and comments around the dot/star are accepted.

Installed 2.1.0 rejects the first example at `AS` but accepts the second. This
was a parser/lowering gap in the unqualified aliased form, not a missing record
resolver. The fix uses the existing snapshot, traversal and result-limit paths.
See [2.2.0 working evidence](v2.2-tasks.md); exact release artifact and client
qualification is still pending.

- [x] Verify wildcard behavior against SurrealDB 3.2.4.
- [x] Implement object/array wildcards, bracket and dot array positions, and
  batched record-link traversal within the source query's transaction snapshot.
- [x] Cover linked/mixed arrays, missing positions, quoted keys, SQL aliases,
  duplicate output names, typed values, persistence and result limits.
- [x] Verify Rust and both Node clients; record scoped acceptance evidence.

## Contract

For `posts: [posts:p1, posts:p2]`:

| Projection | Result |
| --- | --- |
| `posts` | Stored array of typed IDs |
| `posts[0]` or `posts.0` | First typed ID |
| `posts[$]`, `posts[-1]` or `posts.-1` | Last typed ID |
| `posts[-2]` or `posts.-2` | Second-last typed ID |
| `posts.*` or `posts.*.*` | Array of fetched post objects |
| `posts[0].*` or `posts.0.*` | Fetched first post |
| `posts[$].*` or `posts[-1].*` | Fetched last post |
| `posts.*.author.*` | Fetch each post, then its author |

An object wildcard retains the object. An array wildcard targets each element,
fetching record elements and retaining other elements. Following path steps
apply to each selected element. Wildcards do not recursively expand every link
inside an object: a returned post's `author` remains a typed ID unless the path
explicitly traverses it. Embedded objects therefore need no fetch, and can have
the same value with or without a wildcard. Order, duplicates, nested arrays and
logical types are preserved. Missing fields, records or array positions and
wildcards on scalars become null (RomeDB has no distinct NONE value).

SurrealDB 3.2.4 was run locally as a differential oracle: `posts`, `posts.*`,
`posts.*.*`, `posts[0].*` and `posts[$].*` have the behavior above. Dot numeric
positions are a RomeQL extension. Negative bracket indexes are also an intentional
extension: SurrealDB 3.2.4 returns NONE for `posts[-1]`, whereas RomeQL counts from
the end. Bracket indexes must touch the preceding field/index (`posts[0]`);
spaced bracket forms retain existing SQL parsing. Overflowing indexes are rejected.

Existing table aliases take precedence over same-named fields for `alias.*`.
Use an explicit source qualifier (`u.posts.*`) to remove that ambiguity. Explicit
AS controls output column names; duplicate names retain separate positions.
This is projection syntax, not complete SurrealQL compatibility: fetched paths
follow the existing top-level fetch restrictions (including no fetched DISTINCT,
RETURNING, or use of fetched aliases in filtering/grouping/ordering). Collection
INSERT SELECT can materialize paths; native INSERT SELECT does not accept fetches.

## Bounds and implementation

All target reads run in the frontend after the main engine statement, never from
an engine scalar callback. Target resolution follows existing `record::fetch`
rules for collections and supported native tables. References are deduplicated and fetched in batches per
traversal wave. The source and target reads share an atomic snapshot; failure
preserves work preceding the query in a caller transaction. Ordinary native
INSERT SELECT retains its original conflict-policy boundaries, including OR FAIL. Profile fetch counters
include target reads. Result limits count expanded values, including duplicates.

Paths have at most 64 steps, traversal has at most 1,000,000 evaluation visits,
and at most 16,384 distinct reference identities are fetched per query. Fetched
and output tagged JSON are each bounded to 64 MiB; these are logical byte bounds,
not heap caps. Existing fetch-slot limits also apply. Cancellation uses existing
cooperative engine boundaries, with no latency guarantee during Rust traversal.

Reference: [SurrealDB idioms](https://surrealdb.com/docs/reference/query-language/language-primitives/idioms).

## Verification (2026-09-25)

`bash romedb/scripts/check.sh` passed on Linux x64 with Rust 1.88.0 and Node
24.19.0: 742 Rust tests, 117 Node/application tests, three C ABI tests, scoped
formatting/Clippy and strict TypeScript. The seven record-projection tests cover
plain IDs versus fetched objects, bracket/dot/signed indexes, bounded cyclic
traversal, reference limits, batching, persistence and transaction recovery.
The existing 33-test derived suite confirms native INSERT conflict behavior,
including OR FAIL. Both synchronous and asynchronous Node clients cover the new
embedded-object and linked-record syntax.
