# RomeDB / RomeQL 0.2.11

Released for Linux x64 on 2026-10-28. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.11).

RomeDB 0.2.11 adds array path indexes: search indexes on values inside arrays of objects.

- `CREATE SEARCH INDEX roles ON docs(team.*.role) USING ARRAY` ([contract](v2.2-language.md#array-path-indexes-0211)) indexes every `role` inside the `team` array. A `*` step goes into each element of an array, and paths can step through several arrays, such as `team.*.links.*.kind`.
- `array::contains(team.*.role, 'Lead')` finds documents through that index. The same query without the index scans and returns the same rows.
- A null or missing value reached through a `*` path is never a key, so `array::contains(team.*.role, NULL)` is false.
- With a strict schema, the field before the first `*` must be an array of objects, or an array of arrays for `*.*`.
- The first `*` index in a collection raises that collection's catalog to version 8. Other collections keep their version.

Upgrading. 0.2.11 opens databases made by 0.2.9 and 0.2.10, and qualification checks both upgrades. 0.2.10 and earlier refuse to open a database that has any version 8 collection. Back up before creating the first `*` index if you might need to go back to an older release.

Security. The shipping dependency review finds zero advisories, with no accepted exceptions. See [the review](dependency-security.md) and [qualification](release-0.2.11-qualification.json).

The build source is `b8129ced1784250be7259f0c49170dfc8c9d9ac0`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/40652589338) passes. All 24 installed verification groups pass, including the 0.2.9 to 0.2.11 and 0.2.10 to 0.2.11 upgrade and restore on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET. The production-profile Rust consumer and the bounded application workload also pass.
