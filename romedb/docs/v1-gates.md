# Embedded V1 gate review — 2026-09-09

The [V1 release preview checklist](preview-release.md) now governs delivery priority. This document is the stable-V1 backlog and historical evidence, not an open-ended preview checklist.

This is a navigation and prioritization aid, not a replacement for the parent RomeDB.md and RomeQL.md plans. The current implementation is not release-complete. Most recent complete scoped evidence: 644 passing Rust tests with one ignored trigger-cancellation gate, 89 passing Node/application tests, formatting, Clippy and strict TypeScript (`/tmp/romedb-update-rollback-check.log`, based on `49253c971` plus UPDATE OR ROLLBACK support). These totals do not prove the gates below. Installed-package evidence is separate: Linux Node 22/24 now cover leading RIGHT/LEFT UPDATE FROM, target-alias CTEs and pagination. The standalone Rust consumer also covers joined RIGHT/LEFT UPDATE FROM, target-alias CTEs, pagination and committed reopen. See verification.md for exact runs and limitations.

| Required area | Current evidence | What still prevents a completion claim |
|---|---|---|
| Pinned dialect and grammar | UPSTREAM.md; parser tests; tests/tests/sql_compat.rs and scalar_subqueries.rs | Mixed collection/native query coverage, correlation, remaining aliases/type propagation, managed-name authorization and broader baseline differential qualification |
| Documents, validation and indexes | CRUD, checks, catalog, returning, insert_select and transaction suites | Broader write forms and dependency access; complete concurrent schema/resource qualification |
| Value/query/result contract | contracts.md; types and expression tests; Node lossless value and transfer tests | Complete expression propagation, ordering/index equivalence across supported forms; stable release result/error contract |
| Transactions and cancellation | interrupt.rs regressions; Node operation tokens and queue/close tests | Ignored pinned trigger Interrupt-to-Busy defect; ambiguous/interrupted commit qualification, deadlines and full lifecycle/platform coverage |
| One-hop forward links | tests/tests/links.rs; documented snapshot and batching probes | Initial per-resolver encoded-value bounds exist; target batch/row/VM counters exist; total query/outer-result memory accounting and detailed target plans remain open |
| Exact vectors | tests/tests/vectors.rs; benchmark reports | Broader numerical/platform qualification and representative high-dimensional 100k–1m evaluation, without a promised latency target |
| Bundled QuickJS functions | Fixed slugify/normalize implementation and function tests | Runtime/platform/resource/performance release qualification; user JS remains outside V1 |
| Recovery, backup and upgrades | Process-kill tests, offline restore rehearsal, legacy catalog fixtures | Interrupted I/O/commit/checkpoint matrix, previous released-binary upgrade/restore and advertised-platform evidence |
| CLI and developer tools | Script/terminal/inspection/migration/transfer tests | Row streaming, total resource accounting, terminal/platform and complete tool qualification |
| Rust and Node distribution | Offline Rust consumer; offline Node tarball consumer and declarations | Release artifacts/notices, platform/Node matrix, prebuild selection, registry readiness; no publishing has occurred |
| Application validation | Master-plan requirement | An initial task-tracker template and ai-application-guide.md workflow are tested; external developer/pilot evidence and broader application-pattern qualification remain missing |

Cloud beta requirements remain deferred until after embedded V1. They do not block embedded implementation. V2 inverse links, indexed ANN/FTS/spatial and user scripts are not substitutes for unfinished V1 work.

## Current implementation priorities — 2026-09-09

The active SQL write gaps include UPDATE FROM source USING/NATURAL/FULL and
non-leading RIGHT joins, and broader scope/planner qualification. See [the implementation design](update-from.md). Initial
single-source, derived/CTE, inner/left joins and leading RIGHT sources with ON or no constraint execute;
repeated encoded typed record IDs collapse before pagination and mutation. The deduplication, pagination position-list and candidate-selection loops have
cooperative progress checks, including OFFSET-discarded candidates. It still needs complete workspace accounting and broader
cancellation/recovery coverage.

Ordinary collection UPDATE/DELETE LIMIT/OFFSET now execute, with missing-binding
validation, native coercion/error comparisons, RETURNING and rollback/index
coverage. Initial FROM pagination also executes after assignment evaluation and
duplicate resolution. Tuple writes now include
explicit values, single-row VALUES and the qualified SELECT subset, including
SELECT-arm compounds. Multi-row VALUES and compound VALUES arms remain rejected;
pinned native multi-row tuple behavior includes a correlated compiler panic.
See [contracts.md](contracts.md) for current supported behavior and alias limits.

