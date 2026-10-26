# RomeQL record sources

User scope: 2026-10-11. Add `SELECT title FROM docs:a` through the existing
collection SELECT path, preserving typed values and SQL behavior.

- [x] Add regression coverage and confirm the unsupported source fails.
- [x] Lower fixed record sources to an indexed, zero-or-one collection source.
- [x] Verify aliases, joins, filters, nested queries, typed keys and missing IDs.
- [x] Document the contract and run focused then full RomeDB scoped checks.

Cloud rate limits were inspected for advice only; Cloud changes and deployment
are outside this implementation.

Baseline: the existing CLI accepts collection creation and document insertion,
then rejects `SELECT title FROM docs:a` with `FDB_ENGINE` / `near ":a": syntax
error`. The focused Rust regressions additionally cover alias/filter composition,
CTEs, derived/correlated queries, joins, transaction visibility, the complete
int64 range, string/integer identity and parameter isolation.
The existing Node record/cardinality test now exercises source projections,
dynamic keys and missing IDs in both synchronous and asynchronous clients.

Focused qualification: three Rust regressions pass (3.42 seconds). With 1,003
documents, the profiled record source returns the correct row with three measured
reads, zero full-scan steps, one index step and two B-tree seeks. EXPLAIN reports
an ID-index SEARCH. The outer zero-or-one result is labeled SCAN in the plan;
this is not a collection scan. Two initial test expectations were too strict:
they rejected that outer label and assumed at most two reads. The bounded counter
assertion now includes the index entry, document and outer result.

Full scoped qualification passes under the shared heavy-command wrapper:
formatting and eight-package Clippy, 996 Rust tests, 179 Node/application tests,
strict TypeScript and five C ABI tests, with no failures, ignored or skipped
tests. The two shipping-policy Python tests also pass. All 44 SELECT integration
tests pass, including the three new regressions. The updated Node record-source
test passes in both synchronous and asynchronous clients.

Commands: `scripts/romedb-heavy bash romedb/scripts/check.sh` from `turso/`,
then `scripts/romedb-heavy env PATH=... bash -c
'romedb/scripts/check-node.sh && romedb/scripts/check-c.sh'`. The main script
completed formatting, Clippy and Rust, then stopped because the systemd scope
did not inherit the shell's Node PATH. The remaining stages passed with the
explicit Rust/Node/pnpm PATH; passing Rust checks were not repeated. Rust test
compilation took 12m28s; Node tests took 86.6s; C ABI tests took 6.70s after a
21.1s binding build. These are qualification timings, not performance claims.

The source addition has no upstream or catalog/protocol change. Existing
uncommitted service/client/WAL work was preserved. No release or Cloud deployment
was performed. Completed task logs were removed after recording these results.

## Release delivery (2026-10-12)

The user separately authorized a minor release and Cloud deployment. RomeDB/RomeQL
2.3.0 is published and independently downloaded/verified; see [the release](release-2.3.0.md)
and [completed checklist](v2.3-tasks.md). Cloud's private 0.4.1 service now uses the
qualified engine; hosted fixed/bound sources, missing IDs, typed keys and index SEARCH
pass, and the fixture is removed. The original implementation-only scope and
qualification above remain historical. The API rate limit was not changed.
