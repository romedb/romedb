# Choosing an INSERT conflict policy

Collection SQL INSERT supports ABORT, ROLLBACK, FAIL, IGNORE and REPLACE for the
implemented VALUES and SELECT sources. Plain INSERT uses ABORT. These policies
handle candidate insertion validation and constraint failures; they do not make
preparation, cancellation or resource errors into ignored rows. See the
[write contract](contracts.md#limited-writes) and [UPDATE guide](update-conflicts.md).

| Policy | Candidate conflict | Earlier successful inserts | Result |
| --- | --- | --- | --- |
| ABORT | Restore the statement | Undone | Error |
| ROLLBACK | Roll back the transaction | Undone, including prior pending work | Error |
| FAIL | Restore the candidate and stop | Retained | Error; no partial RETURNING rows |
| IGNORE | Restore the candidate and continue | Retained | Successfully inserted rows |
| REPLACE | Remove conflicting documents and insert the new one | Retained unless replaced later | Completed insertions |

## Record identity conflicts

Use a fresh database for each policy:

```sql
CREATE TABLE docs;
CREATE UNIQUE INDEX docs_n ON docs(n);
INSERT INTO docs(id,n,extra) VALUES(docs:a,1,'old');
BEGIN;
INSERT INTO docs(id,n) VALUES(docs:pending,9);
```

Run this statement, substituting the policy being tested for ABORT:

```sql
INSERT OR ABORT INTO docs(id,n)
VALUES(docs:b,2),(docs:a,3),(docs:c,4)
RETURNING n;
```

Inspect the result after success or error with a separate client call:

```sql
SELECT n FROM docs ORDER BY n;
```

The verified fixture gives:

| Policy | Sorted stored values | Transaction state | RETURNING |
| --- | --- | --- | --- |
| ABORT | 1, 9 | Active | Error |
| ROLLBACK | 1 | Autocommit | Error |
| FAIL | 1, 2, 9 | Active | Error |
| IGNORE | 1, 2, 4, 9 | Active | 2, 4 |
| REPLACE | 2, 3, 4, 9 | Active | 2, 3, 4 |

Explicitly COMMIT or ROLLBACK any transaction left active. The CLI's script mode
stops at the first error, so use interactive input or separate client calls to
inspect a failed statement. Use ORDER BY when a SELECT source needs a particular
candidate order; unordered SELECT sources do not guarantee a winner.

## Replacement is a new document

After REPLACE in this example, `docs:a` contains the supplied `id` and `n`; its old
`extra` field is absent. The existing object `UPSERT docs:a {n:3}` instead applies
a shallow patch to that identity and preserves unspecified fields.

INSERT REPLACE can remove more than one document: the same record ID and other
documents matching different unique index keys can all conflict with one new
candidate. Their managed index entries are deleted with them. Implicit deletions
are not additional insertions in the affected count. A subsequent candidate can
replace a row already reported in RETURNING.

If id is omitted, RomeDB generates a new record ID. Replacing a unique-key conflict
then removes the previous identity. Existing references to that identity are not
rewritten automatically. Supply the intended ID when identity must be preserved.
A record's integer key and string key remain distinct.

REPLACE still validates the new document. Missing required fields and failed
CHECKs restore the statement, including earlier conflict deletions. Collection
fields do not provide SQL NOT NULL default substitution. Null unique-index keys
do not conflict with one another.

## Recovery and limits

Inspect Rust `execute_report` or the Node result/error `transaction` property.
An active transaction can contain successful inserts from an errored FAIL
statement. In autocommit, those inserts have committed before the error reaches
the caller. Blind retries can duplicate work; use an explicit transaction and
roll it back if the application requires all-or-nothing behavior.

Rust `write_with_result_limits` and Node `writeWithResultLimits` wrap the write
atomically: any error, including FAIL, restores that statement while preserving
earlier outer-transaction work when the engine keeps that transaction active.
Other engine errors can independently abort the transaction.

Candidate buffers count input rows even if IGNORE would skip them. Result limits
apply to completed RETURNING rows, not final surviving document count. These are
separate bounds, not a total-memory guarantee.

These SQL policy forms do not add an ON CONFLICT clause, change the typed Rust
`insert` method into replacement, or alter object UPSERT semantics. V1 release
qualification remains in progress; see [status](status.md).