Choose the next implementation against these remaining requirements; do not treat
the historical failures below as current reproductions. SQL progress does not
close the independent total-resource, deterministic interrupted-I/O, previous-
release upgrade/restore, platform/distribution or external-application gates.
The unresolved trigger-interruption core proposal remains separately reviewable;
no upstream patch has been applied.

## Historical SQL priority review — 2026-09-08

Rechecked the current debug Node addon built by the complete scoped run at `e7d7dc863`; subsequent commits contain test/documentation changes. Fixture: `docs` and `native(n INTEGER)` contain 1,2,3; `keys(n INTEGER)` contains 1,4. The two local-WITH/alias-collision SELECT probes were rerun in a disposable in-memory database; raw output: `/tmp/romedb-gate-recheck.log`. Other rows retain their earlier evidence and are labeled accordingly.

| Probe | Pinned ordinary SQL | Collection route | Next action |
|---|---|---|---|
| Qualified outer scalar: `SELECT d.n,(SELECT max(n) FROM native WHERE n<d.n) FROM source d ORDER BY d.n` | `(1,NULL),(2,1),(3,2)` | Same | Resolved example; broaden only against a concrete uncovered requirement |
| Basic WITH UPDATE selecting key 2 | Returns 12, affects 1 | Same | Resolved example |
| Chained CTE named after the UPDATE target | Returns 11,12,13, affects 3 | Initial direct-FROM nonrecursive CTE fix now matches | Qualify nested/compound CTE scopes, parameters and failure recovery |
| Inner WITH scalar below | `(1,NULL),(4,3)` | Same after local-WITH scope fix | Broaden local-WITH parameters, nested scope, callback and write-recovery qualification |
| Inner derived alias collision below | `(1,0),(4,2)` | Same, rechecked | Resolved example; existing alias regression coverage applies |
| Source-free scalar UNION ALL using outer keys | Native rejects compound WHERE-clause subquery | Collection rejects unresolved key | Preserve the pinned limitation; do not treat this probe as a required new compound capability |

Reference probes (all three now have implementation evidence; broader scope qualification remains open):

```sql
WITH docs AS (SELECT 2 AS n), chosen AS (SELECT n FROM docs)
UPDATE docs SET n=n+10 WHERE n IN (SELECT n FROM chosen) RETURNING n;

SELECT n,(WITH chosen AS (SELECT n AS m FROM native)
          SELECT max(m) FROM chosen WHERE m<n) AS prior
FROM docs a RIGHT JOIN keys b USING(n) ORDER BY n;

SELECT n,(SELECT max(m) FROM (SELECT 0 AS m UNION ALL SELECT 2) b
          WHERE m<n) AS v
FROM docs a RIGHT JOIN keys b USING(n) ORDER BY n;
```

For the ordinary oracle, replace the outer/write `docs` source with `native`; in the first query also rename the target-named CTE and its reference to `native`. All three reference probes now have implementation evidence; none provides complete release evidence. Initial write-context lowering now fixes FROM references in nonrecursive CTE bodies, including derived FROM wrappers and compound arms, preserving target-alias binding. The successful 696-case UPDATE/DELETE matrix covers direct, derived-wrapper, UNION ALL, UNION, INTERSECT, EXCEPT, EXISTS and IN-filtered bodies with default/MATERIALIZED/NOT MATERIALIZED chosen CTEs, plain/arithmetic/aggregate reads, returned rows, affected counts, final indexed data and rollback. Missing-parameter and uniqueness-failure recovery now preserve prior transaction work and valid indexes, with corrected retry and rollback checks for collection-name and target-alias collisions. Expression-subquery traversal now preserves target binding, with distinct generated membership names preventing the reproduced nested-IN preparation crash. Nested WITH directly inside write CTE definitions now uses scoped flattening; 24 native-rejected alias cases remain outside the comparison evidence. See cte-write-binding.md. Correlated/recursive/deeper scopes and broader failure/resource qualification remain open because an incorrect rewrite can select a different mutation set. The resource, recovery, packaging/platform and application gates above remain full V1 requirements.

## Historical SQL probes and implementation notes

Probed the locally built addon after 41a2daf95 using matching `docs` collection and `native(n INTEGER)` rows 1, 2, 3. Each probe ran inside BEGIN/ROLLBACK, so UPDATE probes did not affect later comparisons.

