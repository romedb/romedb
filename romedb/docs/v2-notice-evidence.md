# Versioned V2 notice preparation

All native inventories are regenerated for the 2.0.0 package metadata. Four
packages without standalone license files now have checksummed archived
Cargo.toml declarations and standard license reference texts. Apache-2.0 is
selected from the declared alternatives for crc32c, pack1 and htmlescape; softaes
declares MIT. The original crate author metadata is retained without inventing
copyright years or claiming a reference template was an upstream LICENSE file.
crc32c additionally retains its complete combine.rs source and the referenced
zlib distribution terms from zlib 1.2.11's header.

The generator verifies package/archive identity, the exact declared expression,
the selected alternative and every text checksum. Twenty offline regression
tests pass, including corruption, stale state, wrong license selection and
failure atomicity. `--require-complete` rejects any remaining package lacking
archive, supplemental or declaration-backed text. This is a reproducible
source-attribution collection, not a claim that filename discovery alone is a
complete native runtime audit. The final artifact dependency/linkage check and
bundling of these texts remain release gates.

Reproduction: `python3 romedb/scripts/prepare-v2-notices.py`.
Logs: `/tmp/romedb-v2-final-notices.log`, `/tmp/romedb-v2-notice-tests.log`.
Earlier evidence and its four-gap counts below are historical.

# V2 notice packaging evidence

Current release scope is native Rust, Node.js/TypeScript, Python, PHP, Swift, C# and Go. Browser
source/build dependencies were removed at the user's request; wasm-util and WASI
SDK notice gaps below are historical browser concerns, not native release gates.
Native inventories/bundles have been regenerated for lock
`eccecb92bd14841e0f0980680eb051f365db9454cbade2212a8ffdd80d3c6443`:
Node 187 and Python 184 collected texts, four unresolved external packages each.
See [browser removal](browser-removal.md). Earlier artifacts retain the exact
contents and hashes recorded below.


Working-tree qualification, 2026-09-25. This records a concrete packaging gap
closed for development artifacts; complete distribution attribution remains open.

The prior generator only collected conventionally named files inside crate
archives. Existing repository-source supplements were documented separately and
were absent from the browser crate bundle. Twelve distinct original source texts
are now retained under `notice-sources/`, with 34 package/source associations in
`notice-source-supplements.json`. Initial fetched bytes match the previously pinned
SHA-256 values. The follow-up pins Bon, DataSketches and Tantivy repository texts
at their crate-recorded revisions, including DataSketches NOTICE and Tantivy
AUTHORS. No dependency version or license declaration was changed.

`bundle-crate-notices.py --supplements <manifest>` works offline. For every
applicable supplement it checks the crate archive against Cargo.lock, matches
both the revision and crate subdirectory against `.cargo_vcs_info.json`, checks
the source URL contains that exact revision, and verifies the local text hash.
It retains source URLs and per-package attribution, deduplicates identical texts,
and excludes only packages with collected texts from the unresolved list.
The initial results below precede the workspace source follow-up at the end of
this document. Unreviewed inline notices remain unresolved.

| Target | Distinct collected texts | Packages still without collected texts |
|---|---:|---:|
| Node Linux x86_64 | 181 | 11 |
| Python Linux x86_64 | 178 | 12 |
| Browser WASI threads | 133 | 11 |

All three generated bundles pass `--check` with the pinned manifest. The browser
build checks its bundle before compiling; the candidate builder now passes the
supplements manifest to its artifact notice generation. The ten offline tests in
`romedb/scripts/test-notice-bundle.py` pass, including altered source bytes,
revision mismatch, corrupt archives without archive notices, output preservation,
deduplication and deterministic checking. Shell syntax and candidate-builder
Python compilation also pass.

## Packaged artifacts

The notice-follow-up artifacts are under `/tmp/romedb-v2-notice-followup/`. The browser archive
was packed with pnpm 11.23.0; the Python wheel with Maturin 1.12.6 in the dev
profile. Both packaged notice files match their source files byte for byte.
Every browser runtime asset is unchanged from the WAL-qualified archive, and
all Python API/native-library files are unchanged from the FTS-qualified wheel.
These comparisons retain the earlier runtime qualification scope without
claiming a new runtime test run. The browser binary still excludes FTS.

| Artifact | Bytes | SHA-256 |
|---|---:|---|
| romedb-browser-2.0.0-dev.1.tgz | 6,023,912 | `8e76c5c3906c05446ea431e80e5ed8d3f77902ba4a1780e403630136060e7859` |
| romedb_embedded-2.0.0.dev1-cp310-abi3-manylinux_2_35_x86_64.whl | 81,253,447 | `ea97c19afac25ab83cfd8cd6247e731a0bb82595f4c5d623b6169c3145f1c4c5` |

Logs: `/tmp/romedb-v2-notice-followup-check.log` and
`/tmp/romedb-v2-notice-followup-python.log`. The preceding notice-only artifacts
remain in `/tmp/romedb-v2-notice-packages/`; direct runtime-byte comparison against
them passes for the new packages. See
[WAL runtime evidence](v2-wal-durability-evidence.md) and
[FTS/Python runtime evidence](v2-upgrade-restore-evidence.md).
Node's source bundle was regenerated and checked; no new Node archive is claimed.

## Remaining release work

