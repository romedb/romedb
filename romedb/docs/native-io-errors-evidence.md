# Native returned-I/O-error qualification

This is the finite P4 matrix for the Linux x64 production release, complementing
[process-exit recovery](recovery-io-evidence.md). It uses the public RomeDB
frontend with real filesystem files and test-only synchronous `UnixIO` wrappers.
It does not change production I/O or upstream core.

## Checklist

- [x] Inject four selected failures during WAL COMMIT and database checkpoint.
- [x] Observe COMMIT errors and exact failed-checkpoint result status.
- [x] Require atomic old/new document, relational and V2-index state on reopen.
- [x] Require acknowledged COMMIT data after every checkpoint failure.
- [x] Require rollback, subsequent committed indexed writes, successful checkpoint
  and another reopen after each failure.
- [x] Run focused recovery tests and RomeDB package linting.

## Fault boundary and fixture

The eight cases cross COMMIT/checkpoint with:

| Failure | Deterministic injection |
|---|---|
| ENOSPC | Return the Linux disk-full error before a selected write |
| EIO | Return the Linux I/O error before a selected write |
| Partial write followed by failure | Write half of a selected buffer to the real file, then return `UnexpectedEof` |
| Failed sync | Return EIO before the selected real `fsync` call |

COMMIT failures target the WAL; checkpoint failures target the main database.
Write failures leave the WAL header intact and select the first frame write.
The partial-write case also passes the small frame header before writing half
of the page payload.
The partial-write case models the native backend's positive write progress
followed by a zero-progress syscall. `UnixIO` retries positive short writes and
returns `UnexpectedEof` on zero progress; a backend that falsely reports a full
write after writing only a prefix is outside this test. This does not inject
kernel syscalls into `UnixIO` itself.

Each fixture includes a collection, a unique scalar index, FTS, a two-dimensional
L2 ANN index, a spatial index, and a relational events table. One transaction
changes the document's scalar, text, vector and point values plus the relational
event marker. FTS queries require exactly the current token and no previous
token; ANN nearest-neighbor and spatial queries must match the same transaction
state. Collection integrity and native `integrity_check` must pass.

The fault is armed after staging the transaction for COMMIT, or after a successful
COMMIT for checkpoint. Each child must reach its selected fault, return the
expected public failure and exit with status 74. The parent imposes a 45-second
child deadline. The child exits without destructors, preventing close/drop from
retrying the failed operation before the parent inspects the persisted files.
The recovery boundary is a new owning process/connection after a storage error;
this does not qualify continuing arbitrary work on the damaged connection.

## Failure reporting and commit ambiguity

A failed COMMIT is not proof of rollback. In this matrix, ENOSPC, EIO and the
partial-write error recover the complete old transaction. A failed WAL sync
recovers the complete new transaction: its commit frame reached the file before
sync reported failure. An application must resolve the outcome using an
application transaction/idempotency marker before retrying non-idempotent work.
These outcomes concern process restart with the OS still running, not durability
after machine power loss.

In this synchronous-wrapper matrix, `op_checkpoint` receives the pager error and
returns a PRAGMA row with `busy=1`, `log=NULL`, `checkpointed=NULL`. Those selected
failures do not throw through the query API and their original errno is not
exposed, a diagnostic limitation of that path. The test requires that exact
failed status; it never accepts query completion as checkpoint success.
Applications must check the first result column is zero. Later successful
TRUNCATE checkpointing must return `[0,0,0]` in this fixture.

This is not a universal checkpoint-error contract. An asynchronous completion
error can propagate outside `op_checkpoint` through the query API. The separately
approved [checkpoint barrier/retry exception](proposals/checkpoint-wal-sync.md)
covers failed WAL sync before database backfill, including immediate and deferred
failed completions and subsequent retries. Applications must handle both thrown
errors and unsuccessful checkpoint rows. The synchronous matrix above remains
evidence for its stated injection points; it does not qualify every asynchronous
completion path or the integrated correction's final release artifacts.

## Acceptance and scope

`cargo test --locked -p romedb --lib recovery_io:: -- --nocapture` passes:
four tests, including the eight returned-failure children and the four existing
abrupt-exit children, in 7.31 seconds. All eight cases satisfy the document/index,
relational-event, integrity, rollback, subsequent-write, successful-checkpoint
and second-reopen assertions. The raw log is
`/tmp/romedb-v21-returned-io.log`.

| Failure | Failed COMMIT recovery | Failed checkpoint recovery |
|---|---|---|
| ENOSPC | Complete old transaction | Complete acknowledged transaction |
| EIO | Complete old transaction | Complete acknowledged transaction |
| Partial page write + error | Complete old transaction | Complete acknowledged transaction |
| Sync EIO | Complete new, unacknowledged transaction | Complete acknowledged transaction |

`cargo fmt -p romedb -- --check` and
`cargo clippy --locked -p romedb --all-targets --no-deps -- -D warnings` pass.
Clippy completed in 25.56 seconds; log:
`/tmp/romedb-v21-native-io-clippy.log`. These are scoped local checks; combined
release checks and exact-artifact qualification remain separate gates.

This matrix covers returned errors at one selected write/sync point per phase,
including indexed data maintenance. It does not establish every possible page
boundary, actual disk exhaustion, arbitrary torn sectors, controller cache
behavior, power loss, index-build DDL failures, every filesystem, or multiprocess
writers. Existing process-exit, transaction/index rollback and V1/V2 upgrade and
restore tests supply separate complementary evidence.
