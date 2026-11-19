# RomeDB / RomeQL 0.2.26

Released for Linux x64 on 2026-11-19. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.26).

RomeDB 0.2.26 fixes one defect in `@romedb/client`: on Cloudflare Workers, its places for requests in flight could stay full for good. The engine is unchanged from 0.2.25.

- **Places of requests that cannot finish.** A client object runs up to 32 requests at once and refuses the next one with `RDB_LIMIT` (outcome `not-sent`). On Workers, when a Worker sends its response while some of its database calls are still running, those calls never finish and their timers never run. In 0.2.25 and earlier their places stayed taken, so every later call in that isolate failed with `RDB_LIMIT`, for every visitor, until the isolate was replaced. Now a full client first takes back the places of calls whose `timeoutMs` has passed (20 seconds by default) and stops those calls. A call also gives back its place at once when the `signal` passed to it aborts. The limit of 32 calls at once is unchanged.
- **Getting places back at once.** Give each incoming Worker request an `AbortController`, pass its signal to every database call, and abort it before you return. The client README shows this under "Requests in flight". Do not start one call per row with `Promise.all`: the 33rd call still fails.

Upgrading. 0.2.26 opens databases from 0.2.13 to 0.2.25 with no change, and does not change the file format. Keep `@romedb/node` and `@romedb/client` at the same version.

RomeDB Cloud stays on 0.2.24. The engine is the same, so this release needs no Cloud deployment.

Security. The shipping dependency review finds no vulnerabilities. It accepts one informational notice, RUSTSEC-2026-0543: `chrono` is unmaintained. The Turso engine uses it, it has no fixed version, and 0.2.25 ships the same version. See [the review](dependency-security.md) and [qualification](release-0.2.26-qualification.json).

The build source is `8cc1a46543a6d99fe8c3b6661ac1843260b51ceb`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/44512409069) passes. All 48 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13 to 0.2.25 packages. The production-profile Rust consumer and the bounded application workload also pass.
