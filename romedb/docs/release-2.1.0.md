# RomeDB / RomeQL 2.1.0 release

Status: **released for Linux x64**, 2026-09-25. [Public release](https://github.com/romedb/romedb/releases/tag/fastdb-v2.1.0),
[qualification receipt](release-2.1.0-qualification.json),
[verified download](release-2.1.0-download.json) and
[measured operating envelope](production-envelope.md).
The [finite checklist](v2.1-tasks.md) is complete. Immutable 2.0.0
artifacts remain unchanged.

## Changes

- [SQLite-file adoption](sqlite-adoption.md): check or import a consistent
  snapshot, including committed WAL data, without modifying the source. Supported
  tables remain relational. Unsupported schema features are explicitly rejected;
  existing destinations are never overwritten.
- [Nested projections](nested-projections.md): stored record IDs under plain
  selection, explicit wildcard fetching, first/last array indexes, dot indexes
  and negative-index extensions. Paths are finite and bounded. Compact SQLite
  bracketed identifiers retain their SQL meaning.
- [Native build policy](production-build.md): an optimized named Rust profile
  retaining assertions and overflow checks, with artifact provenance checks and
  a reproducible application workload. See the [measured envelope](production-envelope.md).
- [Full-text construction](fulltext-build-evidence.md): bounded initial insert
  batches reduce repeated index commits while preserving validation, transaction
  rollback and persisted document mapping.
- [Deployment](deployment.md) and [operations](operations.md): one owning Linux
  process per file, multiple connections inside it, error/commit reconciliation,
  checkpoint-status handling, maintenance backups and restore drills.
- [Dependency updates](dependency-security.md): patched Rust dependencies,
  SQLite 3.53.2 for adoption and QuickJS-NG 0.16.2 for stored functions. Existing
  function definitions retain their metadata format; applications should verify
  deterministic outputs when adopting a new JavaScript runtime.
- [Native returned-I/O tests](native-io-errors-evidence.md): disk-full, I/O,
  partial-write and failed-sync cases preserve whole old/new indexed state and
  permit recovery/subsequent writes under the documented acknowledgement rules.

## Completed source and artifact checks

The frozen artifact source is
[`587c3b4afae7382920fcdec64e91b1a97eda6f5a`](https://github.com/romedb/romedb/commit/587c3b4afae7382920fcdec64e91b1a97eda6f5a).
Combined source checks pass **759 Rust**, **121 Node/application** and **five
C ABI** tests, with zero ignored Rust tests, formatting, scoped Clippy and strict
TypeScript. [Hosted scoped CI 36159249358](https://github.com/romedb/romedb/actions/runs/36159249358)
passes on that exact commit. Independent source review found no blocking issue.

The approved checkpoint correction syncs WAL frames before NORMAL-mode database
backfill and preserves that barrier across failed asynchronous sync/retry,
including automatic checkpoint cleanup after a committed write. Seven permanent
crash-model/boundary regressions pass; affected checkpoint/VACUUM checks are
recorded separately in the [review](proposals/checkpoint-wal-sync.md).
The first candidate also exposed a canceled-write recovery defect. Its separately
approved [savepoint correction](proposals/cancellation-savepoint-poison.md)
restores the transaction marker only after successful rollback, while retaining
the abandoned-write guard. Both corrections are present in the frozen source.
The earlier failed bundle remains historical evidence and is not qualified for
publication.

The corrected optimized bundle passes **all 20 installed verification groups**:
CLI and SQLite adoption, C ABI and native ELF checks, both Node installations,
three Python runtime checks, PHP, Go with race detection, Swift, C#, process ownership,
and immutable V1/V2 upgrade and restore. Actual in-flight native-write
cancellation now preserves earlier caller work through COMMIT/root RELEASE in
the installed Node and C ABI checks; all three installed Python cancellation
suites pass too. SQLite adoption passes all 14 source-preservation/cross-engine
fixture groups with SQLite 3.53.2 in the importer. V2 upgrade coverage includes
persisted functions and unchanged backup generations.

| Client | Qualified runtime on the recorded Linux x64 host |
|---|---|
| Rust | Rust 1.88.0; standalone consumer built from the exact distributed source archive |
| Node.js/TypeScript | Node 22.0.0 and 24.19.0 |
| Python | CPython 3.10.21, 3.12.3 and 3.14.7 |
| PHP | PHP 8.3.6 with FFI |
| Go | Go 1.27.1, including race checks |
| Swift | Swift 6.4 |
| C# | .NET SDK 8.0.425 |

The exact-archive Rust consumer passes both smoke suites under
`romedb-production`, retaining all 332 resolved registry/git package identities
from the shipping lockfile. Its tested source archive is byte-identical to the
builder archive, and its receipt is bound to the same build manifest/checksums.
Workload and public-download qualification are separately recorded in
[the release receipt](release-2.1.0-qualification.json) and
[verified download](release-2.1.0-download.json).

| Immutable build identity | SHA-256 |
|---|---|
| Source archive | `83b51ed211f2de363e8bcc1df3a23225c5ac5d702f0c77a252576ee9e34ce8a6` |
| Cargo.lock | `4fffaa23990bc8b514b0081b63da337223320efab2c9371ea4ea42cdf2521194` |
| Build manifest | `5bcbf0153949379d2657e63554f4e86dbd862181d9c9d794c3cab1c172cd3d2d` |
| Build SHA256SUMS | `2c087d8063d9e0733a4fe27f606e96e098177bbaad9d57a19008232b6020496b` |

The source archive contains the frozen prequalification documentation snapshot.
Later qualification records are separate from those immutable source bytes.
The local build manifest continues to identify a candidate; it is not modified
to claim publication. Raw source, installed-matrix and Rust receipts are retained
in the qualified distribution.

## Distribution and compatibility

Linux x64 on the documented Ubuntu 24.04 baseline, with Rust, Node/TypeScript,
Python, PHP, Swift, C# and Go clients in the GitHub release bundle. No cloud,
browser/WASM, additional platform or graph-database product scope is added.
SQL joins, typed references, indexed inverse lookups and bounded document fetching
remain existing capabilities. Language-registry publication is not part of this
release gate.

The release preserves V2 catalog format 3 and reads older supported formats.
Preserve a pre-upgrade backup. Exact-artifact checks include actual released V1
and 2.0.0 artifacts, candidate writes/reopens and backup restores. They do not
promise a 2.0.0 binary downgrade; a binary downgrade is not a data rollback. SQLite adoption is separate from RomeDB version upgrades.

## Completed release evidence

- [x] Clean source/lockfile identity and exact-source hosted scoped CI.
- [x] Integrated checkpoint/cancellation corrections, affected regressions and combined source checks.
- [x] Exact seven-client artifacts, standalone archived Rust consumer, SQLite adoption and process ownership.
- [x] Released V1/V2 upgrade and restore checks, including stored functions.
- [x] Measured search/resource envelope and sustained readers/writer behavior.
- [x] Versioned public download, checksum receipt and release URL.

Both unchanged workload configurations pass: 1,000 documents/32 dimensions
and 5,000 documents/128 dimensions, each with one and four connections,
5,000 mixed operations and 1,000 concurrent rounds where applicable.
The [operating envelope](production-envelope.md) records actual results,
resource headroom and finite scope; these are not universal scale guarantees.

The published archive is `romedb-2.1.0-linux-x64.tar.gz` (71,581,011 bytes),
SHA-256 `94d8988ff82cb1649edf4065bd447f11e78ad26a5e558587b3736444ffd2e686`. Anonymous HTTPS retrieval verified the archive,
its sibling checksum, every internal checksum, qualified payload identity
and the annotated tag pointing to the exact artifact source. See the
[download receipt](release-2.1.0-download.json). Later documentation commits
record qualification; they do not change the tagged build source.
