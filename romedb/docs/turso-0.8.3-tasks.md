# Turso 0.8.3 sync checklist

Scope: user-requested engine sync from 0.8.1 to 0.8.3 (2026-10-20). Keep RomeDB
behavior, catalog version and all approved engine exceptions. Release, Cloud
source and deployment are not part of this sync. Notes: [turso-0.8.3.md](turso-0.8.3.md).

- [x] Fetch `v0.8.3` (`607bff9fd0e547a25a97b3bf06e83bcc5d99d53c`); read CHANGELOG and diff; check upstream CI for the exact commit.
- [x] Create `sync/turso-0.8.3` from `main` (`725b25550`) with an ancestry-preserving merge (`122f8f640`). No conflicts.
- [x] Workflow audit: only `.github/upstream-workflows/fuzz.yml` changed; `romedb-ci.yml` is the only active workflow.
- [x] Review all eight engine exceptions against 0.8.3 (all kept; see the register).
- [x] Scoped format, Clippy and tests for all RomeDB packages.
- [x] Native engine suites that cover the exceptions.
- [x] Metering, MVCC, WAL and checkpoint suites; new `turso_083` regressions.
- [x] New upstream tests that touch RomeDB behavior.
- [x] Previous-release upgrade/restore checks, including released 2.9.0.
- [x] All seven native clients.
- [x] RustSec review and regenerated notices.
- [x] Update UPSTREAM.md, release.json engine base and the exception register.

## Environment

Linux x86_64 (WSL); Rust 1.88.0; Node 24.19.0; Python 3.12.3; Go 1.27.1;
.NET SDK 10.0.401; Swift 6.4.0; PHP system package. Debug builds, run through
`scripts/romedb-heavy`. Lockfile SHA-256:
`0b9b2b4b75070342867d0809048e72137929fb2b319521b287da5f13d16480c7`.
Logs were kept under `.local/logs/turso-0.8.3/` in the planning directory.

## Results

| Check | Result |
|---|---|
| `cargo fmt --check`, nine RomeDB packages | Passed |
| `cargo clippy --all-targets --no-deps -D warnings`, nine RomeDB packages | Passed |
| `cargo test -p romeql-parser -p romedb -p romedb-cli -p romedb-service -p romedb-tests` | 1104 passed, 0 failed, 4 ignored (111 test binaries) |
| New `turso_083` | 4 passed |
| `turso_081` (previous-release fixture, manual MVCC recovery) | 7 passed |
| Metering: `profile` / `execution_meter` / `mutation_meter` / `schema_meter` / `index_method_meter` | 4 / 14 / 6 / 8 / 4 passed |
| Metering: nine `metered_*` suites | 44 passed |
| `wal_replication` / `checkpoint_barrier` / `checkpoint_crash_atomicity` | 2 / 5 / 2 passed |
| `fulltext` / `fts_analysis_hook` / `integrity` | 8 / 3 / 2 passed |
| Native `index_method::` / `trigger::` / `integrity_check::` / `external_apis::` | 131 / 98 / 36 / 14 passed (same as 0.8.1) |
| Native `savepoint` / `first_full` / statement lifecycle | 12 / 1 / 46 passed (same as 0.8.1) |
| Upstream new: `storage::header_version` (includes in-transaction MVCC switch) | 18 passed |
| Upstream new: `turso_core` `mvcc::database::tests` | 411 passed, 6 ignored |
| Upstream new sqltests `mvcc-repeated-index-seek` / `mvcc_sequence`, WAL and MVCC | 4 / 19 passed in each mode |
| `check-upgrades.py`, 1.0.0 to 2.8.0 rows, Node 24 | 14 upgrade/restore checks passed |
| Released 2.9.0 package to this engine (`check-v251-upgrade.cjs`) | Passed: backups unchanged, indexed results match scans, 2.9.0 reads upgraded file, reopen, independent restore |
| Node (`check-node.sh`) | 191 passed; declarations checked |
| C ABI / PHP | 5 tests / 110 contract steps passed |
| Go (`-count=1 -race`) / C# / Swift | 110 / 110 / 110 contract steps passed |
| Python (debug wheel, `check-python-wheel.py`) | 9 tests passed |
| RustSec (advisory-db `999ec89d`, cargo-audit 0.22.2) | 359 shipping packages, 0 unresolved; RUSTSEC-2026-0253 informational, accepted as before |
| Notices (`prepare-v2-notices.py`) | Regenerated; only the lockfile hash and four `turso_*` versions changed; license files unchanged |

The 2.9.0 check used a scratch copy of the development Node package labeled
2.9.1, because the script requires a newer version. The repository version
stays 2.9.0. The Node 22 runtime was not installed, so upgrade checks ran on
Node 24 only. Release qualification must run both required majors.

## Dependency review

The merge changed only the 43 `turso_*` and related workspace crate versions in
`Cargo.lock` (0.8.1 to 0.8.3). Earlier security updates stay above the upstream
tag: rustls 0.23.46, aws-lc-rs 1.18.1, aws-lc-sys 0.45.0, crossbeam-epoch 0.9.20,
memmap2 0.9.11, rusqlite 0.40.2, rquickjs 0.13.0. Tantivy is 0.26.2 on both sides.

## Not done here

- No RomeDB release. Shipping the new engine needs a version decision and full
  release qualification (production profile, both Node majors, three Pythons).
- No Cloud change. Cloud picks up the engine from a RomeDB release tag.
