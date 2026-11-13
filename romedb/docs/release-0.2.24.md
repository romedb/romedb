# RomeDB / RomeQL 0.2.24

Released for Linux x64 on 2026-11-13. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.24).

RomeDB 0.2.24 makes lookups fast again after bulk updates in MVCC databases, and adds what RomeDB Cloud needs to commit one write of more than 1,000,000 rows.

- **Fast lookups after bulk updates.** After a bulk update, rows and index entries have newer versions in memory until the next checkpoint. An exact seek walked over every later shadowed entry, so lookups by record ID, joins, foreign key checks and UNIQUE checks became quadratic until the checkpoint. Exact seeks now stop at their row or key. Indexed UPDATEs after an earlier bulk update took 4.0 s instead of 89.6 s for 5,000 rows in a debug build. Range seeks still walk until a checkpoint.
- **Log-byte budget.** The execution meter estimates the MVCC log bytes each write adds. With a budget set, a statement that passes it stops before COMMIT and changes nothing. RomeDB Cloud uses this to send a write that is too big for a normal commit to its big-commit lane. Local requests have no budget.
- **Faster big commits.** Building a commit's log record read the whole write set again for every 1,024 rows. It now reads it once, so a 1,000,000-row indexed UPDATE takes 4.0 s instead of 103.6 s for that step.
- **Commit limits in the client.** On RomeDB Cloud, `client.limits` also reports `maxCommitBytes` and `maxCommitRows`: the log one write may add and the rows it may change. They are 256 MiB and 5,000,000 where Cloud runs big commits, and 1 MiB and 100,000 otherwise. A write past them fails with `RDB_LIMIT` and changes nothing. `timeoutMs` now accepts up to 600,000 (10 minutes), because a big commit can run for minutes. Send such a write with `prepareWrite` and `submit`, and after a timeout pass the same `PreparedWrite` to `recover`, so it commits once. RomeDB Cloud runs 0.2.24 with big commits on: 1,000,000 indexed rows commit in 65 to 93 s.
- The exact index seeks, the log-byte budget and the single schema scan are patches to Turso core, listed in the core exceptions. The row-ID seek fix is a backport from upstream Turso.

Upgrading. 0.2.24 opens databases from 0.2.13 to 0.2.23 with no change, and does not change the file format. Keep `@romedb/node` and `@romedb/client` at the same version.

RomeDB Cloud runs this release.

Security. The shipping dependency review finds zero advisories, with no accepted exceptions. See [the review](dependency-security.md) and [qualification](release-0.2.24-qualification.json).

The build source is `9ec7f208ff99909c2da5da31cefdbdc8ae5a55df`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/43368974900) passes. All 44 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13 to 0.2.23 packages. The production-profile Rust consumer and the bounded application workload also pass.
