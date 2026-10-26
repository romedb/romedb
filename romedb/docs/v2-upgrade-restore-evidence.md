# V1-to-V2 database upgrade and restore qualification

Working-tree qualification on 2026-09-25; V2 is not released. The fixture is
created by the locally retained V1 package, not handwritten catalog metadata.
Its archive SHA-256 is
`d508c83e749bdfc6359f744b040414b5001fbb826e167e3ea624d60483f01d5f`, matching the
V1 bundle inventory and source `1824de044fbdcb9d183e77fe30421d0ba25f5001`.
The installed V1 addon hash is
`0830edfeb0674a8b589de03faf46a682b5de4378f96f3069ff23e046b232f96e`.
These are local artifact checks, not a fresh external release-state verification.

## Fixture and native gate

`romedb/scripts/check-v2-upgrade.cjs` runs each phase in a separate process. It
refuses an existing output directory and copies backups without overwriting.
The V1 writer creates required/CHECK fields, unique/nested/reference indexes,
relational rows and a view, exact-source migration history, two collections,
string/integer record IDs, int64 limits, negative zero/subnormal/precise binary64,
binary, all five vector encodings, references, nested objects/arrays and
missing/null values. It verifies its own expected values, requires a successful
zero-row-count truncating checkpoint and a zero-length WAL, then closes before
copying the main file. The exported values and inspection metadata become
independent V1 expectations for the subsequent clients.

The native harness then creates V2 spatial/ANN/full-text indexes and an inverse
relationship, tests schema and write rollback, close/reopen, V2 backup restore,
V1 downgrade rejection, and restoration from the untouched V1 backup. Additive
V2 inspection fields (`relations` and scalar-index `kind`) are explicitly checked
separately from the unchanged V1 definitions. Migration operations require
autocommit; the harness preserves that existing contract.

The pre-integration active-source run was **blocked by a reproduced native FTS integrity error**:
after creating the text index, `PRAGMA integrity_check` reports a wrong entry
count for its backing B-tree. The new standalone native probe also reproduces
unreclaimed pages after dropping the FTS index. The active core is unchanged by
the initial investigation. The [separate candidate](proposals/fts-integrity.md)
passed the full fixture and affected upstream checks, and the user approved it.
It is now integrated unchanged in `12109384a`; the active native upgrade/restore
rehearsal passes independently of the browser checks, as recorded below.

Active failure with the final harness: `/tmp/romedb-v2-upgrade-restore-active-final.log`.
Native probe: `/tmp/romedb-fts-integrity-native-control-final.log`.
Fixture directory: `/tmp/romedb-v2-upgrade-restore-04/`.
Its checkpointed `v1-backup.db` is **61,440 bytes**, SHA-256
`ad89aa03787aa40db52fcc94e5b8f92fa8d9977499bf427771ec5758cb2e6181`.
Earlier harness-development runs exposed expectation mistakes (integer
coordinates, additive inspection fields and unqualified nested-field SQL), not
product regressions; the final V1 seed verifies those expectations itself.

The isolated FTS backing-storage candidate passes all native fixture phases:
seed, upgrade, V2 reopen, V1 rejection with FTS present, removal of FTS on a
disposable copy, separate V1 catalog-version rejection, V2 backup restore/write/
reopen, and V1 backup restore/write/reopen. Both backup hashes remain unchanged
after all modifications to the working and restored copies.
Log: `/tmp/romedb-v2-upgrade-restore-candidate-final.log`.
Report/fixtures: `/tmp/romedb-v2-upgrade-restore-candidate-final/`.
Candidate addon SHA-256:
`46692c136ece0460b147eb299122538e5bf1380c57d317ce594234a6ca417d4d`.
Its V1 backup has the same hash as the browser fixture above; its V2 backup has
SHA-256 `4d92596859523e9d860625e8be0056fd53f39f0fd52eba340198daee67dacb2b`.
## Integrated native upgrade and storage checks

The user approved the FTS backing-storage fix, integrated unchanged in isolated
commit `12109384ad868dd673a572305d45a816baa2e545`. The active-source native
integrity suite passes **29 tests**, the index-method suite **31 tests**, and the
drop-table filter **5 tests**, all with zero failures/ignored tests. One drop-table
test overlaps the integrity suite. Logs:
`/tmp/romedb-fts-storage-integrated-integrity.log`,
`/tmp/romedb-fts-storage-integrated-index-method.log`, and
`/tmp/romedb-fts-storage-integrated-drop-table.log`.

The rebuilt active Node addon now passes the complete V1 upgrade/restore harness
above, including the separate FTS-module and catalog-version downgrade rejections.
Log: `/tmp/romedb-v2-upgrade-restore-integrated.log`.
Report and fixtures: `/tmp/romedb-v2-upgrade-restore-integrated/`.
Active debug addon SHA-256:
`e5549d9c8c451dc35f0bee63b2148b6e4ff02ae523de4cf2d193c3efa2109261`.
The V1 backup has the same hash as the original fixture; the V2 backup has
SHA-256 `16544f304dec09c6f3172a948d0f03b3b5675200dd1042bb42f57d5139a11e29`.
Both remain unchanged after writes to the original and restored working copies.

