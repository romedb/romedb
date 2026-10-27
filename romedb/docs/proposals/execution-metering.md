# Execution metering exception

Current status (2026-09-30): **retained and adapted to Turso 0.8.1**. Upstream
statement metrics do not replace the retained work/budget APIs or their completed
cursor-visit and mutation events. The inspected upstream main adds a whole-table
DELETE optimization using `ClearBtree`; a future sync must extend metering or
preserve the metered row path before adopting it. That optimization is absent
from the pinned release. See the
[current exception audit](../core-exception-audit-2026-09-30.md).

Approved by the user on 2026-09-26 for Cloud 0.3.0. This work does not reopen the
completed native 2.1.0 release or change its historical acceptance record.

## Initial index-method positioning continuation (2026-09-27)

The approved successful-positioning correction also applies to IndexMethodQuery.
Its first successful query_start previously advanced without a read event, while
Next counted subsequent hits. One score-only FTS result reported zero reads;
five scored hits with deferred table lookups reported nine rather than ten.
The patch records exactly one successful first position after I/O completion.
Empty results and unfinished I/O do not add visits. Default statement profiling
and retained execution meters share the corrected event and budget interruption.

`index_method_meter` independently checks one index visit per score-only result,
two visits when the actual table payload is read, and managed FTS's three phases:
index results, source-table lookups and materialized-hit visits. Five managed
matches count fifteen visits even with limit one: stable ordering scans all five
materialized hits before limiting output. The tests assert actual index-method
plans, empty/missing results, memory/file operation, exact and short budgets,
sticky failure, caller rollback preservation, and repeated queued I/O polls.
All four counter regressions fail on the unmodified base. Two earlier fixture
assumptions were corrected before that red run: native score access uses Column,
and managed LIMIT does not avoid the materialized-hit scan.

This counts cursor positions exposed by the index method. Tantivy posting/search
work and fast-field collection occur inside query_start before the first exposed
position and remain unqualified. The change does not certify complete search
attribution or make a VM/read limit a bound on all extension CPU or memory.
Keep it in an isolated continuation of the registered counter exception, with
the named regression and the upstream index-method suite. Validation passed four
focused cases,833 scoped Rust tests,121 Node tests,5 C ABI tests and31 upstream
index-method tests, plus scoped formatting/Clippy and Node types. The first full
script completed its Rust checks but could not find Node on PATH; the remaining
Node/type/C ABI components were then run successfully with the corrected PATH.

The released counters undercount seek-based reads. A forty-row indexed self-join
returned forty matches but reported rows_read=39 and btree_seeks=41. B-tree seeks
cannot simply be added to rows_read: missing keys and deferred-but-unused work
must not be billed as row visits.

The first isolated patch counts a completed successful rowid seek, successful
shared index/range positioning, and an actually executed deferred table seek.
It increments only after I/O completion, never on the yielded I/O path. Failed
lookups do not add a row. Existing query results and seek statistics are unchanged.
Generic engine counters contain no organization, price or cloud-specific state.

Regression: `cargo test --locked -p romedb-tests --test profile` covers memory and
file-backed successful/missing point seeks, ascending/descending range seeks,
covering/non-covering indexes and the forty-row indexed join (eighty visits).
Existing scan-reduction and result-equivalence checks remain in that suite.
The private cloud has a separate native diagnostic reproducer and runtime suite.

A second isolated patch adds `execution_meter::ExecutionMeter`, attached through
`Connection::set_execution_meter`. Each program captures the optional Arc when
execution starts. Completed row-read/write events update it directly, so snapshots
survive statement errors, interruption, reset, rollback and drop. It preserves
physical write work on rollback; those counters must never be described as
committed logical mutations. All programs, including internal programs, contribute.
The existing statement and connection profiling APIs remain unchanged.

An optional VM-step budget reserves each dispatch atomically before execution.
Zero interrupts before dispatch, exactly enough permits completion, and exhaustion
uses the existing interrupt/abort path. The budget spans sequential statements;
replacing a meter while a root statement is active is rejected. Callers must
serialize replacement with execution and detach before unmetered cleanup. Final
snapshots require quiescent execution; concurrent field reads are provisional.
Waiting on unfinished I/O does not consume additional VM steps. Parsing, planning
and work within an opcode are not bounded by this facility.

