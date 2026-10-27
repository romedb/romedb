# Core review: sync prepared commit frames in FULL mode

Current status (2026-09-30): **retired in the Turso 0.8.1 sync**. Upstream now
includes prepared frames in the FULL-mode sync decision. Keep the first-commit
sync regression; do not reapply the historical patch below. The separate
checkpoint pending-completion fix remains necessary.

## Historical approval and reproduction (0.7.2)

Status: approved by the user on 2026-09-25 and integrated unchanged in isolated
commit `fe2ccd404e005a198fac5365712cacaa54d47a2c`. The commit's two-file diff
matches the reviewed patch SHA-256 below. The integrated native sync probe and
installed-browser fault matrix pass.
This bug was discovered during OPFS fault qualification.

## Reproduced failure

The first ordinary commit after reopening a database can return successfully
under `PRAGMA synchronous=FULL` without syncing its new WAL commit frames.
The native [standalone probe](wal-first-commit-sync-repro.rs) uses only public
engine APIs and an instrumented `MemoryIO`. It records calls made to the WAL
file and reproduces the behavior independently of browser JavaScript or RomeDB
lowering. A header sync does not cover subsequent frame writes.

Run from the active checkout:

```sh
cargo run --locked -p romedb --example wal_sync_probe
```

Observed control trace (`/tmp/romedb-wal-sync-native-control.log`):

```text
commit 2, mode Full, WAL events: ["write", "sync", "write"]
commit 3, mode Full, WAL events: ["write", "sync"]
commit 4, mode Full, WAL events: ["write", "sync"]
```

The probe fails because the first successful commit's last WAL operation is a
write, not a sync. The browser quota and short-write controls recover correctly,
but a first-commit WAL-flush injection does not trigger on this core. Initial
page-close recovery evidence therefore does not prove power-loss durability.

## Cause and proposed change

`Pager::commit_wal_inner` submits prepared frame writes, then decides whether to
sync by consulting `wal.is_dirty()`. Those frames are not published through
`commit_prepared_frames` until after the sync phase. Publishing sets the dirty
flag, so it can still be false at the first commit's sync decision even though
new frames have just been written. Subsequent commits encounter a dirty flag
left by earlier publication, which masks the first-commit bug.

The isolated candidate changes the FULL-mode sync condition to include a
nonempty prepared-frame list. NORMAL/OFF behavior, frame encoding, write ordering,
publication ordering and public APIs remain unchanged. Existing raw/spilled WAL
writes still use the dirty check. The candidate adds a regression to the existing
native WAL integration module; it checks three commits after reopen, including
the first, and requires a sync after the final frame write.

Candidate checkout: `/tmp/romedb-wal-sync-review`, based on `fb246a8e4`.
The active checkout retains the standalone reproducer and now includes the
approved core fix. The following records its pre-integration candidate evidence.
The [isolated patch](wal-first-commit-sync.patch) changes only
`core/storage/pager.rs` and `tests/integration/wal/test_wal.rs`. SHA-256:
`72ed0afb451277c9406cc0a9b65b7867e7e8d47105296ce1c011476dee994f2d`.
The native candidate passes the exact probe; its first trace is now
`["write", "sync", "write", "sync"]`, and both subsequent commits end in sync.
Log: `/tmp/romedb-wal-sync-native-candidate.log`.
The affected native WAL integration suite passes **9 tests**, with **1 existing
ignored flaky writer/reader test**; no failures. Log:
`/tmp/romedb-wal-sync-wal-suite.log`.

The candidate also passes **11 OPFS fault scenarios in each of Chromium
149.0.7827.55 and Firefox 151.0**. The test-only HTTP shim wraps actual browser
access handles; it is not part of the client package. Cases cover one-shot and
persistent WAL quota errors, partial writes, errors before/after WAL flush,
checkpoint database-write/flush/truncate failures, opening-read errors, handle
close errors and storage-worker failure. Recovery retains previously acknowledged
rows, preserves scalar/spatial index integrity and permits a subsequent write.
Checkpoint failures report `[1, null, null]` through the native SQL result;
the tests require an error or explicit incomplete checkpoint, never a false
successful checkpoint report. Failed commit flushes recover the complete new row
in these runs, demonstrating an ambiguous commit outcome rather than rollback.

Logs: `/tmp/romedb-wal-sync-faults-chromium-v2.log` and
`/tmp/romedb-wal-sync-faults-firefox-v2.log`. This is candidate-only evidence, not
active-core acceptance. Candidate WASM: 21,967,692 bytes, SHA-256
`5bbb72f01cc56e07074083e9724fd4d59488ee31826cc1f71bd51aa0644f48a4`.
It is under the temporary checkout's browser `dist/`. At candidate qualification,
the active browser WASM still had SHA-256
`b1477b809356389451b3ff511250beb3805895be99bd7afd226732f68ad8c06a`.
The WASM target cache was used for the candidate. Subsequent active-source
artifacts and checks are recorded separately below.

The selected core suites pass **76 WAL unit tests and 1 checkpoint-phase test**,
with no failures or ignored cases in those suites. Logs:
`/tmp/romedb-wal-sync-core-wal-final.log` and
`/tmp/romedb-wal-sync-core-checkpoint.log`. An initial mistaken plural module
filter selected zero tests; it is not counted as evidence.

Native formatting, isolated diff checks and active-probe Clippy with warnings
denied pass. Total native affected-suite evidence: **86 passed, 1 preexisting
ignored flaky integration test**, plus the standalone control/fix probe.
These counts describe the reviewed candidate; V2 remains unreleased.

## Review boundary

The repository [workflow](../../../../RomeDB-Workflow.md) requires a separately
reviewed design, isolated commit, patch inventory and regression evidence for an
upstream core exception. This proposal is separate from both the scalar-error
transaction and WASI full-text proposals. Approval of an earlier proposal does
not authorize this patch. The user explicitly approved this patch separately;
it was integrated unchanged in its own commit. Provenance records it. The
integrated qualification distinguishes the new
active-source artifacts and results from the pre-integration candidate counts.
