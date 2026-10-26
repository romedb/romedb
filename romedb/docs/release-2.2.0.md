# RomeDB / RomeQL 2.2.0 release

Author: GLM (ZCode agent), 2026-10-02.

Status: **released for Linux x64**, 2026-10-02. Language contracts
are in [the 2.2.0 working specification](v2.2-language.md), with scope decisions
in [the decision record](v2.2-decisions.md) and milestone evidence in
[the checklist](v2.2-tasks.md). The released 2.1.0 artifacts remain immutable.

2.2.0 is the language release authorized on 2026-10-01: every unfinished
[RomeQL.md](../../../RomeQL.md) item is resolved through a verified
implementation or a documented architecture/product-fit decision.

## Changes

### Reference expansion in ordinary SELECT

```sql
SELECT id, title, author.* AS writer
FROM articles
WHERE author = type::record('writers', $writer)
ORDER BY id;
```

An aliased single-field wildcard now expands through the existing bounded nested
projection resolver in ordinary SELECT lists, without braces, for unqualified and
source-qualified forms. Plain `author` still returns the stored typed reference,
and SQL source stars keep their meaning. See
[nested projections](nested-projections.md).

### Document operations

- `SELECT * OMIT path, ...` removes named object members from returned
  collection documents (S1).
- `UPDATE ... CONTENT` replaces whole documents and `MERGE` recursively merges
  nested objects; both preserve immutable IDs, validation and atomic index
  maintenance (S3).
- `UPDATE ... PATCH [...]` applies a bounded typed subset of JSON Patch
  (add/remove/replace/copy/move/test) with RFC 6901 pointers (S4).
- `SELECT * FROM ... FETCH path, ...` expands explicitly named finite reference
  paths inside the returned document (S2).
- Typed helpers: `array::len/distinct/flatten`, `array::contains`,
  `doc::keys/values/entries/from_entries` (S5).

### Query conveniences

- Array predicates `items[WHERE ...]` and object destructuring
  `profile.{city, country}` as bounded SELECT projections (S6).
- `array::unnest(...)` table function with typed values for SQL expansion (S7).
- Composite equality: arrays, objects and vectors in DISTINCT, UNION,
  INTERSECT, EXCEPT and IN/NOT IN; typed row membership
  `(n, tags) IN (SELECT n, tags ...)`; one collection-backed recursive CTE
  seed with a UNION ALL step.

### Schema rules

- `DEFAULT` constants and `READONLY` enforcement across every write path (S8).
- `DEFINE SCHEMA ON ... STRICT|FLEXIBLE` with atomic existing-data validation,
  and typed array elements such as `array<record<writers>>` (S9).
- Stored computed fields `VALUE (expr)` evaluated on write in dependency
  order (S10).

### Search and maintenance

- Full-text analyzer configuration (`tokenizer='raw|simple|whitespace|ngram'`)
  and `search::analyze('index', text)` inspection sharing the engine tokenizer (S11).
- Managed `REINDEX` rebuilding scalar/reference, spatial, full-text and vector
  indexes transactionally from source documents (S12).
- `doc::before()`, `doc::after()` and `doc::diff()` mutation snapshots in
  collection RETURNING (S13).
- Final `TIMEOUT <n>ms|s` statement deadlines composed with caller
  cancellation (S14).
- Optional fourth-argument typed-ID inclusion filters for `search::text` and
  `search::vector`, applied before top-k.
- F16 ANN graph precision (`quantization='f16'`) with float32 reranking.
- Compound collection indexes (`CREATE INDEX ... ON docs(a,b,...)`) and
  array-element indexes (`USING ARRAY` with `array::contains` seeks).
- Collection `INSERT ... ON CONFLICT(target) DO UPDATE/DO NOTHING` against
  `id` or declared unique indexes, including compound targets.

## Completed source and artifact checks

The complete scoped check passes on the version-aligned 2.2.0 source under the
local resource guard: formatting and scoped Clippy across eight packages,
**966 Rust tests**, **139 Node/application tests**, strict TypeScript and
**five C ABI tests** and **two release-policy tests**, with zero failed, ignored or skipped tests. Every adopted
feature carries focused real-engine regressions linked from
[the checklist](v2.2-tasks.md); indexed features prove native index use without
silent scan substitution.

