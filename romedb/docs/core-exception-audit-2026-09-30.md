# Core exception follow-up audit

Requested scope: recheck local Turso overrides, remove upstream replacements and
correct outdated integrations. This audit preserves the ongoing cloud MVCC
frontend changes and the Turso 0.8.1 pin.

## Checklist

- [x] Inspect the actual core diff, including changes outside the exception table.
- [x] Fetch current upstream main and compare every active exception.
- [x] Confirm retired overrides remain absent and keep their regressions.
- [x] Correct stale proposal status and link current provenance.
- [x] Rerun the affected engine and RomeDB regression suites; record results.

## Revisions

- Release base: `8549c16595d2faf1bdd6ee24aee0be8bfabb3d4a` (Turso 0.8.1).
- Audited fork HEAD: `1da116f1053307934b23ba7647971a1082e9b00a`.
- Freshly fetched upstream main: `36da5b2e435cb07bba3bed2c7e7eef236b6b2e64`.
- Existing uncommitted `romedb/frontend/src/lib.rs` changes implement cloud MVCC
  opening and commit tags. They are outside core and were preserved.

There are 27 changed files under `core/` relative to the release, including one
regression file. All changes map to the seven maintained exceptions below. No
undocumented core implementation override was found. Neither the release nor
the inspected main supplies an equivalent replacement for those seven. No core
implementation change or upstream-main merge was warranted by that comparison.
The later runtime finding below requires an adaptation of the retained meter.

## Decisions

| Exception | Decision and source evidence |
|---|---|
| Trigger interruption | Retain. Upstream `op_program` still maps both `StepResult::Interrupt` and `Busy` to `LimboError::Busy`. The local branch preserves the distinction and saves suspended subprogram state. |
| FTS directory-cache isolation | Already removed during the 0.8.1 sync. The old `fts.rs` cache patch does not exist in the current diff; upstream's replacement FTS implementation owns snapshot isolation. Keep the cache/snapshot regressions. |
| First FULL WAL commit | Already removed during the 0.8.1 sync. Upstream includes prepared frames in `need_fsync`. Current local `wal.rs` changes only concern failed checkpoint barriers. Keep the first-FULL regression. |
| FTS backing integrity and teardown | Retain only the current remainder. Upstream logical index-count checks still include backing indexes; DROP TABLE still collects `get_indices`, which hides those roots. Physical integrity checks remain enabled locally. |
| Read-only scalar errors | Retain. Upstream abort handling lacks the local read-only `ExtensionError` branch preserving caller transactions and named savepoints. Writer cleanup is unchanged. |
| Checkpoint WAL barrier | Retain the companion only. Upstream already starts the pre-backfill sync, but advances directly to `Processing`; it lacks `SyncWalPending`, failed-completion cleanup and the automatic-checkpoint retry handling. |
| Named-savepoint cancellation recovery | Retain. Upstream named frames do not save/restore prior `poisoned_tx` state after successful rollback. Clearing all poison would incorrectly accept earlier abandoned writes. |
| Execution metering and budgets | Retain. Upstream statement counters do not replace retained per-execution limits, completed seek/index-method visits, schema attribution, mutation events or incremental metered Count. Compiler flag additions belong to this exception. |
| FTS savepoint cursor lifecycle | Retain the separately approved 0.8.1 fix. Upstream `BackingStore::open_cursor` still creates an unregistered B-tree cursor, and pager rollback invalidates cursors after truncation. The local stable Box registration and earlier invalidation are both needed. |

The retired WASI patch also remains absent. Browser support is outside the active
native scope and no browser qualification was run.

## File inventory

- `core/connection.rs`: metering attachment and named-savepoint poison state.
- `core/database.rs`, `core/lib.rs`, `core/execution_meter.rs`: metering wiring/API.
- `core/index_method/backing_store.rs`: approved stable cursor registration.
- `core/storage/pager.rs`: cursor invalidation ordering and checkpoint cleanup.
- `core/storage/wal.rs`: checkpoint pending-completion tracking and reset.
- `core/translate/integrity_check.rs`: backing-index logical-count exclusion.
- `core/translate/schema.rs`: backing-root teardown plus mutation flags.
- `core/translate/alter.rs`, `analyze.rs`, `emitter/delete.rs`, `emitter/mod.rs`,
  `emitter/update.rs`, `expr/functions.rs`, `index.rs`, `insert.rs`, `order_by.rs`,
  `sequence.rs`, `trigger.rs`, `upsert.rs`, `view.rs`, `window.rs`: mutation
  classification, replacement flags and internal-maintenance exclusions.
