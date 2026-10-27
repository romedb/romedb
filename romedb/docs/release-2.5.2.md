# RomeDB / RomeQL 2.5.2

Released for Linux x64 on 2026-10-14. [Download the release](https://github.com/romedb/romedb/releases/tag/fastdb-v2.5.2).

This patch release makes spatial `EXPLAIN` read no data. It changes no stored
format: 2.5.0, 2.5.1 and 2.5.2 open each other's files.

In 2.5.1, `EXPLAIN` of a nearest statement ran an index-only search to choose the
first radius, and the planner's scalar-index and candidate counts also ran. A
metered `EXPLAIN` reported between 66 and 3,041 rows read in the test fixture.
Cloud's qualification of 2.5.1 found this, because Cloud requires `EXPLAIN` to
read no customer data.

In 2.5.2, `EXPLAIN` shows the spatial plan, with the first nearest round at its
nominal 100 m radius, and `search::nearest` describes its first index search
instead of running it. Choices that count data are made only when the statement
runs: the starting and later radii, the scalar-index preference and the early
scan. An `EXPLAIN` can therefore name the spatial index for a statement that
runs through a scalar index or a scan. Metered `EXPLAIN` reports zero rows read
for every spatial form. See the [spatial contract](spatial-upgrade.md).

The frozen build source is `c746ea55e3f498a62d90dcca44b2590173d5bce4`.
[Exact-source CI](https://github.com/romedb/romedb/actions/runs/38476106656)
passes. Local scoped checks passed 1,090 Rust tests, 185 Node/application tests,
five C ABI tests, two release-policy tests, formatting, scoped Clippy and strict
TypeScript, with no failures. Four opt-in measurement tests are ignored by
default. The production profile retains debug assertions and overflow checks.

All 35 installed verification groups pass, including CLI graph and spatial
checks, Node 22.0.0/24.19.0, CPython 3.10.21/3.12.3/3.14.7, PHP FFI, Go race
detection, Swift and .NET. Immutable 1.0.0 to 2.5.1 upgrade, candidate writes,
reopen, rollback, unchanged backups and independent restore pass. The 2.4.0
package still refuses files with a new-format spatial index, and the 2.5.0 and
2.5.1 packages read files written by 2.5.2. The standalone production-profile
Rust consumer and the bounded 1,000-row application workload also pass.

The local WSL user session was unavailable after a host restart, so the build and
checks ran with the shared lock and two-job limits of `scripts/romedb-heavy` but
without its memory ceiling. Execution paths are unchanged since 2.5.1, so the
2.5.1 SurrealDB comparison and moving-driver samples still apply.

The shipping dependency review covers 359 package identities with zero unresolved
findings and retains the documented lru informational exception RUSTSEC-2026-0253.
The lockfile differs from 2.5.1 only in RomeDB workspace versions.

[Public download verification](release-2.5.2-download.json) records the anonymous
download and checksums. See [qualification](release-2.5.2-qualification.json) and
[the checklist](v2.5.2-tasks.md).
