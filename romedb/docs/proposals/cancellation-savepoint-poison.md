# Canceled-write savepoint recovery

Raw logs were deleted during the user-requested 2026-10-01 cleanup. The
qualification summary below is retained; Git history retains tracked log content.

Current status (2026-09-30): **retained and adapted to Turso 0.8.1**. The selected
named savepoint still needs its prior transaction-poison state restored after
successful rollback. Neither 0.8.1 nor the inspected upstream main supplies it.
The release-gate wording below belongs to the completed 2.1.0 history; see the
[current exception audit](../core-exception-audit-2026-09-30.md).

## Historical approval and reproduction (0.7.2)

Status: **user approved and integrated** on 2026-09-25 in isolated core commit
`3ae0065e5`. Combined source and fresh artifact qualification remain release gates.
The approved checkpoint durability correction remains integrated and is unrelated
to this newly reproduced release blocker.

## Reproduced failure

The exact optimized 2.1.0 candidate from
`a4df9cc27da6b4d6f0fdcc82aff5b61edafdfdff` fails the installed Python cancellation
contract on CPython 3.10, 3.12 and 3.14. A minimal Node 24 reproduction fails in
the same way. After prior caller writes, canceling a native `INSERT ... SELECT`
returns `FDB_CANCELLED` and leaves the outer transaction active. The frontend
rolls back the canceled statement: its rows disappear and earlier work remains
visible. However, the later `COMMIT` or root `RELEASE` rejects the transaction
as an abandoned write and rolls back the earlier work too. Read-only cancellation
and uncanceled controls commit successfully. Previously committed data remains
intact; this is incorrect recovery of an active caller transaction.

Permanent installed-artifact regressions now cover in-flight plain writes through
COMMIT and root RELEASE in Node and the shared C ABI. Both prove that native
inserts occurred before cancellation, verify undo and prior work, and then require
successful caller completion; Node also reopens the file. They fail at the same
commit boundary on original Node 22/24 and C artifacts; see
[the receipt](cancellation-savepoint-poison/installed-regressions-before.json),
Node 22 (raw log removed),
Node 24 (raw log removed), and
C ABI (raw log removed).

A timing-independent Rust regression extends the existing plain-native write
cancellation test to commit or release its caller scope, rather than only
rolling back. It fails on the unmodified engine with the same error. The ordinary
source suite had not covered this final commit boundary.

- [Python reproduction](cancellation-savepoint-poison/reproduce.py) and
  [before results](cancellation-savepoint-poison/python-before.json).
- [Node reproduction](cancellation-savepoint-poison/reproduce.cjs) and
  [before results](cancellation-savepoint-poison/node-before.json).
- [Deterministic regression patch](cancellation-savepoint-poison/frontend-regression.patch)
  and failing baseline log (raw log removed).

## Cause and proposed correction

The pinned engine marks an interrupted, unjournaled native writer as unsafe to
commit. That protection is correct when the partial write remains. Named
savepoint rollback restores the database pages but currently does not restore
this transaction marker. No supported frontend API repairs the stale marker.
Forcing statement journals once during preparation is insufficient because
engine schema re-preparation can replace the program, after a schema change. The recovery belongs at the engine savepoint boundary.

The proposed correction snapshots the marker when each named savepoint opens
and restores that snapshot only after every required rollback succeeds. This
permits commit after a savepoint actually undoes the offending write. It retains
an earlier marker when the savepoint was opened after an abandoned write;
releasing a savepoint without rollback does not clear anything. No file format,
SQL syntax, dependency or public client API changes are proposed.

The inspected upstream revision
[`64b8ef5742fc18937f9c89806c81e3f6475dc7a3`](https://github.com/tursodatabase/turso/commit/64b8ef5742fc18937f9c89806c81e3f6475dc7a3)
still lacks this state in named-savepoint frames. This proposal is a local
correctness companion, not an existing upstream backport. The current change is
eleven added implementation lines plus a comment adjustment in
`core/connection.rs` and `core/vdbe/execute.rs`; five lifecycle tests cover its
recovery and retained-safety behavior. The attached-database regression is extra
engine coverage; RomeDB does not enable the experimental ATTACH option.

## Verification and maintenance

The [exact three-core-file patch](cancellation-savepoint-poison/core.patch)
includes the two implementation files and five lifecycle regressions. SHA-256:
`4dea3c65be5cbb174721fb25eb164d29e9641681d7028b48bc6f77fd1958e122`.
It applies to `a4df9cc27da6b4d6f0fdcc82aff5b61edafdfdff`.

The isolated candidate passes **41 statement-lifecycle tests** with `fts` and
`conn_raw_api` enabled; see the log (raw log removed).
Coverage includes WAL and MVCC recovery, COMMIT/root RELEASE, unchanged rejection
of unrecovered abandoned writes, pre-existing poison, nested/same-name scopes,
and an attached writer whose savepoint is created lazily. The attached case
also closes all handles and independently reopens both files to check persisted
rows and integrity. The experimental `PRAGMA aux.integrity_check` path uses the
wrong schema on this pin; it was not used to claim integrity. RomeDB's options
do not enable ATTACH, and fixing that separate upstream path is outside this
exception. No injected rollback-I/O-failure test is claimed.

Two independent code reviews found no blocking defect. The marker is restored
only after the currently fallible rollback paths finish; a failed target lookup
retains the marker. The expanded frontend regression passes all ten deterministic cancellation and
caller-boundary combinations within one test; see
the after log (raw log removed). The additional
savepoint tests also pass: **20 tests**, overlapping the lifecycle filter,
with no failures or ignored tests; see
the savepoint log (raw log removed).
Scoped formatting and core/frontend Clippy with warnings denied pass; see
the Clippy log (raw log removed).

Commands (Rust 1.88.0, isolated worktree, locked dependencies):

```sh
cargo test --locked -p turso_core --lib --features fts,conn_raw_api statement_lifecycle_tests -- --nocapture
cargo test --locked -p turso_core --lib --features fts,conn_raw_api savepoint -- --nocapture
cargo test --locked -p romedb --lib cancelled_plain_native_writes_undo_rows_and_preserve_caller_work -- --nocapture
cargo clippy --locked -p turso_core -p romedb --lib --tests --features turso_core/fts,turso_core/conn_raw_api --no-deps -- -D warnings
cargo fmt -p turso_core -p romedb -- --check
```

These tests qualify the proposed correction, not a newly built release bundle.
The user approved this exact patch. Integrated commit `3ae0065e5` matches its
SHA-256 byte for byte. The frontend regression is retained and the
[maintenance register](../core-exceptions.md) records the seventh exception.
Combined source checks and rebuilt installed artifacts remain separate gates;
the first failed bundle must not be published.

The parent `RomeDB-Workflow.md` code-boundary policy requires a separately
reviewed design decision and isolated commit for each core exception. The user
approved this correction separately from the checkpoint fix after reviewing the
exact patch and evidence above.

Maintain this as a separate core exception with owner RomeDB
maintainers. Review on every upstream sync and release. Remove or adapt it only
when upstream restores the marker correctly at named-savepoint rollback,
including pre-existing poison and nested scopes, and the cancellation/commit and
abandonment regressions pass without the local patch. Do not remove the general
abandoned-write protection or blindly clear the marker on any rollback.
