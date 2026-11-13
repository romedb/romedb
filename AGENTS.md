# Turso Agent Guidelines

SQLite rewrite in Rust. 40+ crate workspace.

## Quick Reference

```bash
cargo build                    # build. never build with --release
cargo test                     # rust unit/integration tests
cargo fmt                      # format (required)
cargo clippy --workspace --all-features --all-targets -- --deny=warnings  # lint
cargo run -q --bin tursodb -- -q # run the interactive cli. never run with --release

make test                      # TCL compat + sqlite3 + extensions + MVCC
make test-single TEST=foo.test # single TCL test
make -C sqlite/conformance run-rust ARGS='--snapshot-filter __never__'  # sqltest runner (preferred for new tests)
CI=1 make -C sqlite/conformance run-rust  # use only if snapshot tests are required

scripts/diff.sh "SQL" [label]  # compare sqlite3 vs tursodb output
```

## Testing

### Running Tests

- `cargo test` - Rust unit and integration tests
- `make test` - broad compatibility suite (TCL, sqlite3, extensions, MVCC)
- `make test-single TEST=foo.test` - single legacy TCL test
- `make -C sqlite/conformance run-rust ARGS='--snapshot-filter __never__'` - preferred `.sqltest` runner for new coverage
- `CI=1 make -C sqlite/conformance run-rust` - only when snapshot tests are required

### Test Organization

Default: add coverage to the narrowest existing test harness that can express the bug. Prefer extending an existing test file or directory over creating a new one.

- `sqlite/conformance/sqlite-sqltests/` - preferred for SQL conformance coverage. These tests run the same scenario against both Turso and SQLite, so use them first for parser, planner, executor, and SQL semantics work that fits the `.sqltest` DSL.
- `tests/integration/` - primary fallback when the behavior cannot be expressed cleanly in `.sqltest`. Put API-level regressions, multi-connection orchestration, storage assertions, injected failures, timeout behavior, and other Rust-driven scenarios here.
- `sqlite/conformance/upstream/` - imported upstream SQLite golden tests. Do not modify these for Turso behavior changes; use them as fixed compatibility coverage, and only touch them for intentional upstream sync or harness maintenance.
- `postgres/conformance/pg-sqltests/` - `.sqltest` coverage for the PostgreSQL frontend, run via `make -C postgres/conformance run` (spawns a tursopg server per test and drives it over the wire protocol). Only assert behavior real PostgreSQL also exhibits, so the corpus stays valid for differential runs.
- `testing/cli_tests/` - CLI-focused Python coverage for shell behavior and end-to-end command workflows.
- `tests/fuzz/` - minimized fuzz regressions and targeted edge cases that are easier to keep as Rust tests.
- `testing/simulator/` and `testing/concurrent-simulator/` - deterministic concurrency, scheduling, and failure-injection coverage for state-machine and I/O correctness.
- `testing/differential-oracle/` and `testing/stress/` - differential and long-running stress tooling. Use these for deeper investigation or specialized validation, not as the first stop for a focused regression test.

## Structure

```
limbo/
├── core/           # Database engine (translate/, storage/, vdbe/, io/, mvcc/)
├── sqlite/
│   └── parser/     # SQL parser (lexer, AST, grammar)
├── cli/            # tursodb CLI (REPL, MCP server, sync server)
├── bindings/       # Python, JS, Java, .NET, Go, Rust
├── extensions/     # crypto, regexp, csv, fuzzy, ipaddr, percentile
├── testing/        # simulator/, concurrent-simulator/, differential-oracle/
├── sync/           # engine/, sdk-kit/ (Turso Cloud sync)
├── sdk-kit/        # High-level SDK abstraction
└── tools/          # dbhash utility
```

## Where to Look

| Task | Location | Notes |
|------|----------|-------|
| Query execution | `core/vdbe/execute.rs` | 12k LOC bytecode interpreter |
| SQL compilation | `core/translate/` | AST → bytecode, optimizer in `optimizer/` |
| B-tree/pages | `core/storage/btree.rs` | 10k LOC, SQLite-compatible format |
| WAL/durability | `core/storage/wal.rs` | Write-ahead log, checkpointing |
| SQL parsing | `sqlite/parser/src/parser.rs` | 11k LOC recursive descent |
| Add extension | `extensions/core/` | ExtensionApi, scalar/aggregate/vtab traits |
| Add binding | `bindings/` | PyO3, NAPI, JNI, FRB, CGO patterns |
| Deterministic tests | `testing/simulator/` | Fault injection, differential testing |
| New SQL tests | `sqlite/conformance/sqlite-sqltests/` | `.sqltest` format preferred |
| Quick sqlite3 diff | `scripts/diff.sh` | Compare sqlite3 vs tursodb output for a query |
| MVCC testing REPL | `cli/mvcc_repl.rs` | Multi-conn concurrent txn testing REPL        |

## Guides