| Probe | Ordinary table | Collection |
|---|---|---|
| Correlated scalar below | (1,NULL), (2,1), (3,2) | FDB_ENGINE: no such table: d |
| Leading-WITH UPDATE below | RETURNING 12; affected 1 | FDB_UNSUPPORTED: this collection UPDATE clause |
| `SELECT n%2 AS parity,count(*) AS total FROM source GROUP BY parity HAVING total>1 ORDER BY parity` | (1,2) | (1,2) |

```sql
SELECT d.n,(SELECT max(n) FROM native WHERE n<d.n) AS prior
FROM docs d ORDER BY d.n;

WITH chosen AS (SELECT 2 AS n)
UPDATE docs SET n=n+10 WHERE n IN (SELECT n FROM chosen) RETURNING n;
```

Replace `docs` with `native` for the ordinary-table forms. These probes are narrow current behavior evidence, not complete grouping or correlation qualification.

The leading-WITH rejection above is a pre-change probe. Initial collection UPDATE/DELETE support now carries CTE scope into candidate SELECT lowering and preserves pre-mutation materialization and atomic writes. Tests cover native/collection CTE membership, self-reads, RETURNING, uniqueness failure/retry/rollback and mutation cancellation. Remaining work includes same-name CTE resolution (the chained target-name fixture fails preparation), broader assignments/RETURNING, validation/type coverage and other write clauses. Correlated scalar lowering remains a separate gap.

The original correlated scalar probe above now passes for native inner SELECT predicates: metadata preparation substitutes outer references in a disposable probe, and runtime predicates lower qualified outer fields. Scalar/EXISTS WHERE, JOIN ON and HAVING forms have initial coverage; inner collection sources, deeper scopes and non-predicate correlation remain open. Preserve correlation inside engine execution rather than pre-executing a correlated source once or caching it as an uncorrelated value. Test per-row native oracles, NULL/empty sources, alias shadowing, type/affinity/collation, parameter use and atomic writes.

Recent resource work adds incremental transfer parsing/encoding, incremental lowered SELECT decoding, fetch reference/encoded-byte budgets, and target batch/row/VM profiling. Public results still materialize; these are not complete memory budgets. Further resource work remains necessary but does not replace unfinished SQL semantics.

The earlier native membership reproducer is now covered by implemented lowering and differential/read-write/CTE tests. Reuse that evidence; do not treat its old failure as current behavior. The core trigger proposal remains separately reviewable under the workflow and has not been applied. Full embedded scope, packaging/recovery/platform gates and external application validation remain intact.


## Same-name CTE write oracle

A current pinned native regression uses `native(n INTEGER)` rows 1,2,3 and the prefix `WITH native AS (SELECT 2 AS n), chosen AS (SELECT n FROM native)`. A qualified candidate `SELECT n FROM main.native WHERE n IN (SELECT n FROM chosen)` returns only 2. The corresponding UPDATE increments all three rows to 11,12,13, and DELETE returns all three original rows. Both writes affect 3. This is measured pinned behavior; it is not general SQLite shadowing semantics.

Consequently, making the existing collection candidate SELECT prepare successfully is insufficient: it could silently select a different write set. The next same-name fix must preserve the native write-context binding when lowering to candidates, and qualify aliases, nested scopes and parameters. Current collection chained same-name forms still fail preparation. The related native SELECT guard gap now has initial CTE declaration/unqualified-FROM redaction, with simple/chained/derived-source tests. Qualified expression and deeper scope coverage remains open.


Alias qualification extends the oracle: with `UPDATE native AS target`, a CTE named `native` yields only 12, while a CTE named `target` yields 11,12,13. The collision follows the exposed target identifier, not merely the physical table name. A derived wrapper around the candidate SELECT still yields only 2 and is not a solution.

Relevant pinned engine code: core/translate/update.rs builds target_table with tbl_name.identifier(), plans FROM/CTE references separately, then prepends the target into read_scope_tables; core/translate/delete.rs adds the target before plan_ctes_as_outer_refs; core/translate/planner.rs retains CTE ASTs for replanning and resolves current CTE definitions separately from outer references. A frontend fix must preserve this lookup context and avoid blanket replacement of a target-named CTE: aliases change which name collides, and original CTE validation/parameter behavior must remain accounted for. No core changes were made.


The predicate-correlation regression matrix additionally checks native scalar affinity and pinned correlated-result collation behavior. This advances the dialect gate without closing general correlation or the other embedded release gates. See status.md and verification.md for current test counts.


Native correlated IN/NOT IN now keeps its RHS inside the per-row expression instead of hoisting it into outer WITH scope. Initial native NULL/affinity/collation, evaluation-count and atomic-write comparisons pass; this does not close general correlated SQL qualification.


