# RomeDB / RomeQL 0.2.27

Released for Linux x64 on 2026-11-19. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.27).

RomeDB 0.2.27 makes two changes asked for by Cossack. The engine is unchanged from 0.2.26.

- **Schema changes no longer wait behind a stream of writes.** In `romedb serve`, writers that sent one write after another with no pause could hold a schema change, such as a migration's `CREATE TABLE`, off until its deadline. It then failed with `RDB_CANCELLED` and outcome `unknown`. Now a schema change waits only for the reads and writes already running. Reads and writes that arrive while it waits go after it. RomeDB Cloud already ran writes in order, so it does not change.
- **A full client is told apart from a request that is too big.** Every refusal that `@romedb/client` raises itself is still `RDB_LIMIT` with outcome `not-sent`, and now also has a `limit` field. `'client-requests'` means the client already runs 32 requests, and `'client-bytes'` means its requests in flight already hold too many bytes. Both pass once earlier requests end, so you can wait and send again. `'request-bytes'` and `'request-statements'` mean a saved request is larger than the database accepts, and it never passes. An `RDB_LIMIT` from the server has no `limit`. Test `error.limit` instead of the message. The type is `ClientLimit` in `client.d.ts`.

Upgrading. 0.2.27 opens databases from 0.2.13 to 0.2.26 with no change, and does not change the file format. Keep `@romedb/node` and `@romedb/client` at the same version.

RomeDB Cloud stays on 0.2.24. Its engine is the same and it does not run the local service, so this release needs no Cloud deployment.

Security. The shipping dependency review finds no vulnerabilities. It accepts one informational notice, RUSTSEC-2026-0543: `chrono` is unmaintained. The Turso engine uses it, it has no fixed version, and 0.2.26 ships the same version. See [the review](dependency-security.md) and [qualification](release-0.2.27-qualification.json).

The build source is `4544c0c73747f26418ed8020b018cac3a56bea79`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/44549558884) passes. All 50 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13 to 0.2.26 packages. The production-profile Rust consumer and the bounded application workload also pass.
