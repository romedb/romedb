# Spatial upgrade checklist

Status: released in RomeDB 2.5.0 ([release record](release-2.5.0.md)).
Contract: [spatial-upgrade.md](spatial-upgrade.md).

Unchecked items are future work. Internal prototypes do not establish a released
capability.

## S0. Freeze behavior and public API

- [x] Record the current release and spatial compatibility baseline: 2.4.0 is
  released; catalog version 6 is the newest; the released point contract is
  [v2-spatial.md](v2-spatial.md). Its stale development header is corrected.
- [x] Record the revision of the 2.2 general-geometry decision in RomeQL.md,
  AGENTS.md and the [decision record](v2.2-decisions.md).
- [x] Freeze geometry types, topology rules, dateline rules and numeric model
  (contract sections 1 to 3).
- [x] Freeze function names, `ST_*` alias lowering, NULL behavior and error
  categories (contract section 4).
- [x] Freeze the H3 function additions, array limits and coverage map.
- [x] Freeze explicit bbox, polygon and nearest source signatures (contract section 6).
- [x] Define the ordinary SQL forms that receive automatic index planning (section 7).
- [x] Define input and execution limits with an initial cost assessment (sections 5 and 10).
- [x] Map the dating, driver, viewport and storm examples to planned tests (section 11).
- [x] Record the index migration and compatibility policy (section 8).
- [x] Prototype nearest radius expansion in one snapshot and record round counts
  on dense and sparse fixtures: 3 rounds in a city and 7 in open ocean for k = 5,
  frozen as ceilings in the [2.5.1 evidence](spatial-upgrade-evidence.md#frozen-work-ceilings).
- [x] Prove that per-row prepared-geometry lookup does not re-parse: regions are
  cached by encoded bytes (contract section 5). A per-row timing proof on a large
  region remains with the S3 polygon benchmarks.

## Geometry layer delivered ahead of S2

The scalar geometry layer does not depend on the index choice, so it was built first.

- [x] Exact orientation with a floating-point filter and integer fallback
  (`frontend/src/spatial/exact.rs`).
- [x] Canonical Point, Polygon and MultiPolygon parsing, strict GeoJSON input with
  duplicate-key rejection, and GeoJSON output (`frontend/src/spatial/geometry.rs`).
- [x] Ring, hole, component, dateline and pole validation with a counted work limit.
- [x] Point location with exact boundary tests, half-open crossings and a
  latitude-strip edge index.
- [x] `geo::*` and `ST_*` functions through one table that drives the statement
  rewrite, display names, the SQL helper and the write-path evaluator.
- [x] H3 boundary, parent, children, disk, resolution, distance, area, compact and
  uncompact (`frontend/src/spatial/cells.rs`).
- [x] `search::near` accepts `ST_Point` and `ST_MakePoint` constructors.

Evidence, 2026-10-12, Rust 1.88.0 through `scripts/romedb-heavy`:

| Check | Result |
| --- | --- |
| `cargo test --locked -p romedb --lib spatial` | 22 passed |
| `cargo test --locked -p romedb-tests --test spatial` | 16 passed, including all released spatial tests |
| `cargo clippy --locked -p romedb -p romedb-tests --all-targets --no-deps -- -D warnings` | clean |
| `cargo fmt -p romedb -p romedb-tests -- --check` | clean |

The full scoped milestone script result is recorded under S2.

## S1. Select and qualify the two-dimensional index design

- [x] Implement a bounded Morton/quadtree prototype under RomeDB code
  (`frontend/src/spatial/morton.rs`): monotone 31-bit quantization, key ranges
  with a cover budget, region covers that pass crossing edges to child cells.
- [x] Assess a managed H3 prototype using the pinned dependency. Rejected; see
  [the evidence](spatial-upgrade-evidence.md#decision).
- [x] Compare both designs with the latitude baseline and exhaustive references:
  zero misses for every workload and query.
- [x] Prove conservative candidate coverage: monotone quantization, outward
  radius windows, seam columns, one-quantum widening for region cells; property
  tests in `morton.rs`.
- [x] Prove the nearest stopping rule (contract section 6) and record round counts.
- [x] Measure same-latitude, city, dateline, thin-polygon and distant-population workloads.
- [x] Measure moving-point workloads (repeated driver movement with concurrent
  searches): [2.5.1 evidence](spatial-upgrade-evidence.md#moving-drivers-with-concurrent-searches).
- [x] Freeze numeric acceptance ceilings for the selective fixtures beside the
  benchmarks: asserted by `selective_fixture_work_stays_under_frozen_ceilings`.
- [x] Select one primary design and document the decision.

## S2. Geometry layer and indexed viewport operation

- [x] Canonical geometry parsing, normalization and GeoJSON serialization.
- [x] Ring, hole, component and boundary tests.
- [x] Robust orientation and point-on-segment handling.
- [x] H3 functions with upstream reference fixtures.
- [x] Managed storage for the new format: Morton key, longitude and latitude per
  entry; `spatial` format marker; catalog version 7.
- [x] Atomic build, update, delete, rollback and reopen support through the common
  index paths; the released lifecycle tests pass on the new format.
- [x] `REINDEX` upgrade of latitude-format indexes, with a test that compares all
  four sources before and after.
- [x] Indexed `search::bbox`, including dateline, seam, zero-size and full-world viewports.
- [x] Profile fields for spatial ranges and nearest rounds (contract section 9).
- [x] Full RomeDB scoped milestone checks, 2026-10-12, `romedb/scripts/check.sh` through
  `scripts/romedb-heavy`: shipping policy, formatting and warnings-denied Clippy for
  all nine RomeDB packages; 1,078 Rust tests passed, 0 failed, 3 ignored (the opt-in
  measurements); Node suite 185 passed, 0 failed; C checks passed. One expectation
  changed: `metered_creates` now expects `USING COVERING INDEX` for a new spatial index.

## S3. Indexed radius and polygon searches

- [x] New-format radius searches use Morton ranges; latitude-format behavior unchanged.
- [x] Contains, Covers, Within and point/region Intersects.
- [x] `search::polygon` with prepared regions registered per statement.
- [x] Ordinary WHERE rewrites (contract section 7), keeping every original predicate.
- [x] NULL, invalid and non-constant arguments, aliases and parameters keep
  ordinary results; `NOT INDEXED` comparisons in `romedb/tests/tests/spatial.rs`.
- [x] Indexed results equal exhaustive or scan evaluation in every test.
- [x] Plan choice (EXPLAIN) and no full scan asserted for selective fixtures.
- [x] Investigate the dense-city latency gap: fixed by a covering index ([evidence](spatial-upgrade-evidence.md#covering-index)).

## S4. Exact nearest search with filters

- [x] Complete radius expansion in one snapshot.
- [x] Eligibility applied before the `k` rows are chosen (ordinary form).
- [x] Availability, freshness and other AND filters on the collection.
- [x] Distance and typed-ID ties; `k` from 1 to 10,000.
- [x] `search::nearest` with the optional allowed-ID array, and the ordinary form.
- [x] Invalid explicit arguments rejected without scan fallback.
- [x] Sparse, fewer-than-k, tie, pending-write and rollback cases.
- [x] Benchmark concurrent movement and search (`scripts/bench-spatial-movement.cjs`).
- [x] Cancellation, deadline, result-limit and `k` limit cases during rounds.
- [x] Rare filters stop rounds early; scalar indexes on filter fields are used
  when they select fewer documents (2.5.1).

## S5 and S6

- [x] Released in [2.5.0](release-2.5.0.md): seven-client qualification, 2.4.0
  old-reader rejection with the released artifacts, user documentation and the
  release workflow.
- [x] 2.5.1: interrupted `REINDEX` of a latitude-format index, polygon and region
  steps in the shared native client fixture, and a randomized write stress test.

## Comparison with SurrealDB

- [x] Benchmark seven location workloads and single-driver updates against
  SurrealDB 3.3.0 with the RocksDB file backend, 100,000 documents, two datasets,
  identical results for every sample. See
  [the comparison](spatial-surrealdb-comparison.md) and
  `romedb/scripts/bench-spatial-surreal.cjs`.
- [x] Fixes from the comparison: literal LIMIT in nearest statements, exact
  condition replacement, faster nearest probing with a global fallback, and a
  larger prepared-region cache.
- [x] Open items from the comparison, implemented for 2.5.0: single-field reads,
  keyed index maintenance and faster imports; a semijoin rewrite instead of typed
  `id IN (...)` membership; region indexes for reverse geofencing. The 2.5.0
  rerun is in [the comparison](spatial-surrealdb-comparison.md).
- [x] 2.5.1: field reads scan the canonical layout and stop at the member, which
  halves scans and row conversion, and nearest search uses scalar indexes on
  selective filters. Large groupings and rare filters without an index remain
  slower than SurrealDB; see [the evidence](spatial-upgrade-evidence.md#251-follow-up).