- **[Testing](docs/agent-guides/testing.md)** - test types, when to use, how to write
- **[Code Quality](docs/agent-guides/code-quality.md)** - correctness rules, Rust patterns, comments
- **[Debugging](docs/agent-guides/debugging.md)** - bytecode comparison, logging, sanitizers
- **[PR Workflow](docs/agent-guides/pr-workflow.md)** - commits, CI, dependencies
- **[Transaction Correctness](docs/agent-guides/transaction-correctness.md)** - WAL, checkpointing, concurrency
- **[Storage Format](docs/agent-guides/storage-format.md)** - file format, B-trees, pages
- **[Async I/O Model](docs/agent-guides/async-io-model.md)** - IOResult, state machines, re-entrancy
- **[MVCC](docs/agent-guides/mvcc.md)** - multi-version concurrency, snapshot isolation, checkpointing

## Commit Messages

Use an optional component scope followed by a lowercase imperative summary with
no trailing period:

```text
[scope: ]<imperative summary>

<why the change is needed and what invariant or bug it addresses>

<non-obvious implementation details or tradeoffs, if needed>

Tests: <relevant validation, if useful>

Fixes #1234
```

For example: `core/mvcc: preserve B-tree cleanup markers in commit logs`.
Explain intent rather than narrating the diff. Omit the body only when the
subject fully explains a trivial change. Conventional Commit prefixes such as
`feat(scope):` are not required. See [CONTRIBUTING.md](CONTRIBUTING.md) for a
complete example.

## Benchmark Naming

- Criterion benchmark functions must use `#[turso_macros::codspeed_criterion_benchmark]` so stable and nightly CodSpeed runs get distinct benchmark names.
- Divan benchmark functions must use `#[turso_macros::divan_bench]` for the same stable/nightly naming behavior.

## Core Principles

1. **Correctness paramount.** Production DB, not a toy. Crash > corrupt
2. **SQLite compatibility.** Compare bytecode with `EXPLAIN`
3. **Every change needs a test.** Must fail without change, pass with it
4. **Assert invariants.** Don't silently fail. Don't hedge with if-statements
5. **Own your regressions.** If tests fail after your change, they are your regressions. Debug them directly. Never stash/revert to "check if they fail on main" — that wastes time and is categorically banned.
6. **Validate your hypotheses.**: If you suspect a given cause for a bug, validate it and provide incontrovertible evidence. NEVER make unearned assumptions.
7. **Driver API parity.** Embedded (`bindings/rust`) and serverless (`serverless/rust`) drivers expose the same public API; add features to both in the same change. Spec: `serverless/conformance/differential/README.md`.

## Always use plain language instead of complex jargon

OOGA BOOGA! Programming already complex! Use simple word! Say what you mean! Examples:

```diff
-    /// Number of generated statements outside the engines' shared executable domain.
+    /// Number of statements skipped because EXPLAIN failed in at least one engine.

...

-    fn empty_schema_only_selects_bootstrap_safe_statements() {
+    fn empty_schema_never_chooses_a_statement_that_needs_a_table() {
```

No-one knows what the hell a bootstrap-safe statement is. Everyone knows what "a statement that needs a table" is. Do
not use metaphorical language, such as the following terms: load-bearing, pin, bite, sharp, arm, guard, bless, wedge,
retire, retarget, answer, settle, carry, land, honor.
Do not make up terms if they have equivalents that are commonly used in the domain.

## Code flows from top to bottom

A reader should be able to read a file from the top down without jumping
ahead to find what a name means. The general rule: place a function after
all of its call sites. Callers come first, callees follow.

```text
pub fn commit()         // entry point
fn write_frames()       // called by commit
fn sync_wal()           // called by write_frames
```

When adding a helper, put it below the functions that call it, not at the
end of the file or wherever the cursor happened to be.

## Do not add comments

- Do not add comments. Instead, focus on making your code expressive.

## CI Note

Running in GitHub Action? Max-turns limit in `.github/workflows/claude.yml`. OK to push WIP and continue in another action. Stay focused, avoid rabbit holes.

## RomeDB fork: local Turso core patches

This fork carries patches to Turso core (`core/`). Check every one of them when
merging a new upstream Turso release. The full register, with the regression
tests and the condition for dropping each patch, is
[romedb/docs/core-exceptions.md](romedb/docs/core-exceptions.md); the current
upstream base is in [romedb/UPSTREAM.md](romedb/UPSTREAM.md).

Older patch (2026-11-02): **MVCC savepoint commit resume**, `core/vdbe/execute.rs`,
`SavepointOp::Release` in `op_savepoint`. When `RELEASE` of the savepoint that
opened a transaction commits it, and that commit waits on I/O (on MVCC files, the
automatic checkpoint at about 4 MB of log), the resumed instruction now finishes
the commit through `op_auto_commit` instead of looking up the released savepoint
again and failing with `no such savepoint`. Upstream main `36da5b2e4` (2026-11-02)
still has the bug.

