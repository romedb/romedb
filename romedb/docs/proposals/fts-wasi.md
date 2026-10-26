# Proposed core exception: opt-in full-text support on WASI

Retired: after approval/integration, the user explicitly removed browser support.
Commit `ae6777a17` reverts the core exception; frontend wiring is removed too.
The proposal below is historical and must not be reapplied automatically. See
[removal evidence](../browser-removal.md).


Current status: explicitly approved by the user and integrated in isolated commit
`2ef619c0704025512d9f4d4f5290dec3940a7361`. The original review and candidate-only evidence below are retained
as history. Subsequent pending/unintegrated wording describes that earlier state.
Track upstream replacement/removal in [the maintenance register](../core-exceptions.md).
Combined-source acceptance now passes; see [integrated evidence](../v2-core-integration-evidence.md).
Final release qualification remains open.


Status: qualified proposal ready for review; not approved or integrated. Active core
sources do not include this proposal; the separately approved cache, WAL sync and
FTS backing-storage fixes are recorded in the provenance inventory. The candidate
was qualified in `/tmp/romedb-wasm-fts-review`.

## Gap and proposed change

The pinned engine excludes its FTS index method, scalar functions and optimizer
rewrites on all WASM targets. RomeDB's managed full-text API therefore cannot
operate in a browser even when the frontend compiles. A scan substitute would
violate the indexed-search contract.

The [candidate patch](fts-wasi.patch) introduces the opt-in `fts_wasi` feature,
which enables `fts`. Existing non-WASM conditions remain equivalent. WASM FTS
becomes available only with this additional feature on WASI. It adds a WASI
Tantivy dependency pinned to the already locked 0.26.1, without its mmap default
feature; stopwords, stemming, LZ4 and columnar Zstd remain enabled. The existing
Turso-backed directory, ranking, transaction behavior and index format are used
unchanged. Enabling `fts` alone still leaves the WASM FTS implementation excluded
(its optional dependency is resolved for WASI, but the engine API is gated).

The patch changes cfg attributes and Cargo feature/dependency wiring in ten core
files. It changes no FTS algorithms, cache behavior, storage implementation or
vendor sources. RomeDB would additionally opt in through its WASI-only core
dependency after approval; the exact [frontend wiring patch](fts-wasi-frontend.patch)
is supplied separately. The old broad
[experiment](../probes/fts-wasm-experiment.patch) is retained as historical
evidence and must not be applied as the proposed change.

## Required host contract

The qualified target is `wasm32-wasip1-threads`, Rust 1.88.0 and WASI SDK 33.0.
The browser host must provide shared memory, WASI threads, exception-enabled C++
libraries and a JavaScript coordinator that remains responsive while Rust work
runs. The reactor must link `crt1-reactor.o` and execute `_initialize` exactly
once before dispatch. Merely exporting a worker start function is insufficient.

The prototype dispatches the frontend work to a Rust thread. Its responsive JS
coordinator services nested Tantivy thread requests. A four-worker pool produces
an explicit segment-merge thread allocation error; eight workers passed the
initial lifecycle fixture in both Chromium and Firefox. Eight is evidence for
that fixture, not a general concurrent-query capacity promise. Final client
capacity limits, resource errors and cancellation remain V2-C requirements.

The shared full-text fixture covers index creation, deterministic ordering,
insert/update/delete maintenance, savepoint and transaction rollback, two
connections with a reader snapshot held across writer commit, stable scores
inside that snapshot, integrity audit, index recreation and collection deletion.
It runs natively and through the browser reactor. All database handles are
released before the host receives completion, and the harness checks that no
Rust workers remain active.

## Review and integration

[RomeDB-Workflow.md](../../../../RomeDB-Workflow.md) requires:

> Any necessary local core exception requires a separately reviewed design
> decision, isolated commit, patch inventory entry, and relevant regression tests;
> it is not automatically authorized by this workflow.

The previous approval covered the native FTS cache fix. This is a separate core
exception. After review, integrate the exact qualified patch in an isolated
commit with provenance and the shared regression fixture, enable the target
feature in RomeDB, and rerun combined checks. The scalar-error transaction patch
is a separate pending decision and is not included here.

No public browser binding, OPFS persistence, crash recovery, installed browser
package or complete V2 release qualification is established by these probes.

## Qualification evidence

Both patches apply cleanly to the active tree with `git apply --check`.
At this qualification, the temporary and active Cargo.lock files were identical; no further dependency
version, source or checksum changes are needed. Lock SHA-256:
`620174c2e2d1b2a37bdfa3bf02e37cbdad0f08e24d3bee429742edd33ee93290`.

- Core patch SHA-256:
  `b6771224bade4381592906cbc16962add85819f1881e56860bc02a10a992d881`.
- Frontend feature wiring SHA-256:
  `68adbabbe59c6c2b6b160aa3b12f046e8af17bef2aad6f790d1bddb9b9efa0a1`.
- Candidate reactor passes the full shared fixture in Chromium and Firefox,
  peak eight Rust workers and zero left active; see [runtime evidence](../browser-removal.md)
  for exact artifact hashes, tools, commands and logs.
- The active-core reactor passes the base fixture in both browsers and fails the
  full-text control with `unknown module name 'fts'`, also leaving zero active
  workers. The native shared full-text fixture passes.
- Candidate native engine tests: 26 `index_method::test_fts` integration tests
  and the separate `index_method::fts_rolled_back_optimize_does_not_leak_segment_state`
  regression pass. Logs: `/tmp/romedb-wasm-fts-native-suite.log` and
  `/tmp/romedb-wasm-fts-native-core.log`.

The candidate also passes all six managed full-text integration tests, covering
ranking/plans, persistence, WAL recovery, rollback, malformed queries and atomic
failed writes (`/tmp/romedb-wasm-fts-frontend-suite.log`). Active-source frontend
Clippy with all targets and warnings denied passes after adding the reactor and
shared fixture (`/tmp/romedb-wasm-reactor-native-clippy.log`). Rust formatting,
JavaScript syntax checks and `git diff --check` pass. These focused checks do not
replace combined V2 acceptance, and no candidate-only feature is integrated.

Reproduce the candidate in a separate checkout with these RomeDB sources:

```sh
git apply romedb/docs/proposals/fts-wasi.patch
git apply romedb/docs/proposals/fts-wasi-frontend.patch
cargo test --locked -p core_tester --test integration_tests index_method::test_fts
cargo test --locked -p core_tester --test integration_tests index_method::fts_rolled_back
cargo test --locked -p romedb-tests --test fulltext
# Use the SDK/libclang environment documented in v2-wasm-probe.md.
bash romedb/scripts/build-wasm-probe.sh wasm_async_probe
```

The WASM build emits existing target-specific unused/dead-code warnings from the
upstream core. No unrelated upstream cleanup is included. Native integration
compilation emits the existing unused-import warning from upstream sync code.

## Integration readiness recheck, 2026-09-25

Both unchanged patches pass `git apply --check` on active HEAD
`12109384ad868dd673a572305d45a816baa2e545`, after the approved WAL and FTS
backing-storage fixes. Current Cargo.lock SHA-256 is
`0dcbf5a9c5ead00c327c86d2a3fca97881ed3de9004e0d30f14a7b6b2f90f5b8`;
the lock hash above belongs to the earlier candidate qualification. Applicability
does not establish runtime acceptance on this base. The old temporary directory
remains but is no longer a Git checkout. Reconstruct its source before further
candidate testing; the public browser client still excludes FTS pending approval,
integration and installed-client qualification.
