# Core review: make NORMAL-mode checkpoint backfill crash-atomic

Current status (2026-09-30): **retain the pending-completion/retry companion on
Turso 0.8.1**. The upstream release already supplies the WAL barrier itself.
Local code still observes failed deferred completions, resets the failed barrier
and preserves a committed write when automatic checkpointing fails. The original
patch below describes the 0.7.2 integration.

## Historical approval and reproduction (0.7.2)

Status: **approved by the user on 2026-09-25; integrated as the exact reviewed patch**.
All seven permanent regressions pass on shared source. Combined-source and
artifact acceptance remain separate release gates. This is an additional exception, separate from
the previously approved first-commit FULL-mode sync fix.

## Reproduced defect

A transaction committed under supported `PRAGMA synchronous=NORMAL` can be
backfilled into the database file before its WAL frames have been synced. Power
loss during that backfill can leave a partial transaction in the database while
its unsynced WAL frames disappear. The database then cannot recover a complete
committed state.

The native [standalone trace](checkpoint-wal-sync/repro.rs) uses RomeDB's public
`Database::open_with_io` with instrumented synchronous UnixIO and real files.
After a NORMAL transaction writes six frames, the baseline checkpoint reports
`[0,0,0]` but traces six database-page writes, database sync, and only then WAL
sync. Its assertion requiring WAL sync before the first database write fails.
The earlier WAL header sync happened before those six frame writes and does not
make them durable. Log: `/tmp/romedb-normal-checkpoint-before.log`.

The [upstream power-loss model](checkpoint-wal-sync/crash_atomicity.rs) is also
reproduced against the pinned core. It captures durable file images, persists
alternating unsynced database writes, drops unsynced WAL writes, and reopens the
result. The FULL control passes. NORMAL fails `integrity_check` with page 9 row
IDs outside their parent's row bounds. This is a deterministic simulated
power-loss outcome, not a claim to have interrupted physical hardware power.
Log: `/tmp/romedb-checkpoint-crash-before.log`.

The pinned connection default is FULL, and the control did not reproduce this
defect under FULL. NORMAL remains supported and must preserve transaction
atomicity even though an unsynced acknowledged transaction can be lost at power
failure. The existing returned-I/O-error matrix and ordinary process exits do
not establish this power-loss property.

## Proposed upstream backport

