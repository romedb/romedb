# V2-S3 and V2-Q1 qualification

Date: 2026-09-25. Working tree based on `03a67fc02`, including the existing WAL
integration changes. This is embedded development evidence, not a published V2
artifact. Rust 1.88.0, Node 24.19.0, Linux x64/WSL2.

## H3 cells

The [spatial contract](v2-spatial.md#h3-cell-aggregation) is implemented with
exactly h3o 0.9.4, default features disabled, using libm. The lockfile adds only
h3o, h3o-bit and float_eq; no existing dependency is upgraded.

- Official latitude 45, longitude 40, resolution 2 reference returns
  `822d57fffffffff`.
- All sixteen resolutions preserve equivalent antimeridian/pole positions and
  center-to-cell identity at six geographically distinct points.
- Invalid resolutions, point inputs, non-cell IDs and noncanonical addresses
  reject. Failed writes, caller rollback, scalar cell indexes and reopen preserve
  stored values and index agreement.
- GROUP BY/HAVING and grouped INSERT SELECT work with computed cells and persisted
  cell strings. Both Node clients preserve cell strings and typed Point objects.
- Updated dependency inventory, archive audit and notice bundle checks pass:
  195 declarations, 188 verified cached archives and 128 distinct notice texts.
  Existing separately documented workspace/notice supplements remain necessary;
  this source inventory is not a linked-artifact or complete release audit.

Regressions: `romedb/tests/tests/spatial.rs`, the two `h3_` tests, and the Node H3
test. Focused log: `/tmp/romedb-v2-s3-focused.log`.

## Record brace projections

The [projection contract](v2-record-projections.md) preserves typed positional
columns and zero-or-one cardinality. Parser and real-engine regressions cover:

- Plain and quoted nested paths, aliases, duplicate names, whole-document wildcard,
  embedded-object wildcard, one-hop collection/relational references and missing
  records/fields/targets. Deeper links remain references.
- Indexed ID lookup with no fullscan steps; repeated reference projections share
  a fetch batch and have the same target read count as a single occurrence.
- Exact final payload accounting, row rejection, missing-target NULL accounting,
  pending transaction visibility and usable rollback after a failed wildcard.
- Persistence, malformed/nested forms, unused bindings and unchanged ordinary SQL
  record expressions. Synchronous/asynchronous Node execution, profiling and
  bounded SELECT produce matching values.

Regressions: `romedb/parser/src/tests.rs`, `romedb/tests/tests/record_projection.rs`
and the Node brace test. Focused logs: `/tmp/romedb-v2-brace-focused.log` and
`/tmp/romedb-v2-s3-q1-node-focused.log`.

The first Node run exposed a test-fixture mistake: limit values were numbers
instead of the client's required bigint values. The corrected tests also open
each client lazily, so an earlier assertion cannot leave a later worker open.
The final full Node run below passed after that test correction.

## Combined milestone checks

- `cargo test --locked -p romeql-parser -p romedb -p romedb-cli -p romedb-tests`:
  **703 passed, 0 failed, 0 ignored**, across 60 test/doc-test binaries.
  Log: `/tmp/romedb-v2-s3-q1-rust.log`.
- Rebuilt `romedb-node`, then client/application suite:
  **110 passed, 0 failed, 0 skipped**.
  Logs: `/tmp/romedb-v2-s3-q1-node-build.log`,
  `/tmp/romedb-v2-s3-q1-node-final.log`.
- Five RomeDB packages pass formatting and all-target Clippy with warnings denied.
  Log: `/tmp/romedb-v2-s3-q1-clippy.log`.
- Strict TypeScript via pnpm passes:
  `/tmp/romedb-v2-s3-q1-typescript.log`.
- CLI script smoke produces the expected typed brace expansion and H3 grouped row:
  `/tmp/romedb-v2-s3-q1-cli.log`.

V2-S3 and V2-Q1 are complete. The three spatial milestones are complete under
their documented limited contract. Inverse relationships, full-text, ANN, user
JavaScript, additional bindings/WASM and V2 release qualification remain open.
No V2 publication or cloud changes are included in this milestone.