Nested outer document paths now use the supported predicate correlation route, including derived collection columns. Non-object derived parents return missing fields through a separate accessor; physical stored-document roots remain strict. General nested query scopes and inner collection correlation remain open.


Unique atomic savepoint identities fix a reproduced partial-import cancellation failure; a deterministic nested boundary sweep and repeated Node cancellation runs qualify that fix. Opening I/O failures and RELEASE/commit outcome boundaries remain open. See atomic-savepoints.md.


Cancelled atomic opening now removes an opened empty frame and restores the initial transaction state in the deterministic progress-boundary sweep. Exact absent-frame handling is confined to opening failure before callback execution; unverifiable cleanup remains FDB_ROLLBACK.


A targeted RELEASE sweep adds a 310th distinct Rust regression after the latest full scoped run. It verifies exact complete/restored write sets at pinned in-memory progress boundaries, including FDB_ROLLBACK with completed pending writes. Interrupted I/O and broader commit-outcome qualification remain open.


## Completed 100,000 × 768 vector diagnostic (2026-09-07)

The seeded-vector benchmark at clean commit `bc88ad618b` exited successfully, checking filter counts/index use and all warmup/measured exact top-10 results against an independent float32-coordinate cosine reference. Debug CLI medians: 82.45 s unindexed filter, 1.12 s indexed filter, 388.11 s exact top-10; three measured samples per workload. Report identities, medians and repeated primary counters were verified. See [benchmarks.md](benchmarks.md) for the raw report, command and the nonmonotonic VmHWM accounting caveat. Optimized/real-distribution/platform/resource qualification remains open; this diagnostic does not close the vector release gate or full V1.


## Optimized vector diagnostic (2026-09-07)

The existing release-profile CLI built successfully with Rust 1.88.0 and completed the same seeded 100,000 × 768 diagnostic at clean commit `83583ecdd`. All warmup/sample count and cosine-reference checks passed. Medians were 9.48 s unindexed filter, 138.64 ms indexed filter and 47.12 s exact top-10. Reference data, engine counters and database size match the debug run; binary identity and medians were verified. See [benchmarks.md](benchmarks.md) for commands, comparison and recurring nonmonotonic VmHWM observations. This adds optimized-build evidence but leaves real workloads, broader platform/scale/resource qualification and full V1 incomplete.


## Combined accessor large-vector verification (2026-09-07)

The release-profile 100,000 × 768 seeded run at clean `3cac94cae` passed all warmup/sample reference checks. Exact top-10 median was 32.71 seconds versus 47.12 seconds before; primary VM steps fell from 1,200,080 to 1,100,080 while all 100,000 vectors are still scanned. Binary identity, reference values, sample medians and repeated counters were checked. See [benchmarks.md](benchmarks.md) for report, command and measurement limitations. Substantial latency, whole-document decoding, real-workload/platform/resource qualification and full V1 remain open.


Qualified outer fields now work in simple native inner SELECT projections, with typed logical results and preserved explicit CAST affinity. Scalar/EXISTS/IN, type/empty/parameter/alias cases and atomic writes have initial regression coverage; the complete scoped run passed 315 Rust and 35 Node tests. See status.md. This closes the recorded initial projection probes, not general correlation: inner collections, deeper/local-WITH/compound scopes and other expression positions remain open.


Initial correlated ORDER BY expressions now match native scalar/IN/EXISTS probes through execute/profile, including NULL placement and multiple sort keys. The complete scoped run passed 316 Rust and 35 Node tests. Tested native GROUP BY/LIMIT outer references retain rejection; broader correlation and ordering qualification remain open. See status.md.


Correlated typed projection aliases/ordinals now have logical ordering with projection reuse and initial LIMIT/OFFSET qualification. Volatile-function execute/profile probes preserve native call counts, including LIMIT 0. Mixed DISTINCT ordering with an additional ordinary key remains explicitly unsupported to preserve duplicate semantics. Broader correlated ordering/alias/type qualification remains open; see status.md.


Covered correlated ORDER BY alias arithmetic/function expressions now use logical values and match pinned native alias precedence across tables, views and inherited CTEs. The complete scoped check passed 321 Rust and 35 Node tests; broader correlated scope/type/resource qualification remains open. See status.md.


Mixed DISTINCT ordering of a single correlated typed projection now deduplicates only the projected value, with hidden sort keys excluded. Native differential pagination and volatile-evaluation tests pass; the complete scoped run passed 322 Rust and 35 Node tests. This supersedes the earlier mixed-ordering rejection. Broader DISTINCT type/collation semantics and full V1 remain open.


