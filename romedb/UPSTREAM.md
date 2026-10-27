# Engine provenance

- Repository: https://github.com/tursodatabase/turso (remote `upstream`).
- Baseline: release `v0.8.3`, full commit `607bff9fd0e547a25a97b3bf06e83bcc5d99d53c`.
- Previous base: `v0.8.1`, commit `8549c16595d2faf1bdd6ee24aee0be8bfabb3d4a`; before that `v0.7.2`, commit `046e9cbf67d22491e8ecc941ec2891b02a9f3cad`.
- Upgrade behavior and validation: [0.8.3 notes](docs/turso-0.8.3.md) and
  [checklist](docs/turso-0.8.3-tasks.md); the 0.8.1 [integration notes](docs/turso-0.8.1.md)
  and [task checklist](docs/turso-0.8.1-tasks.md) remain the record of the larger
  0.7.2 upgrade; [core exception review](docs/core-exceptions.md).
- Follow-up override audit: [release and current-main comparison](docs/core-exception-audit-2026-09-30.md).
- RomeDB fork: `git@github.com:romedb/romedb.git` (remote `origin`). Upstream ancestry is preserved; inspect current refs rather than relying on historical branch names.
- Toolchain: upstream Rust 1.88; locally tested with 1.88.0 on Linux x86_64.
- Engine dependency: in-workspace `turso_core` with default features plus `conn_raw_api` for engine WAL hooks and `fts` for V2 full-text search. V2 opens opt into index methods; managed search owns the public surface.
- This release exposes `Connection::prepare`, `prepare_stmt`, and blocking statement execution. RomeDB retains its separate-frontend boundary and the explicitly registered core exceptions.

