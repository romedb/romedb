# Spatial workloads: RomeDB and SurrealDB

Measured 2026-10-14 with the RomeDB 2.5.3 release Node package. This is one local
sample, not a latency or capacity guarantee. Raw results:
[spatial-surrealdb-comparison.json](spatial-surrealdb-comparison.json), measured
after the 2.5.3 publication.

## Setup

| Item | RomeDB | SurrealDB |
| --- | --- | --- |
| Version | 2.5.3 release package | 3.3.0, official Linux x64 release, SHA-256 verified |
| Storage | Embedded, file-backed, a point index and a region index | Server with the RocksDB file backend (`surreal start rocksdb://...`) |
| Commit durability | Default (synced commits) | Default `SURREAL_DATASTORE_SYNC_DATA=every` |
| Access | Node.js client in the same process | Node.js `fetch` over loopback HTTP, one sign-in, bearer token |
| Build | Release Node package (`romedb-production` profile) | Release binary |

Both ran inside one cgroup through `scripts/romedb-heavy`: four CPUs, 24 GiB memory,
on an Intel Xeon E5-2673 v4 under WSL2, with Node.js 24.19.0.

Each dataset has 100,000 people and 500 stored zone polygons, generated from a
fixed seed. The **cities** dataset puts the people in 20 cities with about 5 km
spread; the **uniform** dataset spreads them evenly over the sphere. Attributes
are independent of location: one third `available`, `last_seen` uniform from
0 to 99, and a `fleet` of `'night'` for 0.1% of people. Each workload runs 25
query centers drawn from the data, after one warm-up query.

SurrealDB has no spatial index type. Its documentation recommends storing a
geohash in an indexed field for dense data, so it ran each workload two ways:

- **Scan:** geometry predicates only, such as `location INSIDE $polygon` or
  `geo::distance(location, $p) <= 5000`.
- **Geohash index:** computed fields `gh4` to `gh7`
  (`DEFINE FIELD ... VALUE geo::hash::encode(location, n)`), each with a standard
  index. The client picks the finest precision whose cover stays under 64 cells
  and adds `ghN IN [cells]`, which SurrealDB plans as a union of index scans.
  Nearest search grows the radius in rounds, as RomeDB does.

RomeDB used its documented forms: ordinary SQL (`ST_DWithin`, `ST_Covers`, the
nearest `ORDER BY ... LIMIT` form), `search::bbox` for viewports, and for the
5 km search also the `search::near` join. Zone lookup uses a RomeDB region index
(`USING SPATIAL WITH (geometry = 'region')`) and is a scan in SurrealDB.

Every timed query was also compared across systems. Result sets matched for all
samples of all workloads in both datasets.

Two harness details mattered for fairness:

- HTTP Basic authentication made SurrealDB verify the root password hash on every
  request, adding about 60 ms. The harness signs in once and sends a bearer token,
  as SurrealDB's SDK sessions do; a trivial query then takes about 1 ms.
- RomeDB runs in the client process, while SurrealDB answers over loopback HTTP.
  The server-reported SurrealDB p50 in parentheses excludes that transport cost,
  which is about 4 ms for small queries.

## Results

### Cities dataset

Times are p50 / p95 milliseconds seen by the client. SurrealDB server-reported p50 is in parentheses.

| Workload | Mean rows | RomeDB ordinary SQL | RomeDB explicit source | SurrealDB scan | SurrealDB geohash index |
| --- | --- | --- | --- | --- | --- |
| Available people within 5 km | 371.2 | 30.4 / 53.7 | 20.3 / 40.8 | 250.2 / 326.4 (242.8) | 34.3 / 267.6 (28.7) |
| Map viewport 0.1 x 0.1 degrees | 1353.8 | 6.9 / 10.5 | n/a | 88.0 / 103.3 (75.5) | 44.4 / 58.1 (34.2) |
| Points covered by a storm polygon | 3807.2 | 112.5 / 138.0 | n/a | 96.2 / 111.0 (79.0) | 62.3 / 76.0 (41.9) |
| Which of 500 stored zones contain a point | 0.3 | 2.2 / 2.9 | n/a | 4.6 / 7.3 (1.0) | n/a |
| 5 nearest available, fresh drivers | 5 | 25.2 / 46.6 | n/a | 69.9 / 84.0 (64.4) | 618.9 / 901.7 (589.1) |
| 5 nearest drivers of a 0.1% fleet | 5 | 290.6 / 553.2 | n/a | 54.4 / 68.6 (48.7) | n/a |
| Cell counts in a 0.5 degree viewport | 4998.5 | 96.7 / 132.6 | n/a | 93.5 / 107.0 (83.4) | 49.0 / 59.2 (39.0) |
| Move one driver (autocommit update) | 1 | 7.0 / 9.3 | n/a | 8.4 / 9.5 (4.7) | same writes, with geohash fields |

