# RomeDB / RomeQL 0.2.30

Released for Linux x64 on 2026-11-27. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.30).

RomeDB 0.2.30 adds managed files for RomeDB Cloud: the client can upload application files to a Cloud database, and each file is also a row that SQL can read. The engine is Turso 0.8.4, as in 0.2.29. RomeQL is unchanged except for one reserved name prefix.

- **Files on RomeDB Cloud.** A Cloud database now has `db.files` in `@romedb/client` and `@romedb/node`: `createUploadSession`, `upload`, `get`, `list`, `update`, `createDownloadUrl`, `delete`, `deletionStatus` and `usage`. Your backend needs a database token with the scopes `files:read`, `files:write` or `files:delete`, or an organization key with `manage`.
- **Uploads from the browser.** The new module `@romedb/client/uploads` sends a file from a page straight to storage in parts. The page gets the token of one upload session from your backend, never a database token. There is no file size limit below 4.995 TiB other than your organization's storage allowance. An upload that stopped continues with the same `id`, and files up to 1 GiB are compared with the checksum that storage computed.
- **Files in SQL.** On Cloud, each ready file is a row of the collection `romedb_files` in the same database, so a query can list files and a record can link to one. SQL may change `name`, `description`, `tags` and `metadata` with `files:write`, and `DELETE` a row with `files:delete`, which deletes the file. Nothing can insert a row or change its other fields.
- **Reserved names.** A name that starts with `romedb_` is now reserved. A statement cannot create a table, a collection or a view with such a name, and cannot rename a table to one. A database that already has such a table opens and reads as before. See [managed collections](managed-collections.md).

The examples for a backend and a page are in the [client README](../bindings/client/README.md#files-on-romedb-cloud). Outside Cloud, `db.files` has nothing to call: `romedb serve` and embedded databases do not store files.

Upgrading. 0.2.30 opens databases from 0.2.13 to 0.2.29 with no change, and adds no storage format change, so 0.2.29 can open its files. Check that your application creates no table whose name starts with `romedb_`. Keep `@romedb/node` and `@romedb/client` at the same version.

RomeDB Cloud has served files since 2026-11-27 and runs 0.2.30 from the same day.

Security. The shipping dependency review finds no vulnerabilities. It accepts one informational notice, RUSTSEC-2026-0543: `chrono` is unmaintained. The Turso engine uses it, it has no fixed version, and 0.2.29 ships the same version. See [the review](dependency-security.md) and [qualification](release-0.2.30-qualification.json).

The build source is `912f061e87d18901041ed840fd57a375250e01e5`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/46022929836) passes. All 56 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13 to 0.2.29 packages. The production-profile Rust consumer and the bounded application workload also pass.
