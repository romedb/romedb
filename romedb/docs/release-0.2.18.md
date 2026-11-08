# RomeDB / RomeQL 0.2.18

Released for Linux x64 on 2026-11-08. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.18).

RomeDB 0.2.18 makes full-text search fast for common words.

- **Text search reads only its best hits.** `search::text` used to ask the native index for every hit, read each hit's ID and sort them all to break ties by ID. A common word in a large index matched most documents, so a search took seconds whatever its limit. Without allowed IDs, the search now asks the index for its best hits in score order, widens that window until it holds every hit that ties at the limit, and gives them to the statement as rows, as `search::nearest` does. The results and their order are the same. On 475,000 documents, a common word went from 3.5 to 4.5 s to 0.8 to 0.9 s in a new process, with the same memory. A search with allowed IDs is unchanged.
- **One snapshot.** A statement with `search::text` now runs in one snapshot, like one with `search::nearest`, so the search and the rest of the statement see the same data.

Upgrading. 0.2.18 opens databases from 0.2.13 to 0.2.17 with no change. Keep `@romedb/node` and `@romedb/client` at the same version.

RomeDB Cloud runs this release.

Security. The shipping dependency review finds zero advisories, with no accepted exceptions. See [the review](dependency-security.md) and [qualification](release-0.2.18-qualification.json).

The build source is `cbcf6f3f7b6adc28f1ae890482f3c1d1297667c2`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/42453645371) passes. All 32 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13 to 0.2.17 packages. The production-profile Rust consumer and the bounded application workload also pass.
