# RomeDB / RomeQL 0.2.15

Released for Linux x64 on 2026-11-04. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.15).

RomeDB 0.2.15 gives `@romedb/node` and `@romedb/client` one set of value classes. The engine is unchanged from 0.2.14.

- **`@romedb/node` depends on `@romedb/client`.** It asks for exactly its own version. `@romedb/node/client` and `@romedb/node/values` now pass that package through, and `@romedb/node/embedded-client` uses the same classes. So a `Record` or `Float64` made by one package works in the other, and an application can use the URL client in a Worker and the embedded client in Node with one set of types.
- **Keep both packages at the same version.** If they differ, your package manager installs a second copy of `@romedb/client`, and a client refuses values made by the other copy with `unsupported parameter value`.
- **`@romedb/client` is on npm.** It is the URL client with no native addon and no dependencies, about 30 KB to download. It runs in Node.js, Cloudflare Workers and browsers. The workflow now publishes it first on each release, followed by `@romedb/node`.

Upgrading. 0.2.15 opens databases from 0.2.13 and 0.2.14 with no change, and existing imports of `@romedb/node/client` keep working. The `Database` and `AsyncDatabase` classes of the main `@romedb/node` entry keep their own `Record` and `Vector`. Installing the `romedb-node` release asset by URL now also needs npm, or the `romedb-client` asset of the same release.

RomeDB Cloud stays on 0.2.14. The engine is the same, so this release needs no Cloud deployment.

Security. The shipping dependency review finds zero advisories, with no accepted exceptions. See [the review](dependency-security.md) and [qualification](release-0.2.15-qualification.json).

The build source is `643eaa17ea8873c93684e3a2b6ad27806cfb9cf4`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/41842438218) passes. All 26 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13 and 0.2.14 packages. The production-profile Rust consumer and the bounded application workload also pass.
