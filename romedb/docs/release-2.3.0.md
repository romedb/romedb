# RomeDB / RomeQL 2.3.0

Released for Linux x64 on 2026-10-12. [Download the release](https://github.com/romedb/romedb/releases/tag/fastdb-v2.3.0).

This minor release adds indexed collection record sources:

```sql
SELECT title FROM docs:a;
SELECT title FROM type::record('docs', $key);
```

Integer and string keys stay distinct. Missing IDs return zero rows. Aliases,
additional filters, supported joins and nested queries keep their SQL behavior.
See the [record-source contract](record-sources.md).

The shared Node client connects to embedded files, the authenticated local
service or Cloud. The Linux distribution includes `romedb serve`; shared
applications connect to its single owning process. Rust, Node.js/TypeScript,
Python, PHP, Swift, C# and Go remain supported. Catalog version 5 and protocol
meanings are unchanged. Preserve a pre-upgrade backup and follow the quickstart's
historical full-text REINDEX guidance when upgrading older versions.

The frozen build source is `ff05997e33ba8e979eb2806ebb098b04776d8c6a`.
[Exact-source CI](https://github.com/romedb/romedb/actions/runs/38174318457)
and [PR CI](https://github.com/romedb/romedb/actions/runs/38177948434) pass:
1,024 Rust tests, 180 Node/application tests, five C ABI tests, two release-policy
tests, formatting, scoped Clippy and strict TypeScript, with no failures or skips.
The production profile retains debug assertions and overflow checks.

All 25 installed verification groups pass, including Node 22.0.0/24.19.0,
CPython 3.10.21/3.12.3/3.14.7, PHP FFI, Go race detection, Swift and .NET.
Immutable 1.0.0/2.0.0/2.1.0/2.2.0 upgrade, candidate writes, reopen, rollback,
unchanged backups and independent restore pass. Installed local service/shared
client fixtures, the standalone Rust consumer and a bounded 1,000-row,
32-dimension application workload also pass. The new upgrade harness explicitly
checkpoints before copying its offline backup.

The corrected installed Node smoke verifier and its patch are included separately
from the frozen source. The three-row fixture can use a primary-key seek with
zero iteration steps. The verifier now proves indexed access through positive
B-tree seeks, zero full-scan steps and EXPLAIN SEARCH. This verifier-only correction
does not change any distributed native artifact or Rust source.

The archive contains original build receipts, installed qualification receipts,
source, dependency notices and checksums. Its historical ENVELOPE.md points to
the immutable 2.1.0 measurements; the fresh bounded sample is separate and does
not establish a universal latency or capacity guarantee. The shipping dependency
review has no unresolved findings and retains the documented lru informational
exception RUSTSEC-2026-0253.

[Public download verification](release-2.3.0-download.json) confirms an anonymous
download, outer and internal checksums, the frozen Git source archive, C ABI smoke
and the downloaded Node package's record-source behavior. See
[qualification](release-2.3.0-qualification.json) and
[the completed checklist](v2.3-tasks.md).

Cloud's private runtime is built and deployed separately from this public source
archive. Hosted fixed/bound record sources, indexed access and cleanup are verified;
Cloud remains service 0.4.1 and its 30/minute/client-IP limit is unchanged. Browser/WASM, other platforms, graph-database features and package
registry publication are outside this release.
