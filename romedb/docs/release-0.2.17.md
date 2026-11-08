# RomeDB / RomeQL 0.2.17

Released for Linux x64 on 2026-11-08. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.17).

RomeDB 0.2.17 builds large indexes in bounded memory, and adds `buildIndex` to the Cloud client.

- **Index builds in batches.** Building an index used to read every document of the table into memory first. Full-text, vector, spatial, array and document field indexes, and `REINDEX`, now read 1,024 documents at a time. `REINDEX` keeps the caller's write buffer limit over the whole table, as before.
- **Builds on a private copy.** `Connection::set_offline_build(true)` lets a caller that owns a private copy of the file commit each batch and checkpoint every 256 MiB of text. In one transaction, every statement's old pages stay in memory and the commit writes each changed page again. A full-text index over 800,000 documents (1.72 GB of text) used to fail at 9.58 GB of memory. It now builds in 667 MB, and 2,000,000 documents (4.3 GB of text) built in 1.14 GB. A failed offline build leaves a partial index, so use it only on a copy you discard on failure. RomeDB Cloud's index build jobs use it.
- **`buildIndex` in `@romedb/client`.** On a Cloud URL, `buildIndex(sql, { signal, timeoutMs, waitMs })` starts an index build job and waits for it, like `exportFile`. Reads go on and writes wait. If the caller aborts, `waitMs` runs out or the client closes, the client cancels the build. Local URLs refuse it.
- **Dependencies.** RUSTSEC-2026-0441 marks `pin-utils` as unmaintained. The `futures` crates move to 0.3.33 and `hyper` to 1.9.0, which no longer use it.

Upgrading. 0.2.17 opens databases from 0.2.13 to 0.2.16 with no change. Keep `@romedb/node` and `@romedb/client` at the same version.

RomeDB Cloud runs this release.

Security. The shipping dependency review finds zero advisories, with no accepted exceptions. See [the review](dependency-security.md) and [qualification](release-0.2.17-qualification.json).

The build source is `28fe2199430e506eab9e4a4115f20453fb83fe01`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/42435462328) passes. All 30 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13, 0.2.14, 0.2.15 and 0.2.16 packages. The production-profile Rust consumer and the bounded application workload also pass.
