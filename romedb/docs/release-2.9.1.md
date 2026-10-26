# RomeDB / RomeQL 2.9.1

Released for Linux x64 on 2026-10-20. [Download the release](https://github.com/romedb/romedb/releases/tag/fastdb-v2.9.1).

This patch release moves the engine to Turso 0.8.3. The catalog format and
stored data are unchanged: 2.5.0 to 2.9.1 open each other's files. There is no
API, SQL or client protocol change.

- MVCC durability. The logical-log header changes in memory only after its write
  succeeds, garbage collection keeps the state of transactions still in use,
  passive TRUNCATE keeps commits that arrive late, and B-tree delete and conflict
  checking are one step.
- AUTOINCREMENT. Inserting an explicit id equal to the next id now moves the
  sequence forward. Before, an explicit id 1 in a new table let the next generated
  row reuse id 1 and replace the row.
- `PRAGMA journal_mode` refuses to switch into or out of MVCC inside a transaction,
  as SQLite does. Before, the switch could leave a database that would not open
  again.
- All eight maintained engine exceptions are kept; see
  [the Turso 0.8.3 notes](turso-0.8.3.md) and [the exception register](core-exceptions.md).

The frozen build source is `364091ab9799e1aa562505fe5c93fc426587fd3e`.
[Exact-source CI](https://github.com/romedb/romedb/actions/runs/39389429641)
passes. Its first attempt failed one timing-dependent C ABI cancellation test on
the hosted runner; the rerun and every local run pass. Local scoped checks passed
1,104 Rust tests, 191 Node/application tests, five C ABI tests, two
release-policy tests, three upgrade-table tests, formatting, scoped Clippy and
strict TypeScript, with no failures. Four opt-in measurement tests are ignored by
default. The production profile retains debug assertions and overflow checks.

All 49 installed verification groups pass, including CLI graph and spatial
checks, Node 22.0.0/24.19.0, CPython 3.10.21/3.12.3/3.14.7, PHP FFI, Go race
detection, Swift and .NET. Immutable 1.0.0 to 2.9.0 upgrade, candidate writes,
reopen, rollback, unchanged backups and independent restore pass. The 2.4.0
package still refuses files with a new-format spatial index, and the 2.5.0 to
2.9.0 packages read files written by 2.9.1. The standalone production-profile
Rust consumer and the bounded 1,000-row application workload also pass. The build
and checks ran under `scripts/romedb-heavy` with its memory ceiling.

The shipping dependency review covers 359 package identities with zero unresolved
findings and retains the documented lru informational exception RUSTSEC-2026-0253.
The lockfile differs from 2.9.0 only in RomeDB workspace versions and the Turso
0.8.3 workspace crate versions.

[Public download verification](release-2.9.1-download.json) records the anonymous
download and checksums. See [qualification](release-2.9.1-qualification.json) and
[the checklist](v2.9.1-tasks.md).