Sorted correlated typed DISTINCT now compares logical SQL values and retains a typed representative, correcting integer/real duplicate pagination. Native scalar/membership execute/profile tests cover insertion order and NULLs; the complete scoped check passed 323 Rust and 35 Node tests. Unsorted correlated DISTINCT, broader collation/type semantics and full V1 remain open.


A derived pagination boundary now preserves bound LIMIT/OFFSET in the covered sorted correlated typed projections despite the pinned scalar compiler replacing non-literal limits. Literal-native differential tests and both installed Node clients pass; full scoped checks passed 325 Rust and 35 Node tests. Other pagination forms and broader release qualification remain open.


Integer bound pagination now has scalar/IN/EXISTS coverage across supported predicate-only, native scalar, CAST and typed CASE correlated sources. Shared correlation tracking and integer-literal lowering address zero-limit replacement and per-outer-row bound-counter reuse. The full scoped check passed 327 Rust and 35 Node tests; other pagination parameter types/expressions and broader V1 gates remain open.


Integral real pagination binds now follow the integer-literal correlation path within the pinned engine's exact conversion bounds. Per-row scalar/IN/EXISTS comparisons and endpoint rejections pass; the complete scoped run passed 329 Rust and 35 Node tests. Other coercions, complex pagination and full V1 remain open.


Async startup failure coverage now includes pre-ready transport error/exit/messageerror and repeated real failed-open/retry/reopen behavior. All 35 Node binding tests pass; broader lifecycle/platform and release gates remain open.


Public Node closed/closing operations now expose FDB_CLOSED without transaction observations, with FDB_WORKER precedence for established worker failures. All 37 Node/application tests, strict TypeScript and the offline installed-package smoke pass. Broader cross-client error and release qualification remain open.


Node exactlyOne now exposes FDB_CARDINALITY with completed-statement transaction observations while retaining RangeError. Write-effect and rollback regressions, all 38 Node/application tests, strict TypeScript and installed-package assertions pass. The helper does not undo completed writes; broader error/release qualification remains open.


Node RomeDBError and isRomeDBError now support runtime recognition and safe narrowing of caught unknown errors, with optional transaction observations. All 40 Node/application tests, strict TypeScript and installed-package guard/declaration checks pass. Broader error and release qualification remains open.


Correlated single-column typed DISTINCT now deduplicates logical values independently of projection-alias sorting, correcting source-column pagination and unordered result exhaustion. Native differential execute/profile coverage and the full scoped check pass: 332 Rust tests, 42 Node/application tests, formatting, Clippy and strict TypeScript; one known trigger gate remains ignored. Broader correlation/type/resource and release gates remain open.


Mixed compound-arm collation remains a reproduced query-correctness gap: see mixed-compound-collation.sql and the corresponding verification entry. Native literal-left IS and native-left IN choose differently for the covered BINARY/NOCASE UNION ALL source; current logical lowering disagrees with both. A single rightmost-arm collation override does not close this gate.


The mixed-compound reproducer above now matches native IS and IN results after distinguishing exposed-column and expression collation metadata. Both arm orders and reversed IS/IS NOT operands have regression coverage; full scoped checks pass 403 Rust/44 Node tests, and the rebuilt CLI reproducer passes. This resolves the recorded probe, not broader compound/collation or full V1 qualification.


## Local-WITH scope progress after collation qualification

The local-WITH merged-key reproducer now returns `(1,NULL),(4,3)` through the collection route. Eighteen native/mixed comparisons cover three join forms, three materialization modes and local-column shadowing. Metadata inspection resolves native CTE schemas before qualifying the outer key, and native correlation retains the local WITH scope. The complete scoped run passes 507 Rust tests with one existing ignored gate and 63 Node/application tests (`/tmp/romedb-local-cte-scope-check.log`). Next qualify parameters, deeper/local CTE scopes, callback evaluation and write-source atomicity. General result-memory budgets, interrupted I/O/commit/checkpoint qualification and distribution/platform gates remain separate unfinished V1 requirements.


## Result-budget implementation inventory

[result-budgets.md](result-budgets.md) records the public/internal collection paths and implemented explicit bounded SELECT APIs in Rust and both Node clients, including cooperative worker cancellation. One-hop FETCH now shares the final payload budget before duplicate output cloning. Explicit atomic RETURNING limits and opt-in collection candidate/snapshot limits now exist in Rust, Node and CLI; see result-budgets.md for exact coverage. Engine working memory, temporary decoding/transport/fetch workspace peaks and complete deadline qualification remain unfinished resource requirements. These APIs do not close the V1 resource gate.
