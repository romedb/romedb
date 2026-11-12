# RomeDB implementation status

RomeDB and RomeQL **0.2.23 are the current release for Linux x64**, on the Turso
0.8.3 engine. See the [release record](release-0.2.23.md), the
[sync notes](turso-0.8.3.md) and [engine provenance](../UPSTREAM.md). RomeDB
Cloud runs the same engine but is a separate private project.

Release clients are Rust, Node.js/TypeScript, Python, PHP, Swift, C# and Go.
Browser/WASM is removed. macOS, Windows, ARM and Apple mobile are not supported.
Packages are distributed in the GitHub Linux bundle. The Node.js package is also
its own release asset from 0.2.12 and is [on npm](npm-publishing.md) as
`@romedb/node` from 0.2.12; other clients are not in language registries.
Releases use the optimized `romedb-production` profile, which keeps assertions
and overflow checks; see [the build policy](production-build.md).

Back up before upgrading. Legacy full-text indexes require `REINDEX`.

0.2.13 changed the stored value prefix from `FDB` to `RDB` and the error codes from
`FDB_*` to `RDB_*`. It refuses a database file from 0.2.12 or earlier whose
collections hold documents, with `RDB_UNSUPPORTED`, before it writes to the file.
Create new databases for 0.2.13. The C ABI functions are now `rdb_*`.

## Implemented behavior

- [Language contracts](v2.2-language.md), [record sources](record-sources.md),
  [graphs](graph.md) and the [spatial contract](spatial-upgrade.md).
- [Spatial point/radius indexes](v2-spatial.md) and H3 cells.
- [Record brace projections](v2-record-projections.md),
  [nested projection paths](nested-projections.md) and
  [indexed inverse relationships](v2-relations.md).
- [Native full-text search](v2-fulltext.md) and [ANN search](v2-ann.md).
- [Sandboxed JavaScript functions](v2-user-functions.md), including the
  correction that keeps caller transactions after scalar read errors.
- [Seven native clients](native-language-clients.md), CLI and RomeQL editor parity.
- [Array path indexes](v2.2-language.md#array-path-indexes-0211) such as
  `team.*.role`, from 0.2.11, with catalog version 8 for collections that use them.
- [A directory of databases](service-protocol.md#directory-of-databases) served
  by `romedb serve --directory` with Cloud's organization routes, MVCC-only
  `openEmbedded` with a `maxOpen` limit, private journal files, and a
  [Node.js client for Workers and browsers](../bindings/node/README.md#workers-browsers-and-other-runtimes),
  from 0.2.12.
- [Copies of an open database as new `.romedb` files](service-protocol.md#database-files)
  (`exportFile`, `GET .../file`) while reads and writes continue, and the Node.js
  package [published to npm](npm-publishing.md) by each release, from 0.2.13.
- [Copies that leave named tables empty or start a new identity](service-protocol.md#database-files)
  (`emptyTables`, `newIdentity`), and copies between databases in
  `romedb serve --directory`, from 0.2.14.
- [`@romedb/client`](../bindings/client/README.md), the URL client with no native addon,
  on npm from 0.2.14. From 0.2.15, `@romedb/node` depends on it, so both share one set of
  value classes.

## Release verification

Each release record lists its build source, exact-source CI run, installed
verification groups, upgrade and restore checks and the public download check.

Review [all maintained core exceptions](core-exceptions.md) on each upstream
sync before keeping, adapting or removing them. The WASI-only exception was
reverted and must not be reapplied for the native release.

## Future work

Broader graph traversal, advanced geometry, changefeeds/sync and procedural
scripting remain future proposals. Begin further implementation only under a
new requested scope.
