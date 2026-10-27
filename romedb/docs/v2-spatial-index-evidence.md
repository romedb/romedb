# V2-S2 managed spatial index evidence

Date: 2026-09-25. Working tree based on `03a67fc02`; existing WAL integration
changes remain present. This is development evidence, not release publication.
The supported behavior is in [the spatial contract](v2-spatial.md).

| Requirement | Evidence |
|---|---|
| Strict additive syntax | Parser tests accept one nested path with USING SPATIAL; malformed declarations reject; ordinary CREATE INDEX dispatch is preserved |
| Atomic build and retry | Existing invalid data rejects the build without orphan storage; repair/retry works in the caller transaction |
| Cancellation | Deterministic interruption after an index-build write removes metadata/storage; subsequent build and integrity audit succeed |
| Writes and rollback | Inserts, updates, deletes and unique-constraint failure preserve radius results; caller rollback restores index entries |
| Lifecycle and persistence | Drop/recreate, drop rollback, collection drop and database reopen regressions pass |
| Format compatibility | Spatial metadata requires version 3 and rejects UNIQUE; scalar-only collections retain V1-compatible version 2 metadata |
| Auditing | Integrity checks verify both coordinates, missing entries, document identity and storage schema; stale longitude/missing entries and missing native index are detected |
| Correct radius results | Indexed results equal exhaustive geo::within/geo::distance queries over 370 points, seven centers, seven radius scales and exact floating-point radius boundaries |
| Geographic boundaries | +/-180 equivalence, both poles, coincident points, tiny separations, global radii and the reproduced near-antipodal latitude-bound rounding case |
| SELECT integration | Typed IDs, distance ordering and ID ties, LIMIT, joins and CTEs; bound index/point/radius inputs in both Node clients |
| Effective index use | EXPLAIN names the requested native index; selective fixture reads 6 rows with 6 index steps and zero fullscan steps out of 370 documents |

Focused evidence: `/tmp/romedb-v2-index-focused.log`. This measured plan executes
103 VM steps and seven B-tree seeks; the fixture does not establish production
latency or uniform selectivity. Dense bands of latitude and large radii remain
explicit limitations of the chosen candidate index.

The first broad Rust run reproduced a pending-feature dispatch regression:
FULLTEXT declarations reported FDB_SYNTAX instead of FDB_UNSUPPORTED. The parser
now preserves the documented pending FULLTEXT/VECTOR route. The existing deferred
statement regression passed in `/tmp/romedb-v2-s2-deferred.log`.

Final combined checks passed on the corrected source:

- Rust 1.88.0, full RomeDB scoped suite: **697 passed, 0 failed, 0 ignored**.
  Command: `cargo test --locked -p romeql-parser -p romedb -p romedb-cli -p romedb-tests`.
  Log: `/tmp/romedb-v2-s2-rust-final.log`.
- Rebuilt Node addon on Node 24.19.0, both clients/application suite:
  **108 passed, 0 failed**. Log: `/tmp/romedb-v2-s2-node-final.log`.
- All five RomeDB packages passed formatting and all-target Clippy with warnings
  denied. Log: `/tmp/romedb-v2-s2-clippy-corrected.log`.
- Strict TypeScript passed through pnpm:
  `/tmp/romedb-v2-s2-typescript.log`.

V2-S2 is complete. V2-S3 cell aggregation and the broader V2 release checklist
remain open. No release artifacts were published and no cloud service was changed.
