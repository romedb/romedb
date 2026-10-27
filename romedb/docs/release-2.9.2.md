# RomeDB / RomeQL 2.9.2

Released for Linux x64 on 2026-10-21. [Download the release](https://github.com/romedb/romedb/releases/tag/fastdb-v2.9.2).

This patch release keeps the Turso 0.8.3 engine. The catalog format and stored
data are unchanged: 2.5.0 to 2.9.2 open each other's files. There is no API, SQL
or protocol change.

- Local service. When `romedb serve` stops cleanly, it finishes admitted requests
  and then checkpoints its MVCC log into the database file, so the next start has
  nothing to replay. A service that stopped because an outcome was uncertain keeps
  its log for recovery.
- Node client and RomeDB Cloud. Cloud refuses a request with `503` and a
  `query_queue_*` code before running it when the request waits too long behind a
  checkpoint. The client now waits for `retry-after` (at most 2 s, within the
  request timeout) and sends the same request once more. Other `503` responses are
  returned unchanged.
- Security. `rustls-webpki` 0.103.16 fixes RUSTSEC-2026-0382: certificate
  verification could accept paths through distrusted emSign intermediates.

The frozen build source is `110b5bfc43a34c7cca572db10742b602526010a1`.
[Exact-source CI](https://github.com/romedb/romedb/actions/runs/39549838762)
passes, including the new Node 22 and Python job. Local scoped checks passed 1,107
Rust tests, 192 Node/application tests, five C ABI tests, two release-policy tests
and three upgrade-table tests, with formatting, scoped Clippy and strict
TypeScript. Four opt-in measurement tests are ignored by default.

All 51 installed verification groups pass, including Node 22.0.0/24.19.0, CPython
3.10.21/3.12.3/3.14.7, PHP FFI, Go, Swift and .NET. Immutable 1.0.0 to 2.9.1
upgrade, candidate writes, reopen, rollback, unchanged backups and independent
restore pass. The standalone production-profile Rust consumer and the bounded
1,000-row application workload also pass. The build and checks ran under
`scripts/romedb-heavy`.

The shipping dependency review covers 359 package identities with zero unresolved
findings after the `rustls-webpki` update and retains the documented lru
informational exception RUSTSEC-2026-0253. See
[the review](dependency-security.md) and [qualification](release-2.9.2-qualification.json).
