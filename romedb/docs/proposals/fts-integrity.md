# Core review: FTS backing-storage integrity and cleanup

Current status (2026-09-30): **partially retained on Turso 0.8.1**. The remaining
implementation excludes backing indexes from logical row-count comparisons and
includes their roots in DROP TABLE teardown. Upstream supplies other portions of
the old patch. The historical three-file patch below is not the current diff.

## Historical approval and reproduction (0.7.2)

Status: separately approved by the user on 2026-09-25 and integrated unchanged in
isolated commit `12109384ad868dd673a572305d45a816baa2e545`. Its three-file diff
matches the reviewed patch SHA-256 below. Integrated native storage and V1
upgrade/restore checks pass.
Pre-integration candidate qualification is in
`/tmp/romedb-fts-integrity-review`, based on approved WAL commit
`fe2ccd404e005a198fac5365712cacaa54d47a2c` with the active V2 frontend copied in.
The scalar-error and WASI FTS proposals are not included. This proposal addresses
two reproduced failures from V1-to-V2 upgrade/restore qualification.

## Failures and cause

The [standalone native probe](fts-integrity-repro.rs) uses only public engine
APIs and SQL, without RomeDB lowering. A fresh database containing one text row
passes `PRAGMA integrity_check`. Creating an FTS index makes both `integrity_check`
and `quick_check` report `wrong # of entries in index
__turso_internal_fts_dir_docs_text_key`. The indexed query still finds the row.
Dropping the index then reports `Page 4: never used` instead of `ok`.
Control log: `/tmp/romedb-fts-integrity-native-control-final.log`. The same first
failure blocks the active V1-file upgrade fixture at
`/tmp/romedb-v2-upgrade-restore-04.log`.

The FTS directory intentionally stores its chunks directly in an independent
`backing_btree` index. Its associated schema table has no corresponding rows.
`translate_integrity_check_impl` nevertheless compares that index's membership
and count with the table, producing a false mismatch. The physical root walker
already includes this storage tree and must continue to do so.

Separately, `translate_drop_table` obtains indexes through `Schema::get_indices`,
which intentionally filters out backing trees for ordinary row maintenance.
Consequently, FTS teardown removes the storage schema but never emits `Destroy`
for its backing root. This is a real page leak, not another count false positive.
The first candidate corrected the count comparison but its new drop regression
still failed with the orphan page. That incomplete run is retained at
`/tmp/romedb-fts-integrity-native-suite.log` (27 passed, 1 failed).

## Candidate patch

The [isolated patch](fts-integrity.patch) changes two core sites and adds three
regressions to the existing native integrity suite:

- Skip backing trees only in ordinary row/index logical comparisons. Physical
  integrity checking retains every backing root.
- Enumerate all of a table's indexes during teardown, using the existing
  `Destroy` branch for backing trees. Preserve the filtered iterator used by
  ordinary row maintenance.
- Check valid FTS storage across writes, rollback, checkpoint/reopen and drop;
  drop rollback and reclaiming ordinary/backing indexes; and detection of an
  intentionally out-of-page cell pointer in an FTS backing root.

Patch SHA-256:
`0a769b063076b4d110d6e75e0fcd334254663e89bb4dbe2e055da585ed9c474b`.

No public API, file-format, FTS tokenizer or dependency changes are proposed.
The cleanup fix prevents future leaks; it does not reconstruct ownership of
pages orphaned by earlier development builds. This is not an automatic repair
or downgrade mechanism.

## Qualification

The final candidate native integrity suite passes **29 tests, 0 failures,
0 ignored**, including the new physical-corruption negative control and the
existing ordinary-index corruption tests. Log:
`/tmp/romedb-fts-integrity-native-suite-final.log`.
The same built integration-test executable also passes all **31 index-method
tests** and the **5 tests matching `drop_table`**, with no failures/ignored tests.
The drop filter includes one test already counted in the integrity suite; these
are not 65 distinct tests. Logs:
`/tmp/romedb-fts-integrity-index-method-suite.log` and
`/tmp/romedb-fts-integrity-drop-table-suite.log`.
The complete V1 upgrade/restore harness also passes against the candidate Node
addon, including V2 index creation/write rollback, native integrity, reopen,
checkpointed V2 backup restore, independent V1 backup restoration and unchanged
backup hashes. V1 rejects the upgraded FTS database at module loading; a second
probe removes FTS with V2 on a disposable copy and verifies V1's catalog-version
3 rejection separately. Log: `/tmp/romedb-v2-upgrade-restore-candidate-final.log`.
Before integration, the same final harness failed against the active source at the original
FTS integrity mismatch: `/tmp/romedb-v2-upgrade-restore-active-final.log`.

Candidate addon SHA-256:
`46692c136ece0460b147eb299122538e5bf1380c57d317ce594234a6ca417d4d`.
Its build log is `/tmp/romedb-fts-integrity-node-build-final.log`; it is a debug
artifact from the isolated checkout, not the active addon or a release package.
The source fixture and backup identities are recorded in
`/tmp/romedb-v2-upgrade-restore-candidate-final/report.json` and
upgrade evidence.

Candidate Rust formatting/diff checks and active-probe Clippy with warnings
denied pass. The affected native build retains one existing unused-import warning
in upstream sync code. No full green V2 combined run is claimed: the pending
scalar-error issue is still present. These counts describe the reviewed candidate,
not a subsequent active-source run.

## Review boundary

The repository [workflow](../../../../RomeDB-Workflow.md) requires:
“Any necessary local core exception requires a separately reviewed design
decision, isolated commit, patch inventory entry, and relevant regression tests;
it is not automatically authorized by this workflow.”

This proposal is separate from the already approved FTS cache and WAL fixes, and
from the pending scalar-error and WASI FTS proposals. The user separately approved
this patch, which is now integrated unchanged in its own commit. The provenance
record names that commit; the integrated affected suites and complete native
upgrade/restore rehearsal pass. Combined V2 acceptance still has the separate
scalar-error failures.