Newer patch (2026-11-04): **VACUUM INTO tables without rows**, `core/vdbe/vacuum.rs`
at `StartCopyTable`, set through `Connection::set_vacuum_into_tables_without_rows`.
It lets a file export skip tables so that their rows are never read. Keep it until
upstream can skip tables in `VACUUM INTO`; the register has the regressions.

Newer patch (2026-11-06): **VACUUM INTO in batches**, `core/vdbe/vacuum.rs` and
`core/vdbe/execute.rs`. The copy commits in batches, is built as WAL pages and becomes MVCC
before its header, and is removed after a failure. In-place `VACUUM` does not change. Keep it
until upstream stops building `VACUUM INTO` as one transaction; the register has the
regressions.

Newer patch (2026-11-09): **FTS single-word prefixes**, `core/index_method/fts/prefix.rs`,
called from `query_start` in `core/index_method/fts/mod.rs`. A query word ending in `*`
(`hel*`, or the one-word phrase prefix `"hel"*`) becomes an OR of the 64 indexed words
that start with it and are in the most documents. Queries without such a word build
exactly as before. Keep it until upstream expands a one-word prefix with BM25 scores;
the register has the regressions.

Newer patch (2026-11-11): **FTS typo words**, `word~1` and `word~2` in
`core/index_method/fts/prefix.rs`. The word matches itself and the 64 indexed words within
that many edits that are in the most documents; the other words score at half weight
with the IDF of the most common one. Keep it until upstream FTS scores typo matches with
BM25; the register has the regressions.

Newer patch (2026-11-11): **FTS filter columns**, the `filters` WITH key in
`core/index_method/fts/mod.rs` and the filter terms built in `core/index_method/fts/prefix.rs`.
Filter columns are indexed whole with the `raw` tokenizer, are not default query fields,
and a `column:value` or `column: IN [...]` term on one matches with score 0. Keep it until
upstream FTS can filter on whole values without scoring them; the register has the
regressions.

Newer patch (2026-11-09): **FTS folding and stemming**, the `fold` and `stemmer` WITH
keys in `core/index_method/fts/mod.rs`, built by `Analyzer` in
`core/index_method/fts/analysis.rs`. The filtered analyzer is registered under its own
name (`default+fold+stem-english`). Prefix expansion now builds the expanded words as
term queries, so a stored stem is never stemmed again. Keep it until upstream FTS
offers folding and stemming options; the register has the regressions.

Newest patch (2026-11-13): **MVCC commit scans its write set for schema rows once**,
`step_build_log_record` in `core/mvcc/database/mod.rs`. The created, dropped and removed
B-tree sets of a commit are computed on its first log-record batch and kept in
`BuildLogRecordCtx::schema_btree_changes`, instead of being recomputed from the whole write
set for each batch of 1,024 entries; a 1,000,000-row commit took 103.6 s before its log
record was ready. Keep it until upstream stops recomputing them per batch; the register
has the regression.

Newer patch (2026-11-12): **MVCC log-byte budget**, `core/execution_meter.rs`,
`ProgramState::record_log_write` in `core/vdbe/mod.rs`, the insert and delete opcodes in
`core/vdbe/execute.rs`, and `portable_delete_extension_len` in `core/mvcc/portable_logical.rs`.
The meter adds an estimate of the MVCC logical-log bytes of each row written through an
MVCC cursor. With `ExecutionLimits::max_log_bytes` set, a statement whose
estimate passes the limit stops with an interrupt before COMMIT, like the mutation budget.
Keep it until upstream can limit the log size of a transaction before its commit; the
register has the regressions.

Newer patch (2026-11-12): **MVCC exact seeks over shadowed B-tree rows**,
`seek_btree_and_set_peek` in `core/mvcc/cursor.rs`. An exact rowid seek stops at its row
(backport of upstream `d94a7d320` and `4813f6096`; drop it at the first sync that contains
upstream `2c837d49a`). Ours: an exact index seek steps the B-tree one entry at a time and
stops at the first entry past its key, so UNIQUE checks after a bulk update are no longer
quadratic. Keep it until upstream stops exact index seeks at the end of their key; the
register has the regressions.

The workspace builds with Rust 1.89 (`rust-toolchain.toml`), not upstream's 1.88, because
`rusqlite` 0.41 needs it (RUSTSEC-2026-0459). Keep 1.89 or later on a sync.

On an upstream sync:
- Keep the patch unless upstream changed `SavepointOp::Release` to resume a
  commit in progress (`ProgramState::commit_state` not `Ready`).
- Either way, both regressions must pass:
  `cargo test -p core_tester --test integration_tests test_named_savepoint_release_resumes_commit_after_auto_checkpoint`
  and `cargo test -p romedb-tests --test transactions atomic_requests_on_mvcc_files_survive_automatic_checkpoints`.
- The fix is a candidate for an upstream Turso PR; once upstream ships it, drop
  the local patch and keep the tests.