`romedb-tests --test execution_meter` covers memory/file-backed failure retention,
partial-result drop, reset/rebinding, explicit rollback, zero and exact VM limits,
interrupted physical writes with rollback, EXPLAIN modes, and deterministic queued
I/O polls/resumption using the existing upstream I/O test harness. Adjacent profile,
deadline, cross-thread interrupt, result-limit and transaction regressions pass.

A third isolated patch adds optional read budgets. Each completed instrumented
visit is retained, then an over-budget event interrupts immediately. The scope is
sticky: subsequent statements cannot resume work after exhaustion. In a serialized
execution the maximum overshoot is one counted visit, including zero-budget
execution that reaches an existing row. Missing rows and empty scans count zero.
Sharing a meter across concurrent VMs permits one crossing visit per active VM;
cloud admission must serialize each execution and include the documented bound.
The approved cloud plan explicitly permits documented bounded overshoot. Counts
are never clipped, and the ledger must settle the actual retained value.

With a meter attached, exact Count opcodes advance one row per VM dispatch rather
than reporting the whole count after an uninterruptible batch. The phase survives
I/O and resets with the statement. VM/deadline cancellation remains available
between advances. Default unmetered Count retains its optimized path. Tests cover
exact/short/zero read budgets for table scans, covering and deferred index reads,
Count results/reset/empty input, interrupted writes, and partial Count reads on
injected I/O failure. Waiting for pending I/O never repeats completed reads.

This bounds the existing instrumented visits; it does not establish complete
billable coverage or bound arbitrary work inside virtual-table extensions.
Complete cursor coverage (including hash joins), managed-write/linked-fetch
attribution, internal-work exclusion, DDL accounting and durable cloud settlement
remain required before paid activation.
Hash joins, sort/materialized paths, virtual/search indexes and suspended-I/O
edge cases require their own broader accounting audit; this patch does not claim
complete billable counters for all paths.

Retain this exception until upstream counts successful cursor positioning and
actually performed deferred table reads equivalently and these regressions pass
without the patch. Review counter and budget changes separately from upstream
merges; preserve ancestry and maintain an isolated commit.


## Hash-build duplicate accounting

A fourth isolated correction removes the read event from HashBuild. That opcode
copies keys/payload already read into registers and uses the currently positioned
source rowid; it does not advance or seek the source cursor. Its Rewind/Next/seek
already recorded the visit. A verified non-spilling 100-by-100 hash join reported
300 visits with zero B-tree seeks and 198 full-scan steps; the correct total is
200 (two initial positions plus 198 advances).

Memory and file-backed regressions assert the HashBuild/HashProbe plan, unchanged
results for unique and duplicate keys (100 and 1,000 matches), zero source seeks,
and exactly 200 visits. A retained-meter regression succeeds at a 200-read budget
and interrupts at 199 with the crossing 200th visit retained. Hash-grace spill
paths, temporary/materialized traversal and virtual/search indexes still need
broader attribution qualification; this correction is not evidence for those paths.


## Read-budget interruption and caller transactions

Frontend integration exposed a distinction between engine error and cooperative
interruption cleanup: a read-budget error could roll back prior work in an
explicit transaction. The fifth isolated correction routes an Interrupt from an
already-interrupting program through the existing cooperative abort path, matching
VM/deadline cancellation. Ordinary engine errors retain their existing handling.
A raw-engine regression proves retained crossing reads, an active caller transaction,
prior writes still visible after interruption, and explicit ROLLBACK restoring the
original rows. Frontend tests also cover result-budget and linked-fetch failure.

## Checked frontend read adapter

`Connection::select_metered` accepts checked native/collection SELECTs and direct
record SELECTs, result limits and optional read/VM work limits. Its result retains
`ReadWork` alongside the success or error outcome. A connection-local scope shares
one meter across primary statements and forward/inverse linked target statements.
The meter is attached only for those statement executions. Catalog discovery,
preparation probes and savepoint cleanup remain outside it. A callback failure is
reset before detachment; cleanup failures remain explicit rollback errors. Calls
on a connection must remain serialized, as for existing frontend APIs.

