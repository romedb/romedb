# RomeDB / RomeQL 2.5.0

Released for Linux x64 on 2026-10-13. [Download the release](https://github.com/romedb/romedb/releases/tag/fastdb-v2.5.0).

This minor release adds a bounded spatial model: the PostGIS and H3 features most
applications use, without general GIS. It also makes ordinary document reads,
updates and imports faster.

```sql
CREATE SEARCH INDEX people_location ON people(location) USING SPATIAL;
CREATE SEARCH INDEX zones_shape ON zones(shape) USING SPATIAL WITH (geometry = 'region');

-- Nearest available drivers, using the index
SELECT id FROM people
WHERE available = true AND ST_DistanceSphere(location, ST_Point($x, $y)) IS NOT NULL
ORDER BY ST_DistanceSphere(location, ST_Point($x, $y)) LIMIT 5;

-- Which stored zones contain a point (reverse geofencing)
SELECT id FROM zones WHERE ST_Covers(shape, ST_Point($x, $y));
```

- Polygon and MultiPolygon values with strict GeoJSON exchange, exact
  point-in-region predicates and `ST_*` aliases for common PostGIS names.
- H3 boundary, hierarchy, disk, distance, area and compaction functions.
- A two-dimensional spatial index. Ordinary SQL radius, region and
  nearest-with-filter forms use it, as do `search::bbox`, `search::polygon` and
  `search::nearest`.
- Region indexes for stored zones, used by `search::containing` and by
  `ST_Covers(shape, point)`.
- Ordinary rewrites join on the stored ID and drop conditions the index already
  proves, instead of evaluating them again per row.
- Field reads decode one field instead of the whole document. Updates and deletes
  maintain index entries by key and skip unchanged keys. Imports no longer open a
  savepoint per document.

See the [spatial contract](spatial-upgrade.md) and the
[2.4.0 point contract](v2-spatial.md), which is unchanged.

**Format.** Creating or rebuilding a spatial index in 2.5.0 writes the new index
format and moves its collection to catalog version 7. RomeDB 2.4.0 and older refuse
such files. Existing latitude-format indexes keep working unchanged at their old
catalog version, and `REINDEX` upgrades them. Keep a pre-upgrade backup if you may
need an older release.

The frozen build source is `d4d9df777d624d751e7ebf814311691e841867ee`.
[Exact-source CI](https://github.com/romedb/romedb/actions/runs/38353045533)
passes. Local scoped checks passed 1,082 Rust tests, 185 Node/application tests,
five C ABI tests, two release-policy tests, formatting, scoped Clippy and strict
TypeScript, with no failures. Three opt-in spatial measurement tests are ignored
by default. The production profile retains debug assertions and overflow checks.

All 31 installed verification groups pass, including CLI graph and CLI spatial
checks, Node 22.0.0/24.19.0, CPython 3.10.21/3.12.3/3.14.7, PHP FFI, Go race
detection, Swift and .NET. Immutable 1.0.0/2.0.0/2.1.0/2.2.0/2.3.0/2.4.0 upgrade,
candidate writes, reopen, rollback, unchanged backups and independent restore
pass. The 2.4.0 check also confirms that latitude-format indexes serve the new
search sources unchanged, that `REINDEX` upgrades them, and that the 2.4.0 package
refuses the upgraded file. The standalone production-profile Rust consumer and the
bounded 1,000-row application workload also pass.

A local comparison with SurrealDB 3.3.0 on its RocksDB file backend used the
exact Node package, 100,000 people and 500 stored zones. RomeDB answered map
viewports in 1.4 to 6.5 ms, zone lookups in 2.3 to 2.8 ms, the nearest five
available drivers in 25 to 29 ms and single updates in 6.7 to 7.3 ms. Bulk import
took about 14 s, as in SurrealDB. SurrealDB stayed faster when thousands of
documents are returned or grouped, and for nearest search with a very rare
filter. See [the comparison](spatial-surrealdb-comparison.md). These are samples,
not latency or capacity guarantees.

The shipping dependency review covers 359 package identities with zero unresolved
findings and retains the documented lru informational exception RUSTSEC-2026-0253.
The lockfile differs from 2.4.0 only in RomeDB workspace versions.

[Public download verification](release-2.5.0-download.json) records the anonymous
download and checksums. See [qualification](release-2.5.0-qualification.json) and
[the checklist](v2.5-tasks.md). Lines, overlays, buffers, projections,
browser/WASM, other platforms and package registry publication are outside this
release. Cloud and Studio are unchanged.