Mismatched result samples across all workloads: 0.

| Load step | RomeDB | SurrealDB |
| --- | --- | --- |
| Bulk load 100,000 people and 500 zones | 15.28 s (NDJSON import) | 13.58 s (INSERT batches of 1,000) |
| Build indexes | 8.64 s (point and region index) | 29.34 s (four geohash indexes) |
| Files on disk after the run | 97 MB | 72 MB |

### Uniform dataset

Times are p50 / p95 milliseconds seen by the client. SurrealDB server-reported p50 is in parentheses.

| Workload | Mean rows | RomeDB ordinary SQL | RomeDB explicit source | SurrealDB scan | SurrealDB geohash index |
| --- | --- | --- | --- | --- | --- |
| Available people within 5 km | 0.3 | 3.2 / 3.6 | 2.5 / 2.7 | 299.6 / 341.3 (294.2) | 6.2 / 308.5 (1.9) |
| Map viewport 0.1 x 0.1 degrees | 1 | 1.6 / 1.9 | n/a | 73.9 / 89.4 (69.8) | 5.8 / 78.0 (2.0) |
| Points covered by a storm polygon | 1.1 | 2.5 / 2.8 | n/a | 89.8 / 97.6 (85.1) | 5.7 / 6.7 (1.7) |
| Which of 500 stored zones contain a point | 0 | 3.1 / 3.4 | n/a | 4.9 / 5.5 (1.2) | n/a |
| 5 nearest available, fresh drivers | 5 | 37.3 / 90.4 | n/a | 90.2 / 100.5 (85.0) | 2125.8 / 2691.4 (2055.8) |
| 5 nearest drivers of a 0.1% fleet | 5 | 154.8 / 209.1 | n/a | 58.8 / 68.4 (54.0) | n/a |
| Cell counts in a 0.5 degree viewport | 1.3 | 2.6 / 4.8 | n/a | 78.8 / 92.7 (73.4) | 5.8 / 7.0 (1.8) |
| Move one driver (autocommit update) | 1 | 6.8 / 8.8 | n/a | 8.4 / 9.5 (4.7) | same writes, with geohash fields |

Mismatched result samples across all workloads: 0.

| Load step | RomeDB | SurrealDB |
| --- | --- | --- |
| Bulk load 100,000 people and 500 zones | 14.13 s (NDJSON import) | 13.46 s (INSERT batches of 1,000) |
| Build indexes | 8.82 s (point and region index) | 28.69 s (four geohash indexes) |
| Files on disk after the run | 97 MB | 72 MB |

### Change across releases

RomeDB on the cities dataset. The development run preceded the 2.5.0 open items; 2.5.0, 2.5.1 and 2.5.3 are release packages. Other values are p50.

| Workload | Development run | 2.5.0 | 2.5.1 | 2.5.3 |
| --- | --- | --- | --- | --- |
| Bulk load | 93.74 s | 14.76 s | 14.01 s | 15.28 s |
| Move one driver | 23.4 ms | 6.7 ms | 6.9 ms | 7.0 ms |
| Zone lookup | 20.2 ms | 2.8 ms | 2.9 ms | 2.2 ms |
| Available people within 5 km | 52.0 ms | 31.6 ms | 30.5 ms | 30.4 ms |
| 5 nearest available, fresh drivers | 35.0 ms | 25.1 ms | 29.5 ms | 25.2 ms |
| Storm polygon | 150.0 ms | 130.1 ms | 111.5 ms | 112.5 ms |
| Cell counts | 159.1 ms | 143.9 ms | 135.6 ms | 96.7 ms |
| 5 nearest of a 0.1% fleet, p50 | 445.3 ms | 338.8 ms | 378.6 ms | 290.6 ms |
| 5 nearest of a 0.1% fleet, p95 | 3339.4 ms | 2528.9 ms | 673.0 ms | 553.2 ms |

