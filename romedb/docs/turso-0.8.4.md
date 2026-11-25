# Turso 0.8.4 integration

Engine base: `v0.8.4`, commit `72eafab83fee6ad5876657c5861b1fc9e5a39ec1` (tagged
2026-11-20). Previous base: `v0.8.3`, commit `607bff9fd0e547a25a97b3bf06e83bcc5d99d53c`.
Merged into branch `sync/turso-0.8.4` with an ancestry-preserving merge commit.
Exception decisions are in [the register](core-exceptions.md).

## What changed upstream

0.8.4 is a patch release cut from the 0.8.3 line: 12 commits, of which six are
fixes. It is not upstream `main`, so it does not contain the 0.9.0 work. It
changes no storage format: WAL, B-tree pages and the MVCC logical-log encoding are
the same. RomeDB catalog version 11 is unchanged and needs no migration or
REINDEX.

Changes that reach RomeDB:

- **DELETE with foreign key actions.** When `PRAGMA foreign_keys` is on and a
  foreign key that points at the table has an `ON DELETE` action other than
  `NO ACTION` or `RESTRICT`, the DELETE first collects the row IDs to delete
  (`RowSetAdd`), then deletes them. Before, it did this only when a trigger on a
  child table could write back to the table, and a cascade could change the rows
  under a live scan.
- **UPDATE that changes row IDs.** An UPDATE that sets the row ID or an
  `INTEGER PRIMARY KEY` now opens a statement savepoint. Before, a conflict on a
  later row (`UPDATE t SET k = k + 10` where `k + 10` already exists) left the
  rows already moved in the open transaction, in WAL and MVCC mode.
- **Stale statements.** A statement prepared again after a connection setting
  changed (for example `PRAGMA foreign_keys`) no longer ends the autocommit
  transaction of another statement on the same connection that is still
  returning rows. Before, an `INSERT ... RETURNING` part-way through its rows
  could lose its writes, and a running SELECT could lose its snapshot.
- **UPSERT and CDC.** `ON CONFLICT DO UPDATE` reads the CDC before image before
  it writes the row. RomeDB does not enable CDC.
- **rustls** 0.23.45 and **aws-lc-rs** 1.18.2. RomeDB's lockfile already had
  rustls 0.23.46 and rustls-webpki 0.103.16, so it keeps those and takes
  aws-lc-rs 1.18.2 and aws-lc-sys 0.46.0.

Upstream also changed the JavaScript bindings (parameter binding under the
execution lock), package versions and the macOS release runners. RomeDB does
not ship these.

## Effect on RomeDB

No RomeDB source change was needed. The only merge conflict was `Cargo.lock`;
each conflicting package took the newer of the two versions. Upstream and RomeDB
both changed `core/translate/upsert.rs`, in different places, and the merge
kept both. All engine exceptions are kept unchanged, because 0.8.4 replaces none
of them. The rowid seek backport stays: upstream `2c837d49a` is on `main`, not
in 0.8.4.

- The execution meter still counts every deleted parent and child row of a
  cascading DELETE once. A mutation budget that the cascade passes stops the
  statement and rolls back all of it.
- `turso_084.rs` adds two frontend regressions, each in WAL and MVCC mode: the
  cascading DELETE count and budget above, and a failed row-ID UPDATE inside an
  explicit transaction that undoes only its own rows and keeps the transaction
  open.

The refreshed RustSec review also found RUSTSEC-2026-0592 in `url` 2.5.4, which
the CLI ships. It is not part of 0.8.4; `url` moves to 2.5.9 in its own commit.

## Checks

Run on 2026-11-25 with Rust 1.89 under `scripts/romedb-heavy`, from a clean worktree.

- `romedb/scripts/check.sh`: passed. Format, clippy, 1,179 Rust tests (4 ignored)
  including both `turso_084` regressions, Node 235 passed of 236 (no failures),
  and the C client.
- `cargo test -p core_tester --test integration_tests -- --skip fuzz`: 1,190
  passed, 8 ignored. This includes the new upstream 0.8.4 tests
  (`test_stale_statement_*`, `update_rowid_alias_range_conflict_*`,
  `test_delete_collects_rowids_first_only_when_foreign_keys_are_on`) and the
  integration regressions of every engine exception (savepoint commit resume,
  VACUUM INTO, the four FTS patches).
- `cargo test -p turso_core --lib -- --test-threads 8`: 2,523 passed, 17 ignored,
  nothing skipped. The first run aborted in
  `attached_reader_does_not_pin_read_mark_until_checkpoint_gate_is_available`, which
  failed only when the `mvcc::` module ran in parallel; a build of `main` before the
  merge (`df6c8cb0f`) failed the same way 3 times out of 3, so 0.8.4 did not cause
  it. The cause was unit tests that emptied the whole database registry. This
  branch fixes it with the test-only backport of upstream `c7588f403` (see
  [the register](core-exceptions.md)); after it, the `mvcc::` module passed 3 of 3
  runs on 8 threads (519 tests). The MVCC exception regressions in this crate pass
  (`test_build_log_record_scans_the_write_set_for_schema_rows_once`, the
  `log_byte_*` tests, `test_exact_rowid_seek_over_shadowed_btree_rows`,
  `test_unique_check_over_shadowed_btree_index_entries`).
- RustSec review (advisory-db `10db6998b`, 2026-11-24): 361 shipping packages,
  no unresolved findings after the `url` update; chrono RUSTSEC-2026-0543 stays
  accepted. Notices regenerated with `prepare-v2-notices.py`.

## Not included

The whole-table DELETE fast path (`58f350a90`, `ClearBtreeCount`) is still only on
upstream `main`. The hazard recorded for 0.8.3 applies to the next sync that
takes 0.9.0.
