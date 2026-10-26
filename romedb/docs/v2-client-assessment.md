# Expanded native client scope

PHP, Swift, C# and Go are now requested alongside Rust, Node/TypeScript and Python.
See [implementation and qualification](native-language-clients.md). Earlier
client scope/deferral statements below are historical.

# V2 native clients and historical browser feasibility

Release scope correction: the user selected native Rust, Node.js/TypeScript and
Python only. Browser/WASM feasibility and qualifications below are retained as
parked history, not release gates. The Python wheel has been rebuilt after the
scalar-error fix; see [eight-test installed qualification](v2-python-scalar-evidence.md).


Current follow-up: both core proposals are now approved and integrated. The
installed browser client passes FTS lifecycle, OPFS reopen/recovery, V1 upgrade
and the repeated storage-fault matrix in Chromium/Firefox. See
[combined integration evidence](v2-core-integration-evidence.md), which retains
an unexplained Firefox shutdown timeout from the first fault run. Remaining
release gates include final platform/distribution qualification and attribution.
The earlier milestone results and pending-state descriptions below are historical;
their artifact hashes do not identify the newly integrated build.


Status: development evidence, 2026-09-25. V2-C remains open. No new package is
published. Rust and Node use the native frontend; upstream bindings that bypass
that frontend do not provide RomeQL or managed collections.

## Python development target

`romedb/bindings/python` adds the `romedb-embedded` distribution, imported as
`romedb`. Its PyO3 0.29.0 version was already pinned by the workspace. Maturin
1.12.6 builds a mixed Python/Rust package with the CPython 3.10 stable ABI.
The lockfile adds the RomeDB Python package without changing dependency versions.

The native boundary calls the same RomeDB frontend as Rust and Node. It reuses
the lossless portable-value codec, keeping int64, binary64 (including negative
zero), bytes, typed record IDs and vectors distinct. Public results preserve
duplicate column positions. Calls release the GIL, serialize each connection,
and use the existing cooperative cancellation/deadline path. The transaction
context holds the connection across nested savepoints. A queued operation's
deadline includes its wait for that connection.

Implemented: execution/cardinality helpers, collection CRUD, batches with partial
reports, transaction observations, query profiling, result/write limits,
migrations, document transfer and integrity audits. See the
[Python API overview](../bindings/python/README.md). This is an embedded API;
DB-API 2.0 and an asynchronous Python wrapper are not part of this first binding.

The local debug wheel is Linux x86_64, tagged
`cp310-abi3-manylinux_2_35_x86_64`. Clean offline installations pass **7 client
integration tests each** on CPython **3.10.21, 3.12.3 and 3.14.7**. Tests cover:

- Typed persistence, duplicate labels, document CRUD and quoted field names.
- Nested rollback, index integrity, write/result limits and query profiling.
- Managed FTS/ANN, spatial H3 and a successful JavaScript function invocation.
- Migration history/failure, JSON/NDJSON transfers and partial batch reports.
- Cancellation from another Python thread, deadline interruption, prior-work
  preservation and a timeout while queued behind another thread's transaction.
- Invalid inputs, cardinality errors, idempotent close and rollback on close.

These checks do not qualify throwing/limited JavaScript functions: the separately
documented [scalar-error core review](proposals/udf-error-transaction.md) is still
pending, and the active core remains unchanged.

Wheel SHA-256:
`5e4dd81bf046daab773eba229f6ee27dc6bca274bdb1e85fbeaabcf1d6b3d793`.
It includes the API, native ABI3 library, typing files, license/notice texts and
Maturin's SBOM. Source-notice inventory: 263 normal/build dependency declarations,
256 checked crate archives, 171 distinct notice texts. Seven workspace packages
and 28 packages without conventionally named notice candidates still require
distribution review; these counts are not a complete-license-audit claim.

Logs: `/tmp/romedb-python-wheel.log`, `/tmp/romedb-python310-final-tests.log`,
`/tmp/romedb-python-test-final-tests.log`, `/tmp/romedb-python314-final-tests.log`,
`/tmp/romedb-python-clippy-final.log`. Rust formatting, Python Ruff 0.13.2 checks
and package Clippy with warnings denied pass. This is focused client evidence,
not a new full RomeDB acceptance run. The installed Node addon is unchanged.

Reproduce using the pinned Maturin build backend and a preinstalled interpreter:

