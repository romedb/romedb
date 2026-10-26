# Preview milestone: application and storage readiness

Requirements **P1 and P2 are complete** for the Linux x64 preview baseline.
Source: `8bce5bf7a` plus the tracker export/restore implementation committed with
this report. No engine, catalog format or dependency changes were made.

## Application acceptance (P1)

The tracker now exports people and tasks as typed NDJSON and relational events
as JSON rows in one transaction. Restore runs after migrations on an empty
tracker and imports all three datasets atomically. The four application tests
pass, including the new fresh-file restore workflow.

The new test compares exact documents and event rows after reopen, expanded
owner links and task state, and collection/index integrity. A duplicate event
forces a failure after both collection imports: all three tables remain empty,
indexes remain consistent, and a corrected retry succeeds. Nonempty targets are
rejected. Existing tests cover validation, task/event transaction rollback and
migration lifecycle. See the [runnable walkthrough](../examples/node-task-tracker/README.md#export-and-restore-the-application).

## Storage acceptance (P2)

The existing `persistence`, `migrations`, `transfer`, and `crash_stress` suites
passed as part of the complete scoped check. Coverage includes committed reopen,
failed uniqueness/index changes and mixed rollback; checkpointed offline backup
restoring schema, values, indexes and migration history; transfer validation and
atomic imports; and process kills around joined writes, rewrites, commit and
checkpoint loops at the existing three delays. No new failure matrix was added.

These satisfy the finite preview requirement. They do not claim power-loss or
all-instruction-boundary recovery, nor compatibility with an untested released
binary. Those remain the explicit preview limitations.

## Milestone check

`romedb/scripts/check.sh` exited successfully with Rust 1.88.0 on Linux x64:

- 673 Rust tests passed; zero failed; one existing trigger-cancellation test ignored.
- 101 Node/application tests passed against the rebuilt addon.
- Five-package formatting and Clippy passed; strict TypeScript passed.

Focused development run: `/tmp/romedb-preview-app.log` (four application tests).
Full log: `/tmp/romedb-preview-storage-milestone.log`.
Full log SHA-256: `8b6151487b8a13ad16ecbb532747ebdf14e29e32b8bf8d663e3945d16d511911`.

The ignored trigger Interrupt-to-Busy case remains excluded from advertised
preview cancellation support. It does not affect the tracker workflow, which
uses ordinary explicit transactions and no triggers. No package/platform checks
were run for this application/storage milestone.

Next: candidate delivery, P3 + P4. Reopen P1/P2 only for a concrete regression or
relevant source/dependency change, per the preview checklist.
