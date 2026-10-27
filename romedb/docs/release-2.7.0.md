# RomeDB / RomeQL 2.7.0

Released for Linux x64 on 2026-10-15. [Download the release](https://github.com/romedb/romedb/releases/tag/fastdb-v2.7.0).

This minor release reports which RomeDB release runs a database. The catalog
format and stored data are unchanged: 2.5.0 to 2.7.0 open each other's files.

- `SELECT romedb_version()` returns the release of the engine that runs the
  statement, for example `'2.7.0'`. A database file reports the client package
  that opens it; the local service reports its own engine. It is not
  deterministic, so field CHECK expressions, computed fields and index
  expressions reject it.
- `romedb --version` (or `-V`) prints `romedb 2.7.0`.
- The database details reply (`info()` in Node) adds `engineVersion` for local
  services and embedded files. Cloud reports it too (2.5.3 today, Worker
  `08265f94`). Older services omit it, so clients must treat it as optional.

`sqlite_version()` keeps reporting the SQLite compatibility level and
`turso_version()` the upstream Turso engine or file header writer; neither is
a RomeDB release.

The frozen build source is `69730dd825b1377c0179b3aa9cd1e3eef9e05482`.
[Exact-source CI](https://github.com/romedb/romedb/actions/runs/38713612411)
passes. Local scoped checks passed 1,097 Rust tests, 188 Node/application tests,
five C ABI tests, two release-policy tests, three upgrade-table tests,
formatting, scoped Clippy and strict TypeScript, with no failures. Four opt-in
measurement tests are ignored by default. The production profile retains debug
assertions and overflow checks.

All 43 installed verification groups pass, including CLI graph and
spatial checks, Node 22.0.0/24.19.0, CPython 3.10.21/3.12.3/3.14.7, PHP FFI, Go
race detection, Swift and .NET. Immutable 1.0.0 to 2.6.0 upgrade, candidate
writes, reopen, rollback, unchanged backups and independent restore pass. The
2.4.0 package still refuses files with a new-format spatial index, and the 2.5.0
to 2.6.0 packages read files written by 2.7.0. The standalone production-profile
Rust consumer and the bounded 1,000-row application workload also pass. The build
and checks ran under `scripts/romedb-heavy` with its memory ceiling.

The shipping dependency review covers 359 package identities with zero unresolved
findings and retains the documented lru informational exception RUSTSEC-2026-0253.
The lockfile differs from 2.6.0 only in RomeDB workspace versions.

[Public download verification](release-2.7.0-download.json) records the anonymous
download and checksums. See [qualification](release-2.7.0-qualification.json) and
[the checklist](v2.7.0-tasks.md).