```sh
maturin build --locked --manifest-path romedb/bindings/python/Cargo.toml --out /tmp/romedb-wheels
python3 romedb/scripts/check-python-wheel.py /tmp/romedb-wheels/<wheel>.whl --python /path/to/python --uv /path/to/uv
```

The checker creates a fresh environment, installs offline, checks packaged notices
and typing files, and runs the integration suite with isolated Python imports.
Final optimized artifacts, source distribution, supported operating systems,
minimum libc/runtime requirements and complete notices belong to V2-R.

## Browser feasibility probe

The existing upstream browser binding targets `wasm32-wasip1-threads` with
NAPI/Emnapi workers. It calls the engine directly, so it cannot be relabeled as
RomeDB. A new frontend boundary and browser persistence/lifecycle tests are needed.

The actual RomeDB frontend cross-check used Rust 1.88.0 with that target. The first
attempt stopped at missing `clang++` (`/tmp/romedb-wasm-probe.log`). A second
attempt supplied WASI SDK 24.0, the version used by pinned rquickjs-sys, its
sysroot and C++17 (`/tmp/romedb-wasm-probe-sdk.log`). QuickJS's C archive then
compiled, but the Rust binding failed because rquickjs-sys 0.12.2 ships no
`wasm32-wasip1-threads.rs` binding. This initial blocker is now resolved with a
WASI-only rquickjs-sys bindgen feature and pinned bindgen 0.72.1. The subsequent
[WASM runtime probe](browser-removal.md) also resolves C++ linkage with SDK 33 and
passes ANN, H3, JavaScript and C++ error checks in Chromium and Firefox.

Independently, `core/index_method/mod.rs` excludes the upstream FTS implementation
on WASM. A successful frontend compile would therefore not establish full V2
search support. The runtime probe establishes initial USearch/QuickJS feasibility;
full resource/lifecycle qualification, worker interruption and persistent browser
I/O remain open. No active core or vendor implementation was changed. A separate
temporary FTS candidate now passes lifecycle, rollback, snapshot isolation and
worker cleanup in Chromium and Firefox using an initialized asynchronous
reactor and an eight-worker pool. Its [core proposal](proposals/fts-wasi.md)
remains unapproved and unintegrated; browser persistence is still unimplemented.

The frontend now exposes `Database::open_with_io` with ordinary automatic WAL
management for a future browser storage adapter. Its native memory-backend
regression preserves catalog/index validation and rollback across close/reopen
without touching a host database file. All 14 catalog integration tests pass;
frontend/test-package Clippy with all targets and warnings denied, formatting and
`git diff --check` also pass. Logs: `/tmp/romedb-custom-io-catalog-suite.log` and
`/tmp/romedb-custom-io-clippy.log`. This does not implement or qualify OPFS.

Next browser steps:

- [x] Establish initial reproducible target-specific QuickJS bindings and USearch
  linkage, including real-browser dependency checks.
- [ ] Resolve browser FTS support explicitly; preserve the indexed-search contract.
- [x] Build an in-memory RomeDB frontend browser binding with explicit capabilities;
  see [browser client evidence](browser-removal.md) for the shared protocol,
  installed-package checks and refreshed Python wheel.
- [ ] Prove worker lifecycle, typed values, cancellation, transactions, persistence
  and catalog compatibility in a real browser before selecting release targets.

The native Python target can progress independently. Browser support remains a
required open V2-C item; these probes neither remove it nor claim browser parity.

The later [browser client milestone](browser-removal.md) implements the shared
typed protocol and in-memory client, followed by an
[initial OPFS adapter](browser-removal.md). Installed Chromium/Firefox checks now
cover persistence, ANN/spatial reopen, exclusive ownership and abrupt-page-close
WAL recovery. These supersede the earlier feasibility-only statements above.
Subsequent [WAL/fault qualification](v2-wal-durability-evidence.md) and
[V1-file browser upgrade/restore checks](v2-upgrade-restore-evidence.md) pass on
the identified artifacts. Python was also rebuilt and requalified after the WAL
fix and again after the approved FTS backing-storage fix. The latter wheel passes
new integrity/FTS-drop assertions that fail on the previous wheel, on all three
recorded Python runtimes. See the upgrade evidence for exact artifact hashes.
Final platform/distribution and browser FTS integration gates remain open.

Later [notice packaging qualification](v2-notice-evidence.md) adds verified
repository-source supplements to the generated bundles and checks their packaged
copies. Its updated counts and artifact hashes supersede the notice-only counts
above; the earlier runtime evidence retains its original artifact scope.
