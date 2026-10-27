# RomeDB 2.0.0 release record

Published 2026-09-25: [Linux x64 release](https://github.com/romedb/romedb/releases/tag/fastdb-v2.0.0).
The anonymous public download matches SHA-256 `614744deed4e24f91f14e5ccdb331b07edf743fae04eafda963fa9b7d6c4d006`
(98,684,171 bytes). All 56 internal files match the bundle checksum manifest.
See [download receipt](release-2.0.0-download.json).

Source commit: `4d118ab1f819ff5deb32aa29dfe739ca9b086785`.
Source archive SHA-256: `eedcb1eb2b4b7afa659e2c99abeb9f8d51a74daa3ec75950d3ba69f699d2c32d`.
Cargo.lock SHA-256: `458537ee9ecabbd1b4127df08d5f110d427ff88ac88860d5d2eaedb7db8163a6`.

RomeDB 2.0.0 adds indexed full-text, ANN and spatial search with H3 cells, record brace projections, indexed inverse relationships and sandboxed JavaScript functions.

Linux x64 clients: Rust, Node.js/TypeScript, Python, PHP, Swift, C# and Go. The downloadable bundle includes the CLI, Node archive, Python wheel, shared native library and header, PHP/Swift/Go source packages, C# NuGet package, pinned source, notices and checksums. Package registry publication is not implied. Browser/WASM and cloud hosting are outside this release.

Back up V1 databases before upgrading. Databases using V2 catalog features reject V1; restore the original V1 backup to roll back. The approved scalar-error correction preserves prior caller transaction work. Inspect transaction reports after errors.

Built with Rust 1.88.0 on Ubuntu 24.04 x86_64 under WSL2 (glibc 2.39). Binaries use the required development profile with distributed debug symbols stripped. CLI symbols require glibc 2.38; Node and the shared library require 2.35, with GLIBCXX_3.4.29. Other Linux distributions are not a tested matrix. Read the bundle's RELEASE.md, README.md and BACKUP.md for installation, exact verification and limitations.

The engine remains pinned to Turso v0.7.2 at 046e9cbf67d22491e8ecc941ec2891b02a9f3cad, with five documented core exceptions. Upstream replacement/removal criteria are in the included source's romedb/docs/core-exceptions.md.

## Verification

Scoped acceptance passes 738 Rust tests, 115 Node/application tests and three C ABI tests, plus formatting, Clippy and TypeScript. [Hosted CI](https://github.com/romedb/romedb/actions/runs/36131883058) passed for this source commit.

The exact shipped Node package passes on Node 22.0.0 and 24.19.0; the Python wheel passes eight installed tests on CPython 3.10.21, 3.12.3 and 3.14.7. PHP 8.3.6, Swift 6.4.0, Go 1.27.1 and .NET SDK 8.0.425 pass the shared 42-step contract fixture against the shipped C library; Go also passes race checks. The Rust consumer builds from the extracted source archive and verifies 341 external dependency identities. V1 upgrade, V2 new writes/reopen, intentional V1 downgrade rejection and both V1/V2 backup restores pass.

All four native dependency notice collections and the pinned Rust runtime notice are included. Dynamically linked system libraries are prerequisites, not bundled copies.

The qualification receipt references the original build manifest and checksum file, retained under evidence/build-manifest.json and evidence/build-SHA256SUMS. The top-level manifest and SHA256SUMS also cover this release record and qualification evidence. The source archive retains its pre-publication checklist snapshot.