On the uniform dataset, the 0.1% fleet query fell from 1374 / 1621 ms in 2.5.0 to 187 / 219 ms in 2.5.1 and 155 / 209 ms in 2.5.3 (p50 / p95). 2.5.3 made scans and grouping cheaper per document, which shows in the cell counts and the 0.1% fleet scan. Run-to-run variation on this host is about 10 to 20% for the faster workloads.

## Findings

**Where RomeDB is faster.** Selective spatial searches, zone lookups, single
updates and index builds:

- Map viewports in dense cities take 6.9 ms, about 6 times faster than SurrealDB's
  geohash pattern and 13 times faster than its scan.
- Zone lookup through the region index takes 2.2 to 3.1 ms in the client, against
  4.6 to 4.9 ms for SurrealDB over HTTP. SurrealDB's server-side time is 1.0 to
  1.2 ms, so the two are close once transport is excluded.
- Nearest available, fresh drivers take 25 to 37 ms, against 70 to 90 ms for a
  SurrealDB scan and 0.6 to 2.1 s for its geohash rounds.
- A single-driver update takes about 7 ms in RomeDB with a synced commit, against
  8.4 ms in the SurrealDB client and 4.7 ms on its server.
- Bulk import is close to SurrealDB at 14 to 15 s, and RomeDB builds both spatial
  indexes in about 9 s, against 29 s for SurrealDB's four geohash indexes.

**Where SurrealDB is faster.** Workloads that return or evaluate thousands of
documents, and very selective filters without an index:

- Storm polygons with about 3,800 matches: 113 ms against 62 ms with geohash and
  96 ms for a scan.
- Dashboard grouping over about 5,000 rows: 97 ms against 49 ms with geohash; a
  SurrealDB scan takes 94 ms (136 ms in RomeDB 2.5.1).
- Nearest search for a 0.1% fleet without a fleet index: 155 to 291 ms against
  54 to 59 ms. Rounds stop once the match rate shows that a scan is cheaper, and
  RomeDB's scan costs more per document. With an index on the fleet field, the
  local evaluation in the [spatial evidence](spatial-upgrade-evidence.md#251-follow-up)
  answers the same query in about 7 ms.
- RomeDB files are about 35% larger.

## Changes since the development run

2.5.0 implemented the three open items from the development comparison:

- **Per-document cost.** Field accessors decode one field instead of the whole
  document, updates and deletes maintain scalar and point-index entries by key and
  skip unchanged keys, each operation uses one stack segment, and imports insert
  without a savepoint and catalog read per document.
- **Join-shaped rewrite.** Ordinary rewrites restrict the collection with a
  semijoin on the stored ID column instead of a typed `id IN (...)` list.
- **Reverse geofencing.** Region indexes store up to 32 quadtree cells per zone
  and test candidate shapes exactly, used by `search::containing` and by
  `ST_Covers(shape, point)`.

2.5.1 then made field reads stop at the requested field and return stored bytes,
ended nearest rounds early for selective filters, and used scalar indexes on such
filters when they select fewer documents. 2.5.3 parses simple fields and points
without the JSON decoder and evaluates field accessors before grouping, so the
`GROUP BY` sorter no longer copies whole documents.

## Remaining gaps

- Large result sets still cost RomeDB more per document than SurrealDB in
  evaluation and result conversion; grouping is now close to a SurrealDB scan but
  still about twice its geohash pattern.
- Nearest search with a very selective filter and no index on that field ends in
  a scan, which is several times slower per document than SurrealDB's. Index such
  fields.

## Reproduce

```sh
scripts/romedb-heavy node romedb/scripts/bench-spatial-surreal.cjs PACKAGE_DIR SURREAL_BINARY WORKDIR OUTPUT.json 100000 25
```

`PACKAGE_DIR` is the `@romedb/node` directory installed from the release
package. `BENCH_ONLY`, `BENCH_DATASETS` and `BENCH_PROFILE` select workloads and
datasets and print RomeDB profiles; `BENCH_LABEL` labels the RomeDB build in the
output.
