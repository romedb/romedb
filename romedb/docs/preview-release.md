# V1 release preview checklist

This is the active delivery plan. The preview is a usable embedded evaluation
release, not the stable V1 release described in the parent RomeDB.md master plan.
The full V1 scope stays intact. Existing implementation is the starting point;
preview work closes the finite requirements below rather than expanding a SQL
qualification matrix.

Initial candidate target: Linux x64, Node 22/24, Rust 1.88.0 and the CLI. This
matches the available local evidence. Additional platforms require their own
artifacts and evidence before being advertised. No registry publication or remote
release has occurred.

## Requirements and completion evidence

| ID | Requirement | Done when | Current state |
|---|---|---|---|
| P0 | Freeze preview scope and work policy | This checklist is linked from the README, status and gate review; session handoff uses it | Complete |
| P1 | One usable application workflow | The task tracker demonstrates migrations, typed CRUD, validation/indexes, relational events, a transaction, one-hop fetch, close/reopen and export/import into a fresh database; its focused test verifies exact restored data and index integrity | Complete — [application/storage evidence](preview-storage-evidence.md) |
| P2 | Essential storage correctness | Existing persistence, migration, transfer and bounded process-kill recovery suites pass for the candidate; committed data and indexes survive reopen, failed writes are atomic, and restore yields matching data/indexes | Complete — [application/storage evidence](preview-storage-evidence.md) |
| P3 | Preview contract and getting started | One guide gives exact install/build commands, a persistent application example, result/error/transaction handling and a finite known-limitations list; each advertised capability has an existing runnable example or test | Complete — [quickstart](preview-quickstart.md) and [delivery evidence](status.md) |
| P4 | Installable candidate artifacts | A candidate directory contains the CLI, Node tarball, pinned source/Rust-consumer instructions, licenses/notices, checksums and exact source/toolchain identity; local artifact smoke succeeds for Node 22/24 and standalone Rust | Complete — [candidate delivery evidence](status.md) |
| P5 | Candidate acceptance | Run romedb/scripts/check.sh once on the candidate source, record exact source and results, classify the one known ignored trigger-cancellation test, and confirm P1–P4 evidence still matches the candidate | Complete — [final acceptance](status.md) |
| P6 | Reviewable release handoff | Candidate files, quickstart, change summary and limitations are linked in one manifest; every P0–P5 row has evidence and no unresolved blocker | Complete — [preview handoff](status.md); local candidate only |

Completed milestone: **Application and storage readiness (P1 + P2)**.
Completed milestone: **Candidate delivery (P3 + P4)**.
Completed milestone: **Acceptance/handoff (P5 + P6)**. All preview requirements are closed. Finish each milestone before a completion report. Share brief progress updates
while working; do not stop after each fixture or small commit.

## Essential checks and stopping rules

- Every implementation/test task names a requirement above or a reproduced bug
  blocking it. A proposed task without either stays in the backlog.
- Preserve data/index consistency, atomic failed writes, committed persistence,
  restore correctness and regression coverage for actual bugs. A known violation
  in the advertised workflow blocks the preview; passing unrelated tests cannot
  waive it.
- Use focused tests while implementing. Run the full RomeDB-scoped suite at
  milestone boundaries. Run package/platform checks when producing or changing
  artifacts, not after unrelated frontend edits.
- Reuse existing passing evidence when source and relevant dependencies are
  unchanged. A completed row reopens only for a concrete regression, dependency
  change affecting it, or an explicit scope change.
- Do not add speculative combinations, benchmark scales or platforms to close a
  row. Record evidence and mark it done once its stated acceptance passes.
- Do not use “broader qualification remains open” as a preview blocker. Record
  the specific stable-V1 follow-up separately. Changes to this checklist must
  state the user/application need; do not silently add gates.

## Preview limitations, not automatic work items

- Collection ON CONFLICT, explicit INDEXED BY, UPDATE/DELETE target index hints,
  remaining joined UPDATE USING/NATURAL/FULL/non-leading RIGHT forms, and other
  unimplemented SQL forms are unsupported. Prefer documented supported queries.
  The unfinished NOT INDEXED write-target extension is parked.
- Results materialize in memory. Available input, candidate, result and deadline
  controls are not a total process-memory cap. CLI row streaming and global
  resource accounting are stable-V1 follow-ups.
- Pinned trigger cancellation has a known Interrupt-to-Busy defect. Do not
  advertise cancellation of trigger-bearing writes as supported. P5 must make
  this visible in release notes; if the preview workflow depends on it, fix or
  remove that dependency before acceptance.
- No complete SQLite compatibility, production readiness, migration-free schema
  evolution, power-loss guarantee or untested binary-upgrade guarantee is made.
  Existing bounded recovery/restore checks remain required by P2.
- Exact vectors and bundled string functions are available within documented
  implementations; no million-vector latency or broader runtime/platform claim.
- No Windows/macOS/prebuild matrix, public npm/crates.io availability, cloud,
  sync, inverse links, ANN/FTS/spatial indexes or user JavaScript is advertised.
- External pilots and wider performance/upgrade/platform work remain in the
  stable-V1 plan. The preview enables that feedback; collecting it does not block
  assembling the preview.

See [contracts](contracts.md), [application guide](ai-application-guide.md),
[engine provenance](../UPSTREAM.md) and [stable-V1 gate backlog](v1-gates.md).
