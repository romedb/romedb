# Integrated FULL-mode WAL sync qualification

The separately approved fix is integrated unchanged in
`fe2ccd404e005a198fac5365712cacaa54d47a2c`. Its two-file diff matches the reviewed
patch SHA-256 `72ed0afb451277c9406cc0a9b65b7867e7e8d47105296ce1c011476dee994f2d`.
The [review](proposals/wal-first-commit-sync.md) retains the failing native
control, explanation and affected upstream candidate-suite results. This page
records the subsequent checks against the active V2 working tree on 2026-09-25,
including its frontend cancellation fix and shared client protocol. It is not a
V2 release.

## Native source checks

The active standalone sync probe passes. The first post-reopen commit now records
`["write", "sync", "write", "sync"]`; subsequent commits record
`["write", "sync"]`. Log: `/tmp/romedb-wal-sync-integrated-probe.log`.

The full scoped Rust command completed with **736 passed, 2 failed, 0 ignored**:

```sh
cargo test --locked -p romeql-parser -p romedb -p romedb-cli -p romedb-tests --no-fail-fast
```

Log: `/tmp/romedb-wal-sync-integrated-scoped.log`. The two failures remain the
documented scalar-error transaction cases in `user_functions`:
`sandbox_limits_and_atomic_failure` and
`function_versions_follow_reader_snapshots_and_cancelled_writes_preserve_prior_work`.
All other targets pass, including native recovery, crash, rollback and index
checks. These failures require the separate pending core proposal; they are not
waived or a green acceptance run.

`bash romedb/scripts/check-node.sh` rebuilt the debug addon and completed with
**113 passed, 2 failed, 0 skipped/cancelled**. The failures are the corresponding
user-JavaScript failed-write and worker-cancellation transaction checks, both
reporting `autocommit` where the prior active transaction should survive.
Log: `/tmp/romedb-wal-sync-integrated-node.log`. Because the script stops on the
test failure, strict TypeScript was run separately and passed with
`pnpm --dir romedb/bindings/node run typecheck` (log:
`/tmp/romedb-wal-sync-integrated-node-typecheck.log`).

Formatting and native Clippy pass for all eight RomeDB packages, with warnings
denied and all targets selected for Clippy. Logs:
`/tmp/romedb-wal-sync-integrated-fmt.log` and
`/tmp/romedb-wal-sync-integrated-clippy.log`. These checks did not rebuild or
requalify the previous Python wheel, which predates this fix. The subsequent
[upgrade milestone](v2-upgrade-restore-evidence.md) records the rebuilt wheel and
its installed tests against this WAL fix.

## Installed browser package

The active source was rebuilt with Rust 1.88.0, WASI SDK 33, the optimized dev
profile (O1, assertions retained, no LTO), then packed with pnpm 11.23.0. A fresh
consumer installed the tarball offline with scripts disabled. Node 24.19.0 and
Playwright 1.61.0 ran strict TypeScript and the real browser fixtures against
the installed assets. All passed in Chromium 149.0.7827.55 and Firefox 151.0.

Both browsers passed all **11 storage-fault cases**, in addition to the ordinary
typed-value, cancellation, close, OPFS persistence/index and abrupt-page-close
fixtures. The fault harness instruments actual OPFS handles from the test server;
it is absent from the distributable package. Cases cover one-shot and persistent
WAL quota errors, short WAL writes, errors before/after WAL flush, checkpoint
database-write/flush/WAL-truncate failures, read-on-open errors, handle-close
errors and storage-worker failure. Every case verifies preserved acknowledged
data, scalar/spatial index integrity and a subsequent successful write. Close
and worker failures also permit an immediate reopen in the same page.

Failed WAL flushes recovered the complete new row in these runs. A failed commit
therefore has an uncertain outcome and must not be treated as a confirmed
rollback. Checkpoint failures return the native incomplete result
`[1, null, null]`; the harness explicitly requires a three-column row with status
`1` or a thrown error, not an empty response or status `0`.

Commands from `romedb/bindings/browser` after rebuilding:

```sh
pnpm pack --pack-destination /tmp/romedb-wal-sync-integrated-packages
node tests/check-package.mjs /tmp/romedb-wal-sync-integrated-packages/romedb-browser-2.0.0-dev.1.tgz
```

Build log: `/tmp/romedb-wal-sync-integrated-browser-build.log`.
Installed-consumer log: `/tmp/romedb-wal-sync-integrated-browser-check.log`.
Chromium also repeated all 11 cases after tightening the checkpoint-result
assertion: `/tmp/romedb-wal-sync-integrated-chromium-strict-checkpoints.log`.
The installed Firefox run already used that stricter assertion.

| Development artifact | Bytes | SHA-256 |
|---|---:|---|
| `romedb.wasm` | 21,967,692 | `637cb8b541daa1c0a284a25694d5ef4bbf2f2cd8c5fc3f1aaca7a3f5b3a7e014` |
| `romedb-browser-2.0.0-dev.1.tgz` | 6,021,676 | `727cccd6aa781dd0a2381754b21e62959aa23581d0e5818ba4cf865c515dfe81` |

The tarball is in `/tmp/romedb-wal-sync-integrated-packages/`. The earlier OPFS
and candidate-only artifacts retain their separate identities and evidence.
The ten existing upstream WASI warnings remain; no new dependencies were added.

## V1 upgrade smoke

The locally retained V1 package at
`dist/romedb-1.0.0-linux-x64/romedb-node-1.0.0.tgz` matches its recorded SHA-256
`d508c83e749bdfc6359f744b040414b5001fbb826e167e3ea624d60483f01d5f`.
Its bundle manifest identifies source `1824de044fbdcb9d183e77fe30421d0ba25f5001`.
This is verification of that local artifact, not a fresh download or external
release-state check. It was installed offline into a separate pnpm consumer.

The existing process-isolated upgrade harness passed its old/new/reopen phases
with Node 24.19.0:

```sh
node romedb/scripts/check-preview-upgrade.cjs \
  /tmp/romedb-v1-upgrade-consumer/node_modules/@romedb/node \
  /home/tan/Sites/romedb/turso/romedb/bindings/node
```

The released client creates indexed documents and a relational table. The
rebuilt development client reads both, checks integrity and uniqueness, rolls
back changes across both models, creates a new collection and preserves it
through another process reopen. Log: `/tmp/romedb-wal-sync-v1-upgrade-smoke.log`.
Old addon SHA-256: `0830edfeb0674a8b589de03faf46a682b5de4378f96f3069ff23e046b232f96e`.
New debug addon SHA-256: `f395a78177b6cacf707642bdae1bdaa6562a41c5693e66ba322ee5e69c44b8c0`.
This bounded smoke does not qualify all V1 values/catalog states, V2 feature
upgrades, browser import, backup restore or downgrades.

## Scope

These tests verify sync ordering, reported failures and bounded browser recovery.
They do not simulate OS/power loss or establish all browser/storage platform
guarantees. Broader V1 upgrade/restore, prolonged workloads, final supported platforms,
attribution and distribution remain release gates. The scalar-error transaction
and WASI full-text core proposals remain pending and unintegrated.