After the workspace follow-up below, the browser unresolved list contains crc32c,
pack1 and softaes. Native bundles additionally have htmlescape, which has no
recorded VCS revision in its archive. The generator lists them explicitly.

The pinned crc32c archive contains an inline zlib-derived attribution in
`src/combine.rs`. All three clients now retain its original header, including
Mark Adler's copyright, and record the archive, full-source and header hashes.
The header hash is
`c3bcdf06815ecd0518c701f59a8e8c5dc1a48fc87458a6bc4f3c90b1df3f42a6`.
The referenced zlib.h terms remain unresolved; preserving this header does not
remove crc32c from the outstanding review list. Inline attribution, bundled non-Rust code,
SDK/compiler runtime notices, and the missing wasm-util source license text still
require review. A declaration-only SPDX expression is not substituted for an
unrecovered source notice. Final platform and publication gates remain open.

Regenerate each bundle from the repository root, substituting the corresponding
client inventory, audit and output paths:

```sh
python3 romedb/scripts/bundle-crate-notices.py \
  romedb/docs/browser-dependencies-wasi.json \
  romedb/docs/browser-crate-notices-wasi.json \
  romedb/bindings/browser/notices/THIRD_PARTY_CRATE_NOTICES.md \
  --supplements romedb/docs/notice-source-supplements.json
```

Use `--check` to validate an existing bundle without writing it. Cargo's pinned
archives must be available in the configured CARGO_HOME; generation fetches no
network resources.

## Workspace source follow-up

The generator now also consumes 11 explicit workspace mappings in the supplement
manifest. Applicable packages must be local in Cargo.lock and their Cargo.toml
name/version/license must match the mapping, including inherited workspace
fields. Each referenced notice file stays inside the checkout and must match its
recorded SHA-256. This collects the repository MIT text for workspace crates and
the upstream-retained `licenses/core/` texts for turso_core. It does not establish
complete inline attribution or proof that every collected component is linked.

All **13 offline generator tests** pass. New negative cases reject an inherited
license change and corrupted workspace notice bytes without replacing output.
Regeneration and `--check` pass for all three client source bundles:

| Target | Distinct collected texts | External packages without collected license texts |
|---|---:|---:|
| Node Linux x86_64 | 187 | 4 |
| Python Linux x86_64 | 184 | 4 |
| Browser WASI threads | 139 | 3 |

The remaining external packages are crc32c, pack1 and softaes, plus htmlescape in
native bundles. Workspace packages now have collected texts; this does not
resolve non-crate browser/SDK runtime notices or broader inline attribution.
The original archive audit statuses remain filename-scan evidence; the supplement
manifest and generated bundle record this additional source collection.

These are current source-bundle counts. The earlier artifact hashes above and in
later client qualification documents retain their original notice contents;
this follow-up does not claim newly packed or published artifacts. Final artifact
packaging must include the current generated bundles and repeat their byte checks.

## Candidate builder pnpm check

The retained candidate builder now uses pnpm instead of npm, handles pnpm's JSON
object and absolute archive filename, checks that the archive exists in the
candidate directory, and records a portable basename and package-manager version
in its manifest. Like the installed-package verifier, packing runs the package's
prepack native-addon load check.

A focused run with pnpm 11.23.0 produced
`/tmp/romedb-v2-pnpm-builder-_z_i3x6e/romedb-node-1.0.0.tgz` (80,694,286 bytes).
Both notice files, including the workspace follow-up, and `romedb.node` match the
current source files byte for byte. Builder Python syntax and `git diff --check`
pass. This tests packing and manifest-path handling, not the entire candidate
builder: the development checkout remains dirty, and the builder correctly
requires a clean committed source. Package metadata still says 1.0.0; this local
archive is not a V2 release or a published V1 replacement.

## Pinned upstream source gaps

The 2026-09-25 upstream-tree recheck is retained in
[notice-source-gaps.json](notice-source-gaps.json). Complete, non-truncated GitHub
tree responses contain no license/notice/copying filenames for pack1 1.0.0 at
`db63ffa9c8354b1bfc51f4fad24642368a849fbd` or softaes 0.1.3 at
`8266b9c7ebee874c8f952caed94e8a7b9103a00d`; these revisions come from the cached
crates' `.cargo_vcs_info.json`. The cached source search found their manifest
license declarations but no accompanying copyright/permission text.

The npm registry metadata for @tybys/wasm-util 0.10.1 identifies gitHead
`70f23296562c19ba3a00b9294532c1ebf7090b05`. Its complete repository tree likewise
contains no license/notice/copying filenames. These checks rule out simply
recovering omitted license files from those exact source trees. They do not
resolve the attribution gaps or establish absence of all inline terms. Do not
repeat the same filename search or mark these dependencies cleared on the basis
of their SPDX declarations. Resolution needs additional upstream provenance or
an explicitly reviewed distribution decision; no upstream contact was sent.

## Rust runtime notices

The pinned Rust 1.88.0 distribution's unmodified `COPYRIGHT-library.html`
(standard library and its build dependencies) is now included with the native
Node, Python and C distributions. `rust-runtime-notices.json` records its source
compiler component URL, component checksum and notice-file checksum. The checker
compares every packaged copy with the installed pinned toolchain. Node packing
and Python installed-wheel checks require the runtime notice, and the V2 bundle
retains it alongside the crate and C/C++ notices. System dynamic libraries are
not copied into the bundle; ELF requirements are recorded for the exact artifacts.
