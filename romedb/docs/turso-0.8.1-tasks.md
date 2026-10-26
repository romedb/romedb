# Turso 0.8.1 integration checklist

Scope: user-requested embedded upgrade from 0.7.2; preserve current RomeDB work,
SQL/document contracts, native clients and approved engine exceptions. Cloud
source/deployment and release publication are excluded.

- [x] Read release notes, fetch tag and inspect clean source checkout.
- [x] Create `sync/turso-0.8.1` from `ab3a465f7` with an ancestry-preserving merge.
- [x] Resolve upstream integration and review all eight engine exceptions.
- [x] Adapt engine values, parser AST and execution APIs to 0.8.1.
- [x] Qualify explicit concurrent transactions and transactional managed FTS.
- [x] Expose/test new SQL language and optimizer capabilities through RomeDB.
- [x] Verify old databases, FTS migration, rollback, cancellation and recovery.
- [x] Run scoped formatting, Clippy, native tests and binding smoke checks.
- [x] Record exact provenance, exception decisions and test evidence; audit CI.
- [x] Install latest stable Go/.NET and Linux Swift; run remaining native SDK checks.
- [x] Include the previous-release database fixture in Git and rerun its CI regression.

Upstream tag object: `664022bdc83c2c1a5d245605388843693e3d0142`.
Release commit: `8549c16595d2faf1bdd6ee24aee0be8bfabb3d4a`.
The original checkout branch is `feat/cloud-cli-0.3`; all its committed work is
included, including metering and CLI work. Only RomeDB CI may remain active.

## Validation environment and receipts

Linux x86_64; Rust 1.88.0; Node 24.19.0; Python 3.12; pnpm 11.23.0.
Default debug profiles and the manifests' selected features; no workspace-wide,
all-features, release or browser qualification. Lockfile SHA-256:
`16c3d2397f7d50bf03966506f42c13f77aa946516d02542966c5a51187bf1061`.

| Check | Result / receipt |
|---|---|
| Scoped format, all eight RomeDB packages | Passed (`/tmp/romedb-upgrade-format-final.log`) |
| Scoped Clippy, all eight packages, all targets, no deps, deny warnings | Passed (`/tmp/romedb-upgrade-clippy2.log`) |
| Full eight-package Rust test run | 830 passed; two frontend unit follow-ups below (`/tmp/romedb-upgrade-full3.log`) |
| Frontend unit sweep after callback-oracle update, single test thread | 89 passed; all 832 scoped Rust tests resolved (`/tmp/romedb-upgrade-unit4.log`) |
| Native index-method suite | 131 passed, including FTS concurrent writers, snapshot isolation, recovery and the new savepoint-pin regression |
| Native trigger / integrity / external API suites | 98 / 36 / 14 passed |
| Native statement lifecycle / savepoint filter / first FULL commit | 46 / 12 / 1 passed |
| Standalone Rust consumer, offline path dependency | Passed; 341 registry/git identities remain within the workspace lockfile (`/tmp/romedb-upgrade-rust-consumer4.log`) |
| Node sync/worker, ownership and task-tracker suites | 121 passed in full run; final bounded-source fixture test passed separately: 122 resolved (`/tmp/romedb-upgrade-node2.log`, `node-write-retry2.log`) |
| pnpm-packed Node consumer installation, offline | Passed (`/tmp/romedb-upgrade-node-package3.log`); development addon only, not release packaging qualification |
| Node declaration checking | Passed (`/tmp/romedb-upgrade-types2.log`) |
| C ABI / Python / PHP | 5 tests / 8 tests / 42 contract steps passed |
| Go 1.27.1, race detector enabled | 42 native contract steps passed (`/tmp/romedb-upgrade-go.log`) |
| C#, .NET SDK 10.0.401, runtime 8.0.31 | 42 native contract steps passed (`/tmp/romedb-upgrade-dotnet8.log`) |
| Same C# net8.0 assembly, runtime 10.0.12 | 42 native contract steps passed (`/tmp/romedb-upgrade-dotnet10.log`) |
| Swift 6.4.0, Ubuntu 24.04 x86_64 | 42 native contract steps passed (`/tmp/romedb-upgrade-swift.log`) |
| Turso upgrade regression after tracking migration fixture | All six passed (`/tmp/romedb-upgrade-fixture-followup.log`) |
| Raw engine probes | WAL first-write sync, FTS integrity/drop lifecycle, and 18 scalar-error transaction scenarios passed |
| RomeDB package identity check | Eight crate identities remain consistent; engine manifest points to the new SHA with development status |

The full Rust run's two follow-ups were a test-only callback-count oracle that
assumed identical relational/document plans and a JavaScript setup deadline hit
during parallel compilation. Callback tests still compare native results,
require no evaluation during EXPLAIN, check LIMIT 0, and compare normal/profile
execution counts. The final serial unit sweep verifies the complete unit suite.
The native integrity harness retains the newer rusqlite security baseline; its
SQLite oracle now accepts the exact datatype constraint that newer SQLite emits
while evaluating an invalid STRICT generated column. Turso assertions are intact.