Direct records use a checked primary-key lookup with LIMIT 1, preserving the
existing document result shape while avoiding an unnecessary next-index boundary
visit. Read/result-budget failures retain visits and permit later operations.
Tests prove failed-expression retention, metadata exclusion despite additional
catalog entries, direct-record missing/found behavior, result limits, zero VM
budget, mutation rejection, native/collection forward fetches, inverse fetches,
and preservation of prior transaction work across budget failures.

This adapter is a prerequisite for cloud receipts, not proof of complete billing
coverage. Physical counter coverage for spills/materialization/search still needs
qualification. Logical writes, DDL/import attribution, internal execution retries,
durable receipts and organization settlement remain separate release gates.

## Retained row mutations and mutation budgets

The next isolated continuation adds `row_mutations` and an optional
`max_row_mutations` limit. Ordinary B-tree inserts, matched updates (including
no-op updates) and deletes emit one event. Moving an updated row emits one event;
REPLACE emits a separate event for each conflicting row removed. Trigger writes
share the meter. SQL `changes()` and `total_changes()` retain their existing
semantics. Index opcodes, schema/ephemeral rows, internal sequence maintenance
and change-capture inserts do not add mutation events.

The crossing event is retained and interrupts immediately, with at most one
extra event per serialized VM. Exhaustion is sticky. Trigger budget failures
propagate through cooperative cancellation, undoing the interrupted statement
while preserving prior caller transaction work. Reset/drop an interrupted writer
before replacing or detaching its meter; the API rejects replacement while a
root statement or writer remains active. Waiting on pending I/O cannot repeat a
completed event.

These are completed mutation events, **not committed writes**. Rollback preserves
the diagnostic count. Cloud must verify transaction outcome before reporting
committed writes and must separately exclude managed-document index maintenance,
which uses ordinary hidden SQL tables. Virtual-table writes, materialized-view
attribution and DDL/import billing remain unqualified. This patch does not expose
a checked write adapter or enable cloud paid billing.

Five `mutation_meter` regressions cover memory/file operation counts, conflict
replacement, no-op/rowid updates, RETURNING, index/temporary-work exclusion,
zero/exact/short budgets, explicit rollback retention, trigger rollback and caller
transaction preservation, change-capture modes, and deterministic queued I/O.
The selected integration suites pass 167 distinct tests (the broad run contains
166; the final five-test mutation suite adds the queued-I/O regression), and the
final frontend library run passes 92 tests. No runtime artifact is rebuilt by
this engine-only milestone.

## Checked frontend write adapter

`Connection::write_metered` wraps the existing checked data-write and atomic
result-limit path. `WriteWorkLimits` shares read, mutation and VM limits across
source SELECTs, native DML, primary document storage operations, document lookups,
conflict lookups and CHECK expression execution. `WriteWork` retains attempted
reads/mutations on errors and after rollback. It intentionally has no
`committedWrites` field: a successful call within a caller transaction remains
pending that transaction's outcome.

Customer execution uses explicit statement scopes; catalog queries and savepoint
cleanup stay outside them. Managed physical index maintenance also remains outside
them at this stage, preventing extra logical mutation charges. Its read/VM work
needs a shared budget scope that suppresses only logical mutation attribution
before complete billable write coverage can be claimed. Virtual/search/materialized
paths and DDL/import policy remain separate qualification requirements.

Five `metered_writes` tests cover memory/file native and document mutations,
no-op updates, multiple replacement victims, scalar hidden-index exclusion,
source SELECTs, direct record UPSERT/UPDATE/DELETE, rejected DDL/transaction/read
statements, read/mutation/VM/result limits, late constraint failure, index rollback,
prior caller transaction preservation, and stable work counts as catalog entries
grow. The selected integration set passes 164 distinct tests (163 in the broad
run plus one additional object-write regression), and the final frontend library
suite passes 92 tests. Runtime binaries and cloud serving paths are not changed
by this frontend-only milestone.

## Shared maintenance attribution

