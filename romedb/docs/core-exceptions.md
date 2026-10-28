# Maintained engine exceptions

Required review checklist for every upstream sync. Baseline and full provenance
are in [UPSTREAM.md](../UPSTREAM.md). The current sync decisions below identify retained, retired and newly approved
exceptions. The retired WASI exception is recorded separately. The dated review
below tracks upstream replacement candidates; no patch is removed from the
existing pinned engine merely because a newer upstream revision contains a fix.
No automatic removal or periodic monitoring is configured.

## FTS analyzer inspection (2026-10-02)

The 2.2.0 roadmap authorizes engine changes needed for missing RomeQL features.
The reviewed additive hook extracts tokenizer registration from
`core/index_method/fts/mod.rs` into `core/index_method/fts/analysis.rs` and exports
a streaming visitor shared with real index registration. No storage change or
new dependency. [Design and evidence](proposals/fts-analyzer-inspection.md).
`fts_analysis_hook` passes three tests and existing `fulltext` passes eight.
Retain token/case/Unicode/long-token/ngram boundaries, early-stop, native-query
and reopen regressions. Remove this exception when upstream provides an
equivalent shared visitor and these tests pass without the extraction.

## Follow-up audit (2026-09-30)

Rechecked every `core/` deviation from 0.8.1 and fetched upstream main at
`36da5b2e435cb07bba3bed2c7e7eef236b6b2e64`. The two retired overrides remain
absent; all seven active exceptions still cover missing behavior. No additional
core patch can be removed based on this comparison. Historical proposal pages
now state their current retained/partial/retired status before their old review
records. Full file inventory, replacement criteria, future sync hazards and
fresh regression evidence are in the
follow-up audit.

## MVCC metering follow-up (2026-09-30)

Default-on Cloud qualification exposed an outdated assumption in the retained
execution-metering exception: MVCC cursors return a logical table ID from
`root_page()`, so physical root 1 cannot identify catalog mutations. The Insert
and Delete meter hooks now use the same compiler catalog identity as schema-read
attribution. DDL catalog writes consume no customer mutation budget; inserted,
copied and deleted customer rows still count. This adapts the existing approved
exception without changing upstream change counters or transaction lifecycle.
The regression is `mvcc_schema_writes_do_not_consume_customer_mutation_budgets`
in `schema_meter.rs`; final milestone validation is recorded in the follow-up
audit.

## Upstream sync: Turso 0.8.3 (2026-10-20)

Candidate upstream commit: `607bff9fd0e547a25a97b3bf06e83bcc5d99d53c` (tag `v0.8.3`).
The merge had no conflicts. Upstream changed seven files that RomeDB also
changes (`Cargo.toml`, `Cargo.lock`, `core/connection.rs`,
`core/translate/emitter/update.rs`, `core/translate/sequence.rs`,
`core/vdbe/execute.rs`, `core/vdbe/mod.rs`), always in different places from
the local patches. 0.8.3 replaces no exception, so every patch stays. Notes and
evidence: [integration notes](turso-0.8.3.md), checklist.

| Existing exception | 0.8.3 decision | Evidence |
|---|---|---|
| Trigger interruption | Keep | `op_program` in 0.8.3 still joins `Interrupt` with `Busy`. Native trigger suite: 98 passed. |
| Scalar read errors | Keep | The only upstream `execute.rs` change is in `op_journal_mode_inner`. Native external API suite: 14 passed. |
| Checkpoint WAL barrier companion | Keep | No upstream change under `core/storage/`. `checkpoint_barrier` 5 and `checkpoint_crash_atomicity` 2 passed; first FULL commit 1 passed. |
| Named-savepoint cancellation recovery | Keep | Upstream adds `end_abandoned_mvcc_commit` near, but not in, the savepoint code. Statement lifecycle 46 and savepoint filter 12 passed. |
| FTS backing integrity / root reclamation | Keep | No upstream change under `core/index_method/`. Native integrity suite: 36 passed. |
| FTS savepoint cursor pins | Keep | Same files unchanged. Native index-method suite: 131 passed. |
| FTS analyzer inspection | Keep | `fts_analysis_hook` 3 and `fulltext` 8 passed. |
| Execution counters and budgets | Keep, checked | Upstream adds compare opcodes and an `is_called` column read to the AUTOINCREMENT explicit-ID path and ends abandoned MVCC commits fully. New `turso_083` regression: an explicit-ID insert is still one customer mutation in WAL and MVCC modes. All metering suites pass with unchanged expectations (`profile` 4, `execution_meter` 14, `mutation_meter` 6, `schema_meter` 8, `index_method_meter` 4, nine `metered_*` suites 44). |

