# RomeDB / RomeQL 0.2.29

Released for Linux x64 on 2026-11-25. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.29).

RomeDB 0.2.29 moves the engine from Turso 0.8.3 to Turso 0.8.4, a patch release with six upstream fixes. RomeQL and the client APIs are unchanged from 0.2.28.

- **DELETE with a foreign key action.** When `PRAGMA foreign_keys` is on and a foreign key that points at the table has an `ON DELETE` action such as `CASCADE`, the DELETE now collects the row IDs first and then deletes them. Before, the cascade could change rows under the running scan. The execution meter still counts every deleted parent and child row, and a mutation budget that the cascade passes rolls back the whole statement.
- **UPDATE that changes row IDs.** An UPDATE that sets the row ID or an `INTEGER PRIMARY KEY` now runs in a statement savepoint. Before, a conflict on a later row (for example `UPDATE t SET k = k + 10` where `k + 10` already exists) left the rows already moved inside the open transaction. Now only that statement is undone, and the transaction stays open.
- **Stale statements.** Preparing a statement again after a connection setting changed no longer ends the autocommit transaction of another statement on the same connection that is still returning rows.
- **Dependencies.** `url` moves to 2.5.9 for RUSTSEC-2026-0592 (a stack overflow in `Url::origin()` on nested `blob:` URLs), which affected `url` 2.5.4 in 0.2.28. `aws-lc-rs` moves to 1.18.2.

All RomeDB engine exceptions are kept: 0.8.4 replaces none of them. See [the integration notes](turso-0.8.4.md).

Upgrading. 0.2.29 opens databases from 0.2.13 to 0.2.28 with no change, and adds no storage format change, so 0.2.28 can open its files. Keep `@romedb/node` and `@romedb/client` at the same version.

RomeDB Cloud runs 0.2.29 from 2026-11-25, which also brings it the changes of 0.2.25 to 0.2.28.

Security. The shipping dependency review finds no vulnerabilities. It accepts one informational notice, RUSTSEC-2026-0543: `chrono` is unmaintained. The Turso engine uses it, it has no fixed version, and 0.2.28 ships the same version. See [the review](dependency-security.md) and [qualification](release-0.2.29-qualification.json).

The build source is `f99cff33143f01d87c12e7ce05c423e031bb4657`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/45587984600) passes. All 54 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13 to 0.2.28 packages. The production-profile Rust consumer and the bounded application workload also pass.
