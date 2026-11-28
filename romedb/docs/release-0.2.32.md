# RomeDB / RomeQL 0.2.32

Released for Linux x64 on 2026-11-28. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.32).

RomeDB 0.2.32 gives `romedb serve` a file API for local development. It matches the managed files API of RomeDB Cloud, so the same application code stores files against a local server. The engine, RomeQL and the storage format are the same as in 0.2.31.

- **Files in `romedb serve`.** A local server now answers `db.files` of `@romedb/client` and `@romedb/node`: `createUploadSession`, `upload`, `get`, `list`, `update`, `createDownloadUrl`, `delete`, `deletionStatus` and `usage`. The browser uploader of `@romedb/client/uploads` works against it too. It works in single-database mode and with `--directory`.
- **Where the files are.** The bytes are in a folder beside the database (`app.romedb.files`, or `<id>.files` in a directory). Each file is a row of `romedb_files` in the database, as on Cloud, so SQL can list and join files locally. A row deleted with SQL has its bytes removed.
- **Several download URLs in one call.** `db.files.createDownloadUrls(fileIds, options)` returns the URLs of up to 100 files, for a page that shows many pictures. RomeDB Cloud answers it since 2026-11-28.
- **Usage charge in blocks.** RomeDB Cloud now charges file storage in blocks of 100 GB, by the most stored at one time in the month. The type of `db.files.usage()` follows: `charge` has `blocks`, `blockBytes`, `usdPerBlockMonth`, `billedBytes` and `monthlyUsd`, and the reply has `peakBytes`. Code that read `charge.terabytes` must change.

What the local server does not do: processing jobs (`db.files.process` answers "File processing is unavailable"), limits by plan and charges. Upload sessions and signed URLs end when the server stops. It listens on loopback only. A page may call the routes of one upload and open the signed URLs; every other route still refuses browser origins. See [local files](local-files.md).

Upgrading. 0.2.32 opens databases from 0.2.13 to 0.2.31 with no change, and adds no storage format change, so 0.2.31 can open its files. Keep `@romedb/node` and `@romedb/client` at the same version.

Security. The shipping dependency review finds no vulnerabilities. It accepts one informational notice, RUSTSEC-2026-0543: `chrono` is unmaintained. The Turso engine uses it, it has no fixed version, and 0.2.31 ships the same version. See [the review](dependency-security.md) and [qualification](release-0.2.32-qualification.json). The file API adds no dependency.

The build source is `3f3d3b205213351db1e92ede3b18de907e26e208`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/46124922919) passes. All 60 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13 to 0.2.31 packages. The production-profile Rust consumer and the bounded application workload also pass.