Not taken: the whole-table DELETE fast path (`58f350a90`) is not in 0.8.3. The
hazard recorded in the 2026-09-30 audit still applies to the next sync.

## Upstream sync: Turso 0.8.1 (2026-09-30)

Candidate upstream commit: `8549c16595d2faf1bdd6ee24aee0be8bfabb3d4a`.
This section supersedes the 0.7.2 retention decisions below for current source;
the prior release record remains historical evidence.

| Existing exception | 0.8.1 decision | Replacement or adaptation |
|---|---|---|
| Trigger interruption | Retain | Upstream still merges Interrupt with Busy in subprogram execution; preserve the distinction and saved subprogram state. |
| FTS cache isolation | Remove old patch, retain regression | Upstream replaces the monolithic FTS implementation with snapshot-aware segment/directory state. The old pager-identity patch no longer applies; public concurrent FTS snapshot tests pass. Native index-method suite: 131 passed, including the retained isolation regression. |
| First FULL WAL commit | Remove old patch, retain regression | Upstream prepared-frame/dirty-WAL `need_fsync` logic supplies the barrier. Native first-FULL regression passed; the raw sync probe also confirms the write barrier. |
| FTS backing integrity/root reclamation | Retain necessary remainder | Upstream supplies part of the backing-store lifecycle; local logical-count exclusion and DROP TABLE index ordering remain needed; native integrity suite: 36 passed. |
| Scalar read errors | Adapt | Preserve the read-only ExtensionError cleanup exception through the new VDBE dispatch/abort implementation. Native external API suite: 14 passed; scalar-error transaction probe passed. |
| Checkpoint WAL barrier | Retain pending-completion companion | Upstream supplies the main pre-backfill barrier and broader cleanup. Retain explicit failed-completion observation/retry and published automatic-commit cleanup. RomeDB checkpoint_barrier: 5 passed; checkpoint_crash_atomicity: 2 passed. |
| Named-savepoint cancellation recovery | Adapt | Keep named-frame prior-poison state and recovery after successful rollback in the new connection/savepoint implementation. Native statement lifecycle: 46 passed; savepoint integration filter: 12 passed. |
| Execution counters and budgets | Adapt | Attach meters in both normal and explain dispatch paths; preserve completed cursor visits, cooperative budget interruption, mutation/schema classification and incremental Count. New window-generated ephemeral deletes exclude logical mutations. Full scoped metering/profile suites passed. |

New approved exception: [FTS savepoint cursor pins](proposals/turso-081-fts-savepoint.md).
The user approved its focused patch on 2026-09-30. Backing cursors register only
after allocation in a stable Box; rollback invalidates them before truncating
new pages. Existing fulltext suite: 8 passed; new public 0.8.1 suite: 6 passed.
The exact isolated patch and its safety invariant are in the review record.
Broader current-source regression results are tracked in the
upgrade checklist; pending checks are not release evidence.

## Release review: 2026-09-25

