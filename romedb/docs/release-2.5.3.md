# RomeDB / RomeQL 2.5.3

Released for Linux x64 on 2026-10-14. [Download the release](https://github.com/romedb/romedb/releases/tag/fastdb-v2.5.3).

This patch release makes collection scans and grouping cheaper per document. It
changes no stored format: 2.5.0 to 2.5.3 open each other's files.

- Field reads parse null, boolean, integer, number and escape-free string members
  directly; other members still use the JSON decoder.
- Spatial and other helpers decode canonical points directly and read their
  arguments in place.
- Grouped collection queries evaluate field accessors before grouping, so the
  `GROUP BY` sorter holds the extracted values instead of whole documents and no
  longer spills them to a temporary file.

On a 65 MB file-backed collection of 100,000 documents, a filter scan is 17%
faster, returning three fields 11%, `GROUP BY` with `count(*)` and `sum` 24%,
grouping by H3 cell 25% and a filtered nearest search through the ordinary plan
15%. A ten-times larger page cache gave only 4 to 10% and was not adopted. See
the [measurements](spatial-upgrade-evidence.md#253-scans-and-grouping). Tests
compare the direct parsers with the JSON decoder, and 14 grouped query shapes
return the same rows and errors with and without the rewrite.

Previous-version upgrade checks now come from one version-range table
(`scripts/upgrade_matrix.py`), with a dry run before tagging
(`scripts/check-upgrades.py`); see the
[production build guide](production-build.md#previous-version-upgrade-checks).

The frozen build source is `b69be862370b0e540a87e5f94bce5e1ae95593ba`.
[Exact-source CI](https://github.com/romedb/romedb/actions/runs/38517510416)
passes. Local scoped checks passed 1,093 Rust tests, 185 Node/application tests,
five C ABI tests, two release-policy tests, three upgrade-table tests,
formatting, scoped Clippy and strict TypeScript, with no failures. Four opt-in
measurement tests are ignored by default. The production profile retains debug
assertions and overflow checks.

All 37 installed verification groups pass, including CLI graph and spatial
checks, Node 22.0.0/24.19.0, CPython 3.10.21/3.12.3/3.14.7, PHP FFI, Go race
detection, Swift and .NET. Immutable 1.0.0 to 2.5.2 upgrade, candidate writes,
reopen, rollback, unchanged backups and independent restore pass. The 2.4.0
package still refuses files with a new-format spatial index, and the 2.5.0,
2.5.1 and 2.5.2 packages read files written by 2.5.3. The standalone
production-profile Rust consumer and the bounded 1,000-row application workload
also pass. The build and checks ran under `scripts/romedb-heavy` with its memory
ceiling.

The shipping dependency review covers 359 package identities with zero unresolved
findings and retains the documented lru informational exception RUSTSEC-2026-0253.
The lockfile differs from 2.5.2 only in RomeDB workspace versions.

[Public download verification](release-2.5.3-download.json) records the anonymous
download and checksums. See [qualification](release-2.5.3-qualification.json) and
[the checklist](v2.5.3-tasks.md).