`ExecutionMeter::without_row_mutations()` creates a view sharing the original
atomic counters and immutable limits. The view suppresses logical mutation events
only; completed reads, physical writes and VM steps still contribute. All budget
exhaustion is shared in both directions and cannot be bypassed by switching views.
The existing connection lifecycle guard and serialization requirement apply.
This is an attribution API, not an authorization or read-only boundary.

Twenty selected engine regressions pass, including a new maintenance-view test
proving shared snapshots, zero logical mutation allowance for internal writes,
physical work retention and read/VM/mutation exhaustion propagation. This isolated
core change is a continuation of the approved counter/budget exception.

## Frontend maintenance scopes

Managed scalar index inserts/deletes, full-text backing SQL/count updates and
vector entry/redo-log writes now execute in maintenance views. Their instrumented
read and VM work shares the customer scope without extra logical mutations.
Catalog and transaction cleanup remain unmetered. Scalar tests compare indexed
and unindexed document updates, interrupt after the primary mutation during
maintenance, verify data/index restoration, and check exact read/VM boundaries.
Full-text and vector insert/update/delete cases verify one logical mutation and
index integrity after both success and cancellation.

The combined selected regressions pass 167 distinct tests (153 in the broad run,
one added metered-write case and 13 ANN/full-text tests); the frontend library
passes 92 tests. These results do not establish complete extension work accounting:
ANN graph reconstruction/checkpoint reads and in-memory work, full-text internals,
other search/materialized paths and DDL/import policy still need qualification.
No cloud runtime artifact is rebuilt by this milestone.

## Separate engine schema visits (2026-09-27)

DDL exposed a remaining attribution gap: a five-row ordinary CREATE INDEX reports
seven retained reads, and subsequent five-row CREATE TABLE AS SELECT reports eight.
These are five source visits plus two/three engine schema visits, not extra customer
rows. CTAS already reports five logical mutation attempts; index construction reports
zero. Fixed subtraction is unsafe as the catalog grows or execution fails.

`ExecutionMeter::with_schema_read_limit(limits, max_schema_rows_read)` opts into
separate schema visits. Its `rows_read`/ordinary read budget excludes engine schema
visits; `schema_rows_read` and the additional budget retain those separately. Default
constructors still include schema visits in `rows_read`, with a diagnostic schema
subtotal. Statement metrics retain their existing all-rows behavior. No SQL results,
changes() behavior, storage format, or default execution limits change.

At the existing successful-position/advance/count sites, the compiler's cursor
identity (BTreeTable, sqlite_schema, root 1) determines schema work. Physical root 1
alone is insufficient because temporary pagers can reuse page numbers. Aliases
normalize to the same schema definition; application catalog tables remain ordinary
reads. The exact same visits update legacy statement metrics. Deferred reads use the
actual table cursor, not the referring index. Counting still occurs only after I/O
completion. The opt-in schema limit retains one crossing visit per serialized VM;
exhaustion is sticky across programs and maintenance views and uses cooperative
cancellation. Detach the meter before transaction cleanup as with other budgets.

This is engine attribution, not a checked DDL billing API. Frontend logical catalogs,
managed index builders, extension work and temporary/materialized work still require
separate policy/coverage. Cloud does not activate this mode in this patch. No claim
of all-DDL accounting or paid readiness follows from it.

Regression: `romedb-tests --test schema_meter` covers memory/file index build and
CTAS, default totals and statement metrics, catalog cursor modes/aliases, independent
read/schema/mutation limits, failed unique builds, empty/no-op schema work, sticky
maintenance views, rollback preserving prior caller writes, and queued I/O resumption.

Local qualification passed: 30 focused metering/profile regressions (including six
new schema tests); full RomeDB scoped checks with 803 Rust, 121 Node and five C ABI
tests, formatting, Clippy and Node types; 41 upstream statement-lifecycle regressions.
The initial inventory failed because schema visits were mixed with source reads;
the separated mode fixes that attribution without changing default totals. Two
intermediate compile attempts caught missing program-parameter renames; corrected
before all successful runs. No Cloud runtime rebuild or hosted activation occurred.
