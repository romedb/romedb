# RomeDB / RomeQL 0.2.20

Released for Linux x64 on 2026-11-08. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.20).

RomeDB 0.2.20 lets a copy with a new index catch up with the database it was copied from.

- **`Connection::catch_up_from`.** A long index build can run on a private copy while the database keeps taking writes. When the build ends, `copy.catch_up_from(&source, index)` copies every row that differs from the source, by rowid, and maintains only the new index for the documents that changed. Every other table, including the tables of the indexes that already existed, is copied as it is, and an index method's own tables follow the rows of the table it is on. Any other schema or catalog change during the build is refused with "the schema changed during the index build". RomeDB Cloud uses it so that an index build no longer stops writes while it runs.

Upgrading. 0.2.20 opens databases from 0.2.13 to 0.2.19 with no change. Keep `@romedb/node` and `@romedb/client` at the same version.

RomeDB Cloud runs this release.

Security. The shipping dependency review finds zero advisories, with no accepted exceptions. See [the review](dependency-security.md) and [qualification](release-0.2.20-qualification.json).

The build source is `77f713c73c7e32b0ab38779009eec8395fe030ab`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/42494681925) passes. All 36 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13 to 0.2.19 packages. The production-profile Rust consumer and the bounded application workload also pass.