The full Node/application run has **113 passes, 2 failures, 0 skipped/cancelled**.
Both failures are the still-pending scalar-error transaction cases, unrelated to
this approved fix. Strict TypeScript was run separately after the test script
stopped on those failures and passes. Logs:
`/tmp/romedb-fts-storage-integrated-node.log` and
`/tmp/romedb-fts-storage-integrated-node-typecheck.log`.
Formatting checks pass for all RomeDB packages and the three touched upstream
files. Core Clippy with warnings denied also passes
(`/tmp/romedb-fts-storage-integrated-core-clippy.log`). The rebuilt standalone
native probe returns `ok` before and after FTS teardown
(`/tmp/romedb-fts-storage-integrated-probe.log`). The complete scoped Rust run
finishes with **736 passes, 2 failures, 0 ignored** across 64 test/doc-test results
(`/tmp/romedb-fts-storage-integrated-scoped.log`). Only `user_functions` fails:
`sandbox_limits_and_atomic_failure` and
`function_versions_follow_reader_snapshots_and_cancelled_writes_preserve_prior_work`.
These reproduce the pending scalar-error transaction issue. All other targets,
including managed FTS, pass. This is not a green combined V2 acceptance claim.

Commands from the repository root (Rust 1.88.0, dev profile):

```sh
cargo test --locked -p romeql-parser -p romedb -p romedb-cli -p romedb-tests --no-fail-fast
cargo clippy --locked -p turso_core --lib --features fts --no-deps -- -D warnings
bash romedb/scripts/check-node.sh
pnpm --dir romedb/bindings/node run typecheck
node romedb/scripts/check-v2-upgrade.cjs \
  /tmp/romedb-v1-upgrade-consumer/node_modules/@romedb/node \
  /home/tan/Sites/fastdb/turso/romedb/bindings/node \
  /tmp/romedb-v2-upgrade-restore-integrated
```

The Rust and Node test commands exit nonzero for the identified scalar-error
failures; the other commands pass. The affected upstream suites use the native
`core_tester` integration executable with `integrity_check::`, `index_method::`
and `drop_table` filters. Cargo.lock SHA-256 remains
`0dcbf5a9c5ead00c327c86d2a3fca97881ed3de9004e0d30f14a7b6b2f90f5b8`.

## Installed browser upgrade and restored backup

The installed browser package from the
[WAL integration milestone](v2-wal-durability-evidence.md) passes the new upgrade
fixture in Chromium 149.0.7827.55 and Firefox 151.0 with Node 24.19.0,
pnpm 11.23.0 and Playwright 1.61.0. Runtime assets come from a fresh offline
consumer of the unchanged tarball (SHA-256
`727cccd6aa781dd0a2381754b21e62959aa23581d0e5818ba4cf865c515dfe81`).
Strict TypeScript through the installed exports also passes.

The test copies the closed, checkpointed V1 main file into a fresh OPFS database
name. This is controlled fixture setup, not a public browser import API. It
checks exact portable exports and typed values, validators, indexes, migration
history and native integrity; creates spatial/ANN/inverse indexes over V1 data;
rolls back their creation and then recreates them; rolls back indexed document
updates/deletion; and checkpoints a new relational write. A different page
reopens the result and verifies its data and V2 indexes. A second fresh name
restores the original V1 bytes and proves the backup is isolated from V2 writes.
Both browsers report the exact V1 backup hash above.

From `romedb/bindings/browser`:

```sh
ROMEDB_UPGRADE_FIXTURE_DIR=/tmp/romedb-v2-upgrade-restore-04 \
  node tests/check-package.mjs \
  /tmp/romedb-wal-sync-integrated-packages/romedb-browser-2.0.0-dev.1.tgz \
  --storage-upgrade
```

Log: `/tmp/romedb-v2-browser-upgrade-installed.log`.
The fixture is generated on Linux x86_64 and read by the threaded WASI browser
build. This does not cover all source platforms or arbitrary historical catalogs.
Browser full-text remains pending its separate WASI core proposal and is not
part of this browser check.

## Python wheel after WAL integration

The active Python package was rebuilt in the dev profile after `fe2ccd404`, with
the shared protocol/frontend source. Its offline-installed wheel passes all
**7 client integration tests** on each of CPython 3.10.21, 3.12.3 and 3.14.7 on
Linux x86_64. Tests include typed values, persistence, V2 search/functions,
transactions and cancellation. These do not cover the pending scalar-error
failure cases or replace full combined acceptance.

Artifact: `/tmp/romedb-python-wal-fixed-wheels/romedb_embedded-2.0.0.dev1-cp310-abi3-manylinux_2_35_x86_64.whl`.
Size: **81,027,416 bytes**. SHA-256:
`52610b9084a079ede414cfbd968b740946b75a61aa47ecd8346fd8fbd9a1a061`.
Logs: `/tmp/romedb-python-wal-fixed-build.log` and
`/tmp/romedb-python-wal-fixed-py310.log`, `-py312.log`, `-py314.log`.
This remains a development wheel; final platform, packaging and attribution
qualification remain open.

## Python wheel after FTS backing-storage integration

The wheel rebuilt from the integrated source passes all **7 installed tests** on
CPython **3.10.21, 3.12.3 and 3.14.7** on Linux x86_64. The existing V2 search test
now checks native integrity and quick-check with FTS present, then integrity after
index removal. The preceding WAL-fixed wheel fails the new integrity assertion
with the original backing-index count error; the new wheel passes unchanged tests.
Control: `/tmp/romedb-python-fts-storage-control.log`.

Artifact: `/tmp/romedb-python-fts-storage-fixed-wheels/romedb_embedded-2.0.0.dev1-cp310-abi3-manylinux_2_35_x86_64.whl`.
Size: **81,248,255 bytes**. SHA-256:
`3c624c63997ffbb823a19ae6dddcbc36754e45ef270b4eca4977e60b3aeece56`.
Build: `/tmp/romedb-python-fts-storage-fixed-build.log`.
Installed checks: `/tmp/romedb-python-fts-storage-fixed-py310.log`,
`/tmp/romedb-python-fts-storage-fixed-py312.log`, and
`/tmp/romedb-python-fts-storage-fixed-py314.log`.
These development-wheel checks do not include the pending scalar-error cases or
close final platform, packaging and attribution gates.