Full scoped command:

```sh
cargo test --locked -p romeql-parser -p romedb -p romedb-cli -p romedb-tests \
  -p romedb-node -p romedb-python -p romedb-protocol -p romedb-c --no-fail-fast -j 8
cargo test --locked -p romedb --lib -j 8 -- --test-threads=1
```

Native engine commands use `cargo test --locked -p core_tester --test
integration_tests <filter>` with `index_method::`, `trigger::`,
`integrity_check::`, `external_apis::`, `savepoint` and `first_full`; lifecycle
uses `cargo test --locked -p turso_core --lib statement_lifecycle_tests`.
Core receipts are `/tmp/romedb-upgrade-core-{fts2,trigger,integrity2,external_apis,
savepoint,first_full,lifecycle2}.log`.

The remaining native SDK checks were completed on 2026-09-30 against the same
upgraded debug `libromedb_c.so`. The adapters required no changes. Go and .NET
archives were verified against the official release metadata checksums; the
Swift 6.4.0 Ubuntu 24.04 archive passed PGP signature verification. Swift reports
`Swift version 6.4 (swift-6.4-RELEASE)`. Its missing `libncurses6` prerequisite was
extracted from Ubuntu's package into the user-local Swift toolchain library path.

Go, Swift and .NET are installed under the user's home directory; their PATH and
DOTNET_ROOT configuration is loaded by `.profile` and `.zshrc` from
`~/.config/romedb/native-toolchains.sh`. The C# project retains its `net8.0`
target. Both its supported .NET 8 runtime and the latest .NET 10 runtime were
exercised explicitly. Go/Swift copies of the C header match the canonical header.
Binary release qualification/publication is separate.

Additional native SDK commands (from the repository root with the installed
compilers on PATH):

```sh
export ROMEDB_NATIVE_DIR="$PWD/target/debug"
export LD_LIBRARY_PATH="$ROMEDB_NATIVE_DIR${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export ROMEDB_LIBRARY="$PWD/target/debug/libromedb_c.so"
export ROMEDB_FIXTURE="$PWD/romedb/bindings/fixtures/native-client.json"
export CGO_ENABLED=1 CGO_LDFLAGS="-L$PWD/target/debug"
(cd romedb/bindings/go && go test -race -v ./...)
DOTNET_CLI_TELEMETRY_OPTOUT=1 dotnet run --project romedb/bindings/csharp/Tests -- "$ROMEDB_FIXTURE"
dotnet --fx-version 10.0.12 romedb/bindings/csharp/Tests/bin/Debug/net8.0/Tests.dll "$ROMEDB_FIXTURE"
(cd romedb/bindings/swift && swift test -j 2 -Xlinker "-L$ROMEDB_NATIVE_DIR" -Xlinker -rpath -Xlinker "$ROMEDB_NATIVE_DIR")
cargo test --locked -p romedb-tests --test turso_081
```

The first PR CI run failed because the root `*.db` ignore rule excluded the
migration fixture from Git. The follow-up tracks that exact immutable fixture
(SHA-256 unchanged from its receipt) with a directory-local ignore exception,
so a fresh checkout includes the `include_bytes!` input. All six upgrade tests
passed again locally after this correction.

## Dependency and workflow review

Preserved earlier security updates: rustls 0.23.45, aws-lc-rs 1.18.1,
aws-lc-sys 0.45.0, crossbeam-epoch 0.9.20, memmap2 0.9.11, rusqlite 0.40.2,
and rquickjs 0.13.0. Tantivy advances to upstream 0.26.2. No retained package
was silently downgraded to the release tag's older security baseline.

Only `romedb-ci.yml` remains active. Newly imported inherited
`publish-serverless-python.yml` and `serverless.yml` were relocated to
`.github/upstream-workflows/` before push. Exact upstream CI links and the
per-exception retain/adapt/remove decisions are in [UPSTREAM.md](../UPSTREAM.md)
and [the exception register](core-exceptions.md).

The old database fixture comes from the actual published RomeDB 2.1.0 binary;
[its receipt](../tests/fixtures/README.md) records both archive and database hashes.
The user selected upstream rejection of trailing DML LIMIT and separately
approved [the exact FTS cursor patch](proposals/turso-081-fts-savepoint.md).

## Committed source identity

- Ancestry-preserving upstream merge: `8b3ef9a43e0e94bac05d3a26a305ee64324329af`.
- Separately approved FTS core fix and raw regression:
  `8b1e54e2796e71b39f8061f178f685bfdd25c3d2`.
- The tested combined implementation is the latter commit. Follow-ups record
  SDK evidence and include the previously ignored immutable migration fixture;
  they do not alter the engine or client implementation. Both original RomeDB ancestry
  and the exact upstream release commit are ancestors; merge the sync PR with
  a merge commit, never squash/rebase-merge it.
