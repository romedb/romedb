# V1 resource-control scope (S5)

The master plan requires embedded execution/resource-limit tests. RomeQL also
requires batched forward fetch with query limits. Neither plan explicitly commits
to CLI row streaming or a hard process-wide allocator cap. Those improvements
remain follow-ups; they must not be inferred as additional stable release gates.

Current controls and their evidence:

| Control | Boundary | Implementation/evidence |
|---|---|---|
| Parsed input | 16 MiB, 262,144 tokens and 64 open delimiters | Parser guards and scoped parser regressions; documented in RomeQL.md |
| Retained results and write candidates | Explicit row and encoded-byte budgets; bounded writes roll back on budget errors | ResultLimits APIs, result_limits integration tests and Node result-limit regressions |
| Cancellation/deadlines | Cooperative execution checks, not hard CPU preemption | interrupt.rs and Node AbortSignal/deadline tests; trigger defect is tracked separately under S3 |
| Forward fetch | Batched reads and 64 MiB resolver budget, plus result-budget accounting | frontend/src/links.rs and its budget/rollback/cancellation tests |
| Document transfer | 64 MiB input/default export bound and document-count checks | frontend/src/transfer.rs boundary and atomic-import tests |
| Bundled QuickJS | 64 KiB string input/output, 8 MiB runtime memory, 256 KiB stack, 100 ms cooperative interrupt deadline | frontend/src/bundled.rs includes timeout, allocation, stack and absent external-I/O probes |

The source milestone's complete scoped suite passed (674 Rust and 103 Node tests,
one known ignored trigger test). This supplies existing runtime-control evidence;
no new test run is claimed for this review. Final candidate acceptance must retain
these tests. Package evidence is tracked separately under S6.

These are separate bounds, not a sum that caps process memory. Results can
materialize before limits are checked; engine working sets, temporary values and
native allocations are not globally bounded. Concurrent callers can consume more
memory. The CLI and SDK must keep these limitations visible. The million-vector
measurement under S7 evaluates one workload, not a universal safe-memory bound.

S5 scope reconciliation is complete. The final candidate still needs its normal
scoped acceptance; no speculative allocator or streaming redesign is required by
this interpretation of the current master plan. A later explicit requirement can
change this scope.
