# Maintained engine exceptions

Required review checklist for every upstream sync. Baseline and full provenance
are in [UPSTREAM.md](../UPSTREAM.md). The current sync decisions below identify retained, retired and newly approved
exceptions. The retired WASI exception is recorded separately. The dated review
below tracks upstream replacement candidates; no patch is removed from the
existing pinned engine merely because a newer upstream revision contains a fix.
No automatic removal or periodic monitoring is configured.

## MVCC exact seeks over shadowed B-tree rows (2026-11-12)

On an MVCC file, a B-tree row or index entry is shadowed when a newer version of it is in
memory, after an update or a delete that is not checkpointed yet. A seek that reached a
shadowed B-tree row moved on, and kept moving while rows were shadowed. After an update of
every row, each lookup read the rest of the table, so a statement with one lookup per row
became quadratic. With the debug CLI, `UPDATE items SET price = price + 1` over 5,000 rows
with an index on `price` took 89.6 s after an earlier update of those rows and 3.9 s
without one, and inserting 1,500 rows that reuse `UNIQUE` values the rows had just changed
took 33.9 s. Cloud folds only at about 4 MiB of log, so such rows are common there.

Two changes in `SeekBtreeState` of `MvccLazyCursor::seek_btree_and_set_peek` in
`core/mvcc/cursor.rs`:

- An exact rowid seek stops at its row. This is upstream `d94a7d320` and its follow-up
  `4813f6096`, merged upstream on 2026-11-04 after our 0.8.3 base, in one commit. Our
  `query_btree_version_is_valid` returns `bool`, not `Result`, so the backport has no `?`
  there.
- Ours: an exact index seek (`eq_only` with `SeekKey::IndexKey`) moves the B-tree one entry
  at a time in `AdvanceBTree`, and `CheckRow` ends it at the first entry whose key differs
  from the seek key (`past_equal_index_keys`). It still steps over shadowed entries that
  have the seek key, because a key prefix can match several entries. UNIQUE checks
  (`NoConflict`), `IdxDelete` and foreign key checks use these seeks.

After both: the update above took 4.0 s, and the 1,500 inserts 1.26 s (3,000 inserts went
from over 150 s to 2.68 s).

Not changed: a range seek still walks every shadowed entry, in `_advance_btree_forward`,
which has upstream's FIXME about it. An equality lookup on an index that is not UNIQUE is a
`SeekGE` without `eq_only` and an `IdxGT`, so after every indexed value changed,
`SELECT count(*) FROM items a JOIN items b ON b.price = a.price` over 1,500 rows took 41 s.
A checkpoint ends it (0.24 s after `PRAGMA wal_checkpoint(TRUNCATE)`). Upstream main
`f0f47f6f7` (2026-11-12) has the same walk.

