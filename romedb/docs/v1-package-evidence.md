# S6 optimized Linux release evidence

Published on explicit user approval at 2026-09-14T16:07:40Z:
[RomeDB 1.0.0](https://github.com/romedb/romedb/releases/tag/fastdb-v1.0.0).
GitHub reports draft=false and prerelease=false. Both asset digests still match
the locally verified archive and checksum file. Release source is
1824de044fbdcb9d183e77fe30421d0ba25f5001. The records below describe preparation
and superseded candidate states; publication is now complete. No npm/crates.io
publication was performed.

## Staged GitHub delivery

Draft release 388515990 (fastdb-v1.0.0) targets the exact artifact source
1824de044fbdcb9d183e77fe30421d0ba25f5001. GitHub reports both assets uploaded.
Its archive digest matches the local archive SHA-256 recorded below; the
checksum-file digest also matches locally:
ff0a78c7e37097404cc809a684ed6d2198a062dab0c9be45a863c74ad939fb30.
The release remains a draft, not public delivery.

Hosted CI 34862181988 passed for integrated-core source f41e2d177. The later
release-preparation run 34863800320 passed: 678 Rust tests with zero ignored,
106 Node/application tests, formatting, Clippy and strict TypeScript. Log:
/tmp/romedb-1.0.0-hosted.log. Draft source and both uploaded asset digests were
rechecked against local files after CI completed. Publication approval remains.

## Final archive and Rust consumer

The 1.0.0 standalone Rust consumer passed outside the workspace with Rust 1.88.0,
including typed values, portable JSON, validation/indexes, rollback, bundled
functions, vectors, profiling, limits/cancellation, write policies and reopen.
All 244 resolved registry/git package identities matched the pinned lockfile.
Log: /tmp/romedb-1.0.0-rust-consumer.log.

The final archive dist/romedb-1.0.0-linux-x64.tar.gz was extracted into a fresh
temporary directory and every embedded checksum verified. Archive SHA-256:
17d5a0e98bdb88de9ad63a6e7c7d097491a1dab6e7632202c82d1a6018ff5a01.
Its adjacent .sha256 file contains that digest. Publication remains pending.

## Versioned 1.0.0 candidate

Clean source 1824de044 sets only RomeDB package versions to 1.0.0, preserving
upstream package identities. Locked/offline Cargo metadata validation passed.
The optimized dist/romedb-1.0.0-linux-x64 bundle built successfully and every
SHA256SUMS entry verified. Exact Node 22.0.0/24.19.0 package checks passed
(10 files, 23,387,556 packed bytes), including installed SDK and TypeScript.
Preview.2-to-separately-installed-1.0.0 upgrade and reopen also passed.
Logs: /tmp/romedb-1.0.0-build.log, /tmp/romedb-1.0.0-package22.log,
/tmp/romedb-1.0.0-package24.log and /tmp/romedb-1.0.0-upgrade.log.
Node tarball SHA-256:
d508c83e749bdfc6359f744b040414b5001fbb826e167e3ea624d60483f01d5f.
The artifact remains local and unpublished. Final delivery review must use this
versioned artifact rather than the earlier 0.1.0 review packages below.

## Approved-source bundle

On 2026-09-14, clean source b9cd0c7f1 produced
dist/romedb-v1-approved-linux-x64 with the approved trigger fix included.
Release compilation, notice generation and all bundle checksums passed.
Exact-tarball installation, SDK and TypeScript checks passed on Node 22.0.0 and
24.19.0 (10 files, 23,389,610 packed bytes). A separately installed copy also
passed preview.2-to-candidate upgrade and reopen checks. Logs:
/tmp/romedb-v1-approved-build.log, /tmp/romedb-approved-package22.log,
/tmp/romedb-approved-package24.log, /tmp/romedb-approved-upgrade.log.

Node tarball SHA-256:
28340808e946615f0d6c306464f84b136af98d4dd164d1e1a801ba5a336a9dbc.
CLI SHA-256:
234c2e816dfbf271e0a2e0975de164f234f2ef63bfd43d285eae5f2019a555c4.

This supersedes the unpatched artifact for release preparation. It includes the
current SDK/query/backup/performance handoff. Package version remains 0.1.0;
final release identity and publication are still outstanding. The evidence
below describes the earlier review build and remains historical.

## Earlier unpatched review bundle

Hosted CI for pushed source 38bce712c640917add8875d3b533f352b56aa305 completed
successfully on 2026-09-14:
[run 34850463566](https://github.com/romedb/romedb/actions/runs/34850463566).
The retained log is /tmp/romedb-v1-hosted-ci.log. This validates the pushed
implementation plus packaging/handoff changes; it does not include the isolated
trigger patch or turn the local review bundle into a published stable release.

Built from clean source 7e7f376fcb510b3b093a4044277958fd37c6a3bd on 2026-09-14:

```sh
python3 romedb/scripts/build-preview.py dist/romedb-v1-review-linux-x64 \
  --release --label v1-review-linux-x64
```

Build completed successfully with Rust 1.88.0, release optimization and debug
information on Linux x64/WSL2, glibc 2.39. Log: /tmp/romedb-v1-review-build.log.
Every entry in the bundle SHA256SUMS passed verification. Source archive,
CLI/Node dependency inventories, notice audits and generated notice collections
are retained in the bundle. Inventory audits identify their own scope; they are
not proof of every statically linked component's attribution by themselves.

The exact romedb-node-0.1.0.tgz passed check-node-package.cjs with
ROMEDB_PACKAGE_TARBALL set to that file, on Node 22.0.0 and 24.19.0. The checker
requires byte equality to the current packed package, installs it offline in a
separate consumer, exercises both clients including collection CRUD/rollback,
and checks TypeScript usage. Both runs reported 10 files and 23,386,884 packed
bytes. Logs: /tmp/romedb-v1-package22.log and /tmp/romedb-v1-package24.log.

| Artifact | SHA-256 |
|---|---|
| Node package | de6aecda3f410531620a20cb798a053262275a661062928f442e09b2c9e849d3 |
| CLI | 8420c566895c56f50a75d7b40bde5f5c89977e1b98dd10b5a493fcdbdcc02702 |

An additional installation at /tmp/romedb-v1-review-consumer was used as the
new package in check-preview-upgrade.cjs, with the published preview.2 installed
package as the old package. Creation, upgrade and reopen phases all passed:
/tmp/romedb-v1-review-upgrade.log. This replaces a development-addon-only claim
with an exact optimized-package upgrade check for this source.

The bundled CLI was also executed against a temporary fresh database: object
INSERT automatically created notes, SELECT returned its typed record/document,
and a second CLI process returned the same saved text. Both processes exited 0.
This verifies the candidate quickstart's persistence path using the retained
optimized CLI, independently of the Node addon.

## Remaining release work

This is a local review bundle, not stable V1 or a registry publication. Its Node
package version remains 0.1.0 and its included preview README/limitations retain
preview framing. Final packaging must use the chosen stable version and current
handoff documentation, recheck affected final artifacts, and publish only after
the remaining requirements are closed. The reviewed trigger core exception is
still pending approval; this bundle deliberately records the current unpatched
source rather than claiming that S3 is complete. Linux x64 and Node 22/24 are the
measured package targets; no other platform is established by this evidence.
