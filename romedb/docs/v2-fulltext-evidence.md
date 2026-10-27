# V2-F full-text verification

Date: 2026-09-25. Embedded working tree based on `fb246a8e4`, with existing
engine WAL work preserved. Rust 1.88.0, Node 24.19.0, Linux x64/WSL2.
This qualifies the [full-text contract](v2-fulltext.md), not a V2 release.
V2-F acceptance is complete for the documented native contract. The broader V2
release remains open. Subsequent V1 upgrade checks reopened this gate for
backing-storage integrity and drop-cleanup bugs. The approved fix in `12109384a`
passes active native suites and the complete upgrade/restore rehearsal, closing
that gate again; see [later evidence](v2-upgrade-restore-evidence.md). The counts
below retain their original source scope.

## Engine exception

The user approved the [FTS cache isolation fix](proposals/fts-cache-snapshot.md).
It was integrated as isolated commit `fb246a8e4`. The native reproduction fails
on the original core and passes with the fix. The existing native index-method
suite passes **26 FTS tests**, including the added regression for uncommitted
writer state, reader snapshots held across commit, savepoints and rollback.
No ignored cases; 959 unrelated native tests were filtered out.

## Managed behavior

All six focused integration tests pass in the active checkout:

- Multi-field text validation, exact-definition `IF NOT EXISTS`, failed builds
  without orphan storage, native DDL/storage protection and explicit input errors.
- Typed ranked hits; a 40-way score tie is sorted by stored ID before truncation.
  Native EXPLAIN confirms `QUERY INDEX METHOD fts`; joins and post-filters retain
  the documented limit semantics.
- Build/write/drop rollback, concurrent readers, checkpoint/reopen, delete,
  rebuild, table drop and collection integrity.
- Unique-conflict replacement, object UPSERT and a partially valid INSERT SELECT
  all preserve document/index agreement and transactional counts.
- A child exits with status 73 without running destructors after committed writes
  and an uncommitted indexed update. Reopen recovers the two committed documents
  and FTS entries, excludes uncommitted terms, and passes integrity checks. This
  is abrupt-process-exit evidence, not a power-loss/storage-device guarantee.

Frontend unit checks cover incompatible metadata/tokenizer/schema, stale fields
and counts, and cancellation of builds/writes while preserving prior transaction
work. Parser coverage verifies ordered paths and invalid declaration forms.
A CLI grammar check reproduced native `fast AND NOT guide` incorrectly returning
no matches; `+fast -guide` returns the intended exclusion. The frontend now
rejects negative-only AST clauses explicitly. The sixth regression checks
phrases/prefixes, boolean inclusion/exclusion, empty/all queries and rejection of
those unsupported negative-only shapes. No additional core patch is needed.

## Measured query work

| Fixture | Results | Reported rows read | Fullscan steps | VM steps |
|---|---:|---:|---:|---:|
| 40 matching documents, public limit 3 | 3 | 79 | 39 | 617 |
| One match among 41 documents | 1 | 1 | 0 | 50 |

The first fixture materializes and sorts all matches; its fullscan steps are over
the temporary hit set. The selective query uses the native index. Counters exclude
catalog/count preparation and work internal to Tantivy. These are not elapsed-time
or heap-allocation measurements. Native collector scratch capacity scales with
the indexed-document count even when only one result matches; the public limit
and result budgets do not cap this engine working memory.

## Validation logs

- Native FTS: `/tmp/romedb-v2-fts-upstream.log`.
- Managed focused tests: `/tmp/romedb-v2-fts-integrated.log`.
- Full Rust scope: **721 passed, zero failed/ignored**, across 62 test/doc-test
  results. `/tmp/romedb-v2-fts-rust.log`.
- CLI and Node addon rebuilt successfully from the final source.
- Node/application tests: **112 passed, zero failures/skips**; synchronous and
  asynchronous full-text checks include the grammar guard.
  `/tmp/romedb-v2-fts-node.log`.
- Five-package Clippy, all targets with warnings denied: passed.
  `/tmp/romedb-v2-fts-clippy.log`.
- CLI create/search/rollback/inspection/drop smoke passed: `/tmp/romedb-v2-fts-cli.log`.
- Final CLI grammar smoke passes both supported exclusion and explicit rejection:
  `/tmp/romedb-v2-fts-grammar-final.log`.
- Five-package formatting and strict Node TypeScript: passed.
  `/tmp/romedb-v2-fts-fmt.log`, `/tmp/romedb-v2-fts-typescript.log`.

Dependency inventories contain 259 declarations, 252 verified registry archives
and 168 distinct collected notice texts. The inventory's 32 entries without
collected filename candidates still require the existing supplements and the V2-R
attribution review. WASM capability, final platform artifacts, upgrade acceptance,
benchmarks and publication remain release gates.
