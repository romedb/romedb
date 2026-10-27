# Proposed core exception: preserve transactions on scalar read errors

Current status (2026-09-30): **approved, retained and adapted to Turso 0.8.1**.
Read-only scalar extension failures still require the local cleanup exception to
preserve caller transactions and savepoints. The earlier pending/release-gate
wording below is historical; see the
[current exception audit](../core-exception-audit-2026-09-30.md).

## Historical approval and reproduction (0.7.2)

Current status: explicitly approved by the user and integrated in isolated commit
`f26014f04de4077a49268fd94c37ff9ad6ad6425`. The original review and candidate-only evidence below are retained
as history. Subsequent pending/unintegrated wording describes that earlier state.
Track upstream replacement/removal in [the maintenance register](../core-exceptions.md).
Combined-source acceptance now passes; see [integrated evidence](../v2-core-integration-evidence.md).
Final release qualification remains open.


Status: pending separate review. This scalar-error patch is not integrated. The
candidate patch and regression were qualified in `/tmp/romedb-udf-review`; later approvals
for the WAL and FTS backing-storage fixes do not authorize this proposal.

## Reproduced failure

A native scalar callback returns an extension error during a SELECT inside a
caller-owned savepoint. The pinned engine rolls back the entire transaction,
including earlier work, and subsequent savepoint use can leave autocommit state
inconsistent. The new JavaScript runtime exposes this when a function throws or
hits its resource limit; it is not caused by JavaScript or RomeQL lowering.

The [standalone reproduction](udf-error-transaction-repro.rs) uses only the native
engine and extension API. Against the active engine it fails at
`SELECT fail_three(3)` with `scalar error aborted caller transaction`. Control
log: `/tmp/romedb-udf-native-control.log`. The first managed JavaScript test run
also reproduced the later `cannot start a transaction within a transaction`
failure. This proposed core change has not been applied to the active checkout.

## Narrow proposed change

The [patch](udf-error-transaction.patch) adds one read-only extension-error arm to
`Program::abort` in `core/vdbe/mod.rs`. An extension error from a statement that
was not an active writer preserves the caller's explicit transaction and named
savepoints. Autocommit readers still release their transaction. Writer error
handling, statement journals, interrupts, constraints and error values retain
their existing paths. The patch does not clear changes() for a failed reader.
There is no new API, language syntax, on-disk format or dependency in core.

The same patch extends `tests/integration/external_apis.rs` with a native callback
regression. It covers WAL and MVCC, autocommit, nested savepoints, prior explicit
transaction work, reader failures, writer controls, partial-write rejection,
changes(), commit, and reuse of the connection/savepoint stack afterward.
SQLite 3.45.1 was also checked locally: a failed scalar SELECT preserves prior
transaction work and both nested savepoints in the matching three modes.

## Review requirement

[RomeDB-Workflow.md](../../../../RomeDB-Workflow.md) says:

> Any necessary local core exception requires a separately reviewed design
> decision, isolated commit, patch inventory entry, and relevant regression tests;
> it is not automatically authorized by this workflow.

The FTS cache, WAL sync and FTS backing-storage approvals are separate. This
exception still needs review before integrating the core change. On approval, apply this exact patch,
record its provenance, isolate the core fix and native regression in a commit,
then complete the combined RomeDB acceptance checks. V2-J and V2 release remain
open until those checks pass.

## Candidate verification

Final checks passed in the temporary checkout with this exact core patch:

- **12 native external-API tests**, including the new 24-case WAL/MVCC regression.
- **18 standalone native scenarios** across autocommit, nested savepoints and
  explicit transactions, with both read errors and unchanged writer controls.
- **4 JavaScript integration tests** covering persisted typed lifecycle,
  snapshot replacement, resource/sandbox limits, atomic writes and cancellation;
  plus the existing deferred-feature regression.
- **1 runtime unit regression** for definition checksums and cancellation polled
  directly inside QuickJS, and **1 parser regression**.
- **2 Node tests**, exercising both synchronous and asynchronous clients,
  persistence, typed values, write rollback and worker cancellation.
- Scoped Clippy for all five RomeDB packages/all targets with warnings denied,
  package formatting and Node test syntax checking.

Logs: `/tmp/romedb-udf-review-native-suite-final.log`,
`/tmp/romedb-udf-review-native-probe-final.log`,
`/tmp/romedb-udf-review-integration-final.log`,
`/tmp/romedb-udf-review-unit-final.log`,
`/tmp/romedb-udf-review-parser-final.log`,
`/tmp/romedb-udf-review-node-final.log`, and
`/tmp/romedb-udf-review-clippy-final.log`. The native suite emits one pre-existing
unused-import warning in upstream sync code; that source is unchanged.

The source audit found QuickJS's additional `performance` clock. The final runtime
uses an explicit intrinsic allowlist, excluding clock and weak-reference
intrinsics, and the sandbox regression verifies that `performance` is absent.
Node was rebuilt and its focused checks repeated after this change.

Patch SHA-256:
`953e88426f9c5e5ea52a3f41251c7e64bc0b2bb06c70557f58ff596454c68b0a`.
`git apply --check` passes against the active checkout. No active core/integration
files are changed by this proposal. The temporary Node addon is a debug test
artifact. Subsequent active-addon builds and their exact scope are recorded in
[upgrade evidence](../v2-upgrade-restore-evidence.md). This is focused review
evidence, not full scoped acceptance or a V2 release.

## Integration readiness recheck, 2026-09-25

The unchanged patch still passes `git apply --check` on active HEAD
`12109384ad868dd673a572305d45a816baa2e545`, after the approved WAL and FTS
backing-storage fixes. This checks applicability only; the candidate tests above
were not rerun on that base. The old temporary directory remains but is no longer
a Git checkout, so reconstruct an isolated candidate before relying on its source
state. Integration and combined acceptance still require the separate approval.