Upstream fixed the same bug in
[cc26d08508cbe045472fa3015e2bce4a389b5e06](https://github.com/tursodatabase/turso/commit/cc26d08508cbe045472fa3015e2bce4a389b5e06).
The [upstream portion](checkpoint-wal-sync/upstream-backport.patch) is the unchanged
five-core-file portion of that commit: **75 insertions, 23 deletions**. SHA-256:

```text
acf1ec5ab65a00fedd1ccb4f4734f6fd37b89ce5b951a224845040b5bab81425
```

It adds a WAL-sync checkpoint state after the backfill frame range is fixed under
checkpoint locks and before database-page writes begin. It skips the barrier
only when no frames need backfill or the effective mode is OFF. The effective
sync mode is passed through the WAL trait and existing pager/MVCC callers;
VACUUM's already-forced FULL checkpoint gains the corresponding extra sync, and
its focused sync-count assertions are updated. File formats, transaction framing
and SQL syntax do not change.

The first review also reproduced a retry defect in this backport on our pin:
the WAL state advances past its barrier before an asynchronous sync completion
is checked. If that completion fails outside the state machine, a subsequent
checkpoint can write database pages without a fresh successful WAL sync.
Synchronous error returns do not exhibit this retry path. The local companion
retains `SyncWalPending` until successful completion. External error cleanup
releases ownership only for that finished failed barrier, before any database
backfill I/O was submitted; other checkpoint phases retain their existing I/O
ownership behavior.

The actual automatic-checkpoint threshold exposed another error path: a failed
completion could enter ordinary rollback after its transaction had already
published and released the writer lock, causing an assertion failure. The
companion recognizes only `Program::Committing` plus pager `AutoCheckpoint` plus
the finished failed owned barrier. It reenters the existing automatic-checkpoint
error path, which completes the committed transaction's bookkeeping and cleans
checkpoint state. This matches the existing behavior for immediate sync errors;
it does not replay or roll back the published write.

The [combined integration patch](checkpoint-wal-sync/backport.patch) changes six
core files: **152 insertions, 25 deletions**. Its exact SHA-256 is:

```text
5154633cc2c1187efa0b3066c008ac46f1f999286fb48ccacf3a1a7743da7d88
```

The additional sixth file is `core/vdbe/mod.rs`; the pager/WAL companion owns the
pending completion and narrow cleanup. This is the patch proposed for approval,
not the unchanged upstream portion alone.

A frontend-only check of explicit PRAGMAs is insufficient: automatic and shutdown
checkpoints also enter this engine state machine, and connections can select
NORMAL. Rejecting NORMAL or silently treating it as FULL would change its
existing behavior and would not fix the engine invariant itself.

## Candidate verification

The baseline is HEAD `d442099b66ebe8920fa457e52b10255623a0e1f8` plus the current
RomeDB 2.1 preparation changes. Core files are unchanged from that HEAD.
`git apply --check` passes against this active source.
The temporary candidate checkout is `/tmp/romedb-checkpoint-wal-sync-patched`,
created from the same HEAD and overlaid with the current preparation files before
applying the backport and candidate retry correction. The shared checkout's core
was untouched during review; after approval its six-file diff has exactly the
combined patch SHA-256 recorded above.

The upstream regression removes the newer `SqliteDialect` argument/import for
our API and strengthens the FULL-mode assertion to require all acknowledged
writes to survive. NORMAL permits a complete old or new committed state; its
integrity assertion remains intact. The trace, crash and boundary tests live under this proposal,
not as an ignored or failing gate in the release test suite. Temporary standalone
Cargo manifests use this checkout's exact resolved dependency versions.

- [x] Baseline native trace fails at the missing barrier.
- [x] Baseline upstream crash model: FULL passes, NORMAL fails integrity.
- [x] Exact upstream patch passes applicability check.
- [x] Patched native trace passes with WAL sync before database writes.
- [x] Patched NORMAL and FULL crash-model cases pass, including the stronger
  FULL acknowledged-write assertion.
- [x] Five boundary tests pass: OFF and empty-checkpoint controls plus explicit,
  direct blocking and automatic checkpoint failure/retry. Each failure path
  covers returned errors, immediate failed completions and deferred failed
  completions. No database backfill follows a failed barrier; subsequent writes,
  fresh synced checkpoints, integrity checks and reopen succeed.
- [x] Combined patch applies cleanly to the current shared source; independent
  source review confirms cleanup is restricted to the completed owned barrier.
- [x] Affected upstream unit filters pass with `fts,conn_raw_api`: 126 checkpoint
  and 38 VACUUM tests, zero failures. Logs:
  `/tmp/romedb-checkpoint-core-unit.log` and `/tmp/romedb-vacuum-core-unit.log`.
- [x] Separate user approval of the combined six-file patch and exception register.
- [x] Isolated core integration with all seven permanent regressions passing.
- [ ] Integrated focused regressions and combined release checks.

The seven focused tests pass in
`/tmp/romedb-checkpoint-pending-auto-reentry.log`. The automatic fixture executes
bounded small commits until it reaches the engine's actual threshold above 1000
unbackfilled frames; this pin ignores `PRAGMA wal_autocheckpoint` overrides.
These are isolated-checkout results, not evidence that the shared release source
already includes the patch or that final artifacts have been qualified.

The final repeatable before/after run is recorded in the
[machine-readable receipt](checkpoint-wal-sync/evidence.json). Both runs use the
same final regression sources and exact lockfile. Baseline trace, NORMAL crash
case and failed-barrier boundaries fail as expected; the stronger FULL control
passes. All patched groups pass. The receipt includes all six core source hashes,
regression hashes, exact patch hash and raw log identities. The final local logs
are under `/tmp/romedb-checkpoint-final-before` and
`/tmp/romedb-checkpoint-final-after`.

To reproduce with the configured Rust 1.88 environment, run the standalone
[harness](checkpoint-wal-sync/reproduce.py) against either checkout. Its evidence
directory must be new, and baseline failure is expected:

```sh
source /tmp/romedb-v2-env.sh
python3 romedb/docs/proposals/checkpoint-wal-sync/reproduce.py . /tmp/checkpoint-before-evidence
python3 romedb/docs/proposals/checkpoint-wal-sync/reproduce.py /tmp/romedb-checkpoint-wal-sync-patched /tmp/checkpoint-after-evidence --target-dir /home/tan/Sites/fastdb/turso/target
```

The harness runs one native trace, two crash-model cases and five boundary tests;
it records return codes, source/lockfile hashes, patch hash and raw logs. It does
not apply patches or change the selected checkout.

## Integration and maintenance decision

Approval covers this combined patch as a new maintained core exception, including
the pin-specific failed-sync retry and automatic-checkpoint handling. Integration
will use an isolated commit, keep permanent regressions, update `UPSTREAM.md` and
the core exception register, and rerun combined RomeDB and exact-artifact checks.
RomeDB maintainers own review on every upstream sync and release. Remove or adapt
this exception only after upstream provides both the durability barrier and
equivalent error/retry behavior and these regressions pass without the local patch.

The [repository workflow](../../../../RomeDB-Workflow.md) says: “Any necessary
local core exception requires a separately reviewed design decision, isolated
commit, patch inventory entry, and relevant regression tests; it is not
automatically authorized by this workflow.” Previous approval of the first FULL
commit fix did not cover this separate NORMAL checkpoint defect. The user has
now explicitly approved the checkpoint core fix. Permanent tests live in
`romedb/tests/tests/checkpoint_barrier.rs` and
`romedb/tests/tests/checkpoint_crash_atomicity.rs`; the latter uses a standard
boxed test error in place of `anyhow` without adding a dependency or changing
its model and assertions. The review snapshot and receipts remain immutable.

Integrated focused command:
`cargo test --locked -p romedb-tests --test checkpoint_barrier --test checkpoint_crash_atomicity`.
All seven tests pass; log `/tmp/romedb-checkpoint-integrated.log`. The shared
six-file core diff is byte-for-byte identical to the approved patch. Cargo.lock
is unchanged. Full scoped verification and exact-artifact qualification follow
in the production checklist rather than being inferred from this focused pass.

Isolated integration commit:
[`5f4133d732eb2a297bf32af2f7a3fd3c305be8c5`](https://github.com/romedb/romedb/commit/5f4133d732eb2a297bf32af2f7a3fd3c305be8c5).