Exact-artifact qualification of the Linux x64 CLI, all seven native clients,
cancellation, limits, previous-version upgrade/restore and bounded recovery
passes against the built candidate: **all 23 installed verification groups**,
covering the C ABI, the CLI (including both reference-wildcard example
statements), SQLite adoption, native ELF policy checks, Node 22.0.0/24.19.0,
CPython 3.10.21/3.12.3/3.14.7, PHP 8.3.6 with FFI, Go 1.27.1 with race
detection, Swift 6.4, .NET process ownership, and immutable 1.0.0/2.0.0/2.1.0
upgrade/restore with unchanged previous backups and rejected binary
downgrades. The 55-step 2.2.0 native-client fixture exercises every adopted
language feature through the Node, Python and C# clients; the standalone Rust
consumer covers the remaining Rust path against the same source identities.

## Distribution and compatibility

Linux x64 on the documented Ubuntu 24.04 baseline, with Rust, Node/TypeScript,
Python, PHP, Swift, C# and Go clients in the GitHub release bundle. No cloud,
browser/WASM, additional platform or graph-database product scope is added.

The release introduces managed catalog version 5: field rules (defaults,
read-only, computed values, element types), strict-schema metadata, compound
index layouts, array index entries, custom analyzer configurations and F16
graph identities. Older RomeDB binaries reject version 5 metadata; preserve a
pre-upgrade backup rather than attempting an old-binary rollback. Legacy FTS
storage still migrates through explicit REINDEX during upgrade qualification.
2.1.0 and 2.0.0 databases upgrade in place; exact-artifact checks cover
candidate writes, reopens, unchanged previous backups, independent restores
and rejected downgrades. SQLite adoption is separate from RomeDB version
upgrades.

| Immutable build identity | SHA-256 |
|---|---|
| Source commit | `468be008ed477bb88a9b43cfd72a20c95ff325ca` |
| Source archive | `f43887edcea003645be4dddc8476d86816eac22f8f06e61bdb857622ced3e7b9` |
| Cargo.lock | `9126da52f5870fa826648a7ae76718650026cc70bcf4d5623ea62d8a1aed962b` |
| Build manifest | `db64b0303016d1a153ac4760db2c9e57139ee9aea6e34aa3421bca2da5f5bb14` |
| Build SHA256SUMS | `50163d2cc4d7bb077b96859e29894e319d2d40ca15b7c7cb88cbf6c40069f879` |

The original build manifest is preserved unchanged. The published archive
contains the final qualification logs and receipts under `evidence/`; the Record
Links demo consumes the same qualified Node package.

Source review found two issues after the local qualification. The demo now runs
a versioned full-text rebuild when upgrading an existing 2.1.0 showcase; a
regression using the published old binary fails before the fix and passes after
it. The bundle also contained the 2.1.0 dependency-review receipt, which did not
match its lockfile. The source review receipts now cover the exact 2.2.0 lockfile,
and the builder and bundle verifier reject mismatched or unresolved reviews.
The original candidate remains unchanged and fails the new gate. The final
bundle was rebuilt from the review-fix commit and passed all 23 installed groups
plus the standalone archived-source Rust consumer with corrected evidence.
See [the review record](v2.2-tasks.md#independent-review-and-fixes-2026-10-02).

Exact-source CI also exposed a token cancellation race at nested savepoint
RELEASE. The frontend now checks pending cancellation before releasing the
frame and suspends token delivery until RELEASE completes. Two new deterministic
regressions preserve atomic outcomes and earlier caller work. All 19 cancellation
unit tests and the full source and artifact checks pass after this correction.

## Completed release evidence

Release gates and their evidence are tracked in
[the 2.2.0 checklist](v2.2-tasks.md); this record is completed as each gate
passes, and later documentation commits never change the tagged build source.

## Published delivery, 2026-10-02

All release gates are complete. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/36955122727)
passed; [PR #15](https://github.com/romedb/romedb/pull/15) was merged without
squashing. The annotated `fastdb-v2.2.0` tag identifies the exact artifact source.
See the [qualification receipt](release-2.2.0-qualification.json) and
[independent download receipt](release-2.2.0-download.json).

[Download 2.2.0](https://github.com/romedb/romedb/releases/tag/fastdb-v2.2.0): `romedb-2.2.0-linux-x64.tar.gz` (80,926,443 bytes),
SHA-256 `124561587662978c0cd9e36899728fc11f2a7a9a95e08664100d8c1c44e3a110`. Anonymous HTTPS retrieval verified the sibling checksum,
every internal checksum, the qualified native/package/source payloads and the tag.
The corrected release quickstart is included in the frozen source; the
original build quickstart, manifest and checksums are retained under `evidence/`.
`ENVELOPE.md` retains the explicitly dated 2.1.0 measurements; no new performance
claim is made for 2.2.0.
