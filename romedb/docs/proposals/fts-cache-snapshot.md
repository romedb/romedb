# Approved core exception: isolate FTS directory caches by pager

Current status (2026-09-30): **retired in the Turso 0.8.1 sync**. Upstream's
snapshot-aware FTS implementation replaces this old directory-cache patch.
Retain `test_fts_cache_preserves_connection_snapshots` and the public concurrent
FTS tests, but do not reapply the historical patch below.

## Historical approval and reproduction (0.7.2)

Status: approved by the user and integrated into the active checkout, 2026-09-25.
Required for V2-F. This is a transaction correctness fix, not RomeQL syntax in core.

## Reproduced failure

The RomeDB full-text integration test creates two committed matching documents,
starts a writer transaction, inserts a third match, and queries from that writer.
A second connection then sees three FTS hits even though the source-table snapshot
contains only the two committed matches. The extra hit has a NULL ID and the
committed records' scores also change. The expected two committed rows and scores
are asserted in `romedb/tests/tests/fulltext.rs`.

The [standalone native reproduction](fts-cache-snapshot-repro.rs) uses only
`turso_core` with feature `fts`, `DatabaseOpts::with_index_method(true)`, and SQL.
It additionally checks a reader snapshot retained across writer commit, a fresh
read after commit, delete/update visibility, savepoint rollback and full rollback.

## Cause and patch

`FtsIndexAttachment` shares one directory cache across cursors/connections.
`HybridBTreeDirectory` retains its originating `Arc<Pager>`. The existing
`is_consistent_with_btree()` check reads through that retained pager; it can
validate uncommitted writer metadata when the requesting connection is a reader.
`clone_with_fresh_pending()` then retains the same wrong pager and cache contents.

The [approved patch](fts-cache-snapshot.patch) adds a pager-identity check before
the existing cache validation/reuse. A cache from another pager is treated as
stale and reloaded through the requesting connection's existing cursor. Same-pager
reuse still performs the existing rollback consistency check. No SQL, public API,
on-disk format or Tantivy dependency change is needed for this patch.

Cost: alternating connections reload the directory. A per-pager cache could avoid
that cost later, but is not needed to restore snapshot correctness.

## Review and integration

`RomeDB-Workflow.md` says: “Any necessary local core exception requires a
separately reviewed design decision, isolated commit, patch inventory entry, and
relevant regression tests; it is not automatically authorized by this workflow.”

The standalone reproduction **fails against the unmodified engine** with an
uncommitted NULL-ID hit and changed scores, and **passes with the proposed patch**
in an extracted temporary checkout (Rust 1.88.0). The test includes reader snapshot
retention across writer commit, fresh reads after commit and both rollback forms.
Logs: `/tmp/romedb-v2-fts-control.log` and
`/tmp/romedb-v2-fts-patch-review.log`. All three managed full-text integration tests also pass against the patched
temporary checkout: stable 40-way score ties, native index plans, failed-build
cleanup, invalid writes, transaction/savepoint/drop rollback, concurrent readers,
reopen, deletion, rebuild and table drop. Log:
`/tmp/romedb-v2-fts-review-integration.log`. Five filtered frontend unit checks also passed, including metadata/schema
rejection, stale-field/count auditing and cancellation of build/write with prior
transaction work preserved (`/tmp/romedb-v2-fts-review-unit.log`). This is focused
evidence, not a full scoped acceptance run.

At review time, the active checkout passed the new parser regression, package-scoped formatting,
Clippy with warnings denied for all five RomeDB packages/all targets, and Node
test-file syntax checking. Node behavior and full runtime acceptance were deferred until the approved core
integration. Current combined acceptance is recorded in
the V2-F evidence.

The user approved integration on 2026-09-25. The active core now includes the
reviewed fix and the native index-method suite includes the snapshot regression.
Active-checkout validation: `cargo test --locked -p core_tester --test
integration_tests index_method::test_fts -- --nocapture` passed **26 tests**, zero
failures/ignored, including the new snapshot regression. The command filtered out
959 unrelated tests. Log: `/tmp/romedb-v2-fts-upstream.log`. An existing unused
import warning in upstream sync code was not changed.

The isolated commit contains the core fix, native regression, this review record
and the provenance entry. It is commit `fb246a8e4`. Managed/client acceptance
has now passed.
