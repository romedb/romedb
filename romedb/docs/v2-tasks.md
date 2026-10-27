# Embedded RomeDB and RomeQL V2

Current direction: 2026-09-25. V1 is released. Cloud v0.2.0 remains a separate
workstream; no cloud implementation or deployment belongs in this checklist.
Preserve V1 behavior and upgrade paths, except documented approved corrections.
The release client set is native Rust, Node.js/TypeScript, Python, PHP, Swift,
C# and Go. Browser/WASM support is removed from the active checkout and outside release scope. V2 is released as `fastdb-v2.0.0`. See [the release record](release-2.0.0.md)
and [verified public download](release-2.0.0-download.json).

## Checklist

- [x] V2-0: Replace stale active handoffs; retain release and historical evidence.
- [x] V2-S1: Spatial point construction, strict coordinate validation and a
  documented distance model through RomeQL SELECT and document writes; test
  antimeridian/poles, errors, persistence and transaction rollback.
- [x] V2-S2: Managed spatial index and indexed radius search. Adopt the proposed
  `CREATE SEARCH INDEX ... USING SPATIAL` and `search::near` only with atomic
  build/write/drop, reopen/rollback, catalog versioning, integrity checks,
  deterministic distance/ID ordering, and measured index use. Compare against
  exhaustive distance evaluation at cell boundaries, poles and the antimeridian.
  Candidate filtering must have no false negatives; refine every candidate.
- [x] V2-S3: Cell aggregation with a documented grid/resolution contract; evaluate
  H3 against alternatives before committing dependencies or persisted cell IDs.
  [Design assessment](v2-cell-design.md) selects H3; h3o 0.9.4 without default
  features passes reference, geographic, grouping, persistence and client checks.
- [x] V2-Q1: Record brace projections, keeping zero-or-one rowset cardinality;
  [adopted contract](v2-record-projections.md) specifies object wildcard, one-hop
  forward fetch, naming and limits, with parser/engine/client qualification.
- [x] V2-Q2: Declared inverse relationships with required reference indexes,
  deterministic pagination and bounded expansion; no silent collection scan.
- [x] V2-F: Native indexed full-text search with ranking, filters, index lifecycle,
  explain plans and consistency tests. The separately approved backing-storage
  fix is integrated in `12109384a`; affected native suites, installed Python
  integrity/drop assertions and the complete V1 upgrade/restore rehearsal pass.
  See [integrated evidence](v2-upgrade-restore-evidence.md). Browser support is removed.
- [x] V2-A: Indexed ANN search; define dimensions/metric, deterministic ties,
  filters, lifecycle, recall/performance evidence and exact-search comparison.
- [x] V2-J: Sandboxed user JavaScript functions with bounded execution, no external
  I/O or database re-entry, explicit determinism/versioning and cancellation tests.
- [x] V2-C: Qualify native Rust, Node.js/TypeScript, Python, PHP, Swift, C# and Go clients.
  Implementation and Linux development qualification: [client guide](native-language-clients.md).
  - [x] Shared C ABI with safe handle lifecycle, lossless values and transaction diagnostics.
  - [x] PHP FFI package and native integration checks.
  - [x] Swift Package Manager library and native integration checks.
  - [x] C#/.NET library and native integration checks.
  - [x] Go/cgo module and native integration checks.
  - [x] Native Python binding and initial installed-wheel qualification on Linux
    x86_64 / CPython 3.10, 3.12 and 3.14.
  - [x] Rebuild Python after approved scalar-error integration; eight installed
    tests pass on all three runtimes, including throwing JavaScript, nested
    savepoint recovery, failed-write atomicity and cancellation. See
    [current Python evidence](v2-python-scalar-evidence.md).
  - [x] Same installed native Node development archive passes synchronous/worker
    V2 lifecycle, persistence and TypeScript checks on Node 22.0.0 and 24.19.0;
    see [package evidence](v2-node-package-evidence.md). Final artifacts include
    the subsequent scalar-error fix and current notices; see the release record.
  - [x] Standalone Rust application builds/runs offline outside the workspace,
    using direct V2 APIs and persistence checks; see
    [Rust consumer evidence](v2-rust-client-evidence.md).
  - [x] Qualify final versioned client artifacts and the supported native platform matrix.