Owner: RomeDB maintainers. Review this register on every release and upstream
sync; keep source links and run the named regressions before changing a patch.
Inspected upstream main at
[`64b8ef5742fc18937f9c89806c81e3f6475dc7a3`](https://github.com/tursodatabase/turso/commit/64b8ef5742fc18937f9c89806c81e3f6475dc7a3)
(0.8.0-pre.13). No upstream merge is part of the current 2.1 candidate.

- **First FULL commit:** upstream
  [`fa8aeccfcf50feb715801a76478336dca72935fa`](https://github.com/tursodatabase/turso/commit/fa8aeccfcf50feb715801a76478336dca72935fa)
  checks prepared frames as well as WAL dirtiness before FULL sync, matching the
  central requirement of our patch. This is a concrete removal candidate on a
  future sync; the RomeDB regression has not been run against that upstream
  replacement, and the existing pinned base still needs its local fix.
- **FTS backing storage:** upstream
  [`7e485be941a53b0fbf6d7f132c111787ca3814ce`](https://github.com/tursodatabase/turso/commit/7e485be941a53b0fbf6d7f132c111787ca3814ce)
  contains backing-index integrity/lifecycle changes. Recheck physical-corruption
  detection, root reclamation and upgrade/restore on an actual sync candidate;
  source inspection alone does not establish all our required behaviors.
- **Trigger interruption:** the inspected upstream `op_program` still combines
  `StepResult::Interrupt | StepResult::Busy` into Busy. Retain the local patch.
- **FTS cache isolation and scalar read errors:** upstream FTS and VDBE structures
  have changed substantially. No behaviorally equivalent replacement has been
  qualified. Retain both on the pinned base and test during a future sync.

The review also found a separate upstream NORMAL-mode checkpoint durability fix,
[`cc26d08508cbe045472fa3015e2bce4a389b5e06`](https://github.com/tursodatabase/turso/commit/cc26d08508cbe045472fa3015e2bce4a389b5e06).
The pinned build reproduces its power-loss defect. A separately reviewed
[approved backport](proposals/checkpoint-wal-sync.md) also handles failed-sync
retry and automatic-checkpoint completion on this pin. The user approved its
exact six-file patch on 2026-09-25, and shared core now includes that patch.
Integrated acceptance is recorded separately. It is not the
first-FULL-commit exception and must not be silently included in it.

| Exception / local commit | Required behavior and regression | When to update or remove |
|---|---|---|
| Trigger interruption `ded389aea` | `OpProgram` preserves interruption versus contention; keep the RomeDB after-write trigger cancellation regression and affected native trigger suite. Review. | Upstream propagates Interrupt correctly through subprograms, with saved state and unchanged Busy handling. |
| FTS cache isolation `fb246a8e4` | `test_fts_cache_preserves_connection_snapshots`: no uncommitted membership/score leaks; existing reader snapshot survives writer commit. [Review](proposals/fts-cache-snapshot.md). | Upstream directory caching is scoped to the owning pager or otherwise proves equivalent isolation and rollback behavior. |
| First FULL commit WAL sync `fe2ccd404` | Native first-commit sync probe and FULL/NORMAL/OFF controls. The historical browser flush-fault matrix is supporting evidence, not a current gate. [Review](proposals/wal-first-commit-sync.md). | Upstream flushes prepared frames before publishing/acknowledging FULL commits, including the first commit. |
| FTS backing storage `12109384a` | `test_fts_backing_storage_integrity`, `test_drop_table_frees_backing_and_ordinary_indexes`, `test_fts_backing_storage_physical_corruption`; upgrade/restore rehearsal. [Review](proposals/fts-integrity.md). | Upstream excludes backing trees only from inappropriate logical counts, retains physical checks, and reclaims all backing roots on teardown/rollback. Existing orphan pages still need separate handling. |
| Scalar read errors `f26014f04` | `scalar_read_error_preserves_transactions_and_named_savepoints`, RomeDB user-function tests and Node cancellation/transaction tests. [Review](proposals/udf-error-transaction.md). | Upstream read-only extension failures preserve caller transactions, savepoints and changes(), while autocommit cleanup and writer rollback controls pass. |
| Checkpoint WAL barrier `5f4133d73` | `checkpoint_barrier` and `checkpoint_crash_atomicity`: NORMAL backfill follows successful WAL sync; failed returned/immediate/deferred completions cannot skip the barrier; automatic failures preserve published writes; FULL recovery preserves acknowledged writes. [Review](proposals/checkpoint-wal-sync.md). | Upstream includes the barrier from `cc26d08508cbe045472fa3015e2bce4a389b5e06` and equivalent pending-completion, retry and automatic-checkpoint cleanup. Retain all regressions and recheck checkpoint/VACUUM callers before removing the local companion. |
| Named-savepoint cancellation recovery `3ae0065e5` | `cancelled_plain_native_writes_undo_rows_and_preserve_caller_work` plus five core lifecycle regressions: rollback restores the selected prior poison state, allowing COMMIT/root RELEASE after canceled rows are undone; pre-existing poison and unrecovered abandonment still reject commit. [Review](proposals/cancellation-savepoint-poison.md). | Upstream restores transaction safety state after successful named-savepoint rollback, including prior poison, nested/same-name boundaries and mirrored rollback. Remove only when the frontend, lifecycle and savepoint regressions pass without this local patch. |
| Execution counters and budgets (2026-09-26, approved Cloud 0.3.0 dependency) | Successful rowid/index seeks, initial index-method positioning after completed query_start, and executed deferred table reads count exactly once; missing rows do not count. `index_method_meter` covers first/subsequent/empty FTS results, deferred payload reads, managed materialization, sticky limits and queued I/O; extension-internal traversal remains unqualified. Optional retained per-execution physical-work counters, VM-step budgets and read budgets with bounded overshoot; metered Count advances incrementally; HashBuild does not duplicate its source cursor visit; read/mutation-budget interrupts use cooperative cleanup to preserve caller transactions. Retained ordinary row mutation events distinguish replacement deletions, exclude CDC maintenance and survive rollback; active writers prevent scope replacement. Maintenance views share all work/budgets while suppressing only logical mutation events. Opt-in schema visits and an independent schema budget preserve default all-row counters; `schema_meter` qualifies DDL source/schema separation, rollback and queued I/O. `romedb-tests --test profile --test execution_meter --test mutation_meter --test schema_meter` includes failure, rollback, boundaries and queued I/O. [Review](proposals/execution-metering.md). | Upstream includes equivalent completed-position counters and retained execution/budget APIs, and all listed regressions pass without these patches. Known gap: reads inside an index-method extension's own traversal (FTS, ANN) have no qualification test, so their count may be incomplete. Cloud bills rows read and written from these counters and accepts that gap. |

The canceled-write recovery fix was separately approved on 2026-09-25 after the
first optimized package matrix exposed stale poison at COMMIT/root RELEASE.
The inspected upstream revision still lacks the named-frame state; no equivalent
upstream replacement has been qualified.

## Retired exception: WASI FTS

The user removed browser support from active V2 development. Commit `ae6777a17`
reverts `2ef619c07` in isolation, restoring the ten core files to their prior state.
Frontend WASI feature wiring, the browser package and its build/probe code are
also removed. This exception is **not active** and must not be reapplied on an
upstream sync. The reviewed patches remain historical evidence for any separately
requested future browser work.

## Upstream sync checklist

- [ ] Record the exact proposed upstream SHA and inspect its source, changes and
  issues/PRs for each exception. Add verified issue/PR URLs here when available.
- [ ] Compare behavior, not just patch text. Record per entry: retain, adapt, or
  remove; upstream evidence; candidate commit; and regression results.
- [ ] On an isolated sync branch, drop a local exception only after its regression
  passes using the upstream replacement. A patch conflict is not proof of a fix.
- [ ] If upstream is only partially equivalent, retain the necessary portion and
  document its revised scope. Materially new core behavior needs separate review.
- [ ] Run the affected upstream tests and combined RomeDB acceptance. For storage
  changes include native upgrade/restore and fault checks. Browser/WASI checks
  are outside the active native scope.
- [ ] Update this register, UPSTREAM.md, proposal status and release provenance in
  the sync commit. Preserve the regression even when the local patch is removed.

The scalar-error patch and retired WASI patch remain byte-for-byte available
in their review files. Their SHA-256 values are respectively
`953e88426f9c5e5ea52a3f41251c7e64bc0b2bb06c70557f58ff596454c68b0a`
and `b6771224bade4381592906cbc16962add85819f1881e56860bc02a10a992d881`.
The frontend WASI wiring patch is
`68adbabbe59c6c2b6b160aa3b12f046e8af17bef2aad6f790d1bddb9b9efa0a1`.
