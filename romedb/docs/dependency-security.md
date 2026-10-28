# Dependency security review for 0.2.11
Reviewed 2026-10-28 for the Linux x64 0.2.11 candidate. The exact lockfile scan
covers 362 package/version identities against the RustSec advisory database at
`99670cfc0bc574433034b75876a4b791ccaa2efb` using the pinned `cargo-audit 0.22.2`
binary. It found zero findings in the shipping closure, with no accepted
exceptions. The lockfile differs from 0.2.10 only in the nine RomeDB workspace
package versions.

## Dependency security review for 0.2.10
Reviewed 2026-10-27 for the Linux x64 0.2.10 candidate. The exact lockfile scan
covers 362 package/version identities against the RustSec advisory database at
`3279c99911354c400e1d41e47c7d60e39258b147` (the same database as the 0.2.9
review; no newer commit was published) using the pinned `cargo-audit 0.22.2`
binary. It found zero findings in the shipping closure, with no accepted
exceptions. The lockfile differs from 0.2.9 only in the nine RomeDB workspace
package versions.

## Dependency security review for 0.2.9
Reviewed 2026-10-27 for the Linux x64 0.2.9 candidate, the first release under
the RomeDB name. The exact lockfile scan covers 362 package/version identities
against the RustSec advisory database at
`3279c99911354c400e1d41e47c7d60e39258b147` (fetched just before the review)
using the pinned `cargo-audit 0.22.2` binary.

The first scan found 14 unresolved shipping findings published after the 2.9.2
review: three vulnerabilities and ten unsoundness advisories in `zstd` 0.13.3 and
`zstd-safe` 7.2.4 (RUSTSEC-2026-0391 and RUSTSEC-2026-0395 to 0406), and
RUSTSEC-2026-0386 (`AsyncFd::new` I/O safety) in `tokio` 1.47.0. zstd reaches
the shipping closure only through Tantivy's sstable and docstore compression.
0.2.9 updates Tantivy from 0.26.2 to 0.27.0, which requires `zstd` 0.14.3 and
`zstd-safe` 8.2.0, and moves the exact `tokio` pins in the CLI and service
crates from 1.47.0 to 1.53.3. Tantivy 0.27.0 also moves `lru` to 0.18.5, so the
RUSTSEC-2026-0253 exception kept since 2.6.0 is removed. The final scan found
zero findings in the shipping closure, with no accepted exceptions.

## Dependency security review for 2.9.2
Reviewed 2026-10-21 for the Linux x64 2.9.2 candidate. The exact lockfile
scan covers 359 package/version identities against the RustSec advisory
database at `e6f94f3760d5e6c54c1b05f204002263bc025524` (1,348 advisories,
fetched just before the review; since the 2.9.1 review it adds ten advisories,
for basic-toml, rustls-webpki, wasmtime, wasmtime-wasi-http, webpki-root-certs
and webpki-roots) using the pinned `cargo-audit 0.22.2` binary. The first scan
found RUSTSEC-2026-0382 in the shipped `rustls-webpki` 0.103.15 (certificate
verification may accept distrusted emSign intermediates); 2.9.2 updates it to the
patched 0.103.16. The final scan found zero unresolved shipping findings; the
retained lru informational exception below is unchanged. Apart from that update,
the lockfile differs from 2.9.1 only in the nine RomeDB workspace package versions.

The 2.9.1 review (database `999ec89d5d3f10f20be14750ec8c2d8b44e0ac9b`) found zero
unresolved shipping findings; 2.6.0 to 2.9.1 had the same result.

The 2.5.4 review used the same database. That database added four rustls
0.23.45 advisories (RUSTSEC-2026-0346,
0347, 0348 and 0349), all fixed in 0.23.46. rustls reaches only the CLI,
through reqwest, so 2.5.4 updates it to 0.23.46; no other package changes and
the license texts are the same. Native QuickJS-NG, USearch and SQLite
advisories are unchanged since the 2.5.0 review.

The 2.5.3 and 2.5.2 reviews (database `6aed84668c23d7f535077a9fe918bf1fa0f6a5bc`)
found the same shipping result with rustls 0.23.45, before the rustls
advisories were published.

