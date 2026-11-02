# RomeDB / RomeQL 0.2.12

Released for Linux x64 on 2026-11-03. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.12).

RomeDB 0.2.12 adds what an application needs to keep many databases, one per site or tenant, on one machine.

- `romedb serve --directory DIR` ([contract](service-protocol.md#directory-of-databases)) serves a folder of databases on a loopback address, with the same organization routes as RomeDB Cloud: list, create, get and delete databases, and `/read` and `/query` on each one. Each database is one MVCC file, `DIR/{id}.romedb`. At most `--max-open` databases (default 32) are open at once; the server closes the idle database that was used least recently. The token is in `DIR/romedb.token`.
- `openEmbedded` now opens MVCC files only, the format that `serve` and Cloud use, so one file moves between them without conversion. `create: true` makes an MVCC file. `openEmbedded({ maxOpen })` sets how many files one process can have open, from 1 to 1024 (default 16). Closing a file checkpoints its log and releases its lock.
- Every file of a new database is private. The WAL file and the MVCC log get the database file's permissions, so `openEmbedded` and `serve` make all three files 0600.
- `@romedb/node/client` and the new `@romedb/node/values` use only Web APIs. They run on Cloudflare Workers without `nodejs_compat`, and in browsers. Node still loads the CommonJS files. The client never follows a redirect, so a credential is never sent to another address. New: `ClientError.retryAfterMs`, `ClientError.cause`, and the `cloud` and `sequenceKnown` getters.
- The Node package is attached to this release as its own file, so `pnpm add https://github.com/romedb/romedb/releases/download/romedb-v0.2.12/romedb-node-0.2.12.tgz` installs it.
- Engine fix. On MVCC files, an atomic request whose commit ran the automatic checkpoint (when the log grows past about 4 MB) failed with `no such savepoint` after the data was committed. The fix is a patch to Turso core, listed in [core exceptions](core-exceptions.md). `serve` and Cloud were not affected.

Upgrading. 0.2.12 opens databases made by 0.2.9, 0.2.10 and 0.2.11, and qualification checks all three upgrades. Check these changes:

- Files that `openEmbedded` made in 0.2.11 or earlier are WAL files. In 0.2.12 `openEmbedded` refuses them with `FDB_UNSUPPORTED` and does not change them. Stop the application and convert the file: `romedb migrate-to-mvcc app.romedb ./app-mvcc --backup ./app-before-mvcc`, then open `./app-mvcc/database.romedb`. The synchronous `Database` class and the Rust `Database::open` still use WAL files.
- Binary values and `Vector.bytes` are plain `Uint8Array` in Node too. Before 0.2.12 Node returned `Buffer`. Use `Buffer.from(bytes)` where you need a `Buffer`.
- `saveRequest` and `loadRequest` work only in Node. Elsewhere, store the `PreparedWrite` yourself.
- Rust: `Database::open_for_service` is renamed `Database::open_mvcc`, and `Database::create_mvcc` is new.

Security. The shipping dependency review finds zero advisories, with no accepted exceptions. See [the review](dependency-security.md) and [qualification](release-0.2.12-qualification.json).

The build source is `bbf7cb4840f90f7538ba10b48d44f687083ba4a4`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/41490833288) passes. All 26 installed verification groups pass, including the 0.2.9, 0.2.10 and 0.2.11 to 0.2.12 upgrade and restore on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET. The production-profile Rust consumer and the bounded application workload also pass. The Node package was also installed with pnpm into an empty project and checked on Node 22.0.0 and 24.19.0, and its Workers entry ran in Miniflare without `nodejs_compat`.
