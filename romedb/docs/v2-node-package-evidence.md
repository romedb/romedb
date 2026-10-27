# V2 native Node installed-package qualification

Working-tree qualification, 2026-09-25. Local development archive only: the
Node package manifest still says 1.0.0. It contains V2 development code and must
not be presented as the released V1 package or the final V2 candidate.

## Qualification

The existing `romedb/scripts/check-node-package.cjs` now uses pnpm for packing and
fresh offline installation. The archive has the expected ten files; collected
notices in the consumer match the source package. The generated consumer loads
only the installed package, runs synchronous and asynchronous client checks,
and compiles its strict TypeScript consumer. Missing/incompatible native-addon
loader controls also pass.

The V2 extension to the persistent fixture creates inverse relations and managed
full-text, vector and spatial indexes, then checks record brace projection,
reference expansion, typed IDs, H3 and a successful stored JavaScript function.
An indexed update is rolled back and all three search sources recover their
original results. FTS removal inside a transaction passes native integrity,
then rollback restores the index and search results. The same assertions run
through the initial synchronous client, the worker client after reopen, and a
subsequent synchronous reopen. Native integrity returns `ok` throughout.

The same packed bytes pass on **Node 22.0.0** and **Node 24.19.0** on Ubuntu
24.04.1 LTS / Linux x86_64 / WSL2, glibc 2.39. The second run repacks the current
source and checks exact equality against the supplied archive before installing
it. No other OS or libc baseline is established by these results.

This smoke does not exercise the two known JavaScript scalar-error transaction
failures. Their separate core proposal remains pending, and the full Rust/Node
acceptance gates remain open despite this passing installed-package check.

## Artifact and runtime requirements

Artifact: `/tmp/romedb-v2-node-installed.tgz`, **80,690,787 bytes**.
SHA-256: `55a9c4885b48eb37da11114a65ffbb50b99d4cc836a26ddbfd81f85624dc4aec`.
Addon: debug ELF64 x86_64, **309,939,328 bytes**, SHA-256
`e5549d9c8c451dc35f0bee63b2148b6e4ff02ae523de4cf2d193c3efa2109261`.
It includes the separately approved FTS backing-storage fix.

The [current ELF report](v2-node-elf-linux-x64.json) records no RPATH/RUNPATH,
maximum required GLIBC 2.35 and GLIBCXX 3.4.29, with dependencies on the ELF loader,
libc, libm, libmvec, libgcc_s and libstdc++. Symbol requirements are not a proof
that an untested Linux distribution works. The final versioned/stripped release
artifact must be inspected and tested independently.

Node 22.0.0 was retrieved from the official nodejs.org version archive and checked
against that version's HTTPS SHASUMS256.txt. The Linux x64 runtime archive SHA-256
is `9122e50f2642afd5f6078cafd1f52ede60fc464284384f05c18a04d13d07ae5a`.
This identifies the minimum-version test runtime, not a recommendation to deploy
that old patch release.

## Reproduction

With the desired Node version first on PATH and pnpm 11.23.0 available:

```sh
ROMEDB_PACKAGE_OUTPUT=/tmp/romedb-v2-node-installed.tgz \
  node romedb/scripts/check-node-package.cjs
ROMEDB_PACKAGE_TARBALL=/tmp/romedb-v2-node-installed.tgz \
  node romedb/scripts/check-node-package.cjs
python3 romedb/scripts/inspect-node-elf.py \
  romedb/bindings/node/romedb.node romedb/docs/v2-node-elf-linux-x64.json --check
```

The output path must not already exist; the helper refuses to overwrite it.
Logs: `/tmp/romedb-v2-node-package24.log` and
`/tmp/romedb-v2-node-package22.log`. Both exit zero. The initial pnpm harness
adaptation exposed its absolute tarball filename; using path.resolve corrected
that test-driver issue before either qualifying run. No database code changed in
this milestone and no artifacts were published.
