# RomeDB / RomeQL 0.2.19

Released for Linux x64 on 2026-11-08. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.19).

RomeDB 0.2.19 builds full-text indexes faster.

- **More rows per statement in a full-text build.** A build wrote 128 documents per statement, the size that keeps sixteen text fields plus the ID within 2,176 bound parameters. Each statement has a fixed cost in the native index, so rows per statement now come from that same parameter budget, capped at 900: 725 for an index on two fields, 900 for one, and still 128 for sixteen. On 500,000 documents and two fields an offline build took 287 s instead of 653 s, with the same file and the same results. Peak memory rose from 0.96 GB to 1.31 GB. On RomeDB Cloud a build stops writes for its whole run, so this shortens that stop the same way.

Upgrading. 0.2.19 opens databases from 0.2.13 to 0.2.18 with no change. Keep `@romedb/node` and `@romedb/client` at the same version.

RomeDB Cloud runs this release.

Security. The shipping dependency review finds zero advisories, with no accepted exceptions. See [the review](dependency-security.md) and [qualification](release-0.2.19-qualification.json).

The build source is `4a9e439a66de9da38a0f9fac7524d6fe9ab91829`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/42473746044) passes. All 34 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13 to 0.2.18 packages. The production-profile Rust consumer and the bounded application workload also pass.