Upstream check-runs inspected for the exact 0.8.3 release commit `607bff9fd`:
[Release](https://github.com/tursodatabase/turso/actions/runs/38264553029),
[Publish Crates](https://github.com/tursodatabase/turso/actions/runs/38264553018),
[Run long fuzz tests and stress test](https://github.com/tursodatabase/turso/actions/runs/38264552904),
[C compat Tests](https://github.com/tursodatabase/turso/actions/runs/38264552996),
[Python](https://github.com/tursodatabase/turso/actions/runs/38264552938),
[Dotnet Tests](https://github.com/tursodatabase/turso/actions/runs/38264552961) and
[Java Tests](https://github.com/tursodatabase/turso/actions/runs/38264552953)
succeeded. Like the 0.8.1 runs below, they cover upstream packaging and suites
only, not the combined fork.

Upstream check-runs inspected for the exact 0.8.1 release commit:
[Linux x64 Node 24 database bindings](https://github.com/tursodatabase/turso/actions/runs/36564600099/job/109396698178),
[Linux x64 Node 24 sync bindings](https://github.com/tursodatabase/turso/actions/runs/36564600099/job/109396698143), and
[Linux x64 .NET NativeAOT](https://github.com/tursodatabase/turso/actions/runs/36564670411/job/109404264594)
succeeded. These support upstream packaging/API coverage only; they are not
RomeDB tests or evidence of full engine conformance on the combined fork.

Local integration changes:

1. Eight explicit RomeDB workspace members and corresponding lockfile entries; upstream default-members unchanged.
2. RomeDB implementation, tests, scripts, and documentation under `romedb/`.
3. Inherited workflow YAML files moved unchanged into `.github/upstream-workflows/` so GitHub cannot execute them. Only `.github/workflows/romedb-ci.yml` remains active. Audit this directory on every upstream merge before pushing.
4. Approved engine exceptions are recorded below; upstream parser, bindings and CLI implementation files remain unchanged. The frontend also directly depends on the pinned workspace parser/extension crates for AST lowering and statically linked pure accessors; registration uses the documented unsafe startup extension context API, which is freed before exposing the connection.

The sections below preserve the original exception approvals and release
history. Their old version/dependency statements describe those releases.
For the current tree, the 0.8.3 sync decisions (all exceptions retained), the 0.8.1 exception table and upgrade checklist linked
above are authoritative: the former FTS cache and first-FULL patches are
retired, Tantivy is 0.26.2, and the approved backing-cursor fix is active.

## Approved core exception: trigger interruption

The user explicitly approved candidate 660a61afb947121fa8abf346ed600c780dfe123c
on 2026-09-14. It was integrated unchanged as ded389aea. The isolated patch in
core/vdbe/execute.rs preserves StepResult::Interrupt through OpProgram while
retaining Busy for actual contention and saving subprogram state in both cases.
The frontend cannot safely distinguish these outcomes after the old mapping.
No storage format or public API changes are introduced.

The previously ignored after-write trigger-cancellation regression is enabled.
The isolated candidate passed 98 pinned upstream trigger tests and its full
RomeDB scoped suite; see [review evidence](docs/trigger-interrupt-review.md).
Combined-current-source verification is recorded separately after integration.
On upstream upgrades, check whether the fix is present before removing this
exception; preserve the regression and revalidate affected trigger behavior.

The root planning directory is not a Git repository. The ancestry-preserving checkout lives in `turso/`; product source is `turso/romedb/`.

The separate RomeDB native addon reuses pinned napi 3.8.3, napi-derive 3.5.2 and napi-build 2.3.1 from the existing lockfile. It depends on RomeDB rather than the upstream Node binding and enables FTS through the RomeDB frontend rather than the upstream Node binding. The frontend embeds pinned rquickjs 0.13.0 / QuickJS-NG 0.16.2 for its bundled string catalog and sandboxed functions. The 2.1 security review upgrades the formerly shipped rquickjs 0.12.2 / QuickJS-NG 0.15.1; see [dependency review](docs/dependency-security.md).

The frontend enables serde_json's float_roundtrip feature to prevent one-bit numeric changes when reading stored tagged values. This changes a feature of the combined build, without upgrading the dependency or changing the stored format.

The RomeDB CLI directly depends on Rustyline 15.0.0 already pinned in the upstream lockfile, with default features disabled and file history enabled. The lockfile adds only that dependency edge; upstream CLI source and dependency versions are unchanged.

The CLI's Unix SIGINT listener also uses signal-hook 0.3.18 already pinned in the upstream lockfile. Its dependency edge is RomeDB-only; no upstream dependency versions or implementation files change.

The frontend directly uses stacker 0.1.22, already pinned in the upstream lockfile, to give SQL execution, profiling, audits, parser/preparation and row-execution calls an auxiliary stack when the caller has less than 16 MiB available. Growth requests 32 MiB on the same thread. Only the RomeDB dependency edge is added; engine features, upstream implementation files and dependency versions are unchanged. This addresses observed debug-build parser stack exhaustion before the upstream depth guard, not general execution memory qualification.

## Approved core exception: FTS cache snapshot isolation

Approved by the user on 2026-09-25 after review of
[the design and control/fix evidence](docs/proposals/fts-cache-snapshot.md).
`core/index_method/fts.rs` checks pager identity before reusing its shared
Tantivy directory cache. A cache belonging to another connection is reloaded
through the requesting pager; same-pager reuse retains rollback validation.
This prevents uncommitted hit membership and scores from escaping the writer
and preserves reader snapshots across commit. No storage format or API changes.

The isolated change includes `test_fts_cache_preserves_connection_snapshots`
in the existing native index-method integration suite. Alternating connections
may reload the directory more often. On upstream upgrades, retain the regression
and check for an equivalent cache ownership fix before removing this exception.

V2 dependencies: h3o 0.9.4 without default features implements H3 cells;
Tantivy 0.26.1 and query grammar 0.26.0 use the existing workspace pins. The
frontend's FTS feature and query-grammar dependency add no version upgrades.

V2 ANN qualification adds USearch 2.26.2 (default features disabled) through its
Rust/C++ API and SHA-256 graph checksums with sha2 0.10.9. Graph snapshots and
redo records use ordinary engine transactions; no ANN core hook or external
index sidecar is added. C++ compilation, platform support and source attribution
remain part of the V2 release checks. See `docs/v2-ann.md`.

The V2 Python binding adds a sixth RomeDB workspace member. It reuses PyO3 0.29.0
already pinned upstream, with its CPython 3.10 stable ABI and extension-module
features. Only the new RomeDB package is added to Cargo.lock; no existing crate
version changes. Maturin 1.12.6 is pinned in its Python build metadata. Native
source remains under `romedb/bindings/python`; upstream Python bindings are
unchanged. See `docs/v2-client-assessment.md` for installed-wheel evidence and
the browser feasibility probe.

The following browser-stage history is retained as provenance; browser/WASM
support and its build gates were removed before the native 2.0.0 release.

The historical threaded-WASI probe added a target-specific rquickjs-sys 0.12.2 bindgen feature
under the frontend, pinned bindgen 0.72.1 in Cargo.lock, and no existing crate
version upgrades. Native features remain unchanged. WASI SDK 33.0 and libclang
18.1.1 are external build tools; C++ exceptions require the SDK's exception-enabled
libraries. Core and vendor sources remain unchanged. See `docs/browser-removal.md`
for real-browser dependency checks and the remaining full-text/worker/I/O gates.

The removed browser client added `romedb-protocol` and `romedb-browser` as explicit workspace
members. Python and browser use the same request/typed-value protocol; the Python
extension retains its existing PyO3 lifecycle and cancellation boundary. These
two packages add no external registry/git package identities, versions or checksums
relative to the WASI probe lockfile. Historical browser-stage Cargo.lock SHA-256:
`0dcbf5a9c5ead00c327c86d2a3fca97881ed3de9004e0d30f14a7b6b2f90f5b8`.
The browser uses WASI SDK 33 with a development Rust profile at optimization level
1, assertions retained, no LTO and no debug symbols. JavaScript support libraries
are pinned separately by the browser package's pnpm lockfile.

The browser qualification also reproduced partial ordinary SQL writes after
cancellation inside a caller transaction. The frontend now supplies a private
savepoint for ordinary INSERT/UPDATE/DELETE in existing caller scopes, rolling
back on cancellation and preserving native conflict-error dispositions. Native
oracle checks include rows, transaction state, changes() and last_insert_rowid().
This change uses public engine APIs and does not modify upstream sources. The scalar-error transaction and WASI FTS exceptions were subsequently approved
and integrated; see the maintenance register below. See [browser client evidence](docs/browser-removal.md).

The historical browser OPFS adapter implemented the existing public engine `IO`
and `File` traits and used `Database::open_with_io`. Dedicated browser workers owned
exclusive database/WAL handles, mapped sync to flush, and returned synchronous I/O
completions to Rust. That adapter added no core/storage-format change or dependency.
See [OPFS qualification](docs/browser-removal.md) for the fault/platform gates at
that stage. Neither that browser qualification nor WASI FTS is a current release gate.

## Approved core exception: first-commit FULL-mode WAL sync

The OPFS fault matrix found a native FULL-mode first-commit sync omission. The
user approved the [separate review](docs/proposals/wal-first-commit-sync.md) on
2026-09-25. Isolated commit `fe2ccd404e005a198fac5365712cacaa54d47a2c` applies the
reviewed patch unchanged: the pager includes unpublished prepared frames in its
FULL-mode sync decision, with a native WAL regression. Its two-file diff SHA-256
is `72ed0afb451277c9406cc0a9b65b7867e7e8d47105296ce1c011476dee994f2d`.
No public API or storage format changes. NORMAL/OFF behavior remains unchanged.
Keep the regression and check for an equivalent fix during upstream upgrades.
Candidate evidence: 86 native passes, one existing ignored test, and 11 OPFS
fault cases in each browser. The integrated native sync probe and installed
browser package also pass, including all 11 fault cases in each browser; see
[active-source evidence](docs/v2-wal-durability-evidence.md). The scalar-error
and WASI FTS exceptions are now integrated as recorded below.

## Approved core exception: FTS backing-storage integrity and cleanup

V1 upgrade qualification reproduces false row/index-count comparisons for FTS
backing trees and unreclaimed backing pages after table/index teardown. The
[separate candidate](docs/proposals/fts-integrity.md) preserves physical checks,
fixes teardown enumeration, and passes affected native suites plus the complete
V1 upgrade/restore rehearsal. Its three-file patch SHA-256 is
`0a769b063076b4d110d6e75e0fcd334254663e89bb4dbe2e055da585ed9c474b`.
The user separately approved it on 2026-09-25. Isolated commit
`12109384ad868dd673a572305d45a816baa2e545` applies the exact reviewed patch; its
three-file diff has the same SHA-256. The integrated integrity, index-method and
drop-table suites pass (64 distinct tests), as does the complete native V1
upgrade/restore rehearsal. Core Clippy with warnings denied also passes. See
[active evidence](docs/v2-upgrade-restore-evidence.md).
This approval was separate from the later scalar-error and WASI FTS approvals. Existing orphaned pages from older development builds are not repaired
automatically; preserve the physical-corruption and drop/rollback regressions
when reviewing future upstream replacements.

## Approved core exception: checkpoint WAL barrier and failed-sync retry

The user approved the exact six-file
[checkpoint correction](docs/proposals/checkpoint-wal-sync.md) on 2026-09-25.
The 2.1 candidate integrates upstream's
[`cc26d08508cbe045472fa3015e2bce4a389b5e06`](https://github.com/tursodatabase/turso/commit/cc26d08508cbe045472fa3015e2bce4a389b5e06)
checkpoint barrier plus the reviewed local failed-completion retry and automatic
checkpoint cleanup. The combined core patch SHA-256 is
`5154633cc2c1187efa0b3066c008ac46f1f999286fb48ccacf3a1a7743da7d88`.
Isolated integration commit: `5f4133d732eb2a297bf32af2f7a3fd3c305be8c5`.
It changes `core/storage/wal.rs`, `core/storage/pager.rs`, `core/vdbe/mod.rs`,
`core/vdbe/vacuum.rs`, `core/mvcc/database/mod.rs` and
`core/mvcc/database/checkpoint_state_machine.rs`.

After selecting the backfill range under checkpoint locks, the engine syncs the
WAL before writing database pages unless no frames need backfill or the effective
mode is OFF. It retains the pending barrier until successful completion; a
failed asynchronous completion cannot let a retry bypass WAL sync. Cleanup is
limited to the completed failed barrier before database backfill begins.
Automatic checkpoint failures also complete bookkeeping for the already
published transaction instead of entering ordinary writer rollback. This
exception changes no file format, SQL syntax or dependency version.

Permanent [barrier and retry tests](tests/tests/checkpoint_barrier.rs) cover OFF,
empty checkpoints, explicit PRAGMA, direct blocking and automatic checkpoint
failure/retry, including immediate and deferred failed completions. The
[crash-model tests](tests/tests/checkpoint_crash_atomicity.rs) require complete
old/new state under NORMAL and preservation of acknowledged writes under FULL.
The seven permanent regressions pass on the integrated source. The isolated
candidate's affected checkpoint/VACUUM filters also pass; combined checks and
exact-artifact qualification remain pending. See the review's immutable
before/after evidence. Retain the
tests on every upstream sync and release; remove or adapt the exception only
when upstream supplies both the barrier and equivalent error/retry behavior.

## Approved scalar read errors and historical WASI FTS approval

The user explicitly approved both prepared proposals after reviewing the options.
The exact scalar-error patch is isolated in `f26014f04de4077a49268fd94c37ff9ad6ad6425`;
the exact opt-in WASI FTS core patch is isolated in
`2ef619c0704025512d9f4d4f5290dec3940a7361`. The separately reviewed frontend WASI
feature wiring was applied under `romedb/frontend/Cargo.toml` at that stage and
removed with browser support below. These patches introduced no dependency version
or storage-format changes. Candidate evidence is
in the respective reviews; integrated acceptance is recorded separately.

**Maintenance register:** [core exceptions](docs/core-exceptions.md) lists all
local exceptions, regression gates, and conditions for replacing or removing them.
Review every entry on every upstream sync. An upstream release note or a clean
patch application alone is not sufficient evidence to retain or remove a patch.

## Native-only release scope and browser removal

The user selected Rust, Node.js/TypeScript and Python clients and explicitly
requested removing browser support to reduce overhead. Isolated commit
`ae6777a17` reverts the WASI FTS core feature/cfg exception. The browser workspace
member, WASI QuickJS bindgen wiring, browser source/build/probe code and browser
release gates are removed. `romedb-protocol` stays because Python depends on it.
Five active core exceptions remained after removal. The approved 2.1 checkpoint
barrier/retry correction brought the active count to six. The separately approved named-savepoint
cancellation correction below brings the current count to seven. Earlier browser
sections above are historical, not current build instructions; see
[removal evidence](docs/browser-removal.md).

Historical browser-removal Cargo.lock SHA-256: `eccecb92bd14841e0f0980680eb051f365db9454cbade2212a8ffdd80d3c6443`.
The removal drops `romedb-browser` and bindgen 0.72.1, without adding/upgrading any
package identity. Native dependency declarations and notices are regenerated.

## Additional native language clients

The user requested PHP, Swift, C# and Go in place of browser support. The new
`romedb-c` member is a native ABI over the RomeDB frontend and shared protocol;
all wrapper code stays under `romedb/bindings/`. No upstream core source or
third-party Rust package changed in that addition. Historical client-addition lock SHA-256:
`1f64d71cc6cb57e691be4a7bc9b5a39ef4d00721515831813d1442aca2941aaa`.
See [client contracts and qualification](docs/native-language-clients.md).

The 2.1 dependency updates and current lockfile identity are recorded in the
[security review](docs/dependency-security.md) and its machine-readable receipt.
The checkpoint exception itself does not change Cargo.lock.

## Approved core exception: canceled-write savepoint recovery

The user separately approved the exact three-core-file patch on 2026-09-25.
Isolated commit `3ae0065e5` records the named-savepoint poison snapshot/restore
and five engine lifecycle regressions. SHA-256 of the exact core diff:
`4dea3c65be5cbb174721fb25eb164d29e9641681d7028b48bc6f77fd1958e122`.
After a canceled unjournaled write is undone, COMMIT/root RELEASE can preserve
earlier caller work. An earlier abandoned write still prevents commit when the
chosen savepoint did not undo it. RELEASE without rollback is unchanged.

The isolated candidate passes 41 lifecycle tests, 20 overlapping savepoint tests,
ten deterministic frontend cancellation/scope cases in one regression, formatting
and scoped core/frontend Clippy. Combined source and rebuilt package checks are
recorded separately. No file-format, dependency or public API changes occur.
The inspected upstream `64b8ef5742fc18937f9c89806c81e3f6475dc7a3` lacks this
state; review/remove criteria and immutable evidence are in
[the proposal](docs/proposals/cancellation-savepoint-poison.md) and the
[exception register](docs/core-exceptions.md).

## Approved core exception: execution metering

The user approved a narrowly scoped counter/budget exception for Cloud 0.3.0 on
2026-09-26. The first isolated change corrects missing row-visit counts after
successful cursor seeks and executed deferred table lookups. A second isolated
change adds opt-in retained execution counters and a VM-step budget; counters
survive errors/rollback and are physical work rather than committed billable
mutations. A third isolated change adds read budgets with one counted-visit
overshoot per serialized execution and incremental metered Count execution.
A fourth correction removes duplicate HashBuild charges for already-positioned
source rows, with memory/file and budget-boundary regressions. Complete cursor
coverage and checked frontend attribution remain open. These
patches do not enable paid billing. See [the design and regression contract](docs/proposals/execution-metering.md)
and [the exception register](docs/core-exceptions.md). Work is on the separate
`feat/cloud-metering` branch based on released RomeDB 2.1.0.


Read-budget integration additionally routes a budget-requested interrupt through
the existing cooperative cleanup path, preserving prior explicit-transaction work
for interrupted readers. This is an isolated continuation of the approved metering
exception with raw-engine and checked-frontend transaction regressions.

Retained ordinary B-tree row mutation events and an optional mutation budget are
another isolated continuation of the approved exception. Replacement deletions
are distinct from update mechanics; internal CDC inserts are excluded without
changing SQL change counts. Trigger budget cancellation preserves caller work,
and interrupted writers must be finalized before meter replacement. Five focused
mutation regressions and the adjacent write/read/lifecycle suites pass. Retained
events survive rollback and are not committed billable writes; managed hidden
index and virtual-table attribution remain open. See the metering proposal.

The execution meter additionally supports a shared maintenance view suppressing
only logical mutation events. Reads, physical writes, VM work and all exhaustion
remain shared. The mutation-meter regression verifies scope switching cannot
bypass an exhausted budget. This is part of the approved metering exception.

The approved metering exception additionally provides opt-in engine schema-read
attribution and an independent schema-visit budget. Legacy counters/budgets retain
all-row semantics. Compiler catalog identity is used at existing completed cursor
visits, including deferred table reads and incremental Count; physical root-page
numbers alone do not classify temporary tables. Schema exhaustion uses existing
cooperative cancellation and shared maintenance state. Regressions are in
`romedb/tests/tests/schema_meter.rs`; design and remaining frontend/Cloud limits are
in `docs/proposals/execution-metering.md`. This foundation does not enable billing.

The same approved exception counts successful initial index-method positioning
after query_start completes I/O. Next already records later results. Empty
results do not count, and default profiling and retained read budgets receive
the same event. `romedb/tests/tests/index_method_meter.rs` covers score-only
versus deferred table lookups, memory/file databases, exact/crossing/sticky limits,
caller preservation, queued I/O and managed FTS materialization. Native extension
posting/traversal work before cursor output remains outside this correction.

The Turso 0.8.1 MVCC follow-up adapts those same Insert/Delete mutation hooks to
classify `sqlite_schema` through compiler catalog identity. MVCC cursor logical
table IDs are not physical root pages. The focused `schema_meter` regression
requires DDL to succeed with a zero customer-mutation budget, while INSERT,
CREATE TABLE AS SELECT and DELETE still count their real customer rows. This
changes only the existing execution-metering exception.

## Core exception: shared FTS analyzer inspection (2.2.0)

The user authorized engine updates for the RomeQL 2.2.0 roadmap. The additive
[reviewed hook](docs/proposals/fts-analyzer-inspection.md) extracts existing
registration from `core/index_method/fts/mod.rs` into sibling `analysis.rs`.
A public streaming visitor exposes borrowed token data from the same manager;
callers supply bounds and can stop with their own error. No storage format,
registration behavior or dependency changes. Three hook regressions and eight
existing full-text regressions pass; combined release checks remain pending.
The [exception register](docs/core-exceptions.md) records removal criteria.
