# RomeDB / RomeQL 0.2.13

Released for Linux x64 on 2026-11-04. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.13).

RomeDB 0.2.13 copies an open database into a new file, finishes the rename from FastDB, and is the first release that reaches npm by itself. It has breaking changes: read "Upgrading" first.

- `exportFile` copies a database into a new `.romedb` file while reads, writes and schema changes continue. The file is one snapshot: it holds every write up to the `sequence` that the call returns, and none after it. It opens by itself in `openEmbedded` and `romedb serve FILE`, with no log file beside it.
  - `openEmbedded`: `exportFile(destination)` resolves to `{ sequence, bytes }`. The destination and its log files must not exist, and the new file has mode 0600. `exportFile()` resolves to `{ sequence, size, body }`, where `body` is a stream, and leaves nothing on disk.
  - `romedb serve DATABASE`: `GET /v1/database/file`. `romedb serve --directory`: `GET /v1/organizations/{org}/databases/{id}/file`. The reply carries `content-length`, `x-romedb-sequence` and `x-romedb-database-id`.
  - `@romedb/node/client`: `exportFile()` downloads the file from those routes with Web APIs only, so it also runs on Cloudflare Workers. Cloud URLs are refused with `RDB_UNSUPPORTED`, because Cloud does not have this route yet.
  - `AsyncDatabase`: `exportFile(destination)` resolves to `{ bytes }`.
  - `signal`, `timeoutMs` and closing the database cancel an export and remove its partial work. See [database files](service-protocol.md#database-files) for the full contract.
- `@romedb/node` is on npm ([how it is published](npm-publishing.md)). Install it with `npm install @romedb/node`. Publishing this release runs a GitHub workflow that publishes the Node package attached to the release, so the npm file and the release file are the same. npm accepts the workflow by trusted publishing, without a token, and records provenance.
- `DELETE posts:p1` deletes one record. `DELETE FROM posts:p1` is the same statement, as `SELECT * FROM posts:p1` and `UPDATE posts:p1 MERGE {...}` already were.
- The Node README, which is also the npm page, now starts with RomeQL examples, a table of common RomeQL statements and a guide to choosing an API. Its long sections are split into short ones.

Upgrading. 0.2.13 does not open databases from 0.2.12 or earlier. Create new databases, and change your code for the new names:

- Error codes start with `RDB_` instead of `FDB_`, for example `RDB_CONFLICT`, `RDB_VALIDATION` and `RDB_UNSUPPORTED`. This applies to every client, `romedb serve` and RomeDB Cloud. Code that compares codes or matches `/^FDB_/` must change.
- Every stored value now starts with the bytes `RDB` and `01` instead of `FDB` and `01`. 0.2.13 refuses a file from 0.2.12 or earlier whose collections hold documents, with `RDB_UNSUPPORTED`, and does not write to it. Files with only relational tables still open.
- The C ABI functions are `rdb_abi_version`, `rdb_open`, `rdb_call`, `rdb_interrupt`, `rdb_close` and `rdb_free`, instead of `fdb_*`. The Python, PHP, Go, Swift and C# clients in this release use the new names.

The copy runs in its own read transaction, so automatic checkpoints wait while a copy is built, and the MVCC log grows during a long export. The first checkpoint after the export merges it. A copy needs room for one more database file on the disk beside the database or the destination.

Security. The shipping dependency review finds zero advisories, with no accepted exceptions. See [the review](dependency-security.md) and [qualification](release-0.2.13-qualification.json).

The build source is `54514891294992c65458b4e90ee2f1de6352cd63`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/41658105011) passes. All 20 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET. There are no upgrade groups, because 0.2.13 does not open earlier files: a database written by the released 0.2.12 Node package was refused with `RDB_UNSUPPORTED` and left byte-identical. The production-profile Rust consumer and the bounded application workload also pass.
