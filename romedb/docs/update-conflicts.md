# Choosing an UPDATE conflict policy

Collection UPDATE supports the five policies below in the current prototype,
including supported UPDATE FROM forms. Plain UPDATE uses ABORT behavior. These
policies apply to validation and constraint failures during document mutation;
preparation errors, resource limits and engine failures have separate recovery
rules. See the [write contract](contracts.md#limited-writes).

| Policy | On a conflicting candidate | Earlier successful updates | Statement result |
| --- | --- | --- | --- |
| ABORT | Restore the statement | Undone | Error |
| ROLLBACK | Roll back the whole transaction | Undone, together with prior pending work | Error |
| FAIL | Restore the candidate and stop | Retained | Error; no partial RETURNING rows |
| IGNORE | Restore the candidate and continue | Retained | Successful candidates only |
| REPLACE | Delete other documents with conflicting unique keys, then update | Retained unless a later replacement deletes them | Completed updates |

REPLACE does not resolve invalid field values by deleting other documents. A
validation failure restores the statement, including earlier conflict deletions.
All policies maintain managed indexes with their documents. Ordinary relational
tables retain the pinned engine's behavior; collection fields do not have SQL
NOT NULL default substitution.

## A small comparison

Start with a fresh database for each policy:

```sql
CREATE TABLE items;
CREATE UNIQUE INDEX items_n ON items(n);
INSERT INTO items(n) VALUES(1),(2);
BEGIN;
INSERT INTO items(n) VALUES(3);
```

Run this statement, substituting one policy for ABORT:

```sql
UPDATE OR ABORT items SET n=10 WHERE n<3 RETURNING n;
```

After either success or error, inspect the connection before deciding to commit
or roll back:

```sql
SELECT n FROM items ORDER BY n;
```

The pinned local fixture produces these observations:

| Policy | Sorted values after the statement | Transaction state | Returned rows |
| --- | --- | --- | --- |
| ABORT | 1, 2, 3 | Active | Error |
| ROLLBACK | 1, 2 | Autocommit | Error |
| FAIL | 2, 3, 10 | Active | Error |
| IGNORE | 2, 3, 10 | Active | One row: 10 |
| REPLACE | 3, 10 | Active | Two rows: 10, 10 |

The retained row in an unordered update is not a general ordering guarantee.
REPLACE returns both completed updates in this example even though its second
update deletes the first updated document. Its affected count is two; it does not
count the implicit conflict deletion as an additional update.

The CLI's script mode stops on the first error. Use an interactive session or
separate client calls to perform the inspection after a failed statement. These
examples leave active transactions pending; explicitly COMMIT or ROLLBACK them.

## Handling errors in applications

Use Rust `execute_report` or the Node result/error `transaction` property to
observe before/after transaction state. An active state says the transaction
still exists; it does not say every prior statement was unchanged. FAIL can
retain updates while reporting an error. Without an enclosing transaction, that
successful prefix commits before the error is returned. Retrying blindly can
therefore apply work twice.

Wrap related operations in an explicit transaction when the application needs
all-or-nothing behavior. After a failure, roll back if that transaction is still
active. User savepoints can also undo retained FAIL/IGNORE updates while keeping
earlier work. Engine errors and cancellation can independently end a transaction,
so inspect the report before attempting recovery.

Rust `write_with_result_limits` and Node `writeWithResultLimits` provide an
explicitly atomic write wrapper: any error restores that statement, including
an OR FAIL error. This does not undo earlier statements in an outer transaction.
A result limit failure under REPLACE restores its implicit conflict deletions.

LIMIT selects candidates before conflict processing. IGNORE does not fill a page
with extra candidates to replace skipped rows. REPLACE skips a candidate deleted
by an earlier replacement. Result budgets count completed RETURNING rows, which
can outnumber the final surviving documents.

This guide covers collection UPDATE. See the [INSERT guide](insert-conflicts.md)
for SQL insertion policies and their difference from object UPSERT. Broader expression, interruption,
resource and release qualification remains in progress; see [status](status.md).
