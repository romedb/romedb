# RomeDB / RomeQL 2.8.0

Released for Linux x64 on 2026-10-15. [Download the release](https://github.com/romedb/romedb/releases/tag/fastdb-v2.8.0).

This minor release joins document collections with relational tables. The
catalog format and stored data are unchanged: 2.5.0 to 2.8.0 open each other's
files.

- A relational table refers to a document by storing its record ID as text, for
  example `posts.user_id = 'users:tan'`. Comparisons between a document field and
  a relational column (`=`, `!=`, `IS`, `IS NOT`), and `IN`/`NOT IN` subqueries
  across the two kinds of source, treat that text as the record, so
  `SELECT p.title, u.name FROM posts p JOIN users u ON u.id = p.user_id` and
  `SELECT name FROM users WHERE id IN (SELECT user_id FROM posts)` work. Text that
  is not a record ID compares as before, and a document string field still equals
  the same text. Joins on a collection `id` keep a hash join or key lookup.
- In a join with a collection, an unqualified name is a field of the only
  collection when no relational source has that column, so
  `SELECT p.title, name FROM posts p JOIN users u ON ...` works. Relational column
  names must be qualified, because any document can have that field, unless every
  collection has a strict schema without it. The bare collection alias
  (`SELECT p.title, u FROM ...`) returns the whole document. The error names the
  field to qualify.

The frozen build source is `2676abc05d2f33246d90fe2ecaf1d01c6bdbb507`.
[Exact-source CI](https://github.com/romedb/romedb/actions/runs/38739594172)
passes. Local scoped checks passed 1,099 Rust tests, 188 Node/application tests,
five C ABI tests, two release-policy tests, three upgrade-table tests,
formatting, scoped Clippy and strict TypeScript, with no failures. Four opt-in
measurement tests are ignored by default. The production profile retains debug
assertions and overflow checks.

All 45 installed verification groups pass, including CLI graph and
spatial checks, Node 22.0.0/24.19.0, CPython 3.10.21/3.12.3/3.14.7, PHP FFI, Go
race detection, Swift and .NET. Immutable 1.0.0 to 2.7.0 upgrade, candidate
writes, reopen, rollback, unchanged backups and independent restore pass. The
2.4.0 package still refuses files with a new-format spatial index, and the 2.5.0
to 2.7.0 packages read files written by 2.8.0. The standalone production-profile
Rust consumer and the bounded 1,000-row application workload also pass. The build
and checks ran under `scripts/romedb-heavy` with its memory ceiling.

The shipping dependency review covers 359 package identities with zero unresolved
findings and retains the documented lru informational exception RUSTSEC-2026-0253.
The lockfile differs from 2.7.0 only in RomeDB workspace versions.

[Public download verification](release-2.8.0-download.json) records the anonymous
download and checksums. See [qualification](release-2.8.0-qualification.json) and
[the checklist](v2.8.0-tasks.md).
