# RomeDB / RomeQL 0.2.16

Released for Linux x64 on 2026-11-06. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.16).

RomeDB 0.2.16 makes file exports work on large databases, and adds what RomeDB Cloud needs to hold 10 GB.

- **Large file exports.** `exportFile` and `VACUUM INTO` now commit their copy in batches, every 50,000 rows or 64 MiB, instead of as one transaction. Exports used to stop at about 1.3 GB. A 2 GB MVCC export now peaks at 171 MB of memory, and a 9.9 GB export passed on Cloud with every row checked. A failed export removes its partial file. A custom index method, such as full text, still backfills in one statement, and that is not measured at this size.
- **`exportFile` on Cloud uses a job.** Cloud builds the file in the background, and the client waits for it and then downloads it. `waitMs` bounds the wait, 3 hours by default. `timeoutMs` covers each request. On a service without jobs the client uses `/file`, as it does with `romedb serve`.
- **`romedb sqlite adopt`.** This is `sqlite import` for a source nothing else needs. It moves the file instead of copying it, so a host with room for one copy can take a large database. The source file is gone afterwards.

Upgrading. 0.2.16 opens databases from 0.2.13, 0.2.14 and 0.2.15 with no change. Keep `@romedb/node` and `@romedb/client` at the same version.

RomeDB Cloud runs this release.

Security. The shipping dependency review finds zero advisories, with no accepted exceptions. See [the review](dependency-security.md) and [qualification](release-0.2.16-qualification.json).

The build source is `e592e90f4409ff5063bdff92b5a61b035ba9424d`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/42245825120) passes. All 28 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13, 0.2.14 and 0.2.15 packages. The production-profile Rust consumer and the bounded application workload also pass.
