# Full-text construction batching: 2.1 qualification

Raw logs were deleted during the user-requested 2026-10-01 cleanup. The
qualification summary below is retained; Git history retains tracked log content.

The first optimized candidate from `a4df9cc27` did not complete the 5,000-document,
128-dimension application workload within its 20-minute per-sample bound.
That run had no stage markers and produced no passing workload receipt. A
180-second instrumented repeat seeded all documents in 4.155 seconds, then stayed
inside full-text index creation until the cutoff. See the
original timeout (raw log removed) and
instrumented log (raw log removed).

The frontend created its empty native FTS index, then submitted one indexed
INSERT statement per existing document. Each statement closed its FTS cursor
and committed a Tantivy segment. Initial construction now submits parameterized
batches of at most **128 rows**, retaining the existing empty-index creation
order. Each batch validates the same document fields and record IDs, updates
its count after successful insertion, and remains inside the existing atomic
savepoint. Catalog publication still follows the complete build. Ordinary
post-build writes and search semantics are unchanged.

At the supported maximum of 16 indexed fields, a batch binds 2,176 values, below
the pinned parser's 250,000 numbered-parameter limit. A compile-time assertion
requires the batch size to remain below the native FTS mid-insert flush threshold
(currently 1,000 documents). Review found a conditional replay issue in that
upstream path when flushing yields I/O; this change avoids entering it. No
corruption reproduction or engine correction is claimed, and no core exception
was added for batching. Revisit the bound only after any upstream replacement
has corresponding I/O-reentry evidence.

Focused checks pass:

- Eight fulltext integration tests (raw log removed),
  including 1,001 rows across batches, one/16 fields with a partial tail, missing
  and null fields, stable row mapping, a late invalid type after a completed
  batch, savepoint rollback/recreation, prior-work COMMIT and file reopen.
- Two fulltext unit tests (raw log removed).
- The existing interrupted build/write rollback test (raw log removed).

The same 5,000-document shape passed a bounded probe using a separately copied
**debug** addon: FTS construction took 6.191 seconds, with search correctness,
integrity, checkpoint and reopen checks passing. See the
[report](fulltext-build-evidence/discovery-after.json),
log (raw log removed) and
[source/addon identity](fulltext-build-evidence/discovery-identity.json).
This ran alongside source checks and is discovery evidence, not a production
latency claim or a fair cross-profile speed ratio. Full scoped checks and the
exact optimized artifact workloads remain separate release gates.

The workload harness now emits bounded phase/progress messages to stderr so
future failures identify the active stage. Individual operation timers exclude
logging; overall setup/workload/reopen timers include it. The workload mix,
correctness assertions and per-sample time bound are unchanged.
