# Turso 0.8.3 integration

Engine base: `v0.8.3`, commit `607bff9fd0e547a25a97b3bf06e83bcc5d99d53c` (tagged
2026-10-12). Previous base: `v0.8.1`, commit `8549c16595d2faf1bdd6ee24aee0be8bfabb3d4a`.
Merged into branch `sync/turso-0.8.3` with an ancestry-preserving merge commit.
Exception
decisions are in [the register](core-exceptions.md).

## What changed upstream

0.8.2 and 0.8.3 together are 21 commits. They change no storage format: WAL,
B-tree pages and the MVCC logical-log encoding (`LOG_VERSION`) are the same.
RomeDB catalog version 7 is unchanged and needs no migration or REINDEX.

Changes that reach RomeDB:

- **MVCC durability.** The logical-log header changes in memory only after its
  write succeeds. Garbage collection keeps the final state of transactions still
  in use. Passive TRUNCATE keeps commits that arrive late. B-tree delete and
  conflict checking are one step. Cloud opens databases in MVCC mode, so these
  fixes matter most there.
- **Abandoned MVCC commits.** When a commit is dropped before it finishes, the
  connection now releases its read lock and transaction state
  (`Connection::end_abandoned_mvcc_commit`). An inner `nextval()` commit does not
  end the outer transaction.
- **AUTOINCREMENT.** An explicit row ID equal to the next ID now moves the
  sequence forward. Before, inserting ID 1 into a new table let the next
  generated row reuse ID 1 and replace the row.
- **Journal mode.** `PRAGMA journal_mode` refuses to switch into or out of MVCC
  inside a transaction, as SQLite does. Before, the switch could leave a database
  that would not open again.
- **CDC.** Virtual generated columns are no longer written into CDC update
  records. RomeDB does not enable CDC.

Upstream also changed the sync engine, .NET pooling, package versions and fuzz
tests. RomeDB does not ship these.

## Effect on RomeDB

No RomeDB source change was needed. The merge had no conflicts. All eight
maintained engine exceptions are kept unchanged, because 0.8.3 replaces none of
them.

- `open_with_manual_mvcc_and_io` sets MVCC on a fresh connection outside any
  transaction, so the new journal-mode rule does not affect it.
- An explicit-ID AUTOINCREMENT insert still counts as one customer row mutation
  in both WAL and MVCC modes. The extra opcodes upstream added to that path do
  not count as row mutations.
- `turso_083.rs` adds four frontend regressions: explicit ID 1 then a generated
  ID in WAL and MVCC modes with reopen, the metered mutation count above, and
  the rejected in-transaction MVCC switch followed by a clean reopen.

## Compatibility

Released 2.9.0 databases open with the new engine, and the 2.9.0 package still
opens files the new engine wrote. Backups stay unchanged and indexed results
match scans. Every earlier release in the upgrade table also passes. Details are
in the checklist.

## Not included

Upstream `v0.9.0-pre.1` and later `main` contain the whole-table DELETE fast path
(`58f350a90`, `ClearBtreeCount`). It skips the Delete opcode that the execution
meter counts. A later sync must count that path or disable it for metered
requests before taking it.
