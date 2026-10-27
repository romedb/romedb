# V2-Q2 inverse relationship evidence

Date: 2026-09-25. Embedded working tree based on `03a67fc02`, preserving the
pre-existing WAL integration. Rust 1.88.0, Node 24.19.0, Linux x64/WSL2. This
qualifies [the inverse relationship contract](v2-relations.md), not a V2 release.

## Required behavior

| Requirement | Evidence |
|---|---|
| Explicit declarations and index dependency | Parser grammar; definition without the exact-path scalar index rejects; globally duplicate names reject; dependent index/source drops reject |
| Atomic lifecycle and persistence | Define/drop rollback, reopened declarations and queries, target drop removes its owned declaration while preserving weak references |
| Catalog compatibility | Target metadata requires version 3 and stays at 3 after relation removal; scalar-only source stays at 2; corrupted versions, names, source/path/index dependencies reject on connection open |
| Indexed lookup | Native plan searches the named reference index on key equality, sorts matching IDs, scans the bounded selected-ID CTE and searches each source document by primary key |
| Bounded deterministic pages | Default 100, maximum 1000, zero limit, typed exclusive source-ID cursor, stable stored-ID order, absent/NULL targets and typed string/integer identities |
| Typed, one-hop results | Source documents retain nested references; collection, native and source-free outer SELECTs work; forward/inverse projections can coexist |
| Data/index agreement | Reference updates, source deletion, failed array-valued indexed write and rollback retain correct inverse results; collection integrity checks pass |
| No partial or unbounded expansion | Shared position bound, per-resolver target/output byte bounds, exact duplicate/null accounting, logical depth including the outer array and caller ResultLimits |
| Snapshot and cancellation | Pending writes are visible; deterministic progress-handler interruption at three execution points preserves the caller transaction; retries return identical values and counters |
| Projection-only grammar | Invalid limits/cursors, unknown names even on empty results, dynamic relation names, nested/derived/CTE/compound/DISTINCT expansion, fetched ordering and write sources reject |
| Client/tool parity | Rust public declaration lifecycle; synchronous/asynchronous Node pages, typed cursors, profiles, limits and rollback; CLI page/inspection/drop-rollback smoke |

The selective fixture has 303 source documents, only two referencing the chosen
target. LIMIT 1 produces one result, one fetch batch, **four fetch rows read and
51 fetch VM steps**. The outer ID lookup reads one row with zero fullscan steps.
Duplicate identical expansions retain the same target read count as one request.
This is measured selectivity evidence, not a general latency claim. Many matches
for a target can still require sorting the whole matching ID set before LIMIT.

Native plan evidence includes:

```text
SEARCH __romedb_i_617574686f7273 USING INDEX authors (key=?)
USE SORTER FOR ORDER BY
SCAN selected AS i
SEARCH d USING INDEX sqlite_autoindex___romedb_c_706f737473_1 (id=?)
```

## Verification

- Four real-engine integration tests pass:
  `/tmp/romedb-v2-q2-focused.log` (`romedb/tests/tests/relations.rs`).
- Three frontend regressions pass:
  `/tmp/romedb-v2-q2-unit.log` (`romedb/frontend/src/relations.rs`).
  The final combined run also includes the added maximum-depth array check.
- Full scoped Rust suite: **711 passed, 0 failed, 0 ignored**, across 61 test/doc-test
  binaries. Command:
  `cargo test --locked -p romeql-parser -p romedb -p romedb-cli -p romedb-tests`.
  Log: `/tmp/romedb-v2-q2-rust.log`.
- Rebuilt Node addon; **111 client/application tests pass**, zero failures/skips.
  Logs: `/tmp/romedb-v2-q2-node-build.log`, `/tmp/romedb-v2-q2-node.log`.
- Five RomeDB packages pass formatting and all-target Clippy with warnings denied:
  `/tmp/romedb-v2-q2-clippy.log`.
- Strict TypeScript via pnpm passes: `/tmp/romedb-v2-q2-typescript.log`.
- CLI typed pagination, INFO and declaration rollback pass:
  `/tmp/romedb-v2-q2-cli.log`.

Qualification caught an unintended collection INSERT SELECT path accepting inverse
expansion. A dedicated insert-source lowering flag now rejects that path; normal
forward-fetch behavior remains unchanged. The focused rejection regression passes.

V2-Q2 is complete. No new dependency or upstream implementation change was required. Full-text,
ANN, JavaScript UDFs, additional bindings/WASM and release qualification remain
required. No V2 artifact is published by this milestone.
