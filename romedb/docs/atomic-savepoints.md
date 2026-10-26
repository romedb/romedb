# Atomic savepoint identity and cancellation

RomeDB atomic operations now allocate a distinct reserved savepoint name per connection operation, using a checked monotonic counter. This applies to nested operations such as importing multiple documents. Exhausting the counter returns FDB_LIMIT before opening a savepoint.

Previously every frame was named __romedb_statement. If interruption was reported after an inner SAVEPOINT had opened but before its callback began, the inner atomic call returned early. Outer cleanup then rolled back to the newly opened inner frame with the same name, leaving earlier writes in place while returning FDB_CANCELLED.

A deterministic regression sweeps 48 engine progress boundaries after the first write in nested atomic operations. Before the fix, boundary 4 returned FDB_CANCELLED while retaining the first write. Unique names make outer cleanup target its own frame, including any unfinished nested frames. The regression verifies that FDB_CANCELLED preserves prior rows and the active outer transaction, successful operations contain both writes, and rollback errors never leave only a partial write set in the covered cases.

The failure was discovered by the existing Node cancelled-import test: two imported rows survived cancellation. A timing-based rerun passed, so the deterministic regression is the primary evidence for the cause and fix. Targeted before/after logs are /tmp/romedb-nested-atomic-cancel.log and /tmp/romedb-nested-atomic-cancel-fixed.log.

This does not claim complete cancellation or commit-outcome qualification. A cancellation reported around RELEASE may produce FDB_ROLLBACK and a completed or restored write set; callers must inspect transaction/error reports. Opening I/O failures, interrupted I/O/commit behavior and the separate pinned trigger Interrupt-to-Busy defect remain qualification work. No upstream engine files or persisted data encodings changed.

Verification of the combined working tree passed romedb/scripts/check.sh: 308 Rust tests, 35 Node tests, formatting, Clippy and strict TypeScript (/tmp/romedb-unique-savepoints-check.log). The Node transfer cancellation test then passed three additional isolated runs (/tmp/romedb-transfer-cancel-repeated.log). The known trigger-interruption gate remains ignored.


## Cancelled savepoint opening

Opening errors now attempt rollback/release of the uniquely named frame before returning. When no transaction is active, no cleanup is needed. In an active transaction, only the pinned engine's exact TxError for the missing generated savepoint is accepted as an already-absent frame; other cleanup failures return FDB_ROLLBACK. This exception is confined to opening failure before the operation callback runs.

A deterministic opening sweep reproduced an autocommit connection becoming active at boundary 4 without executing the callback. The regression now checks both initial transaction states, requires cancellation coverage through that boundary, verifies no generated savepoint remains, preserves prior rows, and executes a successful retry. The existing deliberately persistent-interrupt export test reports FDB_ROLLBACK for an active transaction when its handler also prevents cleanup; public CancellationToken delivery remains one-shot. RELEASE/commit ambiguity and interrupted I/O qualification remain open.

Opening cleanup verification passed the full scoped suite: 309 Rust tests, 35 Node tests, formatting, Clippy and strict TypeScript (/tmp/romedb-atomic-open-check.log).


## RELEASE progress boundaries

A 32-case sweep requests interruption at 16 thresholds after both writes, in autocommit and an active outer transaction. It separately observes whether the interrupt callback actually fired. At the first three thresholds in each mode, FDB_CANCELLED restores the original rowset. At the fourth threshold with an outer transaction, FDB_ROLLBACK leaves the complete two-write set pending; explicitly rolling back the outer transaction removes that set and its prior work. Later thresholds complete before interrupt delivery. Every case preserves the initial transaction mode and has either the complete or restored write set, never a partial set.

This qualifies those pinned in-memory VM boundaries, not power-loss, interrupted I/O, all RELEASE implementations or a general commit-outcome oracle. FDB_ROLLBACK must not be interpreted as confirmed rollback or automatically retried. The transaction observation alone does not establish whether an operation's writes remain pending; inspect/reconcile the result or roll back the outer transaction as appropriate.
