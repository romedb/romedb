# RomeDB / RomeQL 2.5.4

Released for Linux x64 on 2026-10-15. [Download the release](https://github.com/romedb/romedb/releases/tag/fastdb-v2.5.4).

This patch release adds `romedb://` connection URLs and removes client round
trips. The engine, stored format and service protocol are unchanged: 2.5.0 to
2.5.4 open each other's files.

- The Node client and the CLI accept `romedb://` as an alias for `https://`. The
  scheme is replaced and the same URL checks apply, so a token, credentials or a
  query string in the URL are still refused. It changes no transport or protocol.
- Node `connect({ deferInfo: true })` on a Cloud database URL skips the details
  request: the database id comes from the URL and Cloud capabilities are
  documented. Reads start from sequence 0; `writeAtomic` fetches the details first
  when no sequence is known, and `prepareWrite` throws `FDB_SEQUENCE_UNKNOWN`
  instead of guessing.
- CLI `db access` reuses the previous reply's sequence when that write was the
  only commit and was confirmed within 2 s; otherwise it fetches the sequence as
  before, so another writer can never push a reply outside the replay window.
- rustls 0.23.46 in the CLI's HTTPS client fixes RUSTSEC-2026-0346 to 0349,
  published on 2026-10-14.

The frozen build source is `92edc941a13e3ae764416ed1e82eef32d4344a2d`.
[Exact-source CI](https://github.com/romedb/romedb/actions/runs/38669494070)
passes. Local scoped checks passed 1,095 Rust tests, 187 Node/application tests,
five C ABI tests, two release-policy tests, three upgrade-table tests,
formatting, scoped Clippy and strict TypeScript, with no failures. Four opt-in
measurement tests are ignored by default. The production profile retains debug
assertions and overflow checks.

All 39 installed verification groups pass, including CLI graph and
spatial checks, Node 22.0.0/24.19.0, CPython 3.10.21/3.12.3/3.14.7, PHP FFI, Go
race detection, Swift and .NET. Immutable 1.0.0 to 2.5.3 upgrade, candidate
writes, reopen, rollback, unchanged backups and independent restore pass. The
2.4.0 package still refuses files with a new-format spatial index, and the 2.5.0
to 2.5.3 packages read files written by 2.5.4. The standalone production-profile
Rust consumer and the bounded 1,000-row application workload also pass. The build
and checks ran under `scripts/romedb-heavy` with its memory ceiling.

The shipping dependency review covers 359 package identities against the
2026-10-15 RustSec database with zero unresolved findings and retains the
documented lru informational exception RUSTSEC-2026-0253. The lockfile differs
from 2.5.3 in RomeDB workspace versions and rustls.

[Public download verification](release-2.5.4-download.json) records the anonymous
download and checksums. See [qualification](release-2.5.4-qualification.json) and
[the checklist](v2.5.4-tasks.md).
