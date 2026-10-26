# RomeDB / RomeQL 2.9.0

Released for Linux x64 on 2026-10-16. [Download the release](https://github.com/romedb/romedb/releases/tag/fastdb-v2.9.0).

This minor release lets the Node SDK and CLI use RomeDB Cloud query version 3,
and fixes a column naming bug. The catalog format and stored data are unchanged:
2.5.0 to 2.9.0 open each other's files.

- Cloud query version 3. For Cloud URLs, the Node SDK and the CLI send
  `x-romedb-query-version: 3` on `/query` and `/read`. Binary and Vector values
  travel as canonical base64, and request bodies may be up to 1 MiB (was 64 KiB).
  Statement count (1 to 32), the 1,000-row result limit and the 256 KiB result
  limit are unchanged. Local services and embedded databases keep version 2.
  Cloud still accepts 2.8.0 and older clients on version 2.
- Node SDK saved writes are version 2 and record their query version; version 1
  saved writes from 2.8.0 replay at version 2. Responses decode both byte arrays
  and base64.
- CLI query journals are version 4 and read journals version 5; journals saved by
  2.8.0 replay without the header.
- A relational query whose subquery reads a document table returns unaliased
  keyword columns, such as `plan` or `key`, under their own names. They used to
  come back quoted (`"plan"`).

The frozen build source is `71817117fe6a9011691e11b8d32a95246c0609db`.
[Exact-source CI](https://github.com/romedb/romedb/actions/runs/38885713924)
passes. Local scoped checks passed 1,100 Rust tests, 191 Node/application tests,
five C ABI tests, two release-policy tests, three upgrade-table tests,
formatting, scoped Clippy and strict TypeScript, with no failures. Four opt-in
measurement tests are ignored by default. The production profile retains debug
assertions and overflow checks.

All 47 installed verification groups pass, including CLI graph and
spatial checks, Node 22.0.0/24.19.0, CPython 3.10.21/3.12.3/3.14.7, PHP FFI, Go
race detection, Swift and .NET. Immutable 1.0.0 to 2.8.0 upgrade, candidate
writes, reopen, rollback, unchanged backups and independent restore pass. The
2.4.0 package still refuses files with a new-format spatial index, and the 2.5.0
to 2.8.0 packages read files written by 2.9.0. The standalone production-profile
Rust consumer and the bounded 1,000-row application workload also pass. The build
and checks ran under `scripts/romedb-heavy` with its memory ceiling.

The shipping dependency review covers 359 package identities with zero unresolved
findings and retains the documented lru informational exception RUSTSEC-2026-0253.
The lockfile differs from 2.8.0 only in RomeDB workspace versions.

[Public download verification](release-2.9.0-download.json) records the anonymous
download and checksums. See [qualification](release-2.9.0-qualification.json) and
[the checklist](v2.9.0-tasks.md).
