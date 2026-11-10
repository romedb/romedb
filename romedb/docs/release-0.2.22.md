# RomeDB / RomeQL 0.2.22

Released for Linux x64 on 2026-11-10. [Download the release](https://github.com/romedb/romedb/releases/tag/romedb-v0.2.22).

RomeDB 0.2.22 adds accent folding and stemming to full-text search, and moves the CLI's bundled SQLite to 3.54.1.

- **Accent folding.** `CREATE SEARCH INDEX articles_text ON articles(title, body) USING FULLTEXT WITH (fold=true)` turns accented and other non-ASCII Latin letters into plain ASCII, so `cafe` finds "Café" and `resume` finds "Résumé". It works with the `default`, `simple` and `whitespace` tokenizers; `simple` and `whitespace` still keep case.
- **Stemming.** `WITH (stemmer='english')` indexes and searches the stem of each word, so `run` finds "running" and "runs", and the phrase `"run shoe"` finds "running shoes". It works with the `default` tokenizer, in 18 languages from `arabic` to `turkish`. Folding runs before stemming, so a query typed without accents stems like the accented text. Other values reject with `RDB_VALIDATION` and list the languages.
- Both options are part of the index definition: `IF NOT EXISTS` with other values rejects, `INFO FOR TABLE` lists `fold` and `stemmer`, and `REINDEX`, reopening and file exports keep them. Indexing, queries, `search::analyze` and `search::snippet` all use the same analyzer. To change them, drop the index and create it again.
- **Prefixes on a stemmed index.** A prefix matches stored words that start with the typed text or with its stem: `running*` finds the stem `run`. While someone types, a prefix with no match uses the longest stored word that the typed text starts with, if that word has at least half of the typed characters: `runni*` finds "running" and `informatio*` finds "information". This is one new patch to Turso core, listed in the core exceptions.
- **SQLite 3.54.1.** The `romedb` CLI reads SQLite files for its import through `rusqlite`, now 0.41.1, which bundles SQLite 3.54.1. This fixes RUSTSEC-2026-0459 (CVE-2026-119375): SQLite before 3.54.0 can return uninitialized memory when it opens a database whose `-wal` file has a different page size, and the CLI import opened the source with its `-wal` file. The Node, Python and C packages do not include SQLite. RomeDB now builds with Rust 1.89, which `rusqlite` 0.41 needs.

Upgrading. 0.2.22 opens databases from 0.2.13 to 0.2.21 with no change. A collection with a full-text index that uses `fold` or `stemmer` moves to catalog version 10, which 0.2.21 and earlier refuse to open; a database without them stays readable by them. Keep `@romedb/node` and `@romedb/client` at the same version.

RomeDB Cloud runs this release.

Security. The shipping dependency review finds zero advisories, with no accepted exceptions. See [the review](dependency-security.md) and [qualification](release-0.2.22-qualification.json).

The build source is `18819538548988efc7ba4591b83f607316ae6262`. [Exact-source CI](https://github.com/romedb/romedb/actions/runs/42815143064) passes. All 40 installed verification groups pass on Node 22.0.0 and 24.19.0, CPython 3.10.21, 3.12.3 and 3.14.7, PHP FFI, Go, Swift and .NET, including the upgrades from the 0.2.13 to 0.2.21 packages. The production-profile Rust consumer and the bounded application workload also pass.
