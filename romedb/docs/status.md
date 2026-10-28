# RomeDB implementation status

RomeDB and RomeQL **0.2.10 are the current release for Linux x64**, on the Turso
0.8.3 engine. See the [release record](release-0.2.10.md), the
[sync notes](turso-0.8.3.md) and [engine provenance](../UPSTREAM.md). RomeDB
Cloud runs the same engine but is a separate private project.

Release clients are Rust, Node.js/TypeScript, Python, PHP, Swift, C# and Go.
Browser/WASM is removed. macOS, Windows, ARM and Apple mobile are not supported.
Packages are distributed in the GitHub Linux bundle, not language registries.
Releases use the optimized `romedb-production` profile, which keeps assertions
and overflow checks; see [the build policy](production-build.md).

Back up before upgrading. Legacy full-text indexes require `REINDEX`.

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
