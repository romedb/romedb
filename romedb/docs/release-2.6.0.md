# RomeDB / RomeQL 2.6.0

Released for Linux x64 on 2026-10-15. [Download the release](https://github.com/romedb/romedb/releases/tag/fastdb-v2.6.0).

This minor release makes Cloud connections start faster. The engine, stored
format and local service protocol are unchanged: 2.5.0 to 2.6.0 open each other's
files.

- Node `connect()` on a Cloud database URL no longer requests the database
  details. `writeAtomic` fetches them when it needs a sequence. Code that calls
  `prepareWrite` right after connecting must first read, call `info()` or pass
  `afterSequence`; `deferInfo: false` restores the 2.5 behavior. This behavior
  change is why the version is 2.6.0.
- Node `connect()` on a Cloud URL sends `POST /warm` in the background, so Cloud
  starts the container and opens the database before the first query.
  `db.warm()` sends it on demand and returns `ready`, `starting` or `skipped`;
  `warm: false` turns the automatic request off. Cloud never lets a warm request
  replace a database used in the last five seconds.
- CLI `db warm UUID`; interactive `db access` warms in the background when stdin
  is a terminal.

On Cloud after a container stop, a read sent 6 s after a warm request spent 37 ms
in the API; without warm the first read spent 1.0 to 1.9 s (Cloud record
`docs/all-users-20261015.md`).

The frozen build source is `6e5d4e7f612e39c854209d985b77dd4d1ee61b0b`.
[Exact-source CI](https://github.com/romedb/romedb/actions/runs/38679000318)
passes. Local scoped checks passed 1,095 Rust tests, 188 Node/application tests,
five C ABI tests, two release-policy tests, three upgrade-table tests,
formatting, scoped Clippy and strict TypeScript, with no failures. Four opt-in
measurement tests are ignored by default. The production profile retains debug
assertions and overflow checks.

All 41 installed verification groups pass, including CLI graph and
spatial checks, Node 22.0.0/24.19.0, CPython 3.10.21/3.12.3/3.14.7, PHP FFI, Go
race detection, Swift and .NET. Immutable 1.0.0 to 2.5.4 upgrade, candidate
writes, reopen, rollback, unchanged backups and independent restore pass. The
2.4.0 package still refuses files with a new-format spatial index, and the 2.5.0
to 2.5.4 packages read files written by 2.6.0. The standalone production-profile
Rust consumer and the bounded 1,000-row application workload also pass. The build
and checks ran under `scripts/romedb-heavy` with its memory ceiling.

The shipping dependency review covers 359 package identities with zero unresolved
findings and retains the documented lru informational exception RUSTSEC-2026-0253.
The lockfile differs from 2.5.4 only in RomeDB workspace versions.

[Public download verification](release-2.6.0-download.json) records the anonymous
download and checksums. See [qualification](release-2.6.0-qualification.json) and
[the checklist](v2.6.0-tasks.md).