The 2.5.1 review (database `d8325cf4f578e2df4cc5b98a30e9f46b3b88e9f0`) found the
same shipping result.

The 2.5.0 review (database `57dcc0f2192e025138907a495c1ac9a7e1e3e123`) found the
same shipping result. Its notes on newly excluded findings follow.

The newer database adds three quinn-proto advisories and one unsafe-libyaml
notice. Both packages are outside the Linux normal/build closure of the four
shipping roots, so the receipt lists them with the other excluded workspace
findings.

The 2.4.0 review (database `7a656e76726dc815b52f1bdb7b2a5ea6b51ad3ba`), the
2.3.0 review (database `7eebec69c352c7191b1f13eb95dd510eeca5d1de`) and the
2.2.0 history below record why the retained dependency versions were selected.
They do not substitute for the new exact-lockfile machine-readable scan.

The following 2.2.0 history records why the retained dependency versions were
selected. It does not substitute for the new exact-lockfile machine-readable scan.

Reviewed 2026-10-02 for the Linux x64 2.2.0 candidate. This is a scoped advisory and
applicability review, not a source-code security audit or a claim that the
application has no vulnerabilities. Maintainer responsibilities and private
reporting are in [operations](operations.md).

## Rust dependency closure

The [machine-readable receipt](dependency-security-rustsec.json) records the
exact lockfile checksum, source revision, package identities, advisory database
revision, findings and excluded workspace findings. The 2.2.0 review was rerun against the version-aligned lockfile during source
review. Its checksum matches the candidate manifest; documentation and release
check fixes were uncommitted during the scan. The pinned advisory database is
unchanged from the earlier 2026-10-02 scan.

Tool: official RustSec `cargo-audit 0.22.2` Linux x86_64 binary. Download archive
SHA-256: `ab28a1bdb54db4d5d8ad5981cf1f959410370b3d28250dbd35f6a44248620e39`.
Advisory repository: `https://github.com/RustSec/advisory-db`, pinned commit
`6de4455103aced2cba86e3b86e5c090b22827cf1`, 1279 advisories. Network refresh and
yanked-package checks are disabled during the pinned scan.

`cargo tree --locked` selects normal and build dependencies for `romedb-cli`,
`romedb-node`, `romedb-python` and `romedb-c` on
`x86_64-unknown-linux-gnu`. These include the shared RomeDB frontend used by Rust
and the C library used by PHP/Swift/C#/Go. The union contains 356 package/version
identities, including local packages. Package inclusion is conservative evidence
of possible reachability, not proof that every function is linked or called.
Dev-only and other-target dependencies are excluded. Unrelated upstream
workspace findings remain visible in the receipt and are not silently ignored.

The 2.2.0 Linux shipping closure has **zero RustSec
vulnerability findings** and one informational unsoundness finding with a
documented non-applicability decision. This statement is limited to the pinned
database and dependency closure.