Drop the backport at the first upstream sync that contains `2c837d49a`. Keep our index seek
change until upstream stops exact index seeks at the end of their key; it is a candidate
for an upstream PR. The regressions are `test_exact_rowid_seek_over_shadowed_btree_rows`
(upstream's) and `test_unique_check_over_shadowed_btree_index_entries` in
`core/mvcc/database/tests.rs`. Without our change the first insert of the second test took
158 B-tree steps; with it, every insert takes at most 8.

## FTS typo words (2026-11-11)

Upstream FTS reads `databse~1` as the words `databse` and `1`, because the lenient grammar
keeps `~1` inside a word. `core/index_method/fts/prefix.rs` now treats a word that ends in
`~1` or `~2` (`typo_word`) as a typo query, built by `Expander` like a prefix. The word is
analyzed with the index's analyzer; its length without the stemmer sets the distance
(`allowed_typos`: 1 from 4 characters, 2 from 8). `FuzzyTermQuery::automaton` streams each
segment's term dictionary for the words within the distance, with a swap of two
neighboring characters counted as one edit, and the 64 in the most documents are kept.
The word itself is a plain `TermQuery`. Each other word is a `TermQuery` in a `BoostQuery`
of one half times the IDF of the most common of all the words over its own IDF, so all
of them score as equally rare and a nearby word scores half of what the word would. A
word with no `~1` or `~2` builds as before. No new dependency: `FuzzyTermQuery` and its
Levenshtein automaton are part of Tantivy.

Keep this exception until upstream FTS scores typo matches with BM25. The regression is
`fts_typo_words_match_nearby_words_and_score_below_the_word` in
`tests/integration/index_method/mod.rs`, and the typo test in
`romedb/tests/tests/fulltext.rs`.

## FTS filter columns (2026-11-11)

Upstream FTS indexes every column as analyzed text, so a search cannot keep only the
documents with one `locale` before it picks its best hits. `core/index_method/fts/mod.rs`
now accepts the WITH key `filters`, a comma-separated list of index columns. Those columns
are indexed whole with Tantivy's `raw` tokenizer and no positions, are left out of the
query parser's default fields, and cannot have a weight; at least one column must stay a
text column. In `core/index_method/fts/prefix.rs` a query term on a filter column
(`lang:en`, `lang:"en-US"` or `lang: IN [en vi]`) becomes a `TermQuery` or `TermSetQuery`
inside a `ConstScoreQuery` with score 0, so it matches whole values and adds nothing to a
document's score. A prefix or a phrase slop on a filter column rejects. A query with such
a term is built by `Expander` like a prefix query; a query with neither still goes to the
parser whole. `FTS_MAX_QUERY_BYTES` is now public, because RomeDB adds the filter terms to
the query and checks the length. An index without the key has the same schema as before.

Keep this exception until upstream FTS can filter on whole values without scoring them.
On a sync, check that filter columns stay out of the parser's default fields. The
regressions are `fts_filter_columns_match_whole_values_and_keep_scores` and
`fts_with_clause_rejects_bad_filters` in `tests/integration/index_method/mod.rs`, and the
two filter tests in `romedb/tests/tests/fulltext.rs`.

## FTS folding and stemming (2026-11-09)

Upstream FTS accepts only a tokenizer name, so an index cannot fold accents or stem
words. `core/index_method/fts/mod.rs` now accepts two more WITH keys: `fold` (0 or 1;
`default`, `simple` and `whitespace` tokenizers) and `stemmer` (one of the 18 lowercase
language names in `analysis::STEMMER_LANGUAGES`; `default` tokenizer only). The new
`Analyzer` in `core/index_method/fts/analysis.rs` validates them and builds the tokenizer,
then `AsciiFoldingFilter`, then Tantivy's `Stemmer`, and registers that analyzer under
its own name, such as `default+fold+stem-english`; the schema's text fields use that name.
An index without the keys uses the same tokenizer names and analyzers as before.
`analyze_text` now takes an `Analyzer` instead of a tokenizer name and ngram window. The
`stemmer` feature of Tantivy is on by default, so `frostem` was already a dependency; no
new dependency and no storage change.

Stemming changes the prefix patch below. A stored stem stemmed again can change
(`agreed` is stored as `agre`, and `agre` stems to `agr`), so `prefix.rs` no longer
rewrites a prefix into words for the query parser to analyze. When a query has a
one-word prefix, it builds the query itself: clauses as `BooleanQuery`, boosts as
`BoostQuery`, each expanded word as a `TermQuery` per field with the field's weight, and
every other leaf through the query parser alone. A query without a prefix still goes to
the parser whole. On a stemmed index a prefix is analyzed without the stemmer and
matches stored words that start with it or with its stem; with no match, it uses the
longest stored word that the prefix starts with, of at least half its characters.

Keep this exception until upstream FTS offers folding and stemming. On a sync, check that
the schema's tokenizer name still comes from `Analyzer::name` and that `register_tokenizers`
registers it. The regressions are `fts_with_clause_folds_accents_and_stems_words`,
`fts_with_clause_rejects_bad_fold_and_stemmer` and
`fts_single_word_prefix_on_a_stemmed_index_matches_the_stored_stems` in
`tests/integration/index_method/mod.rs`, `folding_and_stemming_keep_offsets_and_positions`
in `romedb/tests/tests/fts_analysis_hook.rs`, and the two `fold`/`stemmer` tests in
`romedb/tests/tests/fulltext.rs`.

## FTS single-word prefixes (2026-11-09)

Tantivy 0.27's query parser has no expansion for a one-word prefix. It reads `hel*` as
the word `hel`, so the query found nothing, and `"hel"*` failed with
`PhrasePrefixRequiresAtLeastTwoTerms`. `core/index_method/fts/prefix.rs` now parses the
query with Tantivy's lenient grammar, as `parse_query_lenient` did, and replaces each
one-word prefix in the tree before `build_query_from_user_input_ast_lenient` builds the
query. The prefix is analyzed with the field's tokenizer; each segment's term dictionary
gives the words that start with it; the 64 in the most documents (`MAX_PREFIX_TERMS`)
become an OR of plain words, so each scores with BM25. Since the folding and stemming
patch above, those words are built as term queries instead of being given back to the
parser. An analyzed prefix shorter than
`MIN_PREFIX_CHARS` (2) is an error, a prefix that analyzes to two or more words becomes a
phrase prefix, and an ngram index reads `hel*` as `hel`. A query without such a prefix
builds the same query as before. The only call site is `query_start` in
`core/index_method/fts/mod.rs`. No storage change, no new dependency.

Keep this exception until upstream Tantivy or Turso expands a one-word prefix with
BM25 scores. On a sync, check that `query_start` still builds the query through
`prefix::Expander::parse_query`. The regressions are the four
`fts_single_word_prefix_*` tests in `tests/integration/index_method/mod.rs` and
`a_prefix_finds_the_words_that_start_with_it` in `romedb/tests/tests/fulltext.rs`.

## VACUUM INTO in batches (2026-11-06)

`VACUUM INTO` built its copy as one transaction, and on an MVCC source as one MVCC
transaction. Its memory grew with the database, and a commit read spilled pages back, so
a file export stopped at about 1.3 GB. The copy is disposable until the command succeeds,
so it does not need to be one transaction. `VacuumTargetBuildConfig` gains two fields, both
off for in-place `VACUUM`:

- `commit_in_batches` commits and checkpoints the target every 50,000 rows or 64 MiB, and
  after each index and each post-data object. A custom index method backfills its whole
  index in one statement, so that step is still one transaction.
- `convert_to_mvcc` builds an MVCC source's copy as plain WAL pages, as in-place `VACUUM`
  already does, and turns it into MVCC in `FinalizeTargetHeader`. It must stay before the
  header is written, because bootstrapping the metadata table moves the schema cookie.

Because a partial copy is now a valid database, `cleanup_op_vacuum_into` removes the output
file and its side files after a failure. `VACUUM INTO` refuses a path that exists, so it
only ever removes a file it created.

Keep this exception until upstream builds `VACUUM INTO` without one large transaction. On a
sync, check that `vacuum_target_build_step` still has the phases that commit, and that
in-place `VACUUM` still passes both fields as false. The regressions are
`test_vacuum_into_commits_in_batches` in
`tests/integration/query_processing/test_vacuum_into_mvcc.rs`, in both journal modes,
`test_cleanup_vacuum_into_removes_a_partial_output` in `core/vdbe/execute.rs`, and
`test_vacuum_into_preserves_fts_index` and `test_vacuum_into_preserves_custom_index_method`
in `tests/integration/query_processing/test_vacuum.rs`.

## VACUUM INTO tables without rows (2026-11-04)

A file export can leave tables empty so that sign-in tokens never reach a copy that
is handed to someone else. Deleting rows from a finished copy does not work: the
values stay in freed pages. `Connection::set_vacuum_into_tables_without_rows` names
tables that `VACUUM INTO` creates but does not fill, so their rows are never read.
It adds one field to `Connection`, one to `VacuumTargetBuildConfig`, and a skip at
`StartCopyTable` in `core/vdbe/vacuum.rs`. An empty set changes nothing, and in-place
`VACUUM` always passes an empty set. Keep this exception until upstream can skip
tables in `VACUUM INTO`. On a sync, check that `vacuum_target_build_step` still
copies each table in one place. The regressions are
`test_vacuum_into_leaves_named_tables_empty` in
`tests/integration/query_processing/test_vacuum_into_mvcc.rs` and
`emptied_tables_reach_no_part_of_the_file` in `romedb/tests/tests/file_export.rs`.

## MVCC savepoint commit resume (2026-11-02)

Found while making MVCC the only embedded journal mode. `RELEASE` of the savepoint
that opened a transaction commits it. On MVCC files the commit that passes the
automatic checkpoint threshold (about 4 MB of log) waits on checkpoint I/O; the VM
then runs the `RELEASE` instruction again, which looked up the already released
savepoint and failed with `no such savepoint`, although the data was committed.
Every RomeDB atomic request opens its outer scope with a savepoint, so an embedded
MVCC file failed and fenced itself at its first automatic checkpoint (native repro:
batch 84 of 200 with a 4,152,026-byte log; WAL files and `BEGIN`/`COMMIT` were not
affected). `romedb serve` commits with `COMMIT` and Cloud disables automatic
checkpoints, so neither was exposed. The patch makes `SavepointOp::Release` resume
a commit already in progress through `op_auto_commit`, the same re-entry rule
`AutoCommit` uses. Upstream main `36da5b2e4` (2026-11-02) has the same release code.
Both regressions below fail without the patch and pass with it; the core
`savepoint` filter (13) and `mvcc` filter (300) pass.

## FTS analyzer inspection (2026-10-02)

The 2.2.0 roadmap authorizes engine changes needed for missing RomeQL features.
The reviewed additive hook extracts tokenizer registration from
`core/index_method/fts/mod.rs` into `core/index_method/fts/analysis.rs` and exports
a streaming visitor shared with real index registration. No storage change or
new dependency. [Design and evidence](proposals/fts-analyzer-inspection.md).
`fts_analysis_hook` passes three tests and existing `fulltext` passes eight.
Retain token/case/Unicode/long-token/ngram boundaries, early-stop, native-query
and reopen regressions. Remove this exception when upstream provides an
equivalent shared visitor and these tests pass without the extraction.

## Follow-up audit (2026-09-30)

Rechecked every `core/` deviation from 0.8.1 and fetched upstream main at
`36da5b2e435cb07bba3bed2c7e7eef236b6b2e64`. The two retired overrides remain
absent; all seven active exceptions still cover missing behavior. No additional
core patch can be removed based on this comparison. Historical proposal pages
now state their current retained/partial/retired status before their old review
records. Full file inventory, replacement criteria, future sync hazards and
fresh regression evidence are in the
follow-up audit.

## MVCC metering follow-up (2026-09-30)

Default-on Cloud qualification exposed an outdated assumption in the retained
execution-metering exception: MVCC cursors return a logical table ID from
`root_page()`, so physical root 1 cannot identify catalog mutations. The Insert
and Delete meter hooks now use the same compiler catalog identity as schema-read
attribution. DDL catalog writes consume no customer mutation budget; inserted,
copied and deleted customer rows still count. This adapts the existing approved
exception without changing upstream change counters or transaction lifecycle.
The regression is `mvcc_schema_writes_do_not_consume_customer_mutation_budgets`
in `schema_meter.rs`; final milestone validation is recorded in the follow-up
audit.

## Upstream sync: Turso 0.8.3 (2026-10-20)

Candidate upstream commit: `607bff9fd0e547a25a97b3bf06e83bcc5d99d53c` (tag `v0.8.3`).
The merge had no conflicts. Upstream changed seven files that RomeDB also
changes (`Cargo.toml`, `Cargo.lock`, `core/connection.rs`,
`core/translate/emitter/update.rs`, `core/translate/sequence.rs`,
`core/vdbe/execute.rs`, `core/vdbe/mod.rs`), always in different places from
the local patches. 0.8.3 replaces no exception, so every patch stays. Notes and
evidence: [integration notes](turso-0.8.3.md), checklist.

| Existing exception | 0.8.3 decision | Evidence |
|---|---|---|
| Trigger interruption | Keep | `op_program` in 0.8.3 still joins `Interrupt` with `Busy`. Native trigger suite: 98 passed. |
| Scalar read errors | Keep | The only upstream `execute.rs` change is in `op_journal_mode_inner`. Native external API suite: 14 passed. |
| Checkpoint WAL barrier companion | Keep | No upstream change under `core/storage/`. `checkpoint_barrier` 5 and `checkpoint_crash_atomicity` 2 passed; first FULL commit 1 passed. |
| Named-savepoint cancellation recovery | Keep | Upstream adds `end_abandoned_mvcc_commit` near, but not in, the savepoint code. Statement lifecycle 46 and savepoint filter 12 passed. |
| FTS backing integrity / root reclamation | Keep | No upstream change under `core/index_method/`. Native integrity suite: 36 passed. |
| FTS savepoint cursor pins | Keep | Same files unchanged. Native index-method suite: 131 passed. |
| FTS analyzer inspection | Keep | `fts_analysis_hook` 3 and `fulltext` 8 passed. |
| Execution counters and budgets | Keep, checked | Upstream adds compare opcodes and an `is_called` column read to the AUTOINCREMENT explicit-ID path and ends abandoned MVCC commits fully. New `turso_083` regression: an explicit-ID insert is still one customer mutation in WAL and MVCC modes. All metering suites pass with unchanged expectations (`profile` 4, `execution_meter` 14, `mutation_meter` 6, `schema_meter` 8, `index_method_meter` 4, nine `metered_*` suites 44). |

Not taken: the whole-table DELETE fast path (`58f350a90`) is not in 0.8.3. The
hazard recorded in the 2026-09-30 audit still applies to the next sync.

## Upstream sync: Turso 0.8.1 (2026-09-30)

Candidate upstream commit: `8549c16595d2faf1bdd6ee24aee0be8bfabb3d4a`.
This section supersedes the 0.7.2 retention decisions below for current source;
the prior release record remains historical evidence.

| Existing exception | 0.8.1 decision | Replacement or adaptation |
|---|---|---|
| Trigger interruption | Retain | Upstream still merges Interrupt with Busy in subprogram execution; preserve the distinction and saved subprogram state. |
| FTS cache isolation | Remove old patch, retain regression | Upstream replaces the monolithic FTS implementation with snapshot-aware segment/directory state. The old pager-identity patch no longer applies; public concurrent FTS snapshot tests pass. Native index-method suite: 131 passed, including the retained isolation regression. |
| First FULL WAL commit | Remove old patch, retain regression | Upstream prepared-frame/dirty-WAL `need_fsync` logic supplies the barrier. Native first-FULL regression passed; the raw sync probe also confirms the write barrier. |
| FTS backing integrity/root reclamation | Retain necessary remainder | Upstream supplies part of the backing-store lifecycle; local logical-count exclusion and DROP TABLE index ordering remain needed; native integrity suite: 36 passed. |
| Scalar read errors | Adapt | Preserve the read-only ExtensionError cleanup exception through the new VDBE dispatch/abort implementation. Native external API suite: 14 passed; scalar-error transaction probe passed. |
| Checkpoint WAL barrier | Retain pending-completion companion | Upstream supplies the main pre-backfill barrier and broader cleanup. Retain explicit failed-completion observation/retry and published automatic-commit cleanup. RomeDB checkpoint_barrier: 5 passed; checkpoint_crash_atomicity: 2 passed. |
| Named-savepoint cancellation recovery | Adapt | Keep named-frame prior-poison state and recovery after successful rollback in the new connection/savepoint implementation. Native statement lifecycle: 46 passed; savepoint integration filter: 12 passed. |
| Execution counters and budgets | Adapt | Attach meters in both normal and explain dispatch paths; preserve completed cursor visits, cooperative budget interruption, mutation/schema classification and incremental Count. New window-generated ephemeral deletes exclude logical mutations. Full scoped metering/profile suites passed. |

New approved exception: [FTS savepoint cursor pins](proposals/turso-081-fts-savepoint.md).
The user approved its focused patch on 2026-09-30. Backing cursors register only
after allocation in a stable Box; rollback invalidates them before truncating
new pages. Existing fulltext suite: 8 passed; new public 0.8.1 suite: 6 passed.
The exact isolated patch and its safety invariant are in the review record.
Broader current-source regression results are tracked in the
upgrade checklist; pending checks are not release evidence.

## Release review: 2026-09-25

Owner: RomeDB maintainers. Review this register on every release and upstream
sync; keep source links and run the named regressions before changing a patch.
Inspected upstream main at
[`64b8ef5742fc18937f9c89806c81e3f6475dc7a3`](https://github.com/tursodatabase/turso/commit/64b8ef5742fc18937f9c89806c81e3f6475dc7a3)
(0.8.0-pre.13). No upstream merge is part of the current 2.1 candidate.

- **First FULL commit:** upstream
  [`fa8aeccfcf50feb715801a76478336dca72935fa`](https://github.com/tursodatabase/turso/commit/fa8aeccfcf50feb715801a76478336dca72935fa)
  checks prepared frames as well as WAL dirtiness before FULL sync, matching the
  central requirement of our patch. This is a concrete removal candidate on a
  future sync; the RomeDB regression has not been run against that upstream
  replacement, and the existing pinned base still needs its local fix.
- **FTS backing storage:** upstream
  [`7e485be941a53b0fbf6d7f132c111787ca3814ce`](https://github.com/tursodatabase/turso/commit/7e485be941a53b0fbf6d7f132c111787ca3814ce)
  contains backing-index integrity/lifecycle changes. Recheck physical-corruption
  detection, root reclamation and upgrade/restore on an actual sync candidate;
  source inspection alone does not establish all our required behaviors.
- **Trigger interruption:** the inspected upstream `op_program` still combines
  `StepResult::Interrupt | StepResult::Busy` into Busy. Retain the local patch.
- **FTS cache isolation and scalar read errors:** upstream FTS and VDBE structures
  have changed substantially. No behaviorally equivalent replacement has been
  qualified. Retain both on the pinned base and test during a future sync.

The review also found a separate upstream NORMAL-mode checkpoint durability fix,
[`cc26d08508cbe045472fa3015e2bce4a389b5e06`](https://github.com/tursodatabase/turso/commit/cc26d08508cbe045472fa3015e2bce4a389b5e06).
The pinned build reproduces its power-loss defect. A separately reviewed
[approved backport](proposals/checkpoint-wal-sync.md) also handles failed-sync
retry and automatic-checkpoint completion on this pin. The user approved its
exact six-file patch on 2026-09-25, and shared core now includes that patch.
Integrated acceptance is recorded separately. It is not the
first-FULL-commit exception and must not be silently included in it.

| Exception / local commit | Required behavior and regression | When to update or remove |
|---|---|---|
| Trigger interruption `ded389aea` | `OpProgram` preserves interruption versus contention; keep the RomeDB after-write trigger cancellation regression and affected native trigger suite. Review. | Upstream propagates Interrupt correctly through subprograms, with saved state and unchanged Busy handling. |
| FTS cache isolation `fb246a8e4` | `test_fts_cache_preserves_connection_snapshots`: no uncommitted membership/score leaks; existing reader snapshot survives writer commit. [Review](proposals/fts-cache-snapshot.md). | Upstream directory caching is scoped to the owning pager or otherwise proves equivalent isolation and rollback behavior. |
| First FULL commit WAL sync `fe2ccd404` | Native first-commit sync probe and FULL/NORMAL/OFF controls. The historical browser flush-fault matrix is supporting evidence, not a current gate. [Review](proposals/wal-first-commit-sync.md). | Upstream flushes prepared frames before publishing/acknowledging FULL commits, including the first commit. |
| FTS backing storage `12109384a` | `test_fts_backing_storage_integrity`, `test_drop_table_frees_backing_and_ordinary_indexes`, `test_fts_backing_storage_physical_corruption`; upgrade/restore rehearsal. [Review](proposals/fts-integrity.md). | Upstream excludes backing trees only from inappropriate logical counts, retains physical checks, and reclaims all backing roots on teardown/rollback. Existing orphan pages still need separate handling. |
| Scalar read errors `f26014f04` | `scalar_read_error_preserves_transactions_and_named_savepoints`, RomeDB user-function tests and Node cancellation/transaction tests. [Review](proposals/udf-error-transaction.md). | Upstream read-only extension failures preserve caller transactions, savepoints and changes(), while autocommit cleanup and writer rollback controls pass. |
| Checkpoint WAL barrier `5f4133d73` | `checkpoint_barrier` and `checkpoint_crash_atomicity`: NORMAL backfill follows successful WAL sync; failed returned/immediate/deferred completions cannot skip the barrier; automatic failures preserve published writes; FULL recovery preserves acknowledged writes. [Review](proposals/checkpoint-wal-sync.md). | Upstream includes the barrier from `cc26d08508cbe045472fa3015e2bce4a389b5e06` and equivalent pending-completion, retry and automatic-checkpoint cleanup. Retain all regressions and recheck checkpoint/VACUUM callers before removing the local companion. |
| Named-savepoint cancellation recovery `3ae0065e5` | `cancelled_plain_native_writes_undo_rows_and_preserve_caller_work` plus five core lifecycle regressions: rollback restores the selected prior poison state, allowing COMMIT/root RELEASE after canceled rows are undone; pre-existing poison and unrecovered abandonment still reject commit. [Review](proposals/cancellation-savepoint-poison.md). | Upstream restores transaction safety state after successful named-savepoint rollback, including prior poison, nested/same-name boundaries and mirrored rollback. Remove only when the frontend, lifecycle and savepoint regressions pass without this local patch. |
| MVCC savepoint commit resume (2026-11-02) | A resumed `RELEASE` that is committing finishes the commit instead of releasing again. Regressions: core `mvcc::test_named_savepoint_release_resumes_commit_after_auto_checkpoint` and `romedb-tests --test transactions atomic_requests_on_mvcc_files_survive_automatic_checkpoints`. | Upstream resumes `SavepointOp::Release` commits from `ProgramState::commit_state` and both regressions pass without the local patch. |
| Execution counters and budgets (2026-09-26, approved Cloud 0.3.0 dependency) | Successful rowid/index seeks, initial index-method positioning after completed query_start, and executed deferred table reads count exactly once; missing rows do not count. `index_method_meter` covers first/subsequent/empty FTS results, deferred payload reads, managed materialization, sticky limits and queued I/O; extension-internal traversal remains unqualified. Optional retained per-execution physical-work counters, VM-step budgets and read budgets with bounded overshoot; metered Count advances incrementally; HashBuild does not duplicate its source cursor visit; read/mutation-budget interrupts use cooperative cleanup to preserve caller transactions. Retained ordinary row mutation events distinguish replacement deletions, exclude CDC maintenance and survive rollback; active writers prevent scope replacement. Maintenance views share all work/budgets while suppressing only logical mutation events. Opt-in schema visits and an independent schema budget preserve default all-row counters; `schema_meter` qualifies DDL source/schema separation, rollback and queued I/O. `romedb-tests --test profile --test execution_meter --test mutation_meter --test schema_meter` includes failure, rollback, boundaries and queued I/O. [Review](proposals/execution-metering.md). | Upstream includes equivalent completed-position counters and retained execution/budget APIs, and all listed regressions pass without these patches. Known gap: reads inside an index-method extension's own traversal (FTS, ANN) have no qualification test, so their count may be incomplete. Cloud bills rows read and written from these counters and accepts that gap. |

The canceled-write recovery fix was separately approved on 2026-09-25 after the
first optimized package matrix exposed stale poison at COMMIT/root RELEASE.
The inspected upstream revision still lacks the named-frame state; no equivalent
upstream replacement has been qualified.

## Retired exception: WASI FTS

The user removed browser support from active V2 development. Commit `ae6777a17`
reverts `2ef619c07` in isolation, restoring the ten core files to their prior state.
Frontend WASI feature wiring, the browser package and its build/probe code are
also removed. This exception is **not active** and must not be reapplied on an
upstream sync. The reviewed patches remain historical evidence for any separately
requested future browser work.

## Upstream sync checklist

- [ ] Record the exact proposed upstream SHA and inspect its source, changes and
  issues/PRs for each exception. Add verified issue/PR URLs here when available.
- [ ] Compare behavior, not just patch text. Record per entry: retain, adapt, or
  remove; upstream evidence; candidate commit; and regression results.
- [ ] On an isolated sync branch, drop a local exception only after its regression
  passes using the upstream replacement. A patch conflict is not proof of a fix.
- [ ] If upstream is only partially equivalent, retain the necessary portion and
  document its revised scope. Materially new core behavior needs separate review.
- [ ] Run the affected upstream tests and combined RomeDB acceptance. For storage
  changes include native upgrade/restore and fault checks. Browser/WASI checks
  are outside the active native scope.
- [ ] Update this register, UPSTREAM.md, proposal status and release provenance in
  the sync commit. Preserve the regression even when the local patch is removed.

The scalar-error patch and retired WASI patch remain byte-for-byte available
in their review files. Their SHA-256 values are respectively
`953e88426f9c5e5ea52a3f41251c7e64bc0b2bb06c70557f58ff596454c68b0a`
and `b6771224bade4381592906cbc16962add85819f1881e56860bc02a10a992d881`.
The frontend WASI wiring patch is
`68adbabbe59c6c2b6b160aa3b12f046e8af17bef2aad6f790d1bddb9b9efa0a1`.
