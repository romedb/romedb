# Linux x64 V2 release qualification

Final clean-source qualification and publication are complete. See
[release 2.0.0](release-2.0.0.md) and its verified public-download receipt.
The rehearsal below remains historical to its exact source and artifacts.

The user confirmed Linux x64 as the V2 binary platform. Browser/WASM, macOS,
Windows, ARM and cloud are outside this release. This record distinguishes the
completed worktree rehearsal from final committed artifacts and publication.

## Versioned source and package rehearsal

2026-09-25, Ubuntu 24.04 x86_64 under WSL2, glibc 2.39, Rust 1.88.0.
All eight RomeDB crates and client metadata use 2.0.0. The lockfile SHA-256 is
`458537ee9ecabbd1b4127df08d5f110d427ff88ac88860d5d2eaedb7db8163a6`.
No third-party dependency changed during the version alignment.

- Scoped acceptance: 738 Rust tests, 115 Node/application tests, 3 C ABI tests,
  formatting, Clippy and strict TypeScript passed, with no ignored tests.
- Standalone Rust consumer: direct V2 APIs, queries, validation, rollback,
  persistence, cancellation and integrity passed outside the workspace;
  341 registry/git dependency identities match the workspace lockfile.
- Same packed Node client passes offline installed-package and TypeScript checks
  on Node 22.0.0 and 24.19.0.
- Same Python abi3 wheel passes eight installed tests on CPython 3.10.21, 3.12.3
  and 3.14.7.
- Extracted PHP, Go and Swift source packages, and an isolated consumer of the
  actual C# NuGet package, pass the shared 42-step native contract fixture.
  Go also passes its race detector. Toolchains: PHP 8.3.6, Go 1.27.1,
  Swift 6.4.0 and .NET SDK 8.0.425.
- The bundled stripped C library passes its three ownership/concurrency tests;
  the CLI returns the expected H3 cell.
- Released V1 creates the upgrade fixture. V2 upgrade, new writes, reopen,
  intentional V1 downgrade rejection, V2 backup/restore and restoration of the
  original V1 backup all pass in independent processes.

The local bundle is `/tmp/romedb-v2-linux-x64-rehearsal`; exact per-check logs
and the upgrade fixture are `/tmp/romedb-v2-linux-x64-evidence`.
Its source is explicitly an uncommitted worktree snapshot based on `d726bb06d`.
It is not eligible for publication. Manifest SHA-256:
`b19589055d6111b0a9582f15c4cc01c710763cabeeb06a7e547145cbb5344234`; checksum-file SHA-256:
`f90a4ae26168f9f4bede8cf6265b379decce11598faa80c0f7fca88db6e8c008`.

## Native artifacts and attribution

The builder uses the required development profile and strips distributed
copies of debug symbols. Performance evidence remains scoped to its recorded
fixtures; these artifacts are not advertised as optimized builds.
ELF inspection reports no embedded RPATH/RUNPATH. Maximum referenced GLIBC
versions are 2.38 for the CLI and 2.35 for Node and the C library. They require
libstdc++ with GLIBCXX_3.4.29. The Python wheel is tagged manylinux_2_35.
These symbol requirements do not establish a Linux-distribution test matrix.

All dependency declarations have collected archive, supplemental or declared
license reference texts: Node 195, Python 192, C 190 and CLI 236 distinct texts,
zero uncollected declarations. Twenty notice-generator regression tests pass.
The exact pinned Rust standard-library notice is included in Node, Python and
shared-library distributions. Source attribution includes the original archive
declarations and checksummed reference texts. Dynamically linked system
libraries remain platform prerequisites and are not bundled.

Reproduce with `build-v2.py` and `check-v2-bundle.py`. The builder rejects dirty
source unless explicitly asked for a nonpublishable `--development` rehearsal.
Final release requires a clean source commit, scoped hosted CI, qualification of
its exact rebuilt artifacts, publication and downloaded-checksum verification.
