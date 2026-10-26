# Approved V2 core integration

The user approved both prepared patches and requested explicit maintenance notes
for future upstream replacements. They are integrated independently:

- Scalar read-error transaction handling: `f26014f04de4077a49268fd94c37ff9ad6ad6425`
  (only `core/vdbe/mod.rs` and `tests/integration/external_apis.rs`).
- Opt-in WASI FTS: `2ef619c0704025512d9f4d4f5290dec3940a7361`
  (the ten reviewed core feature/cfg files).
- The reviewed frontend WASI feature wiring is applied separately in the existing
  V2 working tree. All three retained patches pass reverse-application checks.

The [exception register](core-exceptions.md) records all six maintained core
exceptions, upstream replacement/removal criteria and retained regressions. It is
linked from UPSTREAM.md, the parent workflow and agent instructions. No equivalent
upstream PR or commit is claimed without verification.

## Integrated verification

- Native external-API suite: **12 passed**, including the WAL/MVCC scalar-error
  regression. Log: `/tmp/romedb-v2-approved-native.log`.
- Native `index_method::test_fts` suite: **26 passed**.
  Log: `/tmp/romedb-v2-approved-native-fts.log`.
- Browser WASM development build passes with Rust 1.88.0, WASI SDK 33.0,
  optimization level 1 and assertions retained. Existing upstream target warnings
  remain. Log: `/tmp/romedb-v2-approved-wasm.log`.
- Source browser acceptance passes Chromium 149.0.7827.55 and Firefox 151.0,
  including FTS indexed plans/ranking, update/delete/drop/rollback, physical
  integrity, OPFS reopen and committed FTS recovery after abrupt page close.
  Scalar read-error checks preserve prior work and named savepoints. Close reports
  zero active Rust workers, peak eight in the fixture. Logs:
  `/tmp/romedb-v2-approved-chromium.log`, `/tmp/romedb-v2-approved-firefox.log`.

- Installed archive TypeScript and V1 upgrade/restore pass in both browsers,
  including FTS creation over V1 text and reopen. Log:
  `/tmp/romedb-v2-approved-browser-upgrade.log`.
- Target WASI frontend/protocol/browser Clippy passes with warnings denied for
  those packages; upstream dependency warnings remain. Core formatting passes.
  Logs: `/tmp/romedb-v2-approved-wasm-clippy.log`,
  `/tmp/romedb-v2-approved-core-fmt.log`.
- Native FTS rolled-back optimize regression also passes (one additional test).
  Log: `/tmp/romedb-v2-approved-native-fts-rollback.log`.

The first installed full run passed Chromium but failed during Firefox persistent
WAL-quota recovery with `OPFS shutdown timed out`. The isolated Firefox case
subsequently passed without source changes. Logs:
`/tmp/romedb-v2-approved-browser-installed.log` and
`/tmp/romedb-v2-approved-firefox-fault-repro.log`. The cause is not established;
do not erase this failure or claim the isolated pass explains it. The complete installed repeat passes TypeScript, normal acceptance, abrupt-page
recovery and all 11 storage-fault modes in both browsers, with unchanged runtime
bytes: `/tmp/romedb-v2-approved-browser-installed-repeat.log`. This passing repeat
does not establish the cause of the earlier timeout; retain it as a qualification
limitation.

The initial combined native run stopped at an older vector-accessor regression
that asserted the previous scalar read-error rollback behavior. Its expected
state now follows the approved contract: explicit transactions and prior rows
survive, both fused and generic accessors agree, and explicit rollback still
removes prior uncommitted work. The updated focused test passes:
`/tmp/romedb-v2-approved-vector-regression.log`. The first no-fail-fast run then
identified nine older Rust assertions and three Node assertions of the same
transaction-wide rollback contract (CLI reports, bundled helpers, compound
sources, RETURNING and nested USING). They now assert retained prior rows and
savepoints, unchanged failed-statement data/indexes, and explicit rollback/retry.
No additional core changes were needed. Historical V1 contract text is explicitly
superseded for this behavior by the V2 transaction note.

Final combined-current-source results:

- **738 Rust tests passed, zero failed/ignored**, across 64 test/doc-test groups:
  `/tmp/romedb-v2-approved-rust-contracts.log`.
- **115 Node/application tests passed, zero failed/skipped/cancelled**:
  `/tmp/romedb-v2-approved-node-contracts.log`.
- Eight-package formatting, Clippy/all targets with warnings denied, and strict
  Node TypeScript pass: `/tmp/romedb-v2-approved-fmt-contracts.log`,
  `/tmp/romedb-v2-approved-clippy-contracts.log`,
  `/tmp/romedb-v2-approved-typescript.log`.

The rebuilt Linux x86_64 debug Node addon is 309,939,392 bytes, SHA-256
`5a131957c826f46db8e58ab2d836dddfe65c549010bfcd14b49cb33f81fc567c`.
This closes the combined scalar-error acceptance gate, not final release packaging
or the full advertised platform matrix.

## Local development artifact

Archive `/tmp/romedb-v2-approved-browser-pack/romedb-browser-2.0.0-dev.1.tgz`:
7,093,249 bytes; SHA-256
`9e4fcc7a0f3699df06b6d40fcd1ed274b00b94353f5bea9964ac1bbada79340f`.
WASM SHA-256:
`d388a6c0e821b3098a111d95f4fe41e03a3a94b8c5cee8c8a09753721397c3d2`.
Cargo.lock remains
`0dcbf5a9c5ead00c327c86d2a3fca97881ed3de9004e0d30f14a7b6b2f90f5b8`.

WASI feature expansion changes the dependency closure, not pinned versions. The
browser declaration inventory and notice bundle were regenerated: 256 packages,
248 verified registry archives, 179 distinct collected notice texts, four packages
still requiring notice review. Packaged notice bytes match current source.
The browser build now checks the dependency inventory against the current feature
closure before checking notice bytes. An unchanged lockfile alone cannot detect
stale inventories after enabling a feature; this check passes for the new closure.
Complete attribution and final platform/distribution qualification remain open.
This artifact is local and unpublished; it is not a V2 release.
