# RomeDB / RomeQL 2.4.0

Released for Linux x64 on 2026-10-12. [Download the release](https://github.com/romedb/romedb/releases/tag/fastdb-v2.4.0).

This minor release adds a property graph in the same database file, transaction
and backup as documents:

```sql
CREATE TABLE people;
CREATE EDGE TABLE follows FROM people TO people ON DELETE CASCADE;
RELATE people:alice -> follows -> people:bob { weight: 2 };

SELECT g.node, g.depth, p.name
FROM GRAPH(people:alice -> follows -> people DEPTH 1 TO 3 MODE REACHABLE) AS g
JOIN people AS p ON p.id = g.node
ORDER BY g.depth;
```

Edge tables keep immutable typed `source`/`target` records, mandatory outgoing
and incoming adjacency indexes and RESTRICT or CASCADE deletion policies.
`GRAPH(...)` supports outgoing, incoming and bidirectional hops, step filters,
fixed paths, bounded `REACHABLE`/`PATHS` recursion and `WITH PATH` arrays. Graph
work is bounded per statement and fails with `FDB_LIMIT` rather than truncating.
See the [graph contract](graph.md).

**Format.** Creating the first edge table writes catalog version 6 and a permanent
graph marker. RomeDB 2.3.0 and older refuse such files, also after every edge
table is dropped. Files without edge tables keep catalog version 5. Keep a
pre-upgrade backup if you may need an older release.

The frozen build source is `fb0ba254810177f857ea307c43fb5283de3bbca0`.
[Exact-source CI](https://github.com/romedb/romedb/actions/runs/38232774422)
passes. Local scoped checks passed 1,046 Rust tests, 185 Node/application tests,
five C ABI tests, two release-policy tests, formatting, scoped Clippy and strict
TypeScript, with no failures or skips. The production profile retains debug
assertions and overflow checks.

All 28 installed verification groups pass, including a CLI graph check, Node
22.0.0/24.19.0, CPython 3.10.21/3.12.3/3.14.7, PHP FFI, Go race detection, Swift
and .NET. Immutable 1.0.0/2.0.0/2.1.0/2.2.0/2.3.0 upgrade, candidate writes,
reopen, rollback, unchanged backups and independent restore pass; the 2.3.0
package refuses the upgraded graph file. The standalone production-profile Rust
consumer and the bounded 1,000-row application workload also pass.

A bounded graph sample with the exact Node package (10,000 nodes, 100,000 edges,
one local host) measured p50/p95 of 2.9/3.6 ms for one hop, 6.8/7.9 ms for two
fixed hops, 6.4/7.0 ms for depth-2 REACHABLE and 10.0/12.8 ms for a filtered
depth-3 REACHABLE. These are samples, not latency or capacity guarantees. The
sample tool `scripts/bench-graph.cjs` was added after the frozen build source; it
changes no shipped artifact.

The shipping dependency review covers 359 package identities with zero unresolved
findings and retains the documented lru informational exception RUSTSEC-2026-0253.
The lockfile differs from 2.3.0 only in RomeDB workspace versions.

[Public download verification](release-2.4.0-download.json) records the anonymous
download and checksums. See [qualification](release-2.4.0-qualification.json) and
[the checklist](v2.4-tasks.md). Browser/WASM, other platforms, graph algorithms,
weighted paths and package registry publication are outside this release.
