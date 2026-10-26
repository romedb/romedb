# Turso 0.8.1 FTS savepoint cursor pins

Status: user approved the focused core fix on 2026-09-30.
Approved patch SHA-256: `8de9b128f9d6bbb85d1cd9a12b1f3eedc15092ff667b969afa9fab3f431dec7c`.
Isolated integration commit: `8b1e54e2796e71b39f8061f178f685bfdd25c3d2`.
Upstream base: `8549c16595d2faf1bdd6ee24aee0be8bfabb3d4a`.

## Gap and failure

The upgraded engine retains index-method cursors through transaction completion.
Its new backing-store cursor does not register with the pager's cursor registry.
A large FTS build followed by a query and ROLLBACK TO can therefore retain page
pins on pages allocated after the savepoint. Pager truncation fails with
`page 102 is pinned`. Failed index builds can also abort caller transactions
instead of preserving prior work. RomeDB cannot release this private engine
cursor through the public frontend API.

Existing RomeDB tests reproduced both failures without this candidate:
`text_bulk_build_batches_rows_and_preserves_savepoint_rollback_and_reopen` and
`failed_text_builds_leave_no_orphans_and_query_inputs_are_explicit`.
Moving pager invalidation earlier alone did not fix either failure.

## Candidate

[Exact core patch](turso-081-fts-savepoint.patch):

1. Allocate the backing B-tree cursor in its final stable Box before registering
   it with the pager. Move only the Box, including when wrapped in an MVCC
   cursor. The existing BTreeCursor Drop unregisters its pointer.
2. Move savepoint cursor invalidation before cache truncation. This releases the
   retained page-stack pins before the pager discards newly allocated pages.

No public API, file format, SQL syntax or transaction policy changes. This
extends cursor lifecycle handling across the new upstream FTS implementation;
it is a new core exception, separate from the retired FTS cache ownership patch.
The registry uses raw pointers; stable allocation and Drop unregistration are
therefore part of the required safety invariant, not optional cleanup.

## Evidence

Control logs: `/tmp/romedb-upgrade-tests3.log` and
`/tmp/romedb-upgrade-tests4.log` (six FTS passes, the two failures above).
Candidate log: `/tmp/romedb-upgrade-tests5.log`:

- Existing fulltext integration suite: 8 passed.
- Turso 0.8.1 public integration suite: 6 passed, including four simultaneous
  writers, snapshot isolation, write-conflict retry and a database created by
  the published 2.1.0 binary.

Current-source validation also passed after removal of the redundant final
invalidation: 131 native index-method tests, including the new raw-engine
bulk-build savepoint regression; all eight RomeDB FTS tests and six public
upgrade tests; 36 native integrity tests; 46 statement lifecycle tests; and
12 native savepoint integration tests. The upgrade tests include rejection of
legacy-index writes without losing caller work before explicit REINDEX.

Removal condition: upstream backing cursors participate in pager invalidation,
and savepoint rollback releases pins before truncation, with these regression
and isolation tests passing without the local patch.
