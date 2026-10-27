# V2-A ANN qualification evidence

2026-09-25, Linux x64/WSL2, Rust 1.88.0, Node 24.19.0. Working tree on
`fb246a8e4`; existing engine WAL edits are retained. Native V2-A combined acceptance has passed. This is not a V2 release or a platform-wide performance claim.

## Implementation and focused checks

USearch 2.26.2 is pinned without default features. It supplies dense HNSW,
mutation, compaction and byte serialization through Rust/C++. The RomeDB adapter
stores graph snapshots, SHA-256 checksums, a bounded redo log and typed-record
mappings in ordinary engine tables. No new upstream core change is needed.

Focused checks passed before combined acceptance:

- One parser regression: explicit dimensions and metric options, either order;
  duplicates, missing options, malformed dimensions and multiple paths reject.
- Five managed integration tests: typed RomeQL hits, bindings and limits,
  post-filters/joins, native result materialization, build/write/drop lifecycle,
  IF NOT EXISTS, validation, indexed unique replacement, failed INSERT SELECT,
  savepoint branching, connection snapshots across commit, reopen and integrity.
- More than 1,000 UPSERTs cross graph checkpoints, then roll back and recover the
  original graph. Reused row/sequence IDs after savepoint rollback cannot return
  stale cache entries because event tokens differ.
- A child commits graph checkpoints and redo records, then exits with status 73
  during an uncommitted indexed mutation, bypassing destructors. Reopen recovers
  only committed records and passes the graph/document audit. This is process
  crash evidence, not a device power-loss guarantee.
- Three frontend regressions: metadata/checksum/stale-vector rejection;
  interrupted build preserves prior transaction work; bulk graph topology has
  a nonempty upper layer that is a proper subset of its base layer.
- One Node test passes for both synchronous and asynchronous clients: typed IDs,
  distances, rollback, validation, profile results, result budgets and inspection.

## Two findings changed the implementation

1. Query expansion 128 produced 570/640 matching exact top-10 members (89.1%) on
   the 100k L2 fixture. Expansion 512 is now the default, with the results below.
2. Reserving native capacity for each insertion recreated search contexts and
   their level generator. The isolated control produced 256 base nodes and zero
   upper-layer nodes. Reserving once produced ten upper-layer nodes. Bulk builds
   now reserve once; later growth uses geometric capacity. A regression checks
   the resulting hierarchy. The adapter allocates one native worker per cache.
   Control source: [ann-topology.rs](probes/ann-topology.rs).

## Measured search work

The [native probe](probes/README.md) uses a fixed generator seed, 100,000 float32
vectors with 64 dimensions and 64 independently generated held-out queries.
Connectivity is 32, construction expansion 200 and query expansion 512.
Comparison is against the library's exhaustive search on the same vectors.

| Native metric | Recall@10 | ANN mean | Exact mean | Build | Serialized graph |
|---|---:|---:|---:|---:|---:|
| Squared L2 | 99.53% | 3.384 ms | 6.506 ms | 133.244 s | 53,138,032 bytes |
| Cosine | 99.84% | 4.605 ms | 9.087 ms | 122.859 s | 53,138,032 bytes |

Native reported memory usage was 118,255,688 bytes per index. This is the
library's memory estimate, not process peak RSS. Buffer save/load took
81.017/109.976 ms for L2 and 56.019/74.520 ms for cosine. Snapshot publication
also incurs database writes; those times are not included in the native probe.

The [frontend benchmark](../frontend/examples/ann_benchmark.rs) instead compares
public RomeQL ANN queries with exact SQL cosine search over 10,000 64-dimensional
vectors and sixteen held-out queries. It checks matching-ID distances within
1e-5 and audits the resulting index. After fixing capacity growth:

| Frontend fixture result | Observation |
|---|---:|
| Recall@10 | 100% |
| Index build | 27.820 s |
| First ANN query, including cold load | 408.987 ms |
| Mean of subsequent ANN queries | 41.640 ms |
| Mean exact SQL query | 4,999.535 ms |

These are local debug-build measurements, not a release benchmark or a guarantee
for other data distributions. The dependency's Linux build script independently
uses C++ O3 and fast-math. Core/frontend Rust remains unoptimized. The earlier
per-insertion-reserve implementation took 42.956 s to build this fixture; query
recall alone did not detect its flat graph. Cache eviction, index count, redo
replay, connection count and graph checkpoint writes affect real workloads.

Engine EXPLAIN shows the materialized `__romedb_ann_hnsw_hits` result. It does not
expose native HNSW traversal statistics. Engine VM counters exclude preparation
work; they must not be presented as total query effort. There is no collection
scan fallback. The native call path and exhaustive comparisons establish what
was exercised; these fixtures do not prove universal recall.

## Acceptance logs

- Parser: `/tmp/romedb-ann-parser.log`.
- Frontend units: `/tmp/romedb-ann-unit.log`; the final combined run repeats all
  three regressions with the final source.
- Managed integration: `/tmp/romedb-ann-integration.log`.
- Node focused: `/tmp/romedb-ann-node.log`.
- Native probes: `/tmp/romedb-ann-probe-100k.log` (expansion 128 control),
  `/tmp/romedb-ann-probe-100k-ef512.log`, `/tmp/romedb-ann-topology-control.log`.
- Frontend benchmark: `/tmp/romedb-ann-frontend-benchmark-final.log`.
- Full Rust scope: **730 passed, zero failed/ignored**, across 63 test/doc-test
  groups. `/tmp/romedb-ann-rust-final.log`.
- Five-package Clippy (all targets, warnings denied): passed.
  `/tmp/romedb-ann-clippy-final.log`.
- Rebuilt CLI create/search/rollback/inspection/drop smoke: passed.
  `/tmp/romedb-ann-cli-final.log`.
- Full Node/application suite: **113 passed, zero failures/skips**, using the
  rebuilt debug addon. `/tmp/romedb-ann-node-final.log`.
- Scoped formatting and strict Node TypeScript: passed.
  `/tmp/romedb-ann-fmt-final.log`, `/tmp/romedb-ann-typescript-final.log`.

Dependency inventories now contain 275 declarations, 268 verified registry
archives and 173 distinct collected notice texts. The 33 entries without
filename candidates still require separate review; USearch's Apache-2.0 text is
included in THIRD_PARTY_NOTICES.md from the crate-recorded source revision
`f91fe5bc000222aa1af6e91daf78c2bb20b0c90e`, with its hash recorded in source
supplements. Existing locked packages were not upgraded; unrelated resolver
changes to Windows dependency edges were restored. Platform/WASM builds, final
artifact attribution and broader release benchmarks remain V2-C/V2-R gates.

The rebuilt debug Node addon reports dynamic requirements for libstdc++.so.6,
libmvec.so.1, libgcc_s.so.1, libm.so.6 and glibc/loader. V2 packaging must inspect
the final artifact's symbol-version requirements and test the advertised Linux
baseline; earlier V1 stripped-addon reports do not establish V2 compatibility.

The debug addon ELF report is `/tmp/romedb-ann-node-elf-debug.json`: 307,419,368
bytes, SHA-256 `5c7e658908e631bd0007c4c71e97105118ecdd6f5ac3e5acb72040525726c55a`,
maximum referenced GLIBC 2.35 and GLIBCXX 3.4.29. It has no ELF search paths.
These are observed symbol requirements, not distribution compatibility proof.
