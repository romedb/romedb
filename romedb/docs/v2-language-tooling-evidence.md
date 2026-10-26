# V2 RomeQL editor qualification

Working-tree qualification on 2026-09-25 in the separate `vscode/` checkout.
This is a local editor candidate, not a V2 engine release or Marketplace publish.

## Changes and checks

- Added implemented `geo::distance`, `geo::within`, `geo::cell` and
  `geo::cell_center` completion entries alongside `geo::point`. V1 selection
  excludes them. The new completion regression fails against the preceding
  editor with only `geo::point`, then passes with the expanded catalog.
- Discover namespaced functions declared with `CREATE OR REPLACE FUNCTION`,
  in addition to CREATE FUNCTION, while retaining V1 filtering.
- Label V2 catalog/snippet entries as development functionality unavailable in
  released V1. The settings and README distinguish implemented development
  syntax from V3 proposals and retain the open JavaScript/browser release gates.
- Added a self-contained V2 fixture and explicit optional engine comparison.
  Original and formatted scripts produce identical checked results for brace
  projections, inverse relations, native FTS/ANN/spatial indexes, spatial/H3,
  a successful JavaScript call and integrity checking. Formatting is idempotent.
  This does not test or resolve the known scalar-error transaction failures.
- All **19 editor tests** pass, including the real TextMate grammar harness.
  TypeScript compilation passes. The installed VSIX passes the VS Code 1.96.0
  extension-host checks in a fresh profile, including the four new geo entries,
  V2 development labels and replacement-function discovery. Existing activation,
  diagnostics, formatting, snippet, V1/V3 filtering and cleanup checks also pass.

The extension itself does not import RomeDB, execute queries, or connect to a
service. The engine comparison is a developer-only script taking an explicitly
supplied local V2 Node package. It used the active debug addon whose SHA-256 is
`e5549d9c8c451dc35f0bee63b2148b6e4ff02ae523de4cf2d193c3efa2109261`.

## Reproduction and artifact

From `/home/tan/Sites/romedb/vscode`, Node 24.19.0 and pnpm 11.23.0:

```sh
pnpm install --frozen-lockfile
pnpm test
node scripts/check-v2-engine.cjs /home/tan/Sites/romedb/turso/romedb/bindings/node
pnpm run package
xvfb-run -a pnpm run test:host
```

`pnpm-lock.yaml` is now the active editor installation lockfile. Every direct
development dependency was checked against and pinned to the version recorded in
the retained npm lockfile. The final frozen install also succeeds
(`/tmp/romeql-v2-pnpm-frozen.log`). Optional publishing-credential and signing-helper build
scripts are explicitly disabled for these unsigned local VSIX checks. The pnpm 11
configuration uses `allowBuilds` and requires an explicit install before script
execution, preventing automatic dependency changes during verification.

Artifact: `/home/tan/Sites/romedb/vscode/romeql-0.2.0.vsix`, **18,096 bytes**.
SHA-256: `44d407ee85f12f3c5c779583ec87a676363ed3eeba2309ec19fe77af9925d788`.
Logs: `/tmp/romeql-v2-editor-control.log`, `/tmp/romeql-v2-editor-tests.log`,
`/tmp/romeql-v2-editor-engine.log`, `/tmp/romeql-v2-editor-package.log`, and
`/tmp/romeql-v2-editor-host.log`. Host exit is zero; graphical initialization and
unrelated bundled-extension proposal warnings and a Marketplace version-lookup
404 remain in the log. The local VSIX installation and test host both succeed. The profile is
`/tmp/romeql-vscode-vCq6U5`. No extension or engine artifact was published.

This closes the current editor catalog/formatter parity item. Full engine
acceptance, client/platform qualification and final V2 release artifacts remain
under the [V2 checklist](v2-tasks.md).
