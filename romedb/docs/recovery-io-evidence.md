# Deterministic commit/checkpoint process-failure evidence

S3's selected interrupted-I/O requirement is covered by the test-only
frontend recovery_io module. Production APIs and upstream core are unchanged.

Four child processes exit immediately before a selected syscall-backend operation:
WAL write and WAL sync during COMMIT; database-file write and sync during
checkpoint. The wrapper is armed only after staging the transaction (or after
successful COMMIT for checkpoint cases). Exit status 73 proves the selected hook
was reached; otherwise the test fails. Each child has a 30-second timeout.

The fixture has a document collection, a unique index, and a relational events
table. After each failure, two reopens require either the complete old or complete
new transaction, agreement between document state and relational event count,
collection/index integrity, and native integrity_check success. Checkpoint cases
must retain the committed new state.

The test uses synchronous UnixIO through a wrapper with shared-WAL coordination
disabled. This is process failure before selected write/sync calls, not power loss,
torn writes, arbitrary I/O faults, every commit boundary, or multi-process writer
coordination. Existing process-kill loops supply complementary phase coverage.

Focused test passed. Full scoped check passed: 676 Rust tests, one known ignored
trigger regression, 103 Node/application tests, formatting, Clippy and TypeScript.
Logs: /tmp/romedb-recovery-io.log and /tmp/romedb-recovery-scoped.log.
The trigger Interrupt-to-Busy exception remains separately pending review.

## Production returned-error matrix

The later [native returned-I/O-error qualification](native-io-errors-evidence.md)
adds ENOSPC, EIO, partial-page writes and failed syncs at COMMIT/checkpoint,
including FTS/ANN/spatial index consistency and subsequent writes. It also records
failed-COMMIT ambiguity and the pinned checkpoint failure-status contract.