| Component | Change | Reason |
|---|---|---|
| crossbeam-epoch | 0.9.18 → 0.9.20 | [RUSTSEC-2026-0204](https://rustsec.org/advisories/RUSTSEC-2026-0204.html), invalid pointer dereference in pointer formatting; pulled through engine/FTS dependencies. |
| rustls | 0.23.35 → 0.23.45 | [RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285.html), TLS 1.3 encryption-level validation; CLI HTTP support uses reqwest/rustls. |
| memmap2 | 0.9.5 → 0.9.11 | [RUSTSEC-2026-0186](https://rustsec.org/advisories/RUSTSEC-2026-0186.html), range validation in mapping advice/flush operations. Tantivy brings this dependency; update avoids retaining the affected library even though RomeDB uses its own FTS directory. |
| rusqlite / libsqlite3-sys | 0.37.0 / 0.35.0 → 0.40.2 / 0.38.2 | The newly shipped CLI importer needs SQLite's current fixes; bundled SQLite changes 3.50.2 → 3.53.2. |
| rquickjs / core / sys | 0.12.2 → 0.13.0 | Bundled QuickJS-NG changes 0.15.1 → 0.16.2, including the security fixes below. |

The dependency updates in the table were made for 2.1.0 and remain in 2.2.0.
No new dependency version change was needed for this review.
The rustls update also resolves aws-lc-rs 1.18.1, aws-lc-sys 0.45.0 and
rustls-webpki 0.103.15. The rusqlite update resolves hashlink 0.12.2 plus
non-Linux SQLite WASM support packages in the workspace lockfile; those WASM
packages are absent from the Linux shipping closure. This does not reinstate a
RomeDB browser target. No upstream core implementation files were changed.

Declared minimum Rust versions are at most 1.87 for the updated rquickjs crate,
1.85 for hashlink, 1.71 for the TLS dependencies, 1.65 for memmap2 and 1.61 for
crossbeam-epoch. Rusqlite's [0.40.2 release](https://github.com/rusqlite/rusqlite/releases/tag/v0.40.2)
explicitly restores Rust 1.88 support. Actual compilation and tests use the
project's pinned Rust 1.88.0; declarations alone are not compilation evidence.

## Bundled native libraries

The [native review receipt](dependency-security-native.json) records retrieved
advisory identities/ranges, source URLs and exact bundled versions.

QuickJS-NG 0.15.1 was affected by published RegExp allocation-failure memory
corruption, JSON rope-indentation disclosure, and error-stack/Promise OOM
use-after-free issues. Memory-limited UDF execution does not make allocation
failure irrelevant. Rquickjs 0.13.0 bundles QuickJS-NG 0.16.2, verified from
`rquickjs-sys-0.13.0/quickjs/quickjs.h`; the listed upstream fixes are in 0.16.0.
The 2026-10-02 retrieval returned the same ten published repository advisories
and affected/patched version ranges as the earlier review. They were inspected, including older
array/proxy/typed-array fixes and their version/architecture restrictions.
See [QuickJS-NG advisories](https://github.com/quickjs-ng/quickjs/security/advisories).

The new `patched_runtime_preserves_rope_json_indentation` regression exercises
the actual JSON indentation disclosure trigger and checks the runtime reported
by `INFO FOR FUNCTION`. All five `user_functions` integration tests pass under
Rust 1.88.0, including bounded heap/stack failure, cancellation, preserved caller
transactions and snapshot/definition persistence. This is frontend integration
evidence, not an ASan reproduction of every upstream memory-corruption case.

The CLI's SQLite 3.53.2 includes fixes for corrupt-FTS5 CVE-2025-7709 and the
FTS5/defensive-mode CVE-2026-11822 and CVE-2026-11824. The importer additionally
restricts accepted schemas and applies defensive settings. The source ID and
amalgamation checksum are in [the SQLite notice](sqlite-notice.md). This SQLite
library is specific to adoption; RomeDB query execution remains on the pinned
Turso engine. See [SQLite's vulnerability table](https://sqlite.org/cves.html).

USearch remains 2.26.2. Its public repository advisory endpoint returned an empty
list on the review date. That is recorded evidence of the inspected channel,
not proof of absence of vulnerabilities. Continue reviewing dependency and
upstream changes before each release.

`pnpm --dir romedb/bindings/node audit --json` returned no advisories for the
package's sole development dependency, TypeScript 5.8.3; it declares no Node
runtime dependencies. This result does not cover the native addon. Rust
toolchain binaries, Maturin, application-selected language runtimes and
dynamically linked Ubuntu libraries remain separate maintained prerequisites,
not packages certified by this crate scan.

## Repeatable scan

Use the configured Rust 1.88 toolchain and a clean checkout of the desired
advisory database revision. Run from the repository root:

```sh
python3 romedb/scripts/check-dependency-advisories.py \
  --database /path/to/advisory-db \
  --cargo-audit /path/to/cargo-audit \
  --output romedb/docs/dependency-security-rustsec.json
```

The command fails for any unresolved finding in the shipping closure. Run again
if dependencies change before packaging, and include the receipt with release
evidence. Regenerate all four native dependency inventories/notices after any
lockfile change; published artifacts retain their own immutable provenance.
