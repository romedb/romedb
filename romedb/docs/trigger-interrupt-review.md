# Trigger interruption core exception for review

Approved explicitly by the user on 2026-09-14 and integrated unchanged as
ded389aea. The patch inventory is in ../UPSTREAM.md. The original isolated
validation below remains applicable to the unchanged core patch; current
combined-source verification passed as recorded below.

Combined verification completed after integration: 678 Rust tests passed with
zero ignored, 106 Node/application tests passed, formatting, Clippy and strict
TypeScript passed. Log: /tmp/romedb-approved-trigger-scoped.log. The previously
ignored after-write trigger regression passed. Together with the unchanged
patch's 98 upstream trigger tests and selected recovery-I/O evidence, this closes
the named S3 implementation requirement. Final artifacts must be rebuilt from
this integrated source; earlier review bundles do not contain the fix.

Candidate based on 9a82a36ea; isolated from main. Requirement S3: preserve
cancellation errors and transaction cleanup when a native trigger is interrupted.

The OpProgram executor currently maps both StepResult::Interrupt and Busy to
LimboError::Busy. The patch preserves saved subprogram state and returns Interrupt
for an interrupt; genuine contention continues returning Busy. No storage format
or public API changes. Frontend relabeling cannot safely distinguish the two.

Validation on Rust 1.88.0, Linux x64:
- Previously ignored after-write regression: passed, then enabled in the candidate.
- Pinned upstream trigger suite: 98 passed, zero failed.
- Full RomeDB scoped suite: 675 Rust tests passed, zero ignored; 103 Node tests
  passed; formatting, Clippy and strict TypeScript passed.

Logs: /tmp/romedb-trigger-review.log, /tmp/romedb-trigger-upstream.log,
/tmp/romedb-trigger-scoped.log. Dev/test debug symbols and incremental compilation
were disabled; test scope and debug assertions were preserved.

This validates the named defect and the required surrounding suites. It is not
an interrupted commit/checkpoint or arbitrary cancellation-boundary guarantee.
The approved exception is recorded in UPSTREAM.md and integrated into main.
Published preview artifacts remain unchanged and predate this fix.