- `core/vdbe/execute.rs`: trigger propagation, savepoint recovery and metering.
- `core/vdbe/insn.rs`: mutation-classification flags.
- `core/vdbe/mod.rs`: metering, scalar error cleanup and failed checkpoint handling.
- `core/vdbe/statement_lifecycle_tests.rs`: retained cancellation regressions.

The four changed upstream integration files contain retained regression tests:
`external_apis.rs`, `index_method/mod.rs`, `integrity_check.rs`, and
`wal/test_wal.rs`. The integrity test's SQLite oracle additionally accepts the
newer rusqlite datatype-constraint result; Turso assertions remain intact.

## Newer upstream changes to watch on the next sync

Upstream's [ClearBtree cursor registration](https://github.com/tursodatabase/turso/commit/d56465122)
fixes a different cursor from `BackingStore::open_cursor`; it does not replace
the approved FTS savepoint fix.

The newer [whole-table DELETE optimization](https://github.com/tursodatabase/turso/commit/58f350a90)
adds `ClearBtreeCount` and bypasses the ordinary Delete opcode. It is absent from
0.8.1. Before a future sync adopts it, preserve mutation/read budget semantics by
instrumenting that path or retaining row execution when metering is enabled.
Existing `mutation_meter` already asserts an unqualified DELETE mutation count;
also qualify cancellation and partial-work retention before enabling the fast
path for metered requests. No unreleased optimization was backported here.

## Validation

Fresh local results against the audited worktree, all with zero failures:

| Scoped check | Passed |
|---|---:|
| RomeDB integration: checkpoint barrier/crash atomicity, execution/mutation/schema/index-method metering, profile, fulltext, transactions and Turso 0.8.1 | 64 |
| RomeDB frontend `interrupt::tests` | 17 |
| Core `statement_lifecycle_tests` | 46 |
| Native integration `index_method::` | 131 |
| Native integration `trigger::` | 98 |
| Native integration `integrity_check::` | 36 |
| Native integration `external_apis::` | 14 |
| Native integration `savepoint` | 12 |
| Native integration `first_full` | 1 |

The native integration filter counts can overlap; they are not a claim of unique
test coverage. The native integration binary was freshly built with
`cargo test --locked -p core_tester --test integration_tests --no-run -j4` before
running those filters. RomeDB checks used `--locked -p romedb-tests` for integration
and `--locked -p romedb --lib` for cancellation; core lifecycle checks used
`--locked -p turso_core --lib`. The lifecycle build emitted an existing
configuration-dependent unused `HashMap` import warning. No core sources were
edited to suppress it. Documentation links and `git diff --check` also pass.

The historical proposal pages now state their current status before the original
approval evidence, preventing a future sync from reapplying retired patches.

## Subsequent MVCC catalog-mutation correction

Default-on Cloud customer journeys exposed catalog writes counted as billable
customer mutations. The old Insert/Delete exclusion relied on cursor physical
root 1; MVCC exposes a logical table identity instead. Both hooks now pass their
compiler cursor identity to the existing meter, sharing the schema-read
classifier. This adapts the approved execution-metering exception. Upstream
change counts and transaction/view lifecycle remain unchanged.

The new `schema_meter` regression covers CREATE TABLE/INDEX, column/table renames,
ADD COLUMN and DROP with zero allowed customer mutations, then checks actual
INSERT, CTAS and DELETE counts. Before the fix CREATE TABLE exhausted the zero
mutation budget; after the fix the focused meter/0.8.1 suites passed 39 tests.
The final full scoped command `cargo test --locked -p romeql-parser -p romedb
-p romedb-cli -p romedb-tests` completed successfully with 834 reported passing
test executions, zero failures and zero ignored tests (including child-process
reports). Changed Rust files pass rustfmt and `git diff --check`. The run used two
build jobs/two test threads under the local 24GiB resource guard after diagnosing
WSL OOMs from the earlier uncapped build. Its persistent log is
`/home/tan/romedb-mvcc-default-romedb-scoped.log`. Final artifact regeneration and
Cloud qualification remain separate gates.
