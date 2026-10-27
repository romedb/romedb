# RomeDB / RomeQL 0.2.9

Released for Linux x64 on 2026-10-27. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.9).

RomeDB 0.2.9 is the first release under the RomeDB name. It continues FastDB 2.9.2, renamed and re-versioned for the alpha stage. Names change everywhere and nothing is kept for compatibility:

- The product, packages and commands are RomeDB and RomeQL. The CLI is `romedb`, the Node package is `@romedb/node`, and the C# package is `RomeDB.Embedded`.
- Database files use the `.romedb` extension. Internal tables use the `__romedb_` prefix.
- Headers are `x-romedb-*`, environment variables are `ROMEDB_*`, and the SQL function is `romedb_version()`.
- Cloud is `https://cloud.romedb.com`.

0.2.9 does not open FastDB 1.x or 2.x database files. Export data from FastDB and import it into RomeDB.

Query protocol 3 returns binary and vector values as base64 strings, in embedded use as well as over the network.

Security. Tantivy 0.27.0 brings `zstd` 0.14.3 and `zstd-safe` 8.2.0. These fix RUSTSEC-2026-0391 and RUSTSEC-2026-0395 to 0406. `tokio` 1.53.3 fixes RUSTSEC-2026-0386. `lru` 0.18.5 removes the last accepted exception, RUSTSEC-2026-0253. The shipping dependency review finds zero advisories. See [the review](dependency-security.md) and [qualification](release-0.2.9-qualification.json). The default full-text tokenizer is unchanged.

The build source is `81bda5f69043ae46e313cb936671f7fd37f44d4a`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/40430583821) passes. All 20 installed verification groups pass, including Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET. The production-profile Rust consumer and the bounded application workload also pass.