- [x] V2-R: Full scoped acceptance, V1 database upgrade/restore, client artifacts,
  language/docs/tooling parity, benchmarks and explicit supported-platform release.
  - [x] Integrate the separately approved first-commit FULL-mode WAL sync fix in
    isolated commit `fe2ccd404`; see [review](proposals/wal-first-commit-sync.md).
  - [x] Repeat the native sync probe and installed-browser durability/fault checks
    on the integrated source; see [evidence](v2-wal-durability-evidence.md).
  - [x] Initial V1 released-client database upgrade/new-write/reopen smoke against
    the rebuilt Node client; the final broader upgrade/restore rehearsal also passes.
  - [x] Include pinned repository notice supplements in offline-generated client
    bundles and verify the browser/Python packaged copies; see
    [notice evidence](v2-notice-evidence.md). Pinned Bon/DataSketches/Tantivy
    follow-up texts and crc32c inline attribution are also included. Final native
    attribution is collected and packaged, with checksummed declarations and
    20 offline generator tests.
  - [x] Align the RomeQL editor with implemented V2 spatial/H3 and replacement
    function syntax; qualify formatter/engine parity and the installed VSIX. See
    [tooling evidence](v2-language-tooling-evidence.md). No publication is claimed.
  - [x] Integrate the separately approved scalar-error and opt-in WASI FTS fixes
    in isolated commits; maintain [upstream replacement/removal criteria](core-exceptions.md).
  - [x] Combined acceptance passes 738 Rust and 115 Node/application tests, with
    formatting, Clippy and TypeScript; see [evidence](v2-core-integration-evidence.md).
  - [x] Qualify final native artifacts, finish attribution and publish the release.
  - [x] Integrate the separately approved FTS backing-storage fix in isolated
    commit `12109384a`, preserving the reviewed patch exactly.
  - [x] Repeat native V1 upgrade/restore and affected suites on that integrated source.

Unique conflict targets, OMIT, SPLIT, strict schemas and array-element indexes
remain RomeQL ergonomic candidates requiring separate semantics review; they are
not silently promoted into required scope. Broader graph traversal, advanced
geometry, changefeeds/sync and procedural scripting remain V3.

## Release closure

All V2 milestones are complete for Linux x64. Clean source commit
`4d118ab1f819ff5deb32aa29dfe739ca9b086785` supplies the published artifacts.
[Hosted CI](https://github.com/romedb/romedb/actions/runs/36131883058) passes
738 Rust tests, 115 Node/application tests and three C ABI tests, plus formatting,
Clippy and TypeScript. Final artifacts pass all seven clients and V1 upgrade/restore.
The Rust consumer also passes from the extracted source archive. The public
98,684,171-byte bundle and all 56 internal checksums are verified.

Detailed milestone evidence remains historical to its recorded source/artifact.
The [final release record](release-2.0.0.md) supersedes earlier pending-state
statements and development artifacts. Future capabilities require new scope;
there is no remaining V2 release gate in this checklist.

## Removed browser support

The user removed browser support, then added PHP, Swift, C# and Go to the native
Rust, Node.js/TypeScript and Python client set.
Browser code, build/probe scripts and the browser-only WASI core exception are
removed. Firefox/Chromium/OPFS/Vite qualification does not gate the release.
Historical reviews and evidence remain; a verified source archive is outside the
checkout. See [the removal record](browser-removal.md). Python's shared protocol
is retained. No browser artifact will be published with V2.

## Final release delivery checklist

- [x] Align all RomeDB package identities to 2.0.0, including Go's /v2 module path.
- [x] Prepare a reproducible bundle containing CLI, Node archive, Python wheel,
  shared native library, PHP/Swift/Go sources, C# package, source, notices and checksums.
- [x] Resolve remaining attribution gaps and include notices in every native package.
  See [Linux rehearsal evidence](v2-linux-release-evidence.md) and the
  [final release qualification](release-2.0.0.md).
- [x] Verify the exact versioned artifacts, V1 upgrade/restore and advertised platforms.
- [x] Commit the complete intended source, run scoped CI and verify provenance.
- [x] Publish the versioned release and verify downloadable artifacts and checksums.

All release package identities are 2.0.0; the versioned GitHub release is public.
The user confirmed Linux for V2. Binary scope is Linux x64, continuing the V1
release platform. macOS and Windows are outside this release.

Versioned-source verification passed: 738 Rust tests, 115 Node/application tests,
3 C ABI tests, formatting, Clippy and strict TypeScript. The log is
`/tmp/romedb-v2-versioned-acceptance.log`. The bundle build and exact-artifact
verification scripts are `build-v2.py` and `check-v2-bundle.py`; an uncommitted
source rehearsal must use `--development` and is never publication eligible.
Attribution covers every native dependency declaration. Runtime linkage, pinned
Rust notices and inclusion in final packages are verified in the release record.
