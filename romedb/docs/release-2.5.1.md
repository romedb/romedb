# RomeDB / RomeQL 2.5.1

Released for Linux x64 on 2026-10-13. [Download the release](https://github.com/romedb/romedb/releases/tag/fastdb-v2.5.1).

This patch release closes the remaining spatial upgrade items. It changes no
stored format: 2.5.0 and 2.5.1 open each other's files.

- **Nearest search with selective filters.** Radius rounds stop once the observed
  match rate shows that one scan is cheaper. When the filter field has a scalar
  index, the planner counts both candidate sets and uses the smaller one. In
  2.5.0 a scalar index on the filter field made nearest search slower.
- **Metered reads** charge nearest rounds and planning counts to the statement's
  read and VM budgets. 2.5.0 charged only the final statement.
- **Faster document reads.** Field accessors walk the stored layout and stop at
  the requested field, and accessors that return encoded values pass the stored
  bytes through. Scans and row conversion take about half the time.
- **More evidence.** Interruption, deadline and limit tests during nearest rounds,
  interrupted `REINDEX` of a 2.4.0-format index, frozen work ceilings, a
  randomized write test against scans, a moving-driver benchmark and polygon
  steps in the shared native client fixture.

On a local 100,000-document evaluation, nearest search for a 0.1% fleet fell from
886 to 347 ms without a fleet index and from 523 to 7 ms with one. A scan with one
filter fell from 258 to 125 ms. See the [evidence](spatial-upgrade-evidence.md#251-follow-up).
These are samples, not latency or capacity guarantees.

The frozen build source is `ae6300eb7c7246c66c45c68debb4aaf2331a286a`.
[Exact-source CI](https://github.com/romedb/romedb/actions/runs/38396791847)
passes. Local scoped checks passed 1,089 Rust tests, 185 Node/application tests,
five C ABI tests, two release-policy tests, formatting, scoped Clippy and strict
TypeScript, with no failures. Four opt-in measurement tests are ignored by
default. The production profile retains debug assertions and overflow checks.

All 33 installed verification groups pass, including CLI graph and CLI spatial
checks, Node 22.0.0/24.19.0, CPython 3.10.21/3.12.3/3.14.7, PHP FFI, Go race
detection, Swift and .NET. Immutable 1.0.0/2.0.0/2.1.0/2.2.0/2.3.0/2.4.0/2.5.0
upgrade, candidate writes, reopen, rollback, unchanged backups and independent
restore pass. The 2.4.0 package still refuses files with a new-format spatial
index, and the 2.5.0 package opens and reads files written by 2.5.1. The
standalone production-profile Rust consumer and the bounded 1,000-row application
workload also pass.

With the release Node package, the [SurrealDB comparison](spatial-surrealdb-comparison.md)
and three moving-driver runs were repeated; both are in the release evidence.

`INFO FOR TABLE` and `INFO FOR INDEX` now report `geometry` (`point` or `region`)
and `format` for spatial indexes.

The shipping dependency review covers 359 package identities with zero unresolved
findings and retains the documented lru informational exception RUSTSEC-2026-0253.
The lockfile differs from 2.5.0 only in RomeDB workspace versions.

[Public download verification](release-2.5.1-download.json) records the anonymous
download and checksums. See [qualification](release-2.5.1-qualification.json) and
[the checklist](v2.5.1-tasks.md).
