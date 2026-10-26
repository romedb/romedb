# RomeDB 1.0.0 — embedded database

Release source: 1824de044fbdcb9d183e77fe30421d0ba25f5001.
GitHub distribution: Linux x64 archive and checksum file.

RomeDB combines ordinary relational SQL with typed document collections in one
embedded database. Each collection has its own backing table. Document insert
and ID-based upsert create missing collections automatically; validation and
managed indexes participate in the same transactions as relational data.

This release includes:

- Synchronous and worker-based asynchronous Node/TypeScript clients, with typed
  collection CRUD helpers alongside parameterized queries and explicit result
  cardinality helpers.
- Rust embedded API and CLI, migrations, schema/index inspection, query metrics,
  typed JSON/NDJSON document transfer and an offline backup/restore procedure.
- Nested document values, typed record references, scalar/unique indexes,
  explicit one-hop forward fetch, exact vector operations and bounded bundled
  slugify/normalize functions.
- The approved trigger-cancellation fix, preserving cancellation instead of
  misreporting database contention.

## Installation and compatibility

The downloadable Linux x64 archive contains the optimized CLI, local
romedb-node-1.0.0.tgz package, full source, dependency notices, manifest and
checksums. Node package installation and TypeScript checks passed on Node 22.0.0
and 24.19.0. Rust uses the pinned source checkout with Rust 1.88.0; no crates.io
or npm registry publication is claimed. Verify SHA256SUMS before use and follow
the archive README. Native binaries were built on Linux x64/WSL2 with glibc 2.39;
other platforms/distributions are not covered by this artifact evidence.

The engine is Turso v0.7.2, pinned at
046e9cbf67d22491e8ecc941ec2891b02a9f3cad, plus the documented local core exception.
RomeDB does not promise complete SQLite or SurrealDB compatibility. Supported
query forms and context-specific restrictions are in QUERY-CONTRACT.md.

## Data and performance limits

Preview.2-to-installed-1.0.0 upgrade and reopen checks passed. Back up before
upgrading; no downgrade guarantee is made. Use the checkpointed offline backup
procedure for the whole database. Document transfer does not include schemas,
indexes, relational data or migration history.

Exact vector search scans the data. The measured 1m-document/768-dimensional
fixture took about 406 seconds median for cosine top 10 and reached about
24.3 GiB process high-water RSS, including earlier loading/workloads. This is
capacity evidence, not an interactive retrieval guarantee. Indexed scalar
filtering on that fixture took about 1.51 seconds median. See PERFORMANCE.md for
the source identity, three-sample limits and development-host conditions.

Results can materialize in memory. Configured limits are not a global process
memory cap, and cancellation is cooperative. Inspect transaction observations
before retrying failed writes. One-hop fetch is explicit; inverse traversal,
indexed ANN/FTS/spatial search, user JavaScript, changefeeds, sync and cloud
hosting remain outside embedded V1.

## Verification

The integrated implementation passed 678 Rust tests with zero ignored and
106 Node/application tests, formatting, Clippy and strict TypeScript. The core
patch also passed 98 pinned upstream trigger tests. Exact versioned Node packages
passed Linux x64 installation and SDK checks on Node 22/24, and the bundle's
checksums verified. See romedb/docs/v1-package-evidence.md in the source archive
for the versioned-artifact history; final delivery evidence is recorded alongside
the release preparation after candidate packaging.
